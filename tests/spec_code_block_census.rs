//! v0.104.6 D110：spec 里 ```mora 代码块的**可解析性普查**。
//!
//! ## 为什么做这件事
//!
//! D108/D109 在 §14.2 的**产生式**层面查出「spec 承诺、实现解析不了」。
//! 但 spec 的真正载体是那 **20 段 ```mora 示例代码** —— 它们是读者照抄的
//! 东西，比产生式更容易过时。
//!
//! ⚠ 而且 `docs/mora-spec.md` **不在版本控制内**（`.gitignore` 第 8 行 `docs/`），
//! 漂移不会有任何 git 信号。
//!
//! ## 本文件**真的读 spec**
//!
//! D108 第一版只硬编码源码、从不打开 spec，把 spec 改回去测试照样全绿。
//! 这里从文件里提取代码块，spec 一改立刻反映。

use mora::parser_v3::ParserV3;

/// 提取 spec 里所有 ```mora … ``` 代码块。
fn spec_mora_blocks() -> Vec<(usize, String)> {
    let text = std::fs::read_to_string("docs/mora-spec.md").expect("读 docs/mora-spec.md");
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "```mora" {
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && lines[j].trim() != "```" {
                j += 1;
            }
            out.push((start + 1, lines[start..j].join("\n")));
            i = j;
        }
        i += 1;
    }
    out
}

/// 判一个代码块是否**本来就没有可执行代码**（纯注释 / 纯空行）。
///
/// D118：§14.3「注释」整段只写 `-- 单行注释`，被普查报成
/// `empty program: parser produced no executable instructions` ——
/// 那是**普查工具的假阳性**，不是 spec 错：演示注释的块本就该是空程序。
fn is_inert_block(src: &str) -> bool {
    // ⚠ 判据必须是「每一行都**要么**空 **要么**是注释」。
    //   第一版写成 filter(非空且非注释) + all(is_empty)，而 filter 后剩下的
    //   元素按定义非空 —— `all(is_empty)` 成了恒真的空真，函数对任何输入
    //   都返回 true。正常路径不会暴露它（坏块会被误归 inert 从清单消失），
    //   是下面那条反向对照把它咬住的。
    src.lines()
        .map(|l| l.trim())
        .all(|l| l.is_empty() || l.starts_with("--"))
}

/// 普查：逐块判定能否**解析**（只解析，不执行 —— 执行需要 mock_llm /
/// 网络 / 文件，且 spec 示例常是片段）。
///
/// 不断言全绿：本测试把结果落到输出，作为下一轮据以修 spec 的清单。
/// 断言只保证「块被找出来了」。
#[test]
fn d110_census_of_spec_mora_blocks() {
    let blocks = spec_mora_blocks();
    eprintln!("spec 中 ```mora 代码块数: {}", blocks.len());
    assert!(
        !blocks.is_empty(),
        "从 spec 里一个 ```mora 块都没提取到 —— 提取逻辑坏了（不是 spec 变了）"
    );

    let mut bad = Vec::new();
    let mut inert = Vec::new();
    for (line, src) in &blocks {
        if is_inert_block(src) {
            inert.push(*line);
            continue;
        }
        match ParserV3::compile(src) {
            Ok(_) => {}
            Err(e) => {
                let first = e.lines().next().unwrap_or("").to_string();
                bad.push(format!("spec 行 {line}: {first}"));
                eprintln!(
                    "  spec:{line} 不可解析 — {first}\n    |{}|",
                    src.lines().next().unwrap_or("")
                );
            }
        }
    }
    eprintln!(
        "D118 纯注释块（预期空程序，{inert:?}）；不可解析清单（{}/{}）：\n  {}",
        bad.len(),
        blocks.len(),
        bad.join("\n  ")
    );
}

