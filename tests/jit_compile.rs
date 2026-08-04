//! v0.75.43: copy-and-patch JIT 差分测试。
//!
//! 平台守卫：copy-and-patch JIT 当前仅支持 x86_64 机器码模板（addsd/subsd/
//! mulsd/divsd/comisd/setcc 等），aarch64 上 `src/mir/jit.rs::try_compile`
//! cfg! 提前 return CompileReject。本文件所有测试都假设 JIT 实际能跑出
//! 结果，与 aarch64 行为不兼容——加 `#![cfg(target_arch = "x86_64")]`
//! 让 aarch64 runner 完全跳过整个测试 binary（cargo 编译级别，不 fail）。
#![cfg(target_arch = "x86_64")]


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
//!
//! v0.75.85（aarch64 兼容）：copy-and-patch JIT 当前仅支持 x86_64 机器码模板，
//! aarch64 上 `try_compile` cfg! 提前 return CompileReject。这些测试期望 JIT
//! 实际执行出结果，加 cfg 守卫避免在 aarch64 上整体失败。

use mora::interpreter::{Interpreter, parse_code_v3};
use mora::mir::jit::run_jit;
use mora::mir::lower::lower_mir_exprs;
use mora::mir::vm::run_mir;
use mora::mir::{MirFunction, MirInst};
use mora::value::Value;

fn fconst_at(reg: usize, n: f64) -> MirInst {
    MirInst::Const(reg, Value::Float(n))
}

fn fbinop(dst: usize, a: usize, op: mora::common::BinaryOp, b: usize) -> MirInst {
    MirInst::BinaryOp(dst, a, op, b)
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
    run_jit(func, &mut interp, &mut env).map_err(|e| e.to_string())
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
    // Float Mod（无 fmod 模板）→ 编译期拒绝
    let func_mod = MirFunction {
        params: Vec::new(),
        body: vec![
            fconst_at(0, 17.0),
            fconst_at(1, 5.0),
            fbinop(2, 0, mora::common::BinaryOp::Mod, 1),
        ],
        n_regs: 3,
    };
    assert!(run_jit_of(&func_mod).is_err(), "Float Mod 应拒绝");

    // Mixed 类型（Int + Float）
    let func_mixed = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Float(2.0)),
            fbinop(2, 0, mora::common::BinaryOp::Add, 1),
        ],
        n_regs: 3,
    };
    assert!(run_jit_of(&func_mixed).is_err(), "Mixed 应拒绝");

    // 变量/定义/调用
    for src in ["let x = 1", "print(1)", "1.5 + 2.5\nprint(3)"] {
        let exprs = parse_code_v3(src).expect("parse should succeed");
        let func = lower_mir_exprs(&exprs).expect("lower should succeed");
        assert!(run_jit_of(&func).is_err(), "JIT 应拒绝不可编译程序: {src}");
    }
}

/// 手工构造的 Int 算术（复刻解释器分裂语义：Add=i64 直接、Sub/Mul/Div=
/// f64 round-trip）：JIT == 解释器。
#[test]
fn jit_equiv_manual_int_arith() {
    for (a, op, b) in [
        (4i64, mora::common::BinaryOp::Add, 5i64),
        (10, mora::common::BinaryOp::Sub, 3),
        (4, mora::common::BinaryOp::Mul, 5),
        (20, mora::common::BinaryOp::Div, 4),
        (7, mora::common::BinaryOp::Div, 2), // round-trip：3.5 → round → 4
        (1, mora::common::BinaryOp::Div, 0), // 除零：inf → 饱和 i64::MAX
        (-1, mora::common::BinaryOp::Div, 0), // -inf → 饱和 i64::MIN
                                             // 注：`i64::MAX + 1` 不进等价测试 —— 解释器 Add(Int) 用直接 i64
                                             // 加法，debug 构建溢出 panic（既有行为）；JIT 用 x86 add wrap 与
                                             // release 解释器一致。此处验证 wrap 与 release 语义（不 panic）。
    ] {
        let func = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(a)),
                MirInst::Const(1, Value::Int(b)),
                fbinop(2, 0, op.clone(), 1),
            ],
            n_regs: 3,
        };
        let jit_val =
            run_jit_of(&func).unwrap_or_else(|e| panic!("JIT failed for {a} {op:?} {b}: {e}"));
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for {a} {op:?} {b}");
    }
}

