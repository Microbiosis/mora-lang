//! v0.104.6 D245 —— 入站 LSP `position` / `range` 的**负数**必须被收敛，
//! 且**出站 range 不得含荒谬数值**。
//!
//! ## 缺陷
//!
//! LSP 规定 `line` / `character` 从 0 起（`uinteger`），但那是**协议约束**，
//! 不是可依赖的输入。各 provider 各自写 `as_i64().unwrap_or(0) as usize`，
//! 而整数 `as` 是**回绕** —— `(-1i64) as usize == 18446744073709551615`。
//!
//! `server.rs::pos_of` 早已带 `.max(0)`（D194），而 `definition` / `hover` /
//! `formatting` 三处**各写了一遍**且全都没有守卫 ⇒ 仓库内两套行为
//! （与 D244 在 `checkpoint` 里遇到的形态完全一致）。
//!
//! ## 实测后果（修前，`hover_v3`，文档 `...let beta: Int = alpha\nbeta`）
//!
//! ```text
//! position {"line": -1, "character": 0}
//!   → offset 落到 text.len()，在**文件末尾**找到了标识符 `beta`
//!   → 返回 `let beta: Int`，range 的 line = 1.8446744073709552e19
//! ```
//!
//! 不是「无结果」，而是**静默返回错误内容 + 荒谬出站 range**。客户端照该
//! range 应用 rename / format 就会**改坏用户文件**。

use mora::lsp::json::{self, Value as J};
use mora::lsp::providers::{definition_v3, hover_v3};
use mora::lsp::server::DocumentState;
use std::collections::HashMap;

const URI: &str = "file:///t.mora";
/// 文档以标识符 `beta` 结尾 —— 这是「回绕落到末尾反而命中标识符」的
/// **最坏情况**：修前负 position 会返回一个**看似合理**的错误答案。
const DOC: &str = "let alpha: Int = 1\nlet beta: Int = alpha\nbeta";

fn docs() -> HashMap<String, DocumentState> {
    let mut m = HashMap::new();
    m.insert(
        URI.to_string(),
        DocumentState {
            uri: URI.to_string(),
            text: DOC.to_string(),
            version: 1,
            diagnostics: vec![],
        },
    );
    m
}

fn hover_params(line: i64, ch: i64) -> J {
    let s = format!(
        r#"{{"textDocument":{{"uri":"{URI}"}},"position":{{"line":{line},"character":{ch}}}}}"#
    );
    json::parse(&s).expect("params parse")
}

fn def_params(line: i64, ch: i64) -> J {
    let s = format!(
        r#"{{"textDocument":{{"uri":"{URI}"}},"position":{{"line":{line},"character":{ch}}}}}"#
    );
    json::parse(&s).expect("params parse")
}

/// 递归收集出站 JSON 里的所有 `Number`。
fn numbers(v: &J, out: &mut Vec<f64>) {
    match v {
        J::Number(n) => out.push(*n),
        J::Array(items) => items.iter().for_each(|i| numbers(i, out)),
        J::Object(m) => m.values().for_each(|i| numbers(i, out)),
        _ => {}
    }
}

/// 出站 JSON 里的每个数值都必须是**小整数**（≤ 文档行数/长度量级）。
///
/// 这是守恒判据：修前会吐出 `1.8446744073709552e19`，一眼就能否掉，
/// 且**不依赖**「结果应该等于什么」这种容易被误判的期望。
fn assert_no_insane_numbers(label: &str, v: &J) {
    let mut nums = Vec::new();
    numbers(v, &mut nums);
    for n in &nums {
        assert!(
            *n >= 0.0 && *n <= 64.0,
            "{label}: 出站 JSON 含荒谬数值 {n}（客户端会照此 range 应用编辑，\
             可能改坏用户文件）\n  {v:?}"
        );
    }
}

/// ① 负 position ≡ `(0, 0)`：退化到文档开头，而不是回绕到末尾。
#[test]
fn d245_negative_position_degrades_to_origin() {
    let d = docs();
    let origin = hover_v3(&d, &hover_params(0, 0)).expect("origin should respond");

    for (label, line, ch) in [
        ("line=-1", -1i64, 0i64),
        ("char=-1", 0i64, -1i64),
        ("both negative", -1i64, -1i64),
        ("large negative", -100_000i64, -100_000i64),
    ] {
        let got = hover_v3(&d, &hover_params(line, ch)).expect("should respond");
        assert_eq!(
            got, origin,
            "{label}: 负 position 未退化到 (0,0)\n  origin = {origin:?}\n  got    = {got:?}"
        );
        assert_no_insane_numbers(&format!("hover {label}"), &got);
    }
}

/// ② definition 侧同理（它与 hover 是两处独立实现）。
#[test]
fn d245_definition_negative_position_degrades_to_origin() {
    let d = docs();
    let origin = definition_v3(&d, &def_params(0, 0));

    for (label, line, ch) in [
        ("line=-1", -1i64, 0i64),
        ("char=-1", 0i64, -1i64),
        ("both negative", -1i64, -1i64),
    ] {
        let got = definition_v3(&d, &def_params(line, ch));
        assert_eq!(
            got, origin,
            "definition {label}: 负 position 未退化到 (0,0)"
        );
        assert_no_insane_numbers(&format!("definition {label}"), &got);
    }
}

/// ③ 对照组：把成因钉在**语言事实**上，不钉 `pos_of`（D235 教训）。
#[test]
fn d245_control_group_negative_as_usize_wraps_to_max() {
    assert_eq!(
        (-1i64) as usize,
        usize::MAX,
        "负 i64 as usize 应回绕成 usize::MAX"
    );
    // 正侧不受影响（对照组另一半：说明缺陷只在负侧）。
    assert_eq!(0i64 as usize, 0);
    assert_eq!(37i64 as usize, 37);
}

/// ④ 正例不回归：正常位置仍命中正确标识符。
#[test]
fn d245_valid_positions_still_resolve_correctly() {
    let d = docs();
    // 第 1 行是 `let beta: Int = alpha`：
    //   0-2 `let` | 4-7 `beta` | 10-12 `Int` | 16-20 `alpha`
    // 取 **17**（`alpha` 内部）。曾误取 13 ⇒ 命中的是 `Int`（10-12），
    // 返回 `variable Int: <inferred>` —— 红的不是产品，是我的坐标。
    let r = hover_v3(&d, &hover_params(1, 17)).expect("valid position should respond");
    assert_no_insane_numbers("valid hover", &r);
    let text = format!("{r:?}");
    assert!(text.contains("alpha"), "正常位置应命中 alpha，实际 {text}");
    // (0,0) 落在 `let` 关键字上，仍须返回**合法** JSON 而非荒谬数值。
    let origin = hover_v3(&d, &hover_params(0, 0)).expect("origin should respond");
    assert_no_insane_numbers("origin hover", &origin);
}
