//! v0.75.43: copy-and-patch JIT 差分测试。
//!
//! 守卫：JIT（run_jit 原生执行）与 MIR 解释器（run_mir）对同一程序产出
//! **相同的 Value**。两类输入：
//!
//! 1. 手工构造 `MirFunction`（绕过 lower 常量折叠）— 直接测机器码模板
//!    发射（addsd/subsd/mulsd/divsd/comisd/setcc 真实执行）。
//! 2. lower 折叠输入 — 测全链路一致性。
//!
//! 不可编译子集（Int×Int 算术 / Mod / Var / 调用等）→ run_jit 返回 Err
//! （回落解释器，语义由解释器锁定）。

use mora::interpreter::{Interpreter, parse_code_v3};
use mora::mir::interp::run_mir;
use mora::mir::jit::run_jit;
use mora::mir::lower::lower_mir_exprs;
use mora::mir::{MirFunction, MirInst};
use mora::value::Value;

fn fconst_at(reg: usize, n: f64) -> MirInst {
    MirInst::Const(reg, Value::Float(n))
}

fn fbinop(dst: usize, a: usize, op: mora::common::BinaryOp, b: usize) -> MirInst {
    MirInst::BinaryOp(dst, a, op, b)
}

fn lit(i: i64) -> MirInst {
    MirInst::Const(0, Value::Int(i))
}

/// 构造单 BinaryOp 函数体：[r0=a, r1=b, r2=op]。
fn binop_func(a: f64, op: mora::common::BinaryOp, b: f64) -> MirFunction {
    MirFunction {
        params: Vec::new(),
        body: vec![
            fconst_at(0, a),
            fconst_at(1, b),
            fbinop(2, 0, op.clone(), 1),
        ],
        n_regs: 3,
    }
}

fn run_interp(func: &MirFunction) -> Result<Value, String> {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&std::sync::Arc::new(func.clone()), &mut interp, &mut env)
}

fn run_jit_of(func: &MirFunction) -> Result<Value, String> {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_jit(func, &mut interp, &mut env)
}

/// 手工构造的 Float 算术：JIT == 解释器。
#[test]
fn jit_equiv_manual_float_arith() {
    for (a, op, b) in [
        (4.0, mora::common::BinaryOp::Add, 5.0),
        (10.0, mora::common::BinaryOp::Sub, 3.0),
        (4.0, mora::common::BinaryOp::Mul, 5.0),
        (20.0, mora::common::BinaryOp::Div, 4.0),
        (1.0, mora::common::BinaryOp::Div, 0.0), // IEEE inf（同解释器）
    ] {
        let func = binop_func(a, op.clone(), b);
        let jit_val =
            run_jit_of(&func).unwrap_or_else(|e| panic!("JIT failed for {a} {op:?} {b}: {e}"));
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for {a} {op:?} {b}");
    }
}

/// 手工构造的 Float 比较：JIT == 解释器。
#[test]
fn jit_equiv_manual_float_cmp() {
    for (a, op, b) in [
        (1.0, mora::common::BinaryOp::Less, 2.0),
        (3.0, mora::common::BinaryOp::GreaterEqual, 3.0),
        (4.0, mora::common::BinaryOp::Equal, 4.0),
        (4.0, mora::common::BinaryOp::NotEqual, 5.0),
        (2.0, mora::common::BinaryOp::Greater, 5.0),
        (2.0, mora::common::BinaryOp::LessEqual, 1.0),
    ] {
        let func = binop_func(a, op.clone(), b);
        let jit_val =
            run_jit_of(&func).unwrap_or_else(|e| panic!("JIT failed for {a} {op:?} {b}: {e}"));
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for {a} {op:?} {b}");
    }
}

/// NaN 比较：无序 → 所有有序比较 false（comisd + jp 修正路径）。
#[test]
fn jit_equiv_nan_comparison() {
    let nan = f64::NAN;
    for op in [
        mora::common::BinaryOp::Equal,
        mora::common::BinaryOp::NotEqual,
        mora::common::BinaryOp::Less,
        mora::common::BinaryOp::Greater,
        mora::common::BinaryOp::LessEqual,
        mora::common::BinaryOp::GreaterEqual,
    ] {
        let func = binop_func(nan, op.clone(), 1.0);
        let jit_val = run_jit_of(&func).expect("NaN cmp should compile");
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for NaN {op:?}");
    }
}

/// lower 折叠输入（全 Const）— 全链路一致性。
#[test]
fn jit_equiv_folded_constants() {
    for src in [
        "1 + 2",
        "10 - 3",
        "4 * 5",
        "20 / 4",
        "17 % 5",
        "1 < 2",
        "3 >= 3",
        "1 + 2 * 3",
    ] {
        let exprs = parse_code_v3(src).expect("parse should succeed");
        let func = lower_mir_exprs(&exprs).expect("lower should succeed");
        let jit_val = run_jit_of(&func).expect("folded consts should compile");
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for {src}");
    }
}

/// 不可编译子集 → run_jit 必须 Err（回落解释器）。
#[test]
fn jit_rejects_uncompilable() {
    // Int×Int 算术（i64 语义超出 v1）
    let func = MirFunction {
        params: Vec::new(),
        body: vec![
            lit(0),
            MirInst::Const(1, Value::Int(2)),
            fbinop(2, 0, mora::common::BinaryOp::Add, 1),
        ],
        n_regs: 3,
    };
    assert!(run_jit_of(&func).is_err(), "Int×Int 应拒绝");

    // Mod（无 SSE2 fmod）
    let func_mod = MirFunction {
        params: Vec::new(),
        body: vec![
            fconst_at(0, 17.0),
            fconst_at(1, 5.0),
            fbinop(2, 0, mora::common::BinaryOp::Mod, 1),
        ],
        n_regs: 3,
    };
    assert!(run_jit_of(&func_mod).is_err(), "Mod 应拒绝");

    // 变量/定义/调用
    for src in ["let x = 1", "print(1)", "1.5 + 2.5\nprint(3)"] {
        let exprs = parse_code_v3(src).expect("parse should succeed");
        let func = lower_mir_exprs(&exprs).expect("lower should succeed");
        assert!(run_jit_of(&func).is_err(), "JIT 应拒绝不可编译程序: {src}");
    }
}
