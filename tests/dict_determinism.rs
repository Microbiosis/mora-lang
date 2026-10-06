//! 字典序列化的**可复现性**护栏。
//!
//! ## 缺陷
//!
//! `Value::Dict` 底层是 `HashMap<String, Value>`。Rust 的 `RandomState`
//! 每进程随机，因此直接 `map.iter()` 得到的键序**每次运行都不同**。而
//! `flow::json::value_to_json` 与 `Value` 的两个 `Display` 实现原先都是
//! 直接迭代，于是：
//!
//! - `json.stringify(dict)` 的输出不可复现；
//! - `print(dict)` 的输出不可复现；
//! - 任何对 JSON 文本做 diff / 哈希 / 缓存的地方会随机失配。
//!
//! 实测（修复前）连跑 5 次 `examples/hm_basic_demo.mora`，`int_dict` 出现
//! **5 种不同**的键序。
//!
//! ## 为什么用 12 个键
//!
//! `HashMap` 迭代序恰好等于键序的概率是 `1/n!`；n=12 时约 `2.4e-9`。
//! 也就是说「断言输出已排序」这条用例在修复前**几乎必然失败**、修复后
//! **必然通过** —— 它不是一条概率性的弱断言。
//!
//! ## 为什么不算语言语义变更
//!
//! JSON 对象的键序在语义上无关（RFC 8259 明确对象是**无序**的）；字典的
//! 相等性判定也走 `HashMap` 的 `PartialEq`，与迭代序无关。本改动只影响
//! **输出顺序**，不改变任何值、相等性或求值结果。
//!
//! 仓库内已有正确先例：`http_server.rs::value_to_json` 与
//! `mcp_server.rs::mora_to_json` 一直先收进 `BTreeMap` 再输出 —— 本次
//! 只是让核心的两处与它们对齐。

use std::sync::Arc;

fn run(src: &str) -> String {
    let (func, _w) = mora::parser_v3::ParserV3::compile(src).unwrap_or_else(|e| panic!("{e}"));
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    match mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v}"),
        Err(e) => panic!("run failed: {e}"),
    }
}

/// 12 个键，**按逆序书写** —— 逆序书写比正序更容易在无 bug 时也碰巧通过，
/// 但对 `HashMap` 而言书写顺序本来就不影响迭代序，这里只是让用例更严。
const KEYS: [&str; 12] = [
    "k12", "k11", "k10", "k09", "k08", "k07", "k06", "k05", "k04", "k03", "k02", "k01",
];

fn dict_literal() -> String {
    let body = KEYS
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{k}: {i}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

/// 从输出里抽出键序（形如 `a: 1` / `"a":1`）。
fn key_order(rendered: &str) -> Vec<String> {
    rendered
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            let k = part.split(':').next()?.trim();
            Some(k.trim_matches('"').to_string())
        })
        .collect()
}

fn assert_sorted(name: &str, rendered: &str, expected_keys: usize) {
    let order = key_order(rendered);
    assert_eq!(order.len(), expected_keys, "[{name}] 键数不符: {rendered}");
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(
        order, sorted,
        "[{name}] 键序未排序 —— Dict 迭代序泄漏到输出了:\n  got: {order:?}\n  rendered: {rendered}"
    );
}

/// `json.stringify(dict)` 的输出必须键序稳定（端到端，走真实解释器）。
///
/// 这条最关键：Mora 的 `Router` / `McpServer` / `ai.chat` 都会产出 JSON
/// 文本，键序随机意味着响应不可复现、无法写断言。
#[test]
fn json_stringify_dict_is_sorted() {
    let rendered = run(&format!("let d = {}\njson.stringify(d)\n", dict_literal()));
    assert_sorted("json.stringify/12", &rendered, KEYS.len());
}

