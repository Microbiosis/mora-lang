//! v0.104.6 D366 —— `control.rs` 的 **quasiquote** 与 **match 无匹配分支**
//! （否定轮，无产品变更）
//!
//! D360 只扫了 `src/mir/handlers/` 的 panic 面，D361 钉了 `values.rs` 的索引，
//! D362/D363 钉了 `effects.rs`。本轮钉 `control.rs`（148 行）里两个
//! 完整机制：`h_quasiquote`（L112，Lisp 同源语法）与
//! `h_match_expr`（L17）的「无 arm 匹配」分支。
//!
//! ## quasiquote 的记号是 `,,`（**不是** `,@`）
//!
//! `docs/mora-spec.md:916` 明写：
//!
//! ```text
//! let code = `(sum ,,items)    -- `,,` 是 unquote-splice，items 被展开
//! ```
//!
//! 实测（**零缺陷**）：
//!
//! | 形态 | 实测 |
//! |---|---|
//! | `` `,x `` | `code:3` ✅ unquote |
//! | `` `,,xs ``（xs = `[1,2,3]`）| `code:1, 2, 3` ✅ splice 展开 |
//! | `` `,@[xs] `` | **parse 失败** |
//! | `` `,@ [xs] `` | **parse 失败** |
//!
//! ⇒ 后两条**不是缺陷**：lexer 只切 `,,` → `TokenType::CommaComma`
//! （`lexer.rs:379` 注释原文：`v0.88: `,,` → CommaComma`），
//! `,@` 里的 `@` 让表达式解析失败。spec 承诺的就是 `,,`。
//!
//! **教训**：我第一反应是按 **Lisp 习惯**写 `,@`，
//! 测出「parse 失败」后差一步就写成缺陷。
//! 是 grep `docs/mora-spec.md` + `lexer.rs` 才确认 `,,` 才是 Mora 的记号。
//!
//! ## 括号深度决定 `,` 是不是 unquote（设计如此）
//!
//! `emit.rs:1070` 的 `_ if depth == 0` 守卫 + 注释第 5 条
//! 「深度 > 0 时，逗号为**静态源码**的一部分」：
//!
//! ```text
//! `(+ 1 ,,ys)   →  code:(+ ,,ys)      ← 括号内 ⇒ 静态
//! `,,ys         →  code:1, 2, 3       ← 顶层 ⇒ splice
//! ```
//!
//! ## `h_match_expr` 无 arm 匹配 → Nil，且**后续语句继续执行**
//!
//! ```rust
//! // control.rs:57
//! if !matched && let Some((_pat, _guard, _func, output_reg)) = arms.first() {
//!     regs[*output_reg] = Value::Nil;
//! }
//! ```
//!
//! 实测 `match 99 { 1 => "one" }` → `nil` + 后续 `print` 正常（exit 0），
//! 即**不吞并后续语句**（D35 修过同一族：match 后语句被饿死）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d366_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d366_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let path_str = p.to_string_lossy().into_owned();
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.contains("不兼容 v0.03")
                && !l.starts_with("[9layer]")
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// **装置自检**（D356 教训：先证装置有效，再看它测出的数据）。
#[test]
fn d366_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// **`quote(expr)` 捕获的是**源码文本**，不是求值结果**。
///
/// `tests/fixtures/e2e/lisp.mora:49` 的注释明写「`quote(expr)` 在解析期
/// 捕获 expr **源码文本**」。本条钉住这一点 + 往返不变式
/// `eval(quote(expr)) == expr`。
#[test]
fn d366_quote_captures_source_text_and_roundtrips() {
    // 求值路径
    let (code, got) = ev("print(eval(quote(2 + 3)))\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "5.0", "eval(quote(2+3)) 应为 5; 实得: {got}");

    // 冻结路径：闭包源码原样保留
    let (code, got) = ev("let q = quote(fn(z) z + 1 end)\nprint(q)\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert!(
        got.contains("fn(z) z + 1 end"),
        "quote 应冻结**源码文本**; 实得: {got}"
    );

    // 冻结出来的东西能 eval 回去
    let (code, got) = ev("let f = eval(quote(fn(y) y * 2 end))\nprint(f(5))\n");
    assert_eq!(code, 0, "往返应成立; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "10.0",
        "eval(quote(fn)) 作用于 5 应得 10; 实得: {got}"
    );
}

/// **quasiquote unquote**（depth 0 的 `,expr`）。
#[test]
fn d366_quasiquote_unquote_at_top_level() {
    let (code, got) = ev("let x = 3\nlet q = `,x\nprint(q)\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert!(got.contains("3"), "`,x` 应把 x 的值 3 插进去; 实得: {got}");
    assert!(
        !got.contains(",x"),
        "unquote 后不应再留 `,x` 记号; 实得: {got}"
    );
}

/// **quasiquote unquote-splice**（`,,expr`，**spec §11.7 承诺的记号**）。
#[test]
fn d366_quasiquote_unquote_splice_with_double_comma() {
    let (code, got) = ev("let xs = [1, 2, 3]\nlet q = `,,xs\nprint(q)\n");
    assert_eq!(
        code, 0,
        "`` `,,xs `` 应正常（spec §11.7 的写法）; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("1, 2, 3"),
        "splice 应把 [1,2,3] 展开为 `1, 2, 3`; 实得: {got}"
    );
}

/// **括号深度 > 0 时逗号是静态的**（`emit.rs:1070` 的 `_ if depth == 0` 守卫）。
///
/// 这是**设计**（注释第 5 条：「深度 > 0 时，逗号为静态源码的一部分」），
/// 本条钉住它，防止将来有人「顺手放宽」成 Lisp 那样处处插值。
#[test]
fn d366_comma_inside_brackets_is_static() {
    let (code, got) = ev("let ys = [1, 2]\nlet q = `(+ 1 ,,ys)\nprint(q)\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert!(
        got.contains("(+ 1 ,,ys)"),
        "括号内的 `,,` 应保持静态; 实得: {got}"
    );
}

/// **splice 的操作数必须�� List** —— 非 List 明确报错。
///
/// `control.rs:136-141` 的错误信息用 `type_name` 而**不是** `{:?}`
/// （`Value::Dict` 的 Debug 键序每进程随机，会让同一条消息跨进程不同）。
#[test]
fn d366_splice_on_non_list_errors_with_type_name() {
    let (code, got) = ev("let n = 5\nlet q = `,,n\nprint(q)\n");
    assert_eq!(code, 1, "splice 非 List 应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("expected List") && got.contains("float"),
        "诊断应点名期望类型与实际类型名; 实得: {got}"
    );
}

/// **`,@` 记号在本版不可用** —— 这**不是**缺陷，spec 承诺的是 `,,`。
///
/// 本条的作用是把这个事实**显式钉住**：将来若有人加上了 `,@` 支持，
/// 本条会红，提醒同步更新 spec §11.7。
#[test]
fn d366_comma_at_syntax_is_not_supported() {
    for (tag, body) in [
        ("no_space", "let xs = [1, 2]\nlet q = `,@[xs]\nprint(q)\n"),
        (
            "with_space",
            "let xs = [1, 2]\nlet q = `,@ [xs]\nprint(q)\n",
        ),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 2,
            "`,@`（{tag}）在本版不可用 —— lexer 只切 `,,`; \
             若将来支持了，需同步更新 spec §11.7 与本判据。实得 out={got}"
        );
    }
}

/// **源码层断言**：lexer 只切 `,,` → `CommaComma`，没有 `,@` 分支。
#[test]
fn d366_lexer_only_tokenizes_double_comma() {
    let lexer = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lexer.rs"))
        .expect("读 lexer.rs");
    assert!(
        lexer.contains("TokenType::CommaComma"),
        "lexer 应有 `,,` → CommaComma 的分支"
    );
    assert!(
        lexer.contains("单 `,` → Comma"),
        "lexer 注释应说明单 `,` → Comma"
    );
    // spec 承诺的也是 `,,`
    let spec = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/mora-spec.md"))
        .expect("读 spec");
    assert!(
        spec.contains("unquote-splice"),
        "spec §11.7 应有 unquote-splice 章节"
    );
}

/// **`match` 无 arm 匹配 → Nil，且后续语句继续执行**。
///
/// `control.rs:57` 用 `arms.first()` 的 `output_reg` 写 Nil。
/// 关键是**不吞并后续语句**（D35 修过同一族：match 后语句被饿死）。
#[test]
fn d366_match_without_matching_arm_yields_nil_and_continues() {
    let (code, got) =
        ev("let v = 99\nlet r = match v {\n  1 => \"one\"\n}\nprint(r)\nprint(\"after\")\n");
    assert_eq!(code, 0, "无匹配不应报错; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "nil | after",
        "应为 nil 且后续继续执行; 实得: {got}"
    );
}

/// **反向对照**：有匹配时**不能**返回 nil。
#[test]
fn d366_match_with_matching_arm_is_untouched() {
    let (code, got) =
        ev("let v = 1\nlet r = match v {\n  1 => \"one\"\n  2 => \"two\"\n}\nprint(r)\n");
    assert_eq!(code, 0, "应正常; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "one", "应命中第一个 arm; 实得: {got}");
}
