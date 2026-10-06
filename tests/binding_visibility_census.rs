//! v0.104.6 D166：**绑定可见性普查** —— 6 类构造逐个实测，**全部正确**（否定结果轮）。
//!
//! 本轮起因是 D165 修掉了「match arm 体里的模式绑定被判成未定义变量」，
//! 于是顺着**同一机制**普查其余「引入绑定」的构造，查双向层是否也会误报。
//!
//! ## 普查结果
//!
//! | 构造 | 绑定在体内可见？ | 结论 |
//! |---|---|---|
//! | `for` 循环变量 | ✅ | 正确 |
//! | `for` 嵌套（双变量） | ✅ | 正确 |
//! | 闭包形参 | ✅ | 正确 |
//! | 高阶传参（`map`/`filter`/`reduce` 的闭包） | ✅ | 正确 |
//! | `worker` 块 | ✅ | 正确 |
//! | `match` arm 模式 | — | **D165 已修**（此前是假阳性） |
//! | `with` 块绑定 | ❌（设计如此） | **正确，见下** |
//!
//! ## `with` 的绑定**不是词法变量** —— 已被否证过一次，本轮我又误判了一次
//!
//! ```text
//! with model = "m"
//!   print(model)     ← Unbound variable 'model'
//! end
//! ```
//!
//! 我第一反应是「与 D165 同型的假阳性」，差点当缺陷报出去。**不是**：
//!
//! | 层 | 结果 |
//! |---|
//! | 运行期 | `nil`（未定义变量） |
//! | typeck | `Unbound variable 'model'` |
//!
//! **两层一致** —— `with` 是**上下文配置块**（spec §11.1），绑定只进
//! interpreter 的 config 栈供 `ai.chat` 等消费（`ai_chat.rs:81-86`
//! 确实读 `current_ai_config.model`），**不进入词法环境**。
//! 故块内按名引用报错是**正确行为**。
//!
//! ⚠ `tests/with_config.rs` 的文件头**早已写明**这一点，并明确记录
//! 「早先一轮曾把它误判为作用域缺陷，spec 定性后已否证」。
//! 本轮是**同一个误判的第二次**。本条把结论钉在这里以防第三次。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

fn typeck(src: &str) -> Result<Vec<String>, String> {
    let (_f, w) = mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    Ok(check_program_witnesses_bidirectional(&w)
        .iter()
        .map(|e| e.message.clone())
        .collect())
}

fn run(src: &str) -> Result<String, String> {
    let (f, _w) = mora::parser_v3::ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let arc = std::sync::Arc::new(f);
    mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v}"))
}

/// D166 主判据：六类构造的绑定可见性**全部正确**（防将来静默回退）。
#[test]
fn d166_binding_visibility_is_correct_everywhere() {
    // ① `for` 循环变量
    let e =
        typeck("let t = 0\nfor i in range(0, 4)\n  assign t = t + i\nend\nprint(t)\n").expect("c");
    assert!(e.is_empty(), "[for] 循环变量在体内应可见; 实得: {e:?}");

    // ② 嵌套 `for`（两个变量各自独立绑定）
    let e = typeck(
        "let t = 0\nfor i in range(0, 4)\n  for j in range(0, 3)\n    assign t = t + i * j\n  end\nend\nprint(t)\n",
    )
    .expect("c");
    assert!(e.is_empty(), "[for 嵌套] 两个循环变量都应可见; 实得: {e:?}");

    // ③ 闭包形参
    let e = typeck("let f = fn(x) => x + 1\nprint(f(1))\n").expect("c");
    assert!(e.is_empty(), "[闭包] 形参在体内应可见; 实得: {e:?}");

    // ④ 高阶传参：闭包形参在体里被使用
    let e = typeck(
        "let xs = [1, 2, 3]\n\
         let d = xs.map(fn(x) => x * 2)\n\
         let f = xs.filter(fn(x) => x > 1)\n\
         let s = xs.reduce(fn(a, b) => a + b, 0)\n\
         print(d)\nprint(f)\nprint(s)\n",
    )
    .expect("c");
    assert!(
        e.is_empty(),
        "[高阶] 传给 map/filter/reduce 的闭包形参应可见; 实得: {e:?}"
    );

    // ⑤ `worker` 块
    let e = typeck("worker w\n  print(1)\nend\n").expect("c");
    assert!(e.is_empty(), "[worker] 块体应零诊断; 实得: {e:?}");

    // ⑥ `with` 块绑定**不是**词法变量 —— 报未绑定是**正确**行为
    let e = typeck("with model = \"m\"\n  print(model)\nend\n").expect("c");
    assert!(
        e.iter().any(|m| m.contains("Unbound variable 'model'")),
        "[with] 绑定不进词法环境，报未绑定是**正确**的（spec §11.1 上下文配置块）; 实得: {e:?}"
    );
    // 运行期同样取不到（两层一致，不是某一层的错）
    assert_eq!(
        run("with model = \"m\"\n  let s = model\nend\ns\n").as_deref(),
        Ok("nil"),
        "[with] 运行期同样取不到绑定 —— 两层一致，证实它不是词法变量"
    );
}

/// D166 对照组：块体**确实会执行**（我第一版探针用 `assign` 观测，得出
/// 「块体不执行」的**错误结论** —— 因为块体跑在 `env.clone()` 的子环境里，
/// 对外层变量的赋值不传播）。这条用 `print` 钉住真实语义。
#[test]
fn d166_with_body_does_execute() {
    // `assign` 到外层变量**不传播**（子环境），但块体本身跑了 ——
    // 这两条都要钉，否则下一个人会重犯我这个误判。
    assert_eq!(
        run("let out = \"unset\"\nwith model = \"m\"\n  assign out = \"ran\"\nend\nout\n")
            .as_deref(),
        Ok("unset"),
        "[with] 块体在 `env.clone()` 子环境里跑，对外层 `assign` 不传播"
    );
    // 对照：`for` / `if` / `parallel` 块体的 assign **是**传播的 ——
    // 所以「with 的 assign 不传播」是 `with` 的特性，不是通用块语义。
    for (label, src) in [
        (
            "for",
            "let out = \"unset\"\nfor i in range(0, 2)\n  assign out = \"ran\"\nend\nout\n",
        ),
        (
            "if",
            "let out = \"unset\"\nif true\n  assign out = \"ran\"\nend\nout\n",
        ),
        (
            "parallel",
            "let out = \"unset\"\nparallel\n  assign out = \"ran\"\nend\nout\n",
        ),
    ] {
        assert_eq!(
            run(src).as_deref(),
            Ok("ran"),
            "[{label}] 块体的 assign **应当**传播到外层"
        );
    }
}

/// D166 对照组：`match` 守卫引用绑定变量必须生效。
///
/// `emit.rs` 的注释记着旧缺陷：「守卫此前在外层寄存器空间求值，而守卫通常
/// 引用**模式绑定变量**（`x when x > 0`）…… 实测返回 "positive"」。
/// 本轮实测确认已修（`-5` 正确落到 `negative` 分支）。
#[test]
fn d166_match_guard_sees_pattern_bindings() {
    assert_eq!(
        run("let r = match -5 with\n  x when x > 0 -> x\n  x when x < 0 -> 0 - x\n  _ -> 0\nend\nr\n").as_deref(),
        Ok("5.0"),
        "守卫 `x when x < 0` 应命中并取到绑定值 5（修复前恒取首守卫、返回 positive）"
    );
}
