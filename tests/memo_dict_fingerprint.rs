//! 增量 memo 对 `dict` 输入的指纹 —— 防「同长度不同内容」返回陈旧值。
//!
//! ## 缺陷（2026-09-28 修）
//!
//! `src/mir/vm/engine.rs` 的 `value_fp` 对 `Value::Dict` 曾只取**长度**：
//!
//! ```ignore
//! Value::Dict(d) => InputFp::Str(d.len(), 0)
//! ```
//!
//! 而 `MirInst::Index(..)` **在 `is_memoizable_pure` 白名单里**，`d["a"]` 正好
//! 降级成 `Index`。于是循环里
//!
//! ```text
//! let n = 0
//! let s = 0
//! while n < 4
//!   let d = {a: n}      // 长度恒为 1，内容逐轮变
//!   let s = s + d["a"]
//!   let n = n + 1
//! end
//! s
//! ```
//!
//! 长度恒定 → 指纹每轮相同 → memo 命中 → `d["a"]` 永远返回**第一轮**的 0，
//! 结果 **0.0**（应为 6.0）。**无报错、静默错值。**
//!
//! ## 最有力的证据：同语义、两种写法、两种结果
//!
//! | 写法      | 降级为                            | 结果     |
//! |-----------|-----------------------------------|----------|
//! | `d.get("a")` | `Call`（不在白名单）→ 必重算     | **6.0** ✓ |
//! | `d["a"]`  | **`Index`**（在白名单）→ 会被跳过  | **0.0** ✗ |
//!
//! 两者语义完全相同，结果却不同 —— 唯一差别就是 memo 命中与否。
//!
//! ## 修法
//!
//! 指纹改为**与顺序无关的内容哈希**（逐项哈希后异或，HashMap 迭代序不影响），
//! 长度混入高位。代价是 dict 指纹从 O(1) 变 O(k)，但 dict 作为纯节点输入极罕见，
//! 正确性优先。

use std::sync::Arc;

fn run(src: &str) -> String {
    let (prog, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    match execute_program(arc, &mut core) {
        Ok((v, _)) => format!("{v}"),
        Err(e) => format!("ERR: {e}"),
    }
}

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;

fn cases() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "index_dict_len_const_1",
            "let n = 0\nlet s = 0\nwhile n < 4\n  let d = {a: n}\n  let s = s + d[\"a\"]\n  let n = n + 1\nend\ns\n",
            "6",
        ),
        (
            "index_dict_len_const_2",
            "let n = 0\nlet out = 0\nwhile n < 3\n  let d = {a: n, b: 9}\n  let out = d[\"a\"]\n  let n = n + 1\nend\nout\n",
            "2",
        ),
        (
            "index_dict_accumulate",
            "let n = 0\nlet s = 0\nwhile n < 4\n  let d = {a: n}\n  let s = s + d[\"a\"]\n  let n = n + 1\nend\ns + 0.0\n",
            "6.0",
        ),
        (
            "index_bracket_vs_get_same_length",
            "let n = 0\nlet b = 0\nlet g = 0\nwhile n < 4\n  let d = {a: n}\n  let b = b + d[\"a\"]\n  let g = g + d.get(\"a\")\n  let n = n + 1\nend\nif b == g then 1 else 0 end\n",
            "1",
        ),
        (
            "index_list_control",
            "let n = 0\nlet s = 0\nwhile n < 4\n  let xs = [n]\n  let s = s + xs[0]\n  let n = n + 1\nend\ns\n",
            "6",
        ),
        (
            "pure_binop_control",
            "let n = 0\nlet s = 0\nwhile n < 4\n  let s = s + n\n  let n = n + 1\nend\ns\n",
            "6",
        ),
        (
            "index_dict_len_varies",
            "let n = 0\nlet s = 0\nwhile n < 3\n  let d = {a: n, b: n, c: n}\n  let s = s + d[\"a\"]\n  let n = n + 1\nend\ns\n",
            "3",
        ),
    ]
}

#[test]
fn dict_memo_fingerprint_does_not_alias_same_length_dicts() {
    let all = cases();
    let mut failures = Vec::new();
    for (name, src, expected) in &all {
        let got = run(src);
        if &got != expected {
            failures.push(format!(
                "  [{name}]\n    src      = {src:?}\n    expected = {expected}\n    actual   = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 个 memo 指纹用例与基线不符：\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}
