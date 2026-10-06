//! v0.104.6 D346 —— `xform.*` 5 入口：`attach` 是**恒等函数**，
//! 四个构造器只返回**占位字符串**（`Value::Xform` 全仓零构造）
//!
//! ## 缺陷：spec 承诺的 transducer 机制**一行都没实现**
//!
//! `docs/mora-spec.md:1029-1033` 逐条承诺：
//!
//! ```text
//! xform.map(fn)              → transducer
//! xform.filter(fn)           → transducer
//! xform.take(n)              → transducer
//! xform.comp(xf1, xf2)      → transducer
//! xform.attach(xf, stream)   → **将 transducer 应用到 stream**
//! ```
//!
//! 而 `src/interpreter/builtins/xform.rs` 的实现（5 个 arm 全长）：
//!
//! ```rust
//! "map"    => Ok(Value::String(format!("<xform.map({})>",    describe(fn_val)))),
//! "filter" => Ok(Value::String(format!("<xform.filter({})>", describe(pred_val)))),
//! "take"   => Ok(Value::String(format!("<xform.take({})>",   describe(n)))),
//! "comp"   => Ok(Value::String(format!("<xform.comp({})>",   describe(other)))),
//! "attach" => Ok(stream.clone()),      // ← **恒等函数**
//! ```
//!
//! 实测（真实 CLI）：
//!
//! ```text
//! xform.attach([1, 2], …)   →  [1.0, 2.0]     ← 与输入**完全相同**
//! xform.take([1,2,3], 99)   →  <xform.take(list)>
//! xform.take([1,2,3])       →  <xform.take(list)>   ← 少传一个参数，输出**一样**
//! ```
//!
//! ## 为什么说 `Value::Xform` 是**死类型**
//!
//! `value.rs:93` 定义了 `Xform` 变体，但
//! `grep "Value::Xform"` 在全仓**零构造点** ——
//! `xform.*` 全部返回 `Value::String`。该类型从未被创建过。
//!
//! ## 为什么本条**不修**
//!
//! 修它 = **实现整套 transducer 机制**（pipeline 存储 / `attach` 真正应用
//! pipeline 到 stream / stream 的定义），这是**功能实现**而非缺陷修复，
//! 规模远超一次审计，且涉及「transducer 到底作用在什么类型上」的设计决定
//! （`xform.attach` 的第二参在 spec 里是 `stream`，而 `Value` 里**没有**
//! `Stream` 变体——D315 已记「Stream 相关能力未实现」）。
//!
//! ⇒ 与 D328 的 `partial` / `compose` 属**同一族**：dispatch 里写了、
//! typeck 也登记了、但底层能力不存在。
//!
//! **危害评估**：`xform.*` 零文档（`README.md` **零命中**）、
//! spec 里的三条签名是它**唯一**的「承诺」来源。
//! 用户照 spec 写 `xform.take(xs, 3)` 会得到一个**占位字符串**，
//! 拿它去 `attach` 只会**原样返回输入** —— 不会崩、不会错算，
//! 但也不会有任何效果。⇒ **静默无效**，比报错更糟。
//!
//! 已用判据钉住这个现状，让它**可见**；是否实现 transducer 交由裁决。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d346_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；取 stdout **全部**实质行。
///
/// ⚠ 探针**不带**尾随换行 —— `format!("print({body})")` 若 `body`
/// 自身以 `\n` 结尾，会多出一个孤立的 `)`，parser 报「Expected ')'」
/// （D345 翻了两次车，症状像产品 parser 坏了）。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d346x_{}_{}", n, slug(body)));
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
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **主断言**：`xform.attach` 是**恒等函数** —— 原样返回输入，不做任何变换。
///
/// 这条是整个 `xform.*` 的要害：spec 说它「将 transducer 应用到 stream」，
/// 实际是 `Ok(stream.clone())`。
#[test]
fn d346_attach_is_an_identity_function() {
    for (body, want) in [
        ("print(xform.attach([1, 2]))", "[1.0, 2.0]"),
        ("print(xform.attach([1, 2, 3]))", "[1.0, 2.0, 3.0]"),
        ("print(xform.attach(\"abc\"))", "abc"),
        ("print(xform.attach(nil))", "nil"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(code, 0, "`{body}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`{body}` 应**原样返回输入**（attach 是恒等函数）; 实得 {got}\n\
             ⚠ 若本条红，说明有人真的实现了 transducer —— 那是**有意的**功能变更"
        );
    }
}

/// **主断言**：四个构造器返回**占位字符串**，且**不校验**实参类型。
///
/// 关键反直觉点：`xform.take([1,2,3], 99)` 与 `xform.take([1,2,3])`
/// 输出**完全一样** —— 说明第 2 参被**完全忽略**。
#[test]
fn d346_constructors_return_placeholders_and_ignore_extra_args() {
    // take 的两种调用输出相同 ⇒ 第 2 参被忽略
    let (c1, g1) = ev("print(xform.take([1, 2, 3], 99))\n");
    let (c2, g2) = ev("print(xform.take([1, 2, 3]))\n");
    assert_eq!(c1, 0, "两参形式应成功; 实得 exit={c1} out={g1}");
    assert_eq!(c2, 0, "单参形式应成功; 实得 exit={c2} out={g2}");
    assert_eq!(
        g1, g2,
        "`xform.take` 的**多传实参被完全忽略**（输出逐字相同）; 实得 {g1} vs {g2}"
    );
    assert_eq!(
        g1, "<xform.take(list)>",
        "现状是**占位字符串**（`describe()` 对复杂类型只打印类型名）; 实得 {g1}"
    );

    // map / filter / comp 同样返回占位，且不校验实参类型
    for (body, want) in [
        ("print(xform.map(\"anything\"))", "<xform.map(anything)>"),
        ("print(xform.map(5))", "<xform.map(5)>"),
        (
            "print(xform.filter(\"anything\"))",
            "<xform.filter(anything)>",
        ),
        ("print(xform.filter(5))", "<xform.filter(5)>"),
        ("print(xform.comp(\"anything\"))", "<xform.comp(anything)>"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(
            code, 0,
            "`{body}` 应成功（不校验实参）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, want, "`{body}` 应得 {want}; 实得 {got}");
    }
}

/// **源码侧**：`Value::Xform` 是**死类型** —— 定义了但全仓**零构造**。
///
/// 这条是「attach 为何是恒等函数」的**根因**：连能装 pipeline 的
/// 值类型都没被创建过。
#[test]
fn d346_value_xform_variant_exists_but_is_never_constructed() {
    let value_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/value.rs"))
        .expect("读 value.rs");
    assert!(
        value_rs.contains("    Xform,"),
        "`Value` 应仍有 `Xform` 变体（D345 前的定义）"
    );

    let xform_rs = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/builtins/xform.rs"
    ))
    .expect("读 xform.rs");
    assert!(
        !xform_rs.contains("Value::Xform"),
        "`xform.rs` 一旦开始构造 `Value::Xform`，说明 transducer 真的实现了 —— \
         本条的「死类型」前提需要重新评估"
    );
    // 五个 arm 全是 `Value::String` 占位 / 恒等
    // 五个 arm 里 4 个是 `<xform.*>` 占位构造（map/filter/take/comp）；
    // `attach` 是恒等函数，不产生占位。
    // ⚠ 用 `contains` 逐个数，而不是数 `format!(` 的出现次数 ——
    //   `filter` 的那处 `format!` 跨行，前缀与后续行不在同一串里。
    let ph = xform_rs.matches("\"<xform.").count();
    assert!(
        ph >= 4,
        "`xform.rs` 应有 ≥4 处 `\"<xform.` 占位前缀（map/filter/take/comp）; 实得 {ph}"
    );
    for tag in ["map", "filter", "take", "comp"] {
        assert!(
            xform_rs.contains(&format!("\"<xform.{tag}(")),
            "`xform.{tag}` 的占位前缀应仍在"
        );
    }
    assert!(
        xform_rs.contains("\"attach\" => {") && xform_rs.contains("Ok(stream.clone())"),
        "`attach` 应仍是 `Ok(stream.clone())`（恒等函数）"
    );
}

/// **对照组**：未知方法明确报错；`mock.*` 的类型错也明确报错。
///
/// 这些是「正确报错」的邻面，说明 `xform.*` 的「不校验」是**局部**的
/// （构造器不校验实参类型），不是整个模块都不校验。
#[test]
fn d346_unknown_methods_still_error_explicitly() {
    for (body, needle) in [
        ("print(xform.nosuch())", "no method"),
        ("print(mock.nosuch())", "unknown method"),
    ] {
        let (code, got) = ev(&format!("{body}\n"));
        assert_eq!(code, 1, "`{body}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains(needle),
            "`{body}` 应报 `{needle}`; 实得: {got}"
        );
    }

    // `mock.register` 的**第一个**实参必须是 String（它**有**类型校验）
    let (code, got) = ev("print(mock.register(1, |x| x))\n");
    assert_ne!(
        code, 0,
        "`mock.register(1, …)` 应报错（与 `xform.map(5)` 不校验形成对照）; 实得 exit={code} out={got}"
    );
}
