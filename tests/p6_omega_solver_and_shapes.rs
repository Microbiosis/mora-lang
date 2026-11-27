//! P6-3A: Presburger Linear Shape Solver (`omega`-lite) & Monomial Volume Conservation Tests.
//!
//! Verifies the low-level P6-3 type-system foundation (no new surface syntax yet):
//! 1. Algebraic `DimExpr` equivalence in HM unification & broadcasting:
//!    commutativity (`S1 + S2 == S2 + S1`), inverse cancellation (`(S + P) - P == S`),
//!    like-term folding (`D + D == 2 * D`), distributivity (`2 * (A + B) == 2 * A + 2 * B`),
//!    exact constant division (`(2 * D + 4) / 2 == D + 2`), and monomial commutativity (`H * DH == DH * H`).
//! 2. Linear integer unification (`2 * D == 6 => D = 3`) and integer divisibility refutation (`2 * D == 5` rejected).
//! 3. Static `Tensor::cat` shape arithmetic (`[B, S1, D]` + `[B, S2, D]` on axis 1 => `[B, S1 + S2, D]`),
//!    plus compile-time rejection of non-cat axis mismatch, out-of-bounds axis, rank mismatch, and dtype mismatch.
//! 4. Static symbolic `reshape` volume conservation (`[B, S, H * DH] -> [B, S, H, DH]`, `[B, 2 * S, D] -> [B, S, 2 * D]`)
//!    and compile-time `UnsatisfiedDimConstraint` rejection on provable volume mismatches (`[B, S, H * DH] -> [B, S, H, DH * 2]`).
//! 5. Sound symbolic tensor slicing (`x[:, :S1, :] -> [B, S1, D]`, `x[:, S1:, :] -> [B, S2, D]` on `[B, S1 + S2, D]`)
//!    and static rejection of provably empty symbolic slices (`x[:, S1:S1, :]`, `x[:, (S1 + 1):S1, :]`).

use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::{CoreRuntime, vm};
use mora::typeck::TypeError;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn typecheck_errors(src: &str) -> Vec<TypeError> {
    let (_, witnesses) = ParserV3::compile(src).expect("parse source");
    check_program_witnesses(&witnesses)
}

fn eval_checked(src: &str) -> Value {
    let (prog, witnesses) = ParserV3::compile(src).expect("parse source");
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected typecheck errors: {errs:?}\nsource:\n{src}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = vm::execute_program(Arc::new(prog), &mut core).expect("execute program");
    val
}

#[test]
fn p6_3a_symbolic_dim_commutativity_cancellation_distributivity_and_broadcast() {
    let ok_src = r#"
task cat_comm<B, S1, S2, D>(a: Tensor<f32, [B, S1, D], cpu>, b: Tensor<f32, [B, S2, D], cpu>): Tensor<f32, [B, S2 + S1, D], cpu>
  return Tensor::cat([a, b], 1)
end

task cancel_pad<B, S, P, D>(x: Tensor<f32, [B, (S + P) - P, D], cpu>): Tensor<f32, [B, S, D], cpu>
  return x
end

task like_terms<B, D>(x: Tensor<f32, [B, D + D], cpu>): Tensor<f32, [B, 2 * D], cpu>
  return x
end

task distrib<A, B>(x: Tensor<f32, [2 * (A + B)], cpu>): Tensor<f32, [2 * A + 2 * B], cpu>
  return x
end

task exact_div<D>(x: Tensor<f32, [(2 * D + 4) / 2], cpu>): Tensor<f32, [D + 2], cpu>
  return x
end

task mul_comm<H, DH>(q: Tensor<f32, [H, DH], cpu>, x: Tensor<f32, [H * DH], cpu>): Tensor<f32, [DH * H], cpu>
  return x
end

task bcast_comm<B, S1, S2, D>(a: Tensor<f32, [B, S1 + S2, D], cpu>, b: Tensor<f32, [1, S2 + S1, D], cpu>): Tensor<f32, [B, S1 + S2, D], cpu>
  return a + b
end

let a = Tensor::ones([2, 3, 4], "f32", "cpu")
let b = Tensor::ones([2, 5, 4], "f32", "cpu")
let c = cat_comm(a, b)
let bias = Tensor::ones([1, 8, 4], "f32", "cpu")
let sum = bcast_comm(c, bias)
let lt = like_terms(Tensor::ones([2, 6], "f32", "cpu"))
let ed = exact_div(Tensor::ones([7], "f32", "cpu"))
let mc = mul_comm(Tensor::ones([3, 4], "f32", "cpu"), Tensor::ones([12], "f32", "cpu"))
[c.shape(), sum.shape(), lt.shape(), ed.shape(), mc.shape()]
"#;
    let val = eval_checked(ok_src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::List(vec![Value::Int(2), Value::Int(8), Value::Int(4)].into()),
                Value::List(vec![Value::Int(2), Value::Int(8), Value::Int(4)].into()),
                Value::List(vec![Value::Int(2), Value::Int(6)].into()),
                Value::List(vec![Value::Int(7)].into()),
                Value::List(vec![Value::Int(12)].into()),
            ]
            .into()
        )
    );

    // Negative: S + 1 != S + 2 must be rejected statically
    let errs = typecheck_errors(
        r#"
task bad_off<S>(x: Tensor<f32, [S + 1], cpu>): Tensor<f32, [S + 2], cpu>
  return x
end
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static rejection for S + 1 vs S + 2"
    );

    // Negative: 2 * D == 5 has no integer solution over Z!
    let errs_odd = typecheck_errors(
        r#"
task even_only<D>(x: Tensor<f32, [2 * D, 4], cpu>): Tensor<f32, [D + D, 4], cpu>
  return x
end
let _ = even_only(Tensor::ones([5, 4], "f32", "cpu"))
"#,
    );
    assert!(
        !errs_odd.is_empty(),
        "expected static integer divisibility rejection for 2 * D == 5"
    );
}

