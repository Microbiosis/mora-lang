//! v0.75.51: builtin 模块聚合（P7，Rhai register_plugin/Koto workspace 思想）。
//!
//! 14 个 call_*_method + get_embedding 已按 domain 拆到独立文件：
//! file / event / sandbox / schedule / ai_tokens / ai / ccr / mock /
//! memory / exec / toolplane / skill / plan / mora。本文件仅以 mod 声明
//! 聚合 domain 文件。
//!
//! v0.92: 测试模块按 domain 拆到 `tests/` 子模块（P1.1 god module 拆分）。
//! 生产代码零残留，mod.rs 从 2260 行收缩为纯聚合声明 + 测试模块声明。

use super::*;

// ── 生产 domain 模块 ──
mod ai;
mod ai_tokens;
mod ccr;
mod event;
mod exec;
mod file;
pub mod linalg;
pub mod math;
mod memory;
mod mock;
mod mora;
mod plan;
mod sandbox;
mod schedule;
mod skill;
pub mod stats;
// v0.83: TEA runtime + transducer builtin
mod tea;
mod toolplane;
mod xform;

// ── 测试套件（按 domain 拆分，v0.92 P1.1）──
#[cfg(test)]
mod tests {
    mod ai; // v0.45 ai + v0.47 context
    mod audit; // v0.42.1 audit 持久化
    mod capability; // v0.42 capability (sandbox/key + sandbox.check_call)
    mod dag; // v0.47 dag
    mod heartbeat; // v0.47 heartbeat
    mod orchestrate; // v0.44 orchestrate block syntax
    mod plan; // v0.48 plan
    mod refine; // v0.48 refine
    mod sandbox; // v0.44 container_real
    mod skill; // v0.46 skill
    mod toolplane; // v0.45 toolplane
}
