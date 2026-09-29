//! The device sync API (`/v1`, SPEC.md section 7).

use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use wisprcheap_sync::hlc::Hlc;
use wisprcheap_sync::profile::{KIND_HISTORY, KINDS};
use wisprcheap_sync::protocol::{
    Change, ChangesResponse, DeviceInfo, MeResponse, PairRequest, PairResponse, PushRequest, PushResponse, PushResult,
    PushStatus, PutKeyringRequest, PutKeyringResponse, RenameDeviceRequest, StatsResponse, UserInfo,
};
use wisprcheap_sync::stats::HistoryStats;

use crate::auth::{ClientIp, DeviceAuth};
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::stats;
use crate::util::{now_ms, now_s};
use crate::{SharedState, VERSION};

pub const MAX_PAYLOAD: usize = 64 * 1024;
pub const MAX_BATCH: usize = 500;
pub const MAX_CLOCK_SKEW_MS: u64 = 10 * 60 * 1000;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/v1/pair", post(pair))
        .route("/v1/me", get(me))
        .route("/v1/keyring", put(put_keyring))
        .route("/v1/changes", get(pull).post(push))
        .route("/v1/stats", get(stats_handler))
        .route("/v1/device", axum::routing::patch(rename_device).delete(unpair))
}

fn device_info(d: &db::Device) -> DeviceInfo {
    DeviceInfo { id: d.id.clone(), name: d.name.clone(), platform: d.platform.clone() }
}

async fn pair(
    State(state): State<SharedState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<PairRequest>,
) -> ApiResult<Json<PairResponse>> {
    if !state.limiter.allow(&format!("pair:{ip}"), 10, Duration::from_secs(60)) {
        return Err(ApiError::rate_limited());
    }
    let name = req.name.trim();
    let platform = req.platform.trim().to_lowercase();
    if name.is_empty() || name.chars().count() > 60 || platform.len() > 20 || req.app_version.len() > 40 {
        return Err(ApiError::bad_request("name (1-60 characters), platform and appVersion are required"));
    }
    let mut conn = state.db.lock();
    let Some((token, device, user)) = db::redeem_pairing_code(&mut conn, &req.code, name, &platform, req.app_version.trim())?
    else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "invalid_code", "unknown, expired or already used pairing code"));
    };
    db::audit(&conn, Some(&user.id), Some(&device.id), "device_paired", Some(&format!("{} ({})", device.name, device.platform)), &ip);
    Ok(Json(PairResponse {
        token,
        device: device_info(&device),
        user: UserInfo { id: user.id, username: user.username },
    }))
}

async fn me(State(state): State<SharedState>, auth: DeviceAuth) -> ApiResult<Json<MeResponse>> {
    let conn = state.db.lock();
    Ok(Json(MeResponse {
        user: UserInfo { id: auth.user.id.clone(), username: auth.user.username.clone() },
        device: device_info(&auth.device),
        server_time: crate::util::iso_now(),
        server_version: VERSION.into(),
        keyring: db::keyring(&conn, &auth.user.id)?,
    }))
}

fn validate_keyring(req: &PutKeyringRequest) -> Result<(), ApiError> {
    let salt_ok = wisprcheap_sync::crypto::unb64(&req.salt).is_ok_and(|s| s.len() == 16);
    let kdf = &req.kdf;
    let kdf_ok = kdf.alg == "argon2id" && (8_192..=1_048_576).contains(&kdf.m) && (1..=10).contains(&kdf.t) && (1..=8).contains(&kdf.p);
    let key_id_ok = req.key_id.len() == 16 && wisprcheap_sync::crypto::unb64(&req.key_id).is_ok();
    if !salt_ok || !kdf_ok || !key_id_ok || !req.wrapped_key.starts_with("e1.") || req.wrapped_key.len() > 200 {
        return Err(ApiError::bad_request("invalid keyring"));
    }
    Ok(())
}

