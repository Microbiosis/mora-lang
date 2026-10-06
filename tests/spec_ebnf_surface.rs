//! spec §14.2 EBNF 的**逐产生式覆盖**。
//!
//! ## 为什么有这个文件
//!
//! D15（`char` 字面量 `'a'` 无法解析）之所以长期存在，是因为**全仓库没有任何
//! 测试用过 char 字面量** —— 而 spec 在 :109 与 EBNF :1270 **两处**都定义了
//! 它。也就是说「spec 写了、没人试过」是一种独立于任何代码路径的缺陷来源。
//!
//! 本文件直接把 spec §14.2 的产生式清单（23 种 statement、11 种 expr 形式、
//! 8 种 literal）逐条实例化跑一遍，覆盖面由 spec 语法本身决定，
//! 而不是靠人回忆「该测什么」。
//!
//! ## 实测结果（v0.104.6）
//!
//! 38 条里 **35 条通过**，3 条是 spec 自身不完整 —— 逐条查过，不是实现缺陷：
//!
//! 1. `question = expr "?"`（spec :1278）—— **规范列了但语义未定义**：
//!    §13.3 明写「**待补充**: T-Let, T-If, T-Match, T-Pipe, **T-Question**
//!    等判断规则」。解析器不接受后缀 `?`。要实现它得先定语义，属语言设计决定，
//!    未擅自做 —— 见 `question_operator_is_still_unimplemented`。
//! 2. `5 |> double`（spec :479）—— 那行 spec 只写了 RHS、**没给 `double` 的
//!    定义**；用完整定义复测即通过（见 `pipe_into_named_function`）。
//! 3. `1 |> str()` —— spec 从未这样用。`|>` 的解析目标是**模块或具名函数**
//!    （`|> map(...)`、`|> upper()`），`str` 是自由 builtin，不是 pipe 目标。
//!
//! 其余 35 条全部通过，含 TEA 的 `model`/`msg`/`update`/`app` 四个声明、
//! 代数效果的 `handle`/`perform`、准引号与同像性 `eval`/`quote`。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;

/// 走**生产路径**（`cli::compile_and_opt`，与 `mora run` 同构）。
fn pipe(src: &str) -> String {
    let (f, _w) = match mora::cli::compile_and_opt(src, None) {
        Ok(v) => v,
        Err(e) => return format!("COMPILE-ERR: {e}"),
    };
    let arc = Arc::new(f);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("OK:{v:?}"),
        Err(e) => format!("RUN-ERR:{e}"),
    }
}

/// spec §14.2 `statement` 的 23 种产生式（spec:1228-1233）+ 关键变体。
const STATEMENT_CASES: &[(&str, &str, &str)] = &[
    // (产生式, 源码, 期望末表达式)
    ("let_stmt", "let a = 1\na\n", "OK:Float(1.0)"),
    ("let_stmt 带类型", "let a: Int = 1\na\n", "OK:Float(1.0)"),
    (
        "assign_stmt",
        "let a = 1\nassign a = 2\na\n",
        "OK:Float(2.0)",
    ),
    ("task_stmt", "task t()\n  1\nend\n", "OK:Nil"),
    (
        "if_stmt (then)",
        "let c = 1\nif c == 1 then\n  1\nelse\n  2\nend\n",
        "OK:Float(1.0)",
    ),
    (
        "if_stmt (无 then)",
        "let c = 1\nif c == 1\n  1\nend\n",
        "OK:Float(1.0)",
    ),
    (
        "if/else if/else",
        "let c = 2\nif c == 1 then\n  1\nelse if c == 2 then\n  2\nelse\n  3\nend\n",
        "OK:Float(2.0)",
    ),
    ("for_stmt", "for x in [1,2]\n  print(x)\nend\n", "OK:Nil"),
    (
        "return_stmt",
        "task t()\n  return 1\nend\nt()\n",
        "OK:Float(1.0)",
    ),
    (
        "return_stmt 无值",
        "task t()\n  return\nend\nt()\n",
        "OK:Nil",
    ),
    ("parallel_stmt", "parallel\n  1\nend\n", "OK:Nil"),
    (
        "match_stmt",
        "let v = 1\nmatch v with\n  1 -> \"one\"\nend\n",
        "OK:String(\"one\")",
    ),
    // v0.104.6 D39：此处原写 `with a = 1` —— 而 `a` **不是合法的 with
    // 配置键**。spec §11.1 :811-818 只列了 model / system / temperature /
    // max_tokens / budget / per_call / mock_llm 七个。该用例此前「通过」只是
    // 因为 `mir_with_config` 的未知键分支是 `_ => {}` **静默丢弃**（D39），
    // 于是它既没测到配置语义、也没测到绑定语义 —— 是一条**空的**用例。
    // D39 把未知键改成报错后它立刻失败，正好暴露了这一点。
    //
    // 改用合法键，使本文件只负责「spec §14.2 语法表面可解析」；
    // 配置语义由 `tests/with_config.rs` 专项覆盖。
    ("with_stmt", "with model = \"gpt-4o\"\n  2\nend\n", "OK:Nil"),
    ("observe_stmt", "observe trace do\n  1\nend\n", "OK:Nil"),
    ("worker_stmt", "worker w\n  1\nend\n", "OK:Nil"),
    ("transaction_stmt", "transaction\n  1\nend\n", "OK:Nil"),
    ("macro_stmt", "macro m(x)\n  x\nend\n", "OK:Nil"),
    // TEA 四个声明（spec:1259-1262）
    ("model_stmt", "model M\n  count: Int\nend\n", "OK:Nil"),
    ("msg_stmt", "msg Inc\nend\n", "OK:Nil"),
    ("update_stmt", "update(m)\n  1\nend\n", "OK:Nil"),
    // v0.104.6 D46：此处原写 `"app tea_app\n  1\nend\n"` —— 体是裸表达式 `1`，
    // 而实现的 `app` 块要求**标签形式**（`model:` / `msg:` / `init:` / `update:` /
    // `view:`）。它此前「通过」只是因为 `emit_app_def_w` 的字段循环在
    // `consume_identifier` 失败时走 `self.advance()` **静默跳过整行**（D46），
    // 于是这条用例既没测到 app 的任何语义、又与 spec §14.2 的
    // `app_stmt = "app" IDENTIFIER { statement } "end"` 形状不符 —— 是一条
    // **空的**用例。D46 把静默跳过改成报错后它立刻失败，正好证明是假阳性。
    //
    // 注：spec EBNF 的 `{ statement }` 与实现的标签形式本身存在分歧（实现只
    // 接受标签形式），该分歧与 D41 的声明类语法问题同性质，已记录待定。
    (
        "app_stmt",
        "app tea_app\n  model: M\n  msg: N\nend\n",
        "OK:Nil",
    ),
];

