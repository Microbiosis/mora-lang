//! v0.110.0: Pure value helpers for indexing, compiled pattern matching, trait boxing,
//! quasiquote interpolation, and `eval` assertions.
//!
//! These helpers never push VM frames and never touch the runtime environment: the only
//! optional side effect is recording onto the autograd tape the caller passes in.

mod pattern;

pub use pattern::match_pattern;
pub(crate) use pattern::match_pattern_slot;

use crate::error::{MoraError, MoraResult};
use crate::flow::{dynamic_type_name, type_name, values_equal};
use crate::mir::control::ControlQqSegment;
use crate::runtime::autograd::AutogradContext;
use crate::value::Value;

/// Validate and truncate an index into `0..len` using exact source-facing error contracts.
fn checked_index(n: f64, len: usize, what: &str) -> MoraResult<usize> {
    if n.is_nan() {
        return Err(MoraError::Runtime(format!(
            "{what}: NaN is not a valid index"
        )));
    }
    if n < 0.0 {
        return Err(MoraError::Runtime(format!("negative {what}: {n}")));
    }
    if n >= len as f64 {
        return Err(MoraError::Runtime(format!(
            "{what} {n} out of bounds (len {len})"
        )));
    }
    Ok(n as usize)
}

fn char_at(s: &str, i: usize) -> MoraResult<Value> {
    s.chars().nth(i).map(Value::Char).ok_or_else(|| {
        MoraError::Runtime(format!(
            "string index {} out of bounds (len {})",
            i,
            s.chars().count()
        ))
    })
}

/// Evaluate `obj[idx]` for lists, strings (Unicode scalar indexing), dicts, and tensors.
pub fn index_value(obj: &Value, idx: &Value) -> MoraResult<Value> {
    index_value_tracked(obj, idx, None)
}

/// Evaluate `obj[idx]` with negative tensor indexing and optional reverse-mode autograd tracking.
pub fn index_value_tracked(
    obj: &Value,
    idx: &Value,
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let i = checked_index(*i as f64, list.len(), "index")?;
            list.get(i).cloned().ok_or_else(|| {
                MoraError::Runtime(format!("index {} out of bounds (len {})", i, list.len()))
            })
        }
        (Value::List(list), Value::Float(n)) => {
            let i = checked_index(*n, list.len(), "index")?;
            list.get(i).cloned().ok_or_else(|| {
                MoraError::Runtime(format!("index {} out of bounds (len {})", i, list.len()))
            })
        }
        (Value::List(list), Value::Scalar(s)) => {
            let i = checked_index(s.as_f64(), list.len(), "index")?;
            list.get(i).cloned().ok_or_else(|| {
                MoraError::Runtime(format!("index {} out of bounds (len {})", i, list.len()))
            })
        }
        (Value::Dict(map), Value::String(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Nil)),
        (Value::String(s), Value::Int(i)) => {
            let i = checked_index(*i as f64, s.chars().count(), "string index")?;
            char_at(s, i)
        }
        (Value::String(s), Value::Float(n)) => {
            let i = checked_index(*n, s.chars().count(), "string index")?;
            char_at(s, i)
        }
        (Value::Tensor(t), Value::Int(i)) => t.index_axis0_tracked(*i as isize, autograd),
        (Value::Tensor(t), Value::Float(n)) => {
            if n.is_nan() || !n.is_finite() {
                return Err(MoraError::Runtime(format!(
                    "tensor index: {n} is not a valid index"
                )));
            }
            t.index_axis0_tracked(*n as isize, autograd)
        }
        (Value::Tensor(t), Value::Scalar(s)) => {
            let n = s.as_f64();
            if n.is_nan() || !n.is_finite() {
                return Err(MoraError::Runtime(format!(
                    "tensor index: {n} is not a valid index"
                )));
            }
            t.index_axis0_tracked(n as isize, autograd)
        }
        _ => Err(MoraError::Runtime(format!(
            "cannot index {} with {}",
            type_name(obj),
            type_name(idx)
        ))),
    }
}

/// Update `obj[idx] = val` in place for lists and dicts.
pub fn index_assign_value(obj: &mut Value, idx: &Value, val: &Value) -> MoraResult<()> {
    index_assign_value_fbip(obj, idx, val.clone(), None)
}

