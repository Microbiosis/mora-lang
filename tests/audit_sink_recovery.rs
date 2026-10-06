//! v0.104.6 D399 —— `JsonlAuditSink::new` **吞掉尾部损坏**，
//! 把「日志已损坏」静默当成「空文件」（修复轮）
//!
//! ## 缺陷
//!
//! ```rust
//! let (last_hash, events_count) = read_tail_hash(path)
//!     .unwrap_or_else(|_| (Self::GENESIS_HASH.to_string(), 0));   // ← 吞掉**所有**错误
//! ```
//!
//! 而 `read_tail_hash` **自己**在「文件为空」时已返回 `Ok((GENESIS, 0))`
//! ⇒ 那个 fallback **只会在真实错误时触发**（尾部行损坏 / I/O 失败），
//! 等于把「日志尾部损坏」**静默当成「空文件」**。
//!
//! 实测（写 3 条 → 截掉尾部 30 字节，模拟崩溃写半行）：
//!
//! ```text
//! new()       → Ok（未报错）      ← ParseError 被吞
//! event_count → 0                ← 文件里明明有 2 条完整事件，静默算错
//! ```
//!
//! 之后写入的新事件还会以 genesis 为 `prev` 追加 ⇒ **恢复动作本身
//! 进一步污染了链**。
//!
//! 对一套**防篡改**系统，静默归零计数比直接报错糟得多：
//! 真篡改者能藏在「本来就是 0」这种噪声里。
//!
//! ## 为什么「文件不存在」不是需要兜底的情况
//!
//! `new()` 上面刚用 `OpenOptions::new().create(true).append(true).open(path)`
//! **建过**文件 ⇒ `read_tail_hash` 永远拿得到一个已存在的文件
//! ⇒ 空文件走 `Ok((GENESIS, 0))`，非空但损坏走 `Err`。
//! ⇒ 直接 `?` 传播是安全且正确的。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mora::audit::{AuditEvent, AuditSink, JsonlAuditSink};

static SEQ: AtomicU64 = AtomicU64::new(0);

struct Work(PathBuf);

