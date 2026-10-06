//! v0.104.6 D378 —— `src/pregel/reducers.rs` 的 **reducer 语义矩阵**（否定轮，无产品变更）
//!
//! `src/pregel/` 有 4 个判据，覆盖**入口**（`pregel_entry_edge`）、
//! **上限**（`pregel_max_steps_exhaustion`）、**载荷**（`pregel_multi_target_payload`）、
//! **输入格式**（`pregel_node_input_json`）—— 但**没有覆盖聚合语义本身**。
//!
//! `reducers.rs`（98 行）是超步迭代中「多个入边值合并成一个状态值」的核心：
//!
//! | 函数 | 语义 |
//! |---|---|
//! | `accumulator_reduce` | `+` / `*` 累加，首次写初始化为**运算元**（0 / 1）|
//! | `concat_reduce` | 字符串拼接，非字符串经 `Display` 转换 |
//! | `build_per_key_strategies` | 从 state schema 派生 per-key 合并策略 |
//! | `parse_custom_merge_expr` | `Custom` reducer 的 payload 解析（整数→`IntLit`，否则→变量引用）|
//!
//! ## D235 已修掉「重复实现」这个洞
//!
//! `reducers.rs:86-98` 记录：原有的 `pub fn value_to_json_string` 被**删除**，
//! 因为它是 `pregel/mod.rs` 同名函数的**重复实现**（D209 记过「改一处须同步
//! 另一处」的陷阱），且 dict 无引号、Float 丢小数点。`build_node_input`
//! 现直接构造 `Value::Dict` 交给 `flow::value_to_json`（仓内唯一实现）。

use std::collections::HashMap;

use mora::pregel::reducers::{
    accumulator_reduce, build_per_key_strategies, concat_reduce, parse_custom_merge_expr,
};
use mora::value::Value;

fn bi(n: i64) -> Value {
    Value::BigInt(num_bigint::BigInt::from(n))
}

/// **装置自检**：首写必须初始化为**运算元**（`+`→0，`*`→1）。
#[test]
fn d378_harness_accumulator_identity() {
    assert_eq!(
        accumulator_reduce(None, Value::Int(5), "+").unwrap(),
        Value::Int(5)
    );
    assert_eq!(
        accumulator_reduce(None, Value::Int(5), "*").unwrap(),
        Value::Int(5)
    );
}

/// **累加器：首写 + 折叠**。
#[test]
fn d378_accumulator_folds() {
    for (op, expected) in [("+", Value::Int(8)), ("*", Value::Int(15))] {
        let got = accumulator_reduce(Some(Value::Int(5)), Value::Int(3), op).unwrap();
        assert_eq!(got, expected, "`5 {op} 3` 应得 {expected:?}");
    }
}

/// **累加器：跨数值类型的类型提升**。
///
/// 走 `flow::eval_binary` ⇒ 沿用已验证过的数值塔（D358/D359 修过）。
#[test]
fn d378_accumulator_promotes_numeric_types() {
    assert_eq!(
        accumulator_reduce(Some(Value::Int(5)), Value::Float(1.5), "+").unwrap(),
        Value::Float(6.5)
    );
    assert_eq!(
        accumulator_reduce(Some(Value::Int(5)), bi(3), "+").unwrap(),
        bi(8)
    );
    assert_eq!(accumulator_reduce(Some(bi(5)), bi(3), "+").unwrap(), bi(8));
}

/// **未知 op 必须明确报错**，不能静默按 `+` 处理。
#[test]
fn d378_accumulator_rejects_unknown_op() {
    for op in ["-", "", "sum", "PLUS"] {
        match accumulator_reduce(None, Value::Int(1), op) {
            Ok(v) => panic!("未知 op `{op}` 应报错; 实得 {v:?}"),
            Err(e) => assert!(
                e.contains("Unknown accumulator op"),
                "诊断应点名未知 op; 实得: {e}"
            ),
        }
    }
}

/// **`+` 遇到非数值时回落到 `flow::eval_binary` 的**字符串拼接**规则**。
///
/// ⚠ 脚本层 `5 + "x"` 被 **typeck** 拦下（*expected Float, got String*），
/// 但 **pregel 的边载荷是运行期动态值、绕过 typeck** ——
/// 所以 reducer **必须**自己能处理非数值。
///
/// 实测 `accumulator_reduce(Int(5), String("x"), "+")` → `String("5x")`
/// （走 `flow.rs` 的「String + 任意类型 → 自动转字符串拼接」规则）。
///
/// 这不是缺陷，是**纵深防御的第二道**：既然 typeck 拦不住，
/// reducer 就得给出**确定的**结果而不是 panic 或静默出错。
/// 本条把该行为钉住。
#[test]
fn d378_accumulator_add_falls_back_to_string_concat_for_non_numbers() {
    assert_eq!(
        accumulator_reduce(Some(Value::Int(5)), Value::String("x".into()), "+").unwrap(),
        Value::String("5x".into()),
        "非数值 + Int 走 `flow` 的字符串拼接规则（typeck 在脚本层已拦，此处是纵深防御）"
    );
    // `*` 遇到非数值 ⇒ `eval_binary` 的 Mul 无字符串分支 ⇒ 明确报错
    for v in [Value::String("x".into()), Value::Bool(true)] {
        assert!(
            accumulator_reduce(Some(Value::Int(5)), v.clone(), "*").is_err(),
            "`*` 遇非数值应报错（Mul 没有字符串拼接分支）; 实得 {:?}",
            accumulator_reduce(Some(Value::Int(5)), v, "*")
        );
    }
}

