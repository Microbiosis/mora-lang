//! v0.104.6 D290：JSON **数字**往返的穷举不变式（补 D223 只覆盖字符串的缺口）
//!
//! ## 为什么补这条
//!
//! D223 已经对 `value_to_json` → `json_to_value` 做了**逐码点穷举**的字符串
//! 往返（0x00–0x2FF + 非 BMP），并在文件头写明这套「穷举往返」形状
//! **两次钓出真缺陷**（`memory` 往返 D220/D221、`\uXXXX` 缺失 D206）。
//!
//! 但它的「值形状」组里**数字只有两个**：`Int(-42)` 与 `Float(1.5)`。
//! 而本项目有完整的**数值塔**（`Int` / `Float` / `BigInt`），
//! 且 D246 刚把 `value_as_f64` 的 `BigInt` 分支立成收口。
//!
//! ## 本条测什么
//!
//! | 面 | 规模 | 期望 |
//! |---|---|---|
//! | 有限 `f64` | **20 万个随机位型**（跳过 NaN）+ 21 个特殊/边界值 | **逐位恒等**（`to_bits` 相等） |
//! | `Int` | 9 个边界（`i64::MIN/MAX`、0、±1…） | 恒等 |
//! | `BigInt` | 7 个（含 `2^127-1` / `-2^127`） | 恒等 |
//! | 容器内数字 | dict 嵌 float/int/NaN/空 dict | 与顶层一致 |
//!
//! ## 唯一允许的损失：非有限值 → `null`
//!
//! `inf` / `-inf` / `NaN` 写出为 `null` —— 这是 D251 定的线格式决定
//! （标准 JSON **没有** inf/nan，两个手写序列化器都如此降级）。
//! 本条把它**显式钉住**为「唯一例外」，而不是假装它不存在。
//!
//! ⚠ 若将来线格式改为能表达非有限值，本条会**红**（那时删掉该例外即可）。

use std::collections::HashMap;

use mora::flow::{json_to_value, value_to_json};
use mora::value::Value;

fn rt(v: &Value) -> Result<Value, String> {
    json_to_value(&value_to_json(v))
}

fn lst(v: Vec<Value>) -> Value {
    Value::List(v.into())
}

/// 伪随机但确定性（xorshift64）—— 保证失败可复现，且不依赖 `rand` 依赖。
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// **主判据**：20 万个随机 `f64` 位型，**逐位**往返恒等。
///
/// 用 `to_bits()` 比而不是 `==`：`0.0` 与 `-0.0` 在 `==` 下相等但
/// **位型不同**（符号位），`==` 会把它们当成「往返成功」而漏掉符号丢失。
#[test]
fn d290_finite_f64_round_trip_is_bit_identical() {
    const N: usize = 200_000;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut checked = 0usize;
    let mut bad: Vec<String> = Vec::new();

    for _ in 0..N {
        let f = f64::from_bits(rng.next());
        if f.is_nan() {
            continue; // NaN 的 payload 不规范，单列
        }
        checked += 1;
        match rt(&Value::Float(f)) {
            Ok(Value::Float(g)) if g.to_bits() == f.to_bits() => {}
            Ok(g) => bad.push(format!(
                "{f:e} (bits {:#018x}) → {}",
                f.to_bits(),
                value_to_json(&g)
            )),
            Err(e) => bad.push(format!("{f:e} → 解析失败 {e}")),
        }
        if bad.len() >= 10 {
            break;
        }
    }
    assert!(
        bad.is_empty(),
        "有限 f64 的 JSON 往返**有损**（{}/{} 个位型）：\n  {}",
        bad.len(),
        checked,
        bad.join("\n  ")
    );
}

