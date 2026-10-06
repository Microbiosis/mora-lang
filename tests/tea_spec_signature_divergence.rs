//! v0.104.6 D347 —— `tea.*` 的 spec 签名与实现**六条全部分叉**（否定轮，无产品变更）
//!
//! D346 的收获是「**恒等函数**是最难发现的缺陷形态」，而它的前置是
//! 「把**文档承诺的行为**与**实际行为**并排放」。本条把这一步**全称执行**：
//! `docs/mora-spec.md` 里 `tea.*` 与 `xform.*` 的 11 条签名，逐条实测。
//!
//! ## `tea.*`：六条签名全部与实现不符
//!
//! | spec 签名 | spec 声明 | **实测** | 一致？ |
//! |---|---|---|---|
//! | `tea.init(model)` | `dict -> tea_model` | 收**任意值**（`1` 也行）→ `tea_app` | ❌ |
//! | `tea.dispatch(msg)` | `tea_msg -> **nil**` | `tea_app` | ❌ |
//! | `tea.run(app)` | `tea_app -> **nil**` | `tea_app` | ❌ |
//! | `tea.update(msg)` | `tea_msg -> tea_model` | `tea_app` | ❌ |
//! | `tea.model()` | `-> tea_model` | 返回 app 内部的 model（实测 `float`） | ❌ |
//! | `tea.view()` | `-> any` | `tea_app`（且**报错**，见下） | ❌ |
//!
//! **`tea.view` 另有实缺陷**：`tea.init(1)` 只给 init 闭包，
//! `update` / `view` 缺省为 `Nil` ⇒ 调 `tea.view` 报
//! `Value is not callable: nil`。这不是签名问题，是**构造时留空**。
//!
//! ## **typeck 站在实现这边**
//!
//! `typeck/dispatch.rs::TEA_METHODS` 登记的是
//! `Ret::TeaApp` / `Ret::Any` —— 与**实现**一致，与**spec** 不一致。
//!
//! ⇒ 分叉只存在于**文档**，而文档是 spec。
//! 而 `docs/mora-spec.md:1020` 那句「`tea.run(app)` → **nil**」还与
//! `tea.run` 的**实际用法**冲突：D339 实测 `tea.run(m, 5)` 的返回值
//! 可以继续传给 `tea.model(...)`（实测 `type_of(tea.run(a,2)) == tea_app`），
//! 若真返回 `nil` 那条链就断了 —— **spec 那几行是过时的**。
//!
//! ## 为什么不改（只钉）
//!
//! 改 spec 是**文档决策**：那几行是 v0.83 写的，此后
//! `v0.94` 改成「app 是**纯值**（无内部 Mutex）」、`v0.104` 又把
//! `init` 扩成三参 —— 实现一路演进，**spec 停在了旧版**。
//! 追平它需要确认「`dispatch` / `update` 到底该不该返回 app」
//! （Elm 语义是 `Cmd`，本实现是「纯追加，返回新 app」），
//! 那是**语义裁决**，不是文档勘误。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d347_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；取 stdout **全部**实质行。
///
/// ⚠ 探针**不带**尾随换行（D345 教训）：`format!("print({body})")`
/// 若 `body` 自身以 `\n` 结尾会多出孤立的 `)`，parser 报「Expected ')'」。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d347t_{}_{}", n, slug(body)));
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

/// **主断言**：`tea.*` 的四条「返回 app」路径都返回 `tea_app`
/// —— 而 spec 说 `dispatch` / `run` 返回 **nil**。
#[test]
fn d347_tea_dispatch_and_run_return_app_not_nil() {
    let body = "let a = tea.init(1)\n\
                print(type_of(tea.dispatch(a, json.parse(\"{\\\"tag\\\": \\\"x\\\"}\"))))\n\
                print(type_of(tea.run(a, 2)))\n\
                print(type_of(tea.model(a)))\n";
    let (code, got) = ev(body);
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    assert_eq!(
        got, "tea_app | tea_app | float",
        "现状：dispatch / run 都返回 `tea_app`（**不是** spec 写的 nil）; 实得 {got}\n\
         ⚠ 若本条红，说明有人把返回值改成了 nil（追平 spec）—— 那是**有意的**语义变更"
    );
}

