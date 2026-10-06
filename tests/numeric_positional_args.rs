//! v0.104.6 D150：可选**数值**实参被静默丢弃（已修）。
//!
//! ## 缺陷：`if let Some(Value::Float(n)) = args.get(i)` 只认一种数字表示
//!
//! Mora 的数字**字面量**是 `Float`（D98），`Value::Int` 只由 `len()` 等产生。
//! 于是这类写法把「不是 Float」统统当成「没传」：
//!
//! ```mora
//! schedule.add("j", "every", "tick", len([1, 2, 3]))   // interval_s 变 0
//! ccr.marker("abcdef", len([…8 项]))                    // size 变 0
//! mora.refine(p, "i", len([1, 1]))                     // 返回类型 List → Dict
//! mora.refine_info(p, len([1]))                        // 查第 1 轮 → 返回最新轮
//! tea.run(app, 3)                                      // 上限变 1000（反向极性！）
//! ```
//!
//! 全部 **exit 0、零诊断**。
//!
//! ## `tea.run` 是**反向**的同一个缺陷
//!
//! 它只认 `Value::Int`，于是**最自然**的 `tea.run(app, 3)` 反而失效 ——
//! 实测（队列经 `Cmd::Dispatch` 每轮回流、永不空，故轮数 = count）：
//!
//! | 调用 | 修复前 | 修复后 |
//! |---|---|---|
//! | `tea.run(a, 3)` | count **1000** | count 3 |
//! | `tea.run(a, 10)` | count **1000** | count 10 |
//! | `tea.run(a, len([1,1,1]))` | count 3 | count 3 |
//!
//! ## 同族里早就有人做对了
//!
//! `ai.retry` 的 `backoff_ms` **Int/Float 都认**（`src/interpreter/builtins/ai.rs`）。
//! 即「同族里有人做了 → 破例的是疏漏」—— 已把那条正确做法收成
//! `builtins::optional_num_arg` 一处。
//!
//! ## 记档（**未改**）：负数不在本轮收紧
//!
//! 6 处的负数语义各不相同（`as usize` 饱和到 0 / `as u64` 饱和到 0），
//! 且 `schedule.add` 的负数会撞上下游本就诚实的
//! `Every kind needs interval_s > 0` 报错 —— 属**另一类**问题，按
//! 「收紧一类错误，别顺手改另一类」记档待决。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v}"))
}

/// D150 主判据 ①：`schedule.add` 的两个**数值**实参必须认 `Int`。
///
/// 修复前：用户**明明传了合法值**，却被报成
/// `Every kind needs interval_s > 0`（因为值被静默丢弃成了 0）。
#[test]
fn d150_schedule_add_accepts_int_valued_interval() {
    let out = run("let n = len([1, 2, 3])\n\
         let id = schedule.add(\"j\", \"every\", \"tick\", n)\n\
         let jobs = schedule.list()\n\
         id\n")
    .unwrap_or_else(|e| panic!("合法的 Int 间隔不应报错: {e}"));
    // `schedule.list()` 把 interval_s 回显出来 —— 直接读盘确认落盘值
    let listed = run("let n = len([1, 2, 3])\n\
         schedule.add(\"j\", \"every\", \"tick\", n)\n\
         let jobs = schedule.list()\n\
         jobs[0]\n")
    .unwrap_or_else(|e| panic!("列出任务不应报错: {e}"));
    assert!(
        listed.contains("interval_s: 3"),
        "interval_s 应为 3（修复前静默变 0 → 撞出 `Every kind needs interval_s > 0`）; 实得: {listed}"
    );
    assert!(!out.is_empty());
}

/// D150 主判据 ②：`ccr.marker` 的 size 必须认 `Int`（修复前静默变 0）。
#[test]
fn d150_ccr_marker_accepts_int_valued_size() {
    let out = run("ccr.marker(\"abcdef\", len([1, 2, 3, 4, 5, 6, 7, 8]))\n")
        .unwrap_or_else(|e| panic!("合法的 Int size 不应报错: {e}"));
    assert!(
        out.contains("8"),
        "size 应为 8（修复前静默变 0）; 实得: {out}"
    );
}

