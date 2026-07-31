//! Tier 0 → Tier 1 替换集成测试
//!
//! 这套测试直接调用 `mora::mir::interp::run_mir` + `run_main_task`，
//! 跳过 `Interpreter::interpret/execute/evaluate/call_value_inner/call_task_inner`，
//! 以证明所有 5 个语言面（语法/语义/类型/标准库/运行时）都通过 MIR 解释器执行。
//!
//! 配套的 AST 行为基准保留在 `tests/mir_differential.rs` —— 该文件作为
//! 回归保护继续引用 AST 解释器，是唯一允许保留的 Tier 0 活跃调用方。

use mora::interpreter::Interpreter;
use mora::mir::MirFunction;
use mora::mir::interp::{run_main_task, run_mir};
use mora::mir::lower::{lower_mir_exprs, typecheck_mir_exprs};
use mora::value::{FlowSignal, Value};

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
    let src = r#"
task main()
  let total = 0
  for i in range(0, 10, 1)
    let total = total + i
  end
  if total > 0 then
    print("positive")
  end
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
  if n > 0 then
    let label = "positive"
  end
  if n < 0 then
    let label = "negative"
  end
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
#[test]
fn runtime_transaction_success_path_via_mir() {
    let src = r#"
task main()
  transaction
    let x = 1 + 1
    print("ok=" + x)
  compensation
    print("never")
  end
end
"#;
    run_via_mir(src).expect("transaction success path via MIR");
}

#[test]
fn runtime_transaction_rollback_path_via_mir() {
    let src = r#"
task main()
  transaction
    print("body start")
    rollback
  compensation
    print("rolled back")
  end
end
"#;
    let result = run_via_mir(src);
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

// 抑制 FlowSignal 未使用警告（differential / 单元测试未来需要）
#[allow(dead_code)]
const _FLOW_SIGNAL_PRESENT: FlowSignal = FlowSignal::None;