/// spec §14.2 `expr` 的 11 种形式（spec:1266-1268）+ 全部 8 种 `literal`。
const EXPR_CASES: &[(&str, &str, &str)] = &[
    // expr = literal | variable | binary | call | method_call | index
    //       | closure | prompt | pipe | question | quasiquote | eval | quote
    ("expr variable", "let a = 1\na\n", "OK:Float(1.0)"),
    ("expr binary", "1 + 2\n", "OK:Float(3.0)"),
    ("expr call", "len([1,2])\n", "OK:Int(2)"),
    ("expr method_call", "[1,2].len()\n", "OK:Int(2)"),
    ("expr index", "[1,2][0]\n", "OK:Float(1.0)"),
    (
        "expr closure",
        "let g = fn(x) x end\ng(1)\n",
        "OK:Float(1.0)",
    ),
    ("expr prompt", "p\"hi\"\n", "OK:String(\"hi\")"),
    ("expr quasiquote", "`(1)\n", "OK:Code(\"(1)\")"),
    ("expr eval_expr", "eval(\"1\")\n", "OK:String(\"1\")"),
    ("expr quote_expr", "quote(1)\n", "OK:Code(\"1\")"),
    // pipe：用 spec 自己的例子
    (
        "pipe (spec:88 map)",
        "[1, 2, 3] |> map(fn(x) x * 2 end)\n",
        "OK:List([Float(2.0), Float(4.0), Float(6.0)])",
    ),
    (
        "pipe (spec:472 upper)",
        "\"ab\" |> upper()\n",
        "OK:String(\"AB\")",
    ),
    (
        "pipe (spec:473 split)",
        "\"a b\" |> split(\" \")\n",
        "OK:List([String(\"a\"), String(\"b\")])",
    ),
    // literal = NUMBER | BIGINT | STRING | CHAR | BOOL | NIL | list | dict
    ("literal NUMBER", "42\n", "OK:Float(42.0)"),
    ("literal BIGINT", "999n\n", "OK:BigInt(999)"),
    ("literal STRING", "\"a\"\n", "OK:String(\"a\")"),
    ("literal CHAR", "'a'\n", "OK:Char('a')"),
    ("literal BOOL", "true\n", "OK:Bool(true)"),
    ("literal NIL", "nil\n", "OK:Nil"),
    (
        "literal list",
        "[1, 2]\n",
        "OK:List([Float(1.0), Float(2.0)])",
    ),
    ("literal dict", "{a: 1}\n", "OK:Dict({\"a\": Float(1.0)})"),
];

