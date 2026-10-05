//! Indexed, parameterized reporting queries shared by HTML, CSV and the statistics API.
use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, params};
use serde::Deserialize;
use wisprcheap_sync::protocol::{DeviceMonth, MonthStats};

use crate::stats::Row;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Query {
    pub from: Option<String>,
    pub to: Option<String>,
    pub device: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub before: Option<i64>,
    pub before_id: Option<String>,
}

#[derive(Clone, Copy)]
pub enum Zone {
    Named(Tz),
    Offset(i64),
}

impl Zone {
    pub fn named(name: &str) -> Result<Self> {
        Ok(Self::Named(name.parse()?))
    }
    pub fn date_ms(self, date: NaiveDate) -> Result<i64> {
        let local = date.and_hms_opt(0, 0, 0).unwrap();
        match self {
            Self::Named(tz) => {
                // A few zones advance their clocks at midnight. Use the first valid instant.
                for minute in 0..=180 {
                    if let Some(t) = tz.from_local_datetime(&(local + Duration::minutes(minute))).earliest() {
                        return Ok(t.timestamp_millis());
                    }
                }
                bail!("date does not exist in this time zone")
            }
            Self::Offset(min) => Ok(local.and_utc().timestamp_millis() - min * 60_000),
        }
    }
    pub fn format(self, ms: i64, format: &str) -> String {
        let Some(t) = DateTime::from_timestamp_millis(ms) else { return String::new() };
        match self {
            Self::Named(tz) => t.with_timezone(&tz).format(format).to_string(),
            Self::Offset(min) => (t + Duration::minutes(min)).format(format).to_string(),
        }
    }
}

#[derive(Clone)]
pub struct Filter {
    pub query: Query,
    pub start: i64,
    pub end: i64,
}

const PREDICATE: &str = "user_id=?1 AND deleted=0 AND timestamp_ms>=?2 AND timestamp_ms<?3 \
 AND (?4 IS NULL OR device_id=?4) AND (?5 IS NULL OR provider=?5) \
 AND (?6 IS NULL OR stt_model=?6 OR llm_model=?6) AND (?7 IS NULL OR status=?7)";

impl Filter {
    pub fn new(mut q: Query, zone: Zone, dashboard: bool) -> Result<Self> {
        for value in [&mut q.device, &mut q.provider, &mut q.model, &mut q.status] {
            if value.as_deref() == Some("") {
                *value = None;
            }
            if value.as_ref().is_some_and(|s| s.len() > 100) {
                bail!("filter is too long");
            }
        }
        if q.status.as_deref().is_some_and(|v| !["ok", "failed", "empty"].contains(&v)) {
            bail!("invalid status");
        }
        if dashboard && q.from.as_deref().is_none_or(str::is_empty) && q.to.as_deref().is_none_or(str::is_empty) {
            let now = zone.format(Utc::now().timestamp_millis(), "%Y-%m-01");
            let month = NaiveDate::parse_from_str(&now, "%Y-%m-%d")?;
            q.from = Some(month.checked_sub_months(chrono::Months::new(11)).unwrap().to_string());
            q.to = Some(month.checked_add_months(chrono::Months::new(1)).unwrap().pred_opt().unwrap().to_string());
        }
        if dashboard {
            if q.to.as_deref().is_none_or(str::is_empty) {
                let now = zone.format(Utc::now().timestamp_millis(), "%Y-%m-01");
                let month = NaiveDate::parse_from_str(&now, "%Y-%m-%d")?;
                q.to = Some(month.checked_add_months(chrono::Months::new(1)).unwrap().pred_opt().unwrap().to_string());
            }
            if q.from.as_deref().is_none_or(str::is_empty) {
                let end = NaiveDate::parse_from_str(q.to.as_deref().unwrap(), "%Y-%m-%d")?;
                let month = end.with_day(1).unwrap();
                q.from = Some(month.checked_sub_months(chrono::Months::new(11)).unwrap().to_string());
            }
        }
        let date = |v: &Option<String>| -> Result<Option<NaiveDate>> {
            let Some(v) = v.as_deref().filter(|v| !v.is_empty()) else { return Ok(None) };
            if v.len() != 10 {
                bail!("dates must use YYYY-MM-DD");
            }
            Ok(Some(NaiveDate::parse_from_str(v, "%Y-%m-%d")?))
        };
        let start = date(&q.from)?.map(|d| zone.date_ms(d)).transpose()?.unwrap_or(0);
        let end = date(&q.to)?.map(|d| zone.date_ms(d.succ_opt().unwrap())).transpose()?.unwrap_or(253_402_300_799_000);
        if start >= end {
            bail!("start date must be on or before end date");
        }
        if dashboard {
            let first = NaiveDate::parse_from_str(&zone.format(start, "%Y-%m-01"), "%Y-%m-%d")?;
            let last = NaiveDate::parse_from_str(&zone.format(end - 1, "%Y-%m-01"), "%Y-%m-%d")?;
            if (last.year() - first.year()) * 12 + last.month() as i32 - first.month() as i32 >= 60 {
                bail!("choose a range of at most five years");
            }
        }
        if q.before_id.as_ref().is_some_and(|v| v.len() > 100) {
            bail!("invalid history cursor");
        }
        if q.before.is_some() != q.before_id.is_some() {
            bail!("history cursor requires both timestamp and entry ID");
        }
        Ok(Self { query: q, start, end })
    }
    pub fn url_query(&self) -> String {
        let q = &self.query;
        [
            ("from", &q.from),
            ("to", &q.to),
            ("device", &q.device),
            ("provider", &q.provider),
            ("model", &q.model),
            ("status", &q.status),
        ]
        .into_iter()
        .filter_map(|(k, v)| {
            v.as_ref().filter(|s| !s.is_empty()).map(|v| format!("{k}={}", crate::util::url_encode(v)))
        })
        .collect::<Vec<_>>()
        .join("&")
    }
}

