//! v0.104.6 D124：dict **字面量**同质约束的诊断质量（spec 补注 + 错误信息）。
//!
//! ## 缺陷性质：不是「算错」，是「说不清」
//!
//! `{k: 5, s: "x"}` 被 typeck 拒绝，**这是正确的**（`dict<K, V>` 是泛型，
//! HM 推断需要单一 `V`；运行时 `Value::Dict(HashMap<String, Value>)` 本身
//! 可存异质值，限制只作用于字面量）。但修复前的诊断是：
//!
//! ```text
//! Type error at line 1:9: Type mismatch: expected Float, got String at line 1, column 9
//! ```
//!
//! 两个问题：
//! 1. **没说清为什么** —— 用户看到「类型不匹配」只会以为自己写错了类型，
//!    猜不到这里有一条「dict 字面量必须同质」的约束（它挡住
//!    `{status: 200, body: "…"}` 这类自然写法）。
//! 2. **span 指向整个 dict 的 `{`**（column 9），而不是出问题的那个值。
//!
//! ## 处置
//!
//! - spec §14.2 的 `dict_literal` 产生式旁补注该约束（实现是对的，文档漏了 ——
//!   与 D108 / D109 / D116 / D117 同类）。
//! - typeck 新增专门的 `TypeError::DictValueTypeMismatch`（HM 侧），
//!   消息说明**约束本身**、带上**键名**，span 指向**出问题的值**。

use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 一条诊断的「消息 + 位置」。
///
/// ⚠ 对外的 `typeck::TypeError` 是**结构体**（无 `Display`）；CLI 的
/// `format_error` 才会渲染成 `Type error at line L:C: <message>`。
/// 故这里取字段，**不**拼字符串 —— 拼了就把判据绑死在渲染格式上。
fn typeck_errors(src: &str) -> Vec<(String, usize, usize)> {
    let (_f, wits) = ParserV3::compile(src).expect("语法应通过");
    check_program_witnesses_bidirectional(&wits)
        .iter()
        .map(|e| (e.message.clone(), e.line, e.column))
        .collect()
}

/// 诊断必须**说清约束 + 带上键名**，而不是笼统的「类型不匹配」。
#[test]
fn d124_dict_heterogeneity_error_explains_the_constraint_and_names_the_key() {
    let errs = typeck_errors("let d = {k: 5, s: \"x\"}\nprint(d)\n");
    assert_eq!(errs.len(), 1, "应恰好一条错误，实际: {errs:?}");
    let (msg, _, _) = &errs[0];
    let msg: &str = msg;
    assert!(
        msg.contains("同质"),
        "诊断必须说明「值必须同质」这条约束，否则用户无从知道原因: {msg}"
    );
    assert!(
        msg.contains("dict<K, V>") || msg.contains("dict<K,V>"),
        "诊断应点明约束来自 `dict<K, V>` 的单一 V: {msg}"
    );
    assert!(
        msg.contains("'s'"),
        "诊断必须点名**出问题的键**（这里是 's'）: {msg}"
    );
    assert!(
        msg.contains("Float") && msg.contains("String"),
        "诊断应同时给出两种类型，便于自查: {msg}"
    );
    // 旧的笼统措辞不应再出现
    assert!(
        !msg.contains("Type mismatch: expected"),
        "旧的通用 `UnificationFailure` 措辞不应再用于 dict 同质约束: {msg}"
    );
    // 位置不应在消息里重复（对外 format_error 会用 line/column 字段渲染）
    assert!(
        !msg.contains("at line"),
        "消息里不应重复位置（`format_error` 会加 `at line L:C`）: {msg}"
    );
}

/// span 必须指向**出问题的那个值**，不是整个 dict 的 `{`。
#[test]
fn d124_dict_heterogeneity_error_points_at_the_offending_value() {
    // `{k: 5, s: "x"}` —— 出问题的是第二个值（键 's'）
    let errs = typeck_errors("let d = {k: 5, s: \"x\"}\nprint(d)\n");
    let (_, line, col) = &errs[0];
    assert_eq!(*line, 1, "错误应在第 1 行");
    // 旧实现指向 dict 起始的 `{`（line 1, column 9）；新实现应更靠后
    assert!(
        *col > 9,
        "span 应指向出问题的值（`s` 的位置），而不是 dict 起始的 `{{`（column 9）; \
         实测 column {col}"
    );

    // 反向对照：键顺序反过来，出问题的是**第一个值**之后的那个，span 仍应 > 9
    let errs2 = typeck_errors("let d = {s: \"x\", k: 5}\nprint(d)\n");
    let (msg2, _, col2) = &errs2[0];
    assert!(
        *col2 > 9,
        "反向形态的 span 同样应指向出问题的值; 实测 column {col2}: {msg2}"
    );
    assert!(msg2.contains("'k'"), "反向形态应点名键 'k': {msg2}");
}

/// 对照组：**同质** dict 一律无错（防止「一刀切」把合法写法也拒了）。
#[test]
fn d124_homogeneous_dicts_are_still_accepted() {
    for (name, src) in [
        ("single", "let d = {k: 5}\nprint(d)\n"),
        ("two_same_type", "let d = {k: 5, j: 6}\nprint(d)\n"),
        ("empty", "let d = {}\nprint(d)\n"),
        // 数值提升：`1` 与 `2.5` 在数值塔内应相容（D67 的 numeric promotion）
        ("numeric_promotion", "let d = {a: 1, b: 2.5}\nprint(d)\n"),
    ] {
        let errs = typeck_errors(src);
        assert!(
            errs.is_empty(),
            "[{name}] 同质 dict 不应报错，实际: {errs:?}"
        );
    }
}

/// 对照组：真正的**类型错误**（非同质）仍走原有的通用诊断，不被新变体吞掉。
#[test]
fn d124_unrelated_type_errors_keep_their_own_diagnostics() {
    // `let r: number = "x"` 是标注与右值不符，与 dict 同质无关
    let errs = typeck_errors("let r: number = \"x\"\nprint(r)\n");
    assert!(!errs.is_empty(), "标注类型不符必须仍然报错");
    assert!(
        !errs.iter().any(|(msg, _, _)| msg.contains("同质")),
        "与 dict 无关的类型错误不应被说成「同质」问题: {errs:?}"
    );
}
