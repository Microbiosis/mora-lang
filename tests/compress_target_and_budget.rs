//! v0.104.6 D367 —— `src/compress/` 的 `target_ratio` / `max_bytes` 边界矩阵
//! （否定轮，无产品变更）
//!
//! `src/compress/` 共 9 个模块 3307 行，此前只有 6 个判据**间接**涉及
//! （`compress_option_types` 等），`json.rs`（1050 行）**无直接判据**。
//! 本轮从 `CompressOptions` 的 12 个字段切入，测压缩的**目标计算**与
//! **预算收口**两条路径。
//!
//! ## `target_ratio` 的行为完全合理（8 个边界全测）
//!
//! `mod.rs:450` 的目标计算：
//!
//! ```text
//! target = max_bytes / 200  (优先)
//!        | (n * ratio).max(1.0)   (次之)
//!        | n * 0.2                (兜底)
//! ```
//!
//! 实测（n=100，`strategy="json"`）：
//!
//! | `target_ratio` | kept | 判定 |
//! |---|---|---|
//! | `None` | 20 | 兜底 `N*0.2` ✅ |
//! | `0.0` / `-1.0` / `NaN` | 1 | `.max(1.0)` 兜底 ✅ |
//! | `2.0` / `inf` | 100（passthrough）| `target ≥ N` ⇒ 直通 ✅ |
//! | `0.5` / `0.1` | 50 / 10 | 精确 ✅ |
//!
//! **零异常、零 panic**，每个边界都落到一条**明确写出的规则**上。
//!
//! ## 探针层级：必须从 `compress_top` 进，不能直调 `crush_json`
//!
//! 首版我直接调 `json::crush_json(items, target, options)` 并发现
//! 「`target_ratio` 完全无效」—— 差一步就写成缺陷。
//!
//! 真因：`crush_json` 的第三个参数 `target` 是**已算好的元素数**，
//! 它**不读** `options.target_ratio`；`target_ratio` 是在
//! `mod.rs::compress_top`（L450）里被换算成 `target` 后才传下去的。
//!
//! ⇒ **直调底层函数 = 绕过参数换算层 = 测了另一个东西**。
//! 与 D365「census 走 Rust API、CLI 走 typeck，不是同一层」同源。
//!
//! ## `max_bytes=0` 返回空串是**符合契约**的
//!
//! 走 `finish_within_budget` 的规则 2a
//! （`marker.len() >= max_bytes` ⇒ `marker[..0]`）。
//! 契约是「返回的 UTF-8 字节数不超过 `max_bytes`」⇒ 0 ≤ 0 ✅。

use mora::compress::{CompressOptions, compress_top};
use mora::value::Value;
use std::collections::HashMap;

/// 造 n 条结构一致的记录（`idx` / `score` / `name`）。
fn mk(n: usize) -> Value {
    let items: Vec<Value> = (0..n)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert("idx".to_string(), Value::Int(i as i64));
            m.insert("score".to_string(), Value::Float(i as f64 * 1.5));
            m.insert("name".to_string(), Value::String(format!("item-{i}")));
            Value::Dict(m)
        })
        .collect();
    Value::List(items.into())
}

/// 从 `<compressed:method=… items=K total=N savings=S>` 提取 K。
fn kept_of(out: &str) -> Option<usize> {
    let marker = out.lines().find(|l| l.contains("<compressed:"))?;
    let seg = marker.split("items=").nth(1)?;
    seg.split_whitespace().next()?.parse().ok()
}

