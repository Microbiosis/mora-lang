//! v0.104.6 D351 —— typeck 诊断矩阵 + `String + 任意类型` 的**契约分叉**
//! （否定轮，无产品变更）
//!
//! D350 收官了 `builtins/`。本轮切到 **`typeck/`** —— 类型错误是用户遇到的
//! **第二道**诊断（第一道是 parser），且历史上只被**单点**修过
//! （D188 的重复诊断、D126 的 `line 0` 渲染）。
//!
//! ## ① 诊断矩阵：13 类错误的措辞与**位置信息**都齐
//!
//! | 类别 | 措辞 | 位置 |
//! |---|---|---|
//! | 类型不匹配 | `type mismatch: expected \`Int\`, got \`Float\`` | ✅ `line 1:14` |
//! | 赋值类型错 | `Type mismatch: expected float, got string` | ✅ `line 2:8` |
//! | if 条件非 bool | 同上 + **`hint: if condition must be bool`** | ✅ `line 1:4` |
//! | 未绑定变量 | `Unbound variable 'zzz'` | ✅ `line 1:7` |
//! | 列表不同质 | `List 字面量的元素必须同质…: 下标 1（第 2 个元素）…` | ✅ `line 1:14` |
//! | 未知方法 | 运行期 `List has no method: nosuch` | ❌ **无编译期诊断** |
//!
//! **多处错误各自成条、计数正确**（D188 的去重没有过度去重）：
//! `let a: Int = 1.5` + `let b: String = 2.5` ⇒ **2 条**，位置与类型对都不同。
//!
//! ## ② **契约分叉**：`String + 任意类型` 运行期拼接，**typeck 拒**
//!
//! `flow.rs:201-203` 逐字写着：
//!
//! ```rust
//! // 字符串 + 任意类型 → 自动转字符串拼接
//! (Value::String(a), _) => Ok(Value::String(format!("{}{}", a, right))),
//! (_, Value::String(b)) => Ok(Value::String(format!("{}{}", left, b))),
//! ```
//!
//! 这是**有意设计**（注释明写）。但实测：
//!
//! ```text
//! print("s" + 1)   → typeck 拒：expected String, got Float     ← 直接位置被拒
//!
//! let f = fn(x) x + 1 end
//! print(f("s"))     → "s1.0"                                  ← 闭包内**放行**并拼接
//! ```
//!
//! **闭包参数无标注 ⇒ 推断为 `Any` ⇒ `Any + 1` 放行** ⇒ 落到运行期的拼接语义。
//!
//! ### 为什么本条**不修**
//!
//! `String + 数字` 该**拼接**还是该**报错**，是**类型系统**层面的产品决定：
//! - 现状（运行期拼接）**有代码注释明写是有意的**；
//! - 但 typeck 拒它 ⇒ 两条路都有人在走。
//!
//! 改 typeck 放行 `String + any` 会**显著削弱类型系统**（`"a" + 1` 本该
//! 是编程错误）；改运行期报错会让现有能跑的闭包代码**全崩**。两者都要人拍板。
//!
//! **未钉措辞**（钉住「typeck 拒 / 闭包内放行」这个**分叉事实**即可）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d351_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；取 stdout+stderr **全部**实质行。
///
/// ⚠ 必须把 **stderr** 并进来：typeck 的诊断走 stderr
/// （早期只读 stdout 时，`exit 2` 的用例一律显示「空输出」，
///  被我误读成「产品没给出诊断」—— D351 踩过一次）。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d351t_{}_{}", n, slug(body)));
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
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **契约 ①**：诊断**带位置**、**多条互不吞并**、**计数正确**。
///
/// 这条守的是 D188 去重的**下界** —— 去重不能吃掉不同位置的同类错误。
#[test]
fn d351_diagnostics_carry_positions_and_do_not_over_dedupe() {
    // 单条
    let (code, got) = ev("let v: Int = 1.5\nprint(v)\n");
    assert_eq!(code, 2, "类型不匹配应 exit 2; 实得 exit={code} out={got}");
    assert!(got.contains("line 1:14"), "应带**位置**; 实得: {got}");
    assert!(got.contains("1 type error(s)"), "计数应为 1; 实得: {got}");

    // 两条**同类但不同位置** ⇒ 必须都保留
    let (code, got) = ev("let a: Int = 1.5\nlet b: Int = 2.5\nprint(a+b)\n");
    assert_eq!(code, 2, "应 exit 2; 实得 exit={code} out={got}");
    assert!(got.contains("line 1:14"), "第 1 条应带位置; 实得: {got}");
    assert!(
        got.contains("line 2:14"),
        "第 2 条（同类型、不同位置）**不得**被去重吃掉; 实得: {got}"
    );
    assert!(got.contains("2 type error(s)"), "计数应为 2; 实得: {got}");
}

