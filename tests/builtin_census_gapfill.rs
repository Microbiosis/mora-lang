//! v0.104.6 D79b：补上 D79 普查里**判定不确定**的 4 个内建。
//!
//! ## 为什么要补
//!
//! D79 的普查结论里留了一句诚实标注：`swap` / `into` / `macroexpand` /
//! `batch_chat` 的判定**不确定** —— 第一轮探针把实参写错了（运行期就报错），
//! 既没测到「有签名」也没测到「无签名」。
//!
//! 「我还没查过」不该留成尾巴。此处逐个读运行期实现补齐判据。
//!
//! | 内建 | 运行期（`builtin_impls.rs`） | 声明 |
//! |---|---|---|
//! | `batch_chat(list)` | 逐项 `do_ai_chat`，后者 `Ok(Value::String(…))` | `List[String]` |
//! | `into(list, fn)` | 逐项调 fn，命中 List 时 **extend（展平）** | `List[α]`（α fresh） |
//! | `macroexpand(n, …)` | 跑宏体的 MIR，结果即宏的返回值 | fresh（宏返回什么就是什么） |
//! | `swap(atom, fn)` | `Ok(new_val)`，new_val 是 fn 的返回值 | fresh |
//!
//! ## 两条只能收紧「容器」、不能收紧「元素」的经验
//!
//! - `into` 的元素类型是**回调的返回值**，静态不可知 → 只能声明 `List[α]`
//! - `macroexpand` 的实参个数由**宏定义的形参表**决定
//!   （`expr_args.len() != params.len()` 才报错），故必须**变参**，
//!   否则 `macroexpand("m1", [1,2,3])` 这类调用会被误判元数错。

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

/// 宏必须定义在**顶层**（不在 `task main()` 体内）—— 实测放在体内会
/// `Failed to parse at line 2`。
const MACRO_M1: &str = "macro m1(a, b)\n  a + b\nend\n";

/// `batch_chat` → `List[String]`：**容器**收得住。
#[test]
fn batch_chat_returns_list_of_string() {
    let ok = "let v: list<string> = batch_chat([\"a\", \"b\"])\nprint(1)\n";
    assert!(
        typeck(ok).is_ok(),
        "`batch_chat` 必须接受 list 标注\n  src={ok:?}"
    );
    for bad in ["String", "bool", "Float"] {
        let src = format!("let v: {bad} = batch_chat([\"a\"])\nprint(1)\n");
        assert!(
            typeck(&src).is_err(),
            "[batch_chat] 返回 List[String]，配 `{bad}` 标注必须被拒"
        );
    }
}

/// `into` → `List[α]`：同样收得住**容器**。
#[test]
fn into_returns_list() {
    let ok = "let v: list<number> = into([1, 2], fn(x) x * 2 end)\nprint(1)\n";
    assert!(
        typeck(ok).is_ok(),
        "`into` 必须接受 list 标注\n  src={ok:?}"
    );
    for bad in ["String", "bool"] {
        let src = format!("let v: {bad} = into([1], fn(x) x end)\nprint(1)\n");
        assert!(
            typeck(&src).is_err(),
            "[into] 返回 List，配 `{bad}` 标注必须被拒"
        );
    }
}

/// `macroexpand` 必须**变参** —— 实参个数由宏的形参表决定，多传不能被判元数错。
#[test]
fn macroexpand_is_variadic() {
    for args in ["[1, 2]", "[1, 2, 3]", "[]", "[7]"] {
        let src = format!("{MACRO_M1}let v: any = macroexpand(\"m1\", {args})\nprint(1)\n");
        assert!(
            typeck(&src).is_ok(),
            "`macroexpand` 的实参个数由宏定义决定，不该被判元数错\n  args={args}"
        );
    }
    // 零参调用也要放行（min = 1）
    let zero = format!("{MACRO_M1}let v: any = macroexpand()\nprint(1)\n");
    assert!(typeck(&zero).is_ok(), "`macroexpand()` 最小 0 参应放行");
}

/// `swap` / `macroexpand` 的元素类型只能宽松（回调返回值 / 宏返回值）——
/// 但**必须仍然可用**。这条钉住「不收紧」而不是「收紧」。
#[test]
fn unconstrained_builtins_remain_usable() {
    for (name, src) in [
        (
            "swap",
            "let a = atom(1)\nlet v = swap(a, fn(x) x + 1 end)\nprint(v)\n",
        ),
        (
            "macroexpand",
            "let v = macroexpand(\"m1\", [1, 2])\nprint(v)\n",
        ),
    ] {
        let wrapped = if name == "macroexpand" {
            format!("{MACRO_M1}{src}")
        } else {
            src.to_string()
        };
        assert!(
            typeck(&wrapped).is_ok(),
            "[{name}] 结果类型只能是 fresh var —— 但仍必须可用\n  src={wrapped:?}"
        );
    }
}
