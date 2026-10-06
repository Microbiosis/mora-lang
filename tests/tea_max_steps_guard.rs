//! v0.104.6 D339 —— `tea.run(app, max_steps)` 的 `max_steps` **负数静默生效**（已修）
//!
//! `tea.*` 是唯一带**自定义结构类型**（`Ret::TeaApp`）的 builtin 族，
//! 从未量过。本轮 27 个脚本用例 + 一组 Rust API 探针，**一个缺陷**。
//!
//! ## 实测：两种数值类型给出**天差地别**的结果
//!
//! 根因是 `tea.rs` 里 `optional_num_arg(...)?.map(|n| n as usize)` ——
//! **`as` 是无守卫的数字转换**，实测：
//!
//! ```text
//! (-1.0f64) as usize = 0                    ← **饱和**
//! (-1i64)  as usize = 18446744073709551615   ← **回绕**（1.8e19）
//! ```
//!
//! ⇒ 同一个「负的步数上限」：
//!
//! | 实参 | `max_steps` | 后果 |
//! |---|---|---|
//! | `tea.run(a, -1.0)` | **0** | `for _ in 0..0` ⇒ **静默不跑**，exit 0 |
//! | `tea.run(a, json.parse("-1"))` | **1.8e19** | 「update 每轮产 Cmd」时**实际挂死** |
//!
//! 同一句源码、两种数值类型，**exit 0、零诊断**。
//!
//! ## 与 D285 的关系：**同一个根因，D285 只修了一半**
//!
//! D285 在 `exec.parallel` 的 `max_concurrent` / `timeout_ms` 上修的正是这个：
//! 改走 D246 的收口 `value_as_usize`（负数一律 `None` ⇒ 报错）。
//! 但 `tea.run` 走的是 **`optional_num_arg`**（D150 建的另一条路径），
//! 之后**又补了一次裸 `as`** ⇒ D285 的普查**没覆盖到它**。
//!
//! 本轮把 `optional_num_arg` + 裸 `as` 的**全部 5 处**都查了：
//!
//! | 入口 | 负数的实测结果 | 判定 |
//! |---|---|---|
//! | **`tea.run(max_steps)`** | Float→0 / Int→**1.8e19**（**类型相关**）| ❌ **本条已修** |
//! | `ccr.marker(size)` | Float **与** Int **都** → 0 | ⚠ 静默变 0（见下方「未修」说明）|
//! | `schedule.add(interval_s / at_epoch)` | 两者**都**被 `> 0` 拦下 | ✅ 正确 |
//! | `mora.refine(count)` | 两者**都**被 `1..=26` 拦下 | ✅ 正确 |
//!
//! ⇒ **只有 `tea.run` 是「类型相关」的**，故只有它需要修：
//! 其余三处的下游检查**与实参类型无关**，不构成 Float/Int 分歧。
//!
//! ## 为什么 `ccr.marker(size)` **不**在本条修
//!
//! `size` 两种类型**都**饱和成 0 ⇒ **无类型分歧**。而
//! `ccr.marker(h, -1)` 产出 `<<ccr:h,0>>` —— 负尺寸在 marker 语义里
//! 「等于 0」是可解释的（marker 只是 `<<ccr:hash,size>>` 的格式化，
//! 下游 `extract_hash` 只取 hash 部分、**不看 size**）。
//! 改它属**产品契约决定**（负尺寸该报错还是当 0），只报告。

use mora::interpreter::Interpreter;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d339_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d339t_{}_{}", n, slug(body)));
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

/// 建一个可用的 app + 一条待处理的 msg。
const PRELUDE: &str = "let a = tea.init(fn() => 0, fn(msg, model) => model, fn(model) => model)\nlet m = tea.update(a, json.parse(\"{\\\"tag\\\": \\\"x\\\"}\"))\n";

