//! v0.50: Checkpoint persistence layer for Pregel BSP execution engine.
//!
//! Provides `CheckpointSaver` trait with two backends:
//! - `MemorySaver`: in-memory storage for testing and ephemeral workflows
//! - `SqliteSaver` (feature `checkpoint-sqlite`): file-backed durable storage
//!
//! All operations return `Result` (zero-panic policy). JSON serialization is
//! hand-written via `flow::value_to_json` / `flow::json_to_value` to avoid
//! introducing a serde dependency (consistent with v0.11+ design).

use crate::flow::{json_to_value, value_to_json};
use crate::value::Value;
use std::collections::HashMap;

// ------------------------------------------------------------------
// SendTask
// ------------------------------------------------------------------

/// A dynamic dispatch task queued by a Pregel node (`send` expression).
///
/// When a node returns `Send { target, input }`, the Pregel engine queues it
/// into `pending_sends`. After the UPDATE phase, these tasks are expanded into
/// new active nodes for the next super-step.
#[derive(Debug, Clone, PartialEq)]
pub struct SendTask {
    pub target_node: String,
    pub input: Value,
}

// ------------------------------------------------------------------
// Checkpoint
// ------------------------------------------------------------------

/// A single checkpoint snapshot of Pregel execution state.
///
/// Captures the complete state at the end of a super-step, enabling:
/// - Fault recovery (resume from latest checkpoint)
/// - Time-travel debugging (rewind to any prior step)
/// - Human-in-the-loop (interrupt + resume with `Command`)
#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    /// Unique checkpoint identifier (UUID v4).
    pub id: String,
    /// Schema version for forward-compatibility.
    pub v: u32,
    /// Thread identifier (isolates concurrent orchestrate instances).
    pub thread_id: String,
    /// Super-step index (0 = initial state before first step).
    pub step: usize,
    /// Current channel values (state).
    pub channel_values: HashMap<String, Value>,
    /// Monotonically increasing version per channel.
    pub channel_versions: HashMap<String, u64>,
    /// Last observed version per (node, channel).
    pub versions_seen: HashMap<String, HashMap<String, u64>>,
    /// Dynamic sends queued during this step, processed after UPDATE.
    pub pending_sends: Vec<SendTask>,
    /// Wall-clock timestamp (millis since Unix epoch).
    pub timestamp_ms: u128,
}

impl Checkpoint {
    /// Generate a new random checkpoint ID.
    pub fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Convenience constructor.
    pub fn new(
        thread_id: String,
        step: usize,
        channel_values: HashMap<String, Value>,
        channel_versions: HashMap<String, u64>,
        versions_seen: HashMap<String, HashMap<String, u64>>,
        pending_sends: Vec<SendTask>,
    ) -> Self {
        Self {
            id: Self::new_id(),
            v: 1,
            thread_id,
            step,
            channel_values,
            channel_versions,
            versions_seen,
            pending_sends,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        }
    }

    /// Serialize to JSON string (hand-written, no serde).
    pub fn to_json(&self) -> Result<String, String> {
        let mut map = HashMap::new();

        map.insert("id".to_string(), Value::String(self.id.clone()));
        map.insert("v".to_string(), Value::Int(self.v as i64));
        map.insert(
            "thread_id".to_string(),
            Value::String(self.thread_id.clone()),
        );
        map.insert("step".to_string(), Value::Int(self.step as i64));
        map.insert(
            "channel_values".to_string(),
            Value::Dict(self.channel_values.clone()),
        );

        let channel_versions: HashMap<String, Value> = self
            .channel_versions
            .iter()
            .map(|(k, v)| (k.clone(), Value::Int(*v as i64)))
            .collect();
        map.insert(
            "channel_versions".to_string(),
            Value::Dict(channel_versions),
        );

        let mut versions_seen = HashMap::new();
        for (node, versions) in &self.versions_seen {
            let inner: HashMap<String, Value> = versions
                .iter()
                .map(|(k, v)| (k.clone(), Value::Int(*v as i64)))
                .collect();
            versions_seen.insert(node.clone(), Value::Dict(inner));
        }
        map.insert("versions_seen".to_string(), Value::Dict(versions_seen));

        let pending_sends: Vec<Value> = self
            .pending_sends
            .iter()
            .map(|send| {
                let mut send_map = HashMap::new();
                send_map.insert(
                    "target_node".to_string(),
                    Value::String(send.target_node.clone()),
                );
                send_map.insert("input".to_string(), send.input.clone());
                Value::Dict(send_map)
            })
            .collect();
        map.insert("pending_sends".to_string(), Value::List(pending_sends));

        // u128 does not fit safely in f64 (JSON number), so store as string.
        map.insert(
            "timestamp_ms".to_string(),
            Value::String(self.timestamp_ms.to_string()),
        );

        Ok(value_to_json(&Value::Dict(map)))
    }