async fn put_keyring(
    State(state): State<SharedState>,
    auth: DeviceAuth,
    headers: HeaderMap,
    Json(req): Json<PutKeyringRequest>,
) -> ApiResult<Json<PutKeyringResponse>> {
    validate_keyring(&req)?;
    let if_match = headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_matches('"').parse::<i64>())
        .transpose()
        .map_err(|_| ApiError::bad_request("If-Match must be the current keyVersion"))?;
    let conn = state.db.lock();
    let current = db::keyring(&conn, &auth.user.id)?;
    let version = match (current, if_match) {
        (None, None) => 1,
        (Some(_), None) => return Err(ApiError::new(StatusCode::CONFLICT, "keyring_exists", "the account already has a keyring")),
        (None, Some(_)) => return Err(ApiError::new(StatusCode::PRECONDITION_FAILED, "version_mismatch", "no keyring to replace")),
        (Some(k), Some(v)) if k.key_version != v => {
            return Err(ApiError::new(StatusCode::PRECONDITION_FAILED, "version_mismatch", "the keyring changed meanwhile"));
        }
        (Some(k), Some(_)) if k.key_id != req.key_id => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "key_mismatch",
                "a passphrase change must keep the same data key (use \"Reset encryption\" in the web UI to start over)",
            ));
        }
        (Some(k), Some(_)) => k.key_version + 1,
    };
    db::put_keyring(&conn, &auth.user.id, version, &req)?;
    let event = if version == 1 { "keyring_created" } else { "passphrase_changed" };
    db::audit(&conn, Some(&auth.user.id), Some(&auth.device.id), event, None, &auth.ip);
    Ok(Json(PutKeyringResponse { key_version: version }))
}

#[derive(Deserialize)]
struct PullQuery {
    #[serde(default)]
    since: i64,
    limit: Option<i64>,
    exclude: Option<String>,
}

fn change_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Change, Option<String>)> {
    Ok((
        Change {
            seq: Some(r.get(0)?),
            kind: r.get(1)?,
            id: r.get(2)?,
            hlc: r.get(3)?,
            deleted: r.get::<_, i64>(4)? != 0,
            payload: r.get(5)?,
            device: r.get(6)?,
            stats: None,
        },
        r.get(7)?,
    ))
}

async fn pull(State(state): State<SharedState>, auth: DeviceAuth, Query(q): Query<PullQuery>) -> ApiResult<Json<ChangesResponse>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 1000);
    let exclude_history = q.exclude.as_deref().is_some_and(|e| e.split(',').any(|k| k.trim() == KIND_HISTORY));
    let conn = state.db.lock();
    let mut stmt = conn.prepare(
        "SELECT r.seq, r.kind, r.id, r.hlc, r.deleted, r.payload, r.device_id, h.stats FROM records r \
         LEFT JOIN history_stats h ON r.kind = 'history' AND h.user_id = r.user_id AND h.entry_id = r.id \
         WHERE r.user_id = ?1 AND r.seq > ?2 AND (?3 = 0 OR r.kind != 'history') ORDER BY r.seq LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![auth.user.id, q.since.max(0), exclude_history as i64, limit + 1], change_from_row)?;
    let mut changes = Vec::new();
    for row in rows {
        let (mut change, stats) = row?;
        if change.kind == KIND_HISTORY && !change.deleted {
            change.stats = stats.and_then(|s| serde_json::from_str(&s).ok());
        }
        changes.push(change);
    }
    let has_more = changes.len() as i64 > limit;
    changes.truncate(limit as usize);
    let next_since = changes.last().and_then(|c| c.seq).unwrap_or(q.since.max(0));
    Ok(Json(ChangesResponse { changes, next_since, has_more }))
}

fn valid_id(kind: &str, id: &str) -> bool {
    let charset = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
    match kind {
        KIND_HISTORY => id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
        _ => (1..=100).contains(&id.len()) && charset(id),
    }
}

