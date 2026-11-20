//! Pure value helpers for indexing, compiled pattern matching, trait boxing,
//! quasiquote interpolation, and `eval` assertions (Wave 2 — Stage W2.3).

use crate::error::{MoraError, MoraResult};
use crate::flow::{dynamic_type_name, type_name, values_equal};
use crate::mir::control::{CompiledPattern, ControlQqSegment, PatternTarget};
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
                *list = list.set(i, val.clone());
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
                *list = list.set(i, val.clone());
                Ok(())
            }
        }
        (Value::Dict(map), Value::String(key)) => {
            map.insert(key.clone(), val.clone());
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
pub fn eval_quasiquote(segs: &[ControlQqSegment], regs: &[Value]) -> MoraResult<Value> {
    let mut out = String::new();
    for seg in segs {
        match seg {
            ControlQqSegment::Quote(s) => out.push_str(s),
            ControlQqSegment::Unquote(r) => out.push_str(&value_to_string(&regs[r.0 as usize])),
            ControlQqSegment::UnquoteSplice(r) => match &regs[r.0 as usize] {
                Value::List(items) => {
                    let rendered: Vec<String> = items.iter().map(value_to_string).collect();
                    out.push_str(&rendered.join(", "));
                }
                other => {
                    return Err(MoraError::Runtime(format!(
                        "quasiquote unquote-splicing (,,): expected List, got {}",
                        type_name(other)
                    )));
                }
            },
        }
    }
    Ok(Value::Code(out))
}

/// Atomically match `val` against `pattern` and return all captured target bindings on success.
pub fn match_pattern(
    val: &Value,
    pattern: &CompiledPattern,
) -> Option<Vec<(PatternTarget, Value)>> {
    let mut bindings = Vec::new();
    if match_pattern_inner(val, pattern, &mut bindings) {
        Some(bindings)
    } else {
        None
    }
}

fn match_pattern_inner(
    val: &Value,
    pattern: &CompiledPattern,
    bindings: &mut Vec<(PatternTarget, Value)>,
) -> bool {
    match pattern {
        CompiledPattern::Wildcard => true,
        CompiledPattern::Bind(target) => {
            if !matches!(target, PatternTarget::Discard) {
                bindings.push((*target, val.clone()));
            }
            true
        }
        CompiledPattern::Nil => matches!(val, Value::Nil),
        CompiledPattern::Bool(b) => matches!(val, Value::Bool(v) if v == b),
        CompiledPattern::Int(n) => matches!(val, Value::Int(v) if v == n),
        CompiledPattern::Float(f) => matches!(val, Value::Float(v) if v == f),
        CompiledPattern::Scalar(s) => matches!(val, Value::Scalar(v) if v == s),
        CompiledPattern::Char(c) => matches!(val, Value::Char(v) if v == c),
        CompiledPattern::String(s) => matches!(val, Value::String(v) if v == s),
        CompiledPattern::Tuple(elems) => {
            let Value::List(list) = val else {
                return false;
            };
            if list.len() != elems.len() {
                return false;
            }
            list.iter()
                .zip(elems.iter())
                .all(|(item, pat)| match_pattern_inner(item, pat, bindings))
        }
        CompiledPattern::ListVec { elements, rest } => {
            let Value::List(list) = val else {
                return false;
            };
            if rest.is_some() {
                if list.len() < elements.len() {
                    return false;
                }
                for (idx, pat) in elements.iter().enumerate() {
                    if !match_pattern_inner(&list[idx], pat, bindings) {
                        return false;
                    }
                }
                if let Some(rest_pat) = rest {
                    let tail_items: crate::value::list::List =
                        list.iter().skip(elements.len()).cloned().collect();
                    return match_pattern_inner(&Value::List(tail_items), rest_pat, bindings);
                }
                true
            } else {
                if list.len() != elements.len() {
                    return false;
                }
                list.iter()
                    .zip(elements.iter())
                    .all(|(item, pat)| match_pattern_inner(item, pat, bindings))
            }
        }
        CompiledPattern::ListCons { head, tail } => match val {
            Value::Cons { car, cdr } => {
                match_pattern_inner(car, head, bindings) && match_pattern_inner(cdr, tail, bindings)
            }
            Value::List(list) if !list.is_empty() => {
                let first = &list[0];
                let rest_list: crate::value::list::List = list.iter().skip(1).cloned().collect();
                match_pattern_inner(first, head, bindings)
                    && match_pattern_inner(&Value::List(rest_list), tail, bindings)
            }
            _ => false,
        },
        CompiledPattern::Dict { required, rest } => {
            let Value::Dict(map) = val else {
                return false;
            };
            if !*rest && map.len() != required.len() {
                return false;
            }
            for (k, sub_pat) in required {
                let Some(entry_val) = map.get(k) else {
                    return false;
                };
                if !match_pattern_inner(entry_val, sub_pat, bindings) {
                    return false;
                }
            }
            true
        }
        CompiledPattern::TypeAscription { ty_name, inner } => {
            let actual = dynamic_type_name(val);
            let base = type_name(val);
            let matches_ty = match ty_name.as_str() {
                "number" => matches!(val, Value::Int(_) | Value::Float(_)),
                "dynamic" | "any" => true,
                other => actual == other || base == other,
            };
            matches_ty && match_pattern_inner(val, inner, bindings)
        }
        CompiledPattern::Variant {
            enum_name,
            variant,
            fields,
        } => {
            let Value::EnumVariant {
                enum_name: v_enum,
                variant: v_name,
                payload,
            } = val
            else {
                return false;
            };
            if let Some(expected_enum) = enum_name
                && expected_enum != v_enum
            {
                return false;
            }
            if v_name != variant || payload.len() != fields.len() {
                return false;
            }
            payload
                .iter()
                .zip(fields.iter())
                .all(|(item, pat)| match_pattern_inner(item, pat, bindings))
        }
        CompiledPattern::Struct { name, fields, rest } => {
            let Value::StructInstance {
                type_name: s_name,
                fields: s_fields,
                ..
            } = val
            else {
                return false;
            };
            if s_name != name {
                return false;
            }
            if !*rest && s_fields.len() != fields.len() {
                return false;
            }
            for (k, sub_pat) in fields {
                let Some(entry_val) = s_fields.get(k) else {
                    return false;
                };
                if !match_pattern_inner(entry_val, sub_pat, bindings) {
                    return false;
                }
            }
            true
        }
    }
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
