//! v0.104.6 D183：`ai.tokens()` 在 **mock 模式（默认）下恒为 0** —— 而 record
//! 家族对同一次调用报出非零 token（已修）。
//!
//! ## 缺陷
//!
//! `track_tokens` 是运行期填充 `token_usage` 的**唯一**入口
//! （`AiRuntime::record_tokens` 只被单测调用），而它**只被真实 HTTP 响应
//! 路径的两处**调用（`ai_chat.rs` 解析 API 回包 `usage` 之后）。
//! 两条 mock 分支（`mock_llm` 队列 / 通用兜底）都不调它 ——
//! 而**没有 `OPENAI_API_KEY` 正是本地开发者的默认形态**。
//!
//! 于是 `ai.tokens().input / output / total / calls` **恒为 0**。
//!
//! ## 为什么不能只说「mock 下没有真 token」
//!
//! 因为 record 家族对**同一次调用**用**同一套估算**（`len / 4`）把 token
//! 记进了录像。实测同一个程序：
//!
//! ```text
//! $ mora t.mora              →  ai.tokens().total = 0.0
//! $ mora record t.mora tk    →  ✓ recorded
//! $ mora record stats tk     →  Tokens: 17 in + 22 out = 39 total
//! ```
//!
//! **两套子系统对同一个事实给出两个答案**，而运行期那个是**静默**的 0。
//! `ai.tokens()` 在本语言里是 agent 查成本的入口（D76 刚补好它的 typeck
//! 签名与 `calls` 字段），恒 0 等于告诉 agent「你从没花过钱」。
//!
//! ## 修法与安全前提
//!
//! 两条 mock 分支补上 `track_tokens(输入估算, 输出估算)`，与 recorder
//! 用**同一套** `len / 4` 估算 —— 两边从此一致。
//!
//! **调用它是安全的**（已逐条核过，不是想当然）：
//! - `track_tokens` 里的预算强制（`per_call` / `total` / `alert_threshold`）
//!   **不可能触发** —— `AiRuntime` 只有 `Default`（`token_budget: None`，
//!   是全仓**唯一**的赋值点），`TokenBudget` 结构体**从未被构造**，也没有
//!   任何 setter；
//! - 且 `with budget = …` 会**立即报错**（spec §11.1「承诺但未实现」，
//!   D39/D116 已记）—— 那是处理未实现特性的**正确**方式，本轮不动。
//!
//! 故补这一调用**只**更新计数器与 trace 指标，**不引入任何新的失败模式**。
//!
//! ## 明确**不**改的（按 D166 纪律查过，已否证）
//!
//! - **token 预算功能本身**：spec §11.1 + D39/D116 已明确记「承诺但未实现」，
//!   且当前**写入即报错**（实测：`with-config \`budget\` is promised by spec §11.1
//!   but not implemented yet`）。那是**诚实**的处理，不是缺陷。本轮只记档。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d183_{tag}"));
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

fn mora(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(dir)
    .args(args)
    // 显式清掉：本机真有 key 就会走进另一条分支，测不到本缺陷。
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// 从输出里取最后一个「纯数字」行（mock 模式下 `print(t.total)` 的形态）。
fn last_number(out: &str) -> f64 {
    out.lines()
        .filter_map(|l| l.trim().parse::<f64>().ok())
        .next_back()
        .unwrap_or_else(|| panic!("输出里没有纯数字行:\n{}", out))
}
const ONE_CALL: &str = "let a = ai.chat(p\"count my tokens, this is a reasonably long prompt\")\n\
                        let t = ai.tokens()\nprint(t.total)\n";
const TWO_CALLS: &str = "let a = ai.chat(p\"first call with a decent length prompt for testing\")\n\
                         let b = ai.chat(p\"second call with a decent length prompt for testing\")\n\
                         let t = ai.tokens()\nprint(t.calls)\n";

/// **主判据（有牙齿）**：调过一次 `ai.chat` 后 `ai.tokens().total` 必须非零。
///
/// 修前这里恒为 0.0。
#[test]
fn d183_ai_tokens_total_is_populated_in_mock_mode() {
    let dir = WorkDir::new("total");
    let script = dir.write("t.mora", ONE_CALL);
    let out = mora(dir.path(), &[script.to_str().unwrap()]);
    let total = last_number(&out);
    assert!(
        total > 0.0,
        "mock 模式下 `ai.tokens().total` 恒为 0 —— agent 会以为从没花过钱:\n{}",
        out
    );
}

/// `calls` 必须等于实际调用次数（1 次 → 1，2 次 → 2）。
///
/// 这是 D76 刚补好 `TokenUsage::calls` 字段的那个数字；修前它连 mock
/// 的一次调用都数不到。
#[test]
fn d183_ai_tokens_calls_counts_real_calls() {
    let dir = WorkDir::new("calls");
    let s1 = dir.write(
        "c1.mora",
        "let a = ai.chat(p\"only one call here with a long enough prompt\")\n\
         let t = ai.tokens()\nprint(t.calls)\n",
    );
    let o1 = mora(dir.path(), &[s1.to_str().unwrap()]);
    assert_eq!(last_number(&o1), 1.0, "一次调用后 calls 应为 1:\n{}", o1);

    let s2 = dir.write("c2.mora", TWO_CALLS);
    let o2 = mora(dir.path(), &[s2.to_str().unwrap()]);
    assert_eq!(last_number(&o2), 2.0, "两次调用后 calls 应为 2:\n{}", o2);
}

/// **两套子系统必须一致**：运行期 `ai.tokens().total` 与
/// `mora record stats` 对**同一个程序**给出同一个数字。
///
/// 这是本轮的核心论据 —— 修前一个是 0、另一个是 39。
#[test]
fn d183_runtime_counters_agree_with_record_family() {
    let dir = WorkDir::new("agree");
    let script = dir.write("t.mora", ONE_CALL);

    let runtime = last_number(&mora(dir.path(), &[script.to_str().unwrap()]));

    let rec = mora(dir.path(), &["record", script.to_str().unwrap(), "tk"]);
    assert!(rec.contains("recorded"), "录制应成功:\n{}", rec);
    let stats = mora(dir.path(), &["record", "stats", "tk"]);

    // stats 形如 `Tokens:  17 in + 22 out = 39 total`
    let total = stats
        .split("Tokens:")
        .nth(1)
        .and_then(|seg| seg.split("total").next())
        .and_then(|seg| seg.split('=').next_back())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or_else(|| panic!("解析不出 stats 的 token 合计:\n{}", stats));

    assert_eq!(
        runtime, total,
        "同一程序的两种 token 计数必须一致 —— 修前运行期是 0、record 家族是 39"
    );
}

/// **负对照**：一次都没调 `ai.chat` 时，计数器必须是 0。
///
/// 守的是「修复没有把计数器无条件置成非零」。
#[test]
fn d183_ai_tokens_stays_zero_before_any_call() {
    let dir = WorkDir::new("zero");
    let script = dir.write(
        "z.mora",
        "let t = ai.tokens()\nprint(t.calls)\nprint(t.total)\n",
    );
    let out = mora(dir.path(), &[script.to_str().unwrap()]);
    let nums: Vec<f64> = out
        .lines()
        .filter_map(|l| l.trim().parse::<f64>().ok())
        .collect();
    assert_eq!(
        nums,
        vec![0.0, 0.0],
        "没调用过 ai.chat 时 calls/total 必须都是 0:\n{}",
        out
    );
}