/// D118：§14.3「注释」段是纯注释块，**必须**被归到 `inert` 而不是 `bad`。
///
/// 若本测试失败，要么 spec 往那段塞了真代码（该判可解析），
/// 要么 `is_inert_block` 的判据坏了 —— 后者会让**所有**空程序块静默消失。
#[test]
fn d118_inert_blocks_are_classified_not_reported_as_broken() {
    let blocks = spec_mora_blocks();
    let sec = blocks
        .iter()
        .find(|(_, s)| s.contains("单行注释"))
        .unwrap_or_else(|| panic!("spec §14.3 的注释块不见了 —— 块数 {}", blocks.len()));
    assert!(
        is_inert_block(&sec.1),
        "§14.3 块应被识别为纯注释（inert），实际内容:\n{}",
        sec.1
    );

    // 反向对照：真代码块**绝不能**被误判成 inert
    // （否则 is_inert_block 一坏，全部坏块都会从清单里消失）
    assert!(!is_inert_block("let x = 1\n"));
    assert!(!is_inert_block("-- 注释\nlet x = 1\n"));
    assert!(is_inert_block("-- 只有注释\n"));
    assert!(is_inert_block("\n\n"));
}

/// 反向对照：`ParserV3` 确实能解析一段**合法**的 Mora ——
///
/// 若这条失败，说明上面「不可解析」的结论全部不可信（是解析器坏了）。
#[test]
fn d110_parser_actually_works() {
    assert!(ParserV3::compile("let x = 1\nprint(x)\n").is_ok());
    assert!(ParserV3::compile("for i in [1, 2]\n  print(i)\nend\n").is_ok());
    assert!(
        ParserV3::compile("let = = =\n").is_err(),
        "非法源码必须解析失败（若能通过，说明 typeck/parser 整体失守）"
    );
}

// ============================================================
// D110 逐条钉住：spec 承诺但实现不支持的四类
// ============================================================

/// **缺口 1**：`task` / `fn` 的**参数类型标注**完全不支持。
///
/// spec 里出现 7 处（`task greet(name: string): string` 等），但
/// `task f(a: number)` 与 `fn(a: number)` 同样报 `Expected ')' after parameters`。
#[test]
fn d110_function_parameter_annotations_are_not_supported() {
    for (name, src) in [
        ("task_param_annot", "task f(a: number)\n  return a\nend\n"),
        (
            "fn_param_annot",
            "let g = fn(a: number) a + 1\nprint(g(1))\n",
        ),
        // spec §14.2 附近原文：`task greet(name: string): string`
        (
            "spec_example",
            "task greet(name: string): string\n  return \"hi\"\nend\n",
        ),
    ] {
        assert!(
            ParserV3::compile(src).is_err(),
            "{name}: 若参数标注**已支持**，则本测试失败 —— 需把缺口 1 从本文件与 \
             CHANGELOG D110 移除，并更新 docs/mora-spec.md 的相关示例"
        );
    }
    // 对照：无标注的同形签名必须可用（否则不是「标注不支持」而是「签名不支持」）
    assert!(ParserV3::compile("task f(a)\n  return a\nend\n").is_ok());
    assert!(ParserV3::compile("let g = fn(a) a + 1\nprint(g(1))\n").is_ok());
}

/// **缺口 2**：`result<...>` 泛型标注**会**被拒（标注本身不支持，D111 起是**明确报错**
/// 而非静默丢弃）。
///
/// ⚠ 判据必须走 **完整编译**：`ParserV3::compile` 单独调时，标注会被
/// **静默丢弃**（D111 的缺陷），只靠 typeck 检不出来。
#[test]
fn d110_result_generic_annotation_is_rejected_not_silently_dropped() {
    // D111 前：`let r: result<number, string> = 1` 编译通过、typeck 0 条、
    // 程序 exit 0 且 `r = 1.0` —— 标注被丢弃。
    // D111 后：解析阶段就报 `unsupported generic type annotation` 并失败。
    let res = ParserV3::compile("let r: result<number, string> = 1\nprint(r)\n");
    assert!(
        res.is_err(),
        "`result<…>` 标注不受支持，必须**报错**而不是被静默丢弃。\
         若本测试失败，说明静默丢弃又回来了（D111 回归）"
    );

    // `update` 的返回标注是同一个坑（D111 一并修）
    let r2 = ParserV3::compile("update(msg, model): result<number, string>\n  model\nend\n");
    assert!(
        r2.is_err(),
        "`update` 的返回标注同样必须报错（不得静默丢弃）"
    );

    // 对照：受支持的泛型标注照常可用
    assert!(ParserV3::compile("let xs: list<number> = [1]\nprint(1)\n").is_ok());
    assert!(ParserV3::compile("let d: dict<string, number> = {a: 1}\nprint(1)\n").is_ok());
}

