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
    ParserV3::compile(src)
        .map(|_| ())
        .map_err(|e| e.to_string())
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
    // v0.110.0 Wave 2 / Stage 3C：parallel / trait / impl / struct / enum / select / spawn / no_grad / slice 全部接入前端
    ("parallel", "parallel\n  print(1)\nend\n"),
    ("trait", "trait T\n  task f(self): int\nend\n"),
    (
        "impl",
        "trait T\n  task f(self): int\nend\nimpl T for int\n  task f(self): int\n    return self\n  end\nend\n",
    ),
    (
        "while_stmt",
        "let i = 0\nwhile i < 1\n  assign i = i + 1\nend\n",
    ),
    ("type_alias_stmt", "type Num = int\nlet x: Num = 1\n"),
    (
        "struct_stmt",
        "struct Point<T>\n  x: T\n  y: int = 0 @tag(json=\"y,omitempty\")\nend\n",
    ),
    ("enum_stmt", "enum Maybe<T>\n  Just(T)\n  Nothing\nend\n"),
    ("effect_stmt", "effect Log(string): nil\n"),
    ("export_stmt", "export let answer = 42\n"),
    (
        "select_stmt",
        "let ch = channel(1)\nselect\n  default -> print(1)\nend\n",
    ),
    (
        "spawn_expr",
        "task work(): int\n  return 1\nend\nlet h = spawn work()\n",
    ),
    ("no_grad_expr", "let y = no_grad\n  1.0 + 2.0\nend\n"),
    (
        "tensor_slice",
        "let t = Tensor::zeros([4, 4])\nlet s = t[0..2, :, None]\n",
    ),
    (
        "default_and_named_args",
        "task lin(x: int, w: int, b: int = 1): int\n  return x * w + b\nend\nlet r = lin(2, w=3)\n",
    ),
    // 表达式类
    ("closure", "let f = fn(x) x + 1\nprint(f(1))\n"),
    ("pipe", "print([1, 2, 3] |> len())\n"),
    ("eval_expr", "print(eval(1 + 1))\n"),
    ("quote_expr", "print(quote(1))\n"),
    ("list_literal", "print([1, 2])\n"),
    ("dict_literal", "print({a: 1})\n"),
    // v0.104.6 D163：为 D162 补进 §14.2 的非终结符各配一个最小实例。
    // 本表把每条产生式的最小实例**喂给 parser**，故这节文法由它持续验收 ——
    // spec 若改错形状，这里立刻红。
    ("params", "let f = fn(x) x + 1\nprint(f(1))\n"),
    // v0.104.6：形参类型标注已实现（`annotated_params` 用于 task / 闭包 fn）。
    ("annotated_params", "task t(a: number)\n  print(a)\nend\n"),
    // v0.104.6：泛型声明子句 `task f<T>(…)`。
    ("type_params", "task t<T>(a: T)\n  print(a)\nend\n"),
    ("variable", "let v = 1\nprint(v)\n"),
    ("call", "let c = print(1)\n"),
    (
        "method_call",
        "let m = [1, 2, 3]\nlet g = m.get(0)\nprint(g)\n",
    ),
    ("index", "let m = [1, 2, 3]\nprint(m[0])\n"),
    // v0.104.6 D123 补齐的 `primary` / `postfix` 两条。`postfix` 的最小实例
    // 取**值调用**形态 `(primary)(args)` —— 正是此前 spec 推导不出、parser
    // 硬报错的那一形态。
    ("primary", "let v = 1\nprint(v)\n"),
    ("postfix", "print((fn(x) x * 2 end)(5))\n"),
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
    (
        "pattern_variant",
        "enum Maybe\n  Just(int)\n  Nothing\nend\nlet x = Maybe::Just(1)\nlet r = match x with\n  Maybe::Just(v) -> v\n  Maybe::Nothing -> 0\nend\nprint(r)\n",
    ),
    (
        "pattern_struct",
        "struct Pt\n  x: int\nend\nlet p = Pt(1)\nlet r = match p with\n  Pt { x: a } -> a\nend\nprint(r)\n",
    ),
];
/// 主断言：把全部结果**打印出来并列出失败清单**。
///
/// 这不是「全绿即可」的测试 —— 它是**普查报告**：
/// 失败项就是 spec 承诺但实现不支持的语法（D108 的 `handle_stmt` 及
/// v0.110.0 Stage 3C 的 `trait`/`impl` 均已闭环）。
#[test]
fn d109_spec_production_minimal_instances() {
    // v0.110.0 起 §14.2 全部产生式（含 trait / impl / struct / enum / select / spawn / no_grad / slice）均可解析。
    const EXPECTED_UNPARSEABLE: &[&str] = &[];

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

/// D109 / v0.110.0 的 spec 侧校验：已移除的 `with_stmt` / `observe_stmt` / `model_stmt` / `prompt`
/// 不得残留，且 Wave 2 / Stage 3C 新增产生式（含 `parallel_stmt` / `trait_stmt` / `impl_stmt` /
/// `struct_stmt` / `enum_stmt` / `select_stmt` / `spawn_expr` / `no_grad_expr` / `tensor_slice`）必须有定义。
#[test]
fn d109_spec_union_and_missing_productions() {
    let text = std::fs::read_to_string("docs/mora-spec.md").expect("读 docs/mora-spec.md");
    let stmt_start = text
        .find("statement   =")
        .expect("spec §14.2 里找不到 statement 联合式");
    let stmt_end = text[stmt_start..]
        .find(';')
        .expect("statement 联合式缺少分号终止");
    let stmt_union = &text[stmt_start..stmt_start + stmt_end];
    for expected in [
        "parallel_stmt",
        "struct_stmt",
        "enum_stmt",
        "trait_stmt",
        "impl_stmt",
        "select_stmt",
        "while_stmt",
        "export_stmt",
        "effect_stmt",
        "type_alias_stmt",
    ] {
        assert!(
            stmt_union.contains(expected),
            "statement 联合式缺少 `{expected}`。\n实际：{stmt_union}"
        );
    }
    assert!(
        !text.contains("observe_stmt")
            && !text.contains("with_stmt")
            && !text.contains("model_stmt")
            && !text.contains("prompt      ="),
        "v0.110.0 已移除 observe_stmt / with_stmt / model_stmt / prompt，spec §14.2 不应再保留"
    );
    for prod in [
        "parallel_stmt = \"parallel\"",
        "struct_stmt = \"struct\"",
        "enum_stmt   = \"enum\"",
        "trait_stmt  = \"trait\"",
        "impl_stmt   = \"impl\"",
        "select_stmt = \"select\"",
        "spawn_expr  = \"spawn\"",
        "no_grad_expr = \"no_grad\"",
        "tensor_slice = expr \"[\"",
    ] {
        assert!(
            text.contains(prod),
            "docs/mora-spec.md §14.2 缺少产生式 `{prod}`"
        );
    }
}
