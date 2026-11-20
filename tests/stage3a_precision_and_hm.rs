//! Stage 3A end-to-end verification tests covering:
//! - Track 3A-1: lexical numeric precision, suffixes, underscores, scientific notation,
//!   right-associative `**`, `@` matmul, unary `!` / `not`, and Int returns from
//!   `range`, `random.rand_int`, and `list.shape`.
//! - Track 3A-2: unified Span-tracked HM solver, rigid/multi-char generics, qualified
//!   let-polymorphism with ML Value Restriction, Type::Error poison recovery,
//!   BuiltinModule fail-closed diagnostics, and undefined free-function rejection.

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;
use std::sync::Arc;

fn typeck_errors(src: &str) -> Result<Vec<mora::typeck::TypeError>, String> {
    let (_prog, witnesses) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    Ok(check_program_witnesses(&witnesses))
}

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) = ParserV3::compile(src).expect("source must compile");
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors for {src:?}: {errs:?}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core).expect("execution must succeed");
    val
}

// ============================================================================
// Track 3A-1: Lexical Precision, Operators & Int-Returning Builtins
// ============================================================================

#[test]
fn track3a1_numeric_literals_underscores_scientific_and_suffixes() {
    use mora::common::{
        ScalarValue, f64_to_bf16_bits, f64_to_f16_bits, f64_to_fp8_e4m3_bits, f64_to_fp8_e5m2_bits,
    };
    for (src, expected) in [
        ("1", Value::Int(1)),
        ("42", Value::Int(42)),
        ("1_000_000", Value::Int(1_000_000)),
        ("255u8", Value::Scalar(ScalarValue::U8(255))),
        ("127i8", Value::Scalar(ScalarValue::I8(127))),
        ("42i", Value::Int(42)),
        ("100u64", Value::Scalar(ScalarValue::U64(100))),
        ("1.0", Value::Float(1.0)),
        ("1e5", Value::Float(100_000.0)),
        ("2.5e-2", Value::Float(0.025)),
        ("1_000.5", Value::Float(1_000.5)),
        ("1f32", Value::Scalar(ScalarValue::F32(1.0))),
        (
            "1f16",
            Value::Scalar(ScalarValue::F16(f64_to_f16_bits(1.0))),
        ),
        (
            "1bf16",
            Value::Scalar(ScalarValue::BF16(f64_to_bf16_bits(1.0))),
        ),
        (
            "1fp8",
            Value::Scalar(ScalarValue::FP8E4M3(f64_to_fp8_e4m3_bits(1.0))),
        ),
        (
            "1fp8_e5m2",
            Value::Scalar(ScalarValue::FP8E5M2(f64_to_fp8_e5m2_bits(1.0))),
        ),
    ] {
        let got = compile_check_and_run(&format!("{src}\n"));
        assert_eq!(got, expected, "literal `{src}` mismatch");
    }

    assert_eq!(
        compile_check_and_run("type_of(1)\n"),
        Value::String("int".to_string())
    );
    assert_eq!(
        compile_check_and_run("type_of(1.0)\n"),
        Value::String("float".to_string())
    );
    assert_eq!(
        compile_check_and_run("type_of(1e5)\n"),
        Value::String("float".to_string())
    );

    for bad_lit in [
        "1i7",
        "1.5f7",
        "1.0i32",
        "256u8",
        "1e+",
        "99999999999999999999999999",
    ] {
        let res = ParserV3::compile(&format!("{bad_lit}\n"));
        assert!(
            res.is_err(),
            "malformed/unsupported numeric literal `{bad_lit}` must fail to compile"
        );
    }
}

#[test]
fn track3a1_power_operator_right_associative_and_precedence() {
    assert_eq!(compile_check_and_run("2 ** 3 ** 2\n"), Value::Int(512));
    assert_eq!(compile_check_and_run("2 * 3 ** 2\n"), Value::Int(18));
    assert_eq!(compile_check_and_run("-2 ** 2\n"), Value::Int(-4));
    assert_eq!(compile_check_and_run("(-2) ** 2\n"), Value::Int(4));
    assert_eq!(compile_check_and_run("2.0 ** 3.0\n"), Value::Float(8.0));
}

