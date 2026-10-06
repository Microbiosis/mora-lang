//! 语法形态速查表 —— **写探针前先查这里**。
//!
//! ## 为什么有这个文件
//!
//! D136 / D137 / D138 我**连踩三次**同一个坑：探针源码本身语法非法，
//! 却据此解读 provider 行为：
//!
//! | 轮次 | 探针写法 | 我误读成 | 真相 |
//! |---|---|---|---|
//! | D136 | `fn g() … end` | 「补全 provider 坏了」 | 具名 `fn` **不支持**（D118 已查） |
//! | D136 | `task doer()` 定义后不调用 | 「`if` 在 task 内不执行」 | **正确行为**（`task` 是定义） |
//! | D136/D137 | `match v` ⏎ `1 -> print(1)` ⏎ … | 「foldingRange 的 match 有缺口」 | arm 体**不能跨行**（语法非法） |
//! | D138 | `match v { 1 -> {` ⏎ … ⏎ `}` ⏎ `}` | 「多行花括号 match 不折叠」 | 花括号 arm 体**不能是块**（语法非法） |
//!
//! 四次里有**三次**是「源码根本跑不通」。反复犯的根因是：**凭印象写语法，
//! 不先确认它能编译**。本文件把常用形态的**实测**可编译性固化下来。
//!
//! ## 怎么用
//!
//! 写新探针前，在下表里找对应形态 —— 若标记为「✗ 语法非法」，
//! **换一个形态**，别再据此解读任何 provider 的行为。
//!
//! ⚠ 本表是**实测快照**（v0.104.6）。语法演进后请同步更新，
//! 或直接跑 `cargo test --test mora_syntax_forms -- --nocapture` 复核。

use mora::parser_v3::ParserV3;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Form {
    /// 应当可编译
    Ok,
    /// 已知语法非法（附原因）
    Rejected,
    /// 已知缺口（能编译但功能受限），见注释
    Gap,
}