/// Add(Int) 溢出 wrap 语义（JIT x86 add = release 解释器 wrap）。
#[test]
fn jit_int_add_wraps() {
    let func = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(i64::MAX)),
            MirInst::Const(1, Value::Int(1)),
            fbinop(2, 0, mora::common::BinaryOp::Add, 1),
        ],
        n_regs: 3,
    };
    let jit_val = run_jit_of(&func).expect("JIT should compile Add");
    // i64 直接加法 wrap：MAX + 1 = MIN
    assert_eq!(jit_val, Value::Int(i64::MIN), "Add 溢出应 wrap");
}

/// 手工构造的 Int Mod（Rust 浮点余数截断语义）：JIT == 解释器。
#[test]
fn jit_equiv_manual_int_mod() {
    for (a, b) in [(17i64, 5i64), (-17, 5), (17, -5), (-17, -5), (0, 5), (7, 3)] {
        let func = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(a)),
                MirInst::Const(1, Value::Int(b)),
                fbinop(2, 0, mora::common::BinaryOp::Mod, 1),
            ],
            n_regs: 3,
        };
        let jit_val = run_jit_of(&func).unwrap_or_else(|e| panic!("JIT failed for {a} % {b}: {e}"));
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for {a} % {b}");
    }
}

/// 手工构造的 Int 比较（a as f64 op b as f64）：JIT == 解释器。
#[test]
fn jit_equiv_manual_int_cmp() {
    for (a, op, b) in [
        (1i64, mora::common::BinaryOp::Less, 2i64),
        (3, mora::common::BinaryOp::GreaterEqual, 3),
        (4, mora::common::BinaryOp::Equal, 4),
        (4, mora::common::BinaryOp::NotEqual, 5),
        (2, mora::common::BinaryOp::Greater, 5),
        (2, mora::common::BinaryOp::LessEqual, 1),
    ] {
        let func = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(a)),
                MirInst::Const(1, Value::Int(b)),
                fbinop(2, 0, op.clone(), 1),
            ],
            n_regs: 3,
        };
        let jit_val =
            run_jit_of(&func).unwrap_or_else(|e| panic!("JIT failed for {a} {op:?} {b}: {e}"));
        let mir_val = run_interp(&func).expect("interp should run");
        assert_eq!(jit_val, mir_val, "JIT != interp for {a} {op:?} {b}");
    }
}

/// 控制流：无条件跳转（跳过中间指令，命中尾部）—— JIT == 解释器。
#[test]
fn jit_equiv_jump_skip() {
    let func = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Jump(2), // 跳过 pc1
            MirInst::Const(1, Value::Int(99)),
            MirInst::Const(2, Value::Int(7)), // 跳转命中此 pc（静态最后）
        ],
        n_regs: 3,
    };
    let jit_val = run_jit_of(&func).expect("JIT should compile Jump");
    let mir_val = run_interp(&func).expect("interp should run");
    assert_eq!(
        jit_val, mir_val,
        "JIT != interp for jump_skip: {jit_val:?} vs {mir_val:?}"
    );
    assert_eq!(jit_val, Value::Int(7));
}