impl Work {
    fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mora_d399_{n}_{tag}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("建临时目录");
        Work(d)
    }
    fn log(&self) -> PathBuf {
        self.0.join("audit.jsonl")
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ev(actor: &str) -> AuditEvent {
    AuditEvent::new(actor, "act", Some("t".into()), None, Some(7))
}

fn write_events(p: &Path, n: usize) {
    let s = JsonlAuditSink::new_fresh(p).expect("new_fresh");
    for i in 0..n {
        s.write(ev(&format!("a{i}"))).expect("write");
    }
    s.flush().expect("flush");
}

// ── ① 核心：尾部损坏必须**报错**，不得静默归零 ──

/// **损坏的尾部 ⇒ `new()` 返回 `Err`**。
///
/// 修前：返回 `Ok` 且 `event_count == 0`（文件里明明有 2 条完整事件）。
#[test]
fn d399_corrupt_tail_must_error_not_reset_to_zero() {
    let w = Work::new("corrupt");
    let p = w.log();
    write_events(&p, 3);
    let before = JsonlAuditSink::new(&p).expect("合法日志应能打开");
    assert_eq!(before.event_count(), 3, "前提：完整日志计数为 3");
    assert!(before.verify_chain().is_ok(), "前提：完整日志链有效");
    drop(before);

    // 截掉尾部 30 字节 —— 模拟崩溃时的「写半行」
    let content = fs::read_to_string(&p).unwrap();
    let truncated: String = content.chars().take(content.len() - 30).collect();
    fs::write(&p, &truncated).unwrap();

    // `JsonlAuditSink` 未实现 `Debug` ⇒ 不能用 `expect_err`，改用 match
    let err = match JsonlAuditSink::new(&p) {
        Ok(_) => panic!("尾部损坏必须报错，不得静默当成空文件"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("parse error") || err.contains("prev/hash"),
        "错误应说明是解析/字段缺失问题; 实得: {err}"
    );
    assert!(
        err.contains("line 3"),
        "错误应**点名出错的行号**（第 3 条被截断）; 实得: {err}"
    );
}

/// **反向对照：文件里的完整事件一条都不能被静默丢弃**。
///
/// 本条与上面配套：若将来有人「优雅降级」成返回 `(GENESIS, 0)`，
/// 本条会抓住「计数与文件实际内容不符」这个后果。
#[test]
fn d399_intact_events_are_never_silently_dropped() {
    let w = Work::new("intact");
    let p = w.log();
    write_events(&p, 3);
    let content = fs::read_to_string(&p).unwrap();
    let truncated: String = content.chars().take(content.len() - 30).collect();
    fs::write(&p, &truncated).unwrap();

    // 完整行数 = 3（末行被截断成**非空但残缺**，`lines()` 仍计它）
    let lines_present = fs::read_to_string(&p)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .count();
    assert_eq!(lines_present, 3, "前提：文件里仍有 3 条非空行（末行残缺）");

    match JsonlAuditSink::new(&p) {
        Ok(s) => {
            // 若将来改成「优雅降级」：它报的计数必须与文件实际非空行数一致
            assert_eq!(
                s.event_count() as usize,
                lines_present,
                "打开成功却报 event_count={}，而文件里有 {lines_present} 条非空行 —— \
                 计数被静默改错了",
                s.event_count()
            );
        }
        Err(e) => {
            // 这是**正确**结果：损坏必须暴露，不能被当成空文件
            assert!(
                e.to_string().contains("parse error"),
                "Err 应来自尾部解析失败; 实得: {e}"
            );
        }
    }
}

// ── ② 反向对照：合法路径**不得**被误伤 ──

/// **空文件** ⇒ `new()` 成功、计数 0（这是被传播的 `Err` 唯一不该影响的情形）。
#[test]
fn d399_empty_file_opens_as_genesis() {
    let w = Work::new("empty");
    let p = w.log();
    fs::write(&p, "").unwrap();
    let s = JsonlAuditSink::new(&p).expect("空文件应能打开（genesis）");
    assert_eq!(s.event_count(), 0);
    assert!(s.verify_chain().is_ok(), "空日志链校验应通过");
    // 写第一条后链应有效
    s.write(ev("first")).expect("write");
    s.flush().expect("flush");
    assert!(s.verify_chain().is_ok(), "首条写入后链应有效");
}

/// **不存在的文件** ⇒ `new()` 自行创建并以 genesis 启动。
#[test]
fn d399_absent_file_is_created() {
    let w = Work::new("absent");
    let p = w.log();
    assert!(!p.exists(), "前提：文件尚不存在");
    let s = JsonlAuditSink::new(&p).expect("不存在的文件应被创建");
    assert!(p.exists(), "文件应已创建");
    assert_eq!(s.event_count(), 0);
    s.write(ev("x")).expect("write");
    s.flush().expect("flush");
    assert!(s.verify_chain().is_ok());
}

/// **已有合法日志** ⇒ 追加打开并**正确恢复** `last_hash` 与计数。
///
/// 这是 `new()` 的**正路**，本轮改动不得破坏它。
#[test]
fn d399_append_to_valid_log_recovers_count_and_chain() {
    let w = Work::new("append");
    let p = w.log();
    write_events(&p, 3);

    let s = JsonlAuditSink::new(&p).expect("合法日志应能追加打开");
    assert_eq!(s.event_count(), 3, "应从尾部恢复出 3 条");
    s.write(ev("post")).expect("追加写");
    s.flush().expect("flush");
    assert_eq!(s.event_count(), 4);
    assert!(
        s.verify_chain().is_ok(),
        "追加后整条链仍应有效（prev 正确接续）"
    );

    // 重开一次，计数应继续累积
    let s2 = JsonlAuditSink::new(&p).expect("重开");
    assert_eq!(s2.event_count(), 4, "重开后应恢复出 4 条");
    assert!(s2.verify_chain().is_ok());
}

// ── ③ 防篡改核心能力不变量（本轮改动不得削弱） ──

/// **改动中间一行 ⇒ 链校验必须失败**。
///
/// 这是整套 hash 链存在的理由，独立于本轮改动 —— 钉住它防止「修了 A 坏了 B」。
#[test]
fn d399_tampering_middle_line_is_detected() {
    let w = Work::new("tamper");
    let p = w.log();
    write_events(&p, 4);
    assert!(
        JsonlAuditSink::new(&p).unwrap().verify_chain().is_ok(),
        "前提：原始日志链有效"
    );

    // 把第 2 行的 actor 改掉（保持 JSON 合法、只改内容）
    let content = fs::read_to_string(&p).unwrap();
    let tampered = content.replacen("\"a1\"", "\"aX\"", 1);
    assert_ne!(tampered, content, "篡改应真的改了内容");
    fs::write(&p, &tampered).unwrap();

    let s = JsonlAuditSink::new(&p).expect("被篡改的日志仍能打开");
    let err = s.verify_chain().expect_err("改动中间一行必须被链校验抓到");
    assert!(
        err.to_string().contains("line 2"),
        "错误应点名被篡改的**物理行号（1 基）**; 实得: {err}"
    );
}

/// **行号基准三处必须收敛到 1 基**（收敛钉）。
///
/// D399 修前的实测矛盾：**同一个损坏文件**，
/// - `new()` → `read_tail_hash` 报 `line 3`（1 基计数器）
/// - `verify_chain` 报 `line 1`（`enumerate()` 的 0 基下标）
///
/// 运维对着同一个文件拿到互相矛盾的指引。既有单测的断言消息
/// 「tamper at line 1 should fail at line 1」也证明**本意是 1 基**。
///
/// 本条把「同一性质用同一基准」钉死，任一处回退到 0 基都会红。
#[test]
fn d399_line_numbers_are_one_based_everywhere() {
    // ① `verify_chain`：篡改第 2 个物理行 ⇒ 报 line 2
    let w = Work::new("base1");
    let p = w.log();
    write_events(&p, 4);
    let content = fs::read_to_string(&p).unwrap();
    fs::write(&p, content.replacen("\"a1\"", "\"aX\"", 1)).unwrap();
    let err = JsonlAuditSink::new(&p)
        .unwrap()
        .verify_chain()
        .expect_err("篡改应被抓到")
        .to_string();
    assert!(
        err.contains("line 2"),
        "① verify_chain 应报 1 基行号; 实得 {err}"
    );

    // ② `read_tail_hash`：损坏第 3 个物理行 ⇒ 报 line 3（与 ① 同一基准）
    let w2 = Work::new("base2");
    let p2 = w2.log();
    write_events(&p2, 3);
    let c2 = fs::read_to_string(&p2).unwrap();
    let cut: String = c2.chars().take(c2.len() - 30).collect();
    fs::write(&p2, &cut).unwrap();
    let msg = match JsonlAuditSink::new(&p2) {
        Ok(_) => panic!("损坏尾部必须报错"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("line 3"),
        "② read_tail_hash 应报 1 基行号，与 ① 同基准; 实得 {msg}"
    );
}

/// **删除中间一行 ⇒ 链校验必须失败**（prev 接不上）。
#[test]
fn d399_deleting_a_line_is_detected() {
    let w = Work::new("delete");
    let p = w.log();
    write_events(&p, 4);

    let content = fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    let kept: String = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| format!("{l}\n"))
        .collect();
    fs::write(&p, &kept).unwrap();

    let s = JsonlAuditSink::new(&p).expect("删行后仍能打开");
    assert!(
        s.verify_chain().is_err(),
        "删除中间一行必须破坏链（prev 接不上）"
    );
}
