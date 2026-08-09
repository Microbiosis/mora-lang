//! v0.75.98: MoraError —— 全仓统一的错误类型（v0.91 错误统一计划首次落地）。
//!
//! ## 背景
//!
//! 架构审查报告（v0.75.90）标记「🟡 警告级风险 1」：
//! `Result<T, String>` 不统一——`typeck/check_union`、`flow.rs`、`http_server.rs`、
//! `checkpoint/*` 全模块、`document/*` 全模块、`compress/*` 全模块、
//! `compress/text.rs` 等返回 `Result<T, String>`。错误聚合（按 kind 维度去重）
//! 在跨模块边界断裂。
//!
//! ## 设计
//!
//! `MoraError` 枚举——v0.75.98 首次落地，仅**试点**于 `check_union` 路径。
//! v0.75.98+ 后续 commit 逐步覆盖其它模块。
//!
//! ```ignore
//! pub enum MoraError {
//!     /// typeck 内部错误（双向 check helper 等）
//!     Typeck(String),
//!     /// I/O / 文件系统错误（checkpoint / document backend）
//!     Io(String),
//!     /// 序列化错误（JSON / TOML / etc.）
//!     Serialization(String),
//!     /// 其它通用错误（catch-all）
//!     Other(String),
//! }
//! ```
//!
//! ## 原则
//!
//! - **不破坏现有公共 API**——`TypeError` / `HMTypeError` / `AuditError` /
//!   `SandboxError` / `JitError` / `ParseError` / `KeepErrorsConstraint` 7 个
//!   独立 Error 类型继续存在（向后兼容）
//! - **试点仅覆盖 check_union**——最小风险路径，验证 MoraError API 设计
//! - **From impls 后续按需添加**——v0.75.98 仅提供最少 `Display` + `Error` derive
//!
//! ## 不变的
//!
//! - 任何 `Result<T, String>` 路径暂时不迁移（待 v0.75.99+ 逐步覆盖）
//! - 现有 7 个独立 Error 类型不变
//! - v0.91 错误统一完整计划（统一所有 Result<T, String> + 字段结构化）保留为
//!   后续 commit（不在本次 commit 范围）

use std::fmt;

/// v0.75.98: MoraError —— Mora 全仓统一错误枚举的**试点**。
///
/// 当前仅覆盖 `typeck::hm::util::check_union` 一个返回点（最小试水）。
/// 后续 commit 逐步扩展到 `flow.rs` / `http_server.rs` / `checkpoint/*` /
/// `document/*` / `compress/*` 等模块。
///
/// 与现有 7 个独立 Error 类型（`TypeError` / `HMTypeError` / `AuditError` /
/// `SandboxError` / `JitError` / `ParseError` / `KeepErrorsConstraint`）
/// 共存——这些类型历史存在，保留向后兼容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoraError {
    /// typeck 内部错误（双向 check helper、HM 推断错误等）
    Typeck(String),
    /// I/O / 文件系统错误（checkpoint / document backend 等）
    Io(String),
    /// 序列化错误（JSON / TOML / toml / etc.）
    Serialization(String),
    /// 其它通用错误（catch-all——未分类前使用）
    Other(String),
}

impl fmt::Display for MoraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Typeck(msg) => write!(f, "typeck error: {}", msg),
            Self::Io(msg) => write!(f, "io error: {}", msg),
            Self::Serialization(msg) => write!(f, "serialization error: {}", msg),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for MoraError {}

/// v0.75.98: 从 `String` 构造 MoraError 的便捷 impl。
/// 允许现有 `? String` 调用方逐步迁移到 `? MoraError`。
impl From<String> for MoraError {
    fn from(s: String) -> Self {
        MoraError::Other(s)
    }
}

impl From<&str> for MoraError {
    fn from(s: &str) -> Self {
        MoraError::Other(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typeck_variant_display() {
        let e = MoraError::Typeck("subtype failed".to_string());
        assert_eq!(e.to_string(), "typeck error: subtype failed");
    }

    #[test]
    fn io_variant_display() {
        let e = MoraError::Io("file not found".to_string());
        assert_eq!(e.to_string(), "io error: file not found");
    }

    #[test]
    fn serialization_variant_display() {
        let e = MoraError::Serialization("invalid json".to_string());
        assert_eq!(e.to_string(), "serialization error: invalid json");
    }

    #[test]
    fn other_variant_display_passthrough() {
        let e = MoraError::Other("custom error".to_string());
        assert_eq!(e.to_string(), "custom error");
    }

    #[test]
    fn from_string_into_other() {
        let e: MoraError = "test".to_string().into();
        assert_eq!(e, MoraError::Other("test".to_string()));
    }

    #[test]
    fn from_str_into_other() {
        let e: MoraError = "test".into();
        assert_eq!(e, MoraError::Other("test".to_string()));
    }
}