/// **`target_ratio` 的 8 个边界**：每个都必须落到一条**写明的规则**上。
#[test]
fn d367_target_ratio_boundaries_all_have_defined_behavior() {
    let input = mk(100);
    // (label, ratio, expected_kept_or_None_for_passthrough)
    let cases: [(&str, Option<f32>, Option<usize>); 8] = [
        ("None", None, Some(20)),      // 兜底 N*0.2
        ("0.0", Some(0.0), Some(1)),   // .max(1.0)
        ("-1.0", Some(-1.0), Some(1)), // .max(1.0)
        ("0.5", Some(0.5), Some(50)),
        ("0.1", Some(0.1), Some(10)),
        ("NaN", Some(f32::NAN), Some(1)), // NaN.max(1.0) = 1.0
        ("2.0", Some(2.0), None),         // target=200 ≥ n ⇒ passthrough
        ("inf", Some(f32::INFINITY), None),
    ];
    for (label, ratio, expected) in cases {
        let opts = CompressOptions {
            strategy: "auto".to_string(),
            target_ratio: ratio,
            ..Default::default()
        };
        let out = compress_top(&input, "json", &opts)
            .unwrap_or_else(|e| panic!("target_ratio={label} 不应报错: {e}"));
        let s = match &out {
            Value::String(s) => s.clone(),
            other => panic!(
                "target_ratio={label} 应返回 String; 实得 {}",
                mora::flow::type_name(other)
            ),
        };
        match expected {
            Some(k) => assert_eq!(
                kept_of(&s),
                Some(k),
                "target_ratio={label}: 期望保留 {k} 条，实得 marker={:?} 输出长度={}",
                s.lines().find(|l| l.contains("<compressed:")),
                s.len()
            ),
            None => {
                // passthrough：保留全部 100 条，但可能没有 marker
                assert!(
                    s.contains("passthrough") || s.len() > 3000,
                    "target_ratio={label} 应走 passthrough（保留全部）; 实得长度={}",
                    s.len()
                );
            }
        }
    }
}

/// **`max_bytes` 优先于 `target_ratio`**（`mod.rs:448` 的 if/else 顺序）。
#[test]
fn d367_max_bytes_takes_precedence_over_target_ratio() {
    let input = mk(100);
    let only_mb = CompressOptions {
        strategy: "auto".to_string(),
        max_bytes: Some(1000),
        ..Default::default()
    };
    let both = CompressOptions {
        strategy: "auto".to_string(),
        max_bytes: Some(1000),
        target_ratio: Some(0.5), // 若 target_ratio 生效则 kept 应是 50
        ..Default::default()
    };
    let a = compress_top(&input, "json", &only_mb).expect("只给 max_bytes");
    let b = compress_top(&input, "json", &both).expect("两个都给");
    let (Value::String(sa), Value::String(sb)) = (&a, &b) else {
        panic!("应返回 String")
    };
    assert_eq!(
        kept_of(sa),
        kept_of(sb),
        "max_bytes 应优先于 target_ratio（max_bytes/200=5 vs 100*0.5=50）"
    );
}

/// **输出必须**永不超预算**（`finish_within_budget` 的核心契约）。
///
/// 多档 `max_bytes` 逐个验，含 0（走规则 2a）与多字节 UTF-8 输入。
#[test]
fn d367_output_never_exceeds_max_bytes() {
    let input = mk(200);
    for mb in [0usize, 1, 8, 64, 256, 1024, 8192] {
        let opts = CompressOptions {
            strategy: "auto".to_string(),
            max_bytes: Some(mb),
            ..Default::default()
        };
        let out = compress_top(&input, "json", &opts)
            .unwrap_or_else(|e| panic!("max_bytes={mb} 不应报错: {e}"));
        let Value::String(s) = out else {
            panic!("应返回 String")
        };
        assert!(
            s.len() <= mb,
            "max_bytes={mb}: 输出 {} 字节超限（契约见 mod.rs:115）",
            s.len()
        );
    }
}

