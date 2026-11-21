//! Mora 核心编译器与运行时库（v0.110.0）
//!
//! AI-native 静态类型语言核心，面向大模型底层、计算图与算子、大模型推理/训练与资源动态调控，
//! 并具备 Go 式云原生并发与 HTTP/MCP 原语友好支持。
//! 暴露 lexer / parser_v3 / mir / typeck / runtime / value / flow / lsp / cli 等模块，
//! 让 CLI binary（`src/main.rs`）和 LSP server binary（`src/bin/lsp.rs`）共享。

/// v0.51: 版本号叙事的单一真相源。
///
/// 编译期从 build.rs 注入的 `MORAGIT_VERSION` env 读取 (来自 `Cargo.toml`
/// 的 `version` 字段)。`env!` 在编译期展开, 零运行时开销。
///
/// 改 `Cargo.toml` 的 version 后, 下次 `cargo build` 自动重 build.rs,
/// 所有引用 `mora::VERSION` 或 `env!("MORAGIT_VERSION")` 的位置同步更新。
pub const VERSION: &str = env!("MORAGIT_VERSION");

// v0.104.6 D475: 测试用临时目录/文件 —— **本仓唯一的临时文件创建入口**
// （`tests/test_tempdir_hygiene.rs` 守卫：别处直接调 `temp_dir()` 即判失败）
pub mod test_support;
// v0.75.53: CLI 子命令（共享编译/路径辅助，从 main.rs 拆出）
pub mod cli;
// v0.75.98: MoraError —— 全仓统一错误枚举
pub mod common;
pub mod error;
pub mod flow;
pub mod lexer;
pub mod lsp;
pub mod mir;
pub mod parser_v3;
pub mod runtime;
pub mod typeck;
pub mod value;

/// Resolve embedded pure-Mora standard library module source by canonical specifier
/// (e.g. `"std/net/http"`, `"std/mcp"`, `"std/math/bigint"`, `"std/testing"`).
pub fn resolve_std_module(spec: &str) -> Option<&'static str> {
    let norm = spec.trim().replace('\\', "/");
    match norm.as_str() {
        "std/net/http" | "std/net/http.mora" => Some(include_str!("../std/net/http.mora")),
        "std/mcp" | "std/mcp.mora" => Some(include_str!("../std/mcp.mora")),
        "std/math/bigint" | "std/math/bigint.mora" => Some(include_str!("../std/math/bigint.mora")),
        "std/testing" | "std/testing.mora" => Some(include_str!("../std/testing.mora")),
        _ => None,
    }
}
