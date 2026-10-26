//! v0.104.6 D484：**Core 层自己的值表示** —— `Value::CoreClosure`。
//!
//! ## 为什么
//!
//! `Value::Closure` 装的是 `Arc<MirFunction>`（**MIR** 的函数体）⇒ 装不下
//! Core 闭包（函数体是 Core IR 的 `CoreFunction`）⇒ D482/D483 之前 Core 执行器
//! 遇到闭包只能**响亮报错**。
//!
//! D484 给语言值加了 `Value::CoreClosure { params, env, core_body }`（形状与
//! `Value::Closure` **同款**）⇒ Core 执行器能执行自己的闭包，不必绕回 MIR 层。
//!
//! **其余值一律复用 `Value`** —— 不另立 `CoreValue` 类型：
//!
//! - Core 执行器仍用 Mora 的 `Environment` 与既有 handler ⇒ **不重复实现**
//!   环境语义（持久 map + 父链 + define/assign/lookup）；
//! - 不引入 `Value ↔ CoreValue` 的转换层 ⇒ 边界上没有第二套语义。
//!
//! （与 `Value::Task` / `Value::Tool` / `Value::Macro` 同款：语言值里带
//! IR 层载荷，已有先例。）
//!
//! ## 判据
//!
//! ① `Value::CoreClosure` 与 **MIR 闭包可区分** —— 类型名 / 显示 / JSON 三处
//!    都用**不同的名字**。混称 "closure" 会让「调用失败」难以定位（两种闭包的
//!    函数体不同、执行器不同）；
//! ② Core 执行器造出的闭包值**形态正确**（形参 / 捕获的环境 / Core 体）——
//!    不是「能调用就行」，载荷也得对；
//! ③ 闭包调用走**唯一机制**（`mir::core_exec::call_core_closure`）—— 具名调用
//!    与值调用两条路径的结果**一致**。

use std::sync::Arc;

use mora::mir::core::CoreFunction;
use mora::value::{EnvRef, Value};

/// 构造一个程序的 CoreFunction（走 9 层里的 EHIR → Core 段）。
fn core_of(src: &str) -> CoreFunction {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("解析");
    let fcfg = mora::mir::witness_to_fcfg::witness_to_fcfg(&witnesses);
    let table = mora::typeck::export::export_type_table(&witnesses);
    let ehir = mora::typeck::annotate::annotate(&fcfg, &table);
    mora::mir::ehir_to_core::ehir_to_core(&ehir, vec![], func.effects.clone())
}

/// 走 **Core 执行器**执行，返回末值。
fn exec_core(src: &str) -> Result<Value, String> {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).map_err(|e| e.to_string())?;
    let core = mora::mir::pipeline::compile_core(&func, &witnesses);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = mora::mir::effect::Effects::new();
    mora::mir::core_exec::execute(&core, &mut env, &mut interp, &mut effects)
}

/// 一个**空**的 Core 函数体（用来造测试值）。
fn empty_core_fn() -> CoreFunction {
    CoreFunction {
        params: vec![],
        blocks: vec![],
        entry: 0,
        effects: mora::mir::effect::EffectRow::Empty,
        n_regs: 0,
    }
}

/// 判据 ①：`Value::CoreClosure` 与 MIR 闭包**可区分**。
///
/// 三处（类型名 / 显示 / JSON）都必须用**不同**的名字 ⇒ 混称会让
/// 「调用失败」这类问题难以定位。
#[test]
fn core_closure_is_distinguishable_from_mir_closure() {
    let core_closure = Value::CoreClosure {
        params: vec!["x".to_string()],
        env: EnvRef(Box::default()),
        core_body: Arc::new(empty_core_fn()),
    };
    let mir_closure = Value::Closure {
        params: vec!["x".to_string()],
        env: EnvRef(Box::default()),
        mir_body: Arc::new(mora::mir::MirFunction {
            params: vec!["x".to_string()],
            body: vec![],
            n_regs: 0,
            effects: mora::mir::effect::EffectRow::Empty,
        }),
    };

    // 类型名
    assert_eq!(mora::flow::type_name(&core_closure), "coreclosure");
    assert_eq!(mora::flow::type_name(&mir_closure), "closure");
    // 显示
    assert_eq!(core_closure.to_string(), "<coreclosure>");
    assert_eq!(mir_closure.to_string(), "<closure>");
    // JSON
    let jc = mora::flow::value_to_json(&core_closure);
    let jm = mora::flow::value_to_json(&mir_closure);
    assert!(jc.contains("coreclosure"), "JSON 应能区分 Core 闭包：{jc}");
    assert!(
        jm.contains("closure") && !jm.contains("coreclosure"),
        "JSON：{jm}"
    );
}

/// 判据 ②：Core 执行器造出的闭包值**形态正确**。
///
/// 不只「能调用就行」—— 形参、捕获的环境、Core 体三样都得对。
#[test]
fn core_executor_creates_a_well_formed_core_closure() {
    let v = exec_core("let base = 10\nlet f = fn(x) x + base end\nf\n").expect("应执行成功");
    let Value::CoreClosure {
        params,
        env,
        core_body,
    } = &v
    else {
        panic!("应是 Core 闭包值，实际 {:?}", v);
    };
    assert_eq!(params, &vec!["x".to_string()], "闭包形参名应保留");
    assert!(
        !core_body.blocks.is_empty(),
        "Core 闭包体应是**完整子函数**（有自己的块集合），实际 {} 块",
        core_body.blocks.len()
    );
    assert!(
        env.env().values.contains_key("base"),
        "闭包应捕获外层绑定 `base`（捕获的环境作父链），实际捕获 {:?}",
        env.env().values
    );
}

/// 判据 ③：闭包调用走**唯一机制** —— 两条路径结果一致。
///
/// - **具名调用** `f(1)`：走 `h_call` 的「**环境**查找」分支，查到 Core
///   闭包 ⇒ 宿主委派回 Core 执行器；
/// - **值调用**（管道 `|>`）：Core 执行器内部的 `ClosureCall`。
///
/// 两条都经 `mir::core_exec::call_core_closure` ⇒ 结果必须一致。
#[test]
fn named_and_value_calls_agree() {
    let named = exec_core("let f = fn(x) x + 1\nf(1)\n").expect("具名调用应成功");
    let valued = exec_core("let f = fn(x) x + 1\n1 |> f\n").expect("值调用应成功");
    assert_eq!(
        named, valued,
        "具名调用与值调用应走同一套闭包调用机制，结果必须一致"
    );
    // 顺带确认「唯一机制」确实被用上了：Core 产物里两条路径的**调用形态不同**
    // （具名 = `Call`，值 = `ClosureCall`），却给出同一个值。
    let core = core_of("let f = fn(x) x + 1\nf(1)\n1 |> f\n");
    let has_named = core
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .any(|i| matches!(i, mora::mir::core::CoreInst::Call(_, name, _) if name == "f"));
    let has_value = core
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .any(|i| matches!(i, mora::mir::core::CoreInst::ClosureCall(_, _, _)));
    assert!(
        has_named && has_value,
        "用例应同时覆盖具名调用（Call）与值调用（ClosureCall）：named={has_named} value={has_value}"
    );
}
