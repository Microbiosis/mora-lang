//! v0.104.6 D323：比较运算符的**穷尽矩阵** —— 契约钉死，本轮**未发现新缺陷**。
//!
//! D319 做了数值塔的**算术**矩阵（挖出 `BigInt ⊗ Int` 除零 panic）。
//! 本轮做它的搭档：**比较**矩阵 —— 10 种值两两 × 6 个比较运算符。
//!
//! ## 量出来的契约
//!
//! ### 一、顺序比较（`<` `>` `<=` `>=`）**只对数字有定义**
//!
//! `String` / `Char` / `Bool` / `Nil` / `List` / `Dict` 的顺序比较**全部**
//! 报 `Runtime error (MIR): Operands must be numbers`（**干净报错**，
//! 不是静默错值）。数值塔内（`Float` / `Int`）两两可比且满足三角关系。
//!
//! ⚠ 「字符串不能比大小」是**能力缺口**（多数语言可以），不是缺陷 ——
//! 报错而非静默给错答案，符合本项目一贯立场。
//!
//! ### 二、跨类型相等**一律被 typeck 拒绝**，绝不静默给 `true`
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `nil == false` / `nil == 0` / `nil == 0.0` | typeck 拒绝 |
//! | `false == 0` / `true == 1` / `true == 1.0` | typeck 拒绝 |
//! | `'a' == "a"` / `"1" == 1` | typeck 拒绝 |
//! | `1n == 1i` / `1n == 1` | typeck 拒绝（D319 发现 3 的现状） |
//! | `1 == 1.0` / `1i == 1.0` | **true**（运算符有提升，与 D318 一致） |
//!
//! ### 三、深度相等**正确且递归**
//!
//! `[[1],[2]] == [[1],[2]]` → `true`；`{a:{b:1}} == {a:{b:1}}` → `true`；
//! `{a:1,b:2} == {b:2,a:1}` → `true`（**键序无关**）；长度/顺序不符正确给 `false`。
//!
//! ⚠ 键**序**无关 ≠ 键**身份**无关：`{a:1} == {b:1}` → **`false`**（正确，
//! `Value::Dict` 是 `HashMap<String, Value>`）。D323 取证时一度把这两者
//! 搞混、把正确值判成缺陷 —— 与 D317/D320 同一族错误。
//!
//! ### 四、`==` / `!=` **完全对称**：0 个不对称格子。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d323_cmp_{}", slug(tag)));
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

fn cmp_of(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **主判据（否定轮）**：跨类型相等**必须**被 typeck 拒绝。
///
/// 这条钉的是「绝不静默给错答案」——本轮矩阵量出来的最关键一条。
/// 若将来有人给 `==` 加提升（就像给 `match` 加过那样），这里会红，
/// 提醒那是一次**语义变更**（D319 发现 3 仍在等裁决）。
#[test]
fn d323_cross_type_equality_is_rejected_never_silently_true() {
    for e in [
        "nil == false",
        "nil == 0",
        "nil == 0.0",
        "false == 0",
        "false == 0.0",
        "true == 1",
        "true == 1.0",
        "'a' == \"a\"",
        "\"1\" == 1",
        "1n == 1i",
        "1n == 1",
    ] {
        let (code, got) = cmp_of(e);
        assert_eq!(
            code, 2,
            "**D323**：跨类型相等 `{e}` 当前被 typeck 拒绝（exit 2）。\n\
             ⚠ 若它变成了 exit 0 并给出 `true`，那是**语义变更**（`==` 获得了\
             数值提升，或跨类型相等被静默放行）—— 后者会让 `nil == false` 这类\
             表达式静默给出错误答案。请先回到 CHANGELOG D319/D323 记录裁决依据。\n\
             实得: exit={code} out={got}"
        );
    }
    // 对照：数值塔内的 Int↔Float 提升是**既定**行为（D318 钉住）。
    for (e, want) in [
        ("1 == 1.0", "true"),
        ("1i == 1.0", "true"),
        ("1n == 1n", "true"),
    ] {
        let (code, got) = cmp_of(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 {got}");
        assert_eq!(got, want, "`print({e})` 应得 {want}; 实得 {got}");
    }
}

/// **深度相等**必须递归、且键**序**无关。
#[test]
fn d323_deep_equality_is_recursive_and_order_insensitive_for_dicts() {
    for (e, want) in [
        ("[1,2,3] == [1,2,3]", "true"),
        ("[1,2,3] == [1,2,3,4]", "false"),
        ("[1,2,3] == [3,2,1]", "false"),
        ("[[1],[2]] == [[1],[2]]", "true"),
        ("{a:1} == {a:1}", "true"),
        ("{a:1} == {a:2}", "false"),
        ("{a:1,b:2} == {b:2,a:1}", "true"),
        ("{a:{b:1}} == {a:{b:1}}", "true"),
        ("{a:{b:1}} == {a:{b:2}}", "false"),
        // 键**身份**相关（`Value::Dict` 是 `HashMap`）——「键序无关」≠「键身份无关」
        ("{a:1} == {b:1}", "false"),
    ] {
        let (code, got) = cmp_of(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`print({e})` 应得 {want}; 实得 {got}\n\
             ⚠ 特别注意 `{{a:1}} == {{b:1}}` 应为 **false**（键身份相关）；\
             D323 取证时曾把它误判为缺陷。"
        );
    }
}

/// **顺序比较只对数字有定义**，其余类型**必须干净报错**（不是静默给错值）。
#[test]
fn d323_ordering_is_numbers_only_and_errors_cleanly_otherwise() {
    // 数字：可比较
    for e in ["2 < 3", "2i <= 2i", "2.0 >= 1.5"] {
        let (code, got) = cmp_of(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 {got}");
    }
    // 其余类型：报错，且消息指明原因
    for e in [
        "\"a\" < \"b\"",
        "'a' < 'b'",
        "true < false",
        "nil < nil",
        "[1] < [2]",
        "{a:1} < {a:2}",
    ] {
        let (code, got) = cmp_of(e);
        assert_ne!(code, 0, "`{e}` 顺序比较当前**必须报错**（只对数字有定义）");
        assert!(
            got.contains("must be numbers"),
            "`{e}` 的报错应指明「操作数必须是数字」; 实得: {got}"
        );
    }
}

/// `==` / `!=` **完全对称** —— 本轮矩阵实测 0 个不对称格子。
#[test]
fn d323_equality_is_symmetric_across_all_type_pairs() {
    for (a, b) in [
        ("2", "2i"),
        ("2i", "2"),
        ("2", "2n"),
        ("\"a\"", "\"b\""),
        ("[1,2]", "[1,3]"),
        ("{a:1}", "{b:1}"),
        ("'a'", "'b'"),
        ("true", "false"),
    ] {
        for op in ["==", "!="] {
            let (c1, v1) = cmp_of(&format!("{a} {op} {b}"));
            let (c2, v2) = cmp_of(&format!("{b} {op} {a}"));
            assert_eq!(
                c1, c2,
                "`{a} {op} {b}` 与 `{b} {op} {a}` 的退出码应一致; 实得 {c1} vs {c2}"
            );
            if c1 == 0 {
                assert_eq!(
                    v1, v2,
                    "`{a} {op} {b}` = {v1} 但 `{b} {op} {a}` = {v2} —— \
                     相等/不等必须**对称**"
                );
            }
        }
    }
}