#[test]
fn track3a1_matmul_vectors_matrices_and_type_rejection() {
    assert_eq!(
        compile_check_and_run("[1, 2, 3] @ [4, 5, 6]\n"),
        Value::Int(32)
    );
    assert_eq!(
        compile_check_and_run("[1.0, 2.0] @ [3, 4]\n"),
        Value::Float(11.0)
    );
    let mat = compile_check_and_run("[[1, 2], [3, 4]] @ [[5, 6], [7, 8]]\n");
    assert_eq!(
        mat.to_string(),
        "[[19, 22], [43, 50]]",
        "2x2 matmul should produce exact Int matrix"
    );
    let mv = compile_check_and_run("[[1, 2], [3, 4]] @ [5, 6]\n");
    assert_eq!(mv.to_string(), "[17, 39]");

    for bad in ["\"a\" @ \"b\"\n", "[\"a\"] @ [\"b\"]\n"] {
        let errs = typeck_errors(bad).expect("should compile to witnesses");
        assert!(
            !errs.is_empty(),
            "non-numeric `@` `{bad}` must be rejected statically"
        );
    }

    let (prog, witnesses) = ParserV3::compile("[1, 2] @ [1, 2, 3]\n").expect("compile");
    assert!(check_program_witnesses(&witnesses).is_empty());
    let mut core = CoreRuntime::default();
    assert!(execute_program(Arc::new(prog), &mut core).is_err());
}

#[test]
fn track3a1_unary_not_and_bang() {
    assert_eq!(compile_check_and_run("!true\n"), Value::Bool(false));
    assert_eq!(compile_check_and_run("!false\n"), Value::Bool(true));
    assert_eq!(compile_check_and_run("not false\n"), Value::Bool(true));
    assert_eq!(compile_check_and_run("not true\n"), Value::Bool(false));
    assert_eq!(compile_check_and_run("!!true\n"), Value::Bool(true));
    assert_eq!(
        compile_check_and_run("let b: bool = !(1 > 2)\nb\n"),
        Value::Bool(true)
    );
    for bad in ["!1\n", "not \"yes\"\n"] {
        let errs = typeck_errors(bad).expect("compile");
        assert!(
            !errs.is_empty(),
            "unary not `{bad}` on non-bool must be rejected by typeck"
        );
    }
}

#[test]
fn track3a1_range_rand_int_and_shape_produce_int() {
    let range_val = compile_check_and_run("let xs: list<int> = range(0, 4)\nxs\n");
    assert_eq!(range_val.to_string(), "[0, 1, 2, 3]");

    let bad_range = typeck_errors("let xs: list<string> = range(3)\nxs\n").expect("compile");
    assert!(!bad_range.is_empty());

    let rand_val = compile_check_and_run("let r: int = random.rand_int(7, 7)\nr\n");
    assert_eq!(rand_val, Value::Int(7));

    let shape_val = compile_check_and_run("let s: list<int> = [[1, 2, 3], [4, 5, 6]].shape()\ns\n");
    assert_eq!(shape_val.to_string(), "[2, 3]");
}

// ============================================================================
// Track 3A-2: Unified HM Constraint Solver & Type Algebra
// ============================================================================

#[test]
fn track3a2_rigid_generics_and_multichar_type_params() {
    let ok = compile_check_and_run(
        "task first<TT, Tx>(a: TT, b: Tx) : TT\n  return a\nend\nfirst(1, \"ok\")\n",
    );
    assert_eq!(ok, Value::Int(1));

    let bad_arith =
        typeck_errors("task bad<T>(x: T) : T\n  return x + 1\nend\n").expect("must compile");
    assert!(
        !bad_arith.is_empty(),
        "rigid generic `T` must not unify with numeric `+`"
    );

    let bad_ret = typeck_errors("task swap_err<TT, Tx>(a: TT, b: Tx) : TT\n  return b\nend\n")
        .expect("must compile");
    assert!(
        !bad_ret.is_empty(),
        "distinct rigid generics `TT` and `Tx` must not unify"
    );
}

#[test]
fn track3a2_qualified_let_polymorphism_and_ml_value_restriction() {
    let ok_poly = compile_check_and_run(
        "let inc = fn(x) x + 1 end\nlet a: int = inc(1)\nlet b: float = inc(2.5)\n[a, int(b)]\n",
    );
    assert_eq!(ok_poly.to_string(), "[2, 3]");

    let bad_poly =
        typeck_errors("let inc = fn(x) x + 1 end\nlet a = inc(1)\nlet b = inc(\"s\")\nb\n")
            .expect("compile");
    assert!(
        !bad_poly.is_empty(),
        "qualified let-polymorphism must still reject incompatible string argument"
    );

    // Value restriction: call result `make_id()` is non-syntactic-value and stays monomorphic.
    let value_restricted = typeck_errors(
        "task make_id()\n  return fn(x) x end\nend\n\
         let id_mono = make_id()\n\
         let a = id_mono(1)\n\
         let b = id_mono(\"s\")\n\
         b\n",
    )
    .expect("compile");
    assert!(
        !value_restricted.is_empty(),
        "ML value restriction must prevent generalizing result of function application"
    );
}

