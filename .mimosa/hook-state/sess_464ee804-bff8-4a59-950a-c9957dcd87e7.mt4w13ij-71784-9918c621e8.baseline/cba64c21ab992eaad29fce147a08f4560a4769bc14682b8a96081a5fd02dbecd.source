//! v0.85: Mora 配置常量集中模块。
//!
//! 将分散在 `interpreter/mod.rs` 中的 AI 相关环境变量常量集中到此处，
//! 消除 `compress/text.rs` 等 Foundation/Kernel 层模块对 `interpreter`
//! 的跨层导入（耦合分析报告 W6）。所有模块通过 `crate::config::*` 获取
//! 配置，不再需要引用 `crate::interpreter` 仅为取常量。

/// AI 模型名称环境变量
pub const AI_MODEL_ENV: &str = "MORA_AI_MODEL";
/// AI 模型名称默认值
pub const AI_MODEL_DEFAULT: &str = "example-model";
/// OpenAI API Key 环境变量
pub const AI_API_KEY_ENV: &str = "OPENAI_API_KEY";
/// AI 服务端点 URL 环境变量
pub const AI_BASE_URL_ENV: &str = "MORA_AI_BASE_URL";
/// AI 服务端点 URL 默认值
pub const AI_BASE_URL_DEFAULT: &str = "https://api.openai.com/v1";