fn rejected(c: &Change, error: &str) -> PushResult {
    PushResult { kind: c.kind.clone(), id: c.id.clone(), status: PushStatus::Rejected, seq: None, current: None, error: Some(error.into()) }
}

async fn push(State(state): State<SharedState>, auth: DeviceAuth, Json(req): Json<PushRequest>) -> ApiResult<Json<PushResponse>> {
    if req.changes.len() > MAX_BATCH {
        return Err(ApiError::bad_request(format!("at most {MAX_BATCH} changes per request")));
    }
    let now = now_ms();
    let mut conn = state.db.lock();
    let tx = conn.transaction()?;
    let mut seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) FROM records", [], |r| r.get(0))?;
    let mut results = Vec::with_capacity(req.changes.len());
    for c in &req.changes {
        if !KINDS.contains(&c.kind.as_str()) {
            results.push(rejected(c, "invalid_kind"));
            continue;
        }
        if !valid_id(&c.kind, &c.id) {
            results.push(rejected(c, "invalid_id"));
            continue;
        }
        let Some(hlc) = Hlc::parse(&c.hlc) else {
            results.push(rejected(c, "invalid_hlc"));
            continue;
        };
        if hlc.ms > now + MAX_CLOCK_SKEW_MS {
            results.push(rejected(c, "clock_skew"));
            continue;
        }
        let payload = if c.deleted { None } else { c.payload.clone() };
        if !c.deleted {
            match &payload {
                None => {
                    results.push(rejected(c, "missing_payload"));
                    continue;
                }
                Some(p) if p.len() > MAX_PAYLOAD || !p.starts_with("e1.") => {
                    results.push(rejected(c, "payload_too_large"));
                    continue;
                }
                _ => {}
            }
        }
        let existing: Option<(Change, Option<String>)> = tx
            .query_row(
                "SELECT seq, kind, id, hlc, deleted, payload, device_id, NULL FROM records WHERE user_id = ?1 AND kind = ?2 AND id = ?3",
                params![auth.user.id, c.kind, c.id],
                change_from_row,
            )
            .optional()?;

        if c.kind == KIND_HISTORY {
            match &existing {
                // Immutable: an existing entry is never replaced (only deleted), so a retry is harmless.
                Some((e, _)) if !c.deleted || e.deleted => {
                    results.push(PushResult { kind: c.kind.clone(), id: c.id.clone(), status: PushStatus::Exists, seq: e.seq, current: None, error: None });
                    continue;
                }
                _ => {}
            }
            let stats: Option<HistoryStats> = if c.deleted { None } else { c.stats.clone() };
            if !c.deleted && stats.as_ref().is_none_or(|s| s.validate().is_err()) {
                results.push(rejected(c, "invalid_stats"));
                continue;
            }
            seq += 1;
            tx.execute(
                "INSERT INTO records (user_id, kind, id, hlc, deleted, payload, device_id, seq, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(user_id, kind, id) DO UPDATE SET hlc = ?4, deleted = ?5, payload = ?6, device_id = ?7, seq = ?8, updated_at = ?9",
                params![auth.user.id, c.kind, c.id, c.hlc, c.deleted as i64, payload, auth.device.id, seq, now_s()],
            )?;
            if let Some(s) = stats {
                let (stt, llm, total) = s.recomputed_costs();
                tx.execute(
                    "INSERT OR IGNORE INTO history_stats (user_id, entry_id, device_id, ts, stats, cost_stt, cost_llm, cost_total) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![auth.user.id, c.id, auth.device.id, s.ts, serde_json::to_string(&s).map_err(ApiError::internal)?, stt, llm, total],
                )?;
            } else {
                tx.execute("UPDATE history_stats SET deleted = 1 WHERE user_id = ?1 AND entry_id = ?2", params![auth.user.id, c.id])?;
            }
            results.push(PushResult { kind: c.kind.clone(), id: c.id.clone(), status: PushStatus::Applied, seq: Some(seq), current: None, error: None });
            continue;
        }

        // Last writer wins, by HLC.
        if let Some((e, _)) = &existing
            && Hlc::parse(&e.hlc).is_some_and(|stored| stored >= hlc)
        {
            results.push(PushResult { kind: c.kind.clone(), id: c.id.clone(), status: PushStatus::Stale, seq: None, current: Some(e.clone()), error: None });
            continue;
        }
        seq += 1;
        tx.execute(
            "INSERT INTO records (user_id, kind, id, hlc, deleted, payload, device_id, seq, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(user_id, kind, id) DO UPDATE SET hlc = ?4, deleted = ?5, payload = ?6, device_id = ?7, seq = ?8, updated_at = ?9",
            params![auth.user.id, c.kind, c.id, c.hlc, c.deleted as i64, payload, auth.device.id, seq, now_s()],
        )?;
        results.push(PushResult { kind: c.kind.clone(), id: c.id.clone(), status: PushStatus::Applied, seq: Some(seq), current: None, error: None });
    }
    tx.commit()?;
    Ok(Json(PushResponse { results }))
}

