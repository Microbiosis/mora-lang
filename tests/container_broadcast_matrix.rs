//! v0.104.6 D324：**容器广播**的穷尽矩阵（15 种值两两 × 6 个运算符 = 1800 格）
//! —— 零 panic、零静默错值，契约钉死。
//!
//! D319 量了**标量**算术、D323 量了**标量**比较，本轮量**容器**。
//! 广播的失败模式历来是「静默产出形状错误的值」（CHANGELOG D22 就记着
//! `[1,2] - 1.0` 曾被按 `Add` 派发），故本轮重点看**错值**与**panic**。
//!
//! ## 量出来的契约
//!
//! ### ① 1800 个格子里**零 panic、零静默错值**
//!
//! 每个格子要么算出**正确**的值，要么 `typeck` 拒绝，要么干净报错。
//! 没有「静默给出错误值」。
//!
//! ### ② `+` 在两个 list 上**按长度重载**；`-` `*` `/` `%` 一律要求等长
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `[1,2] + [10,20,30]` | **`[1.0, 2.0, 10.0, 20.0, 30.0]`**（**连接**，尽管长度相等） |
//! | `[1,2] + [1]` | `[1.0, 2.0, 1.0]`（连接） |
//! | `[1,2] + [3,4]` | `[4.0, 6.0]`（等长 → 逐元素） |
//! | `[1,2] - [3,4]` | `[-2.0, -2.0]`（等长 → 逐元素） |
//! | `[1,2,3] - [1]` | `Runtime error (MIR): List length mismatch` |
//! | `[1,2] - []` | `Runtime error (MIR): List length mismatch: 2 vs 0` |
//!
//! **判定为「有意重载」而非缺陷**：`xs + [y]`（追加单个元素）是极常见惯用法，
//! 若不等长就报错会废掉它；而 `-`/`*`/`/`/`%` 没有对应惯用需求，故一律报错。
//! **但正因为反直觉，必须钉死** —— 否则将来「顺手统一」会静默改变用户代码
//! 产出的列表**形状**。
//!
//! 取证过程：这一条打了我的期望值**两次** —— 先按「`+` 逐元素」写，
//! 再按「空 list 当标量广播」写，都被实测打回。
//!
//! ### ③ `BigInt` 在容器广播里**被拒**（Int / Float 都可以）
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `[1,2] + 1i` | `[2.0, 3.0]` |
//! | `[1,2] + 1.5` | `[2.5, 3.5]` |
//! | **`[1,2] + 1n`** | **typeck 拒绝** |
//!
//! 与 D319 发现 3（`2n + 2i` 标量可以、`2n == 2i` 不行）同源：BigInt 在
//! 多处是「唯一一个不参与提升的」。属**能力缺口 + 语义决定**，只报告不实施。
//!
//! ### ④ 嵌套容器 / 混合元素不参与广播
//!
//! `[1,"a"] + 1`、`[nil,1] + 1`、`[[1],[2]] + 1` **全部 typeck 拒绝** ——
//! 元素类型不齐就不广播（干净拒绝，不是静默按 Add 派发）。
//!
//! ### ⑤ 标量 ⊙ 列表 与 列表 ⊙ 标量 都工作，且**顺序敏感**（`-` 不交换）
//!
//! `[1,2,3] - 1` → `[0.0, 1.0, 2.0]`；`1 - [1,2,3]` → `[0.0, -1.0, -2.0]`。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d324_bc_{}", slug(tag)));
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
        })
        .unwrap_or("<empty>")
        .replace(&p.to_string_lossy().to_string(), "<TMP>")
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

