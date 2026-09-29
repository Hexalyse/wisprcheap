//! SQLite storage (one file in the data directory). All access goes through one connection behind a
//! mutex: plenty for a personal or family server, and it keeps writes strictly serialised.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::util::{b64, new_id, new_pairing_code, normalize_pairing_code, now_s, random_bytes, sha256};

const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0,
    disabled INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE sessions (
    id_hash BLOB PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    ip TEXT,
    user_agent TEXT
);
CREATE TABLE invites (
    code_hash BLOB PRIMARY KEY,
    purpose TEXT NOT NULL,
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
);
CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    platform TEXT NOT NULL,
    app_version TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    last_ip TEXT,
    revoked_at INTEGER
);
CREATE TABLE pairing_codes (
    code_hash BLOB PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_name TEXT,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
);
CREATE TABLE keyrings (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    key_version INTEGER NOT NULL,
    key_id TEXT NOT NULL,
    salt TEXT NOT NULL,
    kdf TEXT NOT NULL,
    wrapped_key TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE records (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    hlc TEXT NOT NULL,
    deleted INTEGER NOT NULL,
    payload TEXT,
    device_id TEXT,
    seq INTEGER NOT NULL UNIQUE,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, kind, id)
);
CREATE INDEX records_feed ON records(user_id, seq);
CREATE TABLE history_stats (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entry_id TEXT NOT NULL,
    device_id TEXT,
    ts TEXT NOT NULL,
    stats TEXT NOT NULL,
    cost_stt REAL,
    cost_llm REAL,
    cost_total REAL,
    deleted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, entry_id)
);
CREATE INDEX history_ts ON history_stats(user_id, ts);
CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT,
    device_id TEXT,
    event TEXT NOT NULL,
    detail TEXT,
    ip TEXT,
    at INTEGER NOT NULL
);
CREATE INDEX audit_user ON audit_log(user_id, at);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#];

pub const SESSION_MAX_AGE_S: i64 = 30 * 86_400;
pub const SESSION_IDLE_S: i64 = 7 * 86_400;
pub const PAIRING_TTL_S: i64 = 10 * 60;
pub const INVITE_TTL_S: i64 = 7 * 86_400;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {}; COMMIT;", i + 1))
                .with_context(|| format!("database migration {}", i + 1))?;
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Online backup to `target` (a consistent copy while the server runs).
    pub fn backup_to(&self, target: &Path) -> Result<()> {
        let conn = self.lock();
        let mut dst = Connection::open(target)?;
        let backup = rusqlite::backup::Backup::new(&conn, &mut dst)?;
        backup.run_to_completion(256, std::time::Duration::from_millis(10), None)?;
        Ok(())
    }
}

// --- Users ---

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub created_at: i64,
}

fn user_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        username: r.get(1)?,
        password_hash: r.get(2)?,
        is_admin: r.get::<_, i64>(3)? != 0,
        disabled: r.get::<_, i64>(4)? != 0,
        created_at: r.get(5)?,
    })
}

const USER_COLS: &str = "id, username, password_hash, is_admin, disabled, created_at";

pub fn user_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
}

pub fn create_user(conn: &Connection, username: &str, password_hash: &str, is_admin: bool) -> Result<String> {
    let id = new_id("usr");
    conn.execute(
        "INSERT INTO users (id, username, password_hash, is_admin, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, username, password_hash, is_admin as i64, now_s()],
    )?;
    Ok(id)
}

pub fn user_by_name(conn: &Connection, username: &str) -> Result<Option<User>> {
    Ok(conn
        .query_row(&format!("SELECT {USER_COLS} FROM users WHERE username = ?1"), [username], user_from_row)
        .optional()?)
}

pub fn user_by_id(conn: &Connection, id: &str) -> Result<Option<User>> {
    Ok(conn.query_row(&format!("SELECT {USER_COLS} FROM users WHERE id = ?1"), [id], user_from_row).optional()?)
}