#[test]
fn p6_3a_tensor_cat_static_shape_arithmetic_and_error_guards() {
    // Concrete heterogeneous cat-axis list literal works directly with exact output shape [6, 4]!
    let ok_src = r#"
let a = Tensor::ones([3, 4], "f32", "cpu")
let b = Tensor::ones([2, 4], "f32", "cpu")
let c = Tensor::ones([1, 4], "f32", "cpu")
let cat0: Tensor<f32, [6, 4], cpu> = Tensor::cat([a, b, c], 0)
let cat1: Tensor<f32, [2, 7], cpu> = Tensor::cat([Tensor::ones([2, 3], "f32", "cpu"), Tensor::ones([2, 4], "f32", "cpu")], -1)
[cat0.shape(), cat1.shape()]
"#;
    let val = eval_checked(ok_src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::List(vec![Value::Int(6), Value::Int(4)].into()),
                Value::List(vec![Value::Int(2), Value::Int(7)].into()),
            ]
            .into()
        )
    );

    // Negative 1: non-cat axis mismatch ([2, 3, 4] vs [2, 5, 8] along axis 1)
    let errs_axis = typecheck_errors(
        r#"
let a = Tensor::ones([2, 3, 4], "f32", "cpu")
let b = Tensor::ones([2, 5, 8], "f32", "cpu")
let _ = Tensor::cat([a, b], 1)
"#,
    );
    assert!(
        errs_axis
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint on non-cat axis mismatch, got {errs_axis:?}"
    );

    // Negative 2: out-of-bounds cat axis
    let errs_oob = typecheck_errors(
        r#"
let a = Tensor::ones([2, 3], "f32", "cpu")
let b = Tensor::ones([2, 4], "f32", "cpu")
let _ = Tensor::cat([a, b], 3)
"#,
    );
    assert!(
        errs_oob
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint on out-of-bounds cat axis, got {errs_oob:?}"
    );

    // Negative 3: rank mismatch
    let errs_rank = typecheck_errors(
        r#"
let a = Tensor::ones([2, 3], "f32", "cpu")
let b = Tensor::ones([2, 3, 4], "f32", "cpu")
let _ = Tensor::cat([a, b], 0)
"#,
    );
    assert!(!errs_rank.is_empty(), "expected rank mismatch rejection");

    // Negative 4: dtype mismatch
    let errs_dt = typecheck_errors(
        r#"
let a = Tensor::ones([2, 3], "f32", "cpu")
let b = Tensor::ones([2, 3], "f64", "cpu")
let _ = Tensor::cat([a, b], 0)
"#,
    );
    assert!(!errs_dt.is_empty(), "expected dtype mismatch rejection");
}

#[test]
fn p6_3a_symbolic_reshape_volume_conservation_and_mismatch_rejection() {
    let ok_src = r#"
struct MultiHeadSplit<const B: int, const S: int, const H: int, const DH: int>
  tag: int
end

impl<const B: int, const S: int, const H: int, const DH: int> MultiHeadSplit<B, S, H, DH>
  task split_heads(self, x: Tensor<f32, [B, S, H * DH], cpu>): Tensor<f32, [B, S, H, DH], cpu>
    return x.reshape([B, S, H, DH])
  end

  task merge_heads(self, x: Tensor<f32, [B, S, H, DH], cpu>): Tensor<f32, [B, S, DH * H], cpu>
    return x.reshape([B, S, DH * H])
  end

  task shift_factor(self, x: Tensor<f32, [B, 2 * S, DH], cpu>): Tensor<f32, [B, S, 2 * DH], cpu>
    return x.reshape([B, S, 2 * DH])
  end
end

let m = MultiHeadSplit<2, 4, 3, 5>(tag = 1)
let x = Tensor::ones([2, 4, 15], "f32", "cpu")
let h = m.split_heads(x)
let back = m.merge_heads(h)
let shifted = m.shift_factor(Tensor::ones([2, 8, 5], "f32", "cpu"))
[h.shape(), back.shape(), shifted.shape()]
"#;
    let val = eval_checked(ok_src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::List(
                    vec![Value::Int(2), Value::Int(4), Value::Int(3), Value::Int(5)].into()
                ),
                Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(15)].into()),
                Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(10)].into()),
            ]
            .into()
        )
    );

    // Negative 1: symbolic reshape volume mismatch ([B, S, H * DH] -> [B, S, H, DH * 2] reduces to 1 != 2)
    let errs_sym = typecheck_errors(
        r#"
struct BadSplit<const B: int, const S: int, const H: int, const DH: int>
  tag: int
end
impl<const B: int, const S: int, const H: int, const DH: int> BadSplit<B, S, H, DH>
  task bad(self, x: Tensor<f32, [B, S, H * DH], cpu>): Tensor<f32, [B, S, H, DH * 2], cpu>
    return x.reshape([B, S, H, DH * 2])
  end
end
"#,
    );
    assert!(
        errs_sym
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint for symbolic reshape mismatch, got {errs_sym:?}"
    );

    // Negative 2: concrete reshape volume mismatch ([2, 6] -> [3, 5])
    let errs_conc = typecheck_errors(
        r#"
let _ = Tensor::ones([2, 6], "f32", "cpu").reshape([3, 5])
"#,
    );
    assert!(
        errs_conc
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint for concrete reshape mismatch, got {errs_conc:?}"
    );
}

