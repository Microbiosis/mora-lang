//! v0.104.6 D119：跨行管道 `|>` —— spec 的**规范形态**此前完全不可解析。
//!
//! ## 缺陷
//!
//! `emit_pipe_w` 的循环是 `while self.match_token_exact(TokenType::Pipe)`，
//! **不跳换行**。而 spec 的管道示例一律是跨行的：
//!
//! - §2.1  `[1,2,3] |> map(fn(x) x*2 end)`
//! - §7.6  `let result = "hello world"` 换行 `|> upper()` 换行 `|> split(" ")` 换行 `|> map(…)` 换行 `|> filter(…)`
//! - §18.1 `router` 换行 `|> route("POST", …)` 换行 `|> listen(…)`
//! - §18.2 `server` 换行 `|> tool(…)` 换行 `|> serve()`
//!
//! 实测（修复前）：单行 `"hello world" |> upper()` → `HELLO WORLD` ✓；
//! 换成 spec 的跨行形态 → **`Failed to parse at line 2`**。
//!
//! ## 修法与它必须防住的那件事
//!
//! 跳过的换行在**没看到 `|>` 时必须回退**。否则下一行若是普通语句
//! （`let a = 1` 换行 `print(6)`），被吞掉的换行就是语句分隔符 ——
//! 两条语句会被并成一条。本文件的对照组专门盯这一条。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn last_value(src: &str) -> String {
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

/// D119 主判据：spec §7.6 的**逐字形态**必须可解析**且求值正确**。
///
/// 只测「能解析」不够 —— 修复前的失败是 parse error，但同类的「跳过换行」
/// 写法可以做到解析通过而语义全错（吞掉后续语句）。故落到执行层。
#[test]
fn d119_spec_section_7_6_multiline_pipe_runs() {
    // spec §7.6 的管道段（`upper` / `split` 均已实现；`map` / `filter` 在方法表里）
    let src = "let xs = [1, 2, 3]\n  |> map(fn(x) x * 2 end)\n  |> filter(fn(x) x > 2 end)\nxs\n";
    ParserV3::compile(src).expect("spec §7.6 的跨行管道必须可解析");
    assert_eq!(
        last_value(src),
        "List([Float(4.0), Float(6.0)])",
        "跨行管道必须逐级求值：[1,2,3] |> map(*2) => [2,4,6] |> filter(>2) => [4,6]"
    );
}

/// 管道每级都可跨行（不止第一处），且 §7.6 的字符串形态同样可用。
#[test]
fn d119_multiline_pipe_accepts_string_receiver() {
    let src = "let r = \"hello world\"\n  |> upper()\n  |> split(\" \")\nr\n";
    ParserV3::compile(src).expect("字符串接收者的跨行管道必须可解析");
    assert_eq!(
        last_value(src),
        "List([String(\"HELLO\"), String(\"WORLD\")])",
        "\"hello world\" |> upper() |> split(\" \") 应得 [HELLO, WORLD]"
    );
}

/// 管道起首行可与接收者同行、后续跨行（混排也要能解析）。
#[test]
fn d119_pipe_may_start_inline_then_continue_across_lines() {
    let src = "let r = \"hello world\" |> upper()\n  |> split(\" \")\nr\n";
    ParserV3::compile(src).expect("首个 `|>` 同行、其余跨行的混排形态必须可解析");
    assert_eq!(
        last_value(src),
        "List([String(\"HELLO\"), String(\"WORLD\")])"
    );
}

/// ⚠ **对照组（本缺陷修复最该防住的那件事）**：跳过的换行不得被误当管道。
///
/// 下一行是普通语句时，那个换行就是**语句分隔符**。若换行之后**不是** `|>`，
/// 它不能把两条语句并成一条。
///
/// 判据取**末值**：若两条语句被并成一条，末值就不再是最后一个表达式。
///
/// ℹ 归因说明（别被名字骗了）：本条**不是**在测 `emit_pipe_w` 里那行
/// `self.current = saved` 回退。反向验证（把该行摘掉重跑全量 8 条 + 3 个
/// CLI 样例）**结果完全一致** —— 该回退在当前调用结构下是**不可观测的**：
/// `emit_pipe_w` 由块语句循环调用，而后者末尾本来就有
/// `while self.match_token(&[TokenType::Newline]) {}`，换行迟早被吃掉。
/// 保留那行是为了让 `emit_pipe_w` 只在**真的**用到换行时消费它（局部性），
/// 而不是因为它被测到了。本条测的是**属性**「语句分隔完好」，不是那行代码。
#[test]
fn d119_newlines_do_not_merge_following_statements() {
    // `let a = 1` 之后没有 `|>` —— 那个换行必须原样退回给块语句循环
    let src = "let a = 1\nlet b = a + 1\nb\n";
    ParserV3::compile(src).expect("普通多语句必须可解析");
    assert_eq!(
        last_value(src),
        "Float(2.0)",
        "`let a = 1` 后的换行若被吞，`let b = a + 1` 会被并进上一条，末值不是 2.0"
    );

    // 管道**之后**的换行同样不能把下一条语句并进管道表达式
    let src2 = "let d = fn(x) x * 2\nlet r = 5 |> d\nlet z = 100\nz\n";
    ParserV3::compile(src2).expect("管道后接普通语句必须可解析");
    assert_eq!(
        last_value(src2),
        "Float(100.0)",
        "管道后的换行若被吞，`let z = 100` 会被并进管道表达式"
    );
}

