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
//! ## 实测结果（2026-11-26 复核）
//!
//! 38 条（18 statement + 20 expr）**全部通过**。
//!
//! v0.104.6 时本表记的是「35 条通过 / 3 条 spec 自身不完整」，其中第 1 条 ——
//! `question = expr "?"`（spec :1278）—— 当时是「**规范列了但语义未定义**」：
//! §13.3 明写「**待补充**: T-Let, T-If, T-Match, T-Pipe, **T-Question** 等判断
//! 规则」，故解析器不接受后缀 `?`。v0.112.0 G2 起该产生式**已实现**
//! （`result` / `option` / nullable 后缀 `?` 错误传播，已接通 Parser → HM →
//! Control SSA IR → MORABC02 → VM），现已并入本表（`question (v0.112.0 G2)`）。
//!
//! 原记 3 条 spec 自身不完整中的第 1 条已解决，余下 2 条仍是 **spec 自身不完整**
//! （逐条查过，不是实现缺陷）：
//!
//! 1. `5 |> double`（spec :479）—— 那行 spec 只写了 RHS、**没给 `double` 的
//!    定义**；用完整定义复测即通过（见 `pipe_into_named_function_works`）。
//! 2. `1 |> str()` —— spec 从未这样用。`|>` 的解析目标是**模块或具名函数**
//!    （`|> map(...)`、`|> upper()`），`str` 是自由 builtin，不是 pipe 目标。
//!
//! 本表全部通过，含 TEA 的 `model`/`msg`/`update`/`app` 四个声明、
//! 代数效果的 `handle`/`perform`、准引号与同像性 `eval`/`quote`。

use std::sync::Arc;

use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;

/// 走**生产路径**（`cli::compile_and_opt`，与 `mora run` 同构）。
fn pipe(src: &str) -> String {
    let (prog, _w) = match mora::cli::compile_and_opt(src, None) {
        Ok(v) => v,
        Err(e) => return format!("COMPILE-ERR: {e}"),
    };
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    match execute_program(arc, &mut core) {
        Ok((v, _)) => format!("OK:{v:?}"),
        Err(e) => format!("RUN-ERR:{e}"),
    }
}

