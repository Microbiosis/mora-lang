//! v0.50: SqliteSaver — durable SQLite checkpoint storage.
//!
//! Enabled by the `checkpoint-sqlite` Cargo feature.
//!
//! Schema:
//! ```text
//! checkpoints(
//!   thread_id  TEXT NOT NULL,
//!   id         TEXT NOT NULL,
//!   v          INTEGER NOT NULL,
//!   step       INTEGER NOT NULL,
//!   data_json  TEXT NOT NULL,  -- full Checkpoint JSON blob
//!   timestamp_ms INTEGER NOT NULL,
//!   PRIMARY KEY (thread_id, id)
//! )
//! ```

use super::{Checkpoint, CheckpointSaver};
use crate::error::MoraError;
use parking_lot::Mutex;
use rusqlite::OptionalExtension;

pub struct SqliteSaver {
    conn: Mutex<rusqlite::Connection>,
}

impl SqliteSaver {
    /// Open (or create) a SQLite database at `path` and initialise the schema.
    ///
    /// Use `":memory:"` for an in-memory database (useful in tests).
    pub fn new(path: &str) -> Result<Self, MoraError> {
        let conn = rusqlite::Connection::open(path).map_err(|e| MoraError::Other(e.to_string()))?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS checkpoints (
                thread_id     TEXT NOT NULL,
                id            TEXT NOT NULL,
                v             INTEGER NOT NULL,
                step          INTEGER NOT NULL,
                data_json     TEXT NOT NULL,
                timestamp_ms  INTEGER NOT NULL,
                PRIMARY KEY (thread_id, id)
            )",
            rusqlite::params![],
        )
        .map_err(|e| MoraError::Other(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl CheckpointSaver for SqliteSaver {
    fn save(&self, thread_id: &str, checkpoint: &Checkpoint) -> Result<(), MoraError> {
        let data_json = checkpoint.to_json()?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO checkpoints
             (thread_id, id, v, step, data_json, timestamp_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                thread_id,
                checkpoint.id,
                checkpoint.v as i64,
                checkpoint.step as i64,
                data_json,
                checkpoint.timestamp_ms as i64,
            ],
        )
        .map_err(|e| MoraError::Other(e.to_string()))?;
        Ok(())
    }

    fn load(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<Checkpoint>, MoraError> {
        let conn = self.conn.lock();
        let data_json: Option<String> = if let Some(id) = checkpoint_id {
            conn.query_row(
                "SELECT data_json FROM checkpoints
                 WHERE thread_id = ?1 AND id = ?2",
                rusqlite::params![thread_id, id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
        } else {
            // v0.104.6 D234：`ORDER BY` 必须给**全序**的三级键。
            //
            // 修前只有 `ORDER BY step DESC` —— step 相同时 SQLite 不保证
            // 返回哪一行（实测取的是先插入的 `a`），而 `MemorySaver` 的
            // `max_by_key(step)` 取的是**后**插入的 `b` ⇒ 同一份数据
            // 经两个 saver 恢复出**不同状态**。
            //
            // 同 step 并不罕见：`pregel` 的 fault-retry 会重跑同一步，
            // 两条路径都往同一步写检查点。
            //
            // 三级键 `(step DESC, timestamp_ms DESC, id DESC)` 与
            // `MemorySaver::load` 的 `max_by` 逐项对应，且 `id` 保证
            // 完全并列时也有确定结果。
            conn.query_row(
                "SELECT data_json FROM checkpoints
                 WHERE thread_id = ?1
                 ORDER BY step DESC, timestamp_ms DESC, id DESC
                 LIMIT 1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
        };

        match data_json {
            None => Ok(None),
            Some(json) => {
                let checkpoint = Checkpoint::from_json(&json)?;
                Ok(Some(checkpoint))
            }
        }
    }

    fn list(&self, thread_id: &str) -> Result<Vec<String>, MoraError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                // v0.104.6 D234：与 `MemorySaver::list` 逐项对应的全序三级键
                "SELECT id FROM checkpoints
                 WHERE thread_id = ?1
                 ORDER BY step ASC, timestamp_ms ASC, id ASC",
            )
            .map_err(|e| MoraError::Other(e.to_string()))?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![thread_id], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| MoraError::Other(e.to_string()))?;
        Ok(ids)
    }

    fn delete(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), MoraError> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM checkpoints
             WHERE thread_id = ?1 AND id = ?2",
            rusqlite::params![thread_id, checkpoint_id],
        )
        .map_err(|e| MoraError::Other(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a minimal checkpoint for testing.
    fn make_checkpoint(
        id: &str,
        thread_id: &str,
        step: usize,
        channel_values: HashMap<String, crate::value::Value>,
    ) -> Checkpoint {
        Checkpoint {
            id: id.to_string(),
            v: 1,
            thread_id: thread_id.to_string(),
            step,
            channel_values,
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            pending_sends: vec![],
            timestamp_ms: 0,
        }
    }

    #[test]
    fn sqlite_save_and_load() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        let mut values = HashMap::new();
        values.insert("x".to_string(), crate::value::Value::Int(42));
        let cp = make_checkpoint("a", "t1", 1, values);

        saver.save("t1", &cp).unwrap();
        let loaded = saver.load("t1", Some("a")).unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.id, "a");
        assert_eq!(loaded.step, 1);
        assert_eq!(
            loaded.channel_values.get("x"),
            Some(&crate::value::Value::Int(42))
        );
    }

    #[test]
    fn sqlite_load_latest_without_id() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        saver
            .save("t1", &make_checkpoint("a", "t1", 0, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 2, HashMap::new()))
            .unwrap();

        let latest = saver.load("t1", None).unwrap();
        assert_eq!(latest.unwrap().id, "b");
    }

    #[test]
    fn sqlite_list_and_delete() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 2, HashMap::new()))
            .unwrap();

        let ids = saver.list("t1").unwrap();
        assert_eq!(ids, vec!["a", "b"]);

        saver.delete("t1", "a").unwrap();
        let ids = saver.list("t1").unwrap();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn sqlite_thread_isolation() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t2", &make_checkpoint("b", "t2", 2, HashMap::new()))
            .unwrap();

        assert_eq!(saver.list("t1").unwrap(), vec!["a"]);
        assert_eq!(saver.list("t2").unwrap(), vec!["b"]);
    }

    #[test]
    fn sqlite_roundtrip_json_with_complex_state() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        let mut values = HashMap::new();
        values.insert(
            "messages".to_string(),
            // v0.104.6：`Value::List` 已改用分块存储的 `list::List`
            // （`Value::List(list::List)`），不再是 `Vec<Value>`。此处是
            // 本会话早期 List 改造遗留的编译错误 —— `cargo test`（默认
            // features）碰不到本 target，`cargo clippy --all-targets
            // --all-features` 才暴露。
            crate::value::Value::List(
                vec![
                    crate::value::Value::String("hello".to_string()),
                    crate::value::Value::String("world".to_string()),
                ]
                .into(),
            ),
        );
        values.insert(
            "score".to_string(),
            crate::value::Value::Float(std::f64::consts::PI),
        );

        let mut channel_versions = HashMap::new();
        channel_versions.insert("messages".to_string(), 2);
        let mut versions_seen = HashMap::new();
        let mut inner = HashMap::new();
        inner.insert("messages".to_string(), 1);
        versions_seen.insert("node_a".to_string(), inner);

        let cp = Checkpoint {
            id: "roundtrip-id".to_string(),
            v: 1,
            thread_id: "t1".to_string(),
            step: 3,
            channel_values: values,
            channel_versions,
            versions_seen,
            pending_sends: vec![crate::checkpoint::SendTask {
                target_node: "next".to_string(),
                input: crate::value::Value::Dict(
                    [(
                        "key".to_string(),
                        crate::value::Value::String("val".to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            }],
            timestamp_ms: 999,
        };

        saver.save("t1", &cp).unwrap();
        let loaded = saver.load("t1", Some("roundtrip-id")).unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded, cp);
    }

    #[test]
    fn sqlite_delete_nonexistent_is_noop() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        saver.delete("t1", "ghost").unwrap();
    }

    #[test]
    fn sqlite_load_nonexistent_returns_none() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        let result = saver.load("t1", Some("ghost")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn sqlite_save_replaces_existing_id() {
        let saver = SqliteSaver::new(":memory:").unwrap();
        let mut cp1 = make_checkpoint("dup", "t1", 1, HashMap::new());
        cp1.v = 1;
        saver.save("t1", &cp1).unwrap();

        let mut cp2 = make_checkpoint("dup", "t1", 1, HashMap::new());
        cp2.v = 2;
        saver.save("t1", &cp2).unwrap();

        let loaded = saver.load("t1", Some("dup")).unwrap().unwrap();
        assert_eq!(loaded.v, 2);
    }
}
