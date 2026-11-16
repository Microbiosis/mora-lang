//! v0.110.0: `MoraError` —— 全仓统一的错误类型（`MoraResult<T>`）。
//!
//! 所有跨模块边界（解析、lowering/verify、JSON 编解码、类型检查、运行时 VM、
//! I/O 与调度器）统一传递结构化的 [`MoraError`] 变体，不再提供
//! `String <-> MoraError` 的隐式过渡转换桥。

use std::fmt;

/// Unified error type across Mora compilation, typechecking, VM, concurrency, and async I/O layers.
///
/// v0.75.98 introduced the initial 4-variant pilot (`Typeck`, `Io`, `Serialization`, `Other`);
/// Wave 2 Stage W2.4 promotes [`MoraError`] and [`MoraResult`] as the canonical error contract
/// across runtime scheduling, async I/O multiplexing, VM execution, builtins, and compiler CLI boundaries.
pub type MoraResult<T> = Result<T, MoraError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoraError {
    /// Lexical, parse, or MIR lowering failure.
    Parse(String),
    /// Typechecker internal or constraint error.
    Typeck(String),
    /// VM execution, lookup, arity, or domain runtime failure.
    Runtime(String),
    /// Asynchronous / spawned task panic, join failure, or lifecycle error.
    Task(String),
    /// Channel send/recv on closed, full, or disconnected endpoints.
    Channel(String),
    /// Cooperative cancellation triggered via `TaskContext`.
    Cancelled(String),
    /// Bounded wait or deadline expiration.
    Timeout(String),
    /// I/O, filesystem, process, or async network error.
    Io(String),
    /// Serialization/deserialization error (JSON, etc.).
    Serialization(String),
    /// Generic catch-all error.
    Other(String),
}

impl MoraError {
    /// Stable lowercase variant identifier for diagnostics and telemetry.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Parse(_) => "parse",
            Self::Typeck(_) => "typeck",
            Self::Runtime(_) => "runtime",
            Self::Task(_) => "task",
            Self::Channel(_) => "channel",
            Self::Cancelled(_) => "cancelled",
            Self::Timeout(_) => "timeout",
            Self::Io(_) => "io",
            Self::Serialization(_) => "serialization",
            Self::Other(_) => "other",
        }
    }

    /// Borrow the underlying error detail without category prefixes.
    pub fn message(&self) -> &str {
        match self {
            Self::Parse(msg)
            | Self::Typeck(msg)
            | Self::Runtime(msg)
            | Self::Task(msg)
            | Self::Channel(msg)
            | Self::Cancelled(msg)
            | Self::Timeout(msg)
            | Self::Io(msg)
            | Self::Serialization(msg)
            | Self::Other(msg) => msg,
        }
    }

    /// Return a new error with `prefix` prepended while preserving the structured variant.
    pub fn with_prefix(&self, prefix: &str) -> Self {
        let prefixed = format!("{prefix}{}", self.message());
        match self {
            Self::Parse(_) => Self::Parse(prefixed),
            Self::Typeck(_) => Self::Typeck(prefixed),
            Self::Runtime(_) => Self::Runtime(prefixed),
            Self::Task(_) => Self::Task(prefixed),
            Self::Channel(_) => Self::Channel(prefixed),
            Self::Cancelled(_) => Self::Cancelled(prefixed),
            Self::Timeout(_) => Self::Timeout(prefixed),
            Self::Io(_) => Self::Io(prefixed),
            Self::Serialization(_) => Self::Serialization(prefixed),
            Self::Other(_) => Self::Other(prefixed),
        }
    }

    /// True if this error represents cooperative cancellation or deadline expiration.
    pub fn is_cancellation_or_timeout(&self) -> bool {
        matches!(self, Self::Cancelled(_) | Self::Timeout(_))
    }
}

impl fmt::Display for MoraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "parse error: {}", msg),
            Self::Typeck(msg) => write!(f, "typeck error: {}", msg),
            Self::Runtime(msg) => write!(f, "{}", msg),
            Self::Task(msg) => write!(f, "task error: {}", msg),
            Self::Channel(msg) => write!(f, "channel error: {}", msg),
            Self::Cancelled(msg) => write!(f, "cancelled: {}", msg),
            Self::Timeout(msg) => write!(f, "timeout: {}", msg),
            Self::Io(msg) => write!(f, "io error: {}", msg),
            Self::Serialization(msg) => write!(f, "serialization error: {}", msg),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for MoraError {}

impl From<std::io::Error> for MoraError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<tokio::task::JoinError> for MoraError {
    fn from(e: tokio::task::JoinError) -> Self {
        if e.is_cancelled() {
            Self::Cancelled(e.to_string())
        } else {
            Self::Task(e.to_string())
        }
    }
}

// 注意：不再实现 `From<String> for MoraError` / `From<&str> for MoraError` /
// `From<MoraError> for String` —— 所有调用方必须显式构造具体错误变体或调用 `.to_string()`。

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
    fn explicit_variants_preserve_message() {
        let parse = MoraError::Parse("unexpected token".to_string());
        assert_eq!(parse.kind_name(), "parse");
        assert_eq!(parse.message(), "unexpected token");

        let ser = MoraError::Serialization("bad payload".to_string());
        assert_eq!(ser.kind_name(), "serialization");
        assert_eq!(ser.to_string(), "serialization error: bad payload");
    }

    #[test]
    fn structured_variants_display_kind_and_prefix() {
        let cases = [
            (
                MoraError::Parse("bad token".into()),
                "parse",
                "bad token",
                "parse error: bad token",
            ),
            (
                MoraError::Runtime("div by zero".into()),
                "runtime",
                "div by zero",
                "div by zero",
            ),
            (
                MoraError::Task("worker panicked".into()),
                "task",
                "worker panicked",
                "task error: worker panicked",
            ),
            (
                MoraError::Channel("closed".into()),
                "channel",
                "closed",
                "channel error: closed",
            ),
            (
                MoraError::Cancelled("aborted".into()),
                "cancelled",
                "aborted",
                "cancelled: aborted",
            ),
            (
                MoraError::Timeout("50ms elapsed".into()),
                "timeout",
                "50ms elapsed",
                "timeout: 50ms elapsed",
            ),
        ];
        for (err, kind, raw, formatted) in cases {
            assert_eq!(err.kind_name(), kind);
            assert_eq!(err.message(), raw);
            assert_eq!(err.to_string(), formatted);
            let prefixed = err.with_prefix("ctx: ");
            assert_eq!(prefixed.kind_name(), kind);
            assert_eq!(prefixed.message(), format!("ctx: {raw}"));
        }
        assert!(MoraError::Cancelled("x".into()).is_cancellation_or_timeout());
        assert!(MoraError::Timeout("x".into()).is_cancellation_or_timeout());
        assert!(!MoraError::Task("x".into()).is_cancellation_or_timeout());
    }

    #[test]
    fn external_error_conversions() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let mapped: MoraError = io_err.into();
        assert_eq!(mapped.kind_name(), "io");

        let ser = MoraError::Serialization("bad json".into());
        assert_eq!(ser.kind_name(), "serialization");
    }
}
