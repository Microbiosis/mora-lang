//! v0.104.6 D225：`mora record stats` 的事件分解**看起来穷尽、实则不穷尽** ——
//! `Msg` / `StateMutation` 被计入总数却不显示在任何一行（已修）。
//!
//! ## 缺陷
//!
//! `Events: N total` 下面列了 `ai.chat` / `web.fetch` / `notes` 三行，读起来像
//! 一份**穷尽**分类。但 `Event` 有**五个**变体（`AiChat` / `WebFetch` / `Note` /
//! `Msg` / `StateMutation`），后两类**没有任何一行**。
//!
//! 真实 `mora record stats` 实测（`baseline.jsonl` 的 18 条**全是**
//! `state_mutation`）：
//!
//! ```text
//! Events:        18 total
//!   ai.chat:     0
//!   web.fetch:   0
//!   notes:       0        ← 0+0+0 ≠ 18，零提示
//! ```
//!
//! 用户据此会读成「录了 18 次调用却一次都没成功」，而真相是**18 次状态变更**。
//!
//! markdown 报告更不穷尽 —— 它连 `notes` 都没有，只列 `AI calls` / `Web calls`。
//!
//! ## 修法
//!
//! `RecordingStats` 增 `msg_count` / `state_mutation_count`，在
//! `compute_stats` 的循环里计数（`Msg`/`StateMutation` 本就不进
//! latency/tokens 统计 —— 那是 v0.83 的正确决定，缺的只是**计数**），
//! 并在 CLI 与 markdown 报告里各加一行。子类之和**恒等于** total。
//!
//! ## 判据
//!
//! 判据不写死「某一行是几」，而写成**可验证的不变式**：
//! **各子类之和 == `Events: N total`**。这样将来 `Event` 再加新变体、
//! 分解又漏掉一类时，判据会立刻红 —— 而写死具体数字做不到这件事。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d225_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".mora").join("recordings")).expect("建目录");
        WorkDir(d)
    }
    /// 写一条录制，每类事件若干条；返回写入的事件总数。
    fn write(&self, name: &str, counts: &[(&str, usize)]) -> usize {
        let mut body = String::new();
        let mut id = 1u32;
        for (kind, n) in counts {
            for _ in 0..*n {
                let line = match *kind {
                    "ai.chat" => format!(
                        "{{\"kind\":\"ai.chat\",\"id\":{id},\"ts_ms\":{ts},\"model\":\"m\",\
                         \"prompt_hash\":\"0\",\"prompt_preview\":\"p\",\"response\":\"r\",\
                         \"tokens_in\":2,\"tokens_out\":3,\"latency_ms\":7,\"error\":null,\
                         \"arg_signature\":\"ai.chat(model: string, prompt: string) -> string\"}}\n",
                        id = id,
                        ts = 1_700_000_000_000u128 + id as u128
                    ),
                    "web.fetch" => format!(
                        "{{\"kind\":\"web.fetch\",\"id\":{id},\"ts_ms\":{ts},\"url\":\"u\",\
                         \"method\":\"GET\",\"status\":200,\"latency_ms\":5,\"error\":null,\
                         \"body_len\":3}}\n",
                        id = id,
                        ts = 1_700_000_000_000u128 + id as u128
                    ),
                    "note" => format!(
                        "{{\"kind\":\"note\",\"id\":{id},\"ts_ms\":{ts},\"text\":\"n\"}}\n",
                        id = id,
                        ts = 1_700_000_000_000u128 + id as u128
                    ),
                    "msg" => format!(
                        "{{\"kind\":\"msg\",\"id\":{id},\"ts_ms\":{ts},\"channel\":\"c\",\
                         \"payload\":null,\"prior_state_hash\":0}}\n",
                        id = id,
                        ts = 1_700_000_000_000u128 + id as u128
                    ),
                    _ => format!(
                        "{{\"kind\":\"state_mutation\",\"id\":{id},\"ts_ms\":{ts},\
                         \"var\":\"v\",\"old\":null,\"new\":1,\"prior_state_hash\":0}}\n",
                        id = id,
                        ts = 1_700_000_000_000u128 + id as u128
                    ),
                };
                body.push_str(&line);
                id += 1;
            }
        }
        let p = self
            .0
            .join(".mora")
            .join("recordings")
            .join(format!("{name}.jsonl"));
        std::fs::write(&p, body).expect("写录制");
        counts.iter().map(|(_, n)| *n).sum::<usize>()
    }
    /// 跑 `mora record stats <name>`，返回 `(输出, 事件子类之和)`。
    fn stats(&self, name: &str) -> (String, usize) {
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .arg("record")
        .arg("stats")
        .arg(name)
        .current_dir(&self.0)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora record stats");
        let raw = String::from_utf8_lossy(&out.stdout).into_owned();
        let keep = raw
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("Mora v")
                    && !t.starts_with("AI:")
                    && !t.starts_with("AI 原语")
                    && !t.starts_with("显式 API")
                    && !t.starts_with("Trait 系统")
                    && !t.starts_with("Built-in")
                    && !t.starts_with("v0.15 CLI")
                    && !t.starts_with('⚠')
            })
            .map(str::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        debug_assert!(parse_num(&keep, "Events:") > 0 || keep.contains("Events:"));
        // 分解行 = 缩进 + `名字:` + **纯数字**。
        // ⚠ 必须按「最后一个冒号之后能否解析成整数」来筛：
        //   `  ai.chat:     2`  → 2   （事件子类，要计入）
        //   `  min:         7ms` → 解析失败（延迟，不计入）
        //   `  avg/call:    2 in + 3 out` → 解析失败（不计入）
        // 若改成「以冒号结尾」之类的形状匹配，`sub` 会**恒空** ⇒ 断言恒真 ⇒
        // 判据形同虚设（D212 的教训：判据可能形同虚设而不自知）。
        let sub: Vec<usize> = keep
            .lines()
            .filter(|l| l.starts_with("  "))
            .filter_map(|l| l.rsplit(':').next()?.trim().parse::<usize>().ok())
            .collect();
        (keep, sub.iter().sum::<usize>())
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn parse_num(s: &str, key: &str) -> usize {
    s.lines()
        .find_map(|l| l.trim().strip_prefix(key))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// **主判据（有牙齿）**：子类之和必须**恒等于** `Events: N total`。
///
/// 修前：`state_mutation` 18 条的录制给出 `0 + 0 + 0 ≠ 18`，且零提示。
#[test]
fn d225_event_breakdown_sums_to_the_total() {
    let dir = WorkDir::new("sum");
    let cases: Vec<(&str, Vec<(&str, usize)>)> = vec![
        ("only_state", vec![("state_mutation", 5)]),
        ("only_msg", vec![("msg", 4)]),
        (
            "mixed",
            vec![
                ("ai.chat", 3),
                ("web.fetch", 2),
                ("note", 1),
                ("msg", 2),
                ("state_mutation", 6),
            ],
        ),
        ("only_note", vec![("note", 7)]),
    ];
    for (name, counts) in cases {
        let expected = dir.write(name, &counts);
        let (out, sum) = dir.stats(name);
        assert_eq!(
            sum, expected,
            "[D225] 事件分解**不穷尽** —— 各子类之和 {sum} != `Events: … total` {expected}。\n\
             修前 `Msg` / `StateMutation` 被计入总数却不显示在任何一行，用户会把\
             「全是状态变更」读成「一次调用都没成功」。\n{out}"
        );
    }
}

/// **不回归**：总数与错误数仍要正确（`errors` 不属于事件分类，不得混入子类和）。
#[test]
fn d225_total_and_errors_stay_correct() {
    let dir = WorkDir::new("total");
    let expected = dir.write("mix", &[("ai.chat", 2), ("state_mutation", 3)]);
    let (out, sum) = dir.stats("mix");
    assert!(
        out.contains(&format!("Events:        {expected} total")),
        "{out}"
    );
    assert!(out.contains("Errors:        0"), "{out}");
    assert_eq!(sum, expected, "子类之和应等于总数：\n{out}");
    // tokens / latency 仍只由 ai.chat / web.fetch 贡献（v0.83 的决定）
    assert!(out.contains("Tokens:        4 in + 6 out"), "{out}");
    assert!(out.contains("Latency:       14ms total"), "{out}");
}
