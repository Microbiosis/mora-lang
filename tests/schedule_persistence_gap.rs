//! v0.104.6 D395 —— `src/schedule/`：**「持久化」只写不读，生产上从不写**
//! （否定轮 + 一处文档更正 + 两项待裁决）
//!
//! ## 结论：`tick` 逻辑本身正确，但「持久化」是**名义上的**
//!
//! | 层 | 事实 |
//! |---|---|
//! | 底层写入 | `save()` 完整手写 JSON 序列化器 —— **存在** |
//! | 读回 | **全仓没有**任何 scheduler 的 `load` / `from_json` —— **不存在** |
//! | 中间接线 | `set_persist_path` **全仓唯一调用点是单测**（`schedule/mod.rs:399`）|
//! | 上层入口 | 无 —— 生产 `persist_path` 恒 `None` ⇒ `save()` 直接跳过 |
//!
//! ⇒ **生产上永不落盘**；即便落了盘也无人读。实测确认脚本跑完后
//! 工作目录下**不存在** `.mora_schedule.json`。
//!
//! 而模块 doc 写着「持久化到 `<cwd>`/`.mora_schedule.json`」——
//! **两处都不成立**（D365 型注释漂移，本轮已更正）。
//!
//! ## `tick` 逻辑：否定轮，**零缺陷**
//!
//! 桶索引（`BTreeMap<next_fire, ids>`）、`range(..=now)` 取到期桶、
//! `Every` 按 `now + interval` 重排、`At` 触发即删、`remove` 后惰性清理
//! —— 逐条核对**全部正确**，既有 7 条单测方向也对。
//!
//! ## 顺带钉住两条**潜伏陷阱**（当前不可达，故不修）
//!
//! 1. **`interval_s == 0` 的 `Every` job 会静默永久停摆**：
//!    `tick` 里 `if job.interval_s > 0 { … }` **没有 else**，
//!    而到期桶已被 `buckets.remove(k)` 取走 ⇒ job 留在 `jobs` 里
//!    却再无桶引用 ⇒ 永不触发。`add` 会拒掉 0，但**没有 loader** ⇒ 当前不可达。
//! 2. **`delete_after_run` 字段是装饰性的**：`tick` 从不读它，
//!    `save()` 也从不写它。当前 `At` 恒删、`Every` 恒留恰好等价于
//!    `delete_after_run = (kind == At)`（`add` 的默认值）⇒ **无可观察后果**。

use std::path::Path;
use std::process::Command;

use mora::schedule::{JobKind, Scheduler};

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

fn run_fixture(name: &str) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/e2e")
        .join(name);
    let out = Command::new(exe)
        .args(["run", script.to_str().expect("路径转字符串")])
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

