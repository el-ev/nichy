use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::params;
use serde::Serialize;

use crate::db::{Db, unix_now};

const MAX_TARGETS: usize = 256;
const MAX_TARGET_LEN: usize = 96;

#[derive(Default, Serialize, Clone)]
pub struct StatsData {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub code_requests: u64,
    pub type_requests: u64,
    pub forbidden: u64,
    pub timeouts: u64,
    pub analysis_errors: u64,
    pub bad_requests: u64,
    pub internal_errors: u64,
    pub total_types_analyzed: u64,
    pub by_target: BTreeMap<String, u64>,
    pub first_seen_unix: Option<u64>,
    pub last_request_unix: Option<u64>,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub shortlinks_created: u64,
    pub shortlinks_loaded: u64,
}

#[derive(Serialize)]
pub struct StatsSnapshot {
    #[serde(flatten)]
    pub data: StatsData,
    pub session_started_unix: u64,
    pub now_unix: u64,
    pub cache_size: usize,
    pub cache_capacity: usize,
    pub snippets_total: usize,
}

pub enum Outcome<'a> {
    Success {
        types_count: u64,
        target: Option<&'a str>,
        is_type_expr: bool,
    },
    Forbidden { is_type_expr: bool },
    Timeout { is_type_expr: bool },
    AnalysisError { is_type_expr: bool },
    BadRequest,
    InternalError { is_type_expr: bool },
}

pub struct Stats {
    db: Db,
    session_started_unix: u64,
}

impl Stats {
    pub fn new(db: Db) -> Arc<Self> {
        let now = unix_now();
        {
            let conn = db.lock().unwrap();
            let _ = conn.execute(
                "INSERT INTO stats_counter (key, value) VALUES ('first_seen_unix', ?1) \
                 ON CONFLICT(key) DO NOTHING",
                params![now as i64],
            );
        }
        Arc::new(Self {
            db,
            session_started_unix: now,
        })
    }

    pub fn record(&self, outcome: Outcome<'_>) {
        let now = unix_now();
        let mut updates: Vec<(&'static str, i64)> = vec![
            ("total_requests", 1),
            ("last_request_unix", now as i64),
        ];

        let mut bump_target: Option<String> = None;
        let mode = match outcome {
            Outcome::Success { types_count, target, is_type_expr } => {
                updates.push(("successful_requests", 1));
                updates.push(("total_types_analyzed", types_count as i64));
                bump_target = target.and_then(sanitize_target);
                Some(is_type_expr)
            }
            Outcome::Forbidden { is_type_expr } => {
                updates.push(("failed_requests", 1));
                updates.push(("forbidden", 1));
                Some(is_type_expr)
            }
            Outcome::Timeout { is_type_expr } => {
                updates.push(("failed_requests", 1));
                updates.push(("timeouts", 1));
                Some(is_type_expr)
            }
            Outcome::AnalysisError { is_type_expr } => {
                updates.push(("failed_requests", 1));
                updates.push(("analysis_errors", 1));
                Some(is_type_expr)
            }
            Outcome::InternalError { is_type_expr } => {
                updates.push(("failed_requests", 1));
                updates.push(("internal_errors", 1));
                Some(is_type_expr)
            }
            Outcome::BadRequest => {
                updates.push(("failed_requests", 1));
                updates.push(("bad_requests", 1));
                None
            }
        };
        match mode {
            Some(true) => updates.push(("type_requests", 1)),
            Some(false) => updates.push(("code_requests", 1)),
            None => {}
        }

        let mut conn = self.db.lock().unwrap();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                eprintln!("nichy-web: failed to begin stats transaction: {e}");
                return;
            }
        };
        for (key, delta) in updates {
            apply_counter(&tx, key, delta);
        }
        if let Some(target) = bump_target {
            bump_target_count(&tx, &target);
        }
        if let Err(e) = tx.commit() {
            eprintln!("nichy-web: failed to commit stats transaction: {e}");
        }
    }

    pub fn record_cache_hit(&self) {
        let conn = self.db.lock().unwrap();
        apply_counter(&conn, "cache_hits", 1);
    }

    pub fn record_cache_miss(&self) {
        let conn = self.db.lock().unwrap();
        apply_counter(&conn, "cache_misses", 1);
    }

    pub fn record_shortlink_created(&self) {
        let conn = self.db.lock().unwrap();
        apply_counter(&conn, "shortlinks_created", 1);
    }

    pub fn record_shortlink_loaded(&self) {
        let conn = self.db.lock().unwrap();
        apply_counter(&conn, "shortlinks_loaded", 1);
    }

    pub fn snapshot(
        &self,
        cache_size: usize,
        cache_capacity: usize,
        snippets_total: usize,
    ) -> StatsSnapshot {
        let conn = self.db.lock().unwrap();
        let mut data = StatsData::default();

        let mut stmt = conn
            .prepare_cached("SELECT key, value FROM stats_counter")
            .expect("prepare snapshot");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .expect("query stats");
        for row in rows.flatten() {
            let v = row.1.max(0) as u64;
            match row.0.as_str() {
                "total_requests" => data.total_requests = v,
                "successful_requests" => data.successful_requests = v,
                "failed_requests" => data.failed_requests = v,
                "code_requests" => data.code_requests = v,
                "type_requests" => data.type_requests = v,
                "forbidden" => data.forbidden = v,
                "timeouts" => data.timeouts = v,
                "analysis_errors" => data.analysis_errors = v,
                "bad_requests" => data.bad_requests = v,
                "internal_errors" => data.internal_errors = v,
                "total_types_analyzed" => data.total_types_analyzed = v,
                "cache_hits" => data.cache_hits = v,
                "cache_misses" => data.cache_misses = v,
                "shortlinks_created" => data.shortlinks_created = v,
                "shortlinks_loaded" => data.shortlinks_loaded = v,
                "first_seen_unix" => data.first_seen_unix = Some(v),
                "last_request_unix" => data.last_request_unix = Some(v),
                _ => {}
            }
        }
        drop(stmt);

        let mut stmt = conn
            .prepare_cached("SELECT target, count FROM stats_by_target")
            .expect("prepare targets");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .expect("query targets");
        for row in rows.flatten() {
            data.by_target.insert(row.0, row.1.max(0) as u64);
        }

        StatsSnapshot {
            data,
            session_started_unix: self.session_started_unix,
            now_unix: unix_now(),
            cache_size,
            cache_capacity,
            snippets_total,
        }
    }
}

