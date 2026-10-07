//! v0.104.6 D92b：9 层差分回落的**全语言普查** —— 14 类构造永久走旧路径。
//!
//! 背景见同目录 `nine_layer_block_forms.rs`。本文件把「哪些构造会静默回落」
//! 这件事本身做成**可执行清单**，而不是散落在注释里的印象。
//!
//! ## 为什么值得锁
//!
//! 回落对用户**不可见**（只往 stderr 打一行摘要），而 D57 的注释声称 9 类构造
//! 「不再因这类冗余而永久放弃 DAG 分析 / CSE / 贪心重写」。实测该声称**只对
//! `eval` 成立**，其余 8 类仍在回落（另加 5 类 D57 从未提过的）。
//!
//! ## 清单来源
//!
//! 逐条用**真实 CLI** 跑最小程序，判据＝stderr 是否出现「差分失败」。
//! 形态取自 spec §14.2 的 statement 产生式 + TEA 独立声明 + 字面量/表达式形态。
//! 注意两处**与 spec EBNF 不一致**的实现事实：
//! - `handle` 实为块形态 `handle E { body } { handler }`，
//!   而 spec §14.2 写的是 `handle IDENT "on" IDENT "->" {stmt} "end"`；
//! - `model` / `msg` / `update` 是**无花括号**的 `name … end` 形态。

use mora::mir::pipeline::run_pipeline;
use mora::parser_v3::ParserV3;

/// 最小复现源码 → 是否应当回落。
///
/// `true` = 当前**必须**回落（差分失败）；`false` = 当前差分通过。
const CENSUS: &[(&str, &str, bool)] = &[
    // ── 嵌套体少一条尾部 `Return`（1/2）──
    //
    // v0.104.6 D315：这 5 条**重新**改为 `false`（差分对齐后实测通过）。
    //
    // 这是 D276 的同一处改动。D276 改完后随即撤销，理由与本组无关：
    // `nested_diffs` 的「错位」当时是**唯一**能挡住寄存器级破损的护栏
    // （差分按设计不比较寄存器号），而对齐后 `rel_*` 不再回落、9 层路径的
    // 寄存器破损直达用户（`Runtime error (MIR): … references register 4 but
    // the function only has 1 register(s)`）。
    //
    // **D314 修好了那个前提**（`max_reg_in_node` 的 `_ => 0` 漏算），D315
    // 据此重新对齐。本组的行为验证：10 种块形态 × 3 个优化档位 = **30 组合**，
    // 强制走管线 vs 回落 `emit.rs` 输出**逐行相同**（含嵌套、值位置、
    // 多语句、后接 `for` 等形状）。D92 原文警告的「块里的语句会不执行」
    // 在 30 个组合里一次都没复现。常驻护栏见 `tests/nine_layer_unblocked.rs`
    // 的 `block_forms_behave_identically_on_both_paths`。
    (
        "observe",
        "observe trace \"t\" do\n  print(1)\nend\n",
        false,
    ),
    ("span", "span \"s\" do\n  print(1)\nend\n", false),
    ("parallel", "parallel\n  print(1)\nend\n", false),
    (
        "prompt_section",
        "prompt \"p\" do\n  print(1)\nend\n",
        false,
    ),
    (
        "document_section",
        "document \"d\" do\n  print(1)\nend\n",
        false,
    ),
    // ── 类别缺口（2/2，`Transaction` 等在整个降级链里不存在）──
    ("worker", "worker w do\n  print(1)\nend\n", true),
    ("transaction", "transaction\n  print(1)\nend\n", true),
    // ── 无 handler 的裸 perform ──
    (
        "perform_bare",
        "let g = \"i\"\ng = perform Ai(\"x\")\nprint(g)\n",
        true,
    ),
    // ── 声明形态 ──
    //
    // v0.104.6 D315：`msg`/`struct`/`enum` **重新**改为 `false`（D315 差分
    // 对齐后实测通过）。
    //
    // v0.104.6 **D412**：`model` 与 `tea_standalone` 也**改为 `false`** ——
    // 真因不是 D95 记档的「5 个分支不补 `Const(dst, Nil)`」，而是
    // `model` 字段默认值在**入口**就被丢弃
    // （`let (dreg, _dw) = self.emit_expr_w()?;` 把 `_dw` 扔了
    // ⇒ `WitnessKind::ModelDef` 没有 `defaults`）。
    // 补上后两者实测均通过差分。
    //
    // 行为验证：`msg` / `struct` / `enum` 各 2–3 种形状 × 3 个优化档位 =
    // **30 组合**，强制走管线 vs 回落 `emit.rs` 输出**逐行相同**。
    // D412 的 2 条翻转**同向**（回落 → 通过），无一条反向退化。
    (
        "model",
        "model Counter\n  count: number = 0\nend\nprint(1)\n",
        false,
    ),
    (
        "msg",
        "msg CounterMsg\n  Increment\n  Decrement\nend\nprint(1)\n",
        false,
    ),
    ("struct", "struct P\n  x: number\nend\nprint(1)\n", false),
    ("enum", "enum E\n  A\n  B\nend\nprint(1)\n", false),
    (
        "tea_standalone",
        "model C\n  count: number = 0\nend\nmsg M\n  Inc\nend\nupdate(msg, model)\n  model\nend\nprint(1)\n",
        false,
    ),
    // ── D57 声称已修且在**嵌套位置**确实通过的那一类 ──
    // ⚠ `eval` 的回落是**上下文相关**的：D94 实测 `eval(1 + 1)` 裸顶层
    // → 回落（`pipeline_mir=0`，管线对裸 eval **一条指令都产不出**）；
    // 而嵌在 `print`/`let`/列表/二元运算里 → 差分通过。
    // 下面这条用的是**嵌套**形态（与本表其余条目的写法一致）。
    ("eval", "print(eval(1 + 1))\n", false),
    // 裸顶层形态单独列一条 —— 这是真实管线缺口，D57 的说法对它不成立。
    ("eval_bare", "eval(1 + 1)\n", true),
    // ── 差分通过的主流形态（对照组）──
    ("let", "let x = 1\nprint(x)\n", false),
    ("assign", "let x = 1\nassign x = 2\nprint(x)\n", false),
    (
        "if",
        "let c = 1\nif c == 1 then\n  print(1)\nelse\n  print(2)\nend\n",
        false,
    ),
    (
        "if_elseif",
        "let c = 1\nif c == 1 then\n  print(1)\nelse if c == 2 then\n  print(2)\nelse\n  print(3)\nend\n",
        false,
    ),
    ("for", "for x in [1, 2, 3]\n  print(x)\nend\n", false),
    (
        "while",
        "let i = 0\nwhile i < 3\n  print(i)\n  i = i + 1\nend\n",
        false,
    ),
    (
        "match",
        "let v = 1\nlet r = match v {\n  1 => \"a\"\n  _ => \"b\"\n}\nprint(r)\n",
        false,
    ),
    (
        "match_guard",
        "let v = 1\nlet r = match v {\n  x when x > 0 => \"pos\"\n  _ => \"neg\"\n}\nprint(r)\n",
        false,
    ),
    ("with", "with model = \"m\"\n  print(1)\nend\n", false),
    (
        "handle",
        "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(g)\n",
        false,
    ),
    (
        "update",
        "update(msg, model)\n  model\nend\nprint(1)\n",
        false,
    ),
    (
        "task",
        "task t()\n  print(1)\nend\ntask main()\n  t()\nend\n",
        false,
    ),
    (
        "return",
        "task t()\n  return 1\nend\ntask main()\n  print(t())\nend\n",
        false,
    ),
    ("macro", "macro m(x)\n  print(x)\nend\nprint(1)\n", false),
    ("list_lit", "let xs = [1, 2, 3]\nprint(xs)\n", false),
    ("dict_lit", "let d = {a: 1, b: 2}\nprint(d)\n", false),
    ("template", "let n = 1\nprint(p\"v={n}\")\n", false),
    ("closure", "let f = fn(x) x + 1\nprint(f(1))\n", false),
    ("bigint", "let n = 123n\nprint(n)\n", false),
];

