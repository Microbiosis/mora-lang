//! v0.104.6 D325：`reshape()` 目标**小于**源时**静默丢数据**（已修）。
//!
//! ## 现象（修前）
//!
//! ```text
//! [1,2,3,4,5,6].reshape(1,1)  →  [[1.0]]     ← 5 个元素凭空消失
//! [1,2,3,4].reshape(1,2)      →  [[1.0, 2.0]] ← 2 个元素消失
//! ```
//!
//! **退出码 0、零诊断。** 任何在 reshape 结果上做聚合（`sum` / 循环累加）的
//! 代码都会拿到**错误的数** —— 实测 `[1,2,3,4].reshape(1,1)` 的元素和是
//! **1.0**（应为 10.0）。
//!
//! ## 为什么判为缺陷而不是「设计」
//!
//! 查证了四处（吸取 D320 教训：**肯定断言也需要自己的验证**）：
//!
//! | 出处 | 内容 |
//! |---|---|
//! | `docs/learning-plan.md:166` | 「元素按 ravel 顺序复制，**不足则循环重复**」—— 方向是「补」，**从未说「多就丢」** |
//! | `docs/mora-spec.md:1068` | 只写「重塑列表」 |
//! | `tests/list_methods.rs:110` | `// 既有行为：不足时循环重复已有前缀补齐` —— **填充**是设计 |
//! | `tests/list_methods.rs` 的 6 条 reshape 判据 | 全是「恰好」或「填充」，**无一条覆盖截断** |
//!
//! ⇒ **填充是设计、截断是漏掉的方向**。且 numpy / Julia 的 `reshape` 在
//! size 不匹配时**报错**。修它不破坏任何既有判据（实测 6 个相关测试目标 72 条全绿）。
//!
//! ## 根因
//!
//! `method_dispatch.rs`：`while flat.len() < total` **只增长不收缩**，
//! 而 `flat[r*cols..(r+1)*cols]` 只读**前缀** ⇒ 尾部元素被静默丢弃，
//! 代码里没有任何检查。
//!
//! ## 修法
//!
//! `total < flat.len()` 时返回 `MoraError`，并提示用 `take()`。填充行为**逐字不变**。
//! 与本文件既有立场一致（v0.104.6 修 `list.get` 越界静默返回 nil 时）：
//! **静默改变数据形状必须是显式错误**。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d325_{}", slug(tag)));
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

fn sh(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **主判据**：目标**小于**源时**必须报错**，绝不静默丢元素。
///
/// **反向牙齿已验证**：删掉 `method_dispatch.rs` 里的
/// `if total < flat.len()` 守卫 → 本条红（且元素和从 10.0 掉到 1.0）。
#[test]
fn d325_reshape_never_silently_drops_elements() {
    for (e, held, have) in [
        ("[1,2,3,4].reshape(1,1)", 1, 4),
        ("[1,2,3,4].reshape(1,2)", 2, 4),
        ("[1,2,3,4].reshape(2,1)", 2, 4),
        ("[1,2,3,4,5,6].reshape(1,1)", 1, 6),
        ("[1,2,3,4,5,6].reshape(1,3)", 3, 6),
        ("[1,2,3,4,5,6].reshape(3,1)", 3, 6),
        ("[1,2,3,4,5,6,7,8].reshape(1,2)", 2, 8),
    ] {
        let (code, got) = sh(e);
        assert_ne!(
            code, 0,
            "**D325**：`{e}` 目标只装得下 {held} 个、源有 {have} 个 ⇒ 必须报错。\n\
             修前它**静默丢弃** {held}..{have} 个元素（退出码 0、零诊断），\
             任何在结果上做聚合的代码都会拿到**错误的数**。\n  实得: {got}"
        );
        assert!(
            got.contains("reshape()"),
            "`{e}` 的报错应指明是 reshape; 实得: {got}"
        );
        assert!(
            got.contains("never drops") || got.contains("take()"),
            "`{e}` 的报错应说明「只补不丢」并指向 `take()`; 实得: {got}"
        );
    }
}

/// **聚合口径**：`reshape` 之后元素和必须仍是 10.0（修前是 1.0）。
///
/// 这一条是本缺陷的**可观察后果** —— 它不检查报错文案，只检查
/// 「数据有没有被吃掉」，因此对任何修法都成立。
#[test]
fn d325_reshape_preserves_element_sum() {
    let (code, got) = run(
        "let t = [1,2,3,4].reshape(2,2)\n\
         let s = 0\n\
         for row in t\n  for v in row\n    s = s + v\n  end\nend\n\
         print(s)\n",
        "sum_exact",
    );
    assert_eq!(code, 0, "恰好整除时应成功; 实得 {got}");
    assert_eq!(
        got, "10.0",
        "reshape(2,2) 之后元素和必须仍是 10.0（数据未被改动）"
    );
}

/// **填充与「恰好」两种合法路径必须逐字不变**（本修复只动截断方向）。
#[test]
fn d325_reshape_padding_and_exact_shapes_unchanged() {
    for (e, want) in [
        ("[1,2,3,4].reshape(2,2)", "[[1.0, 2.0], [3.0, 4.0]]"),
        ("[1,2,3,4].reshape(1,4)", "[[1.0, 2.0, 3.0, 4.0]]"),
        ("[1,2,3,4].reshape(4,1)", "[[1.0], [2.0], [3.0], [4.0]]"),
        // 填充：**循环重复**已有前缀（learning-plan.md:166 记的「既有行为」）
        (
            "[1,2,3,4].reshape(2,3)",
            "[[1.0, 2.0, 3.0], [4.0, 1.0, 2.0]]",
        ),
        (
            "[1,2,3,4].reshape(3,2)",
            "[[1.0, 2.0], [3.0, 4.0], [1.0, 2.0]]",
        ),
    ] {
        let (code, got) = sh(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`print({e})` 应得 `{want}`; 实得 `{got}`\n\
             ⚠ 若本条失败，说明有人改了填充/恰好路径 —— D325 只动截断方向。"
        );
    }
}