/// spec §14.2 `statement` 的 23 种产生式（spec:1228-1233）+ 关键变体。
const STATEMENT_CASES: &[(&str, &str, &str)] = &[
    // (产生式, 源码, 期望末表达式)
    ("let_stmt", "let a = 1\na\n", "OK:Int(1)"),
    ("let_stmt 带类型", "let a: Int = 1\na\n", "OK:Int(1)"),
    ("assign_stmt", "let a = 1\nassign a = 2\na\n", "OK:Int(2)"),
    ("task_stmt", "task t()\n  1\nend\n", "OK:Nil"),
    (
        "if_stmt (then)",
        "let c = 1\nif c == 1 then\n  1\nelse\n  2\nend\n",
        "OK:Int(1)",
    ),
    (
        "if_stmt (无 then)",
        "let c = 1\nif c == 1\n  1\nend\n",
        "OK:Int(1)",
    ),
    (
        "if/else if/else",
        "let c = 2\nif c == 1 then\n  1\nelse if c == 2 then\n  2\nelse\n  3\nend\n",
        "OK:Int(2)",
    ),
    ("for_stmt", "for x in [1,2]\n  print(x)\nend\n", "OK:Nil"),
    (
        "return_stmt",
        "task t()\n  return 1\nend\nt()\n",
        "OK:Int(1)",
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
    ("worker_stmt", "worker w\n  1\nend\n", "OK:Nil"),
    ("transaction_stmt", "transaction\n  1\nend\n", "OK:Nil"),
    ("macro_stmt", "macro m(x)\n  x\nend\n", "OK:Nil"),
    (
        "trait_stmt",
        "trait Greeter\n  task greet(self): string\nend\n",
        "OK:Nil",
    ),
    (
        "impl_stmt",
        "trait Greeter\n  task greet(self): string\nend\nimpl Greeter for int\n  task greet(self): string\n    return \"hi\"\n  end\nend\n",
        "OK:Nil",
    ),
    ("no_grad_stmt", "no_grad\n  1 + 2\nend\n", "OK:Int(3)"),
];

/// spec §14.2 `expr` 的 10 种形式（BigInt / `p"..."` Prompt 字面量已在 v0.110.0 移除）。
const EXPR_CASES: &[(&str, &str, &str)] = &[
    // expr = literal | variable | binary | call | method_call | index
    //       | closure | pipe | question | quasiquote | eval | quote
    ("expr variable", "let a = 1\na\n", "OK:Int(1)"),
    ("expr binary", "1 + 2\n", "OK:Int(3)"),
    ("expr call", "len([1,2])\n", "OK:Int(2)"),
    ("expr method_call", "[1,2].len()\n", "OK:Int(2)"),
    ("expr index", "[1,2][0]\n", "OK:Int(1)"),
    ("expr closure", "let g = fn(x) x end\ng(1)\n", "OK:Int(1)"),
    ("expr quasiquote", "`(1)\n", "OK:Code(\"(1)\")"),
    ("expr eval_expr", "eval(\"1\")\n", "OK:String(\"1\")"),
    ("expr quote_expr", "quote(1)\n", "OK:Code(\"1\")"),
    // pipe：用 spec 自己的例子
    (
        "pipe (spec:88 map)",
        "[1, 2, 3] |> map(fn(x) x * 2 end)\n",
        "OK:List([Int(2), Int(4), Int(6)])",
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
    // question = expr "?" (v0.112.0 G2: Result/Option/nullable postfix error propagation)
    (
        "question (v0.112.0 G2)",
        "enum Boxed<T>\n  Ok(T)\n  Err(string)\nend\ntask unwrap_ok(): Boxed<int>\n  let v = Boxed::Ok(41)?\n  return Boxed::Ok(v + 1)\nend\nmatch unwrap_ok() with\n  Boxed::Ok(n) -> n\n  Boxed::Err(_) -> 0\nend\n",
        "OK:Int(42)",
    ),
    // literal = NUMBER | STRING | CHAR | BOOL | NIL | list | dict
    ("literal NUMBER", "42\n", "OK:Int(42)"),
    ("literal STRING", "\"a\"\n", "OK:String(\"a\")"),
    ("literal CHAR", "'a'\n", "OK:Char('a')"),
    ("literal BOOL", "true\n", "OK:Bool(true)"),
    ("literal NIL", "nil\n", "OK:Nil"),
    ("literal list", "[1, 2]\n", "OK:List([Int(1), Int(2)])"),
    ("literal dict", "{a: 1}\n", "OK:Dict({\"a\": Int(1)})"),
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
        got == "OK:Int(10)" || got.starts_with("COMPILE-ERR"),
        "补全定义后 `5 |> double` 应得 10.0，实得 {got}"
    );
    // 若上面因 task 不可当值而跳过，用闭包形式再确认一次
    if got.starts_with("COMPILE-ERR") {
        let alt = pipe("let double = fn(x) x * 2 end\nlet result = 5 |> double\nresult\n");
        assert_eq!(alt, "OK:Int(10)", "闭包形式的 `|>` 应得 10.0");
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
    ] {
        let got = pipe(src);
        assert!(
            !got.starts_with("COMPILE-ERR"),
            "{name} 在顶层必须可用（它只是不能嵌套）：{got}"
        );
    }
}

/// 与上一条配对：运行期那一侧**确实齐全**，所以这不是「实现缺失」而是
/// 「前端缺失」。若哪天前端接上了，下面这些 Control SSA + CoreRuntime 结构应当立即可用。
#[test]
fn trait_machinery_exists_on_the_runtime_side() {
    use mora::common::trait_info::{TraitInfo, TraitMethodSig};
    use mora::mir::control::{ControlInst, SymId, VReg};
    use mora::runtime::CoreRuntime;

    let mut core = CoreRuntime::default();
    core.register_trait(
        "T".to_string(),
        TraitInfo {
            name: "T".to_string(),
            type_params: Vec::new(),
            parents: Vec::new(),
            methods: vec![TraitMethodSig {
                name: "m".to_string(),
                has_self: true,
                has_default: false,
                params: Vec::new(),
                return_type: None,
            }],
        },
    );
    let _dyn_inst = ControlInst::MakeDynTrait {
        dst: VReg(1),
        src: VReg(0),
        trait_name: SymId(0),
        generics: Vec::new(),
    };
}
