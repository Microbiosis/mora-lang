//! v0.99: ambient random 集成测试 —— random 从进程级全局状态机迁移为
//! algebraic effects 后的语言级行为验证。
//!
//! 覆盖四条验收线：
//! 1. 确定性：`random.seed(n)` 后序列可复现（运行时语义不回归）；
//! 2. 类型层：`random.*` 调用携带 ambient 效果行、签名校验生效、
//!    根边界放行 ambient 残差；
//! 3. 可覆写：用户 `handle random_* { ... }` 按动态作用域截获；
//! 4. 并发自然属性：实例/线程间序列按值隔离，无共享无锁。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;
use mora::value::Value;

/// 编译 + typeck + 运行，返回最后表达式值。任何失败 panic。
fn compile_and_run(src: &str) -> Value {
    let (func, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed: {}", e));
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(
        errs.is_empty(),
        "type errors: {:?}",
        errs.iter().map(|e| format!("{:?}", e)).collect::<Vec<_>>()
    );
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = mora::mir::effect::Effects::default();
    run_mir(
        &std::sync::Arc::new(func),
        &mut interp,
        &mut env,
        &mut effects,
    )
    .unwrap_or_else(|e| panic!("run failed: {}", e))
}

/// 编译 + typeck，返回 typeck 错误（用于断言拒绝）。
fn type_errors(src: &str) -> Vec<String> {
    let (_func, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed: {}", e));
    check_program_witnesses_bidirectional(&witnesses)
        .into_iter()
        .map(|e| format!("{:?}", e))
        .collect()
}

// ── 1. 确定性（运行时语义与 v0.91 一致）──

#[test]
fn seeded_sequence_is_deterministic() {
    let src = r#"
task main()
  random.seed(42)
  random.random()
end
"#;
    let a = compile_and_run(src);
    let b = compile_and_run(src);
    assert_eq!(format!("{}", a), format!("{}", b), "same seed → same value");
}

#[test]
fn all_six_methods_run() {
    // 全方法面冒烟：六个操作都经 ambient 分发正常返回。
    // （print 逐项输出 —— 不用混合类型列表字面量，列表字面量有同质约束。）
    let v = compile_and_run(
        r#"
task main()
  random.seed(7)
  print(random.random())
  print(random.rand_int(1, 10))
  print(random.rand_float(0.0, 1.0))
  print(random.rand_choice(["x", "y", "z"]))
  print(random.shuffle([1, 2, 3]))
end
"#,
    );
    assert!(
        matches!(v, Value::Nil),
        "sequence ends with print, got {:?}",
        v
    );
}

// ── 2. 类型层（效果行 + 签名 + 边界）──

#[test]
fn random_call_in_function_propagates_and_is_exempt_at_root() {
    // 使用 random 的函数：效果行携带 ambient 标签（v0.96/0.97 传播机制），
    // 调用上浮到根边界后因 ambient 兜底放行 —— 编译零错误。
    let src = r#"
let noisy = fn() random.random() end
task main()
  noisy()
end
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "ambient residual should be exempt: {:?}",
        errs
    );
}

#[test]
fn rand_int_arity_is_checked() {
    let src = r#"
task main()
  random.rand_int(1)
end
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("Expected 2 arguments")),
        "expected arity error, got: {:?}",
        errs
    );
}

#[test]
fn unknown_random_method_is_rejected() {
    let src = r#"
task main()
  random.nonsense(1)
end
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("random.nonsense")),
        "expected unknown-method error, got: {:?}",
        errs
    );
}

// ── 3. 可覆写（用户 handle 截获 ambient perform）──

#[test]
fn handle_overrides_random_ops() {
    // 用户 handler 按标签截获：body 里 random.random() 的 perform
    // 路由到 handler（返回 0.5），ambient 状态不被触碰。
    // 用法对齐既有 handle_effect.mora：handle 块的值经赋值捕获。
    let v = compile_and_run(
        r#"
let x = 0.0
handle random_random {
  x = random.random()
} {
  0.5
}
x
"#,
    );
    assert!(matches!(v, Value::Float(f) if f == 0.5), "got {:?}", v);
}

// ── 4. 非 ambient unhandled 仍被拒绝（回归防线）──

#[test]
fn non_ambient_unhandled_effect_still_rejected() {
    let src = r#"
effect my_effect(): int
perform my_effect()
"#;
    let errs = type_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("my_effect")),
        "non-ambient unhandled effect must be rejected, got: {:?}",
        errs
    );
}

// ── 5. 并发自然属性（值隔离，无共享无锁）──

#[test]
fn interpreters_are_isolated_across_threads() {
    // 两个线程各自的 Interpreter：同种子 → 同序列，互不干扰。
    // v0.91 全局 Mutex 语义下两条线程共享同一序列，本断言在交错下
    // 不稳定；v0.99 状态按值隔离后成为稳定性质。
    let h1 = std::thread::spawn(|| {
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let mut effects = mora::mir::effect::Effects::default();
        let (func, _) = ParserV3::compile(
            "task main()\n  random.seed(123)\n  [random.random(), random.random()]\nend",
        )
        .unwrap();
        run_mir(
            &std::sync::Arc::new(func),
            &mut interp,
            &mut env,
            &mut effects,
        )
        .unwrap()
    });
    let h2 = std::thread::spawn(|| {
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let mut effects = mora::mir::effect::Effects::default();
        let (func, _) = ParserV3::compile(
            "task main()\n  random.seed(456)\n  [random.random(), random.random()]\nend",
        )
        .unwrap();
        run_mir(
            &std::sync::Arc::new(func),
            &mut interp,
            &mut env,
            &mut effects,
        )
        .unwrap()
    });
    let (a, _b) = (h1.join().unwrap(), h2.join().unwrap());
    // a 的序列 == 单线程同种子序列（未被其他线程推进）
    let reference = compile_and_run(
        "task main()\n  random.seed(123)\n  [random.random(), random.random()]\nend",
    );
    assert_eq!(format!("{}", a), format!("{}", reference));
}
