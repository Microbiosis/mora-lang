//! v0.104.6 D343 —— `plan.create` 的非法 `status` **静默变成 pending**（已修）
//!
//! `plan.*` 是有状态的计划存储（步骤的增删改），从未量过。本轮 20+ 用例，
//! **一个缺陷**。
//!
//! ## 缺陷：同一模块内**两种策略并存**
//!
//! ```text
//! plan.update(p, [["s1", "BOGUS"]])
//!   → exit 1   plan.update: updates[0][1] invalid status 'BOGUS'   ← 严格
//!
//! plan.create(p, [{id:"s2", text:"t2", status:"BOGUS"}])
//!   → exit 0   s2 变成 **pending ⬜**                                ← 静默
//! ```
//!
//! 实现上的差异（`plan.rs`）：
//!
//! ```rust
//! // create（第 42-46 行）—— D143 前：
//! Some(Value::String(s)) => StepStatus::parse(s).unwrap_or(StepStatus::Pending),
//!
//! // update（第 86-89 行）—— 一直是对的：
//! Some(Value::String(s)) => StepStatus::parse(s).ok_or_else(|| …invalid status…)?,
//! ```
//!
//! ## 为什么是缺陷（不是「宽松接受」）
//!
//! ① `StepStatus::parse` 返回 **`Option`** —— 它就是在说「这个字符串**可能不合法**」。
//!    调用方把这个信息丢掉是**漏写**，不是设计（同 `src/plan/mod.rs` 的签名）。
//! ② **同模块内有人做对了**：`plan.update` 对**完全相同**的非法值明确报错。
//!    同一模块、同一枚举、同一 `parse` 函数，两种处理。
//! ③ **危害具体**：拼错状态名（`complete` 少了个 `d`、`in progress` 写成空格…）
//!    的那一步**看起来是待办** ⇒ 依赖 `done` 的下游（完成度统计 / 收尾检查）
//!    会漏掉它，而用户**完全不知道自己拼错了**。
//!
//! ## 修法
//!
//! `create` 改用与 `update` 相同的 `.ok_or_else(...)`，并把
//! `parse` 认得的**全部别名**列进错误消息（`parse` 接受 12 个：
//! `pending`/`todo`/`⬜`、`in_progress`/`in-progress`/`doing`/`🔄`、
//! `done`/`completed`/`finish`/`✅`）。
//!
//! ## 顺带钉住的三条既有行为（**不改**，只记录现状）
//!
//! | 行为 | 实测 | 是否要改 |
//! |---|---|---|
//! | `create` **覆盖**同名计划 | 2 步 → 1 步，零提示，exit 0 | **待裁决**（`create` 语义是否含覆盖？）|
//! | `remove` 不存在的 id 返回 `false` | exit 0，`false` | ✅ 合理（布尔返回本就表意）|
//! | `add` 重复 id 报错 | `step id 's1' already exists` | ✅ 正确 |

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d343_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`（plan 存在解释器状态里，单进程内多用例会互相污染）。
/// 取 stdout **全部**实质行（D330 教训：不能用 `find(第一行)`）。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d343p_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **主断言**：`plan.create` 的非法 `status` 必须报错，且**不能**静默变 pending。
///
/// 三种非法形态全覆盖：拼错的英文、拼错的中文/emoji、纯乱码。
#[test]
fn d343_create_rejects_invalid_status_instead_of_silently_pending() {
    for bad in ["BOGUS", "complete", "in progress", "done!", "✅✅"] {
        let body =
            format!("plan.create(\"p\", [{{id: \"s1\", text: \"t\", status: \"{bad}\"}}])\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 1,
            "非法 status `{bad}` 应报错（修前静默变 pending、exit 0、零诊断）; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("invalid") && got.contains(bad),
            "`{bad}` 的错误应点名这个值; 实得: {got}"
        );
        // ⚠ **不能**断言「消息里不含 pending」—— 错误消息会把
        // `parse` 认得的**全部合法别名**列出来，`pending` 正是其中之一。
        // 第一版这样写，结果自己被自己的修复文案绊倒。
        // 真正要验的是「**没有**静默成功」，上面的 exit==1 已经验了。
    }
}

/// **配对断言**：`plan.update` 早就对同样的非法值报错 —— 两条路径现在**一致**。
///
/// 修前 `create` 静默、`update` 报错，正是「同一模块两种策略并存」。
/// 本条钉住两者**同时**严格。
#[test]
fn d343_create_and_update_both_reject_the_same_invalid_status() {
    let cases = [
        (
            r#"plan.create("p", [{id:"s", text:"t", status:"BOGUS"}])"#,
            "create",
        ),
        (
            r#"plan.create("p", [{id:"s", text:"t"}])
plan.update("p", [["s", "BOGUS"]])"#,
            "update",
        ),
    ];
    for (body, which) in cases {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(
            code, 1,
            "[{which}] 非法 status 应报错; 实得 exit={code} out={got}\n\
             修前：create 静默变 pending、update 报错 —— 同一模块两种策略"
        );
        assert!(
            got.contains("invalid"),
            "[{which}] 应报 invalid; 实得: {got}"
        );
    }
}

/// **对照组 1**：合法的 status（含 `parse` 认得的**全部别名**）照常工作。
///
/// 这条很重要：守卫不能只挡非法值而误伤合法别名 —— 尤其
/// `in-progress`（连字符）与 `✅`（emoji）这些非显然的写法。
#[test]
fn d343_all_valid_status_aliases_still_work() {
    for (alias, expect_status) in [
        ("pending", "pending"),
        ("todo", "pending"),
        ("⬜", "pending"),
        ("in_progress", "in_progress"),
        ("in-progress", "in_progress"),
        ("doing", "in_progress"),
        ("🔄", "in_progress"),
        ("done", "done"),
        ("completed", "done"),
        ("finish", "done"),
        ("✅", "done"),
    ] {
        let body = format!(
            "plan.create(\"p\", [{{id: \"s\", text: \"t\", status: \"{alias}\"}}])\nprint(plan.list(\"p\"))\n"
        );
        let (code, got) = ev(&body);
        assert_eq!(
            code, 0,
            "合法 status `{alias}` 应成功; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains(&format!("status: {expect_status}")),
            "`{alias}` 应解析为 `{expect_status}`; 实得: {got}"
        );
    }
}

/// **对照组 2**：**省略** `status` ⇒ 默认 `pending`（守卫不能误伤这个）。
#[test]
fn d343_omitted_status_still_defaults_to_pending() {
    let (code, got) =
        ev("plan.create(\"p\", [{id: \"s\", text: \"t\"}])\nprint(plan.list(\"p\"))\n");
    assert_eq!(code, 0, "省略 status 应成功; 实得 exit={code} out={got}");
    assert!(
        got.contains("status: pending"),
        "省略 status 应默认 pending; 实得: {got}"
    );
}

/// **对照组 3**：`create` 的其余字段校验不变（id / text / steps 类型）。
#[test]
fn d343_other_create_field_checks_unchanged() {
    for (body, needle) in [
        (r#"plan.create("p", [{text: "x"}])"#, "id must be a string"),
        (r#"plan.create("p", [{id: "x"}])"#, "text must be a string"),
        (r#"plan.create("p", "notalist")"#, "steps must be a list"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(code, 1, "`{body}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains(needle),
            "`{body}` 应报 `{needle}`; 实得: {got}"
        );
    }
}