/// 普查断言：**逐条**核对，失败时同时打印实际与期望，并汇总全表。
#[test]
fn d92b_census_matches_measured_fallback_set() {
    let mut wrong = Vec::new();
    let mut fell_back = Vec::new();
    for (name, src, should_fall_back) in CENSUS {
        let (func, witnesses) = match ParserV3::compile(src) {
            Ok(v) => v,
            Err(e) => {
                wrong.push(format!(
                    "{name}: 源码编译失败 —— 清单里的形态可能已不存在: {e}"
                ));
                continue;
            }
        };
        let (result, _) = run_pipeline(&func, &witnesses);
        if result.differential_ok {
            if *should_fall_back {
                wrong.push(format!(
                    "{name}: 实测**通过**差分，清单记的是回落 —— 9 层管线可能已支持该形态，\
                     **D57/D92 的描述需同步更新**"
                ));
            }
        } else {
            fell_back.push(*name);
            if !*should_fall_back {
                wrong.push(format!(
                    "{name}: 实测**回落**，清单记的是通过 —— 管线对该形态退化了"
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "回落普查与实测不符（{} 条）：\n  - {}\n实测回落清单：{fell_back:?}",
        wrong.len(),
        wrong.join("\n  - ")
    );
}

/// 覆盖面自检：这张表不能悄悄变小。
#[test]
fn d92b_census_covers_both_outcomes() {
    let falls: Vec<&str> = CENSUS.iter().filter(|c| c.2).map(|c| c.0).collect();
    let passes: Vec<&str> = CENSUS.iter().filter(|c| !c.2).map(|c| c.0).collect();
    assert_eq!(
        CENSUS.len(),
        34,
        "普查表现在 {} 条 —— 若有意增删，请同步更新本断言与 CHANGELOG D92b/D94",
        CENSUS.len()
    );
    // v0.104.6 D276 曾把这两个阈值下调（差分对齐后回落项由 14 降到 6），
    // **已随 D276 的撤销一并回滚** —— 回落项回到 14。
    // v0.104.6 D315：D314 修好 9 层 rel 路径的 `n_regs` 破损后重新对齐，
    // 回落项由 14 降到 **6**（翻转 8 条，见 `CENSUS` 注释）。
    // v0.104.6 **D412**：`model` / `tea_standalone` 也走上管线 ⇒
    // 回落项 **6 → 4**，通过项 **28 → 30**。
    //
    // ⚠ 这里用**精确值**而非区间：本表是**静态 34 条**（上面已断言长度），
    // 条数完全确定，不存在扫描器静默失效的风险（D407 的教训不适用于此）。
    assert_eq!(
        falls.len(),
        4,
        "回落项应恰好 4 条（worker / transaction / perform_bare / eval_bare）。\
         当前 {falls:?} —— 若有增减，请同步更新本断言、`FALLBACK_REASONS` 表 \
         与 CHANGELOG 的回落清单。"
    );
    assert_eq!(
        passes.len(),
        30,
        "通过项应恰好 30 条（34 − 4）。当前 {passes:?}"
    );
}
