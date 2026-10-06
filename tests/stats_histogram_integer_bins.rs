//! v0.104.6 D329 —— `stats.histogram` 的 `bins` **必须是整数**（小数静默截断，已修）
//!
//! ## 实测（修前）
//!
//! ```text
//! stats.histogram([1,2,3,4], 2.5) → exit 0   [{count:2.0,lo:1.0,hi:2.5}, {count:2.0,lo:2.5,hi:4.0}]
//! stats.histogram([1,2,3,4], 0.5) → exit 0   []
//! stats.histogram([1,2,3,4],-0.5) → exit 1   （D284 的负数守卫挡住了）
//! ```
//!
//! **两条都静默**。`0.5` 那条静默得最彻底：截断得 `bins = 0`，而
//! `histogram()` 开头就有 `if bins == 0 { return Value::List(Vec::new()) }`
//! —— 用户要「0.5 个分箱」拿到「0 个分箱」，与**显式**传 `0` **完全同形**。
//!
//! ## 因果链
//!
//! ```text
//! Value::Float(0.5)
//!   → value_as_usize（f64 → usize，Rust `as` 是**向零截断**）→ Some(0)
//!   → `if bins == 0` 早返回 → **空列表**、exit 0、零诊断
//! ```
//!
//! `0.5` 还顺手**继承**了 D284 判据
//! `d284_explicit_zero_bins_still_returns_empty_list` 守住的「显式 0 返回空列表」
//! 这条**合法**语义 —— 那条判据是对的（显式 0 确实是 0 个分箱），
//! 但它拦不住小数**绕道**走到同一个分支。
//!
//! ## 判定为缺陷（而非「截断就是设计」）的两条依据
//!
//! ① **`docs/mora-spec.md:981`** 的签名写的是 **`list, int`**：
//! ```text
//! | `stats.histogram(list, bins)` | `list, int -> list<dict>` | 直方图 |
//! ```
//! 小数本就不该进。而 `src/typeck/dispatch.rs:680` 的 `MethodGroup`
//! **只登记元数不校验参数类型** ⇒ 运行时收口是**唯一**能拦的地方。
//!
//! ② **本函数自己的错误消息**写着 `bins must be an integer` ——
//! 消息承诺 integer、行为接受 `2.5`，**自相矛盾**。这条比 ① 更硬：
//! 它不依赖外部文档，**代码自己就说了「要整数」**。
//!
//! ## 为什么只改 `histogram`，不动 `value_as_usize` 收口
//!
//! D246 立的那个收口**已经**用判据把「向零截断」钉成设计：
//!
//! ```rust
//! // tests/value_extraction_saturation.rs:54
//! assert_eq!(value_as_usize(&Value::Float(2.9)), Some(2), "应向零取整");
//! ```
//!
//! 且该收口有 **6 个**调用方，全是「时长 / 步数 / 个数」类**计数**参数：
//! `crush_json` / `exec.parallel(max_concurrent, timeout_ms)` /
//! `max_steps` / `backoff_ms` / `sandbox(cpu_cores, memory_mb)`。
//! 对它们，向零截断是合理的**通用**约定 —— `1.5 ms` 超时没有物理意义，
//! 取 1 ms 比报错更贴近「用户算错了但意思明确」。
//!
//! ⇒ 「bins 必须是整数」是 **`histogram` 自己的**契约（spec 签名 + 自身消息
//! 两处都这么说），**不是收口的**。改收口会连带改掉另外 5 个点的语义，
//! 那是**产品契约决定**，不属本条。已在 `value_as_usize` 的文档注释里
//! 写明这个边界，并点名哪些调用方需要自己补 `fract() == 0.0`。
//!
//! ## 不回归的三条既有语义
//!
//! - **显式整数 `0`** 仍返回空列表（D284 判据）——小数不再被换算成 0，
//!   但用户**显式**传 0 仍是原来的语义；
//! - **浮点写的整数**（`2.0`）仍放行 —— dict 字面量给 `Float`（D98），
//!   拦掉会让 `histogram(xs, 2.0)` 崩；
//! - **其它 5 个调用点**仍按收口截断（`crush_json(xs, 2.5)` /
//!   `exec.parallel(cmds, 2.5)` 均 exit 0），见下方 A/B 对照判据。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d329_hist_{}", slug(tag)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .unwrap_or("<empty>")
        .to_string();
    (out.status.code().unwrap_or(-1), first)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn ev(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **主断言**：非整数 `bins` 必须报错，不得静默截断。
///
/// 覆盖三种非整数形态，缺一不可：
/// - `2.5`（>1，向零截断成 2）—— 静默给出**与 `bins=2` 逐字相同**的直方图；
/// - `0.5`（<1，截断成 0）—— 静默走 `bins == 0` 早返回，给出**空列表**；
/// - `-0.5`（负）—— D284 已挡，但**必须仍被挡**（不回归）。
#[test]
fn d329_fractional_bins_is_rejected_not_silently_truncated() {
    for e in [
        "stats.histogram([1,2,3,4],2.5)",
        "stats.histogram([1,2,3,4],0.5)",
        "stats.histogram([1,2,3,4],-0.5)",
    ] {
        let (code, got) = ev(e);
        assert_eq!(
            code, 1,
            "`{e}` 应报错（exit 1）; 实得 exit={code} out={got}\n\
             修前：小数 bins 被 `value_as_usize` 向零截断，exit 0、零诊断"
        );
        assert!(
            got.contains("integer") && got.contains("bins"),
            "`{e}` 的错误应同时点名 bins 与 integer（spec 签名是 `list, int`）; 实得: {got}"
        );
    }
}

/// **对照组 1**：整数 `bins` 必须照常工作 —— 本条不能把 histogram 弄坏。
#[test]
fn d329_integer_bins_still_works() {
    for (e, want) in [
        ("stats.histogram([1,2,3,4],2)", 2usize),
        ("stats.histogram([1,2,3,4],1)", 1),
        ("stats.histogram([1,2,3,4],4)", 4),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert!(got.contains("count:"), "`{e}` 应产出 bin 字典; 实得: {got}");
        let n = got.matches("count:").count();
        assert_eq!(n, want, "`{e}` 应得 {want} 个 bin; 实得 {n} 个: {got}");
    }
}

/// **对照组 2**：**浮点写的整数**仍放行。
///
/// dict 字面量给 `Float`（D98），`json.parse("2")` 走 `Int`（D129）——
/// `bins` 的来源天然可能是 `Float`。若用「必须是 `Int`」来拦，
/// `histogram(xs, 2.0)` 会崩，且与 D246「两侧数字都必须接受」的收口相悖。
#[test]
fn d329_float_spelled_integer_is_still_accepted() {
    for e in [
        "stats.histogram([1,2,3,4],2.0)",
        "stats.histogram([1,2,3,4],1.0)",
    ] {
        let (code, got) = ev(e);
        assert_eq!(
            code, 0,
            "`{e}` 应成功（2.0 是整数值的浮点写法）; 实得 exit={code} out={got}"
        );
    }
}

/// **对照组 3**：**不回归 D284** —— 显式整数 `0` 仍返回空列表。
///
/// 负数不再被换算成 0，但用户**显式**传 0 仍是「0 个分箱」的原有语义。
/// 这条守住「只修换算、不动语义」的边界，与
/// `stats_histogram_bins_guard.rs::d284_explicit_zero_bins_still_returns_empty_list`
/// 互为呼应（那条钉 `-1` / `-1.0` / 1e11 / 正常值，本条钉 `0` 的正向语义）。
#[test]
fn d329_explicit_integer_zero_bins_still_returns_empty_list() {
    let (code, got) = ev("stats.histogram([1,2,3,4],0)");
    assert_eq!(
        code, 0,
        "显式 bins=0 仍应成功（既有行为）; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got, "[]",
        "bins=0 的既有行为是空列表，不该被本条改掉; 实得: {got}"
    );
}

/// **对照组 4**：**其它 5 个调用点不受影响** —— 收窄的证据。
///
/// 本条改的是 `stats.histogram` 一处。若有人「顺手」把
/// `flow::value_as_usize` 改成拒绝小数，下面两条会立刻变红：
/// `crush_json` / `exec.parallel` 的 `max` 与 `max_concurrent`
/// 仍按 D246 收口**向零截断**（见 `value_extraction_saturation.rs:54`
/// 「应向零取整」），因为它们是**时长 / 条数**类参数，小数取整合理。
///
/// 这条判据存在的意义：把「哪些调用点该收整数」钉成**显式清单**，
/// 而不是留给下一个人凭直觉决定。
#[test]
fn d329_other_value_as_usize_callers_still_truncate() {
    for (e, what) in [
        ("crush_json([1,2,3,4,5], 2.5)", "crush_json 的 max"),
        (
            "exec.parallel([\"echo a\"], 2.5)",
            "exec.parallel 的 max_concurrent",
        ),
    ] {
        let (code, got) = ev(e);
        assert_eq!(
            code, 0,
            "{what} 不属本条范围：仍应按 D246 收口向零截断（exit 0）; \
             实得 exit={code} out={got}\n\
             若此处变红，说明有人把 `value_as_usize` 收口整体改成拒绝小数了 —— \
             那是 6 个调用点的**产品契约决定**，不该由某一条缺陷顺带做掉"
        );
    }
}
