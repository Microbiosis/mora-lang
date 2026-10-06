//! v0.104.6 D272：orchestrate agent 的**形参被静默丢弃** + agent 体**完全不走 typeck**
//!
//! ## ⚠ 本文件是**现状判据**：它断言的是**当前的错误行为**
//!
//! 它今天通过，**恰恰是因为缺陷还在**。修好之后这些断言会**开始失败** ——
//! 那时请按最后一节「何时该改写」整体替换，不要逐条修补。
//!
//! ## 缺陷 ①：形参被解析、被丢弃
//!
//! `parser_v3::parse_agent_def` 会解析 `agent a(x) => …` 的形参列表
//! （`let params = if let TokenType::LParen = …`），但随后的
//! `Some(MirOrchestrateAgent { name, with_config, task_expr, verify_expr,
//! task_body, combiner_body })` **根本没有 params 字段**——
//! `MirAgentDef` / `WitnessAgentDef` 两层也都没有。
//!
//! 真实 CLI：
//!
//! ```mora
//! orchestrate sequential input -> result
//!   agent a(x) => "a:" + x
//!   agent b(x) => "b:" + x
//! end
//! print(result)          -- → b:nil   （exit 0，零诊断）
//! ```
//!
//! `x` 从未被绑定，在字符串拼接里渲染成 `nil`。而**能工作的写法**是用
//! `input`（运行时的管线契约，见 `runtime.rs` 的 Sequential 分支）：
//!
//! ```mora
//!   agent a => "a:" + input
//!   agent b => "b:" + input
//! -- → b:a:nil
//! ```
//!
//! ⇒ `agent a(user) => "processed " + user` 这类写法会为**每次**请求
//! 返回 `"processed nil"` 而 `exit 0`。这比「值被忽略」更隐蔽：输出
//! 是一个看起来完全正常的字符串。
//!
//! ## 缺陷 ②：agent 体现在**完全不走 typeck**
//!
//! 这是①之所以静默的机制。Mora 的 CLI 横幅宣告「typeck 必走」，但
//! agent 是个例外：
//!
//! | 上下文 | 代码 | 实测 |
//! |---|---|---|
//! | 顶层 | `1 + "str"` | `Type error: expected Float, got String` ✓ |
//! | 普通闭包 | `fn(x) { y + 1 }` | `Type error: Unbound variable 'y'` ✓ |
//! | **orchestrate agent 体** | `1 + "str"` | **`1.0str`，exit 0** ❗ |
//! | **orchestrate agent 体** | `"a:" + x` | **`b:nil`，exit 0** ❗ |
//!
//! 注意最后一行不止是漏检「未绑定变量」：连**类型不匹配**都不报
//! （`1 + "str"` 被强行算成 `"1.0str"`）。⇒ 整个 agent 体的类型检查
//! 都被跳过了，不只是绑定问题。
//!
//! ## 与前一轮的关系（不是重复发现）
//!
//! 上一轮（D216 附带的「未结论的探查记录」，2026-10-03）已实测到
//! 「agent 声明的参数 `x` 始终未被绑定」，但**判为「能力缺口」并搁置**：
//!
//! > 这更像**能力缺口**（没有 channel 的用户表面）而不是「静默算错」…
//! > **本轮不擅自扩范围，留作候选。**
//!
//! 本条的增量正是把那个判断**推翻**：
//! ① 它**不是**「表达不了」—— `input` 管线是能用的（`b:a:nil`）；
//!    形参形态是「**能写、且静默给出错值**」，属静默错值族。
//! ② 补上了此前没有的机制：agent 体不走 typeck（闭包走）。
//! ③ 补上了修法的**兼容性代价**：声明但**不使用**形参的程序
//!    （`agent a(x) => "A"`）今天**能正常工作** ⇒ 直接禁掉该语法会破坏它们。
//!
//! ## 何时该改写本文件
//!
//! 修法有三种（属产品契约决定，见 CHANGELOG D272「修法选项」）：
//!
//! | 选项 | 修好后的行为 |
//! |---|---|
//! | ① 禁掉形参语法 | `agent a(x) =>` 变成**解析错误** |
//! | ② 绑定形参 | `x` **等于**管线输入（`(x)` ≡ `input`） |
//! | ③ 给 agent 体接上 typeck | 未绑定变量 / 类型不匹配变成**类型错误** |
//!
//! 任一选项落地后，把下面的现状断言替换为该选项的**期望行为**断言。
//! 无论选哪个，**「普通闭包与顶层走 typeck、agent 体不走」这条不对称
//! 都应当被消除或至少被显式记录**。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn run(source: &str) -> Result<Value, String> {
    let (func, _w) = ParserV3::compile(source).map_err(|e| format!("compile: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
}

fn as_str(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => panic!("期望 String 结果，实际：{other:?}"),
    }
}

/// 跑 typeck。
///
/// 注意：`ParserV3::compile` + `run_mir` 这条库级 harness **本身不跑 typeck**
/// —— 顶层 `1 + "str"` 在库里同样算成 `"1.0str"`。typeck 是 CLI 额外加的一遍，
/// 所以判 typeck 行为必须显式调它，不能借 `run` 的执行结果推断。
fn typeck_errors(source: &str) -> Vec<String> {
    let (_func, witnesses) =
        ParserV3::compile(source).unwrap_or_else(|e| panic!("compile 应成功: {e}"));
    mora::typeck::check_mir::check_program_witnesses(&witnesses)
        .into_iter()
        .map(|e| format!("{e:?}"))
        .collect()
}

fn sequential(agent_a: &str, agent_b: &str) -> Result<Value, String> {
    let src = format!(
        "orchestrate sequential input -> result\n\
         \x20 agent a => {agent_a}\n\
         \x20 agent b => {agent_b}\n\
         end\n\
         result\n"
    );
    run(&src)
}