/// (形态名, 源码, 期望, 说明)
const FORMS: &[(&str, &str, Form, &str)] = &[
    // ── 函数 / 闭包 ────────────────────────────────���──────
    (
        "具名 task 定义",
        "task d()\n  print(1)\nend\n",
        Form::Ok,
        "语句位置的**唯一**具名定义形式",
    ),
    (
        "具名 fn 定义",
        "fn d()\n  print(1)\nend\n",
        Form::Rejected,
        "D118 实测：语句位置只认 `task`；`fn` 仅作闭包表达式",
    ),
    (
        "闭包（单表达式 + end）",
        "let f = fn(x) x + 1 end\nprint(f(1))\n",
        Form::Ok,
        "spec §14.2 `closure` 产生式",
    ),
    (
        "闭包（省略 end）",
        "let f = fn(x) x + 1\nprint(f(1))\n",
        Form::Ok,
        "v0.87 起允许；D118 修复前会**吞掉后续语句**",
    ),
    (
        "闭包（return 体）",
        "let f = fn() return 1 end\nprint(f())\n",
        Form::Ok,
        "D118：与省略 end 形式**并存**",
    ),
    // ── match ─────────────────────────────────────────────
    (
        "match with 形态（spec）",
        "let r = match 1 with\n  1 -> 10\n  _ -> 20\nend\nprint(r)\n",
        Form::Ok,
        "spec §14.2 EBNF `match_stmt`；可折叠",
    ),
    (
        "match 花括号（单行 arm）",
        "let r = match 1 { 1 -> 10 _ -> 20 }\nprint(r)\n",
        Form::Ok,
        "fixtures 常用形态",
    ),
    (
        "match 花括号（arm 跨行）",
        "let r = match 1 { 1 -> 10\n  _ -> 20 }\nprint(r)\n",
        Form::Ok,
        "合法，但 foldingRange **不产出**范围（D138 已知次要缺口）",
    ),
    (
        "match 跨行 arm（with 形态）",
        "let r = match 1\n  1 -> 10\n  _ -> 20\nend\nprint(r)\n",
        Form::Rejected,
        "D137 实测：报 `Expected '{' or 'with' after match subject`",
    ),
    (
        "match 花括号 arm 体是块",
        "let r = match 1 {\n  1 -> {\n  print(1)\n }\n  _ -> 20\n}\nprint(r)\n",
        Form::Rejected,
        "D138 实测：报 `Expected '}'`；花括号 arm 体只能单行 expr",
    ),
    // ── 管道 / 跨行 ───────────────────────────────────────
    (
        "单行管道",
        "let r = \"hi\" |> upper()\nprint(r)\n",
        Form::Ok,
        "方法形态 → 接收者方法调用",
    ),
    (
        "跨行管道（spec §7.6）",
        "let r = \"hello world\"\n  |> upper()\n  |> split(\" \")\nprint(r)\n",
        Form::Ok,
        "D119 修复前整段 parse error",
    ),
    (
        "跨行二元运算",
        "let b = 1\n  + 2\nprint(b)\n",
        Form::Rejected,
        "D119 实测：跨行只对 `|>` 生效，二元运算仍不支持",
    ),
    // ── 后缀调用 ─────────────────────────────────────────
    (
        "连续调用 f()(…)",
        "let mk = fn(x) fn(y) x + y end end\nprint(mk(1)(2))\n",
        Form::Rejected,
        "D123 实测：报 `Expected ')'`；分两步可行",
    ),
    (
        "分两步调用",
        "let mk = fn(x) fn(y) x + y end end\nlet g = mk(1)\nprint(g(2))\n",
        Form::Ok,
        "连续调用的可用等价写法",
    ),
    // ── TEA / 效果 / 声明（§11）—— D140 实测 ──────────────────────
    // ⚠ 三条标 `Rejected` 的是 **spec EBNF 承诺、实现却不接受**的形态
    //    （与「实现不支持且 spec 未承诺」性质不同，后者才该补实现）。
    (
        "TEA app（仅 body）",
        "app Counter\n  print(1)\nend\n",
        Form::Rejected,
        "spec §11 `app_stmt = \"app\" IDENTIFIER { statement } \"end\"` 允许，\
         实现却要求 `model:`/`msg:`/`init:`/`update:`/`view:` 五个字段（D140）",
    ),
    (
        "TEA app（五字段）",
        "app Counter\n  model: Counter\n  msg: CounterMsg\n  init: 0\n  \
         update: fn(msg, model) => model\n  view: fn(model) => model\nend\n",
        Form::Ok,
        "真实形态（见 `tests/fixtures/e2e/tea_app.mora`）",
    ),
    (
        "顶层 update(m)",
        "update(m)\n  m.x = 1\nend\n",
        Form::Rejected,
        "spec §11 `update_stmt` 承诺顶层形态，实现只把它当 `app` 的**字段**（D140）",
    ),
    (
        "handle（on … -> 形态）",
        "handle ask\n  on prompt -> return 1\nend\n",
        Form::Rejected,
        "spec §11 `handle_stmt` 的 `on` 形态不可用；只有 `{ } { }` 形态可用（D140）",
    ),
    (
        "handle（花括号形态）",
        "let r = handle ask { 5 } { 7 }\nprint(r)\n",
        Form::Ok,
        "D140",
    ),
    (
        "model 声明",
        "model Point\n  x: number\n  y: number\nend\n",
        Form::Ok,
        "§11",
    ),
    ("msg 声明", "msg Move\n  x\n  y\nend\n", Form::Ok, "§11"),
    (
        "macro 定义与调用",
        "macro m(x)\n  return x + 1\nend\nprint(m(1))\n",
        Form::Ok,
        "§11",
    ),
    (
        "transaction + compensation",
        "transaction\n  print(1)\n  compensation\n    print(2)\nend\n",
        Form::Ok,
        "§11 `transaction_stmt` 的补偿子句",
    ),
    ("worker 块", "worker w\n  print(1)\nend\n", Form::Ok, "§11"),
    (
        "with 块",
        "with model = \"m\"\n  print(1)\nend\n",
        Form::Ok,
        "§11",
    ),
    (
        "assign 语句",
        "let a = 1\nassign a = 5\nprint(a)\n",
        Form::Ok,
        "§11 `assign_stmt`",
    ),
    // ── 解构 / 标注 ─────────────────────────────────────
    (
        "let 解构",
        "let [a, b] = [1, 2]\nprint(a)\n",
        Form::Rejected,
        "D110 实测：`Expected variable name after 'let'`；`match` 支持同模式",
    ),
    (
        "let 泛型标注",
        "let r: result<number, string> = 1\nprint(r)\n",
        Form::Rejected,
        "D110：不受支持的泛型必须**报错**（D111 起不再静默丢弃）",
    ),
    (
        "函数参数类型标注",
        "task f(a: number)\n  return a\nend\nprint(f(1))\n",
        Form::Rejected,
        "D110 实测：报 `Expected ')' after parameters`；spec 7 处 + README 首例都用它",
    ),
    (
        "裸 list 标注",
        "let xs: list = [1]\nprint(xs)\n",
        Form::Rejected,
        "D130 实测：`unsupported type annotation 'list'`；须写 `list<number>`",
    ),
    (
        "Result 传播 `?`",
        "let x = f(1)?\nprint(x)\n",
        Form::Rejected,
        "D124 实测：Result 传播**完全未实现**（spec 有 3 处示例 + 1 条产生式）",
    ),
    // ── 异质集合 ─────────────────────────────────────────
    // ⚠ 注意这两条**解析通过但 typeck 拒绝**：`ParserV3::compile` 只解析、
    // **不跑 typeck**（异质约束在 `infer_list` / `infer_dict` 层）。
    // 故在本表（解析层）它们算 `Ok`，另见 `d139_heterogeneous_literals_are_rejected_by_typeck`。
    (
        "异质 list 字面量",
        "let xs = [1, \"x\"]\nprint(xs)\n",
        Form::Ok,
        "解析通过；**typeck 拒绝**（D125：`List 字面量的元素必须同质`）",
    ),
    (
        "异质 dict 字面量",
        "let d = {a: 1, b: \"x\"}\nprint(d)\n",
        Form::Ok,
        "解析通过；**typeck 拒绝**（D124：`Dict 字面量的值必须同质`）",
    ),
    (
        "dict.set 异质值",
        "let d = {a: 1}\nlet e = d.set(\"b\", \"x\")\nprint(e)\n",
        Form::Ok,
        "D127 修复前**假阳性**被拒（形参绑成 V 而非 spec 的 `any`）",
    ),
    (
        "json.parse 异质",
        "let d = json.parse(\"{\\\"a\\\":1,\\\"b\\\":\\\"x\\\"}\")\nprint(d)\n",
        Form::Ok,
        "运行时 dict 本就可异质；限制只作用于**字面量**",
    ),
];