/// 对照组：单行管道行为**不变**（本修复只放宽换行，不得改变原语义）。
#[test]
fn d119_single_line_pipe_is_unchanged() {
    // 方法形态 `x |> f(args)` → `x.f(args)`（v0.104.2 的就地改写）
    assert_eq!(
        last_value("let r = \"hi\" |> upper()\nr\n"),
        "String(\"HI\")",
        "单行管道必须与修复前一致"
    );
    // 裸标识符形态 → 值应用 `f(x)`（§7.6 `5 |> double`）
    assert_eq!(
        last_value("let double = fn(x) x * 2\nlet r = 5 |> double\nr\n"),
        "Float(10.0)",
        "裸标识符管道走值应用，不得被改成方法调用"
    );
}

/// 对照组：没有 `|>` 的源码，`emit_pipe_w` 的净效果必须是**零**。
///
/// 这条直接盯「回退」这个动作：本函数对不含 `|>` 的表达式不该有任何影响。
#[test]
fn d119_sources_without_pipe_are_unaffected() {
    // 裸函数调用 + 后续语句：换行分隔必须完好
    let src = "let a = len([1, 2])\nprint(a)\nlet b = 1\nb\n";
    ParserV3::compile(src).expect("无管道源码必须照常解析");
    assert_eq!(last_value(src), "Float(1.0)");
}

/// 反向对照两则：别的换行容忍不能被本修复「顺手」扩进来。
///
/// 1. `|`（union 标注用的 `Or` token）**不是** `|>`。若判据写成「跳过换行后
///    只要不是 `End` 就继续」，union 标注就会被管道吞掉。
#[test]
fn d119_bare_or_union_annotation_is_not_treated_as_a_pipe() {
    let src = "let x: string | number = 1\nprint(x)\nprint(\"after\")\n";
    let (_f, wits) = ParserV3::compile(src).expect("union 标注必须照常可解析");
    // 后续语句仍在顶层（没被并进标注表达式）
    assert_eq!(
        last_value("let x: string | number = 1\nlet y = 7\ny\n"),
        "Float(7.0)",
        "union 标注后的换行必须退回给块语句循环"
    );
    let _ = wits;
}

/// 跨行**二元运算**（`let b = a` 换行 `+ 1`）不在本修复范围内。
///
/// 钉住它是为了标明边界：本修复只放宽 `|>`，不是「让任意表达式跨行」。
/// 若日后实现了跨行二元运算，这条应更新而不是被遗忘。
#[test]
fn d119_multiline_binary_operator_is_still_not_supported() {
    assert!(
        ParserV3::compile("let b = a\n  + 1\nprint(b)\n").is_err(),
        "跨行二元运算若已支持，则本测试失败 —— 需更新本文件与 CHANGELOG D119"
    );
}

/// D119 的一致性钉子：**真的打开 `docs/mora-spec.md`**，取出 §7.6「管道」那一段
/// ```` ```mora ```` 块并逐字交给解析器。
///
/// 为什么要读文件而不是硬编码：D110 的教训 —— 只硬编码几段源码、从不打开
/// spec 的「一致性测试」不是一致性测试，spec 改回去它照样全绿。
/// spec 改写、本文件不动，判据必须跟着变。
///
/// ℹ §7.6 的块里还含 `let double = fn(x) return x * 2 end` 与 `5 |> double`
/// （裸标识符 → 值应用），一并覆盖。
#[test]
fn d119_spec_section_7_6_pipe_block_parses_verbatim() {
    let text = std::fs::read_to_string("docs/mora-spec.md")
        .expect("读 docs/mora-spec.md（若 spec 未纳入版本控制，此文件仍应在磁盘上）");

    // 定位 §7.6 标题，再取其后的第一个 ```mora 块
    let sec = text
        .find("### 7.6")
        .unwrap_or_else(|| panic!("spec 里找不到 §7.6 管道"));
    let open_rel = text[sec..]
        .find("```mora")
        .unwrap_or_else(|| panic!("§7.6 之后没有 ```mora 块"));
    let start = sec + open_rel + "```mora".len();
    let end_rel = text[start..]
        .find("```")
        .unwrap_or_else(|| panic!("§7.6 的 ```mora 块未闭合"));
    let block = text[start..start + end_rel].trim();

    assert!(
        block.contains("|>"),
        "对照组失败：§7.6 的块里应仍含 `|>`（若 spec 改写，删掉本测试）"
    );
    ParserV3::compile(block).unwrap_or_else(|e| {
        panic!("spec §7.6 的管道示例必须**逐字**可解析，实际块:\n{block}\n\n错误: {e}")
    });
}