fn sequential_with_params(body_a: &str, body_b: &str) -> Result<Value, String> {
    let src = format!(
        "orchestrate sequential input -> result\n\
         \x20 agent a(x) => {body_a}\n\
         \x20 agent b(x) => {body_b}\n\
         end\n\
         result\n"
    );
    run(&src)
}

/// **单** agent 的 sequential —— `result` 就是该 agent 的返回值。
///
/// 双 agent 链里 `result` 取的是**链尾** agent 的值（last-write-wins），
/// 想观察第一个 agent 的产物必须用这个。
fn sequential_one(body_a: &str) -> Result<Value, String> {
    let src = format!(
        "orchestrate sequential input -> result\n\
         \x20 agent a => {body_a}\n\
         end\n\
         result\n"
    );
    run(&src)
}

/// 缺陷 ① 主断言：声明的形参**不会**被绑定，体内引用它得到 `nil`。
///
/// 修好之后本条会红（期望 `"a:" + x` 拿到真实输入）。
#[test]
fn d272_declared_agent_params_are_never_bound() {
    let got = as_str(
        sequential_with_params("\"a:\" + x", "\"b:\" + x")
            .unwrap_or_else(|e| panic!("应跑通，实际：{e}")),
    );
    assert_eq!(
        got, "b:nil",
        "现状：`x` 从未绑定 ⇒ 拼接出 \"b:nil\"。若本条失败说明形参已被绑定，\
         请把本文件改写为对应修法的期望行为"
    );
}

/// 对照组：`input` 管线是**能用的** ⇒ 本条证明①不是「整个 surface 不可用」。
#[test]
fn d272_input_pipeline_does_work() {
    let got = as_str(
        sequential("\"a:\" + input", "\"b:\" + input")
            .unwrap_or_else(|e| panic!("应跑通，实际：{e}")),
    );
    assert_eq!(
        got, "b:a:nil",
        "对照组：不用形参、改读 `input` 时管线正常（首轮 input 为 nil）"
    );
}

/// 兼容性边界：声明但**不使用**形参的程序今天**能正常工作**。
///
/// 这条决定了「直接禁掉形参语法」不是零成本方案 —— 本条会在那种修法下变红。
#[test]
fn d272_declared_but_unused_params_are_harmless_today() {
    let got = as_str(
        sequential_with_params("\"A\"", "\"B\"").unwrap_or_else(|e| panic!("应跑通，实际：{e}")),
    );
    assert_eq!(
        got, "B",
        "现状：形参声明了但体内没引用 ⇒ 无害。禁掉形参语法的方案会破坏这类程序"
    );
}

/// 缺陷 ②：agent 体内的**类型不匹配**既不被 typeck 拦下，也会被强行算成字符串拼接。
///
/// 修好之后本条会红（typeck 应报错）。
#[test]
fn d272_type_mismatch_inside_agent_body_is_not_caught() {
    // 执行层：静默算成字符串拼接。
    let got = as_str(sequential_one("1 + \"str\"").unwrap_or_else(|e| panic!("应跑通，实际：{e}")));
    assert_eq!(
        got, "1.0str",
        "现状：`1 + \"str\"` 在 agent 体内不被拦下 ⇒ 算成 \"1.0str\"。\
         若本条失败说明 agent 体已接上 typeck"
    );
    // 类型检查层：同样零诊断。
    let errs = typeck_errors(
        "orchestrate sequential input -> result\n\
         \x20 agent a => 1 + \"str\"\n\
         end\n\
         result\n",
    );
    assert!(
        errs.is_empty(),
        "现状：agent 体内的类型不匹配**不产生任何 typeck 诊断**。\
         实际诊断：{errs:?} —— 若本条失败说明 typeck 已接上 agent 体"
    );
}

/// 对照组：**顶层**的同一表达式必须报类型错误。
///
/// 与 `d272_type_mismatch_inside_agent_body_is_not_caught` 并置才构成
/// 「同一段代码、两个上下文、一个报错一个不报」的完整证据。
#[test]
fn d272_top_level_type_mismatch_is_caught() {
    let errs = typeck_errors("let z = 1 + \"str\"\nz\n");
    assert!(
        !errs.is_empty(),
        "顶层 `1 + \"str\"` 应报类型不匹配 —— 这是本缺陷的对照基准，\
         若它也不报，说明问题不在 agent 而在 typeck 整体"
    );
}

/// 对照组：**普通闭包**体内的未绑定变量必须被 typeck 检出。
///
/// 证明「未绑定变量检查本身是好的」，缺的是把它接到 orchestrate agent 门上。
#[test]
fn d272_closure_unbound_variable_is_caught() {
    let errs = typeck_errors("let f = fn(x) { y + 1 }\nf(1)\n");
    assert!(
        !errs.is_empty(),
        "闭包体内 `y` 未绑定应报错 —— 同样是本缺陷的对照基准"
    );
}

/// 对照组：agent 体内的**未绑定变量**同样零诊断。
///
/// 这是缺陷①之所以静默的机制（与运行时渲染成 `nil` 是两件事：
/// 一条管诊断、一条管取值）。
#[test]
fn d272_unbound_name_inside_agent_body_is_not_caught() {
    let errs = typeck_errors(
        "orchestrate sequential input -> result\n\
         \x20 agent a => y + 1\n\
         end\n\
         result\n",
    );
    assert!(
        errs.is_empty(),
        "现状：agent 体内引用未绑定变量 `y` 不产生任何 typeck 诊断。\
         实际诊断：{errs:?} —— 若本条失败说明 typeck 已接上 agent 体"
    );
}