/// **缺口 1 / 3 的补充**：受支持的标注**确实生效**（会被 typeck 强制）——
/// 否则 D111 的「静默丢弃」就无从判别。
#[test]
fn d110_supported_annotations_are_enforced() {
    let err_count = |src: &str| {
        let (_, wits) = ParserV3::compile(src).expect("语法应通过");
        mora::typeck::check_mir::check_program_witnesses_bidirectional(&wits).len()
    };
    assert!(
        err_count("let r: string = 1\nprint(r)\n") > 0,
        "`string` 标注必须被 typeck 强制（否则无法区分「标注生效」与「标注被丢弃」）"
    );
    assert!(
        err_count("let r: list<number> = 1\nprint(r)\n") > 0,
        "`list<…>` 标注必须被 typeck 强制"
    );
    assert_eq!(
        err_count("let r: number = 1\nprint(r)\n"),
        0,
        "`number` 标注与右值相容，不应报错"
    );
}

/// **缺口 3**：`let` 的**解构模式**完全不支持，而 `match` 支持同样模式。
///
/// `let {a, b} = …` / `let [a, b] = …` 均报 `Expected variable name after 'let'`；
/// 但 `match d { [head, ...tail] => … }` 正常。
#[test]
fn d110_let_destructuring_is_not_supported() {
    for (name, src) in [
        ("dict_destructure", "let {a, b} = {a: 1, b: 2}\nprint(a)\n"),
        ("list_destructure", "let [a, b] = [1, 2]\nprint(a)\n"),
        (
            "list_rest",
            "let [head, ...tail] = [1, 2, 3]\nprint(head)\n",
        ),
    ] {
        assert!(
            ParserV3::compile(src).is_err(),
            "{name}: 若 let 解构**已支持**，则本测试失败 —— 需更新本文件与 CHANGELOG D110"
        );
    }
    // 对照：match 支持同样模式（含 list rest），说明模式语法本身在语言里存在
    assert!(
        ParserV3::compile("let d = [1, 2, 3]\nlet r = match d {\n  [head, ...tail] => head\n  _ => 0\n}\nprint(r)\n").is_ok()
    );
}

/// **D116**：spec §11.1 曾把 `budget` / `per_call` 列在「**支持的配置键**」下，
/// 但实现对这两个键**明确报错**（D39 的消息原文即
/// 「promised by spec §11.1 but not implemented yet」）。spec 侧已更正。
#[test]
fn d116_spec_does_not_claim_unimplemented_budget_keys() {
    let text = std::fs::read_to_string("docs/mora-spec.md").expect("读 docs/mora-spec.md");

    // 找到 §11.1 的「支持的配置键」列表
    let start = text
        .find("支持的配置键")
        .unwrap_or_else(|| panic!("spec 里找不到「支持的配置键」"));
    let list = &text[start..start + 600];

    for key in ["budget", "per_call"] {
        let in_supported = list
            .lines()
            .take_while(|l| !l.trim_start().starts_with("**"))
            .any(|l| l.trim_start().starts_with(&format!("- `{key}`")));
        assert!(
            !in_supported,
            "`{key}` 未被实现，写它会报错 —— 不得列在「支持的配置键」下"
        );
    }
    assert!(
        list.contains("承诺但未实现"),
        "§11.1 应显式标注 `budget` / `per_call` 为「承诺但未实现」"
    );
    // 对照：`mock_llm` / `model` 等确实被支持（D91 已修好 mock_llm）
    assert!(
        list.lines()
            .take(8)
            .any(|l| l.trim_start().starts_with("- `mock_llm`")),
        "对照失败：`mock_llm` 确实受支持（D91），应留在「支持的配置键」下"
    );
}

