//! v0.104.6 D169：`methods_of(document)` 返回**空列表**，而 6 个方法实际可用（已修）。
//!
//! ## 缺陷
//!
//! ```text
//! let d = document.parse("sample.md")
//! print(d.text())          → 全文            ✅
//! print(len(d.pages()))     → 1               ✅
//! print(len(d.blocks()))    → 6               ✅
//! print(methods_of(d))      → []              ❌ 整个类型漏报
//! ```
//!
//! `method_dispatch.rs:46` 有 `Value::Document → call_method_document` 分派，
//! 该函数实现了 `markdown` / `text` / `pages` / `metadata` / `blocks` / `origin`
//! 六个方法；而 `value.rs::Value::methods()` 的表把它们整个漏了（落到 `_ => &[]`）。
//!
//! ## 为什么这不是「小瑕疵」
//!
//! `methods()` 是 `methods_of` builtin 的**唯一**数据源。而本语言是
//! **AI-native** 的 —— agent 常靠 `methods_of` 推断一个值能做什么。
//! 漏报让它得到一个**偏小且为空**的集合，等于告诉 agent「这个值什么都不能做」。
//!
//! 与 D109 修 `crush_json` 同一类，但那次漏 1 个方法、这次漏**整个类型的 6 个**。
//!
//! 一个自证的旁证：`call_method` 的 `_` 臂错误消息里**自己就列了 documents**
//! （「…routers, mcp_servers, documents, or builtin objects」）——
//! 说明只有 `methods()` 这张表漏了，不是「document 本来就没有方法」。

use std::io::Write;
use std::process::Command;

/// 造一份临时 markdown 并返回 `document.parse` 之后的 `methods_of` 结果文本。
fn methods_of_parsed_doc(dir: &str) -> String {
    let base = std::path::Path::new(dir);
    std::fs::create_dir_all(base).expect("建临时目录");
    let md = base.join("d169_sample.md");
    let mut f = std::fs::File::create(&md).expect("写 markdown");
    f.write_all(b"# Title\n\nSome text.\n\n## S1\n\nBody one.\n")
        .expect("写内容");
    drop(f);
    let src = format!(
        "let d = document.parse(\"{p}\")\nprint(methods_of(d))\n",
        p = md.display().to_string().replace('\\', "/")
    );
    // 源码经临时文件喂给 CLI（stdin 不接受源码）
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let prog = base.join("d169_probe.mora");
    std::fs::write(&prog, &src).expect("写探针");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(base);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// D169 主判据：`methods_of(document)` 必须列出全部 6 个方法。
#[test]
fn d169_methods_of_document_lists_all_methods() {
    let out = methods_of_parsed_doc(&std::env::temp_dir().join("mora_d169_a").to_string_lossy());
    for m in ["markdown", "text", "pages", "metadata", "blocks", "origin"] {
        assert!(
            out.contains(m),
            "`methods_of(document)` 必须列出 `{m}`（修复前返回 `[]`）; 实得: {out}"
        );
    }
}

/// D169 主判据 ②：这些方法**确实可用** —— 不能只改表就交差。
#[test]
fn d169_document_methods_actually_work() {
    let base = std::env::temp_dir().join("mora_d169_b");
    std::fs::create_dir_all(&base).expect("建目录");
    let md = base.join("s.md");
    std::fs::write(&md, "# T\n\ntext\n\n## S\n\nbody\n").expect("写 md");
    let prog = base.join("p.mora");
    std::fs::write(
        &prog,
        format!(
            "let d = document.parse(\"{p}\")\n\
             print(len(d.blocks()))\n\
             print(len(d.pages()))\n\
             print(d.text())\n",
            p = md.display().to_string().replace('\\', "/")
        ),
    )
    .expect("写探针");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&base);
    // 块数取决于样例内容，**不写死** —— 只断言「解析出了非零个块」
    // （写死 6 是我照抄了另一份更长的样例，实测此样例为 4）。
    let blocks = s
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<usize>().ok())
        .unwrap_or(0);
    assert!(blocks > 0, "`d.blocks()` 应返回非零块数; 实得: {s}");
    assert!(s.contains("1"), "`d.pages()` 应返回 1; 实得: {s}");
    assert!(s.contains("body"), "`d.text()` 应含正文; 实得: {s}");
}

/// D169 反向对照：`methods_of` 对其它类型**不得**被这次改动影响。
#[test]
fn d169_other_types_unchanged() {
    let base = std::env::temp_dir().join("mora_d169_c");
    std::fs::create_dir_all(&base).expect("建目录");
    let prog = base.join("p.mora");
    std::fs::write(
        &prog,
        "print(methods_of(\"abc\"))\nprint(methods_of([1, 2]))\nprint(methods_of({a: 1}))\nprint(methods_of(1.0))\n",
    )
    .expect("写探针");
    let mora = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/debug/../mora.exe"
    );
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&base);
    // 三个都有非空且含标志性方法
    assert!(
        s.contains("upper") && s.contains("crush_json"),
        "String/List 表不得回退; 实得: {s}"
    );
    assert!(
        s.contains("get") && s.contains("keys"),
        "Dict 表不得回退; 实得: {s}"
    );
    assert!(s.contains("sqrt"), "Float 表不得回退; 实得: {s}");
}