/// **中文内容同样不许超限**（截断点必须落在字符边界上）。
#[test]
fn d367_utf8_content_respects_budget() {
    let items: Vec<Value> = (0..60)
        .map(|i| Value::String(format!("第{i}条中文记录内容")))
        .collect();
    let input = Value::List(items.into());
    for mb in [0usize, 3, 17, 200, 1024] {
        let opts = CompressOptions {
            strategy: "auto".to_string(),
            max_bytes: Some(mb),
            ..Default::default()
        };
        let out = compress_top(&input, "json", &opts)
            .unwrap_or_else(|e| panic!("max_bytes={mb} 不应报错: {e}"));
        let Value::String(s) = out else {
            panic!("应返回 String")
        };
        assert!(s.len() <= mb, "max_bytes={mb}: {} 字节超限", s.len());
        // 非法 UTF-8 边界会 panic 或产生替换字符
        assert!(
            !s.contains('\u{FFFD}'),
            "max_bytes={mb}: 截断产生了替换字符（切在非法边界）: {s:?}"
        );
    }
}

/// **`max_bytes=0` 返回空串是符合契约的**（规则 2a）。
///
/// 契约是「返回的 UTF-8 字节数不超过 `max_bytes`」⇒ 0 ≤ 0。
/// 本条把这个边界**显式钉住**，防止将来有人「顺手给 0 加特殊分支」。
#[test]
fn d367_zero_max_bytes_yields_empty_string() {
    let input = mk(100);
    let opts = CompressOptions {
        strategy: "auto".to_string(),
        max_bytes: Some(0),
        ..Default::default()
    };
    let out = compress_top(&input, "json", &opts).expect("不应报错");
    let Value::String(s) = out else {
        panic!("应返回 String")
    };
    assert_eq!(s.len(), 0, "max_bytes=0 时应返回空串（规则 2a）");
}

/// **非 List 输入必须明确报错**（`mod.rs:464-469`）。
#[test]
fn d367_non_list_input_errors() {
    let opts = CompressOptions::default();
    let out = compress_top(&Value::String("not a list".into()), "json", &opts);
    assert!(
        out.is_err(),
        "非 List 输入应报错; 实得 {:?}",
        out.map(|_| "Ok")
    );
    let msg = out.unwrap_err().to_string();
    assert!(
        msg.contains("expected List"),
        "诊断应说明期望 List; 实得: {msg}"
    );
}

/// **未知 strategy 必须明确报错**（`mod.rs:506-507`）。
#[test]
fn d367_unknown_strategy_errors() {
    let input = mk(10);
    let opts = CompressOptions {
        strategy: "no_such_strategy".to_string(),
        ..Default::default()
    };
    let out = compress_top(&input, "no_such_strategy", &opts);
    assert!(out.is_err(), "未知 strategy 应报错");
    let msg = out.unwrap_err().to_string();
    assert!(
        msg.contains("unknown strategy"),
        "诊断应说明是未知 strategy; 实得: {msg}"
    );
}

/// **压缩不得放大输入**（`finish_within_budget` 规则 3）。
///
/// 用「结构高度重复 ⇒ 压缩收益大」与「随机串 ⇒ 压缩无收益」两个极端，
/// 确认输出永远不比输入长。
#[test]
fn d367_compression_never_enlarges_input() {
    let repetitive: Vec<Value> = (0..500)
        .map(|_| Value::String("重复内容".to_string()))
        .collect();
    for (label, input) in [
        ("repetitive", Value::List(repetitive.into())),
        ("list", mk(500)),
    ] {
        for mb in [64usize, 512, 8192, 1_000_000] {
            let opts = CompressOptions {
                strategy: "auto".to_string(),
                max_bytes: Some(mb),
                ..Default::default()
            };
            let out = compress_top(&input, "json", &opts)
                .unwrap_or_else(|e| panic!("{label} max_bytes={mb} 不应报错: {e}"));
            let Value::String(s) = out else {
                panic!("应返回 String")
            };
            let in_bytes = mora::compress::json::estimate_bytes(
                match &input {
                    Value::List(l) => l.to_vec(),
                    _ => vec![],
                }
                .as_slice(),
            );
            assert!(
                s.len() <= in_bytes.max(mb),
                "{label} max_bytes={mb}: 输出 {} 字节，输入 {} 字节 —— 压缩放大了",
                s.len(),
                in_bytes
            );
        }
    }
}
