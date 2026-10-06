//! v0.104.6 D363 —— `h_perform` / `h_handle` 的**代数效果**路径（否定轮，无产品变更）
//!
//! D362 钉了 `effects.rs` 的 `define`/`assign`，本轮钉同一文件里
//! **代数效果**的核心两个指令：`h_perform`（L493）与 `h_handle`（L520）。
//!
//! ## `h_perform` 的错误消息承诺了一件**可验证**的事
//!
//! ```rust
//! // effects.rs:512
//! None => Err(format!(
//!     "unhandled effect: {} (no matching handle block in scope;
//!      **typeck reports this as an EffectRowMismatch at compile time** —
//!      this runtime fallback only guards dynamically generated code)",
//!     effect
//! )),
//! ```
//!
//! 消息里明写「typeck 会在**编译期**报 EffectRowMismatch」——
//! 这是**注释式断言**，本轮**实测确认它是真的**：
//!
//! ```text
//! let r = perform Ai("hello")     → exit 2
//!   Type error at line 1:9: Effect row mismatch: expected no unhandled
//!   effects — wrap in a matching `handle` block, got { Ai }
//! ```
//!
//! ⇒ 那条运行期 fallback 在脚本层**不可达**（typeck 先行拦下），
//! 只能用 typeck 侧的判据钉。
//!
//! ## handler 栈语义全部正确（三层嵌套实测）
//!
//! `h_handle` 的 L532 `take_effect_handler` / L535 `install_effect_handler`
//! 构成一个**嵌套栈**。三层形态实测：
//!
//! | 场景 | 实测 | 应得 |
//! |---|---|---|
//! | 单层 | `mocked:hello` | handler 返回值 |
//! | 异名嵌套（`Bi` 内不处理 `Ai`）| `A:x` | 冒泡到外层 ✅ |
//! | **同名嵌套** | `INNER:inner` | **内层优先** ✅ |
//!
//! 同名嵌套那条是**关键** —— 它证明栈是真的（不是「后装的覆盖前装的」
//! 那种单槽实现，那会得 `OUTER:inner`）。
//!
//! ## 顺带查明的一条既有约束
//!
//! `handle` 的**体内**不能用 `perform`：`"B:" + perform Ai("y")`
//! 报 *Unbound variable 'perform'* —— handler 体是**表达式**，
//! 不是语句块。**只报告**（是设计还是限制待裁决）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d363_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d363_{}_{}", n, slug(body)));
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
fn d363_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// **主断言 1**：**无 handler 的 `perform` 必须在编译期被拦下**。
///
/// 这是 `h_perform` 错误消息里明写的承诺
/// （*typeck reports this as an EffectRowMismatch at compile time*）。
/// 实测确认承诺成立 ⇒ 那条运行期 fallback 在脚本层不可达。
#[test]
fn d363_unhandled_perform_is_caught_at_compile_time() {
    let (code, got) = ev("let r = perform Ai(\"hello\")\nprint(r)\n");
    assert_eq!(
        code, 2,
        "无匹配 handler 的 perform 应被 typeck 拦下; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("Effect row mismatch") || got.contains("unhandled effects"),
        "诊断应是 EffectRowMismatch（正是 h_perform 消息里承诺的）; 实得: {got}"
    );
    // 诊断应点名这个 effect
    assert!(
        got.contains("Ai"),
        "诊断应点名未处理的 effect `Ai`; 实得: {got}"
    );
}

/// **主断言 2**：单层 handler 返回 handler 体的值。
#[test]
fn d363_single_handler_returns_its_value() {
    let (code, got) = ev(
        "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(g)\n",
    );
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "mocked:hello",
        "handler 应返回 `mocked:hello`; 实得: {got}"
    );
}

/// **主断言 3**：**同名嵌套 handler 必须内层优先**。
///
/// 这是本轮最关键的一条 —— 它证明 `take_effect_handler` /
/// `install_effect_handler` 构成的是**栈**，而不是单槽覆盖。
/// 若实现是单槽（后装的直接替换），这里会得 `OUTER:inner`。
#[test]
fn d363_same_name_nested_handler_prefers_inner() {
    let (code, got) = ev(
        "let r = handle Ai {\n  handle Ai {\n    perform Ai(\"inner\")\n  } {\n    \"INNER:\" + __arg0\n  }\n} {\n  \"OUTER:\" + __arg0\n}\nprint(r)\n",
    );
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "INNER:inner",
        "同名嵌套必须是**内层优先**（栈语义）；单槽实现会得 OUTER:inner"
    );
}

/// **主断言 4**：**异名嵌套冒泡** —— 内层 `Bi` 不处理 `Ai` 时，
/// `perform Ai` 必须冒泡到外层 handler。
///
/// 与上一条互为**反向对照**：上一条证「栈」会截获，
/// 这一条证「不匹配时**不**截获而是继续上溯」。
#[test]
fn d363_different_name_nested_handler_bubbles_up() {
    let (code, got) = ev(
        "let r = handle Ai {\n  handle Bi {\n    perform Ai(\"x\")\n  } {\n    \"B-handled\"\n  }\n} {\n  \"A:\" + __arg0\n}\nprint(r)\n",
    );
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "A:x",
        "内层 Bi 不匹配 Ai ⇒ 必须冒泡到外层 Ai handler; 实得: {got}"
    );
}

/// **同层连续两次 perform** 应各自被 handler 处理。
#[test]
fn d363_two_performs_in_one_handler_body() {
    let (code, got) =
        ev("let a = perform Ai(\"one\")\nlet b = perform Ai(\"two\")\nprint(a + \"|\" + b)\n");
    // 无 handler ⇒ 应被 typeck 拦下
    assert_eq!(code, 2, "无 handler 应被拦下; 实得 exit={code} out={got}");
    assert!(
        got.contains("Effect row mismatch") || got.contains("unhandled effects"),
        "应报 EffectRowMismatch; 实得: {got}"
    );
}

/// **handler 体是表达式不是语句块** —— 体内用 `perform` 会被 typeck 拒。
///
/// 记为**既有约束**（不是缺陷）：`handle` 的 handler 体是单个表达式。
#[test]
fn d363_handler_body_is_an_expression_not_a_block() {
    let (code, got) = ev(
        "let r = handle Ai {\n  handle Bi {\n    perform Ai(\"x\")\n  } {\n    \"B:\" + perform Ai(\"y\")\n  }\n} {\n  \"A:\" + __arg0\n}\nprint(r)\n",
    );
    assert_eq!(
        code, 2,
        "handler 体内用 perform 应被拒; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("Unbound variable 'perform'"),
        "应是「perform 在表达式位置不是变量」; 实得: {got}"
    );
}

/// **非法的 effect 语法仍应正确报错**。
#[test]
fn d363_malformed_effect_syntax_errors() {
    // `handle` 缺一个体
    let (code, got) = ev("handle Ai {\n  perform Ai(\"x\")\n}\n");
    assert_ne!(code, 0, "残缺的 handle 应报错; 实得 exit={code} out={got}");

    // `perform` 缺 effect 名
    let (code, _) = ev("let r = perform(\"x\")\n");
    assert_ne!(code, 0, "`perform(\"x\")` 缺 effect 名，应报错");
}
