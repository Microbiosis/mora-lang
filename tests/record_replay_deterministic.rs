//! v0.104.6 D181：`mora replay` **从不重放** —— 它在重跑程序，两端键还都对不上（已修）。
//!
//! ## 这是本仓库最严重的一条
//!
//! `mora --help` 写的是 `mora replay <file> <name>  Replay recording (deterministic)`，
//! README 把 Record / replay / diff 列为「deterministic AI-call regression testing」。
//! 而实测：**`ai.chat` 的重放在任何情况下都匹配不上录像**。
//!
//! ## 两个独立缺陷
//!
//! 1. **查找不可达。** `lookup_ai_chat` 只写在 `real_ai_chat` 里，而
//!    `real_ai_chat` 只有 `OPENAI_API_KEY` **非空**时才会被调用。
//!    没有 key（本地常态，也正是「重放」唯一合理的场景）**永远走不到查找**。
//! 2. **键两端不一致。** 查找用 `prompt_text`（`"user: find me"`，带 role 前缀），
//!    而 `record_ai_chat` 存的是**裸 prompt**（`"find me"`），
//!    `hash_prompt` 必然不等。
//!
//! 两条合起来，两种环境都失败，而**失败方式相反**：
//!
//! ## 修前实测（真实 CLI，录像 response 刻意选一个可辨识的值）
//!
//! ```text
//! # 无 key（本地常态）
//! $ mora replay play.mora rr
//! [Mock response for: find me]              ← 新 mock，录像里的值没用上
//! ✓ replayed 3 events from …rr.jsonl         ← 还报告「成功」
//!
//! # 有 key（把 base_url 指到必然连不上的本地端口，不产生外发请求）
//! $ OPENAI_API_KEY=… MORA_AI_BASE_URL=http://127.0.0.1:9 mora replay play.mora rr
//! Runtime error during replay: ai.chat: network error connecting to
//!   http://127.0.0.1:9/chat/completions: Connection refused
//! ```
//!
//! 后者尤其恶劣：**它根本不查录像，直接发真实网络请求** ——
//! 与「deterministic」正好相反，还要按次计费。
//!
//! ## 修法
//!
//! 把查找上移到 `do_ai_chat` 的 mock/real **分支之前**，键与录制端**逐字一致**
//! （`effective_model` + 裸 `prompt`），两条分支共用。
//! `real_ai_chat` 里那个够不着的查找删掉（避免第二份真相）。
//!
//! ## 对照：`web.fetch` 的重放一直是**正确**接线的
//!
//! `real_web_fetch` 的 `lookup_web_fetch` 就在函数开头、网络调用之前，
//! 且用 `url` 直接做键 —— 与 `ai.chat` 正好不对称。**同一家族里，
//! 一个接对了、一个接错了**，这本身就是「不要以为同族行为一致」的例子。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d181_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, file: &str, body: &str) -> PathBuf {
        let p = self.0.join(file);
        std::fs::write(&p, body).expect("写文件");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 录像里那个刻意可辨识的响应值。
const RECORDED: &str = "DISTINCTIVE_RECORDED_VALUE_12345";

/// 跑 `mora <args>`。`api_key` 为 `Some` 时附带 key 与一个**必然连不上的**
/// 本地 base_url —— 目的正是「若它敢发网络请求，这里会立刻炸」，
/// 而**不会产生任何外发流量**。
fn mora(dir: &Path, args: &[&str], api_key: Option<&str>) -> (String, i32) {
    let mut cmd = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ));
    cmd.current_dir(dir).args(args);
    match api_key {
        Some(k) => {
            cmd.env("OPENAI_API_KEY", k);
            cmd.env("MORA_AI_BASE_URL", "http://127.0.0.1:9");
        }
        None => {
            // 显式清掉：本机可能真有 key，那会把测试推进「有 key」分支。
            cmd.env_remove("OPENAI_API_KEY");
            cmd.env_remove("MORA_AI_BASE_URL");
        }
    }
    let out = cmd.output().expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 录一份：prompt 为 `find me`，响应刻意用 [`RECORDED`]。
fn record_recording(dir: &WorkDir, name: &str) {
    let script = dir.write(
        "rec.mora",
        &format!(
            "with mock_llm = [\"{RECORDED}\"]\n  let a = ai.chat(p\"find me\")\n  print(a)\nend\n"
        ),
    );
    let (out, code) = mora(
        dir.path(),
        &["record", script.to_str().unwrap(), name],
        None,
    );
    assert_eq!(code, 0, "录制应成功: {}", out);
    assert!(out.contains("recorded"), "应录到事件: {}", out);
}

