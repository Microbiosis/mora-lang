//! Mora 标准库（v11+ 拆出）
//!
//! 暴露 lexer / parser_v3 / runtime / typeck / mir / lsp 等模块，
//! 让 CLI binary（src/main.rs）和 LSP server binary（src/bin/lsp.rs）共享。

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
