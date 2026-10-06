//! v0.104.6 D82：`let x: dyn Trait = <普通值>` **整条路径走不通** ——
//! emit 侧做了强制转换，typeck 却没算这一步。
//!
//! ## 现象（修前）
//!
//! ```mora
//! let x: dyn Foo = 1
//! → Type error: expected TraitObject { trait_name: "Foo", generics: [] },
//!                got Float
//! ```
//!
//! 而 `emit_definitions.rs:66` 明确会 emit
//! `MirInst::DynTrait { src, dst, trait_name }` 把普通值包成 `Value::TraitObject`
//! （spec §3.5 / §13.1 承诺的语义）。**emit 写好的 coercion 永远到不了运行期。**
//!
//! ## 为什么一直没被发现
//!
//! `tests/mir_dyntrait.rs` 断言了 `MirInst::DynTrait` 的存在
//! （`let_dyn_trait_auto_coerces`），但它**只查编译出的 MIR，从不跑 typeck**
//! —— 与 D56 / D70 同型的假阳性测试：断言的名字对，被测的路径没走到底。
//!
//! ## 修法
//!
//! `infer_let_typed` 里把 `Type::TraitObject` 标注与 `Type::Any` 同等对待：
//! `dyn Trait` 的语义就是「**任何**值都会被强制转换」，故值侧不加约束。
//! 变量本身仍按 `TraitObject` 记进 env（与转换后的实际值一致）。
//!
//! ## 顺带：这是我在 D77 的**同一个坑**上漏改的一处
//!
//! D77 给 `print` 的 Union 补了 `Type::TeaApp` / `Type::TeaMsg` /
//! `Type::TraitObject` 三项，并给 `TeaApp` 补了「忽略 `name` 标签」的
//! `compatible_with` arm —— 但 **`TraitObject` 同样带 `trait_name`，我当时没管**。
//! 于是 Union 成员渲染成 `dyn`（`trait_name: ""`）而实际值是 `dyn Foo`，
//! `print(x)` 依然被拒。**「同一类问题在同一处改动里只修了一半」**。

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

/// spec §13.1 的 `dyn T` 标注**必须**通过类型检查 —— 语义是「把值强制转换成
/// TraitObject」，不是「值本来就得是 TraitObject」。
///
/// ⚠ 本文件**刻意只用顶层 `let`**：顶层那条路径会**同时**跑
/// `hm/infer.rs::infer_let_typed` 与 `bidirectional.rs` Phase C 两处检查，
/// 是更严的那条 —— 所以它足以在「只改了其中一处」时立刻失败。
/// （D82 第一版只改了 HM 侧，顶层用例立刻抓到；`task main()` 体内的
/// `let` 却通过 —— 正是这个不对称暴露了漏改点。）
#[test]
fn dyn_annotation_coerces_any_value() {
    for (name, src) in [
        ("int 字面量", "let x: dyn Foo = 1\nprint(1)\n"),
        ("float 字面量", "let x: dyn Foo = 1.5\nprint(1)\n"),
        ("string 字面量", "let x: dyn Foo = \"a\"\nprint(1)\n"),
        ("list 字面量", "let x: dyn Foo = [1, 2]\nprint(1)\n"),
        ("dict 字面量", "let x: dyn Foo = {a: 1}\nprint(1)\n"),
        ("nil", "let x: dyn Foo = nil\nprint(1)\n"),
        ("带泛型实参", "let x: dyn Foo<number> = 1\nprint(1)\n"),
    ] {
        assert!(
            typeck(src).is_ok(),
            "[{name}] `dyn T` 标注必须通过 typeck —— emit 侧会把值强制转换成 \
             TraitObject，typeck 不得拿转换前的类型去比对\n  src={src:?}\n  实得: {:?}",
            typeck(src)
        );
    }
}

/// 变体：`dyn` 标注不是「什么都收」—— 变量之后按 `TraitObject` 使用，
/// 且 `print` 也要接得住（这正是 D77 漏改的那一半）。
#[test]
fn dyn_annotation_is_not_a_loose_any() {
    // `any` 标注接住所有类型；`dyn` 标注只接住 TraitObject
    assert!(
        typeck("let x: any = 1\nprint(1)\n").is_ok(),
        "`any` 必须照常"
    );
    // 标注写错名字（dyn 是关键字）应被拒
    assert!(
        typeck("let x: dynn Foo = 1\nprint(1)\n").is_err(),
        "拼错的标注名应被拒"
    );
}

