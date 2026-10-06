//! v0.104.6 D404 —— `ccr.marker` 的 **size 负数饱和成 0**：**有意决策，只补齐覆盖**（否定轮）
//!
//! ## 结论：这是 **D339 明确记录的「不修、只钉现状 + 报告」**的产品契约决定
//!
//! 实测（真实 CLI）：
//!
//! ```text
//! ccr.marker(h, 8)      → <<ccr:0000000000000001,8>>
//! ccr.marker(h, -1)     → <<ccr:0000000000000001,0>>
//! ccr.marker(h, -99999) → <<ccr:0000000000000001,0>>
//! exit 0，零诊断
//! ```
//!
//! `tests/tea_max_steps_guard.rs::d339_…_still_becomes_zero_for_both_types`
//! 已经**专门钉住**这个行为，其 doc 写明：
//! 「改它属**产品契约决定**（负尺寸该报错还是当 0），只报告」。
//!
//! ## ⚠ 本轮的一次**自我否决**（与 D397 同款，但更隐蔽）
//!
//! 我**真的**改过一次：用 D283（`agent.create` 的 `max_steps` 同样是
//! `as usize` 饱和、已改为报错）与 D246（`value_as_usize` 是唯一提取点）
//! 判定此处是「遗漏」，改成负数报错。
//!
//! **随后发现 D339 的决定记录，回退。** 教训比 D397 更具体：
//!
//! > **「兄弟调用点选了报错」不足以判定本调用点是遗漏** ——
//! > 必须先查**本调用点自己**是否已被决定过。
//! > 而**一条专门钉住某行为的判据，本身就是「该行为是有意的」的证据** ——
//! > 找到它，就说明有人想过并留了记录。
//!
//! D397 的教训是「文档声明优先于一致性启发式」；
//! 本轮补上的是「**判据的断言内容也是声明**」—— 只查代码注释不够，
//! 要查**判据**里有没有把它钉住。
//!
//! ## 本文件的实际贡献：把覆盖从「1 条」补到「完整矩阵」
//!
//! D339 只钉了「`Float` / `Int` 两种类型都变 0」；
//! 本文件补齐：**非有限值**、**缺省**、**`nil`**、**小数截断**、
//! **可达性分析**、以及「若将来有人改语义，钉它的判据会红」这条元约束。

use std::path::Path;
use std::process::Command;

use mora::ccr::{CcrStore, InMemoryCcrStore, extract_hash, make_marker};
use mora::value::Value;

fn run_fixture(name: &str) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/e2e")
        .join(name);
    let out = Command::new(exe)
        .args(["run", script.to_str().expect("路径转字符串")])
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

// ── ① 现状钉：负数**饱和成 0**，不报错（D339 决定） ──

/// **负数 size 静默变 0，两种数值类型一致**（D339 的钉法，本文件补齐 `nil` 等）。
#[test]
fn d404_e2e_negative_size_saturates_to_zero() {
    let (code, out) = run_fixture("ccr_marker_negative_size.mora");
    assert_eq!(code, 0, "负数当前**不报错**（D339 决定）; out={out}");
    for needle in [
        "pos=<<ccr:0000000000000001,8>>",
        "zero=<<ccr:0000000000000001,0>>",
        "absent=<<ccr:0000000000000001,0>>",
        "frac=<<ccr:0000000000000001,2>>",
        "neg=<<ccr:0000000000000001,0>>",
        "nil=<<ccr:0000000000000001,0>>",
    ] {
        assert!(out.contains(needle), "缺 `{needle}`; out={out}");
    }
    assert!(
        out.contains("unreachable"),
        "脚本应跑到最后一行（当前不终止）"
    );
}

/// **元约束：钉住该决定的判据必须存在。**
///
/// 有人若要改这个语义（改成报错），`tea_max_steps_guard.rs` 里那条会红 ——
/// 本条确保**那条判据没被顺手删掉**。
#[test]
fn d404_the_deliberate_decision_is_still_pinned() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tea_max_steps_guard.rs"),
    )
    .expect("读 tests/tea_max_steps_guard.rs");
    assert!(
        src.contains("d339_ccr_marker_negative_size_still_becomes_zero_for_both_types"),
        "D339 钉住「负尺寸变 0」的判据不见了 —— 该决定会失去护栏"
    );
    assert!(
        src.contains("改它属产品契约决定"),
        "D339 判据应保留「属产品契约决定」的说明"
    );
    // `ccr.marker` 的实现注释也应记录该决定
    let impl_src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interpreter/builtins/ccr.rs"),
    )
    .expect("读 builtins/ccr.rs");
    let code: String = impl_src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains(".map(|n| n as usize)"),
        "`ccr.marker` 的 size 换成了别的算法 —— 有意决策被改动，需重新评估"
    );
}

// ── ② 若将来要改，会用到的收口（记录，不启用） ──

