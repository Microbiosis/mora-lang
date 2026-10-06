//! v0.104.6 D326：String / Dict 方法的参数形态矩阵（54 个调用）—— 零 panic，
//! 并钉住三条**未文档化**的现状契约。
//!
//! D325 在 list 方法面挖出 `reshape()` 静默丢数据。本轮扫同一手法未覆盖的
//! **String / Dict** 方法面。
//!
//! ## 量出来的三条
//!
//! ### ① `split("")` 继承 Rust 的 2n+1 行为（**未文档化、未钉住**）
//!
//! ```text
//! "a,b,,c".split("")  →  [, a, ,, b, ,, ,, c, ]   ← 2n+1 个元素，两端多出空串
//! ```
//!
//! 横向对比：`"abc".split("")` 在 **Python 报 `ValueError`**、Java / Go 给
//! `["a","b","c"]`、**Rust** 给 `["", "a", "b", "c", ""]`。Mora 当前继承的是
//! **Rust** —— 也是唯一产生两端空串的那个。
//!
//! 用户写 `s.split("")` 几乎总是想要「拆成字符」，多出的两个空串与该意图不符；
//! 但「修成什么」（报错 / 给字符）两种都自洽 ⇒ **语义决定，本轮只报告不实施**，
//! 用本判据把当前行为钉住。
//!
//! ### ② `split` 与 `dict.get` 都**只接受恰好 1 个参数**（无 limit / 无 default）
//!
//! `"a,b".split(",", 2)`、`{a:1}.get("a","def")` 均被 typeck 拒绝。
//! Python 两者都支持 ⇒ 属**能力缺口**，不是缺陷。
//!
//! ### ③ `dict.get(缺键)` **静默返回 `nil`**
//!
//! 与 `list.get(越界)` **报错**（D153 明确改过）形成对照。但 dict 查不到键是
//! 常态（`d["missing"]` 同样给 `nil`），内部自洽 ⇒ 只记档。
//!
//! ## 其余 51 个调用全部符合预期
//!
//! 零 panic；参数个数/类型错、非字符串键、非字符串 needle 全部被 typeck 拒绝；
//! `upper`/`lower` 对 CJK 正确；`len` 按**字符**不按字节（CJK 串 `"中文abc".len()`
//! → `5`）。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d326_{}", slug(tag)));
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

/// **契约 ①**：空分隔符的当前行为被钉住。
///
/// ⚠ 若将来裁决要改（报错 or 给纯字符列表），本条会红并应翻转；
/// 失败消息里已写明两种合法答案。
#[test]
fn d326_status_quo_split_with_empty_separator_keeps_rust_semantics() {
    for (e, want) in [
        ("\"a,b,,c\".split(\",\")", "[a, b, , c]"),
        // 2n+1：字符之间 + 两端都是空串（Rust 语义）
        ("\"abc\".split(\"\")", "[, a, b, c, ]"),
        ("\"a,b\".split(\"\")", "[, a, ,, b, ]"),
        // 空字符串本身
        ("\"\".split(\",\")", "[]"),
        // 分隔符不存在 / 多字符 / 重复
        ("\"a,b,,c\".split(\"z\")", "[a,b,,c]"),
        ("\"a,b,,c\".split(\"a,\")", "[, b,,c]"),
        ("\"a,b,,c\".split(\",,\")", "[a,b, c]"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`print({e})` 当前应得 `{want}`。\n\
             ⚠ 这条钉的是**未文档化**的现状（继承 Rust 的 2n+1 语义）。\n\
             若裁决改为「报错」或「给纯字符列表」，本条应随之翻转 ——\n\
             两种答案都自洽，务必在 CHANGELOG D326 记下依据。\n  实得: `{got}`"
        );
    }
}

/// **`replace` 的空模式行为与 Python 一致**（这条**不是**争议项，只是钉住）。
#[test]
fn d326_replace_with_empty_pattern_matches_python() {
    for (e, want) in [
        ("\"a,b,,c\".replace(\",\", \";\")", "a;b;;c"),
        ("\"a,b,,c\".replace(\",\", \"\")", "abc"),
        ("\"a,b,,c\".replace(\"z\", \"X\")", "a,b,,c"),
        // 空模式在两端与每个字符间插入 —— 与 Python 的 str.replace 一致
        ("\"ab\".replace(\"\", \"X\")", "XaXbX"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 `{want}`; 实得 `{got}`");
    }
}

/// **契约 ②③ + 正常路径**：`split` / `dict.get` 只接受恰好 1 个参数；
/// 参数类型/个数错、非字符串键、缺键行为，全部钉住。
#[test]
fn d326_arity_types_and_missing_key_behaviour() {
    // 正常路径
    for (e, want) in [
        ("\"a,b,,c\".contains(\"b\")", "true"),
        ("\"a,b,,c\".starts_with(\"a\")", "true"),
        ("\"a,b,,c\".ends_with(\"c\")", "true"),
        ("\"a,b,,c\".starts_with(\"\")", "true"), // 空前缀恒真（标准语义）
        ("\"  pad  \".trim()", "pad"),
        ("\"abc\".upper()", "ABC"),
        ("\"abc\".lower()", "abc"),
        ("\"中文abc\".upper()", "中文ABC"), // CJK 不受影响
        ("\"中文abc\".len()", "5"),         // 字符数，不是字节数
        ("{a:1}.get(\"a\")", "1.0"),
        ("{a:1}.set(\"b\", 2)", "{a: 1.0, b: 2.0}"),
        ("{a:1}.keys()", "[a]"),
        ("{a:1}.values()", "[1.0]"),
        ("{a:1}.len()", "1"),
        ("{}.keys()", "[]"),
    ] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 `{want}`; 实得 `{got}`");
    }

    // 缺键 → nil（**静默**，与 `list.get` 越界报错形成对照；dict 内部自洽）
    for e in ["{a:1}.get(\"zz\")", "{a:1}.get(\"\")", "{}.get(\"a\")"] {
        let (code, got) = ev(e);
        assert_eq!(code, 0, "`{e}` 应成功（缺键给 nil）; 实得 exit={code}");
        assert_eq!(got, "nil", "`print({e})` 应得 nil; 实得 `{got}`");
    }

    // 参数个数 / 类型错：全部被拒
    for e in [
        "\"a,b\".split(\",\", 2)",
        "\"a,b\".split()",
        "\"a,b\".split(\",\", 1, 2)",
        "\"ab\".replace(\"a\")",
        "\"ab\".replace(\"a\", \"z\", 1)",
        "\"ab\".contains()",
        "\"ab\".contains(1)",
        "{a:1}.get()",
        "{a:1}.get(\"a\",\"def\")",
        "{a:1}.set(\"b\")",
        "{a:1}.set(1, 2)", // 非字符串键
        "{a:1}.get(1)",    // 非字符串键
    ] {
        let (code, got) = ev(e);
        assert_eq!(
            code, 2,
            "`{e}` 当前应被 typeck 拒绝（exit 2）—— 参数个数/类型约束。\n\
             若它变成 exit 0，那要么是**能力新增**（`split` 支持 limit、\
             `get` 支持 default —— 属语义变更），要么是**约束被放宽**\
             （非字符串键被静默接受 —— 那是缺陷）。请分辨后再改本判据。\n\
             实得: exit={code} out={got}"
        );
    }
}