/// 不带 mock 队列的播放脚本 —— 修前它只会拿到 `[Mock response for: …]`。
fn write_player(dir: &WorkDir, prompt: &str) -> PathBuf {
    dir.write(
        "play.mora",
        &format!("let a = ai.chat(p\"{prompt}\")\nprint(a)\n"),
    )
}

/// **主判据（有牙齿）**：无 key 时重放**必须命中录像**。
///
/// 修前这里输出 `[Mock response for: find me]`。
#[test]
fn d181_replay_uses_the_recorded_response_in_mock_mode() {
    let dir = WorkDir::new("nokey");
    record_recording(&dir, "rr");
    let player = write_player(&dir, "find me");

    let (out, code) = mora(
        dir.path(),
        &["replay", player.to_str().unwrap(), "rr"],
        None,
    );
    assert_eq!(code, 0, "重放应成功: {}", out);
    assert!(
        out.contains(RECORDED),
        "重放必须返回**录像里的**响应，而不是新 mock 的:\n{}",
        out
    );
    assert!(
        !out.contains("[Mock response for:"),
        "重放不该跑出新的 mock 响应:\n{}",
        out
    );
}

/// **最恶劣的那条**：有 key 时也**必须**命中录像，且**不得**发网络请求。
///
/// 修前这里报 `network error connecting to …/chat/completions` ——
/// 即完全不查录像、直接发真实请求，与「deterministic」相反还要计费。
#[test]
fn d181_replay_never_hits_the_network_when_a_recording_matches() {
    let dir = WorkDir::new("withkey");
    record_recording(&dir, "rr");
    let player = write_player(&dir, "find me");

    let (out, code) = mora(
        dir.path(),
        &["replay", player.to_str().unwrap(), "rr"],
        Some("sk-fake-key-for-d181-test-000000"),
    );
    assert_eq!(code, 0, "命中录像不该有任何错误（更不该联网）: {}", out);
    assert!(
        out.contains(RECORDED),
        "有 key 时也应返回录像里的响应:\n{}",
        out
    );
    assert!(
        !out.contains("network error") && !out.contains("Connection refused"),
        "命中录像却发了网络请求 —— deterministic 重放最忌讳的事:\n{}",
        out
    );
}

/// **负对照**：录像里**没有**的 prompt 不得被错放，应落回 mock。
///
/// 守的是「修复没有把所有 ai.chat 都变成返回录像里的那个值」。
#[test]
fn d181_replay_falls_back_to_mock_for_an_unrecorded_prompt() {
    let dir = WorkDir::new("miss");
    record_recording(&dir, "rr");
    let player = write_player(&dir, "a completely different prompt");

    let (out, code) = mora(
        dir.path(),
        &["replay", player.to_str().unwrap(), "rr"],
        None,
    );
    assert_eq!(code, 0, "未录到的 prompt 应落回 mock 并正常结束: {}", out);
    assert!(
        !out.contains(RECORDED),
        "未录到的 prompt 不该返回别的 prompt 的响应:\n{}",
        out
    );
    assert!(
        out.contains("[Mock response for:"),
        "未录到的 prompt 应走 mock 兜底:\n{}",
        out
    );
}

/// `with model = …` 时，录制端与重放端必须用**同一个** model 做键。
///
/// 修前 mock 分支记的是 `model` **参数**、real 分支记 `effective_model` ——
/// 配了 `with model` ���二者可能不同，键就对不上。
#[test]
fn d181_replay_matches_when_a_with_block_sets_the_model() {
    let dir = WorkDir::new("withmodel");
    let script = dir.write(
        "rec.mora",
        &format!(
            "with model = \"my-model\", mock_llm = [\"{RECORDED}\"]\n  \
             let a = ai.chat(p\"find me\")\n  print(a)\nend\n"
        ),
    );
    let (out, code) = mora(
        dir.path(),
        &["record", script.to_str().unwrap(), "rr"],
        None,
    );
    assert_eq!(code, 0, "录制应成功: {}", out);

    // 播放脚本也用 `with model` 指向同一个模型。
    let player = dir.write(
        "play.mora",
        "with model = \"my-model\"\n  let a = ai.chat(p\"find me\")\n  print(a)\nend\n",
    );
    let (out, code) = mora(
        dir.path(),
        &["replay", player.to_str().unwrap(), "rr"],
        None,
    );
    assert_eq!(code, 0, "重放应成功: {}", out);
    assert!(
        out.contains(RECORDED),
        "`with model` 形态下也必须命中录像（model 键要一致）:\n{}",
        out
    );
}