pub fn list_users(conn: &Connection) -> Result<Vec<(User, i64)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {USER_COLS}, (SELECT COUNT(*) FROM devices d WHERE d.user_id = users.id AND d.revoked_at IS NULL) \
         FROM users ORDER BY username"
    ))?;
    let rows = stmt.query_map([], |r| Ok((user_from_row(r)?, r.get(6)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn set_password(conn: &Connection, user_id: &str, hash: &str) -> Result<()> {
    conn.execute("UPDATE users SET password_hash = ?2 WHERE id = ?1", params![user_id, hash])?;
    Ok(())
}

pub fn set_disabled(conn: &Connection, user_id: &str, disabled: bool) -> Result<()> {
    conn.execute("UPDATE users SET disabled = ?2 WHERE id = ?1", params![user_id, disabled as i64])?;
    if disabled {
        conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    }
    Ok(())
}

pub fn delete_user(conn: &Connection, user_id: &str) -> Result<()> {
    conn.execute("DELETE FROM users WHERE id = ?1", [user_id])?;
    Ok(())
}

// --- Sessions ---

pub struct Session {
    pub user: User,
    pub csrf: String,
    pub id_hash: Vec<u8>,
}

/// Creates a session; returns the cookie value.
pub fn create_session(conn: &Connection, user_id: &str, ip: &str, user_agent: &str) -> Result<String> {
    let cookie = b64(&random_bytes::<32>());
    let now = now_s();
    conn.execute(
        "INSERT INTO sessions (id_hash, user_id, csrf, created_at, last_seen_at, ip, user_agent) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
        params![sha256(cookie.as_bytes()), user_id, b64(&random_bytes::<16>()), now, ip, user_agent.chars().take(200).collect::<String>()],
    )?;
    Ok(cookie)
}

pub fn session(conn: &Connection, cookie: &str) -> Result<Option<Session>> {
    let hash = sha256(cookie.as_bytes());
    let now = now_s();
    let found = conn
        .query_row(
            &format!(
                "SELECT s.csrf, s.created_at, s.last_seen_at, u.{} FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.id_hash = ?1",
                USER_COLS.replace(", ", ", u.")
            ),
            [&hash],
            |r| {
                let user = User {
                    id: r.get(3)?,
                    username: r.get(4)?,
                    password_hash: r.get(5)?,
                    is_admin: r.get::<_, i64>(6)? != 0,
                    disabled: r.get::<_, i64>(7)? != 0,
                    created_at: r.get(8)?,
                };
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, user))
            },
        )
        .optional()?;
    let Some((csrf, created, last_seen, user)) = found else { return Ok(None) };
    if user.disabled || now - created > SESSION_MAX_AGE_S || now - last_seen > SESSION_IDLE_S {
        conn.execute("DELETE FROM sessions WHERE id_hash = ?1", [&hash])?;
        return Ok(None);
    }
    if now - last_seen > 60 {
        conn.execute("UPDATE sessions SET last_seen_at = ?2 WHERE id_hash = ?1", params![hash, now])?;
    }
    Ok(Some(Session { user, csrf, id_hash: hash }))
}

pub fn delete_session(conn: &Connection, id_hash: &[u8]) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE id_hash = ?1", [id_hash])?;
    Ok(())
}

pub fn delete_user_sessions(conn: &Connection, user_id: &str) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    Ok(())
}

// --- Invitations and password reset links ---

pub const INVITE_SIGNUP: &str = "signup";
pub const INVITE_RESET: &str = "reset";

pub struct Invite {
    pub code_hash: Vec<u8>,
    pub purpose: String,
    pub user_id: Option<String>,
}

pub fn create_invite(conn: &Connection, purpose: &str, user_id: Option<&str>, created_by: &str) -> Result<String> {
    let code = b64(&random_bytes::<24>());
    conn.execute(
        "INSERT INTO invites (code_hash, purpose, user_id, created_by, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![sha256(code.as_bytes()), purpose, user_id, created_by, now_s() + INVITE_TTL_S],
    )?;
    Ok(code)
}

