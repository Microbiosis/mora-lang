//! v0.104.6 D224：`mora record list` 的 **LAST MODIFIED** 列显示的是
//! **最后一条事件的时间戳**，不是文件 mtime（已修）。
//!
//! ## 缺陷
//!
//! `cli/record.rs` 表头写着 `LAST MODIFIED`，打的却是
//! `format_ts(info.last_ts_ms)`；`record/analysis.rs` 的排序键同样是
//! `last_ts_ms`（注释写「最新在前」）。
//!
//! 「这份录制**什么时候存在**」与「它记录的**最后一条事件**什么时候发生」
//! 是**两件事** —— 跨机器录制、导入旧录制、时钟偏移都会让两者分叉。
//!
//! 真实 `mora record list` 实测（一份**刚写**的文件，事件时间戳指向 2023-11）：
//!
//! ```text
//! NAME          SIZE  EVENTS   LAST MODIFIED
//! d224_probe     3KB  12       1053d ago     ← 修前
//! d224_probe     3KB  12       2min ago     ← 修后（真实文件 mtime）
//! ```
//!
//! 排序同理：修前把「内容时间最新」当成「最近录制」。
//!
//! ## 修法
//!
//! `RecordingInfo` 增 `modified_ms`（取自 `fs::metadata().modified()`，
//! 该 metadata 本来就为了取 size 而读，**零额外 I/O**），
//! 列与排序键都改用它 —— 让表头名副其实。
//!
//! ## 判据
//!
//! 写一份**事件时间戳很旧、文件刚创建**的录制，`mora record list` 显示的
//! 相对时间必须**很小**。修前会显示上千天。
//!
//! 隔离：`recordings_dir()` = `current_dir()/.mora/recordings`，
//! 故把 cwd 设成测试临时目录，**不碰**用户真实的 `.mora/recordings`。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d224_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".mora").join("recordings")).expect("建目录");
        WorkDir(d)
    }
    fn write_recording(&self, name: &str, ts_ms: u128) {
        let p = self
            .0
            .join(".mora")
            .join("recordings")
            .join(format!("{name}.jsonl"));
        let line = format!(
            "{{\"kind\":\"ai.chat\",\"id\":1,\"ts_ms\":{ts_ms},\"model\":\"m\",\
             \"prompt_hash\":\"0000000000000000\",\"prompt_preview\":\"p\",\
             \"response\":\"r\",\"tokens_in\":1,\"tokens_out\":1,\"latency_ms\":0,\
             \"arg_signature\":\"ai.chat(model: string, prompt: string) -> string\"}}\n"
        );
        std::fs::write(&p, line).expect("写录制");
    }
    fn list(&self) -> String {
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .arg("record")
        .arg("list")
        .current_dir(&self.0)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora record list");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **主判据（有牙齿）**：`LAST MODIFIED` 必须反映**文件**，不是文件**内容**。
///
/// 事件时间戳取 2023-11（约 1000 天前），文件**刚**创建 —— 显示的相对时间
/// 必须很小。
#[test]
fn d224_last_modified_column_reflects_the_file_not_its_events() {
    let dir = WorkDir::new("column");
    // 2023-11-14 ≈ 1000 天前
    dir.write_recording("old_events", 1_700_000_000_000);
    let out = dir.list();
    let line = out
        .lines()
        .find(|l| l.contains("old_events"))
        .unwrap_or_else(|| panic!("list 输出里没有 old_events：\n{out}"));
    assert!(
        !line.contains("d ago") || line.contains("0d ago"),
        "[D224] `LAST MODIFIED` 显示了 `{line}` —— 那是**最后一条事件**的时间\
         （2023-11），而文件是**刚**写的。表头说的是「最近修改」，指的应是文件 mtime。"
    );
    assert!(
        line.contains("just now") || line.contains("min ago") || line.contains("s ago"),
        "[D224] `LAST MODIFIED` 应是「刚 / 几分钟前」量级，实得：`{line}`"
    );
}

/// **排序键**同源：按**文件** mtime 排，而不是事件时间。
#[test]
fn d224_listing_is_sorted_by_file_mtime_not_event_time() {
    let dir = WorkDir::new("order");
    // 两个文件的 mtime 相反于它们的事件时间
    dir.write_recording("newer_file_old_events", 1_700_000_000_000);
    dir.write_recording("older_file_new_events", 1_800_000_000_000);
    // 让第二个文件**更旧**：把它的 mtime 往前调
    let older = dir
        .0
        .join(".mora")
        .join("recordings")
        .join("older_file_new_events.jsonl");
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    set_mtime(&older, t);

    let out = dir.list();
    let pos_new_file = out.find("newer_file_old_events").expect("缺 newer_file");
    let pos_old_file = out.find("older_file_new_events").expect("缺 older_file");
    assert!(
        pos_new_file < pos_old_file,
        "[D224] 排序按的是**事件时间**（older_file 的事件更新）⇒ 它排在了前面。\
         表头/排序说的是「最近修改」，应按**文件 mtime**。\n{out}"
    );
}

/// 改文件 mtime（Windows 上 `set_last_write_time` 生效）。
fn set_mtime(p: &std::path::Path, t: std::time::SystemTime) {
    let cmd = format!(
        "[System.IO.File]::SetLastWriteTime('{}', [DateTime]::FromFileTimeUtc({}))",
        p.display(),
        t.duration_since(std::time::UNIX_EPOCH)
            .expect("时间在 epoch 之后")
            .as_nanos() as i64
            / 100
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &cmd])
        .output();
}
