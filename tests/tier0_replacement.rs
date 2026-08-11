//! Tier 0 → Tier 1 替换集成测试
//!
//! 这套测试直接调用 `mora::mir::vm::run_mir` + `run_main_task`，
//! 跳过 `Interpreter::interpret/execute/evaluate/call_value_inner/call_task_inner`，
//! 以证明所有 5 个语言面（语法/语义/类型/标准库/运行时）都通过 MIR 解释器执行。
//!
//! Tier 0 AST 执行器已移除（v0.55 ParserV3 替换 AST 解释器）；本文件直接走
//! MIR 路径，不依赖任何 AST 活跃调用方。

use mora::interpreter::Interpreter;
use mora::mir::lower::{lower_mir_exprs, typecheck_mir_exprs};
use mora::mir::vm::{run_main_task, run_mir};
use mora::mir::{MirFunction, MirInst};
use mora::value::Value;

/// 公共执行入口：parse → typeck → lower → run_mir → run_main_task
/// 这是 `src/main.rs::run_file()` 的纯库版本，可被测试独立调用。
fn run_via_mir(source: &str) -> Result<(), String> {
    let mut exprs = mora::interpreter::parse_code_v3(source)?;
    let type_errs = typecheck_mir_exprs(&mut exprs);
    if !type_errs.is_empty() {
        return Err(format!(
            "typeck: {} error(s); first = {}",
            type_errs.len(),
            type_errs[0].message
        ));
    }
    let func: MirFunction = lower_mir_exprs(&exprs)?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存（run_mir + run_main_task 共享同一项）
    let func_arc = std::sync::Arc::new(func);
    run_mir(&func_arc, &mut interp, &mut env)?;
    run_main_task(&func_arc, &mut interp, &mut env)
}

// ─── 1. 语法 (syntax) ───────────────────────────────────────────────
// Let / Define / Call / task main 全套必须经 MIR 而非 AST execute 落地。
#[test]
fn syntax_let_print_task_main_runs_via_mir() {
    let src = r#"
task main()
  let greeting = "Hello, Tier 1!"
  print(greeting)
end
"#;
    run_via_mir(src).expect("syntax path must execute via MIR");
}

// ─── 2. 语义 (semantics) ───────────────────────────────────────────
// for / if / return / match 走 MIR lowering → MirInst::For/If/Return/MatchExpr。
// 注：v0.55 语法 `if cond then body end`；使用 len/range builtin 避免 sandbox。
// 注：单 task 内联验证控制流语义——跨 task 调用依赖 task registry，
// 而当前 `run_main_task` 只扫描自身 func body 的 TaskDef（Tier1 简化），
// 完整跨 task 调用在 Tier 2 阶段补齐。
#[test]
fn semantics_control_flow_runs_via_mir() {
    // v0.75.11: `if ... then` 要求 then 与分支同行（表达式语法）；
    // 块式 if 用 brace 形态 `if cond { ... }`。
    let src = r#"
task main()
  let total = 0
  for i in range(0, 10, 1)
    let total = total + i
  end
  if total > 0 { print("positive") }
  print("sum=" + total)
end
"#;
    run_via_mir(src).expect("control flow must execute via MIR");
}

// ─── 3. 类型系统 (type system) ──────────────────────────────────────
// mir::lower::typecheck_mir_exprs 必须在 MIR 路径之前通过，验证解耦。
// type alias + enum 经 MIR 落到 env（α.3）。
// 注：脚本内多个 task 跨 task 调用时，MIR task 注册表只覆盖同一 func body；
// 单 task 内联 if/return 同样能验证类型→MIR 链。
#[test]
fn typeck_passes_then_mir_runs() {
    let src = r#"
type Bytes = number

task main()
  let n = 5
  let label = "zero"
  if n > 0 { let label = "positive" }
  if n < 0 { let label = "negative" }
  print(label)
end
"#;
    run_via_mir(src).expect("typeck + MIR path must succeed");
}