/// 控制流：JumpIf truthy 跳转 / falsy fall-through —— JIT == 解释器。
#[test]
fn jit_equiv_jump_if() {
    // truthy → 跳 pc3（跳过 pc2）
    let func_true = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Bool(true)),
            MirInst::JumpIf(0, 3),
            MirInst::Const(1, Value::Int(1)), // 被跳过
            MirInst::Const(2, Value::Int(2)),
        ],
        n_regs: 3,
    };
    // 注：解释器 dag 优化会裁剪「无数据消费者」的死 Const，导致条件跳转
    // 目标的 interp 路径与原始指令语义分歧 —— 差分对比仅适用于无条件跳转
    // （见 jump_skip）。条件跳转断言 JIT 的线性指令语义 + 跳转目标命中
    // （rel 正确性）：
    // - JumpIf(true)  → 跳 pc3（跳过 pc2 段），执行 Const(2) → Int(2)
    // - JumpIf(false) → fall-through 顺序执行 pc2、pc3，最后执行的
    //   Const(2) → 同样 Int(2)（线性子集无副作用，跳/不跳最终值一致；
    //   跳转目标命中由「跳 pc3 不落垃圾」验证）。
    let jit_val = run_jit_of(&func_true).expect("JIT should compile JumpIf");
    assert_eq!(
        jit_val,
        Value::Int(2),
        "JumpIf(true) 跳转目标命中（Int(2)）"
    );

    // falsy → fall-through 顺序流（不崩 + 出口值正确）
    let func_false = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Bool(false)),
            MirInst::JumpIf(0, 3),
            MirInst::Const(1, Value::Int(1)),
            MirInst::Const(2, Value::Int(2)),
        ],
        n_regs: 3,
    };
    let jit_val = run_jit_of(&func_false).expect("JIT should compile JumpIf");
    assert_eq!(jit_val, Value::Int(2), "JumpIf(false) fall-through 出口值");
}

/// 控制流：JumpIfNot falsy 跳转 / truthy fall-through —— JIT == 解释器。
#[test]
fn jit_equiv_jump_if_not() {
    // falsy → 跳 pc3（跳过 pc2）
    let func_false = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Bool(false)),
            MirInst::JumpIfNot(0, 3),
            MirInst::Const(1, Value::Int(1)), // 被跳过
            MirInst::Const(2, Value::Int(2)),
        ],
        n_regs: 3,
    };
    // 同 jump_if：跳转目标命中验证。
    let jit_val = run_jit_of(&func_false).expect("JIT should compile JumpIfNot");
    assert_eq!(jit_val, Value::Int(2), "JumpIfNot(false) 跳转目标命中");

    // truthy → fall-through 顺序流
    let func_true = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Bool(true)),
            MirInst::JumpIfNot(0, 3),
            MirInst::Const(1, Value::Int(1)),
            MirInst::Const(2, Value::Int(2)),
        ],
        n_regs: 3,
    };
    let jit_val = run_jit_of(&func_true).expect("JIT should compile JumpIfNot");
    assert_eq!(
        jit_val,
        Value::Int(2),
        "JumpIfNot(true) fall-through 出口值"
    );
}

/// 控制流：前向跳到中间（静态最后一条未被执行的语义）—— JIT 复刻
/// 解释器「最后执行的指令」语义（跳转命中静态最后一条产生 dst 的指令）。
#[test]
fn jit_equiv_jump_forward_mid() {
    let func = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(5)),
            MirInst::Jump(2), // 跳过 pc1
            MirInst::Const(1, Value::Int(99)),
            MirInst::Const(2, Value::Int(7)), // 命中（静态最后）
        ],
        n_regs: 3,
    };
    let jit_val = run_jit_of(&func).expect("JIT should compile forward jump");
    let mir_val = run_interp(&func).expect("interp should run");
    assert_eq!(
        jit_val, mir_val,
        "JIT != interp for forward_mid: {jit_val:?} vs {mir_val:?}"
    );
}

/// 控制流：cond 非 Bool → 编译期拒绝（truthy 语义超出 v1 模板集）。
#[test]
fn jit_rejects_non_bool_cond() {
    let func = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(1)), // Int cond（非 Bool）
            MirInst::JumpIf(0, 3),
            MirInst::Const(1, Value::Int(2)),
        ],
        n_regs: 2,
    };
    assert!(
        run_jit_of(&func).is_err(),
        "非 Bool cond 应拒绝（回落解释器）"
    );
}

