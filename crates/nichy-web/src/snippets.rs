use rusqlite::{OptionalExtension, params};

use crate::db::{Db, unix_now};
use crate::hash::content_hash;

const ID_MIN_LEN: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub id: String,
    pub is_type_expr: bool,
    pub content: String,
    pub target: String,
}

pub struct SnippetStore {
    db: Db,
}

impl SnippetStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn put(&self, is_type_expr: bool, content: &str, target: &str) -> Option<String> {
        let full_hash = content_hash(is_type_expr, content, target);
        let conn = self.db.lock().unwrap();
        let mut select_stmt = conn
            .prepare_cached("SELECT is_type, content, target FROM snippets WHERE id = ?1")
            .expect("prepare snippet select");
        let mut insert_stmt = conn
            .prepare_cached(
                "INSERT INTO snippets (id, is_type, content, target, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .expect("prepare snippet insert");
        for len in ID_MIN_LEN..=full_hash.len() {
            let id = &full_hash[..len];
            let existing: Option<(i64, String, String)> = select_stmt
                .query_row(params![id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .optional()
                .ok()
                .flatten();

            match existing {
                Some((is_type, c, t))
                    if (is_type != 0) == is_type_expr && c == content && t == target =>
                {
                    return Some(id.to_string());
                }
                Some(_) => continue,
                None => {
                    let _ = insert_stmt.execute(params![
                        id,
                        i64::from(is_type_expr),
                        content,
                        target,
                        unix_now() as i64
                    ]);
                    return Some(id.to_string());
                }
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT COUNT(*) FROM snippets")
            .expect("prepare snippets len");
        stmt.query_row([], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    pub fn get(&self, id: &str) -> Option<Snippet> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT id, is_type, content, target FROM snippets WHERE id = ?1")
            .expect("prepare snippet get");
        stmt.query_row(params![id], |r| {
            Ok(Snippet {
                id: r.get(0)?,
                is_type_expr: r.get::<_, i64>(1)? != 0,
                content: r.get(2)?,
                target: r.get(3)?,
            })
        })
        .optional()
        .ok()
        .flatten()
    }
}

pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 52
        && id
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn put_then_get_round_trips() {
        let store = SnippetStore::new(open_in_memory());
        let id = store.put(false, "struct X;", "").expect("put");
        let s = store.get(&id).expect("get");
        assert_eq!(s.content, "struct X;");
        assert_eq!(s.target, "");
        assert!(!s.is_type_expr);
        assert!(s.id.len() >= ID_MIN_LEN);
    }

    #[test]
    fn put_is_idempotent() {
        let store = SnippetStore::new(open_in_memory());
        let id1 = store.put(true, "Option<u8>", "").expect("first");
        let id2 = store.put(true, "Option<u8>", "").expect("second");
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_content_yields_different_id() {
        let store = SnippetStore::new(open_in_memory());
        let id1 = store.put(false, "struct A;", "").expect("a");
        let id2 = store.put(false, "struct B;", "").expect("b");
        assert_ne!(id1, id2);
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let store = SnippetStore::new(open_in_memory());
        assert!(store.get("aaaaaaaa").is_none());
    }

    #[test]
    fn is_valid_id_rejects_bad_chars() {
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("ABCDEFGH")); // uppercase outside alphabet
        assert!(!is_valid_id("ab1defgh")); // '1' is not in the base32 alphabet
        assert!(!is_valid_id("ab/defgh"));
        assert!(is_valid_id("abcdefgh"));
        assert!(is_valid_id("abc23456"));
    }

    #[test]
    fn put_grows_id_on_prefix_collision() {
        let store = SnippetStore::new(open_in_memory());
        let real_hash = content_hash(false, "real content", "");
        let collision_id = &real_hash[..ID_MIN_LEN];
        {
            let conn = store.db.lock().unwrap();
            conn.execute(
                "INSERT INTO snippets (id, is_type, content, target, created_at) \
                 VALUES (?1, 0, 'other content', '', 0)",
                params![collision_id],
            )
            .expect("inject collision row");
        }
        let id = store.put(false, "real content", "").expect("put");
        assert!(id.len() > ID_MIN_LEN, "should grow past collision; got id {id}");
        assert!(
            real_hash.starts_with(&id),
            "grown id must be a prefix of the real hash: {id} vs {real_hash}",
        );
    }
}
