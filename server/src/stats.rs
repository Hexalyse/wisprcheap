//! Monthly statistics over the readable history fields (SPEC.md section 6).

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};
use wisprcheap_sync::protocol::{DeviceMonth, MonthStats};
use wisprcheap_sync::stats::{HistoryStats, MODE_COMMAND, STATUS_FAILED, STATUS_OK};

/// One stored entry: its statistics, the device, and the server-recomputed costs.
pub struct Row {
    pub entry_id: String,
    pub device_id: Option<String>,
    pub stats: HistoryStats,
    pub cost_total: Option<f64>,
    pub cost_stt: Option<f64>,
    pub cost_llm: Option<f64>,
}

impl Row {
    /// Server price when known, else the client's estimate.
    pub fn total(&self) -> f64 {
        self.cost_total.or(self.stats.cost_total).or(self.cost_stt).or(self.stats.cost_stt).unwrap_or(0.0)
    }
    pub fn stt(&self) -> f64 {
        self.cost_stt.or(self.stats.cost_stt).unwrap_or(0.0)
    }
    pub fn llm(&self) -> f64 {
        self.cost_llm.or(self.stats.cost_llm).unwrap_or(0.0)
    }
    pub fn unknown_price(&self) -> bool {
        self.cost_stt.is_none() && self.stats.cost_stt.is_none()
    }
}

/// Entries of a user between two UTC timestamps (ISO strings), newest first.
pub fn rows(conn: &Connection, user_id: &str, from_iso: &str, to_iso: &str, limit: i64) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT entry_id, device_id, stats, cost_total, cost_stt, cost_llm FROM history_stats \
         WHERE user_id = ?1 AND deleted = 0 AND ts >= ?2 AND ts < ?3 ORDER BY ts DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![user_id, from_iso, to_iso, limit], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (entry_id, device_id, stats, cost_total, cost_stt, cost_llm) = row?;
        if let Ok(stats) = serde_json::from_str::<HistoryStats>(&stats) {
            out.push(Row { entry_id, device_id, stats, cost_total, cost_stt, cost_llm });
        }
    }
    Ok(out)
}

/// `YYYY-MM` of an ISO timestamp shifted by `offset_min` minutes.
pub fn month_of(ts: &str, offset_min: i64) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc) + Duration::minutes(offset_min);
    Some(t.format("%Y-%m").to_string())
}

/// First instant (UTC ISO) of month `YYYY-MM` in the given offset.
pub fn month_start_iso(month: &str, offset_min: i64) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(&format!("{month}-01T00:00:00Z")).ok()?.with_timezone(&Utc) - Duration::minutes(offset_min);
    Some(t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

pub fn next_month(month: &str) -> Option<String> {
    let (y, m) = month.split_once('-')?;
    let (y, m): (i32, u32) = (y.parse().ok()?, m.parse().ok()?);
    Some(if m >= 12 { format!("{:04}-01", y + 1) } else { format!("{y:04}-{:02}", m + 1) })
}

pub fn valid_month(month: &str) -> bool {
    month.len() == 7 && month_start_iso(month, 0).is_some()
}

/// Aggregates `rows` into months (newest first), with a per-device breakdown.
pub fn aggregate(rows: &[Row], offset_min: i64) -> Vec<MonthStats> {
    let mut months: BTreeMap<String, (MonthStats, BTreeMap<String, DeviceMonth>)> = BTreeMap::new();
    for r in rows {
        let Some(month) = month_of(&r.stats.ts, offset_min) else { continue };
        let (m, devices) = months
            .entry(month.clone())
            .or_insert_with(|| (MonthStats { month: month.clone(), ..Default::default() }, BTreeMap::new()));
        let ok = r.stats.status == STATUS_OK;
        m.entries += ok as u64;
        m.commands += (ok && r.stats.mode == MODE_COMMAND) as u64;
        m.failed += (r.stats.status == STATUS_FAILED) as u64;
        m.words += r.stats.words;
        m.audio_minutes += r.stats.duration_sec / 60.0;
        m.stt_usd += r.stt();
        m.llm_usd += r.llm();
        m.total_usd += r.total();
        m.unknown_price += (r.unknown_price() && r.stats.status != STATUS_FAILED) as u64;
        let device = r.device_id.clone().unwrap_or_default();
        let d = devices.entry(device.clone()).or_insert_with(|| DeviceMonth { device, ..Default::default() });
        d.entries += ok as u64;
        d.words += r.stats.words;
        d.total_usd += r.total();
    }
    months
        .into_values()
        .rev()
        .map(|(mut m, devices)| {
            m.by_device = devices.into_values().collect();
            m
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: &str, words: u64, status: &str, total: Option<f64>, device: &str) -> Row {
        Row {
            entry_id: ts.into(),
            device_id: Some(device.into()),
            stats: HistoryStats {
                ts: ts.into(),
                mode: "dictation".into(),
                duration_sec: 60.0,
                stt_provider: "elevenlabs".into(),
                stt_model: "scribe_v2".into(),
                stt_ms: 1,
                keyterms: 0,
                llm_model: None,
                llm_ms: None,
                input_tokens: 0,
                output_tokens: 0,
                words,
                status: status.into(),
                retry: false,
                cost_stt: total,
                cost_llm: None,
                cost_total: total,
            },
            cost_total: None,
            cost_stt: None,
            cost_llm: None,
        }
    }

    #[test]
    fn months_and_offsets() {
        assert_eq!(month_of("2026-09-30T23:30:00.000Z", 120).as_deref(), Some("2026-10"));
        assert_eq!(month_of("2026-09-30T23:30:00.000Z", 0).as_deref(), Some("2026-09"));
        assert_eq!(month_start_iso("2026-10", 120).as_deref(), Some("2026-09-30T22:00:00.000Z"));
        assert_eq!(next_month("2026-12").as_deref(), Some("2027-01"));
        assert!(valid_month("2026-09") && !valid_month("2026-13") && !valid_month("26-09"));
    }

    #[test]
    fn aggregation() {
        let rows = vec![
            row("2026-09-01T10:00:00.000Z", 10, "ok", Some(0.01), "dev_a"),
            row("2026-09-02T10:00:00.000Z", 5, "ok", Some(0.02), "dev_b"),
            row("2026-09-03T10:00:00.000Z", 0, "failed", None, "dev_a"),
            row("2026-08-03T10:00:00.000Z", 1, "ok", None, "dev_a"),
        ];
        let m = aggregate(&rows, 0);
        assert_eq!(m.iter().map(|m| m.month.as_str()).collect::<Vec<_>>(), ["2026-09", "2026-08"]);
        assert_eq!((m[0].entries, m[0].failed, m[0].words), (2, 1, 15));
        assert!((m[0].total_usd - 0.03).abs() < 1e-12);
        assert_eq!(m[0].by_device.len(), 2);
        assert_eq!(m[1].unknown_price, 1);
    }
}