/// **对照组 4**：`add` / `remove` / 计划不存在的既有契约不变。
#[test]
fn d343_add_remove_and_missing_plan_contracts_unchanged() {
    // remove 不存在的 id → `false`（布尔返回本就表意「没删掉」）
    let (code, got) = ev(
        "plan.create(\"p\", [{id: \"s\", text: \"t\"}])\nprint(plan.remove(\"p\", \"nosuch\"))\n",
    );
    assert_eq!(
        code, 0,
        "remove 不存在的 id 应返回 false 而非报错; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "false", "应得 false; 实得 {got}");

    // add 重复 id → 报错
    let (code, got) = ev(
        "plan.create(\"p\", [{id: \"s\", text: \"t\"}])\nprint(plan.add(\"p\", \"s\", \"dup\"))\n",
    );
    assert_eq!(code, 1, "add 重复 id 应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("already exists"),
        "应说明 id 已存在; 实得: {got}"
    );

    // 计划不存在 → 三条路径都明确报错
    for (body, needle) in [
        (r#"print(plan.add("nosuch", "i", "t"))"#, "not found"),
        (r#"print(plan.remove("nosuch", "i"))"#, "not found"),
        (r#"print(plan.list("nosuch"))"#, "not found"),
        (r#"print(plan.update("nosuch", []))"#, "not found"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(code, 1, "`{body}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains(needle),
            "`{body}` 应报 `{needle}`; 实得: {got}"
        );
    }
}

/// **现状钉住**：`plan.create` **静默覆盖**同名计划（本条**不改**）。
///
/// 实测：2 步的计划被同名 `create`（1 步）替换 ⇒ 变成 1 步、**零提示**、exit 0。
///
/// 这是**另一个候选缺陷**，但 `create` 的语义是否包含「覆盖」属
/// **产品契约决定**（与 D334 的 `min_arity` 同性质：机制上说得通，
/// 意图上要你拍板），故只钉现状。
///
/// ⚠ 若本条红，说明有人给 `create` 加了「已存在」检查 —— 那是**有意的**变更。
#[test]
fn d343_create_silently_overwrites_an_existing_plan() {
    let (code, got) = ev(
        "let p = plan.create(\"p\", [{id: \"s1\", text: \"t1\"}, {id: \"s2\", text: \"t2\"}])\n\
         print(len(plan.list(\"p\")))\n\
         print(plan.create(\"p\", [{id: \"z\", text: \"z\"}]))\n\
         print(len(plan.list(\"p\")))\n",
    );
    assert_eq!(
        code, 0,
        "同名 create 应成功（现状是**覆盖**）; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got, "2 | p | 1",
        "现状：同名 create **静默覆盖**（2 步 → 1 步，零提示）; 实得 {got}\n\
         ⚠ 若本条红，说明有人加了「已存在」检查 —— 那是**有意的**语义变更"
    );
}