/// 边界与特殊值：有限值逐位恒等；非有限值**必须**降级为 `null`。
#[test]
fn d290_float_boundaries_and_non_finite() {
    let finite: Vec<f64> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        1.5,
        1e-300,
        -1e-300,
        1e300,
        -1e300,
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::MIN,
        1e21,
        1e-7,
        0.1,
        1.0 / 3.0,
        9007199254740993.0,
    ];
    for f in finite {
        match rt(&Value::Float(f)) {
            Ok(Value::Float(g)) => assert_eq!(
                g.to_bits(),
                f.to_bits(),
                "f64 {f:e} 往返位型不同（{:#018x} → {:#018x}）",
                f.to_bits(),
                g.to_bits()
            ),
            Ok(g) => panic!("f64 {f:e} 往返变成了非 Float：{}", value_to_json(&g)),
            Err(e) => panic!("f64 {f:e} 往返解析失败：{e}"),
        }
    }
    // 非有限值 → null（D251 定的线格式）
    for f in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(
            value_to_json(&Value::Float(f)),
            "null",
            "非有限值应写出为 null（标准 JSON 无 inf/nan，D251 决定）"
        );
    }
}

/// `Int` 边界往返恒等。
#[test]
fn d290_int_boundaries_round_trip() {
    for i in [
        0i64,
        1,
        -1,
        42,
        -42,
        i64::MAX,
        i64::MIN,
        i64::MAX - 1,
        i64::MIN + 1,
    ] {
        match rt(&Value::Int(i)) {
            Ok(Value::Int(g)) => assert_eq!(g, i, "Int {i} 往返不等"),
            Ok(g) => panic!("Int {i} 往返变成非 Int：{}", value_to_json(&g)),
            Err(e) => panic!("Int {i} 往返解析失败：{e}"),
        }
    }
}

/// `BigInt` 往返恒等；**小值降级为 `Int` 是数值塔的正常行为**。
#[test]
fn d290_bigint_round_trip() {
    for s in [
        "123456789012345678901234567890",
        "-123456789012345678901234567890",
        "170141183460469231731687303715884105727", // 2^127-1
        "-170141183460469231731687303715884105728", // -2^127
    ] {
        let v = Value::BigInt(s.parse::<num_bigint::BigInt>().expect("应可解析"));
        match rt(&v) {
            Ok(got) => assert_eq!(got, v, "BigInt {s} 往返不等，读回 {}", value_to_json(&got)),
            Err(e) => panic!("BigInt {s} 往返解析失败：{e}"),
        }
    }
    // 小 BigInt 降级成 Int —— 数值塔的既定行为（`value_as_f64` 亦同）
    for s in ["0", "1", "-1"] {
        let v = Value::BigInt(s.parse::<num_bigint::BigInt>().expect("应可解析"));
        match rt(&v) {
            Ok(Value::Int(_)) => {}
            Ok(got) => panic!("小 BigInt {s} 应降级为 Int，读回 {}", value_to_json(&got)),
            Err(e) => panic!("小 BigInt {s} 往返解析失败：{e}"),
        }
    }
}

/// 容器内嵌数字：与顶层**同样**的语义（嵌套的 NaN 也降级为 `null`）。
#[test]
fn d290_numbers_inside_containers_behave_the_same() {
    let d = Value::Dict(
        [
            ("f".to_string(), Value::Float(1.5)),
            ("i".to_string(), Value::Int(-42)),
            ("n".to_string(), Value::Float(f64::NAN)),
            ("e".to_string(), Value::Dict(HashMap::new())),
            (
                "l".to_string(),
                lst(vec![Value::Int(1), Value::Float(-0.5)]),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let json = value_to_json(&d);
    assert!(
        json.contains("\"n\":null"),
        "嵌套的 NaN 也应写出为 null（与顶层一致）：{json}"
    );
    let got = json_to_value(&json).unwrap_or_else(|e| panic!("嵌套往返解析失败：{e}"));
    match &got {
        Value::Dict(m) => {
            assert_eq!(m.get("f"), Some(&Value::Float(1.5)), "f 丢失：{json}");
            assert_eq!(m.get("i"), Some(&Value::Int(-42)), "i 丢失：{json}");
            assert_eq!(
                m.get("e"),
                Some(&Value::Dict(HashMap::new())),
                "e 丢失：{json}"
            );
            // NaN 降级成 Nil（null）
            assert_eq!(m.get("n"), Some(&Value::Nil), "n 未按 null 降级：{json}");
        }
        other => panic!("往返后不再是 dict：{other:?}"),
    }
}
