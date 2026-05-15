use std::sync::Arc;

use rusqlite::params;

use nichy::TypeLayout;

use crate::db::{Db, unix_now};
use crate::hash::content_hash;

#[derive(Clone)]
pub struct CacheKey {
    pub hash: String,
}

impl CacheKey {
    pub fn new(is_type_expr: bool, input: &str, target: Option<&str>) -> Self {
        Self {
            hash: content_hash(is_type_expr, input, target.unwrap_or("")),
        }
    }
}

pub struct AnalysisCache {
    db: Db,
    pub capacity: usize,
}

impl AnalysisCache {
    pub fn new(db: Db, capacity: usize) -> Arc<Self> {
        assert!(capacity > 0, "cache capacity must be > 0");
        Arc::new(Self { db, capacity })
    }

    pub fn get(&self, key: &CacheKey) -> Option<Arc<Vec<TypeLayout>>> {
        let conn = self.db.lock().unwrap();
        // Atomic MRU bump + fetch: returns no row if the key is absent.
        let mut stmt = conn
            .prepare_cached(
                "UPDATE cache SET last_used = ?2 WHERE key_hash = ?1 RETURNING types_json",
            )
            .expect("prepare cache get");
        let row: Option<Vec<u8>> = match stmt
            .query_row(params![&key.hash, unix_now() as i64], |r| {
                r.get::<_, Vec<u8>>(0)
            }) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                eprintln!("nichy-web: cache get failed: {e}");
                return None;
            }
        };
        let bytes = row?;
        match serde_json::from_slice::<Vec<TypeLayout>>(&bytes) {
            Ok(types) => Some(Arc::new(types)),
            Err(e) => {
                eprintln!("nichy-web: cache value not parseable: {e}");
                None
            }
        }
    }

    pub fn put(&self, key: CacheKey, val: Arc<Vec<TypeLayout>>) {
        let json = match serde_json::to_vec(&*val) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("nichy-web: cache value not serializable: {e}");
                return;
            }
        };
        let conn = self.db.lock().unwrap();
        let mut put_stmt = conn
            .prepare_cached(
                "INSERT INTO cache (key_hash, types_json, last_used) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(key_hash) DO UPDATE SET types_json = excluded.types_json, last_used = excluded.last_used",
            )
            .expect("prepare cache put");
        if let Err(e) = put_stmt.execute(params![&key.hash, &json, unix_now() as i64]) {
            eprintln!("nichy-web: cache put failed: {e}");
            return;
        }
        drop(put_stmt);
        // Trim down to capacity by dropping the least-recently-used rows.
        let mut trim_stmt = conn
            .prepare_cached(
                "DELETE FROM cache WHERE key_hash IN (\
                    SELECT key_hash FROM cache ORDER BY last_used DESC LIMIT -1 OFFSET ?1\
                 )",
            )
            .expect("prepare cache trim");
        if let Err(e) = trim_stmt.execute(params![self.capacity as i64]) {
            eprintln!("nichy-web: cache trim failed: {e}");
        }
    }

    pub fn len(&self) -> usize {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT COUNT(*) FROM cache")
            .expect("prepare cache len");
        stmt.query_row([], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn empty_layouts() -> Arc<Vec<TypeLayout>> {
        Arc::new(Vec::new())
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let cache = AnalysisCache::new(open_in_memory(), 4);
        assert!(cache.get(&CacheKey::new(false, "x", None)).is_none());
    }

    #[test]
    fn put_then_get_round_trips() {
        let cache = AnalysisCache::new(open_in_memory(), 4);
        let key = CacheKey::new(false, "struct X;", None);
        cache.put(key.clone(), empty_layouts());
        assert!(cache.get(&key).is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn lru_evicts_oldest_at_capacity() {
        let cache = AnalysisCache::new(open_in_memory(), 2);
        let k1 = CacheKey::new(false, "1", None);
        let k2 = CacheKey::new(false, "2", None);
        let k3 = CacheKey::new(false, "3", None);
        cache.put(k1.clone(), empty_layouts());
        // Make sure last_used distinguishes k1 from k2.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        cache.put(k2.clone(), empty_layouts());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        cache.put(k3.clone(), empty_layouts());
        assert!(cache.get(&k1).is_none(), "oldest entry should be evicted");
        assert!(cache.get(&k2).is_some());
        assert!(cache.get(&k3).is_some());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn key_differs_by_target() {
        let cache = AnalysisCache::new(open_in_memory(), 4);
        let k_default = CacheKey::new(false, "code", None);
        let k_wasm = CacheKey::new(false, "code", Some("wasm32-unknown-unknown"));
        cache.put(k_default.clone(), empty_layouts());
        assert!(cache.get(&k_default).is_some());
        assert!(cache.get(&k_wasm).is_none());
    }

    #[test]
    fn key_differs_by_input_kind() {
        let cache = AnalysisCache::new(open_in_memory(), 4);
        let code = CacheKey::new(false, "Option<u8>", None);
        let typ = CacheKey::new(true, "Option<u8>", None);
        cache.put(code.clone(), empty_layouts());
        assert!(cache.get(&code).is_some());
        assert!(cache.get(&typ).is_none());
    }
}