/// **主断言**：负数 `max_steps` 必须**报错**，两种实参类型都要。
///
/// 修前：`-1.0` 静默变 0（不跑）、`Int(-1)` 回绕成 1.8e19（挂死），
/// 两者都 **exit 0、零诊断**。
#[test]
fn d339_negative_max_steps_is_rejected_for_both_numeric_types() {
    for (label, expr) in [
        ("Float -1.0", "tea.run(m, -1.0)"),
        ("Int -1", "tea.run(m, json.parse(\"-1\"))"),
        ("Float -0.5", "tea.run(m, -0.5)"),
    ] {
        let (code, got) = ev(&format!("{PRELUDE}print({expr})\n"));
        assert_eq!(
            code, 1,
            "[{label}] `{expr}` 负数步数应**报错**; 实得 exit={code} out={got}\n\
             修前：`-1.0f64 as usize` **饱和成 0** ⇒ 静默不跑；\
             `-1i64 as usize` **回绕成 1.8e19** ⇒ 队列非空时挂死。两者都 exit 0、零诊断"
        );
        assert!(
            got.contains("non-negative"),
            "[{label}] 错误应点明非负要求（与 `exec.parallel` 的 max_concurrent 同措辞）; 实得: {got}"
        );
    }
}

/// **转换本身的数值**：把「为什么两种类型不同」钉成事实。
///
/// 这条是主断言的**依据** —— 若哪天 Rust 改了 `as` 的饱和/回绕语义，
/// 本条会先于主断言变红，提示「缺陷的前提变了」。
#[test]
fn d339_bare_as_truncation_differs_between_float_and_int() {
    assert_eq!((-1.0f64) as usize, 0, "f64 → usize 是**饱和**");
    assert_eq!(
        (-1i64) as usize,
        18_446_744_073_709_551_615usize,
        "i64 → usize 是**回绕**（这正是缺陷的前提）"
    );
    assert_eq!(
        (2.9f64) as usize,
        2,
        "正小数向零截断（D246 收口的既定约定）"
    );
}

/// **对照组 1**：合法 `max_steps` 全部照常工作 —— 本条不能把 `tea.run` 弄坏。
#[test]
fn d339_valid_max_steps_still_works() {
    for e in [
        "tea.run(m, 5)",
        "tea.run(m, 0)",
        // 正小数按 D246 收口向零截断后放行
        "tea.run(m, 0.5)",
        "tea.run(m, 1.5)",
        // 缺参 ⇒ 默认上限 1000
        "tea.run(m)",
        // 显式 nil 与缺参同义
        "tea.run(m, nil)",
    ] {
        let (code, got) = ev(&format!("{PRELUDE}print(tea.model({e}))\n"));
        assert_eq!(
            code, 0,
            "`{e}` 应成功（合法步数 / 缺参 / nil）; 实得 exit={code} out={got}"
        );
    }
}

/// **对照组 2**：`tea.*` 其余入口的既有契约不变。
#[test]
fn d339_other_tea_contracts_unchanged() {
    // init 的 0-3 参**全部可选**（`args.first()` / `get(1)` / `get(2)` 都 `unwrap_or(Nil)`）
    for e in ["tea.init()", "tea.init(1)", "tea.init(\"m\")"] {
        let (code, got) = ev(&format!("print({e})\n"));
        assert_eq!(
            code, 0,
            "`{e}` 应成功（三参全可选 ⇒ min_arity=0）; 实得 exit={code} out={got}"
        );
        assert_eq!(got, "<tea_app>", "`{e}` 应得 tea_app; 实得 {got}");
    }
    // dispatch / update 的 2 参由 typeck 拦（运行期消息本身也点明）
    for e in ["tea.dispatch(a)", "tea.update(a)"] {
        let (code, got) = ev(&format!("let a = tea.init(1)\nprint({e})\n"));
        assert_eq!(
            code, 2,
            "`{e}` 少参应被 typeck 拒; 实得 exit={code} out={got}"
        );
    }
    // 非 TeaApp 实参必须被运行期拒
    for e in [
        "tea.model(1)",
        "tea.view(1)",
        "tea.run(1)",
        "tea.dispatch(1, \"m\")",
    ] {
        let (code, got) = ev(&format!("print({e})\n"));
        assert_eq!(
            code, 1,
            "`{e}` 传非 TeaApp 应报错; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("must be TeaApp"),
            "`{e}` 的错误应点明第一参必须是 TeaApp; 实得: {got}"
        );
    }
    // Msg 必须是 Dict
    for e in ["tea.dispatch(a, \"inc\")", "tea.update(a, [1,2])"] {
        let (code, got) = ev(&format!("let a = tea.init(1)\nprint({e})\n"));
        assert_eq!(
            code, 1,
            "`{e}` 的 Msg 非 Dict 应报错; 实得 exit={code} out={got}"
        );
        assert!(
            got.contains("expected Dict"),
            "`{e}` 应说明 Msg 需 Dict; 实得: {got}"
        );
    }
    // 类型名常量
    let (code, got) = ev("print(tea.model_type())\nprint(tea.msg_type())\n");
    assert_eq!(code, 0, "类型名常量应可取; 实得 exit={code} out={got}");
    assert_eq!(got, "Model | Msg", "类型名常量现状; 实得 {got}");
}

