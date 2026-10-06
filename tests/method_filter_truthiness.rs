//! v0.104.6 D374 —— `method_dispatch` 的 **`filter` 真值路径**（D358 修复的
//! 端到端确认）+ `take` 的 count 守卫（否定轮，无产品变更）
//!
//! `src/interpreter/method_dispatch.rs`（1199 行）**零 panic 点**，
//! 19 个既有判据覆盖 list/dict/string 的方法面。
//! 此前未覆盖的是 **`filter` 接受非 bool 谓词返回值**这条路径 ——
//! 而它正是 D358 修的 `is_truthy`（`BigInt(0)` 误判为真）的**真实消费者**。
//!
//! ## 端到端确认：`[0n, 1n, 2n].filter(fn(x) x end)` ⇒ `[1n, 2n]`
//!
//! ```text
//! [0n, 1n, 2n].filter(fn(x) x end)   →  [1n, 2n]      ✅ 0 被剔除
//! [0, 1, 2].filter(fn(x) x end)     →  [1, 2]        ✅
//! [0.0, 1.0, 2.0].filter(fn(x) x end) → [1.0, 2.0]  ✅
//! ```
//!
//! **修前第一行是 `[0n, 1n, 2n]`（0 被保留）** —— 与 Int/Float 版本不一致。
//! 本文件把这三条**并排**钉住，任何一条退化都会红。
//!
//! ## `map` / `reduce` 不走真值路径
//!
//! `map` 直接 `push(mapped)`（L177），`reduce` 走累加器 —— 都不调
//! `is_truthy`。这解释了为什么 `xs.map(fn(x) x end)` 仍返回全列表
//! （map **不做**过滤，这是设计）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d374_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d374_{n}_{}", slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let path_str = p.to_string_lossy().into_owned();
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.contains("不兼容 v0.03")
                && !l.starts_with("[9layer]")
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// **主断言**：三种数值类型的 `0` 在 `filter` 里行为**完全一致**。
///
/// 这是 D358（`is_truthy` 漏 `BigInt` 分支）的**端到端回归钉**。
#[test]
fn d374_filter_predicate_treats_zero_falsy_for_all_numeric_types() {
    for (lit, expected) in [
        ("[0n, 1n, 2n]", "[1n, 2n]"),
        // ⚠ 裸数字字面量在 mora 里是 **Float**（D356 已钉），
        // 所以 `0` 打印成 `0.0`、`1` 打印成 `1.0`。
        // 首版这里写期望 `[1, 2]` ⇒ 假红。
        ("[0, 1, 2]", "[1.0, 2.0]"),
        ("[0.0, 1.0, 2.0]", "[1.0, 2.0]"),
    ] {
        let (code, got) = ev(&format!("print({lit}.filter(fn(x) x end))\n"));
        assert_eq!(code, 0, "`{lit}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(
            got.trim(),
            expected,
            "`{lit}.filter(fn(x) x end)`：0 必须被剔除（D358 修前 BigInt 的 0 被保留）"
        );
    }
}

/// **`filter` 对非数值类型同样按真值判定**。
#[test]
fn d374_filter_treats_empty_containers_and_false_as_falsy() {
    for (lit, expected) in [
        (r#"["", "a"]"#, "[a]"),
        ("[[], [1]]", "[[1.0]]"),
        ("[{}, {a: 1}]", "[{a: 1.0}]"),
        ("[false, true]", "[true]"),
    ] {
        let (code, got) = ev(&format!("print({lit}.filter(fn(x) x end))\n"));
        assert_eq!(code, 0, "`{lit}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{lit}` 的真值判定不对; 实得: {got}");
    }
}

/// **`map` 不过滤** —— 它把映射结果直接收集（`method_dispatch.rs:177`）。
///
/// 本条是为了**防止把 map 当 filter 用**而误判：map 的语义与
/// `is_truthy` **无关**，返回全列表是**正确**的。
#[test]
fn d374_map_does_not_filter() {
    let (code, got) = ev("let xs = [0n, 1n, 2n]\nprint(xs.map(fn(x) x end))\n");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "[0n, 1n, 2n]",
        "`map` 不做过滤（`push(mapped)`），零也保留; 实得: {got}"
    );
}

/// **`take` 的 count 边界**：Int 与 Float 都被接受，负数明确报错。
///
/// `method_dispatch.rs:225` 的注释记录了 D153 的修复
/// （此前只匹配 `Float`，`Int` 实参报 *requires a count*）。
/// L 里还有负数守卫。
#[test]
fn d374_take_count_boundaries() {
    for (arg, expected) in [
        ("2", "[1.0, 2.0]"),
        ("2.0", "[1.0, 2.0]"), // D153：Float 实参
        ("0", "[]"),
        ("99", "[1.0, 2.0, 3.0]"), // 超过长度 ⇒ 全取
    ] {
        let (code, got) = ev(&format!("print([1, 2, 3].take({arg}))\n"));
        assert_eq!(code, 0, "`take({arg})` 应正常; 实得 exit={code} out={got}");
        assert_eq!(
            got.trim(),
            expected,
            "`take({arg})` 的结果不对; 实得: {got}"
        );
    }
    // Int 实参也必须被接受（D153 的修复）
    let (code, got) = ev("print([1, 2, 3].take(1))\n");
    assert_eq!(
        code, 0,
        "`take(1)`（Int 实参）应被接受; 实得 exit={code} out={got}"
    );
    assert_eq!(got.trim(), "[1.0]", "实得: {got}");
}

/// **`take` 负数必须报错**，不是静默返回空列表。
#[test]
fn d374_take_rejects_negative_count() {
    let (code, got) = ev("print([1, 2, 3].take(-1))\n");
    assert_eq!(code, 1, "`take(-1)` 必须报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("count") && got.contains("负"),
        "诊断应说明 count 不能为负; 实得: {got}"
    );
}

/// **`reduce` 的签名是 `(reducer, initial)`** —— 不是 `(reducer)`，
/// 也不是 `(initial, reducer)`。本条把参数**顺序**钉住。
#[test]
fn d374_reduce_takes_reducer_then_initial() {
    let (code, got) = ev("print([1n, 2n, 3n].reduce(fn(a, b) a + b end, 0n))\n");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "6n", "reduce 应得 1+2+3+0=6; 实得: {got}");

    // **反向对照**：缺 initial 必须报错
    let (code, got) = ev("print([1, 2, 3].reduce(fn(a, b) a + b end))\n");
    assert_eq!(
        code, 2,
        "缺 initial 必须被 typeck 拒（min_arity=2）; 实得 exit={code} out={got}"
    );
}
