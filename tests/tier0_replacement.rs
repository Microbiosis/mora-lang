//! Tier 0 → Tier 1 / W2.3 替换集成测试
//!
//! 直接调用 `mora::runtime::vm::{execute_program, run_main_task}`，
//! 验证语法/语义/类型/标准库/运行时都通过 Control SSA VM 完整执行。

use std::sync::Arc;

use mora::mir::control::ControlProgram;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{ModuleGlobals, execute_program, run_main_task};
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

/// 公共执行入口：compile → typeck → execute_program → run_main_task
fn run_via_execute(source: &str) -> Result<(), String> {
    let (prog, witnesses) = ParserV3::compile(source).map_err(|e| e.to_string())?;
    let type_errs = check_program_witnesses(&witnesses);
    if !type_errs.is_empty() {
        return Err(format!(
            "typeck: {} error(s); first = {}",
            type_errs.len(),
            type_errs[0].message
        ));
    }
    let mut core = CoreRuntime::default();
    let prog_arc = Arc::new(prog);
    let (_, globals) = execute_program(prog_arc.clone(), &mut core).map_err(|e| e.to_string())?;
    let _ = run_main_task(prog_arc, globals, &mut core).map_err(|e| e.to_string())?;
    Ok(())
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
    run_via_execute(src).expect("syntax path must execute via MIR");
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
  print("sum=" + str(total))
end
"#;
    run_via_execute(src).expect("control flow must execute via MIR");
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
    run_via_execute(src).expect("typeck + MIR path must succeed");
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
    run_via_execute(src).expect("stdlib builtin dispatch must execute via MIR");
}

// ─── 5. 运行时 (runtime) ───────────────────────────────────────────
// Transaction 走 MirInst::Transaction——成功路径合并 child_env，失败路径
// 触发 compensation 后返回 "Transaction rolled back"。
//
#[test]
fn runtime_transaction_success_path_via_mir() {
    let src = "let x = 0i\ntransaction\n  x = 2i\nend\nx\n";
    let (prog, _) = ParserV3::compile(src).expect("compile transaction success");
    let mut core = CoreRuntime::default();
    let (v, _) = execute_program(Arc::new(prog), &mut core).expect("transaction success path");
    assert_eq!(v, Value::Int(2), "成功路径应保留 body 对 x 的修改");
}

#[test]
fn runtime_transaction_rollback_path_via_mir() {
    let src = "let x = 0i\ntransaction\n  x = 1i\n  rollback\ncompensation\n  x = 99i\nend\n";
    let (prog, _) = ParserV3::compile(src).expect("compile transaction rollback");
    let mut core = CoreRuntime::default();
    let result = execute_program(Arc::new(prog), &mut core);
    assert!(result.is_err(), "rollback must surface as Err");
    assert!(
        result
            .as_ref()
            .expect_err("rollback must surface as Err")
            .message()
            .contains("rolled back"),
        "expected rollback error, got: {:?}",
        result
    );
}

#[allow(clippy::type_complexity)]
#[test]
fn tier1_public_api_is_stable() {
    let _fns_exist: (
        fn(
            Arc<ControlProgram>,
            &mut CoreRuntime,
        ) -> mora::error::MoraResult<(Value, ModuleGlobals)>,
        fn(Arc<ControlProgram>, ModuleGlobals, &mut CoreRuntime) -> mora::error::MoraResult<Value>,
    ) = (execute_program, run_main_task);
}

// ─── 6. per-key CRDT 合并策略（v0.75.23）────────────────────────────
// merge_with(key, strategy) 写 current_merge_strategies；parallel worker 的
// merge 阶段按 key 查找合并策略（无策略 fallback LWW）。
// GrowOnlySet 下两个 worker 写同一 key 的 List → 并集去重（LWW 会覆盖）。

fn extract_float_list(v: Value) -> Vec<f64> {
    match v {
        Value::List(l) => l
            .iter()
            .map(|item| match item {
                Value::Float(f) => *f,
                other => panic!("expected float, got {:?}", other),
            })
            .collect(),
        other => panic!("expected List, got {:?}", other),
    }
}

#[test]
fn merge_with_grow_only_set_merges_worker_outputs() {
    let src = "\
merge_with(\"x\", \"grow_only_set\")
let x = []
parallel
  worker w1 do
    x = [1.0, 2.0]
  end
  worker w2 do
    x = [2.0, 3.0]
  end
end
x
";
    let (prog, _) = ParserV3::compile(src).expect("compile merge_with grow_only_set");
    let mut core = CoreRuntime::default();
    let (res, _) = execute_program(Arc::new(prog), &mut core).expect("worker merge run");
    assert_eq!(
        extract_float_list(res),
        vec![1.0, 2.0, 3.0],
        "GrowOnlySet 应并集去重（LWW 下会是 [2,3]）"
    );
}

#[test]
fn merge_with_lww_default_overwrites() {
    // 无策略时 fallback LWW：后写覆盖（对比 G-Set 语义）。
    let src = "\
let x = []
parallel
  worker w1 do
    x = [1.0, 2.0]
  end
  worker w2 do
    x = [2.0, 3.0]
  end
end
x
";
    let (prog, _) = ParserV3::compile(src).expect("compile worker lww");
    let mut core = CoreRuntime::default();
    let (res, _) = execute_program(Arc::new(prog), &mut core).expect("worker lww run");
    assert_eq!(
        extract_float_list(res),
        vec![2.0, 3.0],
        "默认 LWW 应后写覆盖"
    );
}
