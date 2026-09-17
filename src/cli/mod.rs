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
///
/// v0.103: 返回 `Result` —— 此前解析失败直接 `panic!`，于是 CLI 遇到任何
/// 语法错误都会打印 Rust panic 与回溯（"compile_and_opt failed: Failed to
/// parse at line N"）而不是可读的编译错误；`mora file.mora` 与
/// `mora --check file.mora` 两条入口都受影响。调用方统一按 typecheck 错误的
/// 既有约定报错并以退出码 2 结束。
pub fn compile_and_opt(
    source: &str,
    opt_level: Option<crate::mir::ssa::OptLevel>,
) -> Result<
    (
        crate::mir::MirFunction,
        Vec<crate::mir::witness::MirWitness>,
    ),
    String,
> {
    let (mut func, witnesses) = ParserV3::compile(source)?;

    // v0.90.3: 执行器切换 — 9 层管线产出成为生产 MirFunction。
    //
    // 流程：
    //   1. 管线在 apply_rules 之前运行（差分要求 raw-to-raw）
    //   2. 类别级差分（管线 vs emit.rs 直出）绿 → 管线产出经同一套
    //      优化（apply_rules + SSA opt）后作为返回值 — 生产执行 9 层代码
    //   3. 差分红 → 自动回落原管线（不中断编译），DEBUG 模式输出差异
    //
    // 等价性保证：tests/nine_layer_differential.rs 19 fixture 双级差分
    // （类别级 + 执行级）锁定两条管线的语义等价。
    // MORA_9LAYER=0 可禁用（回落纯原管线）。
    let level = opt_level.unwrap_or_default();
    if std::env::var("MORA_9LAYER").map_or(true, |v| v != "0") {
        let (result, mut pipeline_func) =
            crate::mir::pipeline::run_pipeline(&func, &witnesses);
        if result.differential_ok {
            // 双侧同序优化 — 管线产出走与原管线完全一致的优化路径
            crate::mir::optimize::apply_rules(&mut pipeline_func);
            if level.enabled() {
                crate::mir::opt::optimize(&mut pipeline_func, level);
            }
            return Ok((pipeline_func, witnesses));
        }
        if std::env::var("MORA_9LAYER_DEBUG").is_ok_and(|v| v == "1") {
            eprintln!(
                "[9layer] differential FAILED — falling back to emit.rs path | fcfg={} typed={} core={} cmir={} lmir={} | pipeline_mir={} original_mir={}",
                result.fcfg_nodes,
                result.typed_nodes,
                result.core_insts,
                result.cmir_nodes,
                result.lmir_insts,
                result.pipeline_mir_count,
                result.original_mir_count
            );
            for d in &result.differential_diffs {
                eprintln!("[9layer] diff: {}", d);
            }
        }
    }

    // 原管线路径（回落 / MORA_9LAYER=0）
    crate::mir::optimize::apply_rules(&mut func);
    if level.enabled() {
        crate::mir::opt::optimize(&mut func, level);
    }
    Ok((func, witnesses))
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
