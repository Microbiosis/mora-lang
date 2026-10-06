//! D233 判据：`Checkpoint` 的 JSON 往返**不得**静默丢失状态。
//!
//! ## 缺陷背景
//!
//! `Checkpoint` 的文档写「Captures the **complete** state」，用途是
//! fault recovery / time-travel debugging / human-in-the-loop —— 即**恢复状态**。
//! 但 `to_json` 把 `Value` 交给 `flow::value_to_json`，而后者对不可 JSON 化的
//! 变体输出**占位字符串**（`"<agent X>"` / `"<conversation X>"` / `"null"` …）。
//!
//! | 存入 `channel_values` | 修前读回 |
//! |---|---|
//! | `Char('中')` | `String("中")` |
//! | `Code("fn main() {}")` | `String("fn main() {}")` |
//! | `Agent { .. }` | `String("<agent worker>")` |
//! | `Conversation { .. }` | `String("<conversation gpt>")` |
//! | `HttpRequest { .. }` | `String("<http_request GET /x>")` |
//!
//! `to_json` 返回 `Ok`、**零诊断**；`restore_checkpoint` 再把这个字符串写回
//! `channels`，引擎拿着**损坏的状态**继续跑。
//!
//! ## 修法
//!
//! 序列化**前**用 `is_roundtrip_faithful` 检查；不可逆 → 报错。
//! `pregel::run` 的 `saver.save(&thread_id, &cp)?` 会把错误向上传播，
//! 即「宁可明确失败，也不静默写坏检查点」。
//!
//! ⚠ 对 `json.stringify` builtin 而言占位串是**合理取舍**（函数/Agent 本来就
//! 无法 JSON 化）；对 checkpoint 而言是**缺陷**（目标是恢复，不是展示）。
//! 两处语义不同，故检查放在 checkpoint 层。
//!
//! ## 判据形态
//!
//! **正反两侧**都要：可表示的必须**往返恒等**（否则过度收紧也是缺陷），
//! 不可表示的必须**报错**（不能退回静默占位）。

use mora::checkpoint::{Checkpoint, SendTask};
use mora::value::Value;
use std::collections::HashMap;

fn with_channels(values: Vec<(&str, Value)>) -> Checkpoint {
    let mut cv = HashMap::new();
    for (k, v) in values {
        cv.insert(k.to_string(), v);
    }
    let mut cv_ver = HashMap::new();
    cv_ver.insert("ch".to_string(), 1u64);
    Checkpoint::new("t1".to_string(), 3, cv, cv_ver, HashMap::new(), vec![])
}

fn with_send(input: Value) -> Checkpoint {
    let mut cv = HashMap::new();
    cv.insert("ch".to_string(), Value::Int(1));
    let mut cv_ver = HashMap::new();
    cv_ver.insert("ch".to_string(), 1u64);
    Checkpoint::new(
        "t1".to_string(),
        1,
        cv,
        cv_ver,
        HashMap::new(),
        vec![SendTask {
            target_node: "n2".into(),
            input,
        }],
    )
}