pub fn invite(conn: &Connection, code: &str) -> Result<Option<Invite>> {
    Ok(conn
        .query_row(
            "SELECT code_hash, purpose, user_id FROM invites WHERE code_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
            params![sha256(code.as_bytes()), now_s()],
            |r| Ok(Invite { code_hash: r.get(0)?, purpose: r.get(1)?, user_id: r.get(2)? }),
        )
        .optional()?)
}

pub fn use_invite(conn: &Connection, code_hash: &[u8]) -> Result<()> {
    conn.execute("UPDATE invites SET used_at = ?2 WHERE code_hash = ?1", params![code_hash, now_s()])?;
    Ok(())
}

// --- Devices and pairing ---

#[derive(Debug, Clone)]
pub struct Device {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub platform: String,
    pub app_version: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub last_ip: Option<String>,
    pub revoked_at: Option<i64>,
}

const DEVICE_COLS: &str = "id, user_id, name, platform, app_version, created_at, last_seen_at, last_ip, revoked_at";

fn device_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        platform: r.get(3)?,
        app_version: r.get(4)?,
        created_at: r.get(5)?,
        last_seen_at: r.get(6)?,
        last_ip: r.get(7)?,
        revoked_at: r.get(8)?,
    })
}

/// A new one-time pairing code (returned in clear once; stored hashed). Returns (code, expiry).
pub fn create_pairing_code(conn: &Connection, user_id: &str, device_name: Option<&str>) -> Result<(String, i64)> {
    let code = new_pairing_code();
    let expires = now_s() + PAIRING_TTL_S;
    conn.execute(
        "INSERT INTO pairing_codes (code_hash, user_id, device_name, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![sha256(code.as_bytes()), user_id, device_name, expires],
    )?;
    Ok((code, expires))
}

/// Redeems a pairing code: creates the device and returns (token, device, user), or None if the code is
/// unknown, expired or used, or the user is disabled.
pub fn redeem_pairing_code(
    conn: &mut Connection,
    code: &str,
    name: &str,
    platform: &str,
    app_version: &str,
) -> Result<Option<(String, Device, User)>> {
    let hash = sha256(normalize_pairing_code(code).as_bytes());
    let tx = conn.transaction()?;
    let found: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT user_id, device_name FROM pairing_codes WHERE code_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
            params![hash, now_s()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((user_id, preset_name)) = found else { return Ok(None) };
    let Some(user) = user_by_id(&tx, &user_id)? else { return Ok(None) };
    if user.disabled {
        return Ok(None);
    }
    let token = format!("wcs_{}", b64(&random_bytes::<32>()));
    let device_name = preset_name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| name.to_string());
    let device = Device {
        id: new_id("dev"),
        user_id: user_id.clone(),
        name: device_name,
        platform: platform.to_string(),
        app_version: app_version.to_string(),
        created_at: now_s(),
        last_seen_at: None,
        last_ip: None,
        revoked_at: None,
    };
    tx.execute(
        "INSERT INTO devices (id, user_id, name, platform, app_version, token_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![device.id, user_id, device.name, device.platform, device.app_version, sha256(token.as_bytes()), device.created_at],
    )?;
    tx.execute("UPDATE pairing_codes SET used_at = ?2 WHERE code_hash = ?1", params![hash, now_s()])?;
    tx.commit()?;
    Ok(Some((token, device, user)))
}

/// The active device (and its enabled user) for a token.
pub fn device_by_token(conn: &Connection, token: &str) -> Result<Option<(Device, User)>> {
    let found = conn
        .query_row(
            &format!("SELECT {DEVICE_COLS} FROM devices WHERE token_hash = ?1 AND revoked_at IS NULL"),
            [sha256(token.as_bytes())],
            device_from_row,
        )
        .optional()?;
    let Some(device) = found else { return Ok(None) };
    match user_by_id(conn, &device.user_id)? {
        Some(user) if !user.disabled => Ok(Some((device, user))),
        _ => Ok(None),
    }
}

