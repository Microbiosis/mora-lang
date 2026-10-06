//! v0.104.6 D244 —— `Checkpoint::from_json` 的**负数守卫**必须覆盖**全部五个**数字字段。
//!
//! ## 缺陷
//!
//! D148 给 `v` / `step` 加了负数守卫，但**同一个函数**里的
//! `channel_versions` / `versions_seen` / `timestamp_ms` 同样是从 `Value`
//! （外部 JSON，或 `SqliteSaver::load` 读出的 `data_json`）转换，却**没有**守卫。
//! 守卫覆盖了 5 处中的 2 处。
//!
//! 两种失败模式方向相反，且都**零诊断**：
//! 整数 `as` 是**回绕**，浮点 `as` 是**饱和**。
//!
//! ## 实测后果（`from_json` 喂 -1，修前）
//!
//! | 字段 | 修前 | 修后 |
//! |---|---|---|
//! | `v` | 报错 | 报错（D148 已有守卫） |
//! | `step` | 报错 | 报错（D148 已有守卫） |
//! | `channel_versions` | `18446744073709551615` | 报错，点名 `channel_versions[messages]` |
//! | `versions_seen` | `18446744073709551615` | 报错，点名 `versions_seen[node_a][messages]` |
//! | `timestamp_ms` | `340282366920938463463374607431768211455` | 报错 |
//!
//! 危害不止「值荒谬」：
//! - `channel_versions` / `versions_seen` 的语义是「已观测到的最大版本」，
//!   `u64::MAX` 等于宣告「这个 channel 的一切都已见过」⇒ **增量计算永久停滞**。
//! - `timestamp_ms` 是 D234 三级排序键 `(step, timestamp_ms, id)` 的第二项，
//!   `u128::MAX` 会被 `load` 永远排到**第一位**、`list` 永远排到最后一位。
//!
//! 两条 SQL 后端的可达性：`SqliteSaver::load` 直接
//! `Checkpoint::from_json(&data_json)`（`sqlite.rs:117`），`from_json` 本身是
//! `pub` 库 API。

use mora::checkpoint::Checkpoint;