// ─── 4. 标准库 / API (stdlib) ──────────────────────────────────────
// `len(list)` 与 `range(...)` 是 AST builtin 层派发的代表：
// MIR `MirInst::Call("len", ...)` → `mir_call_function("len")` → AST `call_function("len")`。
// 它们证明 MIR 路径不解释 List/Dict/builtin，而只调度 builtin 名字。
#[test]
fn stdlib_builtin_dispatch_via_mir() {
    let src = r#"
task main()
  let xs = [1, 2, 3, 4, 5]
  let n = len(xs)
  let nums = range(0, n, 1)
  print(n)
  print(nums)
end
"#;
    run_via_mir(src).expect("stdlib builtin dispatch must execute via MIR");
}

// ─── 5. 运行时 (runtime) ───────────────────────────────────────────
// Transaction 走 MirInst::Transaction——成功路径合并 child_env，失败路径
// 触发 compensation 后返回 "Transaction rolled back"。
//
// v0.75.11: transaction 语法无前端（lexer 有 token，parser 无解析，
// MirExprKind 无变体）— 与 MirInst::Transaction 无前端可达的现状一致。
// 测试改为直接构造 MirInst（不经 parser），验证 handler 语义本体。
#[test]
fn runtime_transaction_success_path_via_mir() {
    // 成功路径：body 正常执行（Const + Define），compensation 不触发。
    let body = mora::mir::MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, mora::value::Value::Int(2)),
            MirInst::Define("x".to_string(), 0),
        ],
        n_regs: 1,
    
            ..Default::default()};
    let compensation = mora::mir::MirFunction {
        params: Vec::new(),
        body: vec![MirInst::Const(0, mora::value::Value::Int(0))],
        n_regs: 1,
    
            ..Default::default()};
    let func = MirFunction {
        params: Vec::new(),
        body: vec![MirInst::Transaction {
            body: Box::new(body),
            compensation: Box::new(compensation),
        }],
        n_regs: 1,
        ..Default::default()
    };
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    run_mir(&func_arc, &mut interp, &mut env).expect("transaction success path via MIR");
    assert_eq!(
        env.get("x"),
        Some(mora::value::Value::Int(2)),
        "成功路径应把 body 的 Define 合并回 env"
    );
}

#[test]
fn runtime_transaction_rollback_path_via_mir() {
    // 回滚路径：body 内 MirInst::Rollback → dispatch 返回 Err →
    // h_transaction 执行 compensation 并返回 "Transaction rolled back"。
    let body = mora::mir::MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Const(0, mora::value::Value::Int(1)),
            MirInst::Rollback,
        ],
        n_regs: 1,
    
            ..Default::default()};
    let compensation = mora::mir::MirFunction {
        params: Vec::new(),
        body: vec![MirInst::Const(0, mora::value::Value::Int(99))],
        n_regs: 1,
    
            ..Default::default()};
    let func = MirFunction {
        params: Vec::new(),
        body: vec![MirInst::Transaction {
            body: Box::new(body),
            compensation: Box::new(compensation),
        }],
        n_regs: 1,
        ..Default::default()
    };
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    let result = run_mir(&func_arc, &mut interp, &mut env);
    assert!(result.is_err(), "rollback must surface as Err");
    assert!(
        result.as_ref().err().unwrap().contains("rolled back"),
        "expected rollback error, got: {:?}",
        result
    );
}

// ─── 6. 守门：mir_call_function / run_main_task 入口签名稳定 ─────────
// 防止后续重构意外删除 Tier 1 公共 API。
// v0.75.x: 宿主参数从 `&mut Interpreter` 变为 `&mut dyn MirHost`（解耦
// mir ↔ interpreter 双向依赖的契约变更），此处同步更新。
// v0.75.9: 函数参数从 `&MirFunction` 变为 `&Arc<MirFunction>`（走全局
// DAG 缓存，key = Arc 指针）。
#[allow(clippy::type_complexity)]
#[test]
fn tier1_public_api_is_stable() {
    // 这些函数必须在 `mora::mir::interp` 中存在并接受这些参数；
    // 若签名漂移，编译期就会失败——这就是稳定的契约。
    let _fns_exist: (
        fn(
            &std::sync::Arc<MirFunction>,
            &mut dyn mora::mir::host::MirHost,
            &mut mora::value::Environment,
        ) -> Result<Value, String>,
        fn(
            &std::sync::Arc<MirFunction>,
            &mut dyn mora::mir::host::MirHost,
            &mut mora::value::Environment,
        ) -> Result<(), String>,
    ) = (run_mir, run_main_task);
}

