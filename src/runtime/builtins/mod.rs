//! v0.110.0: Runtime built-in functions, value methods, domain modules
//! (`math`/`stats`/`linalg`/`file`/`exec`), and trait dispatch support.
//!
//! Pure built-ins and value methods are dispatched by `src/runtime/vm/builtins.rs`,
//! which delegates to the canonical helpers in this module tree (`impls` / `method`
//! / `traits`); builtins that need the VM stack (callbacks, `eval`, ambient
//! effects, trait mediation) are left to the VM's own dispatch instead.
//! `registry::RUNTIME_BUILTIN_NAMES` remains the single source of truth for the
//! runtime free-function name list.

/// Debug-only branch reachability assertion macro (`SQLite testcase()` style).
macro_rules! testcase {
    ($cond:expr, $label:expr) => {
        debug_assert!(
            $cond,
            "testcase: {} — branch guard unexpectedly bypassed",
            $label
        )
    };
}

use crate::error::{MoraError, MoraResult};
use crate::flow::*;
use crate::value::Value;

/// Extract a required numeric argument at `idx`, rejecting missing/`nil` or non-numeric values.
pub(crate) fn required_num_arg(
    args: &[Value],
    idx: usize,
    callee: &str,
    what: &str,
) -> MoraResult<f64> {
    match args.get(idx) {
        None | Some(Value::Nil) => Err(MoraError::Runtime(format!("{callee}: requires {what}"))),
        Some(Value::Int(i)) => Ok(*i as f64),
        Some(Value::Float(n)) => Ok(*n),
        Some(other) => Err(MoraError::Runtime(format!(
            "{callee}: {what} must be a number, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

/// Extract a required string argument at `idx`, rejecting missing/`nil` or non-string values.
pub(crate) fn required_str_arg(
    args: &[Value],
    idx: usize,
    callee: &str,
    what: &str,
) -> MoraResult<String> {
    match args.get(idx) {
        None | Some(Value::Nil) => Err(MoraError::Runtime(format!("{callee}: requires {what}"))),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(MoraError::Runtime(format!(
            "{callee}: {what} must be a string, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

/// Extract a non-negative finite millisecond duration from a numeric `Value`.
pub(crate) fn duration_from_ms(val: &Value, callee: &str) -> MoraResult<std::time::Duration> {
    match val {
        Value::Int(i) if *i >= 0 => Ok(std::time::Duration::from_millis(*i as u64)),
        Value::Float(f) if f.is_finite() && *f >= 0.0 => {
            Ok(std::time::Duration::from_secs_f64(*f / 1000.0))
        }
        Value::Int(_) | Value::Float(_) => Err(MoraError::Runtime(format!(
            "{callee}: duration must be a non-negative finite number, got {val}"
        ))),
        other => Err(MoraError::Runtime(format!(
            "{callee}: duration must be a number, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

pub mod exec;
pub mod file;
pub(crate) mod impls;
pub mod linalg;
pub mod math;
pub(crate) mod method;
mod numeric;
mod registry;
pub mod stats;
pub(crate) mod traits;

pub use exec::call_exec_method;
pub use file::call_file_method;
pub use registry::RUNTIME_BUILTIN_NAMES;
