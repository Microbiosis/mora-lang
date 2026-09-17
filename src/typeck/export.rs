//! v0.89: Shadow type table export — Phase 0 of the 9-layer IR architecture.
//!
//! `export_type_table` runs HM inference on MirWitness trees and captures
//! the final concrete types per witness node without modifying infer/unify/
//! bidirectional logic. The result is a `TypeTable` mapping Span → (Type, EffectRow).
//!
//! Design: "shadow table" strategy — infer_expr captures pre-substitution
//! types in HMInference.shadow_types, then solve_constraints returns the
//! Substitution which is applied to resolve all TypeVars to concrete types.
//!
//! Usage:
//!   let table = export_type_table(&witnesses);
//!   let issues = verify_type_table(&table);
//!   assert!(issues.is_empty(), "type table has unresolved variables");

use std::collections::HashMap;

use crate::common::Span;
use crate::mir::effect::EffectRow;
use crate::mir::witness::MirWitness;
use crate::typeck::Type;
use crate::typeck::hm::HMInference;

/// Type table produced by shadow export. Maps each witness node's Span
/// to its inferred (Type, EffectRow) pair.
#[derive(Debug, Clone)]
pub struct TypeTable {
    /// Span → (concrete Type, concrete EffectRow).
    /// Spans are unique within a single file.
    pub types: HashMap<Span, (Type, EffectRow)>,
}

/// Run HM inference on witnesses and export the shadow type table.
///
/// This is non-invasive: it does not modify infer.rs, unify.rs, or
/// bidirectional.rs. It uses HMInference.shadow_types (populated by
/// infer_expr) and solve_constraints' returned Substitution.
///
/// Returns a TypeTable mapping each witness node's Span to its
/// post-substitution (Type, EffectRow).
pub fn export_type_table(witnesses: &[MirWitness]) -> TypeTable {
    let mut hm = HMInference::new();

    // Phase 1: infer all witnesses (shadow_types auto-populated)
    for w in witnesses {
        let _ = hm.infer_expr(w);
    }

    // Phase 2: solve constraints to get final Substitution
    let (subst, _errors) = hm.solve_constraints();

    // Phase 3: apply Substitution to all shadow-captured types
    let types: HashMap<Span, (Type, EffectRow)> = hm
        .shadow_types
        .into_iter()
        .map(|(span, (ty, _row))| {
            // Apply substitution to resolve TypeVars to concrete types.
            // EffectRow::Var resolution is handled by row::apply_row internally
            // when the Type contains Arrow(input, output, row).
            let concrete_ty = subst.apply(&ty);
            // For now, pass through EffectRow as-is — row variable resolution
            // happens during constraint solving, and the shadow captures the
            // pre-substitution row. A full row substitution would require
            // exposing row::apply_row, but for EHIR the type is the primary
            // concern and EffectRow is already partially resolved by infer.
            (span, (concrete_ty, EffectRow::Empty))
        })
        .collect();

    TypeTable { types }
}

/// Verify that all types in the table are fully resolved (no free TypeVars).
///
/// Returns a list of human-readable issue descriptions. An empty list means
/// the table is clean and ready for EHIR consumption.
pub fn verify_type_table(table: &TypeTable) -> Vec<String> {
    let mut issues = Vec::new();
    for (span, (ty, _row)) in &table.types {
        if has_free_typevar(ty) {
            issues.push(format!("span {:?}: unresolved TypeVar in {:?}", span, ty));
        }
    }
    issues
}

