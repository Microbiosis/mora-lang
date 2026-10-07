//! v0.104.6 D415：`infer_orchestrate_kind` 注入的 `input` **类型选错**
//! ⇒ 合法的惯用写法被 typeck **拒绝执行**（假阳性，修复轮）
//!
//! ## 现象（D414 引入，D415 修）
//!
//! ```mora
//! orchestrate sequential input -> result
//!   agent a => input + "x"
//! end
//! ```
//!
//! ```text
//! Type error (位置未跟踪): Type mismatch: expected String, got TypeVar('\0')
//! exit 2          ← 不只是 --check 误报，mora run 同样拒绝执行
//! ```
//!
//! 而**同一表达式在普通闭包里通过**：
//!
//! ```mora
//! let q = "S"
//! let f = fn (q) { q + "x" }
//! f(q)           → Sx        exit 0
//! ```
//!
//! ⇒ D414 CHANGELOG 里写的「与顶层/普通闭包一致」**并不成立**。
//!
//! ## 为什么全量门禁 2828 全绿却抓不到
//!
//! **仓内 `.mora` fixture 里零个 agent 体使用 `input`** ——
//! 没有任何现存测试跑过「agent 体里对 `input` 做运算」这条路径。
//! ⇒ 这类回归**结构上**不可能被既有语料覆盖。
//! 本文件就是补上这条缺口。
//!
//! ## 根因
//!
//! D414 注入的是 `Type::Unknown`。但：
//!
//! | 层 | 对 `Unknown` 的处理 |
//! |---|---|
//! | `Type::compatible_with()` | ✅ 放行（v0.84 起，Unknown = top type） |
//! | **solver 的 `unify()`** | ❌ **不放行** ⇒ `Eq(String, result_ty)` 求解失败 |
//!
//! ⇒ 作用域建模是对的，**类型载体选错了**。
//!
//! ## 修法：注入 **fresh TypeVar**（HM 标准做法）
//!
//! TypeVar 能被 solver 正常绑到 `String`，`input + "x"` 随即通过；
//! 同时 `1 + "str"`、`zzz + 1` 仍被检出（见下面两条配对判据）。
//!
//! ## ⚠ 配对要求
//!
//! 「不误报」与「仍检出」**必须同时**成立才算修好：
//! 只钉前者，则「typeck 整体对 orchestrate 沉默」也能让它变绿
//! （那正是 D414 之前的状况）。

use std::process::Command;

use mora::parser_v3::ParserV3;