#[test]
fn track3a2_diagnostic_spans_and_error_poison_recovery() {
    let errs = typeck_errors("let x = 1\nlet y = 2\nlet bad: string = 42\nbad\n").expect("compile");
    assert_eq!(errs.len(), 1, "expected exactly 1 type error, got {errs:?}");
    assert_eq!(errs[0].line, 3, "diagnostic span should point to line 3");
    assert!(errs[0].column >= 1, "column must be 1-based positive");

    // Broken expression on line 1 should poison `broken` with Type::Error and not
    // cause noisy cascading mismatch diagnostics when `broken` is referenced below.
    let cascade = typeck_errors("let broken = missing_var\nlet derived: int = broken\nderived\n")
        .expect("compile");
    assert_eq!(
        cascade.len(),
        1,
        "poison sentinel should avoid secondary cascade errors, got {cascade:?}"
    );
    assert!(
        cascade[0].message.contains("missing_var"),
        "root diagnostic should name missing_var: {cascade:?}"
    );
}

#[test]
fn track3a2_builtin_modules_and_undefined_functions_fail_closed() {
    for module_name in ["math", "json", "random"] {
        let errs = typeck_errors(&format!("{module_name}(1)\n")).expect("compile");
        assert!(
            !errs.is_empty(),
            "calling builtin module `{module_name}(1)` directly must be rejected"
        );
        assert!(
            errs.iter()
                .any(|e| e.message.contains("is a module, not a function")),
            "expected module-call error for `{module_name}`, got {errs:?}"
        );

        let arith_errs = typeck_errors(&format!("{module_name} + 1\n")).expect("compile");
        assert!(
            !arith_errs.is_empty(),
            "using builtin module `{module_name}` in arithmetic must fail closed"
        );
    }

    let undef_errs = typeck_errors("totally_missing_free_fn(1, 2)\n").expect("compile");
    assert!(
        !undef_errs.is_empty(),
        "undefined free function calls must be rejected statically"
    );

    // Pre-registered mutually recursive tasks typecheck and execute cleanly.
    let mutual = compile_check_and_run(
        "task is_even(n: int) : bool\n  if n == 0 then\n    return true\n  end\n  return is_odd(n - 1)\nend\n\
         task is_odd(n: int) : bool\n  if n == 0 then\n    return false\n  end\n  return is_even(n - 1)\nend\n\
         is_even(6)\n",
    );
    assert_eq!(mutual, Value::Bool(true));

    // Undefined PascalCase::method calls must also fail closed rather than escaping as Any.
    let assoc_undef = typeck_errors("NonExistent::foo(1)\n").expect("compile");
    assert!(
        !assoc_undef.is_empty(),
        "undefined qualified associated calls must fail closed statically"
    );
}

#[test]
fn track3a2_type_aliases_expand_and_enum_variants_validate() {
    let ok_alias = compile_check_and_run(
        "type UserId = int\n\
         type MyId = UserId\n\
         let id: MyId = 42\n\
         id\n",
    );
    assert_eq!(ok_alias, Value::Int(42));

    let bad_alias = typeck_errors(
        "type UserId = int\n\
         type MyId = UserId\n\
         let id: MyId = \"wrong\"\n\
         id\n",
    )
    .expect("compile");
    assert!(
        !bad_alias.is_empty(),
        "type aliases must expand in HM checking so mismatched values are rejected"
    );

    let ok_enum = typeck_errors("enum Color\n  Red\n  Green\nend\nColor::Red\n").expect("compile");
    assert!(
        ok_enum.is_empty(),
        "valid unit enum variant path should pass typecheck, got {ok_enum:?}"
    );

    let bad_enum =
        typeck_errors("enum Color\n  Red\n  Green\nend\nColor::Blue\n").expect("compile");
    assert!(
        !bad_enum.is_empty(),
        "unknown enum variant `Color::Blue` must be rejected"
    );
}
