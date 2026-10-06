//! v0.104.6 D99：`json.stringify` 对**非有限浮点**产出**非法 JSON**，
//! 语言自己的 `stringify → parse` 往返会断。
//!
//! ## 实测（修前）
//!
//! | 表达式 | `stringify` 输出 | 再 `parse` |
//! |---|---|---|
//! | `json.stringify([1.0/0.0, 2.0])` | `"[inf,2.0]"` | **失败**：`Unexpected character in JSON: inf,2.0` |
//! | `json.stringify(0.0/0.0)` | `"NaN"` | **失败**：`Unexpected character in JSON: NaN` |
//!
//! PowerShell 的真实 JSON 解析器同样拒绝 `[inf,2.0]`
//! （`Invalid JSON primitive: inf.`）—— 即**任何** JSON 消费方都会挂。
//!
//! ## 根因
//!
//! `flow/json.rs::value_to_json` 的 Float 分支：
//! ```ignore
//! if f.fract() == 0.0 { format!("{:.1}", f) } else { format!("{}", f) }
//! ```
//! `inf.fract()` 与 `NaN.fract()` **都是 `NaN`**，而 `NaN == 0.0` 恒为 `false`
//! → 走 `format!("{}", f)` → Rust 输出 `inf` / `NaN`，**都不是 JSON 字面量**。
//!
//! `1.0 / 0.0` 是最普通的算术，无需刻意构造 —— 故这不是边缘情形。
//!
//! ## 修法
//!
//! 非有限浮点序列化为 `null`（JSON 无法表示它们的通行做法，也与
//! `http_server` 自己的 `value_to_json` 行为一致 —— 它经 `JsonValue` 映射后
//! 已经输出 `null`，两处此前**不一致**）。

/// 跑一段源码，返回其末表达式值。
fn run(src: &str) -> Result<mora::value::Value, String> {
    use std::sync::Arc;
    let (func, _w) = mora::parser_v3::ParserV3::compile(src).map_err(|e| e.to_string())?;
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    let r = mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )?;
    mora::mir::vm::run_main_task(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )?;
    Ok(r)
}

fn s(src: &str) -> String {
    match run(src) {
        Ok(v) => v.to_string(),
        Err(e) => format!("Err({})", e),
    }
}

/// 正无穷不得出现在 JSON 输出里。
#[test]
fn d99_infinity_is_not_emitted_as_bare_inf() {
    let out = s(r#"json.stringify(1.0 / 0.0)"#);
    assert_ne!(
        out, "inf",
        "JSON 无 `inf` 字面量 —— 任何解析器都会拒绝（实测 PowerShell: Invalid JSON primitive: inf.）"
    );
    assert_eq!(out, "null", "非有限浮点应序列化为 null");
}

/// NaN 同理。
#[test]
fn d99_nan_is_not_emitted_as_bare_nan() {
    let out = s(r#"json.stringify(0.0 / 0.0)"#);
    assert_ne!(out, "NaN", "JSON 无 `NaN` 字面量");
    assert_eq!(out, "null", "非有限浮点应序列化为 null");
}

/// 列表里的非有限浮点同样要处理（递归路径）。
#[test]
fn d99_non_finite_inside_containers_is_handled() {
    let out = s(r#"json.stringify([1.0 / 0.0, 2.0])"#);
    assert_eq!(out, "[null,2.0]", "递归序列化同样要 null 化");
    let out2 = s(r#"json.stringify({a: 0.0 / 0.0})"#);
    assert!(
        out2.contains("null") && !out2.contains("NaN"),
        "dict 内的 NaN 也要 null 化，实际：{out2}"
    );
}

/// **核心不变量**：`stringify → parse` 往返必须不断。
/// 这是修前唯一会失败的性质。
#[test]
fn d99_stringify_parse_roundtrip_survives_non_finite() {
    for (name, src) in [
        ("inf", r#"json.stringify(1.0 / 0.0)"#),
        ("nan", r#"json.stringify(0.0 / 0.0)"#),
        ("list", r#"json.stringify([1.0 / 0.0, 2.0])"#),
    ] {
        let ser = s(&format!("let s = {}\n s", src));
        assert_ne!(ser, "Err", "{name}: stringify 自身应成功");
        let back = s(&format!("let v = json.parse({})\n v", quote(&ser)));
        assert!(
            !back.starts_with("Err"),
            "{name}: stringify 的输出必须能被自己的 parse 读回。\n  序列化结果: {ser}\n  解析结果: {back}"
        );
    }
}

/// 有限浮点**不能**被牵连（防「一刀切 null 化」的过度修复）。
#[test]
fn d99_finite_floats_are_untouched() {
    assert_eq!(s(r#"json.stringify(1.5)"#), "1.5");
    assert_eq!(
        s(r#"json.stringify(42.0)"#),
        "42.0",
        "整值浮点须保留小数点（D84 的类型对称性）"
    );
    assert_eq!(s(r#"json.stringify(0.0)"#), "0.0");
    assert_eq!(s(r#"json.stringify(-1.5)"#), "-1.5");
    assert_eq!(s(r#"json.stringify([1.5, 2.5])"#), "[1.5,2.5]");
}

/// 把序列化结果包成 Mora 字符串字面量，便于下一条语句回读。
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}