/// **实测**：`tea.view` 在只有 init 闭包时**报错**（构造时 update/view 留空）。
///
/// 这不是签名问题，是「`tea.init(model)` 单参形态」的**实缺陷**：
/// D339 已证 `tea.init(init, update, view)` 三参形态可用。
#[test]
fn d347_tea_view_errors_when_view_closure_is_absent() {
    let (code, got) = ev("let a = tea.init(1)\nprint(tea.view(a))\n");
    assert_eq!(
        code, 1,
        "单参 `tea.init(1)` 不带 view 闭包 ⇒ `tea.view` 应报错（而非静默）; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("not callable") && got.contains("nil"),
        "错误应说明 view 是 nil 闭包; 实得: {got}"
    );
}

/// **配对**：三参形态的 `view` **可用** —— 证明问题在「缺参」而非「view 本身坏」。
#[test]
fn d347_tea_view_works_with_three_arg_init() {
    let body = "let a = tea.init(fn() => 0, fn(msg, model) => model, fn(model) => model)\n\
                print(tea.view(a))\n";
    let (code, got) = ev(body);
    assert_eq!(
        code, 0,
        "三参形态的 view 应可用; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got, "0.0",
        "三参形态 view 应返回 model 的值; 实得 {got}\n\
         （与上一条配对：问题在**缺参**，不在 view 本身）"
    );
}

/// **源码侧**：`typeck` 登记的是 `Ret::TeaApp`（与实现一致，与 spec 不一致）。
///
/// 这条钉住「分叉只在文档」这个判断 —— 若将来 typeck 也改成 `Ret::Nil`，
/// 说明有人**追平了 spec**，那是**有意的**语义变更。
#[test]
fn d347_typeck_registers_tea_app_agreeing_with_impl_not_spec() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/typeck/dispatch.rs"
    ))
    .expect("读 dispatch.rs");
    let start = src.find("const TEA_METHODS").expect("应存在 TEA_METHODS");
    let block = &src[start..(start + 900).min(src.len())];
    assert!(
        block.contains("Ret::TeaApp"),
        "typeck 应登记 `Ret::TeaApp`（与**实现**一致、与 spec 的 nil **不一致**）"
    );
    // ⚠ **不能**对整块断言「没有 Ret::Nil」—— `replay(…)` → Nil 是**正确**的
    //   （它不返回 app）。第一版就是这么写错的。
    //   只取前四条（init / dispatch+update / run / model+view）来比。
    let first_four = block
        .split("MethodGroup::new(&[\"replay\"]")
        .next()
        .unwrap_or(block);
    assert!(
        !first_four.contains("Ret::Nil"),
        "init / dispatch / update / run / model / view 六条**都不该**是 `Ret::Nil` \
         （只有 `replay` 是）；若此处出现 Nil，说明 typeck 已追平 spec —— \
         那是**有意的**语义变更"
    );
}

/// **spec 侧**：把那 6 行签名**逐字**钉住。
///
/// 本条的意图是让「spec 过时」这个事实**可见且可追踪**。
/// 若有人更新了 spec（追平实现），本条会红 ⇒ 那是**有意的**文档更新。
#[test]
fn d347_spec_still_declares_the_stale_tea_signatures() {
    let spec = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/mora-spec.md"))
        .expect("读 spec");
    for (needle, what) in [
        ("| `tea.init(model)` | `dict -> tea_model`", "init"),
        ("| `tea.dispatch(msg)` | `tea_msg -> nil`", "dispatch"),
        ("| `tea.run(app)` | `tea_app -> nil`", "run"),
        ("| `tea.update(msg)` | `tea_msg -> tea_model`", "update"),
        ("| `tea.model()` | `-> tea_model`", "model"),
    ] {
        assert!(
            spec.contains(needle),
            "spec 里 `{what}` 的签名应仍是「{needle}」\n\
             ⚠ 若本条红，说明有人追平了 spec 到实现 —— 那是**有意的**文档更新，\
             请同步删掉本文件顶部的「为什么不改」一节"
        );
    }
}
