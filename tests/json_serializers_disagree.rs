//! v0.104.6 D292：`lsp::json` 序列化器把整值 `Float` 写成**无小数点**形式
//! ⇒ HTTP/MCP 响应里的 Mora `Float(6.0)` 出去是 `6`，客户端读回 **Int**
//!
//! ## 运行时证实（D291 当时标注「运行时影响未证实」，本条**证实**了）
//!
//! 起一个真实 HTTP handler，POST `{"n": 5}`：
//!
//! | 端点（handler 体） | 结果 |
//! |---|---|
//! | `fn(req) => type_of(req.body.n)` | **`"float"`** ← 整数 JSON 字段变 `Float` |
//! | `fn(req) => req.body.n` | `5`（响应即 `5.0`，读回是 **Int**） |
//! | `fn(req) => req.body.n + 1` | `6`（`Float(6.0)`，**无小数点**） |
//!
//! 而 `json.parse("{\"n\":5}")` 给的是 `Int(5)` —— D129 的立论。
//! ⇒ **同一段 JSON，`json.parse` 与 HTTP 请求体两条路径类型不同。**
//!
//! ## 违反的既有约束（D84）
//!
//! `flow::json::value_to_json` 的注释写着：
//!
//! > v0.84：Float 必须始终输出小数点，即使 `fract() == 0.0`（如 `42.0 → "42.0"`），
//! > 以保持与 `parse_json_number` 的类型对称性。**若 Float 输出 "42"，反序列化后
//! > 会变成 `Int(42)`，类型降级不可逆。**
//!
//! 而 `src/lsp/json.rs` 的 `write_value` 恰恰这么做了：
//!
//! ```text
//! if n.fract() == 0.0 && n.abs() < 1e15 { write!(f, "{}", *n as i64) }
//! ```
//!
//! ## 本文件是**现状判据**：钉住两个序列化器当前的不一致
//!
//! 它今天通过，**恰恰是因为不一致还在**。统一之后本文件会**红** ——
//! 那时请把两处 `assert_ne!` 改成 `assert_eq!`。
//!
//! ## 为什么不擅自统一
//!
//! 改 `lsp::json` 会同时改 **LSP 协议字段**的线格式（`Range.line` 等
//! 按 spec 是整数，输出 `0.0` 虽合法但会动到所有 LSP 客户端）。
//! 改 `http_server::value_to_json` 则无法单独做到（`JsonValue::Number` 就是 f64）。
//! ⇒ 属**对外 wire format 决定**，需裁决。

use mora::flow::value_to_json;
use mora::value::Value;

/// 对照：`flow` 侧**遵守** D84（整值 Float 仍带小数点）。
#[test]
fn d292_flow_serializer_keeps_the_decimal_point() {
    for f in [6.0f64, 42.0, -3.0, 0.0] {
        let j = value_to_json(&Value::Float(f));
        assert!(
            j.contains('.'),
            "flow 侧应给 Float({f}) 保留小数点（D84），实得 {j}"
        );
    }
}

/// 现状：`lsp::json` 侧**丢掉**小数点（D84 未覆盖到这条路径）。
#[test]
fn d292_lsp_serializer_drops_the_decimal_point() {
    assert_eq!(
        mora::lsp::json::to_string(&mora::lsp::json::Value::Number(6.0)),
        "6",
        "现状：lsp::json 把整值 Float 写成无小数点形式。\
         统一之后本条会红 —— 请把 assert_eq! 改成 assert_ne! 并更新 CHANGELOG"
    );
}

/// 两个序列化器对**同一个 Mora `Float(6.0)`** 给出不同线格式。
///
/// 这条是本文件的核心断言：它直接对应实跑观测到的
/// 「`json.stringify` 给 `6.0`、HTTP 响应给 `6`」。
#[test]
fn d292_the_two_serializers_disagree_on_integral_float() {
    let v = Value::Float(6.0);
    let flow_side = value_to_json(&v);
    let lsp_side = mora::lsp::json::to_string(&mora::lsp::json::Value::Number(6.0));
    assert_eq!(flow_side, "6.0", "flow 侧应给 6.0（D84）");
    assert_eq!(lsp_side, "6", "lsp 侧当前给 6 —— 读回即 Int，与 D84 冲突");
    assert_ne!(
        flow_side, lsp_side,
        "现状：两个手写序列化器对同一个 Float(6.0) 线格式不同。\
         统一之后本条会红"
    );
}

/// `BigInt` 的线格式同样两路不同（D291 记录）。
#[test]
fn d292_the_two_serializers_disagree_on_bigint() {
    let bi: num_bigint::BigInt = "123456789012345678901234567890".parse().unwrap();
    let flow_side = value_to_json(&Value::BigInt(bi.clone()));
    let lsp_side = mora::lsp::json::to_string(&mora::lsp::json::Value::String_(bi.to_string()));
    assert_eq!(flow_side, "123456789012345678901234567890", "flow：裸数字");
    assert_eq!(
        lsp_side, "\"123456789012345678901234567890\"",
        "lsp 侧：带引号字符串"
    );
}