#[derive(Default)]
pub struct Summary {
    pub entries: u64,
    pub failed: u64,
    pub empty: u64,
    pub commands: u64,
    pub words: u64,
    pub audio_minutes: f64,
    pub stt_usd: f64,
    pub llm_usd: f64,
    pub total_usd: f64,
    pub unknown: u64,
    pub stt_avg: Option<f64>,
    pub llm_avg: Option<f64>,
    pub stt_p95: Option<f64>,
    pub llm_p95: Option<f64>,
}

pub fn summary(conn: &Connection, user: &str, f: &Filter) -> Result<Summary> {
    let q = &f.query;
    let sql = format!(
        "SELECT COALESCE(SUM(status='ok'),0), COALESCE(SUM(status='failed'),0), COALESCE(SUM(status='empty'),0), \
        COALESCE(SUM(status='ok' AND mode='command'),0), COALESCE(SUM(words),0), COALESCE(SUM(duration_sec)/60,0), \
        COALESCE(SUM(resolved_stt),0), COALESCE(SUM(resolved_llm),0), COALESCE(SUM(resolved_total),0), \
        COALESCE(SUM(unknown_price AND status!='failed'),0), AVG(NULLIF(stt_ms,0)), AVG(NULLIF(llm_ms,0)) FROM history_stats WHERE {PREDICATE}"
    );
    let mut s = conn.query_row(&sql, params![user, f.start, f.end, q.device, q.provider, q.model, q.status], |r| {
        Ok(Summary {
            entries: r.get::<_, i64>(0)?.max(0) as u64,
            failed: r.get::<_, i64>(1)?.max(0) as u64,
            empty: r.get::<_, i64>(2)?.max(0) as u64,
            commands: r.get::<_, i64>(3)?.max(0) as u64,
            words: r.get::<_, i64>(4)?.max(0) as u64,
            audio_minutes: r.get(5)?,
            stt_usd: r.get(6)?,
            llm_usd: r.get(7)?,
            total_usd: r.get(8)?,
            unknown: r.get::<_, i64>(9)?.max(0) as u64,
            stt_avg: r.get(10)?,
            llm_avg: r.get(11)?,
            ..Default::default()
        })
    })?;
    for (column, target) in [("stt_ms", &mut s.stt_p95), ("llm_ms", &mut s.llm_p95)] {
        let sql = format!(
            "SELECT AVG(v) FROM (SELECT {column} v, ROW_NUMBER() OVER (ORDER BY {column}) n, COUNT(*) OVER () total \
            FROM history_stats WHERE {PREDICATE} AND {column}>0) WHERE n=(total*95+99)/100"
        );
        *target =
            conn.query_row(&sql, params![user, f.start, f.end, q.device, q.provider, q.model, q.status], |r| r.get(0))?;
    }
    Ok(s)
}

