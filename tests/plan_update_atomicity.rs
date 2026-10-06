//! v0.104.6 D401 —— `Plan::update` **非原子**：遇到未知 id 时报错，
//! 但**前面的更新已经生效**（修复轮）
//!
//! ## 缺陷
//!
//! 修前是逐条「查 id → 写 status」，遇到未知 id 就 `?` 返回：
//!
//! ```text
//! update([("a",Done), ("b",Done), ("ghost",Done)])
//!   → Err: step id 'ghost' not found
//!   → 但 a、b **已变成 Done**（done=2 / pending=1）
//! ```
//!
//! 错误消息只说「ghost 不存在」，会让调用方以为什么都没发生 ——
//! 而 `plan` 是**持久留在解释器注册表里**的：脚本报错终止，**plan 状态仍在**。
//!
//! ## 修法：两轮 —— 先全量校验，再统一应用
//!
//! ## 既有测试为何没抓到
//!
//! `plan_update_unknown_id_errors` 只传**单个**未知 id，
//! ⇒ 循环第一轮就返回，**永远碰不到「前面已生效」的情形**。
//! 与 D396 同族：**测试矩阵落在缺陷之外**。
//!
//! ## 为什么 e2e 只能钉错误消息
//!
//! Mora 没有 try/catch，脚本遇到 runtime error 即终止 ⇒ 脚本层
//! **观察不到**错误之后的状态。原子性只能在库级验证。

use std::path::Path;
use std::process::Command;

use mora::plan::{Plan, PlanStep, StepStatus};

fn plan3() -> Plan {
    let mut p = Plan::new();
    for id in ["a", "b", "c"] {
        p.add_step(PlanStep::new(id, id.to_uppercase()))
            .expect("add");
    }
    p
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

// ── ① 核心：失败时**一个字节都不写** ──

/// **未知 id ⇒ 完全不生效**（修前 a/b 已变 Done）。
#[test]
fn d401_failed_update_applies_nothing() {
    let mut p = plan3();
    let err = p
        .update(&[
            ("a".to_string(), StepStatus::Done),
            ("b".to_string(), StepStatus::Done),
            ("ghost".to_string(), StepStatus::Done),
        ])
        .expect_err("ghost 不存在，应报错");
    assert!(err.contains("ghost"), "错误应点名缺失的 id; 实得 {err}");

    assert_eq!(p.complete_count(), 0, "失败后**不得**有任何 step 变成 Done");
    for s in p.steps() {
        assert_eq!(
            s.status,
            StepStatus::Pending,
            "step {:?} 被部分更新了 —��� update 非原子",
            s.id
        );
    }
}

/// **缺失 id 在中间**也一样全不生效（位置无关）。
#[test]
fn d401_missing_id_in_the_middle_applies_nothing() {
    let mut p = plan3();
    let _ = p.update(&[
        ("a".to_string(), StepStatus::Done),
        ("ghost".to_string(), StepStatus::InProgress),
        ("c".to_string(), StepStatus::Done),
    ]);
    assert_eq!(p.complete_count(), 0, "中间缺失也不得部分生效");
    assert_eq!(p.in_progress_count(), 0);
    assert_eq!(p.pending_count(), 3, "三个 step 都应仍是 pending");
}

/// **首个就缺失** ⇒ 同样什么都不写（修前也是不写，本条是反向对照）。
#[test]
fn d401_missing_first_id_applies_nothing() {
    let mut p = plan3();
    let _ = p.update(&[
        ("ghost".to_string(), StepStatus::Done),
        ("a".to_string(), StepStatus::Done),
    ]);
    assert_eq!(p.complete_count(), 0);
}

// ── ② 反向对照：全合法时**必须**全部生效 ──

/// **全部合法 ⇒ 全部生效**（原子性不能变成「什么都不做」）。
#[test]
fn d401_valid_update_applies_everything() {
    let mut p = plan3();
    p.update(&[
        ("a".to_string(), StepStatus::Done),
        ("b".to_string(), StepStatus::InProgress),
        ("c".to_string(), StepStatus::Done),
    ])
    .expect("全合法应成功");
    assert_eq!(p.complete_count(), 2, "a/c 应为 Done");
    assert_eq!(p.in_progress_count(), 1, "b 应为 InProgress");
    assert_eq!(p.pending_count(), 0);
}

/// **空更新列表**是 no-op 且不报错。
#[test]
fn d401_empty_update_is_noop() {
    let mut p = plan3();
    p.update(&[]).expect("空更新应成功");
    assert_eq!(p.complete_count(), 0);
    assert_eq!(p.len(), 3, "空更新不得改变 step 数量");
}

/// **同一 id 重复出现**时以**最后一次**为准（不是第一次）。
#[test]
fn d401_duplicate_id_in_one_batch_uses_the_last_value() {
    let mut p = plan3();
    p.update(&[
        ("a".to_string(), StepStatus::Done),
        ("a".to_string(), StepStatus::InProgress),
    ])
    .expect("应成功");
    assert_eq!(
        p.get("a").unwrap().status,
        StepStatus::InProgress,
        "同一 id 重复出现时最后一次应生效"
    );
}

// ── ③ 端到端：错误消息点名缺失的 id ──

/// **脚本层**：`plan.update` 报出的错误应点名缺失的 id。
///
/// 原子性在脚本层**无法**验证（Mora 无 try/catch，报错即终止），
/// 故 e2e 只钉诊断质量。
#[test]
fn d401_e2e_error_names_the_missing_id() {
    let (code, out) = run_fixture("plan_update_partial.mora");
    assert_ne!(code, 0, "含未知 id 的 update 应非零退出; out={out}");
    assert!(
        out.contains("step id 'ghost' not found"),
        "错误应点名缺失的 id `ghost`; out={out}"
    );
    assert!(
        out.contains("info0=") && out.contains("done: 0.0"),
        "fixture 应先打印更新前的状态作为对照; out={out}"
    );
}
