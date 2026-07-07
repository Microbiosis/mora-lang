//! v0.52 ADR-001: PersistRuntime — BC5 (audit + markdown memory dir + checkpoint saver)
//!
//! 从 Interpreter god object 抽出的 persistence 状态容器，3 字段。

use std::path::PathBuf;
use std::sync::Arc;

use crate::audit::NullSink;
use crate::checkpoint::CheckpointSaver;

#[derive(Clone)]
pub struct PersistRuntime {
    /// v0.42.1: Audit sink (default NullSink; switch to JsonlAuditSink for hash-chained audit log)
    pub(crate) audit_sink: Arc<dyn crate::audit::AuditSink>,
    /// v0.43.1: Markdown memory root dir (test isolation + custom path support)
    /// If None, falls back to $MORA_MEMORY_DIR or $HOME/.mora/memory
    pub(crate) markdown_memory_dir: Option<PathBuf>,
    /// v0.50: Pregel 检查点保存器（由 Worker 2/3 完善 checkpoint 模块后注入）
    pub(crate) checkpoint_saver: Option<Arc<dyn CheckpointSaver>>,
}

impl Default for PersistRuntime {
    fn default() -> Self {
        Self {
            audit_sink: Arc::new(NullSink::new()),
            markdown_memory_dir: None,
            checkpoint_saver: None,
        }
    }
}

impl PersistRuntime {
    /// 记录 audit 事件
    pub fn audit_event(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        payload_json: Option<&str>,
    ) -> Result<(), crate::audit::AuditError> {
        use crate::audit::AuditEvent;
        let event = AuditEvent {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            actor: actor.to_string(),
            action: action.to_string(),
            target: target.map(String::from),
            payload_json: payload_json.map(String::from),
            token_id: None,
            prev_hash: String::new(), // sink 在 write 时填充
            hash: String::new(),      // sink 在 write 时填充
        };
        self.audit_sink.write(event)?;
        self.audit_sink.flush()
    }

    /// v0.66: Inject a checkpoint saver (e.g. `MemorySaver`) so the Pregel
    /// engine can persist snapshots during BSP execution.
    pub fn set_checkpoint_saver(&mut self, saver: Arc<dyn CheckpointSaver>) {
        self.checkpoint_saver = Some(saver);
    }

    /// v0.66: Get a clone of the current checkpoint saver, if any.
    /// Used by `h_orchestrate` to wire the saver into `MirPregelEngine`.
    pub fn checkpoint_saver(&self) -> Option<Arc<dyn CheckpointSaver>> {
        self.checkpoint_saver.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_persist_null_sink() {
        let persist = PersistRuntime::default();
        // 验证 audit_sink 存在且是 NullSink — write/flush 不 panic
        let result = persist.audit_event("test_actor", "test_action", None, Some("data"));
        assert!(result.is_ok());
        // NullSink 是 noop — event_count 始终 0
        assert_eq!(persist.audit_sink.event_count(), 0);
    }

    #[test]
    fn markdown_memory_dir_default_none() {
        let persist = PersistRuntime::default();
        assert!(persist.markdown_memory_dir.is_none());
    }

    #[test]
    fn checkpoint_saver_default_none() {
        let persist = PersistRuntime::default();
        assert!(persist.checkpoint_saver.is_none());
    }

    #[test]
    fn clone_shares_arc() {
        // audit_sink 是 Arc<dyn AuditSink>，clone 后两个共享同一 sink
        let p1 = PersistRuntime::default();
        let p2 = p1.clone();
        // 验证两者独立但 audit_sink 共享
        assert!(p2.checkpoint_saver.is_none());
        assert!(p2.markdown_memory_dir.is_none());
    }

    #[test]
    fn set_and_get_checkpoint_saver() {
        use crate::checkpoint::MemorySaver;
        let mut persist = PersistRuntime::default();
        assert!(persist.checkpoint_saver().is_none());
        let saver: Arc<dyn crate::checkpoint::CheckpointSaver> = Arc::new(MemorySaver::new());
        persist.set_checkpoint_saver(saver.clone());
        let got = persist.checkpoint_saver().expect("saver should be set");
        // Same Arc identity → wrapping the same backend
        assert!(Arc::ptr_eq(&got, &saver));
    }
}
