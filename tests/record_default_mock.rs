//! v0.104.6 D173：`mora record` 在**默认 mock 模式**下记不下任何 `ai.chat`（已修）。
//!
//! ## 缺陷
//!
//! `interpreter/ai_chat.rs` 的 mock 路径有**两条**分支：
//!
//! | 分支 | 条件 | 是否 `record_ai_chat` |
//! |---|---|---|
//! | `mock_llm` 队列 | `with mock_llm = [...]` 且队列非空 | ✅ 记 |
//! | **通用兜底** | 其余一切 mock 调用 | ❌ **不记**（原先直接 `return`） |
//!
//! 而「其余一切」正是**默认形态** —— 本地没有 `OPENAI_API_KEY` 时，
// `ai.chat` 一律走兜底分支。实测（真实 `mora record`）：
//!
//! ```text
//! let a = ai.chat(p"hello")                        → 录制 2 个事件，全是 state_mutation
//! with mock_llm = ["mocked answer"] + ai.chat(…)   → 录制 3 个事件，含 1 个 ai.chat
//! ```
//!
//! ## 为什么严重
//!
//! `mora --help` 标称 `record <file> <name>  Record ai.chat/web.fetch`，
//! README 也把 **Record / replay / diff** 列为
//! 「deterministic AI-call regression testing」。而默认形态下：
//!
//! | 子命令 | 修复前表现 |
//! |---|---|
//! | `mora record` | 文件里只有 `state_mutation`，**0 个 AI 调用** |
//! | `mora replay` | 无内容可放 —— 只是又跑了一遍 mock，输出自然「一致」 |
//! | `mora diff a b` | 只在比 `state_mutation` |
//! | `mora record stats` | `web.fetch: 0`、`Tokens: 0 in + 0 out` |
//! | `mora record timeline` | 两行全是 `state_mutation`，没有 `ai.chat` |
//!
//! 整条「确定性 AI 调用回归测试」的卖点在**默认配置下**失效。
//!
//! ## 修法
//!
//! 兜底分支与 `mock_llm` 分支**对称**地记一次：token 估算同规则
//! （`len / 4`）、mock 延迟记 0、`arg_signature` 用同一条可读化签名。

use std::path::{Path, PathBuf};
use std::process::Command;

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