/// 判据 ①：JSON 可表示的值必须**逐类型往返恒等**。
///
/// 这条防「过度收紧」—— 若修法把 `Char` / `Code` 之外的东西也误判为不可逆，
/// 或把可表示的值也拦下，本条会红。
#[test]
fn d233_json_representable_values_round_trip_faithfully() {
    let cases: Vec<(&str, Value)> = vec![
        ("nil", Value::Nil),
        ("bool-true", Value::Bool(true)),
        ("bool-false", Value::Bool(false)),
        ("int-pos", Value::Int(42)),
        ("int-neg", Value::Int(-42)),
        ("int-zero", Value::Int(0)),
        ("float-frac", Value::Float(1.5)),
        // v0.84：Float 必带小数点，否则往返变 Int（类型降级）
        ("float-whole", Value::Float(42.0)),
        ("float-neg-zero", Value::Float(-0.0)),
        ("float-tiny", Value::Float(1e-300)),
        ("float-big", Value::Float(1e300)),
        // D209：控制字符 / 引号 / 反斜杠 必须转义且可读回
        ("string-plain", Value::String("hello".into())),
        ("string-cjk", Value::String("中文测试".into())),
        ("string-emoji", Value::String("🌍🚀".into())),
        ("string-ctrl", Value::String("a\tb\nc\rd\"e\\f\u{1}".into())),
        ("string-empty", Value::String(String::new())),
        (
            "list-mixed",
            Value::List(
                vec![
                    Value::Int(1),
                    Value::String("a 世界".into()),
                    Value::Float(2.5),
                    Value::Bool(false),
                    Value::Nil,
                ]
                .into(),
            ),
        ),
        (
            "list-nested",
            Value::List(vec![Value::List(vec![Value::Int(9)].into())].into()),
        ),
        ("list-empty", Value::List(vec![].into())),
        (
            "dict-mixed",
            Value::Dict(
                [
                    ("n".to_string(), Value::Int(1)),
                    ("s".to_string(), Value::String("中\u{1}".into())),
                    (
                        "l".to_string(),
                        Value::List(vec![Value::Float(0.25)].into()),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
        ),
        ("dict-empty", Value::Dict(HashMap::new())),
    ];

    for (label, v) in cases {
        let cp = with_channels(vec![("ch", v.clone())]);
        let json = cp
            .to_json()
            .unwrap_or_else(|e| panic!("D233: 可表示的值 {label} 不该被拒: {e}"));
        let back = Checkpoint::from_json(&json)
            .unwrap_or_else(|e| panic!("D233: {label} 反序列化失败: {e}"));
        let got = back.channel_values.get("ch").cloned().unwrap();
        // Float 用位比较（NaN 之外应逐位相同）
        let identical = match (&v, &got) {
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
                        (Value::Float(p), Value::Float(q)) => p.to_bits() == q.to_bits(),
                        _ => x == y,
                    })
            }
            (Value::Dict(a), Value::Dict(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, x)| {
                        b.get(k).is_some_and(|y| match (x, y) {
                            (Value::Float(p), Value::Float(q)) => p.to_bits() == q.to_bits(),
                            _ => x == y,
                        })
                    })
            }
            _ => v == got,
        };
        assert!(
            identical,
            "D233: {label} 往返**不恒等**\n  存入 = {v:?}\n  读回 = {got:?}"
        );
    }
}

/// 判据 ②：不可逆的值必须**报错**，且错误信息点名类型。
#[test]
fn d233_unrepresentable_values_are_rejected_with_named_type() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("char", Value::Char('中'), "char"),
        ("code", Value::Code("fn main() {}".into()), "code"),
        (
            "agent",
            Value::Agent {
                name: "worker".into(),
                tool_names: vec!["t".into()],
                model_route: "m".into(),
                max_steps: 3,
                system: "s".into(),
            },
            "agent",
        ),
        (
            "conversation",
            Value::Conversation {
                messages: vec![("user".to_string(), "hi".to_string())],
                model: "gpt".into(),
                base_url: String::new(),
                api_key: String::new(),
            },
            "conversation",
        ),
        (
            "http_request",
            Value::HttpRequest {
                method: "GET".into(),
                path: "/x".into(),
                query: String::new(),
                body: Box::new(Value::Nil),
                params: HashMap::new(),
            },
            "http_request",
        ),
    ];

    for (label, v, expect_name) in cases {
        let cp = with_channels(vec![("ch", v)]);
        let err = cp.to_json().expect_err(&format!(
            "D233: {label} 不可无损往返，`to_json` 必须报错而不是静默写成占位串"
        ));
        assert!(
            err.contains(expect_name),
            "D233: 错误信息应点名是哪种类型。\n  label={label}\n  err={err}"
        );
        assert!(
            err.contains("channel_values"),
            "D233: 错误信息应指出**在哪**（channel 路径）。err={err}"
        );
    }
}

/// 判据 ③：**嵌套**位置也必须被检出（不能只查顶层）。
///
/// 修前的缺陷形态是「顶层直接放 `Agent`」；真实数据里更常见的是
/// 嵌在 list / dict 里 —— 只查顶层会漏。
#[test]
fn d233_nested_unrepresentable_values_are_detected() {
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "in-list",
            Value::List(vec![Value::Int(1), Value::Char('x'), Value::Int(2)].into()),
            "char",
        ),
        (
            "in-dict",
            Value::Dict(
                [
                    ("ok".to_string(), Value::Int(1)),
                    ("bad".to_string(), Value::Code("x".into())),
                ]
                .into_iter()
                .collect(),
            ),
            "code",
        ),
        (
            "deep-nested",
            Value::Dict(
                [(
                    "a".to_string(),
                    Value::List(
                        vec![Value::Dict(
                            [("b".to_string(), Value::Char('中'))].into_iter().collect(),
                        )]
                        .into(),
                    ),
                )]
                .into_iter()
                .collect(),
            ),
            "char",
        ),
    ];

    for (label, v, expect_name) in cases {
        let cp = with_channels(vec![("ch", v)]);
        let err = cp
            .to_json()
            .expect_err(&format!("D233: 嵌套在 {label} 的不可逆值必须被检出"));
        assert!(
            err.contains(expect_name),
            "D233: {label} 的错误信息应点名类型。err={err}"
        );
    }
}