/// **`concat_reduce` 的拼接与 `Display` 转换**。
///
/// 注释明写「Non-string incoming values are stringified via Display」。
#[test]
fn d378_concat_stringifies_non_strings() {
    for (cur, inc, expected) in [
        (None, Value::String("abc".into()), "abc"),
        (Some(Value::String("abc".into())), Value::Nil, "abcnil"),
        (
            Some(Value::String("abc".into())),
            Value::String("def".into()),
            "abcdef",
        ),
        (Some(Value::Int(42)), Value::String("abc".into()), "42abc"),
        (
            Some(Value::String("abc".into())),
            Value::Float(1.5),
            "abc1.5",
        ),
        (Some(Value::Bool(true)), Value::Bool(false), "truefalse"),
        (Some(Value::Char('中')), Value::Char('文'), "中文"),
    ] {
        let got = concat_reduce(cur, inc);
        assert_eq!(
            got.unwrap(),
            Value::String(expected.into()),
            "concat 结果不对"
        );
    }
}

/// **`concat_reduce(None, Nil)` 得 `"nil"`** —— 这是 `Display` 的**既定语义**，
/// 不是缺陷。
///
/// `concat_reduce` 只有一个调用方（`pregel/mod.rs:1352` 的
/// `MirReducerKind::Concat`），载荷可以是任意 `Value`；`Display` 把
/// `Nil` 渲染成 `"nil"` 与全语言一致（D332/D333 记过 `v.to_string()`
/// 键空间）。
///
/// 本条把该行为**钉住**，防止将来被误判成缺陷而"修复"。
#[test]
fn d378_concat_of_nil_follows_display_semantics() {
    assert_eq!(
        concat_reduce(None, Value::Nil).unwrap(),
        Value::String("nil".into())
    );
    assert_eq!(
        concat_reduce(Some(Value::Nil), Value::Nil).unwrap(),
        Value::String("nilnil".into())
    );
    // Dict/List 经 Display 得 `{k: 1}` / `[1, 2]`（**不是** JSON）—— 同样是 Display 契约
    let d = concat_reduce(
        Some(Value::Dict(
            [("k".to_string(), Value::Int(1))].into_iter().collect(),
        )),
        Value::Nil,
    )
    .unwrap();
    assert_eq!(d, Value::String("{k: 1}nil".into()), "dict 经 Display 拼接");
}

/// **`parse_custom_merge_expr` 的启发式**：能解析成 `i64` 的走字面量，
/// 其余一律当**变量引用**。
#[test]
fn d378_custom_merge_expr_heuristic() {
    use mora::mir::witness::WitnessKind;
    for (s, is_int) in [
        ("42", true),
        ("-7", true),
        ("0", true),
        ("x", false),
        ("", false),
        ("a b", false),
        // ⚠ 小数**不**能 parse 成 i64 ⇒ 落到变量引用（哪怕它看起来该是 Float）
        ("3.5", false),
    ] {
        let w = parse_custom_merge_expr(s);
        match (&w.kind, is_int) {
            (WitnessKind::Literal(_), true) => {}
            (WitnessKind::Variable(_), false) => {}
            other => panic!("{s:?} 的解析结果不符预期: {other:?}"),
        }
    }
}

/// **`build_per_key_strategies` 只收**有静态映射**的 reducer**。
///
/// `MirReducerKind::to_merge_strategy`（`orchestrate/mod.rs:313-314`）
/// **明文规定**：`Merge` / `Sum` / `Product` / `Concat` / `Custom`
/// **无静态映射**，返回 `None` —— 它们走 `pregel/mod.rs:1352` 的
/// `accumulator_reduce` / `concat_reduce` 自定义执行路径。
///
/// ⚠ 首版我期望 `Sum` 进表 ⇒ 假红。判据改成**钉住这个分工**：
/// 只有 `Last` / `Append` / `Add` / `GrowOnly` 进表。
#[test]
fn d378_per_key_strategies_built_from_schema() {
    use mora::mir::orchestrate::{MirReducerKind, MirStateChannel};
    use mora::value::MergeStrategy;

    let mk = |name: &str, reducer: MirReducerKind| MirStateChannel {
        name: name.to_string(),
        ty: "number".to_string(),
        reducer,
    };
    let schema = vec![
        // 有静态映射 ⇒ 进表
        mk("last", MirReducerKind::Last),
        mk("append", MirReducerKind::Append),
        mk("add", MirReducerKind::Add),
        mk("grow", MirReducerKind::GrowOnly),
        // 无静态映射（`orchestrate/mod.rs:328-332`）⇒ 不进表
        mk("sum", MirReducerKind::Sum),
        mk("product", MirReducerKind::Product),
        mk("concat", MirReducerKind::Concat),
        mk("custom", MirReducerKind::Custom("x".into())),
    ];
    let map: HashMap<String, MergeStrategy> = build_per_key_strategies(&schema);
    for k in ["last", "append", "add", "grow"] {
        assert!(
            map.contains_key(k),
            "`{k}` 有静态映射，应进表; 实得 {map:?}"
        );
    }
    for k in ["sum", "product", "concat", "custom"] {
        assert!(
            !map.contains_key(k),
            "`{k}` 的 `to_merge_strategy()` 是 None（需自定义执行），不该进表; 实得 {map:?}"
        );
    }
    // 空 schema ⇒ 空表
    assert!(
        build_per_key_strategies(&[]).is_empty(),
        "空 schema 应得空表"
    );
}
