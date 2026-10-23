//! v0.104.6 D478：执行入口的**命名一致性**守卫 —— 旧名 `run_mir` 不得复活。
//!
//! ## 背景
//!
//! v0.104.6 D467 已把主入口 `run_mir` 更名为 `execute`（旧名暗示存在
//! 「线性执行器」，而自 v0.59 起仓库里**只有** DAG/BSP 一种执行实现）。
//! D478 把残留收干净：`run_mir_dag` → `execute_program`、`run_mir_fn` →
//! `execute_fn`、两个测试文件改名、约 200 处注释引用改指**现存的**名字。
//!
//! ## 本守卫守什么
//!
//! ① **代码里不得出现旧名**：任何非注释代码不得含 `run_mir` 词元 ——
//!    防止「改个名又长回来」或「新增 API 时沿用旧前缀」。
//! ② **注释里提到旧名必须说明它是旧名**：旧名只允许出现在**明确记载历史**
//!    的段落里（标记词见 [`HISTORY_MARKERS`]）—— 防止读者 grep 到一个
//!    不存在的函数名而困惑。
//! ③ **文件名不得带旧前缀** —— 文件名也是 API 表面。
//!
//! ② 的判定按**连续注释段**（同一段落）而不是按行：历史记档常写成
//! 「以下原文是 …」再引一段，标记词在段首、引用在段中。
//!
//! ## 例外
//!
//! 本文件自身被排除（它必须提到旧名才能守它）。

use std::fs;
use std::path::{Path, PathBuf};

/// 旧名词元。
const NEEDLE: &str = "run_mir";

/// 记档标记词：注释里提到旧名时，同段落内必须出现其一。
const HISTORY_MARKERS: [&str; 11] = [
    "原名",
    "旧名",
    "旧承诺",
    "此前",
    "曾",
    "历史",
    "v0.59",
    "D467",
    "迁入",
    "原 `",
    "原 ",
];

/// 本文件自身（它必须提到旧名才能守它）。
const SELF: &str = "execute_naming_hygiene.rs";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// 递归收集 `dir` 下所有 `.rs`。
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// 把一行拆成 `(代码部分, 注释部分)`。
///
/// 字符串字面量先剔除 —— 否则消息文本里的旧名会被当成代码标识符。
fn split_line(line: &str) -> (String, String) {
    let mut code = String::new();
    let mut in_str = false;
    let mut escaped = false;
    let mut comment_at = None;
    let bytes: Vec<char> = line.chars().collect();
    for (i, &c) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '/' if bytes.get(i + 1) == Some(&'/') => {
                comment_at = Some(i);
                break;
            }
            _ => {}
        }
        code.push(c);
    }
    match comment_at {
        Some(i) => (code, bytes[i..].iter().collect()),
        None => (code, String::new()),
    }
}

/// 连续注释段：返回每段（Vec<行>）。
fn comment_runs(text: &str) -> Vec<Vec<String>> {
    let mut runs: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for line in text.lines() {
        let (code, comment) = split_line(line);
        if code.trim().is_empty() && !comment.trim().is_empty() {
            cur.push(comment);
        } else {
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

#[test]
fn no_live_code_carries_the_old_run_mir_prefix() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["src", "tests"] {
        collect(&root.join(dir), &mut files);
    }
    let mut hits = Vec::new();
    for p in &files {
        if p.file_name().and_then(|s| s.to_str()) == Some(SELF) {
            continue;
        }
        let text = fs::read_to_string(p).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let (code, _) = split_line(line);
            if code.contains(NEEDLE) {
                hits.push(format!(
                    "{}:{}: {}",
                    p.strip_prefix(&root).unwrap_or(p).to_string_lossy(),
                    n + 1,
                    code.trim()
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "**代码**里出现旧名 `{}`（{} 处）—— 旧名暗示存在「线性执行器」，\
         而自 v0.59 起仓库里只有 DAG/BSP 一种执行实现。\n\
         现存入口是 `execute`（执行一个 MirFunction）/ `execute_program`\
         （执行整个程序）/ `run_with_signal*`（引擎层）。\n\
         若确实是**历史记档**，请把它写进注释并加标记词{:?}。\n\
         命中：\n{}",
        NEEDLE,
        hits.len(),
        &HISTORY_MARKERS[..4],
        hits.join("\n")
    );
}

#[test]
fn old_name_in_comments_must_be_marked_as_history() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["src", "tests"] {
        collect(&root.join(dir), &mut files);
    }
    let mut hits = Vec::new();
    for p in &files {
        if p.file_name().and_then(|s| s.to_str()) == Some(SELF) {
            continue;
        }
        let text = fs::read_to_string(p).unwrap_or_default();
        for (ri, run) in comment_runs(&text).iter().enumerate() {
            if !run.iter().any(|l| l.contains(NEEDLE)) {
                continue;
            }
            let marked = run
                .iter()
                .any(|l| HISTORY_MARKERS.iter().any(|m| l.contains(m)));
            if !marked {
                hits.push(format!(
                    "{}: 注释段 #{} —— {}",
                    p.strip_prefix(&root).unwrap_or(p).to_string_lossy(),
                    ri + 1,
                    run.first().map(|s| s.trim()).unwrap_or("")
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "注释里提到旧名 `{}` 却**没说明它是旧名**（{} 段）—— \
         读者 grep 到一个不存在的函数名会困惑。\n\
         加标记词{:?}之一即可（例：旧名 `{}` / 原名 `{}`）。\n\
         命中：\n{}",
        NEEDLE,
        hits.len(),
        &HISTORY_MARKERS[..4],
        NEEDLE,
        NEEDLE,
        hits.join("\n")
    );
}

#[test]
fn no_file_name_carries_the_old_prefix() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["src", "tests"] {
        collect(&root.join(dir), &mut files);
    }
    let hits: Vec<String> = files
        .iter()
        .filter(|p| p.file_name().and_then(|s| s.to_str()) == Some(SELF))
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(NEEDLE))
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert!(
        hits.is_empty(),
        "文件名带旧名前缀 `{}`：{:?} —— 文件名也是 API 表面",
        NEEDLE,
        hits
    );
}