/// 构造一个字段齐全的 checkpoint JSON，替换其中指定的片段。
fn json_with(overrides: &[(&str, &str)]) -> String {
    let mut fields: Vec<(&str, &str)> = vec![
        ("id", r#""cp1""#),
        ("v", "1"),
        ("thread_id", r#""t1""#),
        ("step", "3"),
        ("channel_values", "{}"),
        ("channel_versions", r#"{"messages":2}"#),
        ("versions_seen", r#"{"node_a":{"messages":1}}"#),
        ("pending_sends", "[]"),
        ("timestamp_ms", r#""1700000000000""#),
    ];
    for (key, val) in overrides {
        if let Some(slot) = fields.iter_mut().find(|(k, _)| k == key) {
            slot.1 = val;
        }
    }
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("{k:?}:{v}")).collect();
    format!("{{{}}}", body.join(","))
}

/// 五个数字字段 × 五种负数表示 —— 穷举，而不是抽一个 `-1`。
#[test]
fn d244_every_numeric_field_rejects_negative() {
    // 整数回绕最毒（`-1i64 as u64 == u64::MAX`），浮点饱和同样静默（`-1.0 → 0`），
    // 小数负值能同时打中两者，巨大负值能打中 `as` 的边界路径。
    const NEGATIVES: &[(&str, &str)] = &[
        ("Int(-1)", "-1"),
        ("Int(-2)", "-2"),
        ("Float(-1.0)", "-1.0"),
        ("Float(-0.5)", "-0.5"),
        ("Float(-1e300)", "-1e300"),
    ];
    const FIELDS: &[(&str, &str, &str)] = &[
        // (字段名, 该字段的 JSON key, 该字段的**纯 value**模板：{v} = 负数字面量)
        ("v", "v", "{v}"),
        ("step", "step", "{v}"),
        (
            "channel_versions",
            "channel_versions",
            r#"{"messages":{v}}"#,
        ),
        (
            "versions_seen",
            "versions_seen",
            r#"{"node_a":{"messages":{v}}}"#,
        ),
        ("timestamp_ms", "timestamp_ms", "{v}"),
    ];

    for (fname, key, template) in FIELDS {
        for (label, neg) in NEGATIVES {
            let lit = template.replace("{v}", neg);
            let json = json_with(&[(key, &lit)]);
            let res = Checkpoint::from_json(&json);
            assert!(
                res.is_err(),
                "{fname} = {label} 竟然被接受了（守卫缺失）\n  json = {json}"
            );
            let msg = res.unwrap_err();
            // 错误信息必须**点名具体字段**，否则用户无从定位是哪一项坏了。
            assert!(
                msg.contains(if *fname == "channel_versions" {
                    "channel_versions"
                } else if *fname == "versions_seen" {
                    "versions_seen"
                } else {
                    fname
                }),
                "{fname} = {label} 的错误信息没有点名字段：{msg}"
            );
        }
    }
}

/// 对照组：把成因钉在**语言事实**上，不钉 `nonneg_num`（D235 教训）。
#[test]
fn d244_control_group_integer_as_wraps_float_as_saturates() {
    // 整数 as = 回绕。若哪天这条不成立，本判据的前提需要重新审视。
    assert_eq!((-1i64) as u64, u64::MAX, "负 i64 as u64 应回绕成 u64::MAX");
    assert_eq!(
        (-1i64) as u128,
        u128::MAX,
        "负 i64 as u128 应回绕成 u128::MAX"
    );
    // 浮点 as = 饱和。
    assert_eq!((-1.0f64) as u64, 0, "负 f64 as u64 应饱和成 0");
    assert_eq!((-0.5f64) as u64, 0, "负小数 f64 as u64 应饱和成 0");
}

/// 大值**回绕**同样必须报错（`2^32+1 as u32 == 1` 是同一类静默错值）。
///
/// 修前 `v = 4294967297` 会被截成 `1` —— 一个「看似合法」但完全错误的版本号。
#[test]
fn d244_oversized_value_wrapping_is_also_rejected() {
    // v 是 u32：2^32+1 修前回绕成 1。
    let json = json_with(&[("v", "4294967297")]);
    let res = Checkpoint::from_json(&json);
    assert!(
        res.is_err(),
        "v = 2^32+1 竟被接受（回绕成 1）\n  json = {json}"
    );

    // 正数 u32 上界仍必须可用（防过度收紧）。
    let json = json_with(&[("v", "4294967295")]);
    assert_eq!(
        Checkpoint::from_json(&json).expect("u32 上界应可用").v,
        u32::MAX
    );
}

/// 正例不回归：所有合法形式仍能正常解析。
#[test]
fn d244_valid_checkpoints_still_parse() {
    // 整数版本号 + 字符串时间戳（`to_json` 的正常产出形态）。
    let cp = Checkpoint::from_json(&json_with(&[])).expect("合法 checkpoint 应可解析");
    assert_eq!(cp.v, 1);
    assert_eq!(cp.step, 3);
    assert_eq!(cp.timestamp_ms, 1_700_000_000_000);
    assert_eq!(cp.channel_versions.get("messages"), Some(&2));
    assert_eq!(
        cp.versions_seen
            .get("node_a")
            .and_then(|m| m.get("messages")),
        Some(&1)
    );

    // 浮点版本号仍被接受（D148 未收紧这一侧）。
    let cp = Checkpoint::from_json(&json_with(&[("v", "1.0"), ("step", "3.0")]))
        .expect("浮点版本号应被接受");
    assert_eq!(cp.v, 1);
    assert_eq!(cp.step, 3);

    // 数字形式（非字符串）的 timestamp_ms 仍被接受。
    let cp = Checkpoint::from_json(&json_with(&[("timestamp_ms", "1700000000000")]))
        .expect("数字形式 timestamp_ms 应被接受");
    assert_eq!(cp.timestamp_ms, 1_700_000_000_000);

    // 零是合法值，不能被守卫误伤。
    let cp = Checkpoint::from_json(&json_with(&[
        ("v", "0"),
        ("step", "0"),
        ("channel_versions", r#"{"messages":0}"#),
        ("timestamp_ms", "0"),
    ]))
    .expect("0 应被接受");
    assert_eq!(cp.step, 0);
    assert_eq!(cp.timestamp_ms, 0);
    assert_eq!(cp.channel_versions.get("messages"), Some(&0));
}
