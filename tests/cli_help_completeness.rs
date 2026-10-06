//! v0.104.6 D185：`--help` 的用法列表**漏掉 6 个能跑的子命令**（已修）。
//!
//! ## 缺陷
//!
//! `main.rs` 的顶层分派认得这些子命令，`--help` 的用法列表里却没有：
//!
//! | 子命令 | 分派有？ | 修前 `--help` 有用法条目？ |
//! |---|---|---|
//! | `mora run <file>` | ✅ | ❌ |
//! | `mora install <url>` | ✅ | ❌ |
//! | `mora snapshot <file> <name> [--update]` | ✅ | ❌（仅标题行顺带提到） |
//! | `mora record export <name>` | ✅ | ❌ |
//! | **`mora record audit <name>`** | ✅ | ❌ |
//! | `mora record report <name>` | ✅ | ❌ |
//!
//! ## 为什么不是「排版小事」
//!
//! `mora record audit` 是**密钥扫描器** —— 用户问「我这份录像里有没有泄漏
//! API key」时，唯一能回答问题的命令。它在 `--help` 的**标题行与用法列表
//! 里都没有出现过**（`audit` 二字在整段 help 里零匹配）。
//!
//! 对照本会话修过的东西：这个命令在 D177 里刚补上 `bearer-token` 死条目、
//! D180 刚补上「不生效规则必须点名」、D178 刚让它在数据残缺时**硬失败**。
//! 一堆安全保证，用户却找不到入口。
//!
//! `mora record` 参数不足时自报的用法（`main.rs`）也只写了
//! `list|stats|timeline ...`，同样漏了 export/audit/report。
//!
//! ## 判据
//!
//! **每一个能跑的子命令都必须在 `--help` 里出现。**
//! 这条判据的好处是它**对未来也成立**：将来新增一个子命令却忘了登记，
//! 改这一张表就会被当场抓到。

use std::process::Command;

/// 全部**能跑**的子命令（顶层 + `record` 子命令）。
/// 新增子命令时**必须**同时加进这里，否则本测试会红。
const SUBCOMMANDS: &[&str] = &[
    // 顶层
    "run",
    "install",
    "snapshot",
    "record",
    "replay",
    "diff",
    "mcp",
    // record 子命令
    "record list",
    "record stats",
    "record timeline",
    "record export",
    "record audit",
    "record report",
];

fn help_text() -> String {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("--help")
    .output()
    .expect("跑 mora --help");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **主判据**：每个能跑的子命令都必须在 `--help` 的**用法行**里出现。
///
/// 只看「字符串出现过」不够 —— `snapshot` 在标题行里就出现过，
/// 那正是修前的情况。要求它出现在**以 `mora ` 开头的用法行**上。
#[test]
fn d185_every_subcommand_appears_in_help_usage_lines() {
    let help = help_text();
    let usage_lines: Vec<&str> = help
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("mora "))
        .collect();
    assert!(!usage_lines.is_empty(), "解析不到任何用法行:\n{}", help);

    let mut missing: Vec<&str> = Vec::new();
    for cmd in SUBCOMMANDS {
        // 形如 `mora record audit <name> …` 或 `mora snapshot <file> …`
        let needle = format!("mora {cmd}");
        if !usage_lines.iter().any(|l| l.starts_with(&needle)) {
            missing.push(cmd);
        }
    }
    assert!(
        missing.is_empty(),
        "这些子命令能跑，却没有 `--help` 用法条目:\n{:?}\n\n\
         `mora record audit` 尤其重要 —— 它是**密钥扫描器**，\
         修前在整段 help 里 `audit` 二字零匹配。\n\n{}",
        missing,
        help
    );
}

/// `mora record` 参数不足时自报的用法，同样必须涵盖全部子命令。
///
/// 那是用户**真的打错命令时**会看到的那一行。
#[test]
fn d185_record_usage_error_lists_all_subcommands() {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("record")
    .output()
    .expect("跑 mora record");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));

    assert!(
        text.contains("Usage"),
        "`mora record`（缺参数）应自报用法:\n{}",
        text
    );
    for sub in ["list", "stats", "timeline", "export", "audit", "report"] {
        assert!(
            text.contains(sub),
            "`mora record` 的用法行漏了 `{}`:\n{}",
            sub,
            text
        );
    }
}

/// 负对照：`--help` 仍应列出原有的那些条目（防止「重写时把该有的删了」）。
#[test]
fn d185_help_still_lists_the_original_entries() {
    let help = help_text();
    for needle in [
        "mora <file.mora>",
        "mora --check <file>",
        "mora record <file> <name>",
        "mora replay <file> <name>",
        "mora diff <a> <b>",
        "mora mcp tool-list",
        "mora --version",
        "mora --help",
    ] {
        assert!(
            help.lines().any(|l| l.trim().starts_with(needle)),
            "原有条目 `{}` 不见了:\n{}",
            needle,
            help
        );
    }
}
