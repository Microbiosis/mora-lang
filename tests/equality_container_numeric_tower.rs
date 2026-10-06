//! v0.104.6 D391 —— `values_equal` 的**容器臂丢掉数值塔**：
//! 同一个值「单独相等、放进容器就不等」（修复轮）
//!
//! ## 缺陷
//!
//! `flow::values_equal`（`==` 运算符走的路径）在标量侧一路修齐了数值塔
//! （v0.103 `Int ⊂ Float`、D20 `BigInt` 混比、D199 精度），
//! 但**容器臂**仍是 `a == b` —— 那是 `list::List::eq` / `HashMap::eq`，
//! 内部逐元素调用 **`Value::eq`**，而 `Value::eq` **没有**任何跨数值类型的臂。
//!
//! 实测（真实 CLI）：
//!
//! ```text
//! j[0]         == 1.0        → true     ← 标量走数值塔
//! j            == [1.0, 2.0] → false    ← 容器丢掉数值塔
//! d.get("a")   == 1.0        → true
//! d            == {"a": 1.0} → false
//! ```
//!
//! ## 探针为什么必须用 `json.parse` 造值
//!
//! Mora 的**裸数字字面量是 `Float`**，所以 `[1]` 与 `[1.0]` 元素类型**完全相同**，
//! 根本没有混比可测；而 typeck 的**列表同质性**规则又直接拒掉
//! `[1, len("ab")]` 这类混比字面量（实测 exit 2 `expected: float, got: int`）。
//!
//! ⇒ 唯一能进入被测路径的办法是**运行时解析**：`json.parse("[1,2]")`
//! 给 `Int` 元素（D129），再与 Float 字面量列表比较。
//! 首版探针正是栽在这里 —— 它「测出」了 `list_int_float=true`，
//! 看起来像没问题，实则**根本没进入被测条件**。
//!
//! ## 修法：并集，不是替换
//!
//! 容器元素改走 `container_elem_eq(a, b) = values_equal(a, b) || a == b`。
//!
//! 取**并集**是有意为之：`values_equal` 只覆盖
//! Nil / 数值 / String / Char / Bool / List / Dict，
//! **没有** `Cons` / `Code` / `Curry` / `Document` / `Tea*` / `LogicVar` 的臂
//! （落 `_ => false`），而 `Value::eq` **有**。
//! 若容器改走纯 `values_equal` 递归，`[Cons{..}] == [Cons{..}]`
//! 会从 true 变成 false —— 那是**收窄**既有行为，等于引入新缺陷。
//!
//! 判据 `d391_structural_element_equality_is_not_narrowed` 专门守这条。

use std::path::Path;
use std::process::Command;

use mora::flow::values_equal;
use mora::value::Value;
use mora::value::list::List;

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

fn ints(xs: &[i64]) -> Value {
    Value::List(List::from(
        xs.iter().map(|n| Value::Int(*n)).collect::<Vec<_>>(),
    ))
}

fn floats(xs: &[f64]) -> Value {
    Value::List(List::from(
        xs.iter().map(|n| Value::Float(*n)).collect::<Vec<_>>(),
    ))
}

/// 单元素列表（`Value::List` 收的是持久化 `list::List`，不是 `Vec`）。
fn list_of(v: Value) -> Value {
    Value::List(List::from(vec![v]))
}

// ── ① 容器必须与标量用同一套数值塔 ──

/// **列表**：元素逐个走数值塔。
#[test]
fn d391_list_equality_uses_numeric_tower() {
    assert!(
        values_equal(&ints(&[1, 2]), &floats(&[1.0, 2.0])),
        "`Int[1,2]` 应等于 `Float[1,2]`（元素标量本就相等）"
    );
    // 与标量对照：不是「所有列表都相等」
    assert!(
        !values_equal(&floats(&[1.0]), &floats(&[1.5])),
        "`[1.0] == [1.5]` 仍应为 false —— 本条若也通过，说明判据没牙齿"
    );
}

/// **字典**：值逐个走数值塔。
#[test]
fn d391_dict_equality_uses_numeric_tower() {
    let a: Value = Value::Dict([("k".to_string(), Value::Int(1))].into_iter().collect());
    let b: Value = Value::Dict([("k".to_string(), Value::Float(1.0))].into_iter().collect());
    assert!(
        values_equal(&a, &b),
        "字典值 Int(1) 应等于 Float(1.0)（键相同、长度相同）"
    );
}

