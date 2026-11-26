//! 模式先检查整个结构，成功后沿同一组投影收集绑定。只有独占的字段允许移动。
use std::sync::Arc;

use crate::flow::{dynamic_type_name, type_name};
use crate::mir::control::{CompiledPattern, PatternTarget};
use crate::runtime::vm::RegSlot;
use crate::value::Value;

enum Subject<'a> {
    Shared(&'a Value),
    Exclusive(&'a mut Value),
}

#[derive(Clone, Copy)]
enum Field<'a> {
    List(usize),
    Payload(usize),
    Named(&'a str),
    Car,
    Cdr,
}

impl Field<'_> {
    fn read(self, value: &Value) -> Option<&Value> {
        match (self, value) {
            (Self::List(i), Value::List(list)) => list.get(i),
            (Self::Payload(i), Value::EnumVariant { payload, .. }) => payload.get(i),
            (Self::Named(key), Value::Dict(fields) | Value::StructInstance { fields, .. }) => {
                fields.get(key)
            }
            (Self::Car, Value::Cons { car, .. }) => Some(car),
            (Self::Cdr, Value::Cons { cdr, .. }) => Some(cdr),
            _ => None,
        }
    }

    fn exclusive(self, value: &mut Value) -> Option<&mut Value> {
        match (self, value) {
            (Self::List(i), Value::List(list)) => list.get_mut_if_unique(i),
            (Self::Payload(i), Value::EnumVariant { payload, .. }) => payload.get_mut(i),
            (Self::Named(key), Value::Dict(fields) | Value::StructInstance { fields, .. }) => {
                fields.get_mut_if_unique(key)
            }
            (Self::Car, Value::Cons { car, .. }) => Some(car),
            (Self::Cdr, Value::Cons { cdr, .. }) => Some(cdr),
            _ => None,
        }
    }
}

enum Bindings<'a> {
    Check { has_bindings: bool },
    Collect(&'a mut Vec<(PatternTarget, Value)>),
}

impl Subject<'_> {
    fn value(&self) -> &Value {
        match self {
            Self::Shared(value) => value,
            Self::Exclusive(value) => value,
        }
    }

    fn capture(self, start: usize) -> Value {
        if start == 0 {
            return match self {
                Self::Shared(value) => value.clone(),
                Self::Exclusive(value) => std::mem::replace(value, Value::Nil),
            };
        }
        // start 非零仅出现在列表尾模式，且只有实际绑定尾列表时才会物化。
        match self {
            Self::Shared(Value::List(list)) => Value::List(list.slice(start, list.len())),
            Self::Exclusive(Value::List(list)) => {
                let tail = (start..list.len())
                    .map(|i| {
                        if let Some(item) = list.get_mut_if_unique(i) {
                            std::mem::replace(item, Value::Nil)
                        } else {
                            list[i].clone()
                        }
                    })
                    .collect();
                Value::List(tail)
            }
            _ => unreachable!("列表尾只能由列表模式投影"),
        }
    }

    fn visit_field(
        &mut self,
        field: Field<'_>,
        pattern: &CompiledPattern,
        bindings: &mut Bindings<'_>,
    ) -> bool {
        if let Self::Exclusive(value) = self
            && let Some(child) = field.exclusive(value)
        {
            return visit(Subject::Exclusive(child), 0, pattern, bindings);
        }
        field
            .read(self.value())
            .is_some_and(|child| visit(Subject::Shared(child), 0, pattern, bindings))
    }

    fn visit_named(
        &mut self,
        fields: &[(String, CompiledPattern)],
        bindings: &mut Bindings<'_>,
    ) -> bool {
        // 同一字段可在模式中出现多次。前面的投影仍有别名，最后一次才可消费它。
        let last_use = if matches!(self, Self::Exclusive(_)) && fields.len() > 1 {
            fields
                .iter()
                .enumerate()
                .map(|(i, (key, _))| (key.as_str(), i))
                .collect::<std::collections::HashMap<_, _>>()
        } else {
            Default::default()
        };
        fields.iter().enumerate().all(|(index, (key, pattern))| {
            let field = Field::Named(key);
            if last_use
                .get(key.as_str())
                .is_some_and(|&last| last != index)
            {
                Subject::Shared(self.value()).visit_field(field, pattern, bindings)
            } else {
                self.visit_field(field, pattern, bindings)
            }
        })
    }
}

