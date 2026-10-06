//! v0.104.6 D274：**宏体不参与 typeck** —— 类型错与未绑定变量双双静默（**未修**，待裁决）
//!
//! ## ⚠ 本文件混合两类断言
//!
//! - `d274_*`（**现状判据**）断言**当前的错误行为**：宏体零诊断。
//!   修好之后它们**会变红** —— 那时请把 `expect_checked` 翻成 `true`。
//! - `d274_runtime_*`（**正向断言**）断言**运行时契约**（宏的作用域），
//!   修前修后**都应保持绿** —— 它们的作用是**防止将来的修复误伤**：
//!   若让宏体进入 typeck 的同时把作用域收窄，这些会立刻变红。
//!
//! ## 缺陷：宏体整支跳过
//!
//! 普查 HM 里全部 9 处 `Ok((Type::Nil, EffectRow::Empty))` 桩后，
//! `MacroDef { .. }` 是**唯一一处既「有 emit 路径（活分支）」又「有子表达式」**的：
//!
//! | 桩位置 | 覆盖的 kind | 判定 |
//! |---|---|---|
//! | `hm/mod.rs:1015` | `Return(None)` / `Break` / `Continue` | ✅ 正确（无子表达式） |
//! | **`hm/mod.rs:1018`** | `IndexAssign { .. }` | ⚪ **死分支** —— `emit.rs` 无构造点 |
//! | **`hm/mod.rs:1030`** | `TypeAlias`/`EnumDef`/`StructDef`/`Import`/`MacroDef` | 见下 |
//! | `hm/mod.rs:1030` 里的 `TypeAlias`/`EnumDef`/`StructDef`/`Import` | 纯类型声明 | ⚪ **死分支** —— `emit.rs` 无构造点 |
//! | **`hm/mod.rs:1030` 里的 `MacroDef`** | `name`/`params`/`body` | ❗ **活分支且有子体** |
//! | `hm/mod.rs:1064` | `TaskDef` | ✅ 实际**已推断**体（Nil 只表示声明无标量类型） |
//! | `hm/mod.rs:1133` | `AppDef`（tea） | ✅ 实际**已推断** init/update/view |
//! | `hm/mod.rs:1154` | `MsgDef` | ✅ 正确（只有类型名，无表达式） |
//! | `hm/mod.rs:1178` | `EffectSig` | ✅ 正确（纯类型层） |
//!
//! `MirWitness::children()` **早已枚举** `MacroDef => vec![body]` ——
//! 又是「枚举器有、typeck 不用」的同一形态（与 D270/D271/D273 一致）。
//!
//! ## 用户可见后果：静默错值（真实 CLI）
//!
//! | 程序 | 顶层等价 | 宏内 |
//! |---|---|---|
//! | `1 + "str"` | `Type error` exit 2 | **`1.0str`**，exit **0** ❗ |
//! | `"v:" + nosuchvar` | `Type error: Unbound` exit 2 | **`v:nil`**，exit **0** ❗ |
//!
//! 与 D273 的 orchestrate 缺口**完全同型**，只是入口不同（宏 vs orchestrate）。
//!
//! ## 修法（已实测出全部前提，但**不擅自实施**）
//!
//! 现成范式就在同一文件里：`hm/mod.rs:884-886` 的 `FnDef` 走
//! `infer_fn_def(Some(name), params, body, span)`，而 `MacroDef` 的字段
//! **形状完全相同**（`name` / `params` / `body`）。宏体是语句序列也没问题：
//! `WitnessKind::Sequence` 有 `infer_sequence`（do-notation 语义，返回最后
//! 一个表达式），正是宏体该有的行为。
//!
//! ⇒ 修法 = 把 `MacroDef` 移出 Nil 桩组，路由到 `infer_fn_def`；
//! `bidirectional.rs:477` 同样要加一支（照 194-199 登记形参、534-536 预扫体）。
//!
//! **但有一个必须先补的前置条件**：实测宏**支持前向引用** ——
//!
//! ```mora
//! macro outer()
//! inner()          -- inner 定义在后面
//! end
//! macro inner()
//!   "I"
//! end
//! print(outer())   -- → I，exit 0
//! ```
//!
//! 而 `precompute_fn_arities`（防 fn/task 前向引用的那遍）**只收 `FnDef`**。
//! 若不加宏的预登记就推断体，`outer` 的体会在 `inner` 登记前被推断 ⇒
//! **误报 `Unbound variable inner`**，打破今天能跑的程序。
//!
//! ## 为什么不擅自实施
//!
//! 修法需要**新增一个预登记遍**（`bidirectional.rs` 也要平行改一支，
//! 且 `MacroDef.params` 是 `Vec<String>` 而 `FnDef.params` 是
//! `Vec<WitnessParam>`，登记代码不同）。而仓库里**零个 `.mora` 文件使用宏**
//! ⇒ 一旦引入假阳性，**没有任何真实用例能立刻暴露它**。
//! 这正是 D273 里拒绝直接遍历 `sub_witnesses()` 的同一条理由：
//! **假阳性比假阴性更糟。**

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn typeck_error_count(src: &str) -> usize {
    let (_f, w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile 应成功: {e}"));
    mora::typeck::check_mir::check_program_witnesses(&w).len()
}

fn run(source: &str) -> Result<Value, String> {
    let (func, _w) = ParserV3::compile(source).map_err(|e| format!("compile: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
}

fn printed(source: &str) -> String {
    match run(source) {
        Ok(Value::String(s)) => s,
        Ok(v) => format!("{v:?}"),
        Err(e) => panic!("应跑通，实际：{e}"),
    }
}

/// **现状判据**主断言：宏体里的类型不匹配零诊断。
///
/// 修好之后本条会红（`expect_checked` 翻成 `true`）。
#[test]
fn d274_macro_body_type_mismatch_is_not_caught() {
    let n = typeck_error_count("macro m()\n  1 + \"str\"\nend\n1\n");
    assert_eq!(
        n, 0,
        "现状：宏体里的 `1 + \"str\"` 零诊断（应检出）。\
         若本条失败说明宏体已接上 typeck —— 把本文件两处 expect 翻成 true 即可"
    );
}

/// **现状判据**：宏体里的未绑定变量零诊断。
#[test]
fn d274_macro_body_unbound_var_is_not_caught() {
    let n = typeck_error_count("macro m()\n  let z = nosuchvar\nend\n1\n");
    assert_eq!(n, 0, "现状：宏体里引用未绑定变量零诊断（应检出 Unbound）");
}

/// **对照组**：顶层与闭包体**必须**检出同一处错误。
///
/// 这两条保证上面那两条的「0 诊断」不是因为探测器没在工作。
#[test]
fn d274_top_level_and_closure_are_still_checked() {
    assert!(
        typeck_error_count("let z = 1 + \"str\"\nz\n") > 0,
        "顶层类型不匹配必须被检出 —— 若这条也失败，说明问题不在宏"
    );
    assert!(
        typeck_error_count("let f = fn(x) { y + 1 }\nf(1)\n") > 0,
        "闭包体内未绑定变量必须被检出"
    );
}

/// **运行时契约（正向，修前修后都应绿）**：宏体看得见**外层变量**。
///
/// 这条防的是「为了让宏体进入 typeck 而把作用域收窄」的错误修法。
#[test]
fn d274_runtime_macro_body_sees_outer_variables() {
    assert_eq!(
        printed("let g = \"G\"\nmacro m()\n  g\nend\nm()\n"),
        "G",
        "宏体在运行时看得见外层变量（`env.clone()` 语义）——\
         任何 typeck 修复都不得把它收窄成「只看得见形参」"
    );
}

/// **运行时契约（正向）**：宏的**形参被正确绑定**。
#[test]
fn d274_runtime_macro_params_are_bound() {
    // 注意宏的返回值是 **Float**（`1 + 2` 走数值塔），
    // CLI 的 `print` 把它显示成 `3.0`，但 `Value` 本身是 `Value::Float(3.0)`。
    match run("macro add(a, b)\n  a + b\nend\nadd(1, 2)\n").expect("应跑通") {
        Value::Float(f) => assert!(
            (f - 3.0).abs() < 1e-9,
            "宏形参应被实参绑定 ⇒ 1 + 2 = 3.0，实际：{f}"
        ),
        other => panic!("期望数值结果，实际：{other:?}"),
    }
}

/// **运行时契约（正向）**：宏**支持前向引用**。
///
/// 这条是本文件存在的**主要理由**：修法必须先补宏的预登记遍，
/// 否则 `outer` 的体会在 `inner` 登记前被推断 ⇒ 误报 Unbound、
/// **打破今天能跑的程序**。本条会在那种错误修法下变红。
#[test]
fn d274_runtime_macro_supports_forward_reference() {
    assert_eq!(
        printed("macro outer()\n  inner()\nend\nmacro inner()\n  \"I\"\nend\nouter()\n"),
        "I",
        "宏体可调用**后定义**的宏 —— typeck 修复必须先做预登记，否则会误报 Unbound"
    );
}
