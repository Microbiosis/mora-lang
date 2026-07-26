//! v0.53: HM Inference Error Types

use crate::common::Span;

///  Hindley-Milner Type Inference Errors
#[derive(Debug, Clone)]
pub enum TypeError {
    UnboundVariable { name: String, span: Span },

    ArityMismatch {
        expected: usize,
        actual: usize,
        span: Span,
    },

    NotAClosure { found: String, span: Span },

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

    /// HM
    HmDisabled,

    GeneralizationFailed { reason: String, span: Option<Span> },
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
            TypeError::HmDisabled => {
                write!(f, "HM type inference disabled. Set MORA_HM=1 to enable")
            }
            TypeError::GeneralizationFailed { reason, span } => {
                write!(f, "Generalization failed: {}", reason)?;
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