#[test]
fn p6_3a_symbolic_slice_bounds_cancellation_and_empty_rejection() {
    let ok_src = r#"
struct SeqRouter<const B: int, const S1: int, const S2: int, const D: int>
  tag: int
end

impl<const B: int, const S1: int, const S2: int, const D: int> SeqRouter<B, S1, S2, D>
  task swap_halves(self, a: Tensor<f32, [B, S1, D], cpu>, b: Tensor<f32, [B, S2, D], cpu>): Tensor<f32, [B, S1 + S2, D], cpu>
    let joined: Tensor<f32, [B, S1 + S2, D], cpu> = Tensor::cat([a, b], 1)
    let first: Tensor<f32, [B, S1, D], cpu> = joined[:, :S1, :]
    let second: Tensor<f32, [B, S2, D], cpu> = joined[:, S1:, :]
    return Tensor::cat([second, first], 1)
  end
end

let r = SeqRouter<1, 2, 3, 2>(tag = 0)
let a = Tensor::full([1, 2, 2], 1.0f32, "f32", "cpu")
let b = Tensor::full([1, 3, 2], 2.0f32, "f32", "cpu")
let out = r.swap_halves(a, b)
let sh = out.shape()
[sh[0], sh[1], sh[2], int(out[0, 0, 0]), int(out[0, 3, 0])]
"#;
    let val = eval_checked(ok_src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::Int(1),
                Value::Int(5),
                Value::Int(2),
                Value::Int(2),
                Value::Int(1),
            ]
            .into()
        )
    );

    // Negative 3: provably out-of-bounds symbolic index `x[0, S1 + S2, 0]` on `[B, S1 + S2, D]`
    let errs_idx_oob = typecheck_errors(
        r#"
struct BadIdx<const B: int, const S1: int, const S2: int, const D: int>
  tag: int
end
impl<const B: int, const S1: int, const S2: int, const D: int> BadIdx<B, S1, S2, D>
  task oob(self, x: Tensor<f32, [B, S1 + S2, D], cpu>): f32
    return x[0, S1 + S2, 0]
  end
end
"#,
    );
    assert!(
        errs_idx_oob
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint on out-of-bounds symbolic index S1 + S2, got {errs_idx_oob:?}"
    );

    // Negative 1: provably empty symbolic slice `joined[:, S1:S1, :]`
    let errs_empty = typecheck_errors(
        r#"
struct BadSlice<const B: int, const S1: int, const S2: int, const D: int>
  tag: int
end
impl<const B: int, const S1: int, const S2: int, const D: int> BadSlice<B, S1, S2, D>
  task empty(self, x: Tensor<f32, [B, S1 + S2, D], cpu>): Tensor<f32, [B, S1, D], cpu>
    return x[:, S1:S1, :]
  end
end
"#,
    );
    assert!(
        errs_empty
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint on provably empty symbolic slice S1:S1, got {errs_empty:?}"
    );

    // Negative 2: provably inverted symbolic slice `joined[:, (S1 + 1):S1, :]`
    let errs_inv = typecheck_errors(
        r#"
struct BadSlice2<const B: int, const S1: int, const S2: int, const D: int>
  tag: int
end
impl<const B: int, const S1: int, const S2: int, const D: int> BadSlice2<B, S1, S2, D>
  task inv(self, x: Tensor<f32, [B, S1 + S2, D], cpu>): Tensor<f32, [B, S1, D], cpu>
    return x[:, (S1 + 1):S1, :]
  end
end
"#,
    );
    assert!(
        errs_inv
            .iter()
            .any(|e| e.message.contains("Unsatisfied dimension constraint")),
        "expected UnsatisfiedDimConstraint on inverted slice (S1+1):S1, got {errs_inv:?}"
    );
}