/// **D163 已翻转**：`params` 此前被 `task_stmt` / `macro_stmt` / `closure` 引用
/// 却**从未定义**（D110 记录，悬置多轮）。D163 已把它补成真产生式：
///
/// ```text
/// params = "(" [ IDENTIFIER { "," IDENTIFIER } ] ")" ;
/// ```
///
/// 形状对照 parser 实测（实现**不支持**形参类型标注，故不含 `":" type`），
/// 最小实例已加进 `tests/spec_ebnf_census.rs` 由 parser 持续验收。
///
/// D162 的引用完整性判据（`tests/spec_ebnf_wellformedness.rs`）是这条的
/// **推广版** —— 它不只盯 `params` 一个名字，而是查全部被引用却未定义的非终结符。
#[test]
fn d163_spec_params_non_terminal_is_now_defined() {
    let text = std::fs::read_to_string("docs/mora-spec.md").expect("读 docs/mora-spec.md");
    // ⚠ 关闭围栏的搜索必须**跳过开围栏本身**：`text[start..].find("```")` 会返回 0
    //   （切片自己就以围栏开头），于是取到空串 —— 第一版就栽在这里。
    let open = text.find("```ebnf").expect("spec §14.2 的 ebnf 块");
    let after_open = open + "```ebnf".len();
    let rel_close = text[after_open..].find("```").expect("ebnf 块未闭合");
    let ebnf = &text[open..after_open + rel_close];
    assert!(
        ebnf.lines()
            .any(|l| l.trim_start().starts_with("params") && l.contains('=')),
        "params 仍被引用却没有定义 —— D163 已补上产生式，若本条转红说明该改动被回退"
    );
    // 引用侧仍在（三处），确保补定义没有顺手把用法删掉
    for user in ["task_stmt", "macro_stmt", "closure"] {
        assert!(
            ebnf.contains(&format!("{user} ")) && ebnf.contains("params"),
            "对照失败：`{user}` 仍应引用 params"
        );
    }
}

// ============================================================
// D118：普查第二批 —— 具名嵌套定义 / 词法作用域示例 / 引号类型
// ============================================================

/// D118 根因：**具名定义不能嵌套**。
///
/// `task outer()` 内部再写 `task inner()` 会在 `task` 那一行报
/// `Failed to parse`；`fn inner()` 同样（`fn` 在**语句位置根本不是**定义关键字）。
///
/// 而 spec §6.1「词法作用域」原来正是用嵌套 `task` 举例 —— 整段不可解析。
#[test]
fn d118_named_definitions_cannot_be_nested() {
    for (name, src) in [
        (
            "nested_task",
            "task outer()\n  let y = 1\n  task inner()\n    print(y)\n  end\nend\n",
        ),
        (
            "nested_fn",
            "task outer()\n  fn inner()\n    print(1)\n  end\nend\n",
        ),
        (
            "nested_task_toplevel_let",
            "let x = 1\n\ntask outer()\n  task inner()\n    print(x)\n  end\nend\n",
        ),
    ] {
        assert!(
            ParserV3::compile(src).is_err(),
            "{name}: 若具名**嵌套**定义已支持，则本测试失败 —— \
             需把该能力从 D118 移除，并恢复 spec §6.1 的原写法"
        );
    }

    // 对照 1：顶层具名定义可用（所以缺口是「嵌套」不是「具名定义」）
    assert!(ParserV3::compile("task outer()\n  print(1)\nend\n").is_ok());
    // 对照 2：闭包可以出现在 task 体内 —— 嵌套作用域的正确写法
    assert!(
        ParserV3::compile(
            "task outer()\n  let y = 1\n  let inner = fn() print(y) end\n  inner()\nend\n"
        )
        .is_ok()
    );
}