/// 判据 ④：`pending_sends[].input` 同样受约束。
///
/// `input` 是节点间传递的消息（`restore_checkpoint` 也会恢复它），
/// 与 `channel_values` 同等重要。
#[test]
fn d233_pending_send_input_is_also_checked() {
    let cp = with_send(Value::Char('中'));
    let err = cp
        .to_json()
        .expect_err("D233: pending_sends[].input 里的不可逆值也必须报错");
    assert!(
        err.contains("pending_sends") && err.contains("char"),
        "D233: 错误信息应指出 pending_sends 路径与类型。err={err}"
    );

    // 可表示的 input 仍应正常往返
    let ok = with_send(Value::String("payload 世界".into()));
    let json = ok.to_json().expect("D233: 可表示的 input 不该被拒");
    let back = Checkpoint::from_json(&json).expect("from_json");
    assert_eq!(back.pending_sends.len(), 1);
    assert_eq!(back.pending_sends[0].target_node, "n2");
    assert_eq!(
        back.pending_sends[0].input,
        Value::String("payload 世界".into())
    );
}

/// 判据 ⑤：报错信息**稳定**（不随 HashMap 迭代序变化）。
///
/// `Value::Dict` 底层是 `HashMap`，`RandomState` 每进程随机 ⇒ 若遍历顺序
/// 不排序，多 channel 同时含不可逆值时**报错内容每次运行都不同**，
/// 无法写断言 / diff / 日志比对。
#[test]
fn d233_error_message_is_deterministic() {
    let mk = || {
        with_channels(vec![
            ("zzz", Value::Char('z')),
            ("aaa", Value::Code("a".into())),
            ("mmm", Value::Int(1)),
        ])
    };
    let first = mk().to_json().expect_err("必须报错");
    for _ in 0..8 {
        let again = mk().to_json().expect_err("必须报错");
        assert_eq!(
            first, again,
            "D233: 报错内容不稳定 —— 多处不可逆值时必须按确定顺序报告"
        );
    }
    // 排序后应报**字典序最小**的那个 key（aaa）
    assert!(
        first.contains("aaa"),
        "D233: 应按 key 排序报告（首个 = aaa）。err={first}"
    );
}

/// 判据 ⑥：`BigInt` 属于**可表示**一侧（实测钉住，不靠假设）。
///
/// `value_to_json` 把 `BigInt` 输出成**裸数字串**（不加引号），
/// 乍看像会丢类型。但 `json_to_value` 对**超出 i64 范围**的数字判为
/// `BigInt`（D586），于是「大整数」这一唯一会产出 `BigInt` 的场景
/// 恰好往返保真。
///
/// 这条判据是**先实测后归类**的：我第一版把 `BigInt` 从判据里删掉
/// 是不确定，探针跑完（`170141183460469231731687303715884105727`
/// 往返仍是 `BigInt`）才确认它属于白名单。
#[test]
fn d233_bigint_is_faithful_and_must_not_be_rejected() {
    for s in [
        "9223372036854775808",                     // i64::MAX + 1
        "170141183460469231731687303715884105727", // u128::MAX
    ] {
        let v =
            mora::flow::json_to_value(s).unwrap_or_else(|e| panic!("json.parse({s}) 失败: {e}"));
        assert!(
            matches!(v, Value::BigInt(_)),
            "前置：{s} 应被解析为 BigInt（否则本判据测不到东西）"
        );
        let cp = with_channels(vec![("ch", v.clone())]);
        let json = cp
            .to_json()
            .unwrap_or_else(|e| panic!("D233: BigInt({s}) 是可往返的，不该被拒: {e}"));
        let back = Checkpoint::from_json(&json).expect("from_json");
        assert_eq!(
            back.channel_values.get("ch").cloned().unwrap(),
            v,
            "D233: BigInt({s}) 往返必须恒等"
        );
    }
}