pub fn list_devices(conn: &Connection, user_id: &str) -> Result<Vec<Device>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEVICE_COLS} FROM devices WHERE user_id = ?1 ORDER BY revoked_at IS NOT NULL, created_at DESC"
    ))?;
    let rows = stmt.query_map([user_id], device_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Revokes one of the user's devices; false if it isn't theirs (or already revoked).
pub fn revoke_device(conn: &Connection, user_id: &str, device_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE devices SET revoked_at = ?3 WHERE id = ?2 AND user_id = ?1 AND revoked_at IS NULL",
        params![user_id, device_id, now_s()],
    )? == 1)
}

pub fn rename_device(conn: &Connection, user_id: &str, device_id: &str, name: &str) -> Result<bool> {
    Ok(conn.execute("UPDATE devices SET name = ?3 WHERE id = ?2 AND user_id = ?1", params![user_id, device_id, name])? == 1)
}

pub fn touch_device(conn: &Connection, device_id: &str, ip: &str) -> Result<()> {
    conn.execute("UPDATE devices SET last_seen_at = ?2, last_ip = ?3 WHERE id = ?1", params![device_id, now_s(), ip])?;
    Ok(())
}

// --- Keyring ---

pub fn keyring(conn: &Connection, user_id: &str) -> Result<Option<wisprcheap_sync::protocol::Keyring>> {
    let row: Option<(i64, String, String, String, String)> = conn
        .query_row(
            "SELECT key_version, key_id, salt, kdf, wrapped_key FROM keyrings WHERE user_id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(match row {
        None => None,
        Some((key_version, key_id, salt, kdf, wrapped_key)) => Some(wisprcheap_sync::protocol::Keyring {
            key_version,
            key_id,
            salt,
            kdf: serde_json::from_str(&kdf)?,
            wrapped_key,
        }),
    })
}

pub fn put_keyring(conn: &Connection, user_id: &str, version: i64, req: &wisprcheap_sync::protocol::PutKeyringRequest) -> Result<()> {
    conn.execute(
        "INSERT INTO keyrings (user_id, key_version, key_id, salt, kdf, wrapped_key, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(user_id) DO UPDATE SET key_version = ?2, key_id = ?3, salt = ?4, kdf = ?5, wrapped_key = ?6, updated_at = ?7",
        params![user_id, version, req.key_id, req.salt, serde_json::to_string(&req.kdf)?, req.wrapped_key, now_s()],
    )?;
    Ok(())
}

/// "Reset encryption": forgets the keyring and every encrypted record; the statistics stay.
pub fn reset_encryption(conn: &Connection, user_id: &str) -> Result<()> {
    conn.execute("DELETE FROM keyrings WHERE user_id = ?1", [user_id])?;
    conn.execute("DELETE FROM records WHERE user_id = ?1", [user_id])?;
    Ok(())
}

// --- Audit log ---

pub struct AuditEntry {
    pub at: i64,
    pub username: Option<String>,
    pub device: Option<String>,
    pub event: String,
    pub detail: Option<String>,
    pub ip: Option<String>,
}

pub fn audit(conn: &Connection, user_id: Option<&str>, device_id: Option<&str>, event: &str, detail: Option<&str>, ip: &str) {
    let result = conn.execute(
        "INSERT INTO audit_log (user_id, device_id, event, detail, ip, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![user_id, device_id, event, detail, ip, now_s()],
    );
    if let Err(e) = result {
        tracing::error!("audit log: {e}");
    }
}

pub fn audit_list(conn: &Connection, user_id: Option<&str>, limit: i64) -> Result<Vec<AuditEntry>> {
    let sql = "SELECT a.at, u.username, d.name, a.event, a.detail, a.ip FROM audit_log a \
               LEFT JOIN users u ON u.id = a.user_id LEFT JOIN devices d ON d.id = a.device_id \
               WHERE (?1 IS NULL OR a.user_id = ?1) ORDER BY a.id DESC LIMIT ?2";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![user_id, limit], |r| {
        Ok(AuditEntry { at: r.get(0)?, username: r.get(1)?, device: r.get(2)?, event: r.get(3)?, detail: r.get(4)?, ip: r.get(5)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// --- Meta ---

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", params![key, value])?;
    Ok(())
}

pub fn meta_delete(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [key])?;
    Ok(())
}