    /// Deserialize from JSON string.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let value = json_to_value(s)?;
        let map = match value {
            Value::Dict(m) => m,
            _ => return Err("Checkpoint JSON must be a dict".to_string()),
        };

        let id = match map.get("id") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("Checkpoint id must be a string".to_string()),
        };

        let v = match map.get("v") {
            Some(Value::Int(i)) => *i as u32,
            Some(Value::Number(n)) => *n as u32,
            Some(Value::Float(f)) => *f as u32,
            _ => return Err("Checkpoint v must be a number".to_string()),
        };

        let thread_id = match map.get("thread_id") {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("Checkpoint thread_id must be a string".to_string()),
        };

        let step = match map.get("step") {
            Some(Value::Int(i)) => *i as usize,
            Some(Value::Number(n)) => *n as usize,
            Some(Value::Float(f)) => *f as usize,
            _ => return Err("Checkpoint step must be a number".to_string()),
        };

        let channel_values = match map.get("channel_values") {
            Some(Value::Dict(m)) => m.clone(),
            _ => return Err("Checkpoint channel_values must be a dict".to_string()),
        };

        let channel_versions = match map.get("channel_versions") {
            Some(Value::Dict(m)) => m
                .iter()
                .map(|(k, v)| {
                    let num = match v {
                        Value::Int(i) => *i as u64,
                        Value::Number(n) => *n as u64,
                        Value::Float(f) => *f as u64,
                        _ => {
                            return Err(format!(
                                "channel_versions value must be a number: {:?}",
                                v
                            ));
                        }
                    };
                    Ok((k.clone(), num))
                })
                .collect::<Result<HashMap<String, u64>, String>>()?,
            _ => return Err("Checkpoint channel_versions must be a dict".to_string()),
        };

        let versions_seen = match map.get("versions_seen") {
            Some(Value::Dict(m)) => m
                .iter()
                .map(|(node, v)| {
                    let inner = match v {
                        Value::Dict(inner_map) => inner_map
                            .iter()
                            .map(|(k, v)| {
                                let num = match v {
                                    Value::Int(i) => *i as u64,
                                    Value::Number(n) => *n as u64,
                                    Value::Float(f) => *f as u64,
                                    _ => {
                                        return Err(format!(
                                            "versions_seen value must be a number: {:?}",
                                            v
                                        ));
                                    }
                                };
                                Ok((k.clone(), num))
                            })
                            .collect::<Result<HashMap<String, u64>, String>>()?,
                        _ => return Err(format!("versions_seen inner must be a dict: {:?}", v)),
                    };
                    Ok((node.clone(), inner))
                })
                .collect::<Result<HashMap<String, HashMap<String, u64>>, String>>()?,
            _ => return Err("Checkpoint versions_seen must be a dict".to_string()),
        };

        let pending_sends = match map.get("pending_sends") {
            Some(Value::List(items)) => items
                .iter()
                .map(|item| {
                    let send_map = match item {
                        Value::Dict(m) => m,
                        _ => return Err("pending_sends item must be a dict".to_string()),
                    };
                    let target_node = match send_map.get("target_node") {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("pending_sends target_node must be a string".to_string()),
                    };
                    let input = match send_map.get("input") {
                        Some(v) => v.clone(),
                        None => return Err("pending_sends input missing".to_string()),
                    };
                    Ok(SendTask { target_node, input })
                })
                .collect::<Result<Vec<SendTask>, String>>()?,
            _ => return Err("Checkpoint pending_sends must be a list".to_string()),
        };

        let timestamp_ms = match map.get("timestamp_ms") {
            Some(Value::String(s)) => s
                .parse::<u128>()
                .map_err(|e| format!("Invalid timestamp_ms: {}", e))?,
            Some(Value::Int(i)) => *i as u128,
            Some(Value::Number(n)) => *n as u128,
            Some(Value::Float(f)) => *f as u128,
            _ => return Err("Checkpoint timestamp_ms must be a string or number".to_string()),
        };

        Ok(Checkpoint {
            id,
            v,
            thread_id,
            step,
            channel_values,
            channel_versions,
            versions_seen,
            pending_sends,
            timestamp_ms,
        })
    }
}