/// 编译 + 跑 typeck，返回诊断数。
fn typeck_error_count(src: &str) -> usize {
    let (_f, w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile 应成功: {e}"));
    mora::typeck::check_mir::check_program_witnesses(&w).len()
}

/// 走真实 CLI（**typeck 必走的那条路**）—— 库级 harness 不跑 typeck。
///
/// ⚠ `tag` **必须逐条不同**：临时目录会被 `remove_dir_all` 重建，而 Rust
/// 测试默认**并行**⇒ 共用一个目录会互相踩（我第一版就踩了：
/// 两条 CLI 判据同用 `mora_d415_fp`，随机一条报「文件不存在」）。
fn cli_exit(tag: &str, src: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d415_fp_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写脚本");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.code().unwrap_or(-1), text)
}

/// **核心：agent 体里对 `input` 做运算必须被放行**（D415 修复点）。
///
/// 牙齿验证：把注入改回 `Type::Unknown` ⇒ 本条变红。
#[test]
fn d415_agent_body_can_operate_on_input() {
    let src = "orchestrate sequential input -> result\n  agent a => input + \"x\"\nend\nresult\n";
    assert_eq!(
        typeck_error_count(src),
        0,
        "agent 体内的 `input + \"x\"` **不得**报类型错误（D415 已修）。\
         `input` 在运行期是真实值（Sequential 取 input_var 的值、\
         Pregel 取 `Value::String`），typeck 必须把它当**多态**处理。\
         注入 `Type::Unknown` 会被 solver 的 `unify` 拒掉（`compatible_with` 放行、\
         `unify` 不放行）⇒ 合法程序被拒绝执行。"
    );
}

/// **走真实 CLI**：合法程序必须**跑得起来**（不只是库级不报错）。
#[test]
fn d415_legal_agent_body_runs_under_real_cli() {
    let (code, text) = cli_exit(
        "agent_body",
        "let input = \"S\"\norchestrate sequential input -> result\n  agent a => input + \"x\"\nend\nprint(result)\n",
    );
    assert_eq!(
        code, 0,
        "合法程序被 typeck 拒绝执行（exit {code}）:\n{text}"
    );
    assert!(
        text.contains("Sx"),
        "应输出 `Sx`（input=\"S\" ⇒ \"Sx\"）; 实得:\n{text}"
    );
}

/// **配对 ①**：真实缺陷**仍必须**被检出 —— 不能为消假阳性把检出也弄丢。
#[test]
fn d415_type_mismatch_in_agent_body_is_still_caught() {
    let src = "orchestrate sequential input -> result\n  agent a => 1 + \"str\"\nend\nresult\n";
    assert!(
        typeck_error_count(src) > 0,
        "agent 体内的 `1 + \"str\"` 仍必须报（D414 的核心检出不得被 D415 削弱）"
    );
}

/// **配对 ②**：未绑定变量**仍必须**被检出。
#[test]
fn d415_unbound_name_in_agent_body_is_still_caught() {
    let src = "orchestrate sequential input -> result\n  agent a => zzz + 1\nend\nresult\n";
    assert!(
        typeck_error_count(src) > 0,
        "agent 体内未声明的 `zzz` 仍必须报 Unbound"
    );
}

/// **形参与 `input` 同源** —— 形参也要是多态，不能是 Unknown。
#[test]
fn d415_agent_param_can_operate_on_its_value() {
    let src = "let input = \"S\"\norchestrate sequential input -> result\n  agent a(x) => x + \"x\"\nend\nresult\n";
    assert_eq!(
        typeck_error_count(src),
        0,
        "形参 `x` 与 `input` 同值，同样必须多态（D415）"
    );
}

/// **`result` / `input` 在 orchestrate **之外**使用同样不得误报**。
///
/// 这条锁的是**另一处、且是既有**的同族缺陷：`input_var` / `result_var`
/// 自 v0.75.91 的 `ed5afe8` 起就登记为 `Type::Unknown`，与 D414 无关。
/// 修前实测：
///
/// ```text
/// orchestrate sequential input -> result
///   agent a => "A"
/// end
/// print(result + "!")     → expected String, got TypeVar('\u{2}')   exit 2
/// ```
#[test]
fn d415_result_used_outside_orchestrate_is_not_a_false_positive() {
    let src = "orchestrate sequential input -> result\n  agent a => \"A\"\nend\nresult + \"!\"\n";
    assert_eq!(
        typeck_error_count(src),
        0,
        "orchestrate 之外的 `result + \"!\"` **不得**报类型错误 —— \
         `result_var` 曾登记为 `Type::Unknown`，solver 的 `unify` 不放行它 \
         ⇒ 合法程序被拒绝执行（D415 已改为 fresh TypeVar）"
    );
    let (code, text) = cli_exit(
        "outside",
        "orchestrate sequential input -> result\n  agent a => \"A\"\nend\nprint(result + \"!\")\n",
    );
    assert_eq!(
        code, 0,
        "`print(result + \"!\")` 应跑通; 实得 exit {code}:\n{text}"
    );
    assert!(text.contains("A!"), "应输出 `A!`; 实得:\n{text}");
}

/// **对照组**：`input` 真的设成非字符串时，按那个类型推断。
///
/// 防止「为了让上面几条过，把 `input` 一律塞成 String」这种**反向**过拟合。
#[test]
fn d415_input_respects_the_actual_binding_type() {
    let src = "let input = 1\norchestrate sequential input -> result\n  agent a => input + 1\nend\nresult\n";
    assert_eq!(
        typeck_error_count(src),
        0,
        "`input` 绑成 Int 时 `input + 1` 应通过（数值加法）"
    );
    // 而把 Int 与 String 混用仍应报错
    let bad = "let input = 1\norchestrate sequential input -> result\n  agent a => input + \"x\"\nend\nresult\n";
    assert!(
        typeck_error_count(bad) > 0,
        "`input` 绑成 Int 时 `input + \"x\"` 仍应报 —— 说明 typeck 真的在用绑定类型，\
         而不是无脑放行"
    );
}