/// **`ccr.marker` 的负尺寸**（本条**不修**，只钉现状 + 报告）。
///
/// 与 `tea.run` 的关键差别：两种数值类型**都**饱和成 0 ⇒ **无类型分歧**。
/// 改它属产品契约决定（负尺寸该报错还是当 0），只报告。
#[test]
fn d339_ccr_marker_negative_size_still_becomes_zero_for_both_types() {
    let interp = Interpreter::new();
    for (label, v) in [
        ("Float -1.0", mora::value::Value::Float(-1.0)),
        ("Int -1", mora::value::Value::Int(-1)),
    ] {
        let got = interp
            .call_ccr_method("marker", &[mora::value::Value::String("h".into()), v])
            .unwrap_or_else(|e| panic!("ccr.marker({label}) 现状**不报错**; 实得 Err({e})"));
        assert_eq!(
            format!("{got}"),
            "<<ccr:h,0>>",
            "[{label}] 现状是负尺寸变 0（**两种类型一致** ⇒ 无 D339 那种类型分歧）; 实得 {got}\n\
             若此条红，说明有人给它加了负数守卫 —— 那是**有意的**语义变更，\
             请同步更新 D339 报告里的「未修」一节"
        );
    }
}

/// **`schedule.add` / `mora.refine` 的负数**已被**下游检查**拦下（与实参类型无关）。
///
/// 这两条**不需要**修 —— 但必须钉住，否则将来有人以为「它们没守卫」而去重复加固。
#[test]
fn d339_schedule_and_refine_already_reject_negatives_regardless_of_type() {
    let mut interp = Interpreter::new();
    // schedule.add：两个数值实参**都**被 `> 0` 拦下
    for (label, v) in [
        ("Float -1.0", mora::value::Value::Float(-1.0)),
        ("Int -1", mora::value::Value::Int(-1)),
    ] {
        let r = interp.call_schedule_method(
            "add",
            &[
                mora::value::Value::String("j".into()),
                mora::value::Value::String("every".into()),
                mora::value::Value::String("m".into()),
                v,
            ],
        );
        let err = r.expect_err(&format!("schedule.add interval_s={label} 应被拦"));
        assert!(
            err.contains("interval_s > 0"),
            "[{label}] 应报 `interval_s > 0`; 实得: {err}"
        );
    }
    // mora.refine：count **都**被 `1..=26` 拦下
    for (label, v) in [
        ("Float -1.0", mora::value::Value::Float(-1.0)),
        ("Int -1", mora::value::Value::Int(-1)),
    ] {
        let r = interp.call_mora_method(
            "refine",
            &[
                mora::value::Value::String("nonexistent_d339.mora".into()),
                mora::value::Value::String("instr".into()),
                v,
            ],
        );
        let err = r.expect_err(&format!("mora.refine count={label} 应被拦"));
        assert!(
            err.contains("1..=26"),
            "[{label}] 应报 count 取值范围; 实得: {err}"
        );
    }
}
