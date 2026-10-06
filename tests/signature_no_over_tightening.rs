//! v0.104.6 D81：把 D80 的「反向放宽」安全扫**固化成自动测试**。
//!
//! ## 为什么要固化
//!
//! D80 手工扫出 2 处**我自己引入的回归**（`compose` / `partial` 的 slack 用尽
//! 后把多余实参拿去和返回类型比对）。手工扫只能做一次 —— 明天新增签名就没人扫了。
//! 故把判据写成测试，让**每一次新增签名**都自动受检。
//!
//! ## 判据（D72 定、D80 复现三次的那条）
//!
//! 运行期普遍用 `args.first()` / `args.get(N)` **忽略多余实参**，所以补签名时
//! **只能收紧下限、不能收紧上限**。typeck 照「形参个数」封顶，就是把一个类型层
//! 盲区换成「原本合法的程序编译不过」。
//!
//! 故本文件对**20 个已登记模块**各取一个方法，构造「多传实参」的调用，
//! 断言 typeck **不**因此报错。
//!
//! 断言**只跑类型检查、不执行** —— 因为这些调用的实参在运行期本就无效
//! （`tool.create("p", 1, 2, 3)` 会报 `unknown kind '1.0'`），
//! 跑起来只会把「运行期错」误报成回归。D80 那次就有 1 条是这样被误报的。

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

/// **20 个已登记模块各一个方法，传多于声明数量的实参，必须放行。**
///
/// 覆盖 D69–D75 与 D85 登记的全部模块。`random` / `ai` / `agent` 不在内 ——
/// 它们有精确 `Type` 变体与专门分派路径，签名不在本表（见
/// `module_table_keys_are_registered_names` 的注释）。
#[test]
fn module_signatures_never_reject_extra_arguments() {
    // (方法调用, 人类可读标签)
    let calls = [
        ("math.sqrt(4.0, 5, 6)", "math"),
        ("json.stringify({a: 1}, 2, 3)", "json"),
        ("file.exists(\"a\", 1, 2)", "file"),
        ("exec.parallel([\"a\"], 2, 3, 4)", "exec"),
        ("stats.mean([1, 2], 3, 4)", "stats"),
        ("linalg.norm([1, 2], 3, 4, 5)", "linalg"),
        ("document.parse(\"a\", 1, 2)", "document"),
        ("mora.list_refines(1, 2, 3)", "mora"),
        ("bus.emit(\"e\", 1, 2, 3)", "bus"),
        ("mock.names(1, 2, 3)", "mock"),
        ("ccr.put(\"d\", 1, 2)", "ccr"),
        ("plan.list(1, 2, 3)", "plan"),
        ("tea.init(1, 2, 3, 4)", "tea"),
        ("tool.create(\"p\", 1, 2, 3)", "tool"),
        ("skill.load(\"p\", 1, 2)", "skill"),
        ("xform.map(1, 2, 3)", "xform"),
        ("schedule.count(1, 2, 3)", "schedule"),
        ("memory.size(1, 2, 3)", "memory"),
        ("sandbox.mode(1, 2, 3)", "sandbox"),
        // v0.104.6 D85：`web` 与 `file` / `math` 同款（落到 `Type::Unknown`）
        ("web.fetch(\"http://x\", 1, 2)", "web"),
    ];
    assert_eq!(
        calls.len(),
        20,
        "已登记模块的抽样数被改动 —— 与 `module_table_keys_are_registered_names` \
         的 REGISTERED（20 项）应对齐"
    );
    for (call, module) in calls {
        let src = format!("print({call})\n");
        assert!(
            typeck(&src).is_ok(),
            "[{module}] 多传实参**不得**被 typeck 拒 —— 运行期普遍忽略多余实参，\
             收紧上限就是把类型层盲区换成「原本合法的程序编译不过」\n  src={src:?}"
        );
    }
}

/// **零参内建的特例**：少数内建运行期**显式拒绝**多余实参
/// （`gensym` 的 `if !args.is_empty() { return Err(…) }`），
/// 这类被拒是**正确**的 —— 与「一律放行」不是同一条规则。
#[test]
fn zero_arg_builtins_still_reject_extra_arguments() {
    assert!(
        typeck("let v: String = gensym()\nprint(1)\n").is_ok(),
        "`gensym()` 零参必须放行"
    );
    assert!(
        typeck("let v: String = gensym(1)\nprint(1)\n").is_err(),
        "`gensym(1)` 必须被拒 —— 运行期同样会拒（`args.is_empty()` 检查）"
    );
}

/// `compose` / `partial` 是**无上界**变参（spec §12 只写 `...`）。
/// D80 曾用「2 层 slack」实现，被本测试的思路扫出回归；现在走
/// `Signature::variadic`，实参个数与声明无关。
#[test]
fn unbounded_variadics_accept_any_arity() {
    for n in [1usize, 3, 5, 8, 12] {
        let args: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        let joined = args.join(", ");
        for call in [format!("compose({joined})"), format!("partial({joined})")] {
            let src = format!("let v: any = {call}\nprint(1)\n");
            assert!(
                typeck(&src).is_ok(),
                "无上界变参：传 {n} 个实参必须放行\n  src={src:?}"
            );
        }
    }
}
