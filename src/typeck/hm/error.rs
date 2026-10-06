//! v0.53: HM Inference Error Types

use crate::common::Span;

///  Hindley-Milner Type Inference Errors
#[derive(Debug, Clone)]
pub enum TypeError {
    UnboundVariable {
        name: String,
        span: Span,
    },

    ArityMismatch {
        expected: usize,
        actual: usize,
        span: Span,
    },

    NotAClosure {
        found: String,
        span: Span,
    },

    UnificationFailure {
        expected: String,
        got: String,
        span: Option<Span>,
    },

    /// occurs check failure
    OccursCheck {
        var: char, // type variable identifier
        with_ty: String,
        span: Option<Span>,
    },

    GeneralizationFailed {
        reason: String,
        span: Option<Span>,
    },

    /// v0.75.24: 内置参数的字面量非法值（编译期校验）。
    /// 例：`merge_with("x", "bogus")` — 非法策略名在 typeck 阶段拦截，
    /// 不再留到运行时。
    InvalidLiteral {
        what: String,
        value: String,
        span: Option<Span>,
    },

    /// v0.80: Effect row unification failure (algebraic effects).
    /// Two effect rows could not be unified (e.g. different labels, or
    /// concrete row vs empty).
    EffectRowMismatch {
        expected: String,
        got: String,
        span: Option<Span>,
    },

    /// v0.104.6 D124：dict **字面量**的各个值类型不一致。
    ///
    /// 单列一个变体而不是复用 `UnificationFailure`：
    /// - 那条消息是通用类型不匹配，**没说清「为什么这里必须是同一种类型」**
    ///   ——而 dict 字面量同质是 HM 推断（`dict<K, V>` 需要单一 V）的硬约束，
    ///   挡住 `{status: 200, body: "…"}` 这类自然写法，用户极难自行猜到。
    /// - `span` 指向**出问题的那个值**而非整个 dict（旧的实现指向 `{`），
    ///   且带上键名，定位直接可用。
    DictValueTypeMismatch {
        key: String,
        expected: String,
        got: String,
        span: Option<Span>,
    },

    /// v0.104.6 D125：`list` **字面量**的元素类型不一致（`infer_list` 的姊妹约束）。
    ///
    /// 与 [`TypeError::DictValueTypeMismatch`] 同源同因：`list<T>` 需要单一
    /// `T`，而通用的 `UnificationFailure` 消息既不说明约束、span 又指向整个
    /// 列表的 `[`。此处带上**下标**，并把 span 指向出错的那个元素。
    ListElementTypeMismatch {
        index: usize,
        expected: String,
        got: String,
        span: Option<Span>,
    },
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeError::UnboundVariable { name, span } => {
                write!(f, "Unbound variable '{}'", name)?;
                format_location(f, &Some(*span))
            }
            TypeError::ArityMismatch {
                expected,
                actual,
                span,
            } => {
                write!(f, "Expected {} arguments, got {}", expected, actual)?;
                format_location(f, &Some(*span))
            }
            TypeError::NotAClosure { found, span } => {
                write!(f, "Expected closure, found '{}'", found)?;
                format_location(f, &Some(*span))
            }
            TypeError::UnificationFailure {
                expected,
                got,
                span,
            } => {
                write!(f, "Type mismatch: expected {}, got {}", expected, got)?;
                if let Some(s) = span {
                    write!(f, " at line {}, column {}", s.line, s.column)
                } else {
                    Ok(())
                }
            }
            TypeError::DictValueTypeMismatch {
                key,
                expected,
                got,
                span: _,
            } => {
                // ⚠ **不**在这里附 `at line/column`：对外的 `format_error`
                //   会用结构体的 `line`/`column` 字段渲染位置（`Type error at
                //   line L:C`），HM 侧再带一遍就是重复。本分支只给消息文本。
                //   （旧的 `UnificationFailure` 分支确实重复，但那是既有行为，
                //   改动面超出本轮范围。）
                write!(
                    f,
                    "Dict 字面量的值必须同质（`dict<K, V>` 需要单一 V 类型）: \
                     键 '{key}' 的值是 {got}，而前面的值是 {expected}"
                )
            }
            TypeError::ListElementTypeMismatch {
                index,
                expected,
                got,
                span: _,
            } => {
                // 同上：位置由对外 `format_error` 用结构体字段渲染，这里不带。
                //
                // `index` 是 **0-based 下标**（与 `list[idx]` 写法一致）。
                // 两种编号都给：只给下标时「第 0 个元素」读着别扭，只给序数时
                // 用户又会拿它去 `list[n]` 里取下标而错位一个。
                write!(
                    f,
                    "List 字面量的元素必须同质（`list<T>` 需要单一 T 类型）: \
                     下标 {index}（第 {} 个元素）是 {got}，而前面的元素是 {expected}",
                    index + 1
                )
            }
            TypeError::OccursCheck { var, with_ty, span } => {
                write!(
                    f,
                    "Cannot unify type variable '{}' with type containing itself: {}",
                    var, with_ty
                )?;
                if let Some(s) = span {
                    write!(f, " at line {}, column {}", s.line, s.column)
                } else {
                    Ok(())
                }
            }
            TypeError::GeneralizationFailed { reason, span } => {
                write!(f, "Generalization failed: {}", reason)?;
                if let Some(s) = span {
                    write!(f, " at line {}, column {}", s.line, s.column)
                } else {
                    Ok(())
                }
            }
            TypeError::InvalidLiteral { what, value, span } => {
                write!(f, "Invalid {} literal '{}'", what, value)?;
                if let Some(s) = span {
                    write!(f, " at line {}, column {}", s.line, s.column)
                } else {
                    Ok(())
                }
            }
            TypeError::EffectRowMismatch {
                expected,
                got,
                span,
            } => {
                write!(f, "Effect row mismatch: expected {}, got {}", expected, got)?;
                if let Some(s) = span {
                    write!(f, " at line {}, column {}", s.line, s.column)
                } else {
                    Ok(())
                }
            }
        }
    }
}

fn format_location(f: &mut std::fmt::Formatter<'_>, span: &Option<Span>) -> std::fmt::Result {
    if let Some(s) = span {
        write!(f, " at line {}, column {}", s.line, s.column)
    } else {
        Ok(())
    }
}

impl std::error::Error for TypeError {}