/// 剥掉注释后返回其余部分 —— **整行注释**与**行尾注释**都要剥。
///
/// ⚠ 扫源码前**必须**剥注释，否则判据会被**自己写的注释**命中。
/// 本轮连踩两次：
///   - D388：字符串切片带上相邻 item 的 doc comment；
///   - D394：行尾注释 `// delete_after_run`（在一条 `assert_eq!` 行尾）；
///   - 本轮：同 D394，行尾注释让出现次数从 2 变成 3。
fn code_only(s: &str) -> String {
    s.lines()
        .map(|l| {
            let t = l.trim();
            if t.starts_with("//") {
                return "";
            }
            // 剥行尾 `//`（本文件涉及的源码里字符串不含 `//`）
            l.split("//").next().unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── ① `tick` 语义矩阵（否定轮：全对） ──

/// **`Every`**：到期触发、并按 `now + interval` 重排。
#[test]
fn d395_every_fires_at_interval_and_reschedules() {
    let s = Scheduler::new();
    s.add("j", JobKind::Every, "msg", 60, 0)
        .expect("add 应成功");
    let t0 = Scheduler::now();
    assert!(s.tick(t0).is_empty(), "首次 tick 未到期，不应触发");
    assert!(s.tick(t0 + 59).is_empty(), "差 1 秒不应触发");
    assert_eq!(s.tick(t0 + 60), vec!["msg".to_string()], "到点应触发");
    assert_eq!(
        s.tick(t0 + 120),
        vec!["msg".to_string()],
        "再一个周期应再触发"
    );
    assert_eq!(s.count(), 1, "`Every` 不应被删");
}

/// **`At`**：到点触发一次并被删除。
#[test]
fn d395_at_fires_once_then_removed() {
    let s = Scheduler::new();
    let target = Scheduler::now() + 100;
    s.add("once", JobKind::At, "boom", 0, target)
        .expect("add 应成功");
    assert!(s.tick(target - 1).is_empty(), "未到点不应触发");
    assert_eq!(s.count(), 1, "未到点不应被删");
    assert_eq!(s.tick(target), vec!["boom".to_string()]);
    assert_eq!(s.count(), 0, "`At` 触发后应删除");
    assert!(
        s.tick(target + 10_000).is_empty(),
        "已删除的 job 不得再触发（桶内 id 惰性清理）"
    );
}

/// **反向对照**：未到期的 job **一次都不能触发**。
#[test]
fn d395_no_early_trigger() {
    let s = Scheduler::new();
    s.add("j", JobKind::Every, "msg", 3600, 0)
        .expect("add 应成功");
    for t in [0, 1, 60, 1800, 3599] {
        assert!(
            s.tick(Scheduler::now() + t).is_empty(),
            "在 {t}s 处提前触发了"
        );
    }
}

/// **`add` 的参数校验矩阵**。
#[test]
fn d395_add_validation_matrix() {
    let s = Scheduler::new();
    assert!(s.add("", JobKind::Every, "m", 60, 0).is_err(), "空名应拒");
    assert!(s.add("n", JobKind::Every, "", 60, 0).is_err(), "空消息应拒");
    assert!(
        s.add("n", JobKind::Every, "m", 0, 0).is_err(),
        "`Every` 的 interval_s=0 应拒"
    );
    assert!(
        s.add("n", JobKind::At, "m", 0, 0).is_err(),
        "`At` 的 at_epoch=0 应拒"
    );
    assert!(
        s.add("n", JobKind::At, "m", 0, Scheduler::now() - 1)
            .is_err(),
        "`At` 的 at_epoch 在过去应拒"
    );
    assert_eq!(s.count(), 0, "全部被拒后不应留下 job");
}

/// **`remove` 往返**（含 double-remove）。
#[test]
fn d395_remove_roundtrip() {
    let s = Scheduler::new();
    let id = s.add("j", JobKind::Every, "m", 60, 0).expect("add 应成功");
    assert_eq!(s.count(), 1);
    assert!(s.remove(&id), "首次 remove 应成功");
    assert!(!s.remove(&id), "二次 remove 应返回 false");
    assert_eq!(s.count(), 0);
}

// ── ② 持久化缺口（核心发现） ──

/// **`set_persist_path` 生产侧零调用** ⇒ `persist_path` 恒 `None`。
#[test]
fn d395_persist_path_has_no_production_caller() {
    let mut callers = Vec::new();
    for rel in [
        "src/schedule/mod.rs",
        "src/interpreter/builtins/schedule.rs",
        "src/runtime/infra.rs",
    ] {
        for (i, line) in read(rel).lines().enumerate() {
            let t = line.trim();
            if t.contains("set_persist_path(") && !t.contains("pub fn set_persist_path") {
                callers.push(format!("{rel}:{}: {t}", i + 1));
            }
        }
    }
    assert_eq!(
        callers.len(),
        1,
        "`set_persist_path` 应恰好 1 处调用且**只在单测里**; 实得 {callers:?} —— \
         多出来的若在生产路径，持久化结论需重写"
    );
    assert!(
        callers[0].contains("src/schedule/mod.rs:"),
        "唯一调用点应位于 schedule/mod.rs 内; 实得 {}",
        callers[0]
    );
    // ⚠ **不钉绝对行号** —— 文档编辑会让它漂移（399 → 424 已发生过）。
    //   钉「位于 `#[cfg(test)]` 之后」这个**事实**即可。
    let cfg_test_at = read("src/schedule/mod.rs")
        .find("#[cfg(test)]")
        .expect("应能找到单测区起点");
    let call_at = read("src/schedule/mod.rs")
        .find("set_persist_path(path")
        .expect("应能找到调用点");
    assert!(
        call_at > cfg_test_at,
        "唯一调用点应落在 `#[cfg(test)]` 之后（即只在单测里）; \
         call_at={call_at} cfg_test_at={cfg_test_at}"
    );
}

/// **全仓没有 scheduler 的读回函数**。
#[test]
fn d395_no_loader_exists() {
    let src = code_only(&read("src/schedule/mod.rs"));
    for banned in ["fn load", "fn from_json", "fn restore", "fn read_jobs"] {
        assert!(
            !src.contains(banned),
            "出现了 `{banned}` —— 读回路径已实现，持久化结论需重写"
        );
    }
}

/// **`save()` 确实存在**（反向对照：不能只断言「没有」）。
#[test]
fn d395_save_writer_exists() {
    let src = code_only(&read("src/schedule/mod.rs"));
    assert!(
        src.contains("fn save(&self)"),
        "`save()` 应仍在（写入侧存在）"
    );
    assert!(
        src.contains("std::fs::write(path, json)"),
        "`save()` 应确实落盘（只是生产路径到不了）"
    );
}

// ── ③ 两条潜伏陷阱（源码级现状钉） ──

/// **`tick` 里 `interval_s > 0` 没有 else** ⇒ 0 间隔 job 会静默永久停摆。
///
/// 当前**不可达**（`add` 拒绝 0，且没有 loader 能造出这种 job），
/// 故只钉现状、不修。
#[test]
fn d395_zero_interval_job_would_stall_silently() {
    let src = code_only(&read("src/schedule/mod.rs"));
    assert!(
        src.contains("if job.interval_s > 0 {"),
        "`tick` 应含 `interval_s > 0` 守卫"
    );
    let after = src
        .split("if job.interval_s > 0 {")
        .nth(1)
        .expect("应能切出守卫块");
    let block_end = after.find("JobKind::At =>").unwrap_or(after.len());
    let block = &after[..block_end];
    assert!(
        !block.contains("else"),
        "`interval_s > 0` 分支**没有** else ⇒ 0 间隔 job 会静默停摆。\
         若将来加了 else（如重排或告警），本条结论需重写。实得块:\n{block}"
    );
}

/// **`delete_after_run` 是装饰性字段**：`tick` 从不读、`save()` 从不写。
///
/// 当前 `At` 恒删 / `Every` 恒留**恰好**等价于
/// `delete_after_run = (kind == At)`（`add` 的默认值）⇒ 无可观察后果。
#[test]
fn d395_delete_after_run_is_never_read() {
    let src = code_only(&read("src/schedule/mod.rs"));
    // 字段声明 + `add` 里的赋值允许；`tick` 里的读取不允许
    let uses: Vec<&str> = src
        .lines()
        .filter(|l| l.contains("delete_after_run"))
        .collect();
    assert!(
        uses.len() <= 2,
        "`delete_after_run` 出现 {uses:?} —— 超过「声明 + add 赋值」说明 \
         已有地方读它，本条结论需重写"
    );
    assert!(
        !uses.iter().any(|l| l.contains("if job.delete_after_run")),
        "`tick` 竟读了 `delete_after_run` —— 装饰性字段结论需重写"
    );
    // `save()` 的 JSON 模板里也不该有它
    assert!(
        !src.contains("\"delete_after_run\""),
        "`save()` 的 JSON 模板竟含 `delete_after_run` —— 字段已可持久化"
    );
}

// ── ④ 端到端：脚本可达面 ──

/// **`schedule.*` 脚本面往返**（`add` / `list` / `tick` / `remove` / `count`）。
#[test]
fn d395_e2e_schedule_surface() {
    let (code, out) = run_fixture("schedule_surface.mora");
    // 末行是故意的非法 kind ⇒ 非零退出
    assert_ne!(code, 0, "非法 kind 应非零退出; out={out}");
    for (needle, why) in [
        ("id_len=8", "job id 是 8 位 hex"),
        ("count1=1.0", "add 后计数"),
        ("list_len=1", "list 长度"),
        ("has_id=true", "list 元素含 id"),
        ("has_name=true", "list 元素含 name"),
        ("has_kind=true", "list 元素含 kind"),
        ("has_message=true", "list 元素含 message"),
        ("tick_len=0", "3600s 未到不应触发"),
        ("removed=true", "remove 成功"),
        ("count2=0.0", "remove 后计数归零"),
        ("removed_again=false", "二次 remove 返回 false"),
    ] {
        assert!(out.contains(needle), "缺 `{needle}`（{why}）; out={out}");
    }
    // 错误消息**点名**那个非法 kind
    assert!(
        out.contains("kind must be 'every' or 'at', got 'bogus'"),
        "非法 kind 的错误应点名实际值; out={out}"
    );
}
