//! v0.75.10: Pregel engine state types (StepUndo, EngineStats, AgentExecOutcome, VertexState).
//!
//! Extracted from `pregel/mod.rs` to give engine runtime state its own module.
//! These types are the mutable surface of `MirPregelEngine` and are shared
//! between sequential and parallel EXEC paths.

use std::collections::HashMap;

// v0.93: AgentExecOutcome / VertexState 唯一定义在 mod.rs，此处仅再导出
// （见文件底部）。

// ─── StepUndo ────────────────────────────────────────────────────────

/// v0.75.3: Incremental step snapshot (undo log) — only records engine state
/// that EXEC mutates, replacing the per-step full `build_checkpoint()`.
///
/// Contract (EXEC does not write channels): retry only re-runs the EXEC
/// closure, and EXEC writes zero to `channels` / `channel_versions` /
/// `versions_seen` (UPDATE's `apply_write` happens outside the retry loop),
/// so rollback needs no recovery for them.
///
/// If EXEC ever starts writing channels (e.g. UPDATE moves inside the retry
/// loop), this struct must be extended to lazily record the old values.
pub struct StepUndo {
    /// EXEC modifies via `flush_pending_sends`; restored on failure.
    pub old_pending_sends: Vec<crate::checkpoint::SendTask>,
}

// ─── EngineStats ─────────────────────────────────────────────────────

/// v0.74: Engine runtime metrics.
#[derive(Debug, Default, Clone)]
pub struct EngineStats {
    pub steps: usize,
    pub agents_run: usize,
    pub retries: usize,
    pub timeouts: usize,
    pub total_ms: u128,
    /// v0.75.4: Total messages sent (sum of SendTask counts across ADVANCE).
    pub messages_sent: usize,
    /// v0.75.7: Per-agent last execution duration (ms) — FPGA-style scheduling
    /// observability, used to identify stragglers.
    pub per_agent_ms: HashMap<String, u128>,
}

// ─── AgentExecOutcome ────────────────────────────────────────────────

// v0.93: 唯一定义在 `crate::pregel`（mod.rs）—— 此处再导出，消除此前
// state.rs / mod.rs 两份重复定义（engine/ 与 mod.rs 各持一份，属拼接债）。
// 与下方 VertexState 的处理方式一致。
pub use crate::pregel::AgentExecOutcome;

// ─── VertexState ─────────────────────────────────────────────────────

pub use crate::pregel::VertexState;