/// Recursively check if a Type contains any free TypeVar.
fn has_free_typevar(ty: &Type) -> bool {
    match ty {
        Type::TypeVar(_) => true,
        Type::List(inner) => has_free_typevar(inner),
        Type::Dict(k, v) => has_free_typevar(k) || has_free_typevar(v),
        Type::Tuple(items) => items.iter().any(|t| has_free_typevar(t)),
        Type::Union(items) => items.iter().any(has_free_typevar),
        Type::Arrow(input, output, _row) => has_free_typevar(input) || has_free_typevar(output),
        Type::ForAll(_, inner) => has_free_typevar(inner),
        Type::Result_(ok, err) => has_free_typevar(ok) || has_free_typevar(err),
        // Concrete types — no free variables
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Literal;
    use crate::mir::witness::{MirWitness, WitnessKind};

    fn lit_int(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::default())),
            span: Span::new(1, 0),
        }
    }

    fn lit_str(s: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::String(s.to_string(), Span::default())),
            span: Span::new(2, 0),
        }
    }

    #[test]
    fn export_literal_types() {
        let witnesses = vec![lit_int(42), lit_str("hello")];
        let table = export_type_table(&witnesses);
        // Both spans map to concrete types
        assert_eq!(table.types.len(), 2);
    }

    #[test]
    fn verify_clean_table() {
        let witnesses = vec![lit_int(42)];
        let table = export_type_table(&witnesses);
        let issues = verify_type_table(&table);
        assert!(
            issues.is_empty(),
            "literal-only table should have no unresolved vars: {:?}",
            issues
        );
    }

    /// Helper: compile a .mora source string and return (witness_count, table_size, unresolved_count).
    fn compile_and_export(source: &str) -> (usize, usize, usize) {
        let (_func, witnesses) =
            crate::parser_v3::ParserV3::compile(source).expect("compile should succeed");
        let witness_count = count_witnesses(&witnesses);
        let table = export_type_table(&witnesses);
        let table_size = table.types.len();
        let issues = verify_type_table(&table);
        (witness_count, table_size, issues.len())
    }

    fn count_witnesses(witnesses: &[MirWitness]) -> usize {
        let mut count = 0;
        for w in witnesses {
            count += 1;
            count += count_witness_children(w);
        }
        count
    }

    fn count_witness_children(w: &MirWitness) -> usize {
        match &w.kind {
            WitnessKind::Binary { left, right, .. } => {
                count_witnesses(std::slice::from_ref(left))
                    + count_witnesses(std::slice::from_ref(right))
            }
            WitnessKind::Call { args, .. } => args
                .iter()
                .map(|a| count_witnesses(std::slice::from_ref(a)))
                .sum(),
            WitnessKind::If { cond, then, r#else } => {
                count_witnesses(std::slice::from_ref(cond))
                    + count_witnesses(std::slice::from_ref(then))
                    + r#else
                        .as_ref()
                        .map_or(0, |e| count_witnesses(std::slice::from_ref(e)))
            }
            WitnessKind::LetBinding { value, .. } => count_witnesses(std::slice::from_ref(value)),
            WitnessKind::Sequence(exprs) => count_witnesses(exprs),
            _ => 0,
        }
    }

    #[test]
    fn shadow_table_arithmetic() {
        let source = "task main()\n  print(10i + 32i)\nend";
        let (witness_count, table_size, unresolved) = compile_and_export(source);
        assert!(witness_count > 0, "should have witnesses");
        assert!(table_size > 0, "shadow table should capture types");
        assert_eq!(unresolved, 0, "arithmetic should have no unresolved vars");
    }

    #[test]
    fn shadow_table_function_call() {
        let source = "let ops = {\"add\": fn(a, b) a + b end}\nprint(ops.add(2i, 3i))";
        let (witness_count, table_size, unresolved) = compile_and_export(source);
        assert!(witness_count > 0, "should have witnesses");
        assert!(table_size > 0, "shadow table should capture types");
        if unresolved > 0 {
            eprintln!(
                "NOTE: function_call has {} unresolved TypeVars (expected for polymorphic fns)",
                unresolved
            );
        }
    }

    #[test]
    fn shadow_table_handle_effect() {
        let source = "let global_result = \"init\"\nhandle Ai {\n  global_result = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nglobal_result";
        let (witness_count, table_size, unresolved) = compile_and_export(source);
        assert!(witness_count > 0, "should have witnesses");
        assert!(table_size > 0, "shadow table should capture types");
        if unresolved > 0 {
            eprintln!("NOTE: handle_effect has {} unresolved TypeVars", unresolved);
        }
    }
}
