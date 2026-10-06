//! v0.104.6 D152：可选**字符串 / 列表**实参被静默丢弃（已修）。
//!
//! ## 缺陷一（D150 的更坏变体）：`else` 分支**不是默认值，是另一个操作**
//!
//! ```mora
//! plan.create("alpha", [{id: "s1", text: "first"}])
//! plan.create("beta",  [{id: "s1", text: "second"}])
//! plan.list("alpha")   // [{emoji: ⬜, id: s1, status: pending, text: first}]
//! plan.list(5)         // [alpha, beta]   ← 修复前：静默返回**所有计划名**
//! ```
//!
//! D150 那 6 处的 `else` 至少是「同类型的默认值」；这里 `else` 是
//! 「列出全部」—— 用户要某计划的步骤，拿到的是**另一件事**。
//! exit 0、零诊断，下游 `filter` / 下标随之给出错误结果却无任何报错。
//!
//! **同文件的 `create` / `update` 对每个字段都有精确类型报错**
//! （`steps[{}].id must be a string` 等）—— 即「同族里有人做对了」。
//!
//! ## 缺陷二：`sandbox.containerize` 的 mounts / network / image
//!
//! 同函数里 `cpu_cores`(arg 3) / `memory_mb`(arg 4) **一直**是正确写法
//! （Int/Float 都认、`Nil` 显式放行、`_ =>` 报错），而紧邻的
//! `mounts`(arg 1) / `network`(arg 2) / `image`(arg 5) 三处没有 else 分支。
//!
//! 本机 Docker CLI 在但**守护进程未运行**，故 happy path 必然以
//! `docker daemon unreachable` 失败 —— 这恰好让缺陷的**危害**变得可见：
//! image 传错类型时，真正的问题被完全掩盖，exit 1 的**归因是错的**。
//! 修复后类型错误在**接触 Docker 之前**就报出来。

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).map(|v| format!("{v}"))
}

const SETUP: &str = r#"
plan.create("alpha", [{id: "s1", text: "first"}])
plan.create("beta", [{id: "s1", text: "second"}])
"#;

/// D152 主判据 ①：`plan.list` 的 name 传错类型必须**报错**（不再静默走「列全部」）。
#[test]
fn d152_plan_list_rejects_non_string_name() {
    for bad in ["5", "[\"alpha\"]", "{a: 1}", "true"] {
        let src = format!("{SETUP}\nplan.list({bad})\n");
        let res = run(&src);
        assert!(
            res.is_err(),
            "[plan.list({bad})] 传错类型必须报错（修复前静默返回**所有计划名**）; 实际: {res:?}"
        );
        let e = res.unwrap_err();
        assert!(
            e.contains("plan name must be a string"),
            "[plan.list({bad})] 错误信息应点明是 name 的类型问题; 实得: {e}"
        );
    }
}

/// D152 反向对照：`plan.list` 的**两条正确路径**必须逐字不变。
///
/// 特别要钉住「省略参数」—— 它是**合法**用法，不能因为收紧类型而被误伤。
#[test]
fn d152_plan_list_correct_paths_unchanged() {
    let all = run(&format!("{SETUP}\nplan.list()\n")).expect("省略参数应列全部计划名");
    assert_eq!(
        all, "[alpha, beta]",
        "省略 name 时必须照旧列出**所有**计划名（这是合法用法，不能被收紧误伤）"
    );

    let steps = run(&format!("{SETUP}\nplan.list(\"alpha\")\n")).expect("字符串 name 应列步骤");
    assert_eq!(
        steps, "[{emoji: ⬜, id: s1, status: pending, text: first}]",
        "字符串 name 必须照旧返回该计划的步骤"
    );
}

/// D152 反向对照：`plan.list` 传**不存在的**名字仍报「not found」（不是类型错）。
#[test]
fn d152_plan_list_missing_name_still_reports_not_found() {
    let res = run(&format!("{SETUP}\nplan.list(\"nope\")\n"));
    let e = res.expect_err("不存在的计划名应报错");
    assert!(
        e.contains("not found"),
        "「名字合法但不存在」必须仍是 not found（别被类型检查吞掉）; 实得: {e}"
    );
    assert!(
        !e.contains("must be a string"),
        "合法字符串不该被报成类型错; 实得: {e}"
    );
}

/// D152 主判据 ②：`sandbox.containerize` 的 image 传错类型，必须在**接触 Docker 之前**报错。
#[test]
fn d152_containerize_rejects_non_string_image_before_docker() {
    let src = "sandbox.containerize(\"docker\", [], \"host\", 2, 512, 12345)\n";
    let e = run(src).expect_err("image 传数字必须报错");
    assert!(
        e.contains("image must be a string"),
        "应报 image 的类型问题; 实得: {e}"
    );
    assert!(
        !e.contains("docker"),
        "类型错必须在**接触 Docker 之前**报出（修复前它被 docker 错误完全掩盖）; 实得: {e}"
    );
}

/// D152 主判据 ③：network / mounts 传错类型同样必须报错。
#[test]
fn d152_containerize_rejects_non_string_network_and_mounts() {
    for (src, field) in [
        (
            "sandbox.containerize(\"docker\", [], 7, 2, 512, \"alpine\")\n",
            "network",
        ),
        (
            "sandbox.containerize(\"docker\", \"notalist\", \"host\", 2, 512, \"alpine\")\n",
            "mounts",
        ),
    ] {
        let e = run(src).expect_err(&format!("{field} 传错类型必须报错"));
        assert!(
            e.contains(field),
            "[{field}] 错误信息应点名该字段; 实得: {e}"
        );
        assert!(
            !e.contains("docker daemon"),
            "[{field}] 类型错应在接触 Docker 之前报出; 实得: {e}"
        );
    }
}

/// D152 对照组：同函数里**本来就正确**的 `cpu_cores` / `memory_mb` 不得回退。
///
/// 它们是本次普查的**可触达正面样板**（Int/Float 都认、`Nil` 放行、`_` 报错）。
/// `Nil` 显式放行这一点尤其要钉住 —— 收紧类型时不能把 `nil` 当成「类型错」。
#[test]
fn d152_containerize_existing_correct_numeric_args_not_regressed() {
    for src in [
        // Int 来自 len() —— 正面样板的 Int 分支
        "sandbox.containerize(\"docker\", [], \"host\", len([1, 1]), len([5, 1, 2]), \"alpine\")\n",
        // Nil 显式放行
        "sandbox.containerize(\"docker\", nil, nil, nil, nil, nil)\n",
    ] {
        let res = run(src);
        let e = match res {
            Ok(_) => continue,
            Err(e) => e,
        };
        // 本机 Docker 守护进程未跑，happy path 必然止于 spawn —— 那是**允许**的。
        // 不允许的是「类型错」，那说明收紧过头了。
        assert!(
            !e.contains("must be a number") && !e.contains("must be a string"),
            "[{src}] 本就正确的 Int / Nil 路径被误伤; 实得: {e}"
        );
    }
}

/// D152 对照组：`plan.create` / `plan.update` 的既有精确报错不得回退。
#[test]
fn d152_plan_create_field_errors_not_regressed() {
    let e = run("plan.create(\"p\", \"kind\")\n").expect_err("steps 传字符串应报错");
    assert!(
        e.contains("steps must be a list"),
        "`plan.create` 的既有精确报错不得回退; 实得: {e}"
    );
}
