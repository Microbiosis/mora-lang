//! v0.104.6 D109：spec §14.2 **每一条**产生式的最小实例是否真能解析。
//!
//! ## 为什么做这件事
//!
//! D108 抓到 `handle_stmt` 一条：spec 写的形式**实现里根本解析不了**。
//! 既然已知这个类别存在，就把 §14.2 的全部产生式逐条实测，而不是只查被
//! 怀疑的那几条。
//!
//! ## 上一轮探针的教训（自己踩的）
//!
//! 用 PowerShell 驱动 CLI、按 `Parse error:` 子串判定「是否解析失败」，
//! 而实际错误串还有 `Failed to parse at line N` —— 于是把**完全解析不了**的
//! `trait` / `impl` 误判为「OK」。**判据的分类不完备 = 结论不可信。**
//! 本文件直接调 `ParserV3::compile`，二值判定，无歧义。

use mora::parser_v3::ParserV3;

fn parses(src: &str) -> Result<(), String> {
    ParserV3::compile(src).map(|_| ())
}

/// §14.2 里「有定义」的产生式，各自的最小实例。
/// 每条都按 spec 原文的形状书写 —— 不臆造变体。
const PRODUCTIONS: &[(&str, &str)] = &[
    ("let_stmt", "let x = 1\n"),
    ("assign_stmt", "let x = 1\nassign x = 2\n"),
    ("task_stmt", "task t()\n  print(1)\nend\n"),
    (
        "if_stmt",
        "let c = 1\nif c == 1 then\n  print(1)\nelse\n  print(2)\nend\n",
    ),
    ("for_stmt", "for x in [1, 2]\n  print(x)\nend\n"),
    ("return_stmt", "task t()\n  return 1\nend\n"),
    (
        "match_stmt",
        "let v = 1\nlet r = match v with\n  1 => \"a\"\n  _ => \"b\"\nend\n",
    ),
    ("with_stmt", "with model = \"m\"\n  print(1)\nend\n"),
    ("worker_stmt", "worker w do\n  print(1)\nend\n"),
    (
        "transaction_stmt",
        "transaction\n  print(1)\n  compensation\n    print(2)\nend\n",
    ),
    ("macro_stmt", "macro m(x)\n  print(x)\nend\n"),
    (
        "handle_stmt",
        "let g = \"i\"\nhandle Ai {\n  g = perform Ai(\"hi\")\n} {\n  \"mocked:\" + __arg0\n}\n",
    ),
    ("model_stmt", "model M\n  count: number = 0\nend\n"),
    ("msg_stmt", "msg Inc\n  Bump\nend\n"),
    ("update_stmt", "update(msg, model)\n  model\nend\n"),
    // `app` 的真实形状（取自 tests/fixtures/e2e/tea_app.mora）：标签是
    // `model:` / `msg:` / `init:` / `update:` / `view:`，不是我最初猜的裸语句。
    (
        "app_stmt",
        "app Counter\n  model: Counter\n  msg: CounterMsg\n  init: 0\n  update: fn(msg, model) => model\n  view: fn(model) => model\nend\n",
    ),
    // statement 联合式引用、但 §14.2 未定义产生式的四项
    ("parallel", "parallel\n  print(1)\nend\n"),
    ("observe", "observe trace \"t\" do\n  print(1)\nend\n"),
    ("trait", "trait T\n  print(1)\nend\n"),
    ("impl", "impl T\n  print(1)\nend\n"),
    // 表达式类
    ("prompt", "print(p\"hi\")\n"),
    ("closure", "let f = fn(x) x + 1\nprint(f(1))\n"),
    ("pipe", "print([1, 2, 3] |> len())\n"),
    ("eval_expr", "print(eval(1 + 1))\n"),
    ("quote_expr", "print(quote(1))\n"),
    ("list_literal", "print([1, 2])\n"),
    ("dict_literal", "print({a: 1})\n"),
    ("BIGINT", "print(123n)\n"),
    // v0.104.6 D163：为 D162 补进 §14.2 的 6 条非终结符各配一个最小实例。
    // 本表把每条产生式的最小实例**喂给 parser**，故这节文法由它持续验收 ——
    // spec 若改错形状，这里立刻红。
    ("params", "let f = fn(x) x + 1\nprint(f(1))\n"),
    ("variable", "let v = 1\nprint(v)\n"),
    ("call", "let c = print(1)\n"),
    (
        "method_call",
        "let m = [1, 2, 3]\nlet g = m.get(0)\nprint(g)\n",
    ),
    ("index", "let m = [1, 2, 3]\nprint(m[0])\n"),
    (
        "bindings",
        "with model = \"m\", temperature = 0.7\n  print(1)\nend\n",
    ),
    // v0.104.6 D164：`binary` 的算子集与结合强度逐层实测自 emit.rs 的
    // or → and → ==/!= → |> → </>/<=/>= → +/- → *//% 链。
    ("binary", "let a = 1 + 2 * 3\nprint(a)\n"),
    // `and` / `or` 是**标识符派生的中缀算子**（不在 §14.1 关键字表里），
    // 故单列一条钉住这个易被误判的形态。
    (
        "binary_and_or",
        "let a = true and false\nlet b = true or false\nprint(a)\nprint(b)\n",
    ),
    // v0.104.6 D168：`pattern` 覆盖通配 / 字面量 / 变量绑定 / 列表（定长 + 尾随 rest）/ 字典
    (
        "pattern_wildcard",
        "let r = match 1 with\n  _ -> 0\nend\nprint(r)\n",
    ),
    (
        "pattern_literal",
        "let r = match 5 with\n  0 -> 100\n  n -> n\nend\nprint(r)\n",
    ),
    (
        "pattern_list",
        "let r = match [1, 2] with\n  [] -> 0\n  [a] -> a\n  [a, b] -> a + b\n  [x, ..] -> x\n  _ -> -1\nend\nprint(r)\n",
    ),
    (
        "pattern_dict",
        "let r = match {a: 9} with\n  {a: k} -> k\n  _ -> 0\nend\nprint(r)\n",
    ),
];
/// 主断言：把全部结果**打印出来并列出失败清单**。
///
/// 这不是「全绿即可」的测试 —— 它是**普查报告**：
/// 失败项就是 spec 承诺但实现不支持的语法（D108 的 `handle_stmt` 已修，
/// 若这里再出现同类，说明还有未修的）。
#[test]
fn d109_spec_production_minimal_instances() {
    // D109 之后**没有任何**产生式不可解析 —— 下表是实测结果。
    const EXPECTED_UNPARSEABLE: &[&str] = &["trait", "impl"];

    let mut unparseable: Vec<String> = Vec::new();
    let mut report = Vec::new();
    for (name, src) in PRODUCTIONS {
        match parses(src) {
            Ok(()) => report.push(format!("{name}: 可解析")),
            Err(e) => {
                let first = e.lines().next().unwrap_or("").to_string();
                unparseable.push(name.to_string());
                report.push(format!("{name}: **不可解析** — {first}"));
            }
        }
    }
    eprintln!("D109 spec §14.2 产生式普查：\n  {}", report.join("\n  "));

    let got: Vec<&str> = unparseable.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        got, EXPECTED_UNPARSEABLE,
        "不可解析的产生式清单变了：得到 {got:?}，期望 {EXPECTED_UNPARSEABLE:?}。\n  \
         若某条**变得可解析** → 实现升级了，需在 spec 补产生式；\n  \
         若某条**变得不可解析** → 是回归。"
    );
    assert_eq!(report.len(), PRODUCTIONS.len());
}

/// D109 的 spec 侧修正必须留在文件里：联合式不得再引用 `trait_stmt` /
/// `impl_stmt`（二者既无产生式也不可解析），且 `parallel_stmt` / `observe_stmt`
/// 必须补上产生式。
#[test]
fn d109_spec_union_and_missing_productions() {
    let text = std::fs::read_to_string("docs/mora-spec.md").expect("读 docs/mora-spec.md");
    let union = text
        .lines()
        .find(|l| l.trim_start().starts_with("statement   ="))
        .expect("spec §14.2 里找不到 statement 联合式");
    assert!(
        !union.contains("trait_stmt") && !union.contains("impl_stmt"),
        "statement 联合式不得引用 trait_stmt / impl_stmt（既无产生式也不可解析）。\n实际：{union}"
    );
    assert!(
        text.contains("parallel_stmt = \"parallel\""),
        "parallel_stmt 缺产生式（曾被联合式引用）"
    );
    assert!(
        text.contains("observe_stmt  = \"observe\""),
        "observe_stmt 缺产生式（曾被联合式引用）"
    );
}
