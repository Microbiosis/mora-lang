//! v0.104.6 D79：裸内建签名的**普查收尾** —— 5 条返回类型确定的内建补登记。
//!
//! ## 背景
//!
//! D78 补了 6 个 spec §12 内建后，对 **38 个运行期可调用的裸内建**（名单的唯一
//! 事实源是 `interpreter/dispatch.rs::call_function` 的分派表）逐个做了实证普查：
//! **故意写一个错的标注，看 typeck 是否拒绝**。
//!
//! - 标注被拒 → 有签名
//! - 标注被接受 → 结果类型是未解算的 `TypeVar`，标注形同虚设
//!
//! ## 本轮补的 5 条（返回类型逐条核对自 `builtin_impls.rs` 的 `Ok(Value::…)`）
//!
//! | 内建 | 运行期 | 声明 |
//! |---|---|---|
//! | `type_of(x)` | `Ok(Value::String(value_type_name(x)))` | `String` |
//! | `atom(x)` | `Ok(Value::Atom(…))`，`Type::Atom` 存在 | `Atom` |
//! | `methods_of(x)` | `Ok(Value::List(…map(Value::String)))` | `List[String]` |
//! | `gensym()` | `Ok(Value::String(format!("g{n}")))` | `String` |
//! | `is_instance(x, "T")` | `Ok(Value::Bool(… == type_name))` | `Bool` |
//!
//! ## 普查结论里「仍无签名」的那批 —— **如实无法声明**，不是漏登记
//!
//! - `car` / `cdr` / `uncurry`（D78 已登记，但结果是 fresh var）、`deref`
//!   —— 结果由容器元素 / 被调函数 / 原子内容决定。`deref` 尤其无解：
//!   **`Type::Atom` 是单元变体、没有载荷**，类型域里没有位置放「这个原子装什么」。
//! - `read` / `quote` → 运行期产出 `Value::Code`，而 **`Type` 没有 `Code` 变体**
//!   （扩 `Type` 枚举是 v1.0 方向的设计决定，不是缺陷修复该做的事）。
//! - `eval` —— 结果是被求值表达式的类型，需要真正的递归推断。
//! - `swap` / `into` / `macroexpand` / `batch_chat` —— 结果由回调 / 元素类型 /
//!   宏定义决定，同样只能宽松。
//!
//! ⚠ **诚实标注**：`deref` / `into` / `macroexpand` / `swap` / `batch_chat`
//! 这 5 个的普查判定是**不确定的** —— 我第一轮的探针调用写错了实参（运行期就
//! 报错），所以既没测到「有签名」也没测到「无签名」。它们被列在这里只是因为
//! **我还没查过**，不是「已确认无签名」。

/// **只跑类型检查**，不执行。
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

/// 5 条精确声明：错误标注被拒、正确标注通过。
#[test]
fn census_builtins_return_types_are_enforced() {
    // (名, 调用, 正确标注, 错误标注)
    // ⚠ 正例只能用**parser 白名单里的**标注名 —— `Atom` 是 typeck 内部的
    // `Type` 变体名，但**不是**合法的源语言类型标注（写 `let v: Atom` 会在
    // 解析期就被拒，测不到本轮要测的东西）。故 `atom` 一行用 `any` 作正例。
    let cases = [
        ("type_of", "type_of(1)", "String", "Bool"),
        ("atom", "atom(1)", "any", "Bool"),
        ("methods_of", "methods_of([1])", "list<string>", "Bool"),
        ("gensym", "gensym()", "String", "Bool"),
        ("is_instance", "is_instance(1, \"int\")", "bool", "Float"),
    ];
    for (name, call, good, bad) in cases {
        let ok = format!("let v: {good} = {call}\nprint(1)\n");
        assert!(
            typeck(&ok).is_ok(),
            "[{name}] 必须接受 `{good}` 标注\n  src={ok:?}"
        );
        let err = format!("let v: {bad} = {call}\nprint(1)\n");
        assert!(
            typeck(&err).is_err(),
            "[{name}] 配 `{bad}` 标注必须被拒 —— 若通过了，说明它仍无 typeck 签名、\
             结果退化成了 TypeVar\n  src={err:?}"
        );
    }
}

/// `gensym()` 运行期**显式拒绝任何实参**（`if !args.is_empty() { return Err(…) }`），
/// 声明成零参与运行期一致。
#[test]
fn gensym_takes_no_arguments() {
    assert!(
        typeck("let v: String = gensym()\nprint(1)\n").is_ok(),
        "`gensym()` 零参必须放行"
    );
}

/// 本轮只补返回类型、**不加实参约束**（D72 纪律）——
/// 传错类型的实参不应因此被拒。
#[test]
fn census_signatures_do_not_tighten_argument_types() {
    for src in [
        "let v: String = type_of({a: 1})\nprint(1)\n",
        "let v: any = atom({a: 1})\nprint(1)\n",
        "let v: list<string> = methods_of({a: 1})\nprint(1)\n",
        "let v: bool = is_instance(1, 2)\nprint(1)\n",
    ] {
        assert!(
            typeck(src).is_ok(),
            "只补返回类型、不收紧实参 —— 该写法不应被拒\n  src={src:?}"
        );
    }
}

/// 对照组：D68–D78 已登记的内建不得被本轮改动波及。
#[test]
fn previously_registered_builtins_unchanged() {
    for (src, why) in [
        ("let v: String = str(45)\nprint(1)\n", "str 返 String"),
        ("let v: Int = len(\"ab\")\nprint(1)\n", "len 返 Int"),
        ("let v: nil = print(1)\nprint(1)\n", "print 返 Nil"),
        (
            "let v: String = compress(\"a\", \"head_tail\")\nprint(1)\n",
            "compress 返 String",
        ),
    ] {
        assert!(
            typeck(src).is_ok(),
            "{why}，该写法必须照常通过\n  src={src:?}"
        );
    }
}