// ─── 6. per-key CRDT 合并策略（v0.75.23）────────────────────────────
// merge_with(key, strategy) 写 current_merge_strategies；h_worker 的
// run_isolated 读它做 per-key 合并（无策略 fallback LWW）。
// GrowOnlySet 下两个 worker 写同一 key 的 List → 并集去重（LWW 会覆盖）。

fn mk_list_worker(items: Vec<f64>) -> MirFunction {
    let mut body = Vec::new();
    let mut r = 0;
    let list_reg = r;
    r += 1;
    let mut item_regs = Vec::new();
    for v in items {
        body.push(MirInst::Const(r, Value::Float(v)));
        item_regs.push(r);
        r += 1;
    }
    body.push(MirInst::ListLit(list_reg, item_regs));
    body.push(MirInst::Define("x".to_string(), list_reg));
    body.push(MirInst::Halt(None));
    MirFunction {
        params: Vec::new(),
        body,
        n_regs: r,
    
            ..Default::default()}
}

fn wrap_worker(name: &str, body: MirFunction) -> MirFunction {
    MirFunction {
        params: Vec::new(),
        body: vec![
            MirInst::Worker {
                name: name.to_string(),
                body: Box::new(body),
            },
            MirInst::Halt(None),
        ],
        n_regs: 8,
        ..Default::default()
    }
}

#[test]
fn merge_with_grow_only_set_merges_worker_outputs() {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    // merge_with("x", "grow_only_set") 的等价语义（写侧内置名单测见
    // interpreter::dispatch::tests）
    interp.set_merge_strategies(Some(std::collections::HashMap::from([(
        "x".to_string(),
        mora::value::MergeStrategy::GrowOnlySet,
    )])));

    let w1 = std::sync::Arc::new(wrap_worker("w1", mk_list_worker(vec![1.0, 2.0])));
    run_mir(&w1, &mut interp, &mut env).expect("worker1 run");
    let w2 = std::sync::Arc::new(wrap_worker("w2", mk_list_worker(vec![2.0, 3.0])));
    run_mir(&w2, &mut interp, &mut env).expect("worker2 run");

    let vals: Vec<f64> = match env.get("x") {
        Some(Value::List(l)) => l
            .iter()
            .map(|v| match v {
                Value::Float(f) => *f,
                other => panic!("expected float, got {:?}", other),
            })
            .collect(),
        other => panic!("expected List, got {:?}", other),
    };
    assert_eq!(
        vals,
        vec![1.0, 2.0, 3.0],
        "GrowOnlySet 应并集去重（LWW 下会是 [2,3]）"
    );
}

#[test]
fn merge_with_lww_default_overwrites() {
    // 无策略时 run_isolated fallback LWW：后写覆盖（对比 G-Set 语义）。
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let w1 = std::sync::Arc::new(wrap_worker("w1", mk_list_worker(vec![1.0, 2.0])));
    run_mir(&w1, &mut interp, &mut env).expect("worker1 run");
    let w2 = std::sync::Arc::new(wrap_worker("w2", mk_list_worker(vec![2.0, 3.0])));
    run_mir(&w2, &mut interp, &mut env).expect("worker2 run");
    let vals: Vec<f64> = match env.get("x") {
        Some(Value::List(l)) => l
            .iter()
            .map(|v| match v {
                Value::Float(f) => *f,
                _ => panic!("float"),
            })
            .collect(),
        _ => panic!("list"),
    };
    assert_eq!(vals, vec![2.0, 3.0], "默认 LWW 应后写覆盖");
}
