//! v0.104.6：`task` / `fn` 的**形参类型标注**（此前连解析都做不到）。
//!
//! ## 背景
//!
//! 标注语法 `task add(a: number, b: number): number` 与 `fn(x: number) … end`
//! 此前一律 `Parse error: Expected ')' after parameters` —— README「Type
//! checking」节的示例曾因此整块被删（见该节 D117 更正说明）。
//!
//! 承载位与消费端**本就存在**，缺的只是 parser 填值 + 调用点签名登记：
//! - `WitnessParam.type_hint`（witness.rs）+ `FnDef.return_type`
//! - `bidirectional.rs` Phase A/A' 把形参 `type_hint` 绑进 HM 环境做 subtype 检查
//! - `Param.type_hint`（orchestrate::Param）→ 转 `WitnessParam.type_hint`
//! - `hm::precompute_fn_signatures` 把带标注的 `FnDef` 登记进 env 成 curried
//!   `Arrow`，使**调用点**按声明检查实参
//!
//! 本文件锁住「能解析 + 标注生效 + 裸形参零回归」三条性质。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 编译并 typecheck，返回类型错误数。
fn typecheck(src: &str) -> Result<usize, String> {
    let (_, witnesses) = mora::parser_v3::ParserV3::compile(src)
        .map_err(|e| format!("解析失败（应能解析）: {e}"))?;
    Ok(check_program_witnesses_bidirectional(&witnesses).len())
}

/// `task` 形参标注：能解析，且正确实参通过。
#[test]
fn task_param_annotation_parses_and_accepts_correct_arg() {
    assert_eq!(
        typecheck(
            "task add(a: number, b: number): number\n  return a + b\nend\nprint(add(1, 2))\n"
        ),
        Ok(0)
    );
}

/// `task` 形参标注**生效**：错类型实参被拒（此前整段 Parse error）。
#[test]
fn task_param_annotation_rejects_wrong_arg() {
    let errs = typecheck("task f(a: number)\n  return a\nend\nprint(f(\"x\"))\n").unwrap();
    assert!(
        errs > 0,
        "`task f(a: number)` 被传字符串实参必须报类型错 —— 标注须在调用点生效"
    );
}

/// `task` 返回标注生效：把返回值赋给不匹配类型被拒。
#[test]
fn task_return_annotation_is_enforced() {
    let errs =
        typecheck("task f(a: number): number\n  return a\nend\nlet s: string = f(1)\n").unwrap();
    assert!(
        errs > 0,
        "`task f(...): number` 的返回值不得赋给 string —— 返回标注须生效"
    );
}

/// `fn` 闭包形参标注：能解析 + 正确实参通过 + 错实参被拒。
#[test]
fn closure_param_annotation_works() {
    assert_eq!(
        typecheck("let f = fn(x: number) x + 1 end\nprint(f(1))\n"),
        Ok(0)
    );
    let errs = typecheck("let f = fn(x: number) x + 1 end\nprint(f(\"s\"))\n").unwrap();
    assert!(errs > 0, "`fn(x: number)` 被传字符串实参必须报类型错");
}

/// union 标注可用（`parse_type_annotation` 支持 `|`）。
#[test]
fn union_param_annotation_works() {
    assert_eq!(
        typecheck("task f(a: string | number)\n  return a\nend\nprint(f(1))\nprint(f(\"s\"))\n"),
        Ok(0)
    );
}

/// **零回归**：裸形参（无标注）行为不变 —— 不同类型实参都应通过，
/// 且不得因本改动被判错。
#[test]
fn bare_params_are_unaffected() {
    // 裸 task：多类型实参都过。
    assert_eq!(
        typecheck("task g(a)\n  return a\nend\nprint(g(1))\nprint(g(\"s\"))\n"),
        Ok(0)
    );
    // 裸闭包：同上。
    assert_eq!(
        typecheck("let f = fn(x) x + 1 end\nprint(f(1))\nprint(f(\"s\"))\n"),
        Ok(0)
    );
}

/// 混合：部分标注、部分裸形参 —— 标注的那几个被约束，裸的保持自由。
#[test]
fn mixed_annotated_and_bare_params() {
    // 标注位正确 ⇒ 通过。
    assert_eq!(
        typecheck("task f(a: number, b)\n  return a\nend\nprint(f(1, 2))\n"),
        Ok(0)
    );
    // 标注位错误 ⇒ 被拒（裸位 b 不受影响）。
    let errs = typecheck("task f(a: number, b)\n  return a\nend\nprint(f(\"x\", 2))\n").unwrap();
    assert!(errs > 0, "混合声明里标注位 `a: number` 须被检查");
}

/// 形参标注不影响**元数**检查（既有 arity 语义不变）。
#[test]
fn annotated_params_keep_arity_check() {
    // 少传实参仍应报错（标注不豁免元数检查）。
    let errs = typecheck("task f(a: number, b: number)\n  return a\nend\nprint(f(1))\n").unwrap();
    assert!(errs > 0, "少传实参仍须报错");
}