/// **嵌套**：内层列表同样走数值塔。
#[test]
fn d391_nested_container_uses_numeric_tower() {
    let a = list_of(ints(&[1]));
    let b = list_of(floats(&[1.0]));
    assert!(values_equal(&a, &b), "嵌套列表应逐层走数值塔");
}

// ── ② 反向对照：不得把「不等」也一起放过 ──

/// 不等的情形**必须仍然不等**。
#[test]
fn d391_inequality_is_not_widened() {
    let cases: &[(&str, Value, Value)] = &[
        ("数值不同", ints(&[1]), floats(&[1.5])),
        ("长度不同", ints(&[1, 2]), floats(&[1.0])),
        ("类型不同", ints(&[1]), list_of(Value::Bool(true))),
        (
            "字典键不同",
            Value::Dict([("a".into(), Value::Int(1))].into_iter().collect()),
            Value::Dict([("b".into(), Value::Float(1.0))].into_iter().collect()),
        ),
        (
            "字典长度不同",
            Value::Dict(
                [("a".into(), Value::Int(1)), ("b".into(), Value::Int(2))]
                    .into_iter()
                    .collect(),
            ),
            Value::Dict([("a".into(), Value::Float(1.0))].into_iter().collect()),
        ),
    ];
    for (why, a, b) in cases {
        assert!(
            !values_equal(a, b),
            "{why}：应判为不等，但判据放行了 —— `|| a == b` 把不该放的也放了"
        );
    }
}

/// **NaN** 仍不相等（`||` 不应把 `NaN` 变成真）。
#[test]
fn d391_nan_stays_unequal() {
    let a = list_of(Value::Float(f64::NAN));
    let b = list_of(Value::Float(f64::NAN));
    assert!(!values_equal(&a, &b), "NaN 列表不应相等");
}

// ── ③ 不收窄：`values_equal` 未覆盖但 `Value::eq` 覆盖的类型必须保持相等 ──

/// **`Cons` 元素仍按结构相等** —— 这是「不能改成纯 `values_equal` 递归」的理由。
///
/// `values_equal` 没有 `Cons` 臂（落 `_ => false`），`Value::eq` 有。
/// 若容器改走纯 `values_equal` 递归，本条会从 true 变 false ⇒ 引入新缺陷。
#[test]
fn d391_structural_element_equality_is_not_narrowed() {
    let mk = |n: i64| Value::Cons {
        car: Box::new(Value::Int(n)),
        cdr: Box::new(Value::Nil),
    };
    let a = list_of(mk(1));
    let b = list_of(mk(1));
    assert!(
        values_equal(&a, &b),
        "Cons 元素应仍按结构相等（不得因改数值塔而被收窄）"
    );
    assert!(!values_equal(&a, &list_of(mk(2))), "Cons 内容不同仍应不等");
    // 裸 Cons 之间（不经容器）：`values_equal` **从来没有** Cons 臂，
    // 恒落 `_ => false` —— 这是**既有行为**，本轮不改变它。
    // 真正需要守住的是上面两条：**容器内**的 Cons 元素仍按结构相等。
    assert!(
        !values_equal(&mk(7), &mk(7)),
        "裸 Cons 之间本就不由 `values_equal` 判定（无 Cons 臂）—— \
         若本条变红，说明有人给 `values_equal` 加了 Cons 臂，需重新评估"
    );
}

// ── ④ 端到端：真实 CLI 路径 ──

/// 真实脚本里容器与标量给出**一致**答案。
///
/// ⚠ 必须用 `json.parse` 造 `Int` 元素（见文件头「探针为什么必须用 json.parse」）。
#[test]
fn d391_e2e_container_matches_scalar_semantics() {
    let (code, out) = run_fixture("equality_container_tower.mora");
    assert_eq!(code, 0, "fixture 应成功; out={out}");
    for (needle, why) in [
        ("list_val_scalar=true", "元素标量相等"),
        ("list_eq=true", "容器比较须与标量一致"),
        ("dict_val_scalar=true", "字典值标量相等"),
        ("dict_eq=true", "字典比较须与标量一致"),
        ("control_list_neq=false", "不同数值仍应不等"),
        ("control_dict_neq=false", "不同数值仍应不等"),
        ("control_len_neq=false", "长度不同仍应不等"),
    ] {
        assert!(out.contains(needle), "缺 `{needle}`（{why}）; out={out}");
    }
}