/// D118 缺口：**具名 `fn` 定义**完全不存在 —— 语句位置只认 `task`。
///
/// ⚠ spec 从未承诺它（`fn` 在 spec 里只出现在未实现的 trait 方法上下文），
/// 所以这是**能力缺口**而非文档错误。钉住它是为了：日后若实现了，
/// 有人能立刻发现并更新本文件。
#[test]
fn d118_named_fn_definitions_do_not_exist() {
    assert!(
        ParserV3::compile("fn f()\n  return 1\nend\n").is_err(),
        "若具名 `fn` 定义已支持，则本测试失败 —— 需更新本文件与 CHANGELOG D118"
    );
    // 对照：`fn` 作为**闭包表达式**完全可用（这是语言里唯一的 `fn` 用法）
    assert!(ParserV3::compile("let f = fn() return 1 end\nprint(f())\n").is_ok());
    assert!(
        ParserV3::compile("task t()\n  let f = fn() return 1 end\n  print(f())\nend\n").is_ok()
    );
}

/// D118：spec §14.2 的 `closure` 产生式是
/// ```text
/// closure = "fn" "(" params ")" ( expr | "{" { statement } "}" ) ;
/// ```
/// —— **没有 `end`**，体是单个表达式。修复前这种形式会**静默吞掉后续顶层语句**：
///
/// ```mora
/// print("A:start")
/// let f = fn() 1          -- 同行的省略式闭包
/// print("B:after-def")    -- 这些语句此前全部消失
/// print("C:call=", f())
/// print("D:end")
/// ```
///
/// 修复前只输出 `A:start`，**exit 0、零诊断** —— `print` 被当成闭包体语句吞掉。
/// 根因：`emit_block_w` 的块终止集（`is_block_end`）不含 Newline，且 `end`
/// 可选（v0.87 为支持行尾/实参位置而允许省略）。
///
/// 修复：同行 + 表达式起点 → 走 `emit_expr_w`（只吃一个表达式）。
#[test]
fn d118_omitted_end_closure_works_and_does_not_swallow_statements() {
    let src = "print(\"A:start\")\nlet f = fn() 1\nprint(\"B:after-def\")\nprint(\"D:end\")\n";
    let (_func, _wits) =
        ParserV3::compile(src).expect("spec §14.2 的 `fn(params) expr` 必须可解析");
    // 能解析不够 —— 还要能**执行**，且末值取到的是闭包外的语句。
    // 修复前 `let f = fn() 1` 会把后面三条都吞进闭包，顶层什么都不剩。
    let got = run_mir_last_value(src);
    assert!(
        !got.contains("Err"),
        "省略 `end` 的闭包必须可执行，实际: {got}"
    );

    // 对照组 1：`fn(x) x + 1` 闭包**返回值**正确（捕获参数 x）
    assert_eq!(
        run_mir_last_value("let f = fn(x) x + 1\nf(2)\n"),
        "Float(3.0)",
        "`fn(params) expr` 必须返回 expr 的值"
    );
    // 对照组 2：显式 `end` 的 `return` 形式不受影响（`return` 不是表达式）
    assert_eq!(
        run_mir_last_value("let f = fn() return 1 end\nf()\n"),
        "Float(1.0)",
        "`return` 形式必须照旧工作（它不在 emit_expr_w 里，靠本测试防回归）"
    );
    // 对照组 3：换行多语句闭包**不能**被降成「只执行第一条」
    assert_eq!(
        run_mir_last_value("let f = fn()\n  let a = 2\n  a + 40\nend\nf()\n"),
        "Float(42.0)",
        "换行块闭包仍是多语句；只看「首 token 是 Identifier」会把它误降成单表达式"
    );
    // 对照组 4：作函数实参（`)` 终止）与行尾（EOF 终止）两种省略式都可用
    assert!(ParserV3::compile("print([1, 2].map(fn(x) x + 1 end))\n").is_ok());
    assert!(ParserV3::compile("let f = fn(x) x + 1\nprint(f(2))\n").is_ok());

    // ⚠ `d110_function_parameter_annotations_are_not_supported` 里那条对照断言
    //   写的是 `ParserV3::compile("let g = fn(a) a + 1\nprint(g(1))\n").is_ok()` ——
    //   它在 D118 之前**一直通过**，但通过的原因正是缺陷本身：print 被吞进闭包体。
    //   「能编译」与「语义正确」在这里是两回事，那条断言只钉住了前者。
    //   这里补上执行层的钉子。
    assert_eq!(
        run_mir_last_value("let g = fn(a) a + 1\ng(1)\n"),
        "Float(2.0)",
        "省略 end 的闭包必须真能求值"
    );
}