/// `Value` 的 `Display`（`print` 走的就是它）必须键序稳定。
///
/// 直接在 Rust 层构造 `Value::Dict` 并 `to_string()`，避免依赖捕获 stdout
/// —— `print` 的**返回值**恒为 `Nil`，拿不到它打印的文本。
#[test]
fn value_display_dict_is_sorted() {
    use mora::value::Value;
    use std::collections::HashMap;

    let mut map: HashMap<String, Value> = HashMap::new();
    for (i, k) in KEYS.iter().enumerate() {
        map.insert((*k).to_string(), Value::Float(i as f64));
    }
    let rendered = Value::Dict(map).to_string();
    assert_sorted("Display/12", &rendered, KEYS.len());
}

/// 同一进程内两次求值必须给出同一输出（排除「同进程也不同」的退化情形）。
#[test]
fn repeated_evaluation_is_stable() {
    let src = format!("let d = {}\njson.stringify(d)\n", dict_literal());
    let first = run(&src);
    for i in 0..5 {
        let again = run(&src);
        assert_eq!(
            first, again,
            "第 {i} 次重复求值输出不一致:\n  {first}\n  {again}"
        );
    }
}

/// 小字典同样必须稳定 —— 覆盖真实代码里最常见的形态。
#[test]
fn small_dict_is_sorted() {
    let rendered = run("let d = {zeta: 1, alpha: 2, mid: 3}\njson.stringify(d)\n");
    assert_sorted("small", &rendered, 3);
}

// ────────────────── `keys()` / `values()`：语义级（非仅显示）──────────────

/// `d.keys()` 必须按 key 排序。
///
/// 这与上面几条性质不同：`keys()` 的返回**直接进入程序计算**，不是给人看的。
/// 修复前它每次运行返回不同顺序的列表，用户按 `keys()[0]` 取「第一个键」、
/// 或 `for k in d.keys()` 顺序处理，拿到的都是**随机结果**且不报错。
#[test]
fn dict_keys_is_sorted() {
    let rendered = run(&format!("let d = {}\nd.keys()\n", dict_literal()));
    assert_sorted("keys/12", &rendered, KEYS.len());
}

/// `d.values()[i]` 与 `d.keys()[i]` **必须指向同一个键** —— 这是配对不变式。
///
/// 实现上 `values()` 曾直接迭代 `HashMap::values()`，而 `keys()` 迭代
/// `HashMap::keys()`：二者的相对顺序在 `HashMap` 里本就**互不保证**，
/// 于是 `values()[i]` 与 `keys()[i]` 可能是**不同的键**。这条断言把
/// 「同序」这个要求变成可执行的检查。
#[test]
fn dict_values_aligns_with_keys_by_index() {
    let src = format!(
        "let d = {}\n\
         let ks = d.keys()\n\
         let vs = d.values()\n\
         let bad = 0\n\
         let i = 0\n\
         while i < len(ks)\n\
         \x20 if vs[i] != d.get(ks[i]) then\n\
         \x20   let bad = bad + 1\n\
         \x20 end\n\
         \x20 let i = i + 1\n\
         end\n\
         bad\n",
        dict_literal()
    );
    let misaligned = run(&src);
    // 注意本文件的 `run()` 返回的是 **Display**（非 Debug），故 Int 打印为 `0.0`
    // （Mora 的整数字面量在浮点上下文中是 Float）。
    assert_eq!(
        misaligned, "0.0",
        "values()[i] 与 keys()[i] 指向了不同的键（{KEYS:?}）"
    );
}

/// `d.values()` 的顺序必须与其 key 的排序一致（`values` 本身没有 key 可比）。
#[test]
fn dict_values_is_in_sorted_key_order() {
    // {zeta: 26, alpha: 1, mid: 13} → 按 key 排 → alpha, mid, zeta
    //                              → 对应值   1.0, 13.0, 26.0
    let rendered = run("let d = {zeta: 26, alpha: 1, mid: 13}\nd.values()\n");
    assert_eq!(
        rendered, "[1.0, 13.0, 26.0]",
        "values() 必须按 key 排序（alpha=1, mid=13, zeta=26）"
    );
}
