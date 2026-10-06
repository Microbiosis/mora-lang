//! v0.104.6 D117：README 的代码示例**可解析性普查**（用户最先读的文档）。
//!
//! ## 为什么做这件事
//!
//! D110 在 spec 上验证了这套方法有效（53 块 / 22 坏 → 查出 D111 与 D116）。
//! `README.md` 是**用户接触语言的第一份文档**，且它**在版本控制内**
//! （与未被跟踪的 spec 不同），示例与实现脱节会直接误导使用者。
//!
//! ## 本文件**真的读 README**
//!
//! D108 第一版只硬编码源码、从不打开 spec，把 spec 改回去测试照样全绿 ——
//! 那不是「一致性测试」，只是语法冒烟。

use mora::parser_v3::ParserV3;

/// 提取 README 里所有 ```mora … ``` 代码块。
fn readme_mora_blocks() -> Vec<(usize, String)> {
    let text = std::fs::read_to_string("README.md").expect("读 README.md");
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "```mora" {
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && lines[j].trim() != "```" {
                j += 1;
            }
            out.push((start + 1, lines[start..j].join("\n")));
            i = j;
        }
        i += 1;
    }
    out
}

#[test]
fn d117_census_of_readme_mora_blocks() {
    let blocks = readme_mora_blocks();
    eprintln!("README 中 ```mora 代码块数: {}", blocks.len());
    assert!(
        !blocks.is_empty(),
        "从 README 里一个 ```mora 块都没提取到 —— 提取逻辑坏了（不是 README 变了）"
    );

    let mut bad = Vec::new();
    for (line, src) in &blocks {
        if let Err(e) = ParserV3::compile(src) {
            let first = e.lines().next().unwrap_or("").to_string();
            bad.push(format!("README 行 {line}: {first}"));
            eprintln!(
                "  README:{line} 不可解析 — {first}\n    |{}|",
                src.lines().next().unwrap_or("")
            );
        }
    }
    eprintln!(
        "D117 不可解析清单（{}/{}）：\n  {}",
        bad.len(),
        blocks.len(),
        bad.join("\n  ")
    );
}

/// 反向对照：解析器确实能解析合法 Mora。
#[test]
fn d117_parser_actually_works() {
    assert!(ParserV3::compile("let x = 1\nprint(x)\n").is_ok());
    assert!(ParserV3::compile("let = = =\n").is_err());
}