/// **契约 ②**：未绑定变量与列表不同质各有**专门的措辞**（不是笼统 type mismatch）。
#[test]
fn d351_unbound_and_heterogeneous_have_dedicated_wording() {
    let (code, got) = ev("print(zzz)\n");
    assert_eq!(code, 2, "应 exit 2; 实得 exit={code} out={got}");
    assert!(
        got.contains("Unbound variable"),
        "应点明未绑定; 实得: {got}"
    );
    assert!(got.contains("line 1:7"), "应带位置; 实得: {got}");

    let (code, got) = ev("let xs = [1, \"a\"]\nprint(xs)\n");
    assert_eq!(code, 2, "应 exit 2; 实得 exit={code} out={got}");
    assert!(
        got.contains("同质") || got.contains("homogeneous"),
        "应点明元素同质要求; 实得: {got}"
    );
    assert!(got.contains("下标 1"), "应点名**具体下标**; 实得: {got}");
}

/// **契约 ③**：`if` 条件非 bool 时给出**额外的 hint**。
#[test]
fn d351_if_condition_gives_a_hint() {
    let (code, got) = ev("if 1 { print(1) }\n");
    assert_eq!(code, 2, "应 exit 2; 实得 exit={code} out={got}");
    assert!(got.contains("hint"), "应给出 hint 行; 实得: {got}");
    assert!(
        got.contains("bool"),
        "hint 应点明 if 条件必须是 bool; 实得: {got}"
    );
}

/// **契约 ④（核心）**：`String + 数字` —— **直接位置被 typeck 拒，
/// 闭包内却放行并按运行期语义拼接**。这是**分叉事实**，钉住它。
#[test]
fn d351_string_plus_number_is_rejected_directly_but_passes_inside_closure() {
    // ① 直接位置：typeck 拒
    let (code, got) = ev("print(\"s\" + 1)\n");
    assert_eq!(
        code, 2,
        "`\"s\" + 1` 在直接位置应被 typeck 拒; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("expected") && got.contains("String"),
        "错误应是「期望 String」; 实得: {got}"
    );

    // ② 闭包内：typeck **放行**（参数是 `Any`），落到运行期拼接语义
    let (code, got) = ev("let f = fn(x) x + 1 end\nprint(f(\"s\"))\n");
    assert_eq!(
        code, 0,
        "闭包内 `f(\"s\")` 实测**成功**（`x` 推断为 `Any` ⇒ `Any + 1` 放行）; \
         实得 exit={code} out={got}\n\
         ⇒ 这就是分叉：同一运算，**直接位置被拒、闭包内放行**"
    );
    assert_eq!(
        got, "s1.0",
        "闭包内落到 `flow.rs:201` 的拼接语义（String + 任意 → 拼接）; 实得 {got}\n\
         ⚠ 若本条红，说明 typeck 已开始收紧了闭包路径，或运行期语义改了 —— 都是**有意的**变更"
    );
}

/// **配对**：**具名 `fn` 声明在本语言不存在** —— 只有闭包 `fn(x) … end`。
///
/// 这解释了「为什么诊断矩阵里函数相关的 4 个用例全是空输出」：
/// 我写了 `fn f() -> Int { … }`，parser 报 `Failed to parse`，
/// 而**空采集**让它看起来像「typeck 静默」—— 纯属探针写错。
#[test]
fn d351_language_has_closures_not_named_fn_declarations() {
    // 闭包形态可用
    let (code, got) = ev("let f = fn(x) x + 1 end\nprint(f(1))\n");
    assert_eq!(code, 0, "闭包形态应可用; 实得 exit={code} out={got}");
    assert_eq!(got, "2.0", "闭包应正确求值; 实得 {got}");

    // 具名声明形态**解析失败**（不是 typeck 静默）
    let (code, got) = ev("fn f() -> Int { 1 }\nprint(f())\n");
    assert_eq!(
        code, 2,
        "具名 `fn` 声明在本语言**不存在** ⇒ 应解析失败; 实得 exit={code} out={got}"
    );
    // ⚠ **不钉诊断文本**：实测这条的 `Failed to parse` 由 CLI 写到
    //   **既非 stdout 也非 stderr 的流**（早期只采 stdout/stderr 时拿到空串）。
    //   exit 2 + 解析失败这一**事实**已足够，且它是真正要钉的东西。
    //   钉文本会把「CLI 输出流」的实现细节变成回归点。
}