pub fn by_device(conn: &Connection, user: &str, f: &Filter) -> Result<Vec<DeviceMonth>> {
    let q = &f.query;
    let mut stmt=conn.prepare(&format!("SELECT COALESCE(device_id,''), SUM(status='ok'), SUM(words), SUM(resolved_total) FROM history_stats WHERE {PREDICATE} GROUP BY device_id ORDER BY SUM(resolved_total) DESC"))?;
    Ok(stmt
        .query_map(params![user, f.start, f.end, q.device, q.provider, q.model, q.status], |r| {
            Ok(DeviceMonth {
                device: r.get(0)?,
                entries: r.get::<_, i64>(1)?.max(0) as u64,
                words: r.get::<_, i64>(2)?.max(0) as u64,
                total_usd: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn by_model(conn: &Connection, user: &str, f: &Filter) -> Result<Vec<(String, String, u64, f64)>> {
    let q = &f.query;
    let sql = format!(
        "SELECT stt_model,'speech-to-text',COUNT(*),SUM(resolved_stt) FROM history_stats WHERE {PREDICATE} GROUP BY stt_model \
        UNION ALL SELECT llm_model,CASE WHEN mode='command' THEN 'command' ELSE 'cleanup / translation' END,COUNT(*),SUM(resolved_llm) \
        FROM history_stats WHERE {PREDICATE} AND llm_model IS NOT NULL GROUP BY llm_model,mode"
    );
    Ok(conn
        .prepare(&sql)?
        .query_map(params![user, f.start, f.end, q.device, q.provider, q.model, q.status], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u64, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn months(
    conn: &Connection,
    user: &str,
    f: &Filter,
    zone: Zone,
    first: &str,
    last: &str,
    include_empty: bool,
) -> Result<Vec<MonthStats>> {
    let mut date = NaiveDate::parse_from_str(&format!("{first}-01"), "%Y-%m-%d")?;
    let end = NaiveDate::parse_from_str(&format!("{last}-01"), "%Y-%m-%d")?;
    if date > end || (end.year() - date.year()) * 12 + end.month() as i32 - date.month() as i32 >= 60 {
        bail!("choose a range of at most five years");
    }
    let mut out = Vec::new();
    while date <= end {
        let next = date.checked_add_months(chrono::Months::new(1)).unwrap();
        let mut mf = f.clone();
        mf.start = f.start.max(zone.date_ms(date)?);
        mf.end = f.end.min(zone.date_ms(next)?);
        let q = &mf.query;
        // Monthly totals avoid the latency window sorts used by the selected-period summary.
        let sql = format!(
            "SELECT COALESCE(SUM(status='ok'),0),COALESCE(SUM(status='ok' AND mode='command'),0),COALESCE(SUM(status='failed'),0), \
            COALESCE(SUM(words),0),COALESCE(SUM(duration_sec)/60,0),COALESCE(SUM(resolved_stt),0),COALESCE(SUM(resolved_llm),0), \
            COALESCE(SUM(resolved_total),0),COALESCE(SUM(unknown_price AND status!='failed'),0),COUNT(*) FROM history_stats WHERE {PREDICATE}"
        );
        let (mut m, count) =
            conn.query_row(&sql, params![user, mf.start, mf.end, q.device, q.provider, q.model, q.status], |r| {
                Ok((
                    MonthStats {
                        month: date.format("%Y-%m").to_string(),
                        entries: r.get::<_, i64>(0)?.max(0) as u64,
                        commands: r.get::<_, i64>(1)?.max(0) as u64,
                        failed: r.get::<_, i64>(2)?.max(0) as u64,
                        words: r.get::<_, i64>(3)?.max(0) as u64,
                        audio_minutes: r.get(4)?,
                        stt_usd: r.get(5)?,
                        llm_usd: r.get(6)?,
                        total_usd: r.get(7)?,
                        unknown_price: r.get::<_, i64>(8)?.max(0) as u64,
                        ..Default::default()
                    },
                    r.get::<_, i64>(9)?,
                ))
            })?;
        if include_empty || count > 0 {
            m.by_device = by_device(conn, user, &mf)?;
            out.push(m);
        }
        date = next;
    }
    out.reverse();
    Ok(out)
}

pub fn history(conn: &Connection, user: &str, f: &Filter, limit: i64) -> Result<Vec<Row>> {
    let q = &f.query;
    let sql = format!(
        "SELECT entry_id,device_id,stats,cost_total,cost_stt,cost_llm FROM history_stats WHERE {PREDICATE} \
        AND (?8 IS NULL OR timestamp_ms<?8 OR (timestamp_ms=?8 AND entry_id<?9)) ORDER BY timestamp_ms DESC,entry_id DESC LIMIT ?10"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![user, f.start, f.end, q.device, q.provider, q.model, q.status, q.before, q.before_id, limit],
        |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (entry_id, device_id, json, cost_total, cost_stt, cost_llm) = row?;
        let stats = serde_json::from_str(&json)?;
        out.push(Row { entry_id, device_id, stats, cost_total, cost_stt, cost_llm });
    }
    Ok(out)
}

pub fn csv_cell(value: &str) -> String {
    // Spreadsheet programs interpret formula prefixes even inside quoted CSV fields.
    let prefix = if value.trim_start().starts_with(['=', '+', '-', '@', '\t', '\r']) { "'" } else { "" };
    format!("\"{prefix}{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paris_day_boundaries_follow_daylight_saving_time() {
        let zone = Zone::named("Europe/Paris").unwrap();
        for (day, hours) in [("2026-03-29", 23), ("2026-10-25", 25)] {
            let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
            assert_eq!(
                (zone.date_ms(date.succ_opt().unwrap()).unwrap() - zone.date_ms(date).unwrap()) / 3_600_000,
                hours
            );
        }
        assert_eq!(csv_cell("=SUM(A1:A2)"), "\"'=SUM(A1:A2)\"");
        assert_eq!(csv_cell("quote\"comma,"), "\"quote\"\"comma,\"");
    }
}
