//! v0.104.6 D78：6 个 spec §12 承诺的内建在 typeck 侧**完全没有登记**。
//!
//! ## 现象（修前）
//!
//! `compose` / `partial` / `curry` / `apply` / `car` / `cdr` / `uncurry`
//! 在 `builtin_signatures()` 与 `builtin_callee_ty()` 里**一条都没有**，
//! 落到 `unwrap_or_else(|| self.fresh_type_var())` → 结果类型恒为**永不解算的
//! `TypeVar`**，标注形同虚设：
//!
//! ```mora
//! let v: String = compose(f, g)   → 修前被接受  ❌（实得 Value::Compose）
//! let v: String = partial(f, 1)   → 修前被接受  ❌（实得 Value::Partial）
//! ```
//!
//! 之所以一直没被撞见，是因为 `print` 的形参 Union **含 `Any`** —— TypeVar 能
//! 装进任何成员，于是「打不出来」这个症状被掩盖了；只有**显式标注**才暴露。
//! （与 D62 / D67 / D68 同源：TypeVar 对任何类型都兼容。）
//!
//! ## 修法
//!
//! - `compose` → `Type::Compose`、`partial` → `Type::Partial`：**两个 `Type`
//!   变体本就存在**（值域侧也确有 `Value::Compose` / `Value::Partial`），可精确声明。
//! - `curry`：缺 `Type::Curry` 变体（扩 `Type` 枚举是 v1.0 方向的设计决定），
//!   故给 `Any` —— 如实反映「值域侧存在、类型域侧没有对应物」。
//! - `apply` / `car` / `cdr` / `uncurry`：结果由被调函数 / 容器元素决定，
//!   运行期不固定 → 各自 mint 一个 fresh var。
//!
//! 参形一律 `Any`、**不限上界**（`variadic_arrow` 额外挂两层 slack），与 D72
//! 的「只校验下限、不收紧上限」同源 —— 少一处就又是一次 D72 / D76 式的回归。

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

const F: &str = "fn(x) x + 1 end";

/// `compose` / `partial` 的结果类型是**具体**的（`Compose` / `Partial`），
/// 错标注必须被拒。
#[test]
fn compose_and_partial_return_types_are_enforced() {
    for (name, call) in [
        ("compose", format!("compose({F}, {F})")),
        ("partial", format!("partial({F}, 1)")),
    ] {
        let bad = format!("let v: String = {call}\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[{name}] 返回类型是具体的，配 String 标注必须被拒 —— \
             若通过了，说明它仍无 typeck 签名、结果退化成了 TypeVar"
        );
        let ok = format!("let v: any = {call}\nprint(1)\n");
        assert!(typeck(&ok).is_ok(), "[{name}] 必须接受 any");
    }
}

/// **变参**：任意数量的实参都要放行。
///
/// ⚠ v0.104.6 D80 的回归钉子：D78/D79 曾用「curried arrow + 2 层 `Any` slack」
/// 声明 `compose` / `partial`，安全扫查出 `compose(1, 2, 3, 4)` 与
/// `partial(1, 2, 3, 4, 5)` 都会 exit 2 —— slack 用尽后，多传的实参被拿去和
/// **返回类型**比对。spec §12 写的是 `...closure -> compose` / `closure, ...any
/// -> partial`，**根本没有固定上界**，固定 slack 治不了。现改走
/// `Signature::variadic`（`infer_call` 的变参分支完全不设上界）。
///
/// 故本测试特意用**远超旧 slack（2 层）**的实参个数。
#[test]
fn compose_and_partial_accept_unbounded_arities() {
    for n in [1usize, 2, 3, 4, 6, 8] {
        let args: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        let joined = args.join(", ");
        for call in [format!("compose({joined})"), format!("partial({joined})")] {
            let src = format!("let v: any = {call}\nprint(1)\n");
            assert!(
                typeck(&src).is_ok(),
                "无上界的变参：传 {n} 个实参必须放行\n  src={src:?}"
            );
        }
    }
}

/// `gensym()` 运行期**显式拒绝任何实参**（`if !args.is_empty() { return Err(…) }`），
/// 声明成零参与运行期一致 —— 多传一个就该被 typeck 拒。
#[test]
fn gensym_rejects_extra_arguments() {
    assert!(
        typeck("let v: String = gensym()\nprint(1)\n").is_ok(),
        "`gensym()` 零参必须放行"
    );
    assert!(
        typeck("let v: String = gensym(1)\nprint(1)\n").is_err(),
        "`gensym(1)` 必须被拒 —— 运行期 `args.is_empty()` 检查同样会拒"
    );
}

/// `compose` / `partial` 的**结果类型**仍必须精确（改走变参表后不能丢）。
#[test]
fn compose_and_partial_still_have_precise_return_types() {
    assert!(
        typeck("let v: String = compose(f, f)\nprint(1)\n").is_err(),
        "`compose` 返回 Compose，配 String 标注必须被拒"
    );
    assert!(
        typeck("let v: String = partial(f, 1)\nprint(1)\n").is_err(),
        "`partial` 返回 Partial，配 String 标注必须被拒"
    );
}

/// `curry` / `uncurry` / `car` / `cdr` / `apply` 必须照常可用 —— 本次
/// 只补登记，不改变它们**能调用**这件事。
#[test]
fn curry_family_still_usable() {
    for src in [
        format!("let c = curry({F}, 1)\nlet v = c(1)\nprint(v)\n"),
        format!("let c = curry({F}, 1)\nlet v = uncurry(c)\nprint(v)\n"),
        "print(car(cons(1, nil)))\n".to_string(),
        "print(cdr(cons(1, nil)))\n".to_string(),
        format!("print(apply({F}, [1]))\n"),
    ] {
        assert!(typeck(&src).is_ok(), "必须照常可用\n  src={src:?}");
    }
}

/// 对照组：`cons` / `cons` 族的既有签名不得被本次改动波及。
#[test]
fn cons_family_unchanged() {
    assert!(
        typeck("let c: any = cons(1, nil)\nprint(1)\n").is_ok(),
        "`cons` 的既有签名不得被波及"
    );
    assert!(
        typeck("print(car(cons(1, nil)))\n").is_ok(),
        "`car(cons(...))` 不得被波及"
    );
}

/// **防「注册了签名就等于收紧了类型」**：本次只加返回类型、**不加实参约束**，
/// 所以传错类型的实参仍不应被判元数/类型错（与 D72 的下限纪律一致）。
#[test]
fn new_signatures_do_not_tighten_argument_types() {
    for src in [
        "let v: any = compose(1, 2)\nprint(1)\n".to_string(),
        "let v: any = partial(1, 2, 3)\nprint(1)\n".to_string(),
        "let v: any = curry(1, 2)\nprint(1)\n".to_string(),
    ] {
        assert!(
            typeck(&src).is_ok(),
            "本次只补返回类型、不收紧实参 —— 该写法不应被拒（会挡住原本能跑的代码）\n  src={src:?}"
        );
    }
}