const SQL_COUNTER_ABS: &str = "INSERT INTO stats_counter (key, value) VALUES (?1, ?2) \
     ON CONFLICT(key) DO UPDATE SET value = excluded.value";
const SQL_COUNTER_DELTA: &str = "INSERT INTO stats_counter (key, value) VALUES (?1, ?2) \
     ON CONFLICT(key) DO UPDATE SET value = stats_counter.value + excluded.value";

fn apply_counter(conn: &rusqlite::Connection, key: &str, delta: i64) {
    // last_request_unix and first_seen_unix are absolute, not deltas.
    let sql = if key == "last_request_unix" {
        SQL_COUNTER_ABS
    } else {
        SQL_COUNTER_DELTA
    };
    let mut stmt = conn.prepare_cached(sql).expect("prepare counter");
    let _ = stmt.execute(params![key, delta]);
}

fn bump_target_count(conn: &rusqlite::Connection, target: &str) {
    let mut update_stmt = conn
        .prepare_cached("UPDATE stats_by_target SET count = count + 1 WHERE target = ?1")
        .expect("prepare target update");
    let updated = update_stmt.execute(params![target]).unwrap_or(0);
    if updated > 0 {
        return;
    }
    drop(update_stmt);
    let mut count_stmt = conn
        .prepare_cached("SELECT COUNT(*) FROM stats_by_target")
        .expect("prepare target count");
    let total: i64 = count_stmt.query_row([], |r| r.get(0)).unwrap_or(0);
    if (total as usize) >= MAX_TARGETS {
        return;
    }
    drop(count_stmt);
    let mut insert_stmt = conn
        .prepare_cached("INSERT INTO stats_by_target (target, count) VALUES (?1, 1)")
        .expect("prepare target insert");
    let _ = insert_stmt.execute(params![target]);
}

fn sanitize_target(t: &str) -> Option<String> {
    if t.is_empty() || t.len() > MAX_TARGET_LEN {
        return None;
    }
    if !t
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn first_seen_is_persisted_once() {
        let db = open_in_memory();
        let s1 = Stats::new(db.clone());
        let first1 = s1.snapshot(0, 0, 0).data.first_seen_unix;
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let _s2 = Stats::new(db.clone());
        let first2 = _s2.snapshot(0, 0, 0).data.first_seen_unix;
        assert_eq!(first1, first2, "first_seen should not advance across restarts");
    }

    #[test]
    fn success_records_counters_and_target() {
        let stats = Stats::new(open_in_memory());
        stats.record(Outcome::Success {
            types_count: 3,
            target: Some("x86_64-unknown-linux-gnu"),
            is_type_expr: false,
        });
        let snap = stats.snapshot(0, 0, 0);
        assert_eq!(snap.data.total_requests, 1);
        assert_eq!(snap.data.successful_requests, 1);
        assert_eq!(snap.data.code_requests, 1);
        assert_eq!(snap.data.total_types_analyzed, 3);
        assert_eq!(
            snap.data.by_target.get("x86_64-unknown-linux-gnu").copied(),
            Some(1)
        );
    }

    #[test]
    fn failure_paths_increment_specific_buckets() {
        let stats = Stats::new(open_in_memory());
        stats.record(Outcome::Timeout { is_type_expr: true });
        stats.record(Outcome::Forbidden { is_type_expr: false });
        let snap = stats.snapshot(0, 0, 0);
        assert_eq!(snap.data.failed_requests, 2);
        assert_eq!(snap.data.timeouts, 1);
        assert_eq!(snap.data.forbidden, 1);
        assert_eq!(snap.data.type_requests, 1);
        assert_eq!(snap.data.code_requests, 1);
    }

    #[test]
    fn cache_counters_are_independent() {
        let stats = Stats::new(open_in_memory());
        stats.record_cache_hit();
        stats.record_cache_hit();
        stats.record_cache_miss();
        let snap = stats.snapshot(0, 0, 0);
        assert_eq!(snap.data.cache_hits, 2);
        assert_eq!(snap.data.cache_misses, 1);
        // Cache counters must not bump total_requests.
        assert_eq!(snap.data.total_requests, 0);
    }

    #[test]
    fn by_target_stops_growing_at_cap() {
        let stats = Stats::new(open_in_memory());
        for i in 0..(MAX_TARGETS + 5) {
            let t = format!("t{i:04}");
            stats.record(Outcome::Success {
                types_count: 0,
                target: Some(&t),
                is_type_expr: false,
            });
        }
        let snap = stats.snapshot(0, 0, 0);
        assert_eq!(snap.data.by_target.len(), MAX_TARGETS);
        // First MAX_TARGETS targets won; later ones were dropped.
        assert!(snap.data.by_target.contains_key("t0000"));
        assert!(!snap.data.by_target.contains_key(&format!("t{:04}", MAX_TARGETS + 4)));
    }
}
