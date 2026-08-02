//! v0.75.53: CLI 子命令模块（P9，D6 SQLite 单文件拆分惯例）。
//!
//! 从 main.rs 拆出：record（录制/replay/diff/snapshot + 统计）与 mcp。
//! 共享编译/路径辅助在本文件。main.rs 仅保留 dispatch + 执行入口。

pub mod mcp;
pub mod record;

use std::fs;
use std::path::Path;
use std::process;

use crate::interpreter::Interpreter;
use crate::parser_v3::ParserV3;
use crate::typeck::format_error;

/// v0.75.40: 单遍编译 + 优化 — 取代 parse→lower 双阶段。
/// ParserV3::compile 直接 emit MirInst + 并行产出 witness；优化语义与
/// lower_mir_exprs_with_opt 完全一致（cascades apply_rules 恒跑 + SSA opt
/// 显式 --opt 优先，未指定走 env 兜底）。调用方各自做 witness typecheck
/// 并保留原有错误消息。
pub fn compile_and_opt(
    source: &str,
    opt_level: Option<crate::mir::ssa::OptLevel>,
) -> (
    crate::mir::MirFunction,
    Vec<crate::mir::witness::MirWitness>,
) {
    let (mut func, witnesses) =
        ParserV3::compile(source).unwrap_or_else(|e| panic!("compile_and_opt failed: {e}"));
    // v0.58: Cascades 优化 pass（同 lower_mir_exprs_with_opt）
    crate::mir::optimize::apply_rules(&mut func);
    // v0.75.7: SSA 优化管线（显式等级 or env）
    let level = opt_level.unwrap_or_default();
    if level.enabled() {
        crate::mir::opt::optimize(&mut func, level);
    }
    (func, witnesses)
}

fn recordings_dir() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".mora");
    p.push("recordings");
    p
}

fn recording_path(name: &str) -> std::path::PathBuf {
    let mut p = recordings_dir();
    p.push(format!("{}.jsonl", name));
    p
}

fn format_duration(ms: u128) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{:.1}min", ms as f64 / 60_000.0)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

// v0.15: snapshot — 快照测试
fn snapshots_dir() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".mora");
    p.push("snapshots");
    p
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    let mut p = snapshots_dir();
    p.push(format!("{}.snap.jsonl", name));
    p
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{}MB", bytes / (1024 * 1024))
    }
}

fn format_ts(ts_ms: u128) -> String {
    if ts_ms == 0 {
        return "-".to_string();
    }
    // 显示相对时间
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let diff_ms = now.saturating_sub(ts_ms);
    if diff_ms < 60_000 {
        "just now".to_string()
    } else if diff_ms < 3_600_000 {
        format!("{}min ago", diff_ms / 60_000)
    } else if diff_ms < 86_400_000 {
        format!("{}h ago", diff_ms / 3_600_000)
    } else {
        format!("{}d ago", diff_ms / 86_400_000)
    }
}
