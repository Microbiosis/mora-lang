//! v0.104.6 D122：**执行结果级**的编译路径差分普查。
//!
//! D120 证明两条编译路径会分叉，而单点测试只能覆盖「想到的点」。
//! 本探针遍历 `tests/fixtures/e2e/*.mora`，逐条比对
//! `ParserV3::compile`（直出）与 `cli::compile_and_opt`（9 层管线）
//! 产物的**执行结果**（`run_mir` 末值）。
//!
//! ## ⚠ 比对口径：必须用 `Display`，**不能**用 `Debug`
//!
//! `Value::Dict` 内部是 `HashMap<String, Value>`，而 `Debug` for `HashMap`
//! **不排序**（`Display for Value` 才排序 —— 见 `dict_determinism.rs` 的 7 条）。
//! 第一版用 `format!("{v:?}")`，于是 47 条里有 2 条报「分叉」，实为**伪分叉**：
//! 那两条的 Dict 内容逐键相同，只是 `Debug` 迭代序不同。
//! 两处内容完全一致的 Dict 被判不等 —— 是判据的错，不是产品的错。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use std::path::Path;
use std::sync::Arc;

fn run_func(func: mora::mir::MirFunction) -> String {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v}"), // Display —— Dict 会按 key 排序
        Err(e) => format!("Err({e})"),
    }
}

fn direct(src: &str) -> String {
    match mora::parser_v3::ParserV3::compile(src) {
        Ok((f, _)) => run_func(f),
        Err(e) => format!("COMPILE-ERR: {}", e.lines().next().unwrap_or("")),
    }
}

fn pipelined(src: &str) -> String {
    match mora::cli::compile_and_opt(src, None) {
        Ok((f, _)) => run_func(f),
        // ⚠ 归一化：`compile_and_opt` 的 Err 是 `String` 的 **Debug**，带引号
        //   （`"Failed to parse at line 1"`），而 `ParserV3::compile` 的
        //   `e.to_string()` 不带。不归一化会把「两条路径都失败」误报成分叉。
        Err(e) => format!("COMPILE-ERR: {e}"),
    }
}

#[test]
fn d122_every_e2e_fixture_gives_identical_results_on_both_compile_paths() {
    let files = e2e_fixtures();
    assert!(
        files.len() >= 40,
        "对照组失败：e2e fixture 只找到 {} 个（若 fixture 被移动，删掉本测试）",
        files.len()
    );

    let mut diverge = Vec::new();
    for p in &files {
        let src = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("读 {}: {e}", p.display()));
        let d = direct(&src);
        let q = pipelined(&src);
        if d != q {
            diverge.push(format!(
                "{}:\n      direct    = {}\n      pipelined = {}",
                p.file_name().unwrap().to_string_lossy(),
                d,
                q
            ));
        }
    }

    assert!(
        diverge.is_empty(),
        "两条编译路径的执行结果分叉（{} 条）:\n  {}",
        diverge.len(),
        diverge.join("\n  ")
    );
}

/// 反向对照：比对口径**必须**用 `Display` —— `Debug` 会制造伪分叉。
///
/// `Value::Dict` 内部是 `HashMap`：`Debug` for `HashMap` **不排序**，
/// `Display for Value` 才排序（`dict_determinism.rs` 的 7 条钉的是 Display）。
/// 47 条 e2e fixture 里有 2 条含多键 Dict，用 `Debug` 比对就会误报分叉 ——
/// 那两条的 Dict 内容逐键完全相同。
///
/// 本条把「用错口径」这个坑钉住：若哪天 `Value::Dict` 换了容器而 Display
/// 不再排序，这条会先于其它测试报警。
#[test]
fn d122_dict_display_is_sorted_but_debug_is_not_the_comparison_basis() {
    use mora::value::Value;
    use std::collections::HashMap;

    let mut map: HashMap<String, Value> = HashMap::new();
    for (i, k) in ["alpha", "beta", "gamma", "delta", "epsilon"]
        .iter()
        .enumerate()
    {
        map.insert((*k).to_string(), Value::Float(i as f64));
    }
    let d = Value::Dict(map);

    // Display：必须按 key 字母序（这才是「确定性」的对外契约）
    assert_eq!(
        d.to_string(),
        "{alpha: 0.0, beta: 1.0, delta: 3.0, epsilon: 4.0, gamma: 2.0}",
        "Dict 的 Display 必须键序排序（dict_determinism.rs 同契约）"
    );

    // Debug 不保证排序 —— 所以**不能**拿它当跨路径比对口径。
    // 本断言只钉住「Debug 与 Display 是两回事」这个事实，不钉住 Debug 的具体序
    // （它依赖 HashMap 实现，可能随标准库变化）。
    let dbg = format!("{d:?}");
    assert_ne!(
        dbg,
        d.to_string(),
        "若 Debug 与 Display 变成了同一形态，说明 Dict 容器变了 —— \
         重新评估「用 Display 作比对口径」这个前提是否还成立"
    );
}

