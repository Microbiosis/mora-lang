//! v0.104.6 D184：`mora record list` 的 EVENTS 列用**原始行数**，与
//! `mora record stats` 对同一文件给出**不同的数**（已修）。
//!
//! ## 缺陷
//!
//! D178 给 **7 个**下游消费者都加了「跳过行」告警，但 `mora record list`
//! 是**唯一漏掉**的 —— 它还额外用了一个**不同的计数口径**。
//!
//! 修前 `list_recordings` 做两件独立的事：
//!
//! ```text
//! event_count = count_lines(&path)        // ← 原始非空行数
//! (first, last) = load_time_range(&path)  // ← 整文件解析后只取时间戳
//! ```
//!
//! 列名是 `EVENTS`，暗示与 `stats` 的 `Events: N total` 同义。实测一份
//! 首行被截断的录像：
//!
//! ```text
//! $ mora record list      →  r2 … EVENTS 3        ← 数的是行
//! $ mora record stats r2  →  [warn] 1 of 3 line(s) could not be parsed…
//!                        →  Events: 2 total       ← 数的是可读事件
//! ```
//!
//! 同一个文件，两个命令报 3 和 2，而 `list` **零告警**。
//!
//! ## 修法：合并成**一次**加载，零额外成本
//!
//! 关键观察：`load_time_range` **本来就**调用 `load_jsonl` 把整个文件
//! 解析一遍，然后**只**留下首末时间戳、把事件全丢掉。
//! 既然文件已经解析过了，事件数与跳过行就是**同一次加载的副产品**。
//!
//! 故新增 `load_summary()` 一次返回
//! `(可读事件数, 首 ts, 末 ts, 跳过行)`，并删掉 `count_lines` 与
//! `load_time_range`。**解析次数不变**（仍是每文件一次），
//! 而两者对同一文件**必然一致** —— 结构上不可能再漂移。
//!
//! ## 判据
//!
//! 主判据是**跨命令一致性**：`list` 的 EVENTS 必须等于 `stats` 的 Events。
//! 钉的是「同一个事实只有一种算法」，而不是某个具体数字 —— 那样才挡得住
//! 将来任何一边的改动。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d184_{tag}"));
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
    fn recording(&self, name: &str) -> PathBuf {
        self.0
            .join(".mora")
            .join("recordings")
            .join(format!("{name}.jsonl"))
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
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// 录一份，返回其事件数（以 `mora record` 自己的报告为准）。
fn record(dir: &WorkDir, name: &str) {
    let script = dir.write(
        "a.mora",
        "let x = ai.chat(p\"hello there for the test\")\nprint(x)\n",
    );
    let out = mora(dir.path(), &["record", script.to_str().unwrap(), name]);
    assert!(out.contains("recorded"), "录制 {} 应成功:\n{}", name, out);
}

/// 把录像首行截成半行（模拟「进程被 kill，写到一半」）。
fn truncate_first_line(dir: &WorkDir, name: &str) {
    let p = dir.recording(name);
    let body = std::fs::read_to_string(&p).expect("读录像");
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
    assert!(lines.len() >= 2, "前提：录像应至少 2 行");
    let cut = lines[0].len() / 2;
    let cut = lines[0]
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= cut)
        .last()
        .unwrap_or(0);
    lines[0] = lines[0][..cut].to_string();
    std::fs::write(&p, lines.join("\n")).expect("写坏录像");
}

/// 从 `mora record list` 表格里取某录制名的 EVENTS 数。
fn events_column(list_out: &str, name: &str) -> usize {
    list_out
        .lines()
        .find(|l| l.trim_start().starts_with(name))
        .unwrap_or_else(|| panic!("list 输出里没有 `{name}` 行:\n{}", list_out))
        .split_whitespace()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("解析不出 `{name}` 的 EVENTS 列:\n{}", list_out))
}

/// 从 `mora record stats` 的 `Events:  N total` 里取 N。
fn stats_events(stats_out: &str) -> usize {
    stats_out
        .split("Events:")
        .nth(1)
        .and_then(|seg| seg.split("total").next())
        .and_then(|seg| seg.trim().parse::<usize>().ok())
        .unwrap_or_else(|| panic!("解析不出 stats 的事件数:\n{}", stats_out))
}

/// **主判据（有牙齿）**：`list` 与 `stats` 对同一文件必须给出**相同的数**。
///
/// 修前：完好文件两者一致（3/3），**损坏**文件却是 3 vs 2 —— 故必须
/// 覆盖「损坏」这一路，否则该测试在缺陷存在时照样绿。
#[test]
fn d184_list_and_stats_agree_including_on_corrupt_files() {
    let dir = WorkDir::new("agree");
    record(&dir, "ok");
    record(&dir, "bad");
    truncate_first_line(&dir, "bad");

    let list = mora(dir.path(), &["record", "list"]);
    for name in ["ok", "bad"] {
        assert_eq!(
            events_column(&list, name),
            stats_events(&mora(dir.path(), &["record", "stats", name])),
            "[{name}] `record list` 的 EVENTS 必须等于 `record stats` 的 Events —— \
             修前损坏文件是 3 vs 2:\n{}",
            list
        );
    }
}

/// 损坏的录像必须在 `list` 里**显式告警**（D178 之后不留沉默的消费者）。
#[test]
fn d184_list_warns_about_skipped_lines() {
    let dir = WorkDir::new("warn");
    record(&dir, "bad");
    truncate_first_line(&dir, "bad");

    let list = mora(dir.path(), &["record", "list"]);
    assert!(
        list.contains("could not be parsed") && list.contains("SKIPPED"),
        "`record list` 必须对跳过行告警 —— 它是 D178 之后唯一沉默的消费者:\n{}",
        list
    );
}

/// 完好文件不得产生告警（防「一律报警」这种无信息量的做法）。
#[test]
fn d184_list_is_quiet_on_intact_recordings() {
    let dir = WorkDir::new("quiet");
    record(&dir, "ok");

    let list = mora(dir.path(), &["record", "list"]);
    assert!(
        !list.contains("could not be parsed"),
        "完好录像不该有解析告警:\n{}",
        list
    );
    assert_eq!(
        events_column(&list, "ok"),
        3,
        "完好录像应有 3 个事件:\n{}",
        list
    );
}