/// 在独立工作目录里跑一段 Mora，返回 stdout。避免污染仓库的 `.mora/recordings`。
fn run_in(dir: &Path, src: &str) -> (String, String, i32) {
    std::fs::create_dir_all(dir).expect("建工作目录");
    let prog = dir.join("p.mora");
    std::fs::write(&prog, src).expect("写脚本");
    let out = Command::new(mora_exe())
        .current_dir(dir)
        .arg(&prog)
        .output()
        .expect("跑 mora");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// 跑 `mora record <file> <name>`（在 `dir` 下），返回 (stdout, stderr, exit)。
///
/// ⚠ `recording_path()` 取的是**相对 CWD** 的 `.mora/recordings/`，不是相对脚本
/// 所在目录 —— 实测在仓库根跑 `mora record <tmp>\p.mora zz` 时，文件落到了
/// **仓库的** `.mora/recordings/zz.jsonl`。故这里必须 `current_dir(dir)`，
/// 否则测试会污染工作区。
fn record_in(dir: &Path, name: &str, src: &str) -> (String, String, i32) {
    std::fs::create_dir_all(dir).expect("建工作目录");
    let prog = dir.join("p.mora");
    std::fs::write(&prog, src).expect("写脚本");
    let out = Command::new(mora_exe())
        .current_dir(dir)
        .arg("record")
        .arg(&prog)
        .arg(name)
        .output()
        .expect("跑 mora record");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn recording_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(".mora")
        .join("recordings")
        .join(format!("{name}.jsonl"))
}

fn read_recording(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(recording_path(dir, name)).expect("读录制文件")
}

/// D173 主判据：默认 mock 形态（**无** `mock_llm`）也必须录到 `ai.chat` 事件。
#[test]
fn d173_default_mock_ai_chat_is_recorded() {
    let dir = std::env::temp_dir().join("mora_d173_a");
    let _ = std::fs::remove_dir_all(&dir);
    let (out, err, code) = record_in(&dir, "a", "let a = ai.chat(p\"hello\")\nprint(a)\n");
    assert_eq!(code, 0, "record 应成功; stdout={out} stderr={err}");
    let jsonl = read_recording(&dir, "a");
    assert!(
        jsonl.contains("\"kind\":\"ai.chat\""),
        "默认 mock 形态（无 `mock_llm`）必须录到 ai.chat 事件 —— \
         修复前录制文件里只有 state_mutation，整条 record/replay/diff 卖点失效; 实得: {jsonl}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D173 主判据 ②：`mock_llm` 分支**不得回退**（它本来就对）。
#[test]
fn d173_mock_llm_branch_still_recorded() {
    let dir = std::env::temp_dir().join("mora_d173_b");
    let _ = std::fs::remove_dir_all(&dir);
    let (out, err, code) = record_in(
        &dir,
        "b",
        "with mock_llm = [\"mocked answer\"]\n  let a = ai.chat(p\"hello\")\n  print(a)\nend\n",
    );
    assert_eq!(code, 0, "record 应成功; stdout={out} stderr={err}");
    let jsonl = read_recording(&dir, "b");
    assert!(
        jsonl.contains("\"kind\":\"ai.chat\"") && jsonl.contains("mocked answer"),
        "`mock_llm` 分支必须仍然录到 ai.chat 及其 response; 实得: {jsonl}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D173 主判据 ③：两条分支录到的**字段结构**必须一致（否则 stats 无法统一统计）。
#[test]
fn d173_both_mock_branches_record_same_fields() {
    let keys = [
        "\"kind\"",
        "\"id\"",
        "\"ts_ms\"",
        "\"model\"",
        "\"prompt_hash\"",
        "\"prompt_preview\"",
        "\"response\"",
        "\"tokens_in\"",
        "\"tokens_out\"",
        "\"latency_ms\"",
    ];
    for (label, src, tag) in [
        ("default", "let a = ai.chat(p\"hello\")\nprint(a)\n", "k1"),
        (
            "mock_llm",
            "with mock_llm = [\"mocked answer\"]\n  let a = ai.chat(p\"hello\")\n  print(a)\nend\n",
            "k2",
        ),
    ] {
        let dir = std::env::temp_dir().join(format!("mora_d173_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        record_in(&dir, tag, src);
        let jsonl = read_recording(&dir, tag);
        let line = jsonl
            .lines()
            .find(|l| l.contains("\"kind\":\"ai.chat\""))
            .unwrap_or_else(|| panic!("[{label}] 应有 ai.chat 事件; 实得: {jsonl}"));
        for k in keys {
            assert!(
                line.contains(k),
                "[{label}] ai.chat 事件缺少字段 `{k}` —— 两条 mock 分支的记录结构必须一致; 实得: {line}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// D173 反向对照：**普通运行**（不带 record）不得留下录制文件。
///
/// 否则这条修复会把「跑一下脚本」的副作用扩散到磁盘。
#[test]
fn d173_plain_run_writes_no_recording() {
    let dir = std::env::temp_dir().join("mora_d173_plain");
    let _ = std::fs::remove_dir_all(&dir);
    let (out, err, code) = run_in(&dir, "let a = ai.chat(p\"hello\")\nprint(a)\n");
    assert_eq!(code, 0, "普通运行应成功; stdout={out} stderr={err}");
    assert!(
        !recording_path(&dir, "").exists() && !dir.join(".mora").join("recordings").exists(),
        "普通运行不得创建 `.mora/recordings`（`record_ai_chat` 的 `mode.is_record()` 门控必须仍然生效）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
