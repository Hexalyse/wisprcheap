//! Credential cleanup, opt-in retention and observable online backups.
use crate::{AppState, Db, db, util::now_s};
use anyhow::Result;
use rusqlite::params;
use std::path::{Path, PathBuf};

pub fn cleanup(state: &AppState) -> Result<()> {
    let mut conn = state.db.lock();
    let tx = conn.transaction()?;
    let now = now_s();
    tx.execute(
        "DELETE FROM sessions WHERE created_at < ?1 OR last_seen_at < ?2",
        params![now - db::SESSION_MAX_AGE_S, now - db::SESSION_IDLE_S],
    )?;
    tx.execute("DELETE FROM invites WHERE expires_at <= ?1 OR used_at IS NOT NULL", [now])?;
    tx.execute("DELETE FROM pairing_codes WHERE expires_at <= ?1", [now])?;
    if state.config.audit_retention_days > 0 {
        tx.execute(
            "DELETE FROM audit_log WHERE at < ?1",
            [now - i64::from(state.config.audit_retention_days) * 86_400],
        )?;
    }
    if state.config.history_retention_days > 0 {
        let cutoff = (now - i64::from(state.config.history_retention_days) * 86_400) * 1000;
        let targets:Vec<(String,String)>=tx.prepare("SELECT h.user_id,h.entry_id FROM history_stats h JOIN records r \
            ON r.user_id=h.user_id AND r.kind='history' AND r.id=h.entry_id WHERE h.timestamp_ms<?1 AND r.deleted=0 LIMIT 1000")?
            .query_map([cutoff],|r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut seq: i64 = db::meta_get(&tx, "sequence")?.and_then(|v| v.parse().ok()).unwrap_or(0);
        for (user, id) in targets {
            seq += 1;
            tx.execute("UPDATE records SET deleted=1,payload=NULL,seq=?3,updated_at=?4 WHERE user_id=?1 AND kind='history' AND id=?2",params![user,id,seq,now])?;
        }
        // Keep tombstones so reconnecting devices cannot resurrect expired history.
        tx.execute("DELETE FROM history_stats WHERE timestamp_ms<?1 AND NOT EXISTS \
            (SELECT 1 FROM records r WHERE r.user_id=history_stats.user_id AND r.kind='history' AND r.id=history_stats.entry_id AND r.deleted=0)",[cutoff])?;
        db::meta_set(&tx, "sequence", &seq.to_string())?;
    }
    db::meta_set(&tx, "last_cleanup", &now.to_string())?;
    tx.commit()?;
    state.limiter.cleanup();
    Ok(())
}

pub fn backup(database: &Db, dir: &Path) -> Result<PathBuf> {
    db::meta_set(&database.lock(), "backup_last_attempt", &now_s().to_string())?;
    let operation = || -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let target = dir.join(format!("wisprcheap-{}.db", chrono::Utc::now().format("%Y%m%d-%H%M%S-%f")));
        if let Err(e) = database.backup_to(&target) {
            let _ = std::fs::remove_file(&target);
            return Err(e);
        }
        let mut old: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("wisprcheap-") && n.ends_with(".db"))
            })
            .collect();
        old.sort();
        while old.len() > 7 {
            std::fs::remove_file(old.remove(0))?;
        }
        Ok(target)
    };
    let result = operation();
    let conn = database.lock();
    match &result {
        Ok(target) => {
            db::meta_set(&conn, "backup_last_success", &now_s().to_string())?;
            db::meta_set(&conn, "backup_last_path", &target.display().to_string())?;
            db::meta_delete(&conn, "backup_last_error")?;
        }
        Err(e) => db::meta_set(&conn, "backup_last_error", &format!("{e:#}"))?,
    }
    result
}

pub fn disk_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .iter()
    .filter_map(|p| std::fs::metadata(p).ok())
    .map(|m| m.len())
    .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppState, Config};
    #[test]
    fn cleanup_retention_and_backup_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::for_tests(dir.path());
        config.history_retention_days = 7;
        config.audit_retention_days = 7;
        let database = Db::open(&config.db_path()).unwrap();
        let state = AppState::new(config, database);
        {
            let conn = state.db.lock();
            let user = db::create_user(&conn, "test", "hash", true).unwrap();
            let old = (now_s() - 8 * 86_400) * 1000;
            conn.execute(
                "INSERT INTO history_stats(user_id,entry_id,ts,stats,timestamp_ms) VALUES (?1,'old','old','{}',?2)",
                params![user, old],
            )
            .unwrap();
            conn.execute("INSERT INTO records(user_id,kind,id,hlc,deleted,payload,seq,updated_at) VALUES (?1,'history','old','clock',0,'e1.old',1,?2)",params![user,old/1000]).unwrap();
            db::meta_set(&conn, "sequence", "1").unwrap();
            let cookie = db::create_session(&conn, &user, "ip", "agent").unwrap();
            conn.execute("UPDATE sessions SET last_seen_at=?1", [now_s() - db::SESSION_IDLE_S - 1]).unwrap();
            assert!(!cookie.is_empty());
            db::create_pairing_code(&conn, &user, None).unwrap();
            conn.execute("UPDATE pairing_codes SET expires_at=?1", [now_s() - 1]).unwrap();
            db::audit(&conn, Some(&user), None, "old_event", None, "ip");
            conn.execute("UPDATE audit_log SET at=?1", [now_s() - 8 * 86_400]).unwrap();
        }
        cleanup(&state).unwrap();
        {
            let conn = state.db.lock();
            for table in ["history_stats", "sessions", "pairing_codes", "audit_log"] {
                assert_eq!(
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(),
                    0
                );
            }
            let (deleted, payload, seq): (i64, Option<String>, i64) = conn
                .query_row("SELECT deleted,payload,seq FROM records", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap();
            assert_eq!((deleted, payload, seq), (1, None, 2));
        }
        let path = backup(&state.db, &dir.path().join("backups")).unwrap();
        let restored = rusqlite::Connection::open(path).unwrap();
        assert_eq!(restored.query_row("SELECT COUNT(*) FROM records", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(db::meta_get(&state.db.lock(), "backup_last_success").unwrap().is_some());
        let bad = dir.path().join("file-not-directory");
        std::fs::write(&bad, "x").unwrap();
        assert!(backup(&state.db, &bad).is_err());
        assert!(db::meta_get(&state.db.lock(), "backup_last_error").unwrap().is_some());
        assert!(db::meta_get(&state.db.lock(), "backup_last_success").unwrap().is_some());
        assert!(disk_bytes(&state.config.db_path()) >= std::fs::metadata(state.config.db_path()).unwrap().len());
    }
}