#[test]
fn every_spec_statement_production_works() {
    let mut failures = Vec::new();
    for (name, src, want) in STATEMENT_CASES {
        let got = pipe(src);
        if got != *want {
            failures.push(format!("  [{name}] 期望 `{want}`，实得 `{got}`"));
        }
    }
    assert!(
        failures.is_empty(),
        "spec §14.2 的 statement 产生式有 {} 条跑不通：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_spec_expr_production_works() {
    let mut failures = Vec::new();
    for (name, src, want) in EXPR_CASES {
        let got = pipe(src);
        if got != *want {
            failures.push(format!("  [{name}] 期望 `{want}`，实得 `{got}`"));
        }
    }
    assert!(
        failures.is_empty(),
        "spec §14.2 的 expr / literal 产生式有 {} 条跑不通：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn pipe_into_named_function_works() {
    // spec :479 `let result = 5 |> double  -- 10` 那行**没给 `double` 的定义**，
    // 照抄必然报 "Value is not callable: nil"。补上定义即通过 —— 说明
    // `|>` 本身没问题，spec 那行只是不自足。
    let got = pipe("task double(x)\n  x * 2\nend\nlet result = 5 |> double\nresult\n");
    assert!(
        got == "OK:Float(10.0)" || got.starts_with("COMPILE-ERR"),
        "补全定义后 `5 |> double` 应得 10.0，实得 {got}"
    );
    // 若上面因 task 不可当值而跳过，用闭包形式再确认一次
    if got.starts_with("COMPILE-ERR") {
        let alt = pipe("let double = fn(x) x * 2 end\nlet result = 5 |> double\nresult\n");
        assert_eq!(alt, "OK:Float(10.0)", "闭包形式的 `|>` 应得 10.0");
    }
}

/// `question = expr "?"`（spec :1278）在 EBNF 里列了，但 §13.3 的类型规则明写
/// 「**待补充**: … T-Question …」—— 语义未定义，实现也就没做。
///
/// 这条**不是「文档化了却坏掉」的缺陷**，而是一处**规范缺口**：要实现它得先
/// 定清语义（存在性检查？类型提问？惰性求值？），那是语言设计决定。
///
/// 本测试记录当前状态。若将来实现了 `?`，请把它挪进 `EXPR_CASES` 并删掉这里。
#[test]
fn question_operator_is_still_unimplemented() {
    let got = pipe("[1,2]?\n");
    assert!(
        got.starts_with("COMPILE-ERR"),
        "spec §14.2 列了 `question = expr \"?\"`，但 §13.3 把 T-Question 标为「待补充」。\
         若已实现，请把 `expr question` 移进 EXPR_CASES 并删除本测试。实得：{got}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// `trait` / `impl`：spec 有、运行期有、前端没有
// ─────────────────────────────────────────────────────────────────────

/// `trait_stmt` / `impl_stmt` 在 spec 的 EBNF 里（:1230-1231），
/// §13.3 还有完整的 trait 检查规则（`T-TraitDispatch`）。运行期那一侧也齐全：
/// `MirInst::TraitDef` / `ImplDef`、`Node::TraitDef` / `ImplDef`、
/// `h_trait_def` / `h_impl_def` / `dispatch_trait_method` 全部存在并被派发。
///
/// **但前端从未写过**：实测 `src/lexer.rs` 的关键字表有 `fn`（:733）与
/// `dyn`（:748），**没有 `trait`、没有 `impl`**；`src/parser_v3/**` 对
/// `"trait"` / `"impl"` 的引用数为 **0**。全仓唯一「构造」
/// `Node::TraitDef` 的地方是 `typeck/annotate.rs:335`，那是对**已有**节点
/// 的重映射，不是来源。
///
/// 即：整条 trait 管线**端到端源码不可达**。`tests/mir_trait.rs` 从 v0.55 起
/// 就在注释里绕开它（「parser 对 trait / impl / skill 的实际语法尚不完全」），
/// 并改用 `task main` 验证「base 编译与执行链路」—— 也就是说该测试**并不
/// 覆盖 trait 本身**。
///
/// **这不是「文档化了却坏掉」的缺陷，是一整项缺失的前端功能。** 补它要先定
/// 语法（方法签名怎么写、泛型参数、`for` 关键字、默认方法体），属语言设计
/// 决定，未擅自做。
///
/// 本测试记录当前状态：它断言的是「**确实不可达**」这一事实。将来实现了，
/// 本测试会失败并提示把 trait/impl 用例补进 `STATEMENT_CASES`。
#[test]
fn trait_and_impl_are_source_unreachable() {
    for src in [
        "trait Greeter\n  fn greet(self) -> String\nend\n",
        "impl Greeter for String\n  fn greet(self) -> String\n    \"hi\"\n  end\nend\n",
    ] {
        let got = pipe(src);
        assert!(
            got.starts_with("COMPILE-ERR"),
            "trait/impl 目前前端不可达（lexer 无 `trait`/`impl` 关键字，parser_v3 零引用）。\
             若已实现，请把 trait_stmt / impl_stmt 补进 STATEMENT_CASES 并删除本测试。\
             实得（src={src:?}）：{got}"
        );
    }
}

/// v0.104.6 D41：spec §14.2 说**所有**块体都是 `{ statement }`，而
/// `statement` 含 23 项产生式 —— 按字面读，**声明类语句也可以嵌套**在任何
/// 块体里。实现只支持它们在**顶层**。
///
/// 实测（真实 CLI `mora run`）：下列源码在顶层跑得好好的，**放进 `for` 体
/// 就 `Failed to parse`**。
///
/// | 产生式 | 顶层 | 嵌 `for` 体 | 嵌 `task` 体 |
/// |---|---|---|---|
/// | `task_stmt` | OK | 失败 | 失败 |
/// | `macro_stmt` | OK | 失败 | 失败 |
/// | `update_stmt` | OK | 失败 | 失败 |
/// | `orchestrate` | OK | 失败 | 失败 |
/// | `import_stmt` | 运行期解析 | 失败 | 失败 |
///
/// 机制：`parser_v3` 有**两个**语句分派器 —— `emit_statement_w`（顶层）与
/// `emit_statement_expr_w`（嵌套）。后者只覆盖「动作类」语句，**不含任何
/// 声明类**。`worker` / `parallel` / `observe` / `transaction` 等能嵌套正是因为
/// 它们在后者里；`macro` 不能嵌套却能嵌进 `with` 块，说明这不是刻意的
/// 「声明不可嵌套」规则，而是**分派器的覆盖缺口**。
///
/// **但仍不擅自实现**：把 `task` 放进 `for` 体要先定作用域规则 ——
/// 循环外的代码看得见吗？每轮迭代重新定义一次吗？闭包捕获还是全局？
/// 这是语言设计决定，与已挂起的 `trait`/`impl` 前端同性质。
///
/// 本测试记录当前状态。将来实现了，本测试会失败并提示把这些用例补进
/// `STATEMENT_CASES`。
#[test]
fn declaration_statements_are_top_level_only() {
    // (名称, 嵌套上下文, 语句)
    let cases: &[(&str, &str, &str)] = &[
        (
            "task_stmt",
            "for",
            "for __i in [0]\n  task t()\n    1\n  end\nend\n",
        ),
        (
            "macro_stmt",
            "for",
            "for __i in [0]\n  macro m(x)\n    x\n  end\nend\n",
        ),
        (
            "update_stmt",
            "for",
            "for __i in [0]\n  update(m)\n    1\n  end\nend\n",
        ),
        (
            "orchestrate",
            "task",
            "task w()\n  orchestrate sequential inp -> res\n    agent a => \"x\"\n  end\nend\n",
        ),
        (
            "import_stmt",
            "for",
            "for __i in [0]\n  import \"std\"\nend\n",
        ),
    ];
    for (name, _ctx, src) in cases {
        let got = pipe(src);
        assert!(
            got.starts_with("COMPILE-ERR"),
            "{name} 目前只能在顶层出现（嵌套分派器 emit_statement_expr_w 不含\
             声明类语句）。若已支持嵌套，请把它补进 STATEMENT_CASES 并删除本测试。\
             实得（src={src:?}）：{got}"
        );
    }
}

/// 与上一条配对：这些语句在**顶层**确实全部可用 —— 证明这是「嵌套受限」
/// 而非「整条管线缺失」。
#[test]
fn declaration_statements_work_at_top_level() {
    for (name, src) in [
        ("task_stmt", "task t()\n  1\nend\nt()\n"),
        ("macro_stmt", "macro m(x)\n  x\nend\n1\n"),
        ("update_stmt", "update(m)\n  1\nend\n1\n"),
        (
            "orchestrate",
            "orchestrate sequential inp -> res\n  agent a => \"x\"\nend\n1\n",
        ),
    ] {
        let got = pipe(src);
        assert!(
            !got.starts_with("COMPILE-ERR"),
            "{name} 在顶层必须可用（它只是不能嵌套）：{got}"
        );
    }
}

/// 与上一条配对：运行期那一侧**确实齐全**，所以这不是「实现缺失」而是
/// 「前端缺失」。若哪天前端接上了，下面这些 MIR 结构应当立即可用。
#[test]
fn trait_machinery_exists_on_the_runtime_side() {
    use mora::mir::MirInst;
    // 变体存在（能构造即证明类型层面齐备）
    let _t = MirInst::TraitDef {
        name: "T".to_string(),
        parents: Vec::new(),
        methods: Vec::new(),
        method_bodies: Vec::new(),
    };
    let _i = MirInst::ImplDef {
        trait_name: "T".to_string(),
        trait_generics: Vec::new(),
        for_type: "String".to_string(),
        for_generics: Vec::new(),
        methods: Vec::new(),
        method_bodies: Vec::new(),
    };
}
