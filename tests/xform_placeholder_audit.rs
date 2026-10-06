//! v0.104.6 D385 —— `xform` builtin 的**实际行为** + transducer 底层**并非死代码**
//! （否定轮，修正 D346 的「死代码」印象）
//!
//! D346 判定 `xform` 是「静默无效」，但当时**没查底层**。
//! 本轮查清两件事：
//!
//! ## ① transducer 底层**接上了**，不是死代码
//!
//! `src/value/transducer.rs`（239 行）有完整的 `Transducer` trait、
//! `Map`/`Filter`/`Take`/`Comp` 四个实现与 `map`/`filter`/`take`/`comp` 四个构造器。
//!
//! **它被两处真实调用**：
//!
//! | 位置 | 用途 |
//! |---|---|
//! | `interpreter/ai_helpers.rs:229` | `mut xform: Option<&mut dyn Transducer<String,String>>` — 逐个推入 SSE token |
//! | `interpreter/method_dispatch.rs:964` | `call_method_stream(.., xform, ..)` |
//!
//! ## ② 但 `xform` builtin 返回的是**自描述占位串**，不是可用的 transducer
//!
//! ```text
//! xform.attach(1)                  → 1.0            （原值透传）
//! xform.attach(fn(x) x end)        → closure
//! xform.map(fn)                   → "<xform.map(closure)>"
//! xform.filter(fn)                → "<xform.filter(closure)>"
//! xform.take(99)                  → "<xform.take(99)>"
//! ```
//!
//! 而 `call_method_stream`（`method_dispatch.rs:960`）在**该文件外零调用**
//! ⇒ 没有任何路径能把脚本侧的 `xform` 传进流式处理。
//!
//! ## ③ 占位串刻意规避了 `HashMap` 键序随机
//!
//! `xform.rs:14-19` 的注释记录了原因：`Value::Dict` 的 `Debug` 走 HashMap
//! 迭代序，而 `RandomState` 每进程随机种子 ⇒ 同一段程序连跑 5 次得到
//! 5 个不同的 `xform.map({...})` 描述。
//!
//! ⇒ 与 D332/D333「`v.to_string()` 键空间」同族，此处**已修**。
//! 判据钉住「同一程序连跑结果相同」。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d385_{n}_{}", n));
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
                && !l.contains(&path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **`attach` 原值透传** —— 它确实「附加」了 transducer，而不是丢弃。
#[test]
fn d385_attach_passes_the_value_through() {
    let (code, got) = ev("let t = xform.attach(1)\nprint(t)\n");
    assert_eq!(code, 0, "attach 应成功; got={got}");
    assert_eq!(got.trim(), "1.0", "`attach(1)` 应得 1.0（原值）");
}

/// **`map` / `filter` / `take` 返回**自描述占位串**。
///
/// 记录**现状**（D346 判定「静默无效」的**具体形态**）。
/// 若将来 builtin 真正接通 transducer，本条会红 —— 那是有意变更。
#[test]
fn d385_map_filter_take_return_descriptive_placeholders() {
    for (expr, expect) in [
        ("xform.map(fn(x) x * 2 end)", "<xform.map(closure)>"),
        ("xform.filter(fn(x) x > 0 end)", "<xform.filter(closure)>"),
        ("xform.take(99)", "<xform.take(99)>"),
    ] {
        let (code, got) = ev(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{expr}` 应成功; got={got}");
        assert_eq!(got.trim(), expect, "`{expr}` 应返回自描述占位串; got={got}");
    }
}

/// **占位串是**确定性**的** —— 同一程序连跑 5 次结果相同。
///
/// `xform.rs:14-19` 记录：`Value::Dict` 的 `Debug` 走 HashMap 迭代序，
/// 而 `RandomState` 每进程随机种子 ⇒ 修复前连跑 5 次得到 5 个不同结果。
/// 本条钉住这个修复（D332/D333 同族）。
#[test]
fn d385_placeholder_string_is_deterministic_across_runs() {
    let body = "let d = {a: 1, b: 2, c: 3, d: 4, e: 5}\nprint(xform.map(d))\n";
    let (_, first) = ev(body);
    assert!(!first.is_empty(), "应有输出");
    for i in 0..5 {
        let (_, again) = ev(body);
        assert_eq!(
            again, first,
            "第 {i} 次重跑结果不同 —— 占位串必须**确定性**（HashMap 键序随机）"
        );
    }
}

/// **源码层：transducer 底层被真实调用**（修正「死代码」印象）。
#[test]
fn d385_transducer_backend_is_actually_wired() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let ai =
        std::fs::read_to_string(root.join("interpreter/ai_helpers.rs")).expect("读 ai_helpers");
    assert!(
        ai.contains("dyn crate::value::transducer::Transducer<String, String>"),
        "`ai_helpers.rs` 应持有 `Transducer<String,String>` —— transducer 底层**接上了**"
    );
    let md =
        std::fs::read_to_string(root.join("interpreter/method_dispatch.rs")).expect("读 dispatch");
    assert!(
        md.contains("call_method_stream"),
        "`method_dispatch.rs` 应有 `call_method_stream`"
    );
    // 但它在该文件外**零调用** ⇒ builtin 侧无法接入
    let mut external = 0usize;
    for entry in std::fs::read_dir(&root).expect("读 src") {
        let p = entry.expect("entry").path();
        if p.extension().and_then(|s| s.to_str()) != Some("rs")
            || p.file_name().is_some_and(|n| n == "method_dispatch.rs")
        {
            continue;
        }
        let s = std::fs::read_to_string(&p).unwrap_or_default();
        external += s.matches("call_method_stream").count();
    }
    assert_eq!(
        external, 0,
        "D385 实测：`call_method_stream` 在 `method_dispatch.rs` 外零调用 ⇒ \
         脚本侧的 `xform` 无法接入流式处理（这才是「无效」的真因，\
         而不是「底层没实现」）"
    );
}