#[test]
fn d139_syntax_form_table_matches_reality() {
    let mut mismatches = Vec::new();
    for (name, src, expect, note) in FORMS {
        let actual = if ParserV3::compile(src).is_ok() {
            Form::Ok
        } else {
            Form::Rejected
        };
        // Gap 形态当前一律按「可编译」记录（缺口在运行期，不在解析期）
        let expect = if *expect == Form::Gap {
            Form::Ok
        } else {
            *expect
        };
        if actual != expect {
            mismatches.push(format!(
                "[{name}] 期望 {expect:?} 实得 {actual:?}\n  src={src:?}\n  注：{note}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "语法形态速查表与实现不符（{} 条）—— \
         语法可能已演进，请同步更新 FORMS 表:\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}

#[test]
fn d139_table_is_not_vacuous() {
    // 反向对照：表里必须有真正「非法」的条目，否则这张表可能在测空气
    let rejected = FORMS.iter().filter(|f| f.2 == Form::Rejected).count();
    let ok = FORMS.iter().filter(|f| f.2 == Form::Ok).count();
    assert!(
        rejected >= 9 && ok >= 20,
        "速查表应有足量的两类样本（实测 rejected={rejected}, ok={ok}）"
    );
}

/// 补上速查表在**解析层**表达不了的一层：typeck 拒绝。
///
/// 两条异质字面量在 `FORMS` 里算 `Ok`（`ParserV3::compile` 不跑 typeck），
/// 但它们会被 typeck 拒。这条判据把「解析通过 ≠ 程序合法」钉住 ——
/// 否则日后有人只跑 `compile` 就以为异质字面量可用。
#[test]
fn d139_heterogeneous_literals_are_rejected_by_typeck() {
    use mora::typeck::check_mir::check_program_witnesses_bidirectional;

    for (name, src, needle) in [
        ("list", "let xs = [1, \"x\"]\nprint(xs)\n", "同质"),
        ("dict", "let d = {a: 1, b: \"x\"}\nprint(d)\n", "同质"),
    ] {
        let (_f, wits) = ParserV3::compile(src).expect("解析层应通过");
        let errs = check_program_witnesses_bidirectional(&wits);
        assert!(
            !errs.is_empty(),
            "[{name}] 异质字面量必须被 typeck 拒绝（D124/D125）"
        );
        assert!(
            errs.iter().any(|e| e.message.contains(needle)),
            "[{name}] 诊断应点明「同质」约束; 实际: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}