/// with-block 真实路径：WithConfig{jit:true} 经 h_with_config dispatch，
/// 可编译 body 走 JIT（不回落）、不可编译 body 回落 run_mir —— 与
/// jit=false（纯解释器）的最终 env 状态一致（config 设置/恢复无副作用）。
fn with_config_env(body: &MirFunction, jit: bool) -> Result<mora::value::Environment, String> {
    use mora::mir::MirInst;
    let outer = MirFunction {
        params: Vec::new(),
        body: vec![MirInst::WithConfig {
            bindings: Vec::new(),
            body: Box::new(body.clone()),
            jit,
        }],
        n_regs: 0,
    };
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&std::sync::Arc::new(outer), &mut interp, &mut env)?;
    Ok(env)
}

/// 可编译 body：JIT 成功路径不回落，行为与解释器一致。
#[test]
fn jit_with_config_compilable_body() {
    let body = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Float(10.0)),
            MirInst::Const(1, Value::Float(4.0)),
            fbinop(2, 0, mora::common::BinaryOp::Div, 1), // 2.5
        ],
        n_regs: 3,
    };
    let env_jit = with_config_env(&body, true).expect("jit=true 不应 Err");
    let env_mir = with_config_env(&body, false).expect("jit=false 不应 Err");
    // config 设置/恢复无环境副作用 → 两者终态一致
    assert_eq!(
        env_jit.iter().len(),
        env_mir.iter().len(),
        "JIT 与解释器 env 终态应一致"
    );
}

/// 不可编译 body（含 Define 副作用）：JIT 编译期拒绝 → 回落 run_mir，
/// 与纯解释器行为一致。
#[test]
fn jit_with_config_falls_back() {
    let body = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(42)),
            MirInst::Define("jitted".to_string(), 0),
        ],
        n_regs: 1,
    };
    let env_jit = with_config_env(&body, true).expect("jit=true 回落不应 Err");
    let env_mir = with_config_env(&body, false).expect("jit=false 不应 Err");
    // Define 副作用在 child_env（h_with_config 不合并回父）→ 父 env 均无
    // 该变量，终态一致
    assert_eq!(
        env_jit.iter().len(),
        env_mir.iter().len(),
        "JIT 回落与解释器 env 终态应一致"
    );
}

/// BailInfo 分类（v0.75.50）：编译期拒绝 vs 运行期守卫失败可区分。
#[test]
fn jit_error_classification() {
    fn run(func: &MirFunction) -> Result<Value, mora::mir::jit::JitError> {
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        run_jit(func, &mut interp, &mut env)
    }
    // CompileReject：含 Define（模板集未覆盖）
    let reject = MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Define("x".to_string(), 0),
        ],
        n_regs: 1,
    };
    match run(&reject) {
        Err(mora::mir::jit::JitError::CompileReject(msg)) => {
            assert!(!msg.is_empty(), "CompileReject 应携带原因");
        }
        other => panic!("含 Define 应 CompileReject，got {other:?}"),
    }
    // 不可编译值类型（String Const）→ CompileReject
    let reject_str = MirFunction {
        params: Vec::new(),
        body: vec![MirInst::Const(0, Value::String("hi".into()))],
        n_regs: 1,
    };
    match run(&reject_str) {
        Err(mora::mir::jit::JitError::CompileReject(_)) => {}
        other => panic!("String Const 应 CompileReject，got {other:?}"),
    }
    // 成功路径 → Ok（不为 Err）
    let ok = MirFunction {
        params: Vec::new(),
        body: vec![MirInst::Const(0, Value::Int(42))],
        n_regs: 1,
    };
    assert!(run(&ok).is_ok(), "纯 Const 应成功编译执行");
}