/// 内联差分用例：覆盖 e2e fixture **没覆盖到**的形态。
///
/// 47 个 fixture 里的 `solve { … }` 全在**语句位置**（`rel_*.mora`），
/// 唯独没有 D120 那种 `let r = solve { … }` 的**绑定形态**。
/// 若只跑 fixture，撤掉 D120 的修复这条测试**照样全绿**。
///
/// 本组把「值产生型块构造出现在 `let` 右值」逐个补齐 —— 这正是
/// `node_result_reg_of` 决定成败的那条路径。
#[test]
fn d122_inline_let_bound_block_forms_agree_across_paths() {
    let cases: &[(&str, &str)] = &[
        (
            "solve_binding",
            "let r = solve { unify(1, 1) }\nlet tail = 5\ntail\n",
        ),
        (
            "solve_with_query_var",
            "let r = solve { unify(?X, 1) }\nlet tail = len([1, 2])\ntail\n",
        ),
        (
            "with_config",
            "let w = with model = \"m\"\n  print(1)\nend\nlet tail = 6\ntail\n",
        ),
        (
            "handle_expr",
            "let h = handle ask { 1 } { 2 }\nlet tail = h * 2\ntail\n",
        ),
        (
            "if_expr",
            "let v = if 1 == 1 then 3 else 4 end\nlet tail = v + 1\ntail\n",
        ),
        (
            "match_expr",
            "let m = match 2 { 1 => 10 _ => 20 }\nlet tail = m + 1\ntail\n",
        ),
    ];

    let mut diverge = Vec::new();
    for (name, src) in cases {
        let d = direct(src);
        let q = pipelined(src);
        if d != q {
            diverge.push(format!(
                "{name}:\n      direct    = {d}\n      pipelined = {q}"
            ));
        }
        // 还要确保两条路径都**真的**取到了值（而非同为「什么都没执行」）
        assert!(
            !d.contains("Nil") && !d.starts_with("Err"),
            "{name}: 直出路径应产出值，实际 {d}"
        );
    }
    assert!(
        diverge.is_empty(),
        "内联用例在两条路径上分叉（{} 条）:\n  {}",
        diverge.len(),
        diverge.join("\n  ")
    );
}

/// D123：D118 / D119 的修复在**两条路径上都生效**（不是「半个修复」）。
///
/// D118（省略 `end` 的闭包）与 D119（跨行 `|>`）都改在 `emit.rs`，而
/// 9 层管线的输入正是 `ParserV3` 产出的 `MirWitness` —— 当时只**推理**
/// 得出「修复会覆盖两条路径」，本轮**实测**确认。
///
/// 这些形态 e2e fixture 全都没覆盖（fixture 里 `fn(x) … end` 都带 `end`、
/// 管道都写成单行），故必须内联。
#[test]
fn d123_d118_d119_fixes_hold_on_both_compile_paths() {
    let cases: &[(&str, &str)] = &[
        // ── D118：省略 `end` 的同���闭包（修复前会吞掉后续语句）──
        ("d118_inline_closure", "let f = fn() 1\nf()\n"),
        ("d118_inline_closure_1arg", "let g = fn(a) a + 1\ng(2)\n"),
        ("d118_after_stmt", "let f = fn() 1\nlet tail = 9\ntail\n"),
        ("d118_with_return_form", "let f = fn() return 1 end\nf()\n"),
        (
            // 嵌套闭包 + **分两步**调用：可用。
            // 注意「一步」`make(5)()` 不行 —— 见下方「不支持连续调用」那条。
            "d118_nested_with_end",
            "let make = fn(y) fn() y + 1 end end\nlet g = make(5)\ng()\n",
        ),
        // ── D119：跨行 `|>`（修复前整段 parse error）──
        (
            "d119_pipe_map_filter",
            "let xs = [1, 2, 3]\n  |> map(fn(x) x * 2 end)\n  |> filter(fn(x) x > 2 end)\nxs\n",
        ),
        (
            "d119_pipe_string",
            "let r = \"hello world\"\n  |> upper()\n  |> split(\" \")\nr\n",
        ),
        (
            "d119_pipe_mixed",
            "let r = \"hi\" |> upper()\n  |> split(\" \")\nr\n",
        ),
        // 对照组：单行管道与裸标识符管道（D119 不得改变原语义）
        ("d119_pipe_single_line", "let r = \"hi\" |> upper()\nr\n"),
        (
            "d119_pipe_bare_ident",
            "let d = fn(x) x * 2\nlet r = 5 |> d\nr\n",
        ),
        // 对照组：显式 `end` 的多语句闭包（同行→expr 修复不得把它降成单表达式）
        (
            "d118_multiline_block_closure",
            "let f = fn()\n  let a = 2\n  a + 40\nend\nf()\n",
        ),
    ];

    let mut diverge = Vec::new();
    for (name, src) in cases {
        let d = direct(src);
        let q = pipelined(src);
        if d != q {
            diverge.push(format!(
                "{name}:\n      direct    = {d}\n      pipelined = {q}"
            ));
            continue;
        }
        // 且两条路径都必须**真的求出了值**（而不是同为「什么都没执行」）
        assert!(
            !d.starts_with("COMPILE-ERR") && !d.starts_with("Err"),
            "{name}: 两条路径都应成功求值，实际 {d}"
        );
        assert!(
            !d.contains("Nil") || !name.contains("inline_closure_1arg"),
            "{name}: 末值不应是 Nil（修复回退的症状），实际 {d}"
        );
    }
    assert!(
        diverge.is_empty(),
        "D118/D119 的形态在两条路径上分叉（{} 条）:\n  {}",
        diverge.len(),
        diverge.join("\n  ")
    );
}