/// D150 主判据 ③：`mora.refine` 的 count 决定**返回类型**（D150 里后果最重的一处）。
///
/// 修复前传 `Int` → count 退回 1 → 返回单个 `Dict` 而非 `List[Dict]`。
#[test]
fn d150_refine_count_accepts_int_and_keeps_list_return_type() {
    // 需要一个真实脚本文件作为第 1 参（它是**路径**不是脚本文本）
    let dir = std::env::temp_dir().join("mora_d150_refine");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let script = dir.join("s.mora");
    std::fs::write(&script, "task main()\n  print(\"hi\")\nend\n").expect("写脚本");

    let src = format!(
        "let a = mora.refine(\"{p}\", \"add a comment\", len([1, 1]))\n\
         type_of(a)\n",
        p = script.display().to_string().replace('\\', "\\\\")
    );
    let out = run(&src).unwrap_or_else(|e| panic!("合法的 Int count 不应报错: {e}"));
    assert!(
        out.contains("list"),
        "count=2 时返回类型必须是 list（修复前 Int → count 退回 1 → 静默变成 dict）; 实得: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D150 主判据 ④：`mora.refine_info` 的 iteration 必须认 `Int`。
///
/// 修复前传 `Int` → `iter` 变 `None` → 走 `latest_step()`，
/// 即「查第 1 轮」静默返回**最新一轮**。
#[test]
fn d150_refine_info_accepts_int_valued_iteration() {
    let dir = std::env::temp_dir().join("mora_d150_info");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let script = dir.join("s.mora");
    std::fs::write(&script, "task main()\n  print(\"hi\")\nend\n").expect("写脚本");
    let p = script.display().to_string().replace('\\', "\\\\");

    // 两轮 refine，然后查第 1 轮：Float 与 Int 两条路必须给出**同一个** iteration
    let src = format!(
        "mora.refine(\"{p}\", \"first pass\", 1.0)\n\
         mora.refine(\"{p}\", \"second pass\", 1.0)\n\
         let byFloat = mora.refine_info(\"{p}\", 1.0)\n\
         let byInt = mora.refine_info(\"{p}\", len([1]))\n\
         byFloat\n"
    );
    let by_float = run(&src).unwrap_or_else(|e| panic!("Float 路径不应报错: {e}"));
    let src_int = format!(
        "mora.refine(\"{p}\", \"first pass\", 1.0)\n\
         mora.refine(\"{p}\", \"second pass\", 1.0)\n\
         let byInt = mora.refine_info(\"{p}\", len([1]))\n\
         byInt\n"
    );
    let by_int = run(&src_int).unwrap_or_else(|e| panic!("Int 路径不应报错: {e}"));
    assert_eq!(
        by_int, by_float,
        "查 iteration 1 时 Int 与 Float 必须等价（修复前 Int 静默走 latest_step → 返回第 2 轮）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D150 主判据 ⑤：`tea.run` 的上限必须认 **Float 字面量**（反向极性那处）。
///
/// 队列经 `Cmd::Dispatch` 每轮回流、永不空，故 `count` == 实际轮数。
#[test]
fn d150_tea_run_honours_float_literal_step_limit() {
    let src = r#"
model Counter
  count: number = 0
  step: number = 1
end

msg CounterMsg
  Increment
end

update(msg, model)
  match msg {
    Increment => [{count: model.count + 1i, step: model.step}, json.parse("{\"kind\": \"Dispatch\", \"msg\": {\"tag\": \"Increment\"}}")]
  }
end

app Application
  model: Counter
  msg: CounterMsg
  init: {count: 0, step: 1}
  update: update
  view: fn(model) => model
end

let a = tea.dispatch(Application, {tag: "Increment"})
tea.model(tea.run(a, 3))
"#;
    let out = run(src).unwrap_or_else(|e| panic!("tea.run 不应报错: {e}"));
    assert!(
        out.contains("count: 3"),
        "`tea.run(a, 3)` 必须只跑 3 轮（修复前只认 Int → 用默认 1000 轮）; 实得: {out}"
    );
}

/// 对照组 ①：各 builtin 的**默认**与 Float 路径不得改变。
#[test]
fn d150_defaults_and_float_paths_unchanged() {
    // 省略上限 → 仍用默认 1000
    let src = r#"
model C
  count: number = 0
  step: number = 1
end

msg M
  Increment
end

update(msg, model)
  match msg {
    Increment => [{count: model.count + 1i, step: model.step}, json.parse("{\"kind\": \"Dispatch\", \"msg\": {\"tag\": \"Increment\"}}")]
  }
end

app A
  model: C
  msg: M
  init: {count: 0, step: 1}
  update: update
  view: fn(model) => model
end

let a = tea.dispatch(A, {tag: "Increment"})
let m = tea.model(tea.run(a, 10))
m
"#;
    let out = run(src).unwrap_or_else(|e| panic!("Float 上限不应报错: {e}"));
    assert!(
        out.contains("count: 10"),
        "Float 上限必须照常生效; 实得: {out}"
    );

    // ccr.marker 的 Float 路径（D149 之前就正常）
    let lit = run("ccr.marker(\"abcdef\", 8)\n").unwrap();
    assert!(lit.contains('8'), "Float size 路径不得回退; 实得: {lit}");
}

/// 对照组 ②：**类型错**必须报错（不再静默用默认值）。
#[test]
fn d150_non_numeric_argument_is_rejected() {
    for (call, field) in [
        ("ccr.marker(\"h\", \"big\")", "size"),
        (
            "tea.run(tea.init(fn() => 1, fn(m, x) => m, fn(m) => m), \"x\")",
            "max_steps",
        ),
    ] {
        let res = run(&format!("{call}\n"));
        assert!(
            res.is_err(),
            "[{call}] 非数字实参必须**报错**（修复前静默用默认值）; 实际: {res:?}"
        );
        assert!(
            res.unwrap_err().contains(field),
            "[{call}] 错误信息应点名 `{field}`"
        );
    }
}

/// 对照组 ③：**必选**数值实参本来就全认（Int/Float/BigInt）—— 不得回退。
///
/// 这是**可从 Mora 触达**的正确样板（`math.rs::expect_number`）。
/// 缺陷只发生在**可选**实参上：那里被迫写 `if let`，把「没传」和「类型不对」
/// 混成了同一件事。
#[test]
fn d150_required_numeric_args_still_accept_int() {
    let out = run("math.pow(2, 10)\n").unwrap_or_else(|e| panic!("math.pow 不应报错: {e}"));
    assert!(
        out.contains("1024"),
        "`math.pow(2, 10)` 必须仍是 1024; 实得: {out}"
    );
    // Int 实参（`len()` 产物）也必须走通 —— 这正是必选路径已做对的那一侧
    let inty = run("math.sqrt(len([4, 9]))\n")
        .unwrap_or_else(|e| panic!("math.sqrt 的 Int 实参不应报错: {e}"));
    assert!(
        inty.contains("1.414"),
        "`math.sqrt(len([4,9]))`（Int 实参）必须工作; 实得: {inty}"
    );
}

/// 源码判据：`ai.rs` 的 `backoff_ms` **也**是 Int/Float 都认的那一处正确写法。
///
/// ⚠ 它**没有 Mora 语法入口** —— D59（`tests/ai_namespace_reachability.rs`）已实测
/// `ai.retry(...)` 永远被 parser 解析成「裸名 `ai` + 方法 `retry`」，
/// 产不出单名 `"ai.retry"`。故本条**只能**写成源码判据，不臆造运行时用例（D148 教训）。
///
/// v0.104.6 D246：断言从「字面含 `Value::Float(n)` **且** `Value::Int(i)`」改为
/// 「走 `flow::value_as_usize`」。原断言钉的是**实现形态**而非行为，于是
/// **正确的收敛也会让它红**（把两个 match 臂收进唯一收口，正是 D235–D246
/// 反复证明该做的事）—— 这是「判据钉在错误的代码上」的又一例。
///
/// 意图「不退回单侧」由两处共同保证，强度不降：
/// - **本条**：此处确实走收口，而不是各写各的；
/// - **`tests/value_extraction_saturation.rs::d246_both_int_and_float_are_accepted`**：
///   收口本身 `Int` / `Float` 都认（该条同时有运行时判据，无需源码断言）。
#[test]
fn d150_ai_retry_backoff_accepts_both_number_kinds_in_source() {
    let src = std::fs::read_to_string("src/interpreter/builtins/ai.rs").expect("读 ai.rs");
    let start = src.find("let backoff_ms").expect("找到 backoff_ms 取值");
    let snippet: String = src[start..].chars().take(400).collect();
    assert!(
        snippet.contains("value_as_usize"),
        "`ai.retry` 的 backoff_ms 必须走 `flow::value_as_usize`（Int/Float 都认，\
         不得手写只认一侧的 match）; 实得: {snippet}"
    );
}
