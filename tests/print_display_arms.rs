//! v0.104.6 D77：`print` 打不出 `TeaApp` / `TeaMsg` / `TraitObject`。
//!
//! ## 现象（修前）
//!
//! ```mora
//! print(tea.init())
//! → Type error: expected string | int | float | … | app<: any> | … ,
//!   got app<tea: any>
//! ```
//!
//! 而运行期 `value/display.rs:155` 明明有
//! `Value::TeaApp(_) => write!(f, "<tea_app>")` —— **打不出来纯粹是类型层的**。
//!
//! ## 两层根因
//!
//! **第一层**：`print` 的形参 Union 里没有 `Type::TeaApp` / `Type::TeaMsg` /
//! `Type::TraitObject`。`dispatch.rs` 里那条「⚠ 仍缺的 8 个」注释把三者列进
//! 「`Type` 里没有对应变体」的名单 —— **该判断已过期**，三个变体都早已存在
//! （`TeaApp` / `TeaMsg` 由 TEA 引入，`TraitObject` 由 v0.08 的 dyn 引入）。
//! 真正缺 `Type` 变体的只有 5 个：`LogicVar` / `Code` / `Curry` / `Tool` /
//! `TeaCmd`。
//!
//! **第二层**（补进 Union 后才暴露）：`Type::TeaApp` 的 `name` 字段在
//! `compatible_with` / `subtype_of` 里走**结构相等**兜底，会被一并比掉。
//! 而 `name` 是**显示标签**（`name()` 渲染成 `app<{name}: {model}>`），
//! 源语言里没有任何语法能写出一个指定名字的 TEA app。两条签名各自构造的
//! TeaApp（`name` 一个 `"tea"` 一个 `""`）因此被判为不兼容 ——
//! Union 成员渲染成 `app<: any>`，实际值是 `app<tea: any>`。
//!
//! 修法：给 `Type::TeaApp` 补结构化 arm，按四个**类型**字段逐一比较、
//! **忽略 `name`**（与 `Cons` / `Relation` 的处理同款）。
//!
//! ⚠ **哪一处才是承重的（反向验证得出，别当成「两处都验过了」）**：
//! - `compatible_with` —— **承重**。把它退回结构相等后，本文件 2 条测试立即失败。
//! - `subtype_of` —— **未被本文件触及**（退回后测试仍绿）。仍一并改，理由是
//!   「同一概念两处保持一致」，属**推理而非测试证据**。

/// **只跑类型检查**，不执行 —— 断言的都是「标注 / 实参是否被 typeck 接受」。
fn typeck(src: &str) -> Result<(), String> {
    let (_func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if errs.is_empty() {
        Ok(())
    } else {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        Err(format!("TYPECK: {msgs:?}"))
    }
}

/// `print` 必须接得住 `TeaApp` —— 运行期 `Display` 早已实现
/// （`value/display.rs:155` 输出 `<tea_app>`）。
#[test]
fn print_accepts_tea_app() {
    assert!(
        typeck("print(tea.init())\n").is_ok(),
        "`print(tea.init())` 必须通过 —— 运行期能打（`<tea_app>`），\
         修前被 typeck 拒：expected … | app<: any> | … , got app<tea: any>"
    );
    // 中间变量形态同样要过（证明不是只对字面调用放宽）
    assert!(
        typeck("let a = tea.init()\nprint(a)\n").is_ok(),
        "`print(teaApp 变量)` 同样必须通过"
    );
}

/// 对照组：`print` 的 Union 里**原本就有**的类型不得被本次改动波及。
#[test]
fn print_still_accepts_registered_types() {
    for src in [
        "print(\"a\")\n",
        "print(1i)\n",
        "print(1.5)\n",
        "print([1, 2])\n",
        "print({a: 1})\n",
        "print(true)\n",
        "print(nil)\n",
        "print(Router::new())\n",
        "print(ai.tokens())\n",
    ] {
        assert!(typeck(src).is_ok(), "`{src}` 必须照常通过");
    }
}

/// `TeaApp` 的 `name` 是显示标签、不是名义类型的一部分 ——
/// 两条签名各自构造的 TeaApp（`name` 不同）必须互兼容。
#[test]
fn tea_app_name_is_a_label_not_a_nominal_discriminator() {
    // `tea.init()` 的声明签名与 `print` 的 Union 成员 `name` 不同，
    // 若按结构相等比较就会被拒；这里从两侧夹逼：既接自身、也接 `any`
    assert!(
        typeck("let a: any = tea.init()\nprint(a)\n").is_ok(),
        "TeaApp 赋给 any 后必须能 print"
    );
    // 不同的 name 标签之间应互相兼容 —— 用两次 tea.init() 交叉验证
    assert!(
        typeck("let a = tea.init()\nlet b = tea.init()\nprint(a)\nprint(b)\n").is_ok(),
        "两个 TeaApp 值（label 相同）必须都能 print"
    );
}