/// D118：词法作用域**确实**支持 —— 靠闭包捕获，且**可求值**。
///
/// 这一条是 §6.1 改写的**前提**：如果闭包捕获不可用，就不能断言「实现对、
/// spec 错」，而得反过来怀疑实现。`ParserV3::compile` 只能证明能解析，
/// 所以这里另跑一次真实执行（`run_mir`）确认捕获的是**运行时值**。
#[test]
fn d118_closures_capture_lexical_values() {
    // 解析层：spec §6.1 改写后的原文
    let spec_6_1 = "let x = 10\n\nlet make = fn(y)\n  return fn()\n    x + y\n  end\nend\n\nlet inner = make(20)\ninner()\n";
    ParserV3::compile(spec_6_1).expect("spec §6.1 的闭包示例必须可解析");

    // 执行层：闭包捕获的是**运行时值**（10 + 20 = 30），不是 0 也不是编译期常量。
    // ⚠ 本语言所有裸数字字面量都是 `Value::Float`（D98：`type_of(1)` = `float`）。
    let got = run_mir_last_value(spec_6_1);
    assert_eq!(
        got, "Float(30.0)",
        "闭包应捕获外层 x=10 与参数 y=20 求得 30"
    );
}

/// D118：§16.4 JSON 示例里 `json.parse('…')` 的**单引号**会被当成 char 字面量。
///
/// `'{"name": "Alice"}'` → `Char literal must contain exactly one character`。
/// 单引号在 Mora 里是**字符**字面量（§16.3 的 `'\n'` 就用它），JSON 必须用双引号。
#[test]
fn d118_json_needs_double_quotes_not_single() {
    assert!(
        ParserV3::compile("let d = json.parse('{\"name\": \"Alice\"}')\nprint(d)\n").is_err(),
        "单引号形式应被拒（它是被误解为超长字符字面量）；\
         若本测试失败，说明引号语义变了，需更新 spec §16.4"
    );
    // 对照：双引号 + 反斜杠转义可用，且 spec §16.4 现在的写法必须可解析
    assert!(
        ParserV3::compile("let d = json.parse(\"{\\\"name\\\": \\\"Alice\\\"}\")\nprint(d)\n")
            .is_ok()
    );
    // 对照：§16.3 的 char 字面量单引号是**对的**，不能被「双引号化」误伤
    assert!(ParserV3::compile("let c = 'a'\nprint(c)\n").is_ok());
}

/// 跑一段 Mora 并返回**末表达式**的运行时值（库层观察，`mora run` 不打印它）。
fn run_mir_last_value(src: &str) -> String {
    use mora::interpreter::Interpreter;
    use mora::mir::vm::run_mir;
    use std::sync::Arc;

    let (func, _w) = ParserV3::compile(src).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("Err({e})"),
    }
}