/// D123 查明的能力缺口： **`f(…)(…)` 连续调用不支持**（但**明确报错**，非静默）。
///
/// ```mora
/// let mk = fn(x) fn(y) x + y end end
/// print(mk(1)(2))      -- ✗ Parse error: Expected ')' at line 2
/// let g = mk(1)
/// print(g(2))          -- ✓ 分两步可行
/// ```
///
/// 根因：`emit_call_tail_w` 的后缀链只处理 `.`（方法）与 `[`（索引），
/// **没有 `(`** 分支去消费「对调用结果再次调用」。
///
/// 判定为**能力缺口**而非缺陷：spec §14.2 的 EBNF 里 `call` 的被调用者
/// 只能是 `IDENTIFIER`（**无 `postfix` 产生式**），`expr` 联合式也没有
/// 「后缀链」那一项。故 spec 从未承诺此形态，实现报错**符合 spec**。
///
/// 本条钉住的是「**必须报错**」—— 若哪天有人加了 `(` 分支却处理不周，
/// 这条会从「拒绝」翻成「成功」，提醒同步更新 spec 与 CHANGELOG。
#[test]
fn d123_consecutive_call_chaining_is_rejected_on_both_paths() {
    // 一��调用：明确报错
    let chained = "let mk = fn(x) fn(y) x + y end end\nmk(1)(2)\n";
    let d = direct(chained);
    let q = pipelined(chained);
    assert!(
        d.starts_with("COMPILE-ERR") && q.starts_with("COMPILE-ERR"),
        "`f(…)(…)` 当前应在两条路径上都被**拒绝**（spec 无 postfix 产生式）; \
         direct={d} pipelined={q}"
    );
    assert_eq!(d, q, "两条路径的拒绝理由必须一致（错误串已归一化）");

    // 对照组 1：分两步调用可用，且两条路径取值一致
    let stepwise = "let mk = fn(x) fn(y) x + y end end\nlet g = mk(1)\ng(2)\n";
    assert_eq!(
        direct(stepwise),
        "3.0",
        "分两步调用必须可用（它是连续调用的可用等价写法）"
    );
    assert_eq!(direct(stepwise), pipelined(stepwise));

    // 对照组 2：`f(g(…))` 作为**实参**的嵌套是支持的（不是后缀链）
    let nested_arg = "let a = fn(x) x end\nlet b = fn(x) x * 2 end\na(b(3))\n";
    assert_eq!(
        direct(nested_arg),
        "6.0",
        "`f(g(…))` 是实参嵌套，应与后缀链 `f(…)(…)` 区分开"
    );
    assert_eq!(direct(nested_arg), pipelined(nested_arg));
}

/// 「两条路径一致」**不等于**「值正确」—— 它们可以**一致地算错**。
///
/// 证据：`pipeline_equivalence.rs` 的「深层嵌套闭包」用例原为
/// `let outer = fn(a) fn(b) a + b end end` + `outer(5)(10)`。
/// 修复前两条路径都能编译、都给出末值 `10`（而非 `5 + 10 = 15`）——
/// **exit 0、零诊断**。于是「两条路径等价」这条断言**一直是绿的**，
/// 却从未测到「深层嵌套闭包」的语义：D123 把该形态改为明确报错后，
/// 这条既有测试才暴露出来。
///
/// 本条只钉**语义**：分两步调用必须得 `15`。路径等价由别处负责。
#[test]
fn d123_deeply_nested_closure_gives_the_right_answer_not_just_agreeing() {
    let src = "let outer = fn(a) fn(b) a + b end end\nlet g = outer(5)\ng(10)\n";
    assert_eq!(direct(src), "15.0", "嵌套闭包分两步调用必须得 5 + 10 = 15");
    // 路径一致是**必要不充分**条件：两条都算 10 也「一致」，但那是错的。
    assert_eq!(direct(src), pipelined(src));
}

fn e2e_fixtures() -> Vec<std::path::PathBuf> {
    let dir = Path::new("tests/fixtures/e2e");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("读 tests/fixtures/e2e")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "mora").unwrap_or(false))
        .collect();
    files.sort();
    files
}