#[derive(Deserialize)]
struct StatsQuery {
    from: Option<String>,
    to: Option<String>,
    #[serde(default)]
    offset: i64,
}

/// Statistics for a user between two months (inclusive), in a time-zone offset.
pub fn user_stats(conn: &rusqlite::Connection, user_id: &str, from: &str, to: &str, offset: i64) -> ApiResult<StatsResponse> {
    let start = stats::month_start_iso(from, offset).ok_or_else(|| ApiError::bad_request("from: YYYY-MM"))?;
    let end_month = stats::next_month(to).ok_or_else(|| ApiError::bad_request("to: YYYY-MM"))?;
    let end = stats::month_start_iso(&end_month, offset).ok_or_else(|| ApiError::bad_request("to: YYYY-MM"))?;
    let rows = stats::rows(conn, user_id, &start, &end, 1_000_000)?;
    let devices = db::list_devices(conn, user_id)?.iter().map(device_info).collect();
    Ok(StatsResponse { months: stats::aggregate(&rows, offset), devices })
}

/// Default range: the last 12 months including the current one.
pub fn default_range(offset: i64) -> (String, String) {
    let now = chrono::Utc::now() + chrono::Duration::minutes(offset);
    let to = now.format("%Y-%m").to_string();
    let from = (now - chrono::Duration::days(335)).format("%Y-%m").to_string();
    (from, to)
}

async fn stats_handler(State(state): State<SharedState>, auth: DeviceAuth, Query(q): Query<StatsQuery>) -> ApiResult<Json<StatsResponse>> {
    let offset = q.offset.clamp(-14 * 60, 14 * 60);
    let (default_from, default_to) = default_range(offset);
    let from = q.from.unwrap_or(default_from);
    let to = q.to.unwrap_or(default_to);
    if !stats::valid_month(&from) || !stats::valid_month(&to) || from > to {
        return Err(ApiError::bad_request("from and to are months (YYYY-MM), from <= to"));
    }
    let conn = state.db.lock();
    Ok(Json(user_stats(&conn, &auth.user.id, &from, &to, offset)?))
}

async fn rename_device(State(state): State<SharedState>, auth: DeviceAuth, Json(req): Json<RenameDeviceRequest>) -> ApiResult<StatusCode> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::bad_request("name: 1 to 60 characters"));
    }
    let conn = state.db.lock();
    db::rename_device(&conn, &auth.user.id, &auth.device.id, name)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unpair(State(state): State<SharedState>, auth: DeviceAuth) -> ApiResult<StatusCode> {
    let conn = state.db.lock();
    db::revoke_device(&conn, &auth.user.id, &auth.device.id)?;
    db::audit(&conn, Some(&auth.user.id), Some(&auth.device.id), "device_unpaired", Some(&auth.device.name), &auth.ip);
    Ok(StatusCode::NO_CONTENT)
}
