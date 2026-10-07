//! v0.104.6 D272/D413 —— orchestrate agent 的**形参**：D272 记「形参被解析后丢弃，
//! 体内引用恒为 `nil` 且 exit 0 零诊断」；**D413 已修**（绑定形参）。
//!
//! ## D272 记录的根因
//!
//! `parser_v3/syntax.rs::parse_agent_def` 解析出形参后绑到 **`_params`**，
//! 随即便丢弃；`WitnessAgentDef` / `MirAgentDef` 两个类型**都没有** `params` 字段
//! （`MirOrchestrateAgent` 只是 `MirAgentDef` 的类型别名，所以是**两层**不是三层）。
//!
//! D272 实测（修前）：`agent a(x) => "a:" + x` → **`b:nil`**，exit 0，零诊断。
//!
//! ## D413 采用的修法（D272 三选项中的**②**，由用户选定）
//!
//! | 决策 | 内容 |
//! |---|---|
//! | 单参 | `agent a(x) => …` 的 `x` **等价于** `input`（同一值） |
//! | 多参 | `agent a(x, y) => …` **解析期报错** |
//!
//! ### 为什么多参是「报错」而不是「都绑成 input」
//!
//! - agent 只有**一个**输入值，没有第二个值可绑；
//! - `input` 恒为 `Value::String(input_val.to_string())`（sequential 是裸 `Value`），
//!   **永远是字符串**，没有 list/dict 可按位置解构；
//! - 形参语法在 `docs/mora-spec.md` 里**零出现**、仓内**全部 usages 都是单参**
//!   ⇒ 拒绝的**破坏面为零**，且严格优于「让多参也静默给 nil」。
//!
//! ## 运行时改动覆盖**三条**注入路径
//!
//! | 路径 | 位置 |
//! |---|---|
//! | `Sequential` | `mir/handlers/runtime.rs` —— **不经 pregel 引擎** |
//! | Pregel 顺序 | `pregel/mod.rs` |
//! | Pregel 并行 | `pregel/mod.rs` |
//!
//! ⚠ 只改 pregel 会**漏掉 Sequential** —— 两者是独立代码路径。
//!
//! ## 本文件**仍然钉着**的一条未修不对称（缺陷②）
//!
//! agent 体**完全不走 typeck**（顶层与普通闭包都走）。这属 D272 的**选项③**，
//! 本轮**未选**（需建模 agent 的词法作用域），故相关断言保持原样。
//!
//! ## 解析器为什么能报出**具体**多参错误
//!
//! 解析器主体是 `Option` 驱动的（`None` = 「解析失败」），拿不到原因。
//! ⇒ D413 给 `ParserV3` 加了 `diag: Option<String>` 槽，
//! `compile()` 在 `emit_program()` 之后优先取出它。
//! 否则用户只会看到泛化的 `Failed to parse at line N`。

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

/// **给 `input` 一个真值**（`let` 声明，否则 typeck 报未绑定）后的双 agent 链。
///
/// 这是 D413 的**关键探针**：`input` 未设时首值是 `nil`，
/// 只看 `b:a:nil` 区分不出「绑上了（值恰好是 nil）」与「没绑」。
fn seeded_with_params(body_a: &str, body_b: &str) -> Result<Value, String> {
    let src = format!(
        "let input = \"S\"\n\
         orchestrate sequential input -> result\n\
         \x20 agent a(x) => {body_a}\n\
         \x20 agent b(x) => {body_b}\n\
         end\n\
         result\n"
    );
    run(&src)
}

fn seeded_with_input(body_a: &str, body_b: &str) -> Result<Value, String> {
    let src = format!(
        "let input = \"S\"\n\
         orchestrate sequential input -> result\n\
         \x20 agent a => {body_a}\n\
         \x20 agent b => {body_b}\n\
         end\n\
         result\n"
    );
    run(&src)
}

// ── ① 形参被真正绑定（D413 修复点） ──

/// **形参拿到真实值**（不是 `nil`）。
///
/// 修前同一程序给 `b:nil`。
#[test]
fn d413_agent_param_receives_the_real_input() {
    let got = as_str(
        seeded_with_params("\"a:\" + x", "\"b:\" + x")
            .unwrap_or_else(|e| panic!("应跑通，实际：{e}")),
    );
    assert_eq!(
        got, "b:a:S",
        "形参 `x` 应等价于 `input`：`let input = \"S\"` ⇒ a 得 \"a:S\" ⇒ b 得 \"b:a:S\"。\
         修前（形参被丢弃）是 `b:a:nil`"
    );
}

/// **`x` 与 `input` 是同一个值** —— 两条程序输出**逐字相同**。
///
/// 这条钉住「`(x)` ≡ `input`」这个语义本身，而不只是「x 非空」。
#[test]
fn d413_param_is_exactly_an_alias_of_input() {
    let via_param = as_str(seeded_with_params("\"a:\" + x", "\"b:\" + x").expect("形参版应跑通"));
    let via_input =
        as_str(seeded_with_input("\"a:\" + input", "\"b:\" + input").expect("input 版应跑通"));
    assert_eq!(
        via_param, via_input,
        "形参 `x` 与 `input` 必须是同一个值（D413 选定的语义：(x) ≡ input）"
    );
}

/// **`input` 未设时（首值为 `nil`）行为不变** —— 不是修好了、是把 nil 换个来源。
#[test]
fn d413_param_with_unset_input_still_nil() {
    let got = as_str(sequential_with_params("\"a:\" + x", "\"b:\" + x").expect("应跑通"));
    assert_eq!(
        got, "b:a:nil",
        "`input` 未设时首值是 nil ⇒ 形参也应是 nil（这与修前的 b:nil 不同：\
         修前连第一个 agent 都没串上前一个的值）"
    );
}