// ------------------------------------------------------------------
// CheckpointSaver trait
// ------------------------------------------------------------------

/// Trait for checkpoint persistence backends.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// Pregel engine threads via `Arc<dyn CheckpointSaver>`.
pub trait CheckpointSaver: Send + Sync {
    /// Persist a checkpoint.
    fn save(&self, thread_id: &str, checkpoint: &Checkpoint) -> Result<(), String>;

    /// Load a checkpoint by ID. If `checkpoint_id` is `None`, returns the
    /// latest checkpoint for the thread (highest `step`).
    fn load(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<Checkpoint>, String>;

    /// List all checkpoint IDs for a thread, ordered by step ascending.
    fn list(&self, thread_id: &str) -> Result<Vec<String>, String>;

    /// Delete a checkpoint by ID.
    fn delete(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), String>;
}

// ------------------------------------------------------------------
// High-level helpers: rewind / resume
// ------------------------------------------------------------------

/// Rewind: delete all checkpoints at or after `before_step`.
///
/// This is the "time travel" primitive: after rewinding, the next
/// `resume` will load the last checkpoint before `before_step`.
pub fn rewind(
    saver: &dyn CheckpointSaver,
    thread_id: &str,
    before_step: usize,
) -> Result<(), String> {
    let ids = saver.list(thread_id)?;
    for id in ids {
        if let Some(cp) = saver.load(thread_id, Some(&id))?
            && cp.step >= before_step
        {
            saver.delete(thread_id, &id)?;
        }
    }
    Ok(())
}

/// Resume: load the latest checkpoint for a thread (highest `step`).
///
/// Returns `None` if no checkpoints exist for the thread.
pub fn resume(saver: &dyn CheckpointSaver, thread_id: &str) -> Result<Option<Checkpoint>, String> {
    let ids = saver.list(thread_id)?;
    let mut latest: Option<Checkpoint> = None;
    for id in ids {
        if let Some(cp) = saver.load(thread_id, Some(&id))?
            && latest.as_ref().is_none_or(|l| cp.step > l.step)
        {
            latest = Some(cp);
        }
    }
    Ok(latest)
}

// ------------------------------------------------------------------
// Sub-modules
// ------------------------------------------------------------------

mod memory;
pub use memory::MemorySaver;

#[cfg(feature = "checkpoint-sqlite")]
mod sqlite;
#[cfg(feature = "checkpoint-sqlite")]
pub use sqlite::SqliteSaver;

// ------------------------------------------------------------------
// Unit tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal checkpoint for testing.
    fn make_checkpoint(
        id: &str,
        thread_id: &str,
        step: usize,
        channel_values: HashMap<String, Value>,
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
    fn checkpoint_roundtrip_json() {
        let mut channel_values = HashMap::new();
        channel_values.insert(
            "messages".to_string(),
            Value::List(vec![
                Value::String("hello".to_string()),
                Value::String("world".to_string()),
            ]),
        );
        let mut channel_versions = HashMap::new();
        channel_versions.insert("messages".to_string(), 3);
        let mut versions_seen = HashMap::new();
        let mut inner = HashMap::new();
        inner.insert("messages".to_string(), 2);
        versions_seen.insert("node_a".to_string(), inner);
        let pending_sends = vec![SendTask {
            target_node: "process".to_string(),
            input: Value::Dict(
                [("task".to_string(), Value::String("split".to_string()))]
                    .into_iter()
                    .collect(),
            ),
        }];

        let cp = Checkpoint {
            id: "test-id".to_string(),
            v: 1,
            thread_id: "thread_1".to_string(),
            step: 5,
            channel_values,
            channel_versions,
            versions_seen,
            pending_sends,
            timestamp_ms: 1234567890123,
        };

        let json = cp.to_json().unwrap();
        let cp2 = Checkpoint::from_json(&json).unwrap();
        assert_eq!(cp, cp2);
    }

    #[test]
    fn resume_returns_latest() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 3, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("c", "t1", 2, HashMap::new()))
            .unwrap();

