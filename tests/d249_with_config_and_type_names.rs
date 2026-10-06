//! v0.104.6 D249 —— 两个「同一事实两套实现」的残留：
//! `with-config` 的数值键与 `value_type_simple` 的类型名。
//!
//! ## 缺陷一：`with temperature = len([1,2,3])` 被拒，理由荒谬
//!
//! `mir_with_config` 的 `temperature` / `max_tokens` 只匹配 `Value::Float`。
//! **数字字面量恰好是 `Float`（D98）**，所以 `with temperature = 1` 能过 ——
//! 这个巧合把它藏住了。但任何**返回 `Int` 的表达式**就撞上：
//!
//! ```text
//! with temperature = 1              → Ok
//! with temperature = len([1,2,3])   → Err: "expects a number, got int"
//! ```
//!
//! **「int 明明是数字」** —— 错误信息本身是错的。
//! 即同一个值，取决于它**怎么算出来的**，结果不同（`1` 与 `len([1,2,3])` 都是 3）。
//!
//! 用户想动态设置 temperature / max_tokens 时，只要表达式返回 `Int` 就被拒。
//!
//! ## 缺陷二：`value_type_simple(Int)` 报 `"other"`
//!
//! 该函数缺 `Int` / `BigInt` / `Char` 三个分支，全部落进 `_ => "other"`。
//! 它被**广泛用于错误信息**（`compress` 的全部参数校验 + `required_str_arg`），
//! 于是用户会看到：
//!
//! ```text
//! some_builtin(42) → "must be a string, got other"    ← 毫无信息量
//! ```
//!
//! 而 `flow::type_name` 覆盖完整（`Int`→"int"、`BigInt`→"bigint"、…）。
//!
//! ## 与 D246 的关系
//!
//! D246 立了收口 `flow::value_as_f64` / `value_as_usize` 并写明「新增数值
//! 提取**必须**走它」。本条是**又两处没走到** —— 与 D245/D248 同型：
//! **有收口 ≠ 都被收口**。本判据因此除了行为断言，还带一条
//! **「两份类型名在共有部分不得分叉」**的全称判据。

use std::sync::Arc;

fn run_with(src: &str) -> Result<(), String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|_| ())
}

fn with(key: &str, expr: &str) -> Result<(), String> {
    run_with(&format!("with {key} = {expr}\n  1\nend"))
}

/// ① 主判据：`with-config` 的两个数值键必须**两种数字类型都收**。
#[test]
fn d249_with_config_accepts_int_valued_expressions() {
    for key in ["temperature", "max_tokens"] {
        for (label, expr) in [
            ("float literal", "0.7"),
            ("numeric literal", "1"),
            ("len() (Int)", "len([1, 2, 3])"),
        ] {
            assert!(
                with(key, expr).is_ok(),
                "`with {key} = {expr}`（{label}）应被接受 —— \
                 返回 Int 的表达式不该被拒（错误信息会说「int 明明是数字」）。\
                 实得: {:?}",
                with(key, expr)
            );
        }
    }
}

/// ② 不回归 D39：非数值仍必须**报错**（不是静默失效）。
#[test]
fn d249_with_config_still_rejects_non_numbers() {
    for key in ["temperature", "max_tokens"] {
        let err = with(key, "\"hot\"").expect_err("字符串应被拒绝（D39）");
        assert!(
            err.contains("expects a number"),
            "{key} 对字符串的错误信息不对: {err}"
        );
    }
}

/// ③ 不回归 D147：负数仍必须报「不能为负数」——**不能**被新收口吞掉。
///
/// 这条专门盯 `value_as_usize` 若被误用：`value_as_usize(-1.0)` 返回 `None`，
/// 若直接用它写错误分支，信息会退化成「expects a number, got float」，
/// 丢掉具体数值、且把负数说成 float —— 比修前更糟。
#[test]
fn d249_with_config_still_reports_negative_values() {
    let err = with("max_tokens", "-1.0").expect_err("负数应被拒绝（D147）");
    assert!(
        err.contains("不能为负数") && err.contains("-1"),
        "负数错误信息应含「不能为负数」与具体数值，实得: {err}"
    );
}

/// ④ 主判据：`value_type_simple` 不得把常见值报成 `"other"`。
#[test]
fn d249_value_type_simple_names_common_values() {
    use mora::compress::value_type_simple;
    use mora::value::Value;

    for (v, want) in [
        (Value::Int(42), "int"),
        (Value::Float(4.2), "float"),
        (Value::BigInt(7i64.into()), "bigint"),
        (Value::Char('a'), "char"),
        (Value::String("s".into()), "string"),
        (Value::Bool(true), "bool"),
        (Value::Nil, "nil"),
        (Value::List(Vec::new().into()), "list"),
        (Value::Dict(Default::default()), "dict"),
    ] {
        assert_eq!(
            value_type_simple(&v),
            want,
            "value_type_simple 对 {:?} 报错了类型名（修前 Int/BigInt/Char 全是 \"other\"）",
            v
        );
    }
}

/// ⑤ **全称判据**：两份类型名函数在**共有**的类型上不得分叉。
///
/// `value_type_simple`（compress）与 `flow::type_name` 是「同一事实两套实现」。
/// 本条固定住它们在 9 种共有类型上的一致性 —— 将来任一侧新增/改名分支时，
/// 若忘了另一侧，本条立刻变红并点名是哪个类型。
///
/// ⚠ 它**不**要求两者完全相同：`type_name` 还覆盖 relation / goal / task 等
/// 声明式值，而 `value_type_simple` 目前对那些仍返回 `"other"`
/// （彻底转发的影响面见该函数注释，需单独评估）。
#[test]
fn d249_type_name_functions_agree_on_shared_types() {
    use mora::compress::value_type_simple;
    use mora::flow::type_name;
    use mora::value::Value;

    let cases: Vec<(&str, Value)> = vec![
        ("Int", Value::Int(42)),
        ("Float", Value::Float(4.2)),
        ("BigInt", Value::BigInt(7i64.into())),
        ("Char", Value::Char('a')),
        ("String", Value::String("s".into())),
        ("Bool", Value::Bool(true)),
        ("Nil", Value::Nil),
        ("List", Value::List(Vec::new().into())),
        ("Dict", Value::Dict(Default::default())),
    ];

    let mut diverged: Vec<String> = Vec::new();
    for (label, v) in &cases {
        let a = value_type_simple(v);
        let b = type_name(v);
        if a != b {
            diverged.push(format!(
                "{label}: value_type_simple={a:?} vs type_name={b:?}"
            ));
        }
    }
    assert!(
        diverged.is_empty(),
        "两份类型名函数在共有类型上分叉了：\n  - {}",
        diverged.join("\n  - ")
    );
    assert_eq!(
        cases.len(),
        9,
        "判据自身失效：共有类型清单被改动，请同步更新"
    );
}

/// ⑥ 对照组：钉住「数字字面量给 `Float`」这条**前提**（D98）。
///
/// 若哪天字面量改成给 `Int`，缺陷一的「巧合藏身之处」会消失，本条的
/// 针对性需要重新审视。
#[test]
fn d249_control_group_numeric_literal_is_float() {
    // 间接证据：`with temperature = 1` 在修前就能过，说明 `1` 是 Float。
    assert!(
        with("temperature", "1").is_ok(),
        "对照组前提失效：数字字面量 `1` 不再被接受"
    );
}