// ── ② 多参是解析错误（D413 决策） ──

/// **多参 agent 编译失败，且错误信息点名「至多 1 个参数」。**
#[test]
fn d413_multi_param_agent_is_a_parse_error() {
    let src = "orchestrate sequential input -> result\n\
               \x20 agent a(y, z) => \"y:\" + y\n\
               end\n\
               result\n";
    let err = ParserV3::compile(src).expect_err("多参 agent 应编译失败");
    let msg = err.to_string();
    assert!(
        msg.contains("parameter") && (msg.contains("at most 1") || msg.contains("single")),
        "错误信息应说明「agent 只收一个 input、形参至多 1 个」；实得：{msg}"
    );
    assert!(
        msg.contains("a") && msg.contains("y") && msg.contains("z"),
        "错误信息应点名 agent 名与全部形参；实得：{msg}"
    );
}

/// **单参不被误伤** —— 与上一条配对（否则「全拒」也能让上一条变绿）。
#[test]
fn d413_single_param_agent_still_compiles() {
    let src = "orchestrate sequential input -> result\n\
               \x20 agent a(y) => \"y:\" + y\n\
               end\n\
               result\n";
    ParserV3::compile(src).expect("单参 agent 应编译成功");
}

/// **无形参的 agent 行为完全不变**（`agent a => …` 是既有主流写法）。
#[test]
fn d413_no_param_agent_is_unchanged() {
    let got = as_str(sequential_one("\"a:\" + input").expect("应跑通"));
    assert_eq!(got, "a:nil", "无形参写法不受影响");
}

// ── ③ 对照组：声明但不使用形参仍无害（D272 记录的兼容性边界） ──

/// 声明但**不使用**形参的程序仍能跑（绑不绑都不影响结果）。
#[test]
fn d413_declared_but_unused_params_are_harmless() {
    let got = as_str(
        sequential_with_params("\"A\"", "\"B\"").unwrap_or_else(|e| panic!("应跑通，实际：{e}")),
    );
    assert_eq!(got, "B", "体内没引用形参 ⇒ 无害");
}

// ── ④ 仍未修：agent 体不走 typeck（D272 选项③，本轮未选） ──

/// 缺陷②（**D414 已修**）：agent 体内的类型不匹配现在**会被 typeck 拦下**。
///
/// 原名 `d272_type_mismatch_inside_agent_body_is_not_caught` —— 它钉的是
/// 「不被拦下 + 静默算成 `"1.0str"`」这个**错误**行为。
///
/// D414 给 `WitnessOrchestrateKind` 加了真正的下降推断
/// （`typeck/hm/mod.rs::infer_orchestrate_kind`）⇒ 本条翻转为正确行为断言。
#[test]
fn d414_type_mismatch_inside_agent_body_is_caught() {
    // ① 类型检查层：现在**必须**报出诊断
    let errs = typeck_errors(
        "orchestrate sequential input -> result\n\
         \x20 agent a => 1 + \"str\"\n\
         end\n\
         result\n",
    );
    assert!(
        !errs.is_empty(),
        "agent 体内的 `1 + \"str\"` 应报类型不匹配（D414 已接上 typeck）。实际诊断：{errs:?}"
    );
}

/// 对照组：**顶层**的同一表达式也必须报 —— 与 agent 体构成
/// 「同一段代码、两个上下文、**都**报错」的完整证据。
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
#[test]
fn d272_closure_unbound_variable_is_caught() {
    let errs = typeck_errors("let f = fn(x) { y + 1 }\nf(1)\n");
    assert!(
        !errs.is_empty(),
        "闭包体内 `y` 未绑定应报错 —— 同样是本缺陷的对照基准"
    );
}

/// agent 体内的**未绑定变量**现在**会被检出**（**D414 已修**）。
///
/// 原名 `d272_unbound_name_inside_agent_body_is_not_caught`。
///
/// ⚠ 与 D413 的形参绑定是**两件事**：形参现在**有值**（运行期注入），
/// 而这里是一个**根本没声明**的名字（`zzz`）—— D414 之后两者都能被检出。
#[test]
fn d414_unbound_name_inside_agent_body_is_caught() {
    let errs = typeck_errors(
        "orchestrate sequential input -> result\n\
         \x20 agent a => zzz + 1\n\
         end\n\
         result\n",
    );
    assert!(
        !errs.is_empty(),
        "agent 体内**未声明**的名字 `zzz` 应报 Unbound variable（D414 已接上 typeck）。\
         实际诊断：{errs:?}"
    );
}

// ── ⑤ 源码级护栏：params 必须在三层类型上真的存在 ──

/// **`params` 字段确实存在于两个 agent 类型上**（剥注释后再判）。
///
/// 行为判据覆盖「绑没绑」，本条覆盖「字段还在不在」——
/// 若将来有人为简化把字段删了，本条会先一步变红并指出**在哪**。
#[test]
fn d413_params_field_exists_on_both_agent_types() {
    for (path, needle) in [
        ("src/mir/witness.rs", "pub struct WitnessAgentDef"),
        ("src/mir/orchestrate/mod.rs", "pub struct MirAgentDef"),
    ] {
        let full = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let src = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("读 {path} 失败: {e}"));
        let code: String = src
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(t.starts_with("//") || t.starts_with("///") || t.starts_with("//!"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find(needle)
            .unwrap_or_else(|| panic!("{path} 里应能找到 `{needle}`"));
        let body = &code[at..(at + 400).min(code.len())];
        assert!(
            body.contains("params"),
            "{path} 的 `{needle}` 里没有 `params` 字段 —— 形参又无处安放了\
             （D413 已加）。实得:\n{body}"
        );
    }
}