fn visit(
    mut subject: Subject<'_>,
    start: usize,
    pattern: &CompiledPattern,
    bindings: &mut Bindings<'_>,
) -> bool {
    use CompiledPattern as P;
    match pattern {
        P::Wildcard | P::Bind(PatternTarget::Discard) => true,
        P::Bind(target) => {
            match bindings {
                Bindings::Check { has_bindings } => *has_bindings = true,
                Bindings::Collect(out) => out.push((*target, subject.capture(start))),
            }
            true
        }
        P::Nil => matches!(subject.value(), Value::Nil),
        P::Bool(b) => matches!(subject.value(), Value::Bool(v) if v == b),
        P::Int(n) => matches!(subject.value(), Value::Int(v) if v == n),
        P::Float(f) => matches!(subject.value(), Value::Float(v) if v == f),
        P::Scalar(s) => matches!(subject.value(), Value::Scalar(v) if v == s),
        P::Char(c) => matches!(subject.value(), Value::Char(v) if v == c),
        P::String(s) => matches!(subject.value(), Value::String(v) if v == s),
        P::Tuple(elements) | P::ListVec { elements, .. } => {
            let Value::List(list) = subject.value() else {
                return false;
            };
            let rest = match pattern {
                P::ListVec { rest, .. } => rest.as_deref(),
                _ => None,
            };
            let available = list.len() - start;
            if elements.len() > available || (rest.is_none() && elements.len() != available) {
                return false;
            }
            elements
                .iter()
                .enumerate()
                .all(|(i, pat)| subject.visit_field(Field::List(start + i), pat, bindings))
                && rest.is_none_or(|pat| visit(subject, start + elements.len(), pat, bindings))
        }
        P::ListCons { head, tail } => match subject.value() {
            Value::Cons { .. } => {
                subject.visit_field(Field::Car, head, bindings)
                    && subject.visit_field(Field::Cdr, tail, bindings)
            }
            Value::List(list) if start < list.len() => {
                subject.visit_field(Field::List(start), head, bindings)
                    && visit(subject, start + 1, tail, bindings)
            }
            _ => false,
        },
        P::Dict { required, rest } => {
            matches!(subject.value(), Value::Dict(map) if *rest || map.len() == required.len())
                && subject.visit_named(required, bindings)
        }
        P::TypeAscription { ty_name, inner } => {
            let value = subject.value();
            let matches_type = match ty_name.as_str() {
                "number" => matches!(value, Value::Int(_) | Value::Float(_)),
                "dynamic" | "any" => true,
                other => type_name(value) == other || dynamic_type_name(value) == other,
            };
            matches_type && visit(subject, start, inner, bindings)
        }
        P::Variant {
            enum_name,
            variant,
            fields,
        } => {
            matches!(subject.value(), Value::EnumVariant { enum_name: actual_enum, variant: actual_variant, payload, .. }
                if enum_name.as_ref().is_none_or(|name| name == actual_enum)
                    && variant == actual_variant && fields.len() == payload.len())
                && fields
                    .iter()
                    .enumerate()
                    .all(|(i, pat)| subject.visit_field(Field::Payload(i), pat, bindings))
        }
        P::Struct { name, fields, rest } => {
            matches!(subject.value(), Value::StructInstance { def, fields: values, .. }
                if &def.name == name && (*rest || fields.len() == values.len()))
                && subject.visit_named(fields, bindings)
        }
    }
}

fn match_value(
    subject: Subject<'_>,
    pattern: &CompiledPattern,
) -> Option<Vec<(PatternTarget, Value)>> {
    let mut check = Bindings::Check {
        has_bindings: false,
    };
    if !visit(Subject::Shared(subject.value()), 0, pattern, &mut check) {
        return None;
    }
    let mut out = Vec::new();
    if matches!(check, Bindings::Check { has_bindings: true }) {
        let matched = visit(subject, 0, pattern, &mut Bindings::Collect(&mut out));
        debug_assert!(matched, "提取绑定时模式结构必须与检查时一致");
    }
    Some(out)
}

/// 借用匹配：仅为成功匹配的实际绑定构造值。
pub fn match_pattern(
    value: &Value,
    pattern: &CompiledPattern,
) -> Option<Vec<(PatternTarget, Value)>> {
    match_value(Subject::Shared(value), pattern)
}

/// 寄存器最后使用时，仍需逐层验证共享所有权才能移动绑定。失败时保留原值。
pub(crate) fn match_pattern_slot(
    slot: &mut RegSlot,
    pattern: &CompiledPattern,
    consume: bool,
) -> Option<Vec<(PatternTarget, Value)>> {
    let bindings = if consume
        && let RegSlot::Heap(shared) = slot
        && let Some(value) = Arc::get_mut(shared)
    {
        match_value(Subject::Exclusive(value), pattern)
    } else if let Some(value) = slot.as_heap() {
        match_pattern(value, pattern)
    } else {
        match_pattern(&slot.to_value(), pattern)
    };
    if consume && bindings.is_some() {
        *slot = RegSlot::Nil;
    }
    bindings
}
