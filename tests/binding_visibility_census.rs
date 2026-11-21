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
//! | `with` 块绑定（**历史**语法，v0.110.0 解析器已无此形态） | ❌（设计如此） | **正确，见下** |
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
//! **两层一致** —— `with` 当年是**上下文配置块**（spec §11.1），绑定只进
//! **历史** 的 interpreter config 栈供 `ai.chat` 等消费（**历史** `ai_chat.rs:81-86`
//! 确实读 `current_ai_config.model` —— 该栈与 `ai.chat` 已随 v0.110.0 移除，见 `src/runtime/core.rs:3`），
//! **不进入词法环境**。故块内按名引用报错是**正确行为**。
//!
//! ⚠ `tests/with_config.rs` 的文件头**早已写明**这一点，并明确记录（**历史**：该文件现已不存在）
//! 「早先一轮曾把它误判为作用域缺陷，spec 定性后已否证」。
//! 本轮是**同一个误判的第二次**。本条把结论钉在这里以防第三次。

use mora::typeck::check_mir::check_program_witnesses;

fn typeck(src: &str) -> Result<Vec<String>, String> {
    let (_p, w) = mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    Ok(check_program_witnesses(&w)
        .iter()
        .map(|e| e.message.clone())
        .collect())
}

fn run(src: &str) -> Result<String, String> {
    let (p, _w) = mora::parser_v3::ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut core = mora::runtime::CoreRuntime::default();
    let arc = std::sync::Arc::new(p);
    mora::runtime::vm::execute_program(arc, &mut core)
        .map(|(v, _)| format!("{v}"))
        .map_err(|e| e.to_string())
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
}

/// D166 对照组：`for` / `if` / `parallel` 块体的 `assign` 会传播到外层。
#[test]
fn d166_control_block_assign_propagates() {
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
        Ok("5"),
        "守卫 `x when x < 0` 应命中并取到绑定值 5（修复前恒取首守卫、返回 positive）"
    );
}