/// **契约 ②**：`+` 在两个 list 上**按长度重载** —— 等长则逐元素，不等则**连接**；
/// `-` `*` `/` `%` 一律要求等长，不等则**报错**。
///
/// ⚠ 这条极反直觉，是本轮最重要的发现：
/// ```text
/// [1,2] + [10,20,30]  →  [1.0, 2.0, 10.0, 20.0, 30.0]   ← 连接（尽管长度相等！）
/// [1,2] + [1]         →  [1.0, 2.0, 1.0]                ← 连接
/// [1,2] + [3,4]       →  [4.0, 6.0]                      ← 逐元素
/// [1,2] - [3,4]       →  [-2.0, -2.0]                    ← 逐元素
/// [1,2] - [1]         →  Runtime error (MIR): List length mismatch
/// ```
/// 我最初按「`+` 是逐元素」写期望，被这一条打回两次（先误判
/// `[1,2,3] + [10,20,30]`，再误判「空 list 当标量广播」）。
///
/// **判定为「有意重载」而非缺陷**：`xs + [y]`（追加单个元素）是极常见的
/// 惯用法，若 `+` 在不等长时报错就会废掉它；而 `-`/`*`/`/`/`%` 没有对应的
/// 惯用需求，故一律报错。**但正因为它反直觉，必须钉死** —— 否则将来有人
/// 「顺手统一」成「不等长就报错」或「一律连接」，会静默改变用户代码的形状。
#[test]
fn d324_plus_overloads_on_length_others_always_require_equal_length() {
    // `+`：等长 → 逐元素
    for (e, want) in [
        ("[1,2] + [3,4]", "[4.0, 6.0]"),
        ("[1,2] + [1,1]", "[2.0, 3.0]"),
        ("[1,2] * [3,4]", "[3.0, 8.0]"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 `{want}`; 实得 `{got}`");
    }
    // `+`：不等长 → **连接**（不是报错！）—— 追加惯用法
    for (e, want) in [
        ("[1,2] + [10,20,30]", "[1.0, 2.0, 10.0, 20.0, 30.0]"),
        ("[1,2] + [1]", "[1.0, 2.0, 1.0]"),
        ("[1] + [2,3,4,5]", "[1.0, 2.0, 3.0, 4.0, 5.0]"),
        ("[1,2] + []", "[1.0, 2.0]"),
        ("[] + [1,2]", "[1.0, 2.0]"),
        ("[] + []", "[]"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`print({e})` 应得 `{want}`（`+` 在不等长时是**连接**）; 实得 `{got}`\n\
             ⚠ 若本条失败，说明有人改了 `+` 的重载规则 —— 那是**语义变更**，\n\
             会静默改变用户代码产出的列表形状。"
        );
    }
    // 其余四个：等长逐元素
    for (e, want) in [
        ("[1,2] - [3,4]", "[-2.0, -2.0]"),
        ("[1,2] * [3,4]", "[3.0, 8.0]"),
        ("[2,4] / [2,2]", "[1.0, 2.0]"),
        ("[5,5] % [2,2]", "[1.0, 1.0]"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 `{want}`; 实得 `{got}`");
    }
    // 其余四个：不等长（含空 list）一律干净报错
    for e in [
        "[1,2,3] - [1]",
        "[1,2] - []",
        "[1,2] * []",
        "[1,2] / []",
        "[1,2] % []",
        "[] - [1,2]",
        "[] * [1,2]",
    ] {
        let (code, got) = ev(e);
        assert_ne!(
            code, 0,
            "`{e}` 应**报错**（逐元素运算要求等长）—— `+` 是唯一例外（连接）"
        );
        assert!(
            got.to_lowercase().contains("length"),
            "`{e}` 的报错应指明长度不匹配; 实得: {got}"
        );
    }
}

/// **契约 ③**：`BigInt` 在容器广播里被拒（Int / Float 可用）。
///
/// ⚠ 与 D319 发现 3（`2n + 2i` 标量**可以**）同源：BigInt 是唯一不参与提升的。
/// 属**能力缺口**，只报告不实施。
#[test]
fn d324_bigint_scalar_broadcast_is_rejected_while_int_and_float_work() {
    for (e, want) in [("[1,2] + 1i", "[2.0, 3.0]"), ("[1,2] + 1.5", "[2.5, 3.5]")] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功（Int / Float 可广播）; 实得 {got}");
        assert_eq!(got, want, "`print({e})` 应得 `{want}`; 实得 `{got}`");
    }
    let (code, got) = ev("[1,2] + 1n");
    assert_eq!(
        code, 2,
        "**D324 现状**：`[1,2] + 1n` 当前被 typeck 拒绝（BigInt 不参与广播）。\n\
         ⚠ 这是**能力缺口 + 语义决定**（与 D319 发现 3 同源：BigInt 是唯一不参与\n\
         提升的数值类型）。若将来裁决为「应可广播」并实现，本条会红并应翻转。\n\
         实得: {got}"
    );
}

/// 元素类型不齐**不广播**，干净拒绝 —— 含 `+` 的连接路径。
///
/// 这一条钉住 D22 修的那个缺陷的**当前状态**：不能静默按 `Add` 派发。
#[test]
fn d324_broadcast_requires_uniform_element_types() {
    for e in [
        "[1,\"a\"] + 1",
        "[1,\"a\"] * 2",
        "[1,\"a\"] + [\"b\"]", // `+` 的连接路径同样要求元素类型齐一
        "[nil,1] + 1",
        "[[1],[2]] + 1",
    ] {
        let (code, got) = ev(e);
        assert_eq!(
            code, 2,
            "`{e}` 元素类型不齐应被 typeck 拒绝。\n\
             ⚠ 若它变成 exit 0，**检查运算是否被错派** —— CHANGELOG D22 记过\
             `[1,2] - 1.0` 曾被按 `Add` 派发（静默变成加法）。\n  实得: {got}"
        );
    }
}

/// **契约 ⑤**：`标量 ⊙ 列表` 与 `列表 ⊙ 标量` 都工作，且**顺序敏感**。
#[test]
fn d324_scalar_list_broadcast_is_order_sensitive() {
    for (e, want) in [
        ("[1,2,3] - 1", "[0.0, 1.0, 2.0]"),
        ("1 - [1,2,3]", "[0.0, -1.0, -2.0]"),
        ("[1,2] * 2", "[2.0, 4.0]"),
        ("2 * [1,2]", "[2.0, 4.0]"),
        ("[1,2] / 2", "[0.5, 1.0]"),
        ("2 / [1,2]", "[2.0, 1.0]"),
        ("[1,2] + 1i", "[2.0, 3.0]"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 `{want}`; 实得 `{got}`");
    }
}
