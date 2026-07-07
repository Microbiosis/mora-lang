//! v0.50: MemorySaver — in-memory checkpoint storage for testing and ephemeral workflows.

use super::{Checkpoint, CheckpointSaver};
use parking_lot::Mutex;
use std::collections::HashMap;

/// In-memory checkpoint storage.
///
/// Checkpoints are organized by `thread_id` and kept in insertion order.
/// All operations are `Mutex`-guarded for thread-safe access.
pub struct MemorySaver {
    checkpoints: Mutex<HashMap<String, Vec<Checkpoint>>>,
}

impl MemorySaver {
    pub fn new() -> Self {
        Self {
            checkpoints: Mutex::new(HashMap::new()),
        }
    }
}

impl CheckpointSaver for MemorySaver {
    fn save(&self, thread_id: &str, checkpoint: &Checkpoint) -> Result<(), String> {
        let mut guard = self.checkpoints.lock();
        let entry = guard.entry(thread_id.to_string()).or_default();
        entry.push(checkpoint.clone());
        Ok(())
    }

    fn load(
        &self,
        thread_id: &str,
        checkpoint_id: Option<&str>,
    ) -> Result<Option<Checkpoint>, String> {
        let guard = self.checkpoints.lock();
        let entry = match guard.get(thread_id) {
            None => return Ok(None),
            Some(v) => v,
        };

        match checkpoint_id {
            None => {
                // Return the checkpoint with the highest step.
                let latest = entry.iter().max_by_key(|cp| cp.step);
                Ok(latest.cloned())
            }
            Some(id) => Ok(entry.iter().find(|cp| cp.id == id).cloned()),
        }
    }

    fn list(&self, thread_id: &str) -> Result<Vec<String>, String> {
        let guard = self.checkpoints.lock();
        let entry = match guard.get(thread_id) {
            None => return Ok(vec![]),
            Some(v) => v,
        };

        let mut ids: Vec<String> = entry.iter().map(|cp| cp.id.clone()).collect();
        // Sort by step for deterministic ordering, matching SqliteSaver behaviour.
        ids.sort_by_key(|id| {
            entry
                .iter()
                .find(|cp| &cp.id == id)
                .map(|cp| cp.step)
                .unwrap_or(0)
        });
        Ok(ids)
    }

    fn delete(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), String> {
        let mut guard = self.checkpoints.lock();
        if let Some(entry) = guard.get_mut(thread_id) {
            entry.retain(|cp| cp.id != checkpoint_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_saver_smoke() {
        let saver = MemorySaver::new();
        let cp = Checkpoint {
            id: "smoke".to_string(),
            v: 1,
            thread_id: "t".to_string(),
            step: 0,
            channel_values: HashMap::new(),
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            pending_sends: vec![],
            timestamp_ms: 0,
        };
        saver.save("t", &cp).unwrap();
        let loaded = saver.load("t", None).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, "smoke");
    }

    #[test]
    fn memory_saver_latest_by_step() {
        let saver = MemorySaver::new();
        saver
            .save(
                "t",
                &Checkpoint {
                    id: "a".to_string(),
                    v: 1,
                    thread_id: "t".to_string(),
                    step: 1,
                    channel_values: HashMap::new(),
                    channel_versions: HashMap::new(),
                    versions_seen: HashMap::new(),
                    pending_sends: vec![],
                    timestamp_ms: 0,
                },
            )
            .unwrap();
        saver
            .save(
                "t",
                &Checkpoint {
                    id: "b".to_string(),
                    v: 1,
                    thread_id: "t".to_string(),
                    step: 3,
                    channel_values: HashMap::new(),
                    channel_versions: HashMap::new(),
                    versions_seen: HashMap::new(),
                    pending_sends: vec![],
                    timestamp_ms: 0,
                },
            )
            .unwrap();

        let latest = saver.load("t", None).unwrap();
        assert_eq!(latest.unwrap().id, "b");
    }
}