/// **库级：`Int` 负数也饱和成 0**（D339 钉的第二种类型）。
///
/// ⚠ 脚本里**拿不到 `Value::Int(-1)`** —— 字面量是 `Float`，
/// 而 `len()` 只会给非负 `Int` ⇒ 这一路只有库级可达。
#[test]
fn d404_negative_int_saturates_at_library_level() {
    let interp = mora::interpreter::Interpreter::new();
    for (label, v) in [
        ("Float -1.0", Value::Float(-1.0)),
        ("Int -1", Value::Int(-1)),
    ] {
        let got = interp
            .call_ccr_method("marker", &[Value::String("h".into()), v])
            .unwrap_or_else(|e| panic!("ccr.marker({label}) 现状**不报错**; 实得 Err({e})"));
        assert_eq!(
            got.to_string(),
            "<<ccr:h,0>>",
            "[{label}] 现状是负尺寸变 0（两种类型一致 ⇒ 无 D339 那种类型分歧）; 实得 {got}"
        );
    }
}

/// **`value_as_usize` 对负数 / NaN / inf 返回 `None`**。
///
/// 这是「负数该报错时该怎么改」的**现成收口**（D246），
/// 本条把它的契约钉住，以便将来真要改语义时不必重新发明。
#[test]
fn d404_value_as_usize_rejects_non_negatives() {
    for v in [
        Value::Float(-1.0),
        Value::Int(-1),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
    ] {
        assert!(
            mora::flow::value_as_usize(&v).is_none(),
            "`value_as_usize` 竟接受了 {v:?} —— 将来改守卫时会静默失效"
        );
    }
    // 反向对照：非负照常；小数向零截断是文档化约定
    assert_eq!(mora::flow::value_as_usize(&Value::Int(8)), Some(8));
    assert_eq!(mora::flow::value_as_usize(&Value::Float(2.9)), Some(2));
}

// ── ③ CCR 存储层首次外部覆盖（否定轮，零缺陷） ──

/// **`put` / `get` / `len` 往返**，hash 为 16 位 hex（P0-A4 加宽）。
#[test]
fn d404_store_put_get_len() {
    let s = InMemoryCcrStore::new();
    assert!(s.is_empty());
    let h = s.put("hello world");
    assert_eq!(h.len(), 16, "hash 应是 16 位 hex（u64 全宽）");
    let e = s.get(&h).expect("应能取回");
    assert_eq!(e.data, "hello world");
    assert_eq!(e.size, 11, "size 是**字节**数");
    assert_eq!(e.hash, h);
    assert_eq!(s.len(), 1);
    assert!(s.get("deadbeef").is_none());
}

/// **hash 唯一性**，且 **Clone 共享 counter** ⇒ 克隆体接着发号不撞号。
#[test]
fn d404_hashes_are_unique() {
    let s = InMemoryCcrStore::new();
    let hs: Vec<String> = (0..50).map(|i| s.put(&format!("d{i}"))).collect();
    let mut sorted = hs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 50, "50 次 put 应得 50 个不同 hash");
    assert_eq!(s.len(), 50);

    let c = s.clone();
    let h_from_clone = c.put("from-clone");
    assert!(!hs.contains(&h_from_clone), "克隆体的 hash 不得与本体重复");
    assert_eq!(c.len(), s.len(), "Clone 共享 entries");
}

/// **marker 格式与 `extract_hash` 往返**。
#[test]
fn d404_marker_roundtrip() {
    let m = make_marker("abcd1234", 42);
    assert_eq!(m, "<<ccr:abcd1234,42>>");
    assert_eq!(extract_hash(&m), Some("abcd1234"));
}

/// **`extract_hash` 边界**（含一处退化行为）。
///
/// `<<ccr:>>` 被当成合法 marker 并返回**空 hash**（`Some("")`）而非 `None`；
/// 当前**无可观察危害**（`get("")` 返 `None`），只钉现状。
#[test]
fn d404_extract_hash_edges() {
    assert_eq!(extract_hash("not a marker"), None);
    assert_eq!(extract_hash("<<other:hash>>"), None);
    assert_eq!(extract_hash("<<ccr:nocomma>>"), Some("nocomma"));
    assert_eq!(extract_hash("<<ccr:>>"), Some(""));
    assert!(
        InMemoryCcrStore::new().get("").is_none(),
        "空 hash 取不到 entry"
    );
    // 少于 6 字节不得 panic（`starts_with` 先拦下）
    assert_eq!(extract_hash(""), None);
    assert_eq!(extract_hash("<<cc"), None);
    // 非 ASCII：切片落在 ASCII 分隔符上，不 panic
    assert_eq!(extract_hash("<<ccr:中文,42>>"), Some("中文"));
}

/// **模块 doc 与实现一致**：marker 格式是 `<<ccr:HASH,SIZE>>`（Mora 简化去掉 KIND）。
#[test]
fn d404_module_doc_matches_format() {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ccr/mod.rs"))
        .expect("读 src/ccr/mod.rs");
    assert!(
        doc.contains("`<<ccr:HASH,SIZE>>`"),
        "doc 应写明 marker 格式为 `<<ccr:HASH,SIZE>>`（Mora 简化去掉 KIND）"
    );
}
