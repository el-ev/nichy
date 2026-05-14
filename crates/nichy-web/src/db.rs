use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

pub type Db = Arc<Mutex<Connection>>;

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn open(path: &Path) -> rusqlite::Result<Db> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(Arc::new(Mutex::new(conn)))
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current < 1 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS snippets (
    id          TEXT PRIMARY KEY,
    is_type     INTEGER NOT NULL,
    content     TEXT NOT NULL,
    target      TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cache (
    key_hash    TEXT PRIMARY KEY,
    types_json  BLOB NOT NULL,
    last_used   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS cache_last_used_idx ON cache(last_used);

CREATE TABLE IF NOT EXISTS stats_counter (
    key         TEXT PRIMARY KEY,
    value       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS stats_by_target (
    target      TEXT PRIMARY KEY,
    count       INTEGER NOT NULL
);
";

#[cfg(test)]
pub fn open_in_memory() -> Db {
    let conn = Connection::open_in_memory().expect("open in-memory");
    migrate(&conn).expect("migrate");
    Arc::new(Mutex::new(conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let v1: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        migrate(&conn).unwrap();
        let v2: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v1, v2);
    }
}