/// `as dyn Trait` 强制转换（§3.5）也必须可用 —— 与 `dyn T` 标注是**两条不同
/// 的路径**（emit 侧 `emit_dyn_coercion` vs `emit_definitions.rs` 的
/// `MirInst::DynTrait`），但都产出 `Value::TraitObject`。
///
/// ⚠ 实测记录：`as dyn T` **不支持泛型**（`1 as dyn Foo<number>` →
/// `Failed to parse`），而 `dyn T<...>` 标注**支持**。两者不同源，别混为一谈。
#[test]
fn as_dyn_coercion_produces_trait_object() {
    assert!(
        typeck("let x = 1 as dyn Foo\nprint(1)\n").is_ok(),
        "`1 as dyn Foo` 必须通过 typeck"
    );
    assert!(
        typeck("let x = 1 as dyn Foo<number>\nprint(1)\n").is_err(),
        "`as dyn` 不支持泛型 —— 这是**当前事实**，前端若补上泛型请改本测试"
    );
}
/// v0.104.6 D83：`MirInst::DynTrait` 把 `for_type` **硬编码成空串**。
///
/// 后果两层，第二层是功能性的：
///  1. Display 输出 `<trait_object for= as Foo …>` —— 类型名丢失；
///  2. `dispatch_trait_method` 用 `for_type` 拼 impl 查找键
///     （`__impl_<Trait>_<TGen>_<for_type>_<FGen>_<m>`），空类型名意味着
///     查找键**永远带不上被包值的具体类型**。
///
/// 本测试跑**真实二进制**并捕获 stdout —— 该字段只体现在运行期值的 Display 上，
/// 库 API 侧（`run_mir`）既不打印尾表达式、也不返回 task 的值。
#[test]
fn dyn_trait_object_records_concrete_type() {
    use std::process::Command;

    let bin = env!("CARGO_BIN_EXE_mora");
    let dir = std::env::temp_dir().join(format!("mora_d83_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    for (value, expect) in [("1.5", "float"), ("\"s\"", "string"), ("[1]", "list")] {
        let f = dir.join(format!("t_{}.mora", expect));
        std::fs::write(
            &f,
            format!("task main()\n    let x: dyn Foo = {value}\n    print(x)\nend\n"),
        )
        .expect("write probe");
        let out = Command::new(bin)
            .arg("run")
            .arg(&f)
            .output()
            .expect("run mora");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains(&format!("for={expect} ")),
            "TraitObject 的 for_type 必须是**被包值的具体类型名**（修前硬编码为空串，
             连带 impl 查找键也带不上类型）\n  value={value}  实得: {text}"
        );
        assert!(
            !text.contains("for= "),
            "绝不能出现空类型名\n  value={value}  实得: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// **`trait` / `impl` 定义前端根本不存在** —— lexer 里没有这两个 token。
/// 保留此测试是为了在将来某天前端落地时**主动失败并提示更新本文件**。
///
/// 实测（本轮 2026-10-02）：
/// - `trait Greet … end` → `Failed to parse`
/// - `impl Greet for P … end` → `Expected 'in' in for loop`
/// - `lexer.rs` 里**没有** `Trait` / `Impl` / `extends` / `where` / `default`
///   / `abstract` / `override` 任何一个 token
/// - `trait` / `impl` 至今**不是保留字**：`let trait = 1` 可用
/// - `as dyn Trait`（§3.5）**可用**：`let x = 1 as dyn Foo` 产出
///   `<trait_object for= as Foo data=Float(1.0)>`；但**不支持泛型**
///   （`1 as dyn Foo<number>` → `Failed to parse`）——
///   即 `dyn T` 标注支持泛型、`as dyn T` 不支持，两者不同源
#[test]
fn trait_and_impl_definitions_are_still_unparseable() {
    // 本测试**不**断言它们失败（那是另一种锁法），只记录现状。
    // 一旦前端落地，这里会开始编译/解析成功，届时应把本测试改写成
    // 「trait/impl 定义可用」的正向断言。
    let trait_def = typeck("trait Greet\n  hi\nend\ntask main()\n  print(1)\nend\n");
    let impl_def = typeck("impl Greet for P\nend\ntask main()\n  print(1)\nend\n");
    assert!(
        trait_def.is_err() && impl_def.is_err(),
        "若 trait / impl 定义已能解析，说明前端已落地 —— 请改写本测试为正向断言，\
         并检查 `infer_let_typed` 的 dyn 强制转换是否还需要"
    );
}