        let latest = resume(&saver, "t1").unwrap();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().step, 3);
    }

    #[test]
    fn rewind_deletes_later_steps() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 3, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("c", "t1", 5, HashMap::new()))
            .unwrap();

        rewind(&saver, "t1", 3).unwrap();

        let ids = saver.list("t1").unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "a");
    }

    #[test]
    fn memory_save_and_load_by_id() {
        let saver = MemorySaver::new();
        let cp = make_checkpoint(
            "id-1",
            "t1",
            0,
            [("x".to_string(), Value::Int(42))].into_iter().collect(),
        );
        saver.save("t1", &cp).unwrap();

        let loaded = saver.load("t1", Some("id-1")).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, "id-1");
    }

    #[test]
    fn memory_load_latest_without_id() {
        let saver = MemorySaver::new();
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
    fn memory_list_is_sorted_by_step() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("c", "t1", 5, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 3, HashMap::new()))
            .unwrap();

        let ids = saver.list("t1").unwrap();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn memory_delete_removes_checkpoint() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t1", &make_checkpoint("b", "t1", 2, HashMap::new()))
            .unwrap();

        saver.delete("t1", "a").unwrap();
        let ids = saver.list("t1").unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "b");
    }

    #[test]
    fn memory_isolation_between_threads() {
        let saver = MemorySaver::new();
        saver
            .save("t1", &make_checkpoint("a", "t1", 1, HashMap::new()))
            .unwrap();
        saver
            .save("t2", &make_checkpoint("b", "t2", 2, HashMap::new()))
            .unwrap();

        assert_eq!(saver.list("t1").unwrap().len(), 1);
        assert_eq!(saver.list("t2").unwrap().len(), 1);
        assert_eq!(saver.list("t1").unwrap()[0], "a");
        assert_eq!(saver.list("t2").unwrap()[0], "b");
    }

    #[test]
    fn memory_delete_nonexistent_is_noop() {
        let saver = MemorySaver::new();
        // Should not panic / error
        saver.delete("t1", "ghost").unwrap();
    }

    #[test]
    fn memory_load_nonexistent_returns_none() {
        let saver = MemorySaver::new();
        let result = saver.load("t1", Some("ghost")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resume_empty_thread_returns_none() {
        let saver = MemorySaver::new();
        let result = resume(&saver, "ghost").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn rewind_empty_thread_is_noop() {
        let saver = MemorySaver::new();
        rewind(&saver, "ghost", 0).unwrap();
    }

    #[test]
    fn checkpoint_new_generates_uuid() {
        let cp1 = Checkpoint::new(
            "t1".to_string(),
            0,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            vec![],
        );
        let cp2 = Checkpoint::new(
            "t1".to_string(),
            0,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            vec![],
        );
        assert_ne!(cp1.id, cp2.id);
        assert_eq!(cp1.v, 1);
        assert_eq!(cp1.thread_id, "t1");
    }

    #[test]
    fn checkpoint_from_json_rejects_missing_field() {
        let bad = r#"{"id":"x","v":1}"#;
        let result = Checkpoint::from_json(bad);
        assert!(result.is_err());
    }

    #[test]
    fn checkpoint_from_json_rejects_non_dict() {
        let bad = r#"[1,2,3]"#;
        let result = Checkpoint::from_json(bad);
        assert!(result.is_err());
    }
}