/// P6-1 Perceus/FBIP: Update `obj[idx] = val` reusing uniquely-owned `List` / `Dict` storage in place.
pub fn index_assign_value_fbip(
    obj: &mut Value,
    idx: &Value,
    val: Value,
    fbip_stats: Option<&crate::runtime::fbip::FbipCounters>,
) -> MoraResult<()> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let i = checked_index(*i as f64, list.len(), "index")?;
            if i >= list.len() {
                Err(MoraError::Runtime(format!(
                    "index {} out of bounds (len {})",
                    i,
                    list.len()
                )))
            } else {
                let owned = std::mem::take(list);
                let (updated, reused) = owned.set_owned(i, val);
                if reused && let Some(stats) = fbip_stats {
                    stats.on_list_reuse();
                }
                *list = updated;
                Ok(())
            }
        }
        (Value::List(list), Value::Float(n)) => {
            let i = checked_index(*n, list.len(), "index")?;
            if i >= list.len() {
                Err(MoraError::Runtime(format!(
                    "index {} out of bounds (len {})",
                    i,
                    list.len()
                )))
            } else {
                let owned = std::mem::take(list);
                let (updated, reused) = owned.set_owned(i, val);
                if reused && let Some(stats) = fbip_stats {
                    stats.on_list_reuse();
                }
                *list = updated;
                Ok(())
            }
        }
        (Value::Dict(map), Value::String(key)) => {
            let reused = map.insert_mut(key.clone(), val);
            if reused && let Some(stats) = fbip_stats {
                stats.on_dict_reuse();
            }
            Ok(())
        }
        _ => Err(MoraError::Runtime(
            "cannot index assign with given object and index".to_string(),
        )),
    }
}

/// Box `val` into a `Value::TraitObject` with its concrete runtime type name.
pub fn make_dyn_trait(val: Value, trait_name: &str, generics: &[String]) -> Value {
    let for_type = dynamic_type_name(&val);
    Value::TraitObject {
        for_generics: Vec::new(),
        trait_generics: generics.to_vec(),
        for_type,
        trait_name: trait_name.to_string(),
        data: Box::new(val),
    }
}

/// Render a runtime value for quasiquote template interpolation.
///
/// Mirrors Rust `f64::to_string` for whole-number floats (`1.0` -> `"1"`) and
/// extracts `Value::Code` source text verbatim so nested quasiquotes splice cleanly.
pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) | Value::Code(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Scalar(s) => s.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        Value::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

/// Render quasiquote template segments into a `Value::Code` source string.
pub fn eval_quasiquote(
    segs: &[ControlQqSegment],
    regs: &[super::continuation::RegSlot],
) -> MoraResult<Value> {
    let mut out = String::new();
    for seg in segs {
        match seg {
            ControlQqSegment::Quote(s) => out.push_str(s),
            ControlQqSegment::Unquote(r) => {
                out.push_str(&value_to_string(&regs[r.0 as usize].to_value()))
            }
            ControlQqSegment::UnquoteSplice(r) => match regs[r.0 as usize].as_heap() {
                Some(Value::List(items)) => {
                    let rendered: Vec<String> = items.iter().map(value_to_string).collect();
                    out.push_str(&rendered.join(", "));
                }
                _ => {
                    let val = regs[r.0 as usize].to_value();
                    return Err(MoraError::Runtime(format!(
                        "quasiquote unquote-splicing (,,): expected List, got {}",
                        type_name(&val)
                    )));
                }
            },
        }
    }
    Ok(Value::Code(out))
}

/// Evaluate `eval "..." given: expr expect: expr1, ...` assertions.
pub fn eval_assert(
    name: &str,
    given_val: &Value,
    expect_vals: &[Value],
    tolerance: Option<f64>,
) -> MoraResult<()> {
    for expect_val in expect_vals {
        let pass = if let Some(tol) = tolerance {
            match (given_val, expect_val) {
                (Value::Float(g), Value::Float(e)) => (g - e).abs() <= tol,
                (Value::Int(g), Value::Int(e)) => (*g as f64 - *e as f64).abs() <= tol,
                (Value::Int(g), Value::Float(e)) => (*g as f64 - *e).abs() <= tol,
                (Value::Float(g), Value::Int(e)) => (*g - *e as f64).abs() <= tol,
                _ => values_equal(given_val, expect_val),
            }
        } else {
            values_equal(given_val, expect_val)
        };
        if !pass {
            return Err(MoraError::Runtime(format!(
                "eval '{}': assertion failed: given {:?}, expected {:?}",
                name, given_val, expect_val
            )));
        }
    }
    eprintln!("eval '{}': PASSED", name);
    Ok(())
}
