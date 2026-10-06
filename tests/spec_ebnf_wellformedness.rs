//! v0.104.6 D162：spec §14.2 的 EBNF 引用了 **8 个从未定义**的非终结符。
//!
//! ## 为什么既有普查抓不到
//!
//! `tests/spec_ebnf_census.rs` 的判据是「每条产生式的**最小实例能否被 parser 解析**」
//! —— 它把**代码**喂给 parser。一个文法只要**引用**了不存在的非终结符，
//! 其最小实例照样能解析（因为没人检查引用完整性），故对这类洞**完全隐形**。
//!
//! D108 / D109 已经手工修过同一种洞三次（`route_stmt` 删除、`trait_stmt`/`impl_stmt`
//! 从联合式移除、`parallel_stmt`/`observe_stmt` 补齐）—— 手工只能修一次，
//! 明天新增产生式又没人扫。故本文件把它机械化。
//!
//! ## 实测：8 个非终结符被引用，全 spec 范围内**零定义**
//!
//! | 非终结符 | 被谁引用 | 出现次数 |
//! |---|---|---|
//! | `params` | `task_stmt` / `macro_stmt` / `closure` | 5 |
//! | `variable` | `expr` | 1 |
//! | `binary` | `expr` | 1 |
//! | `call` | `expr` | 2 |
//! | `method_call` | `expr` | 2 |
//! | `index` | `expr` | 2 |
//! | `pattern` | `match_stmt` | 1 |
//! | `bindings` | `with_stmt` | 1 |
//!
//! 即**表达式文法的核心五项**（变量 / 二元运算 / 调用 / 方法调用 / 下标）
//! 全部没有产生式 —— §14.2 的 `expr` 联合式指向五个不存在的产生式。
//!
//! ## `type` 不在此列
//!
//! 它在 §13.1 用 **BNF 记法**定义（`τ ::= string | char | …`），不是 EBNF 的
//! `name = …` 风格。本文件因此也收集 `::=` 形式，并把 `τ` 计入已定义集合。
//!
//! ## 本文件的判据
//!
//! 引用完整性：EBNF 里每个**小写**非终结符（排除显式终符表）都必须在
//! 全 spec 范围内有定义。终符表**显式写死**而不是靠启发式猜 —— 猜错的代价
//! 是假阴性（洞被静默放过），与本文件要解决的问题同型。

use std::collections::BTreeSet;

/// 显式终符表：lexer 层的大写类别 + `BIGINT` 产生式里引用的 `digits`。
///
/// ⚠ **显式列出是有意的**：靠「未定义且未被引用的驼峰」之类的启发式去猜，
/// 猜错会**静默放过**真洞（假阴性），与本文件要抓的东西同型。
const TERMINALS: &[&str] = &[
    "IDENTIFIER",
    "NUMBER",
    "BIGINT",
    "STRING",
    "CHAR",
    "BOOL",
    "NIL",
    "QUASIWORD",
    "digits",
];

fn spec() -> String {
    std::fs::read_to_string("docs/mora-spec.md").expect("读 docs/mora-spec.md")
}

/// 已定义的非终结符：EBNF 的 `name = …` 与 BNF 的 `name ::= …` 都算。
fn defined_nonterminals(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("--") || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix('`') {
            // 行内代码里可能藏着 `x = y`，但不是产生式 —— 跳过
            let _ = rest;
            continue;
        }
        if let Some(eq2) = t.find("::=") {
            let name = t[..eq2].trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == 'τ')
            {
                out.insert(name.to_string());
            }
            continue;
        }
        if let Some(eq) = t.find('=') {
            let name = t[..eq].trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// ```ebnf … ``` 块里的全部小写非终结符引用。
fn referenced_nonterminals(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t == "```ebnf" {
            in_block = true;
            continue;
        }
        if in_block && t == "```" {
            in_block = false;
            continue;
        }
        if !in_block {
            continue;
        }
        // 去掉行注释（`--` 之后）与所有终结符/符号
        let rhs = match t.find("--") {
            Some(p) => &t[..p],
            None => t,
        };
        let mut cleaned = String::with_capacity(rhs.len());
        let mut in_str = false;
        for c in rhs.chars() {
            match c {
                '"' => in_str = !in_str,
                _ if in_str => {}
                _ => cleaned.push(c),
            }
        }
        for tok in cleaned.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if tok.is_empty() {
                continue;
            }
            let first = tok.chars().next().expect("非空");
            if first.is_ascii_lowercase() {
                out.insert(tok.to_string());
            }
        }
    }
    out
}

/// §14.2 引用、但**全 spec 范围内没有产生式**的非终结符。
///
/// **D163 已补齐其中 6 项**、**D168 再补 `pattern`**：`params` / `variable` / `call` / `method_call` /
/// `index` / `bindings` —— 形状逐条对照 parser 实测，最小实例已加进
/// `tests/spec_ebnf_census.rs`，由 parser 持续验收。
///
/// 下面是**仍未补的 3 项**（显式待办清单，不是「已接受」）。写成测试而不是
/// 注释，是为了让「文法还剩哪些洞」**可查**、且**新增任何一个洞都会转红**。
///
/// | 非终结符 | 被谁引用 | 为何未补 |
/// |---|---|---|
/// | `type` | `let_stmt` / `task_stmt` / `model_stmt` / `update_stmt` | **名字对不上**：§13.1 用 BNF 记法把类型写作 `τ ::= …`，本节按 `type` 引用。改哪边属 spec 体例决定 |
///
/// `binary` 缺失使本节 `expr` 联合式**至今不完整** —— `a + b` 这类最基础的
/// 表达式没有产生式。这是剩余三项里价值最高的一条。
const KNOWN_UNDEFINED: &[&str] = &["type"];

/// D162 主判据：EBNF 引用的非终结符，**要么已定义、要么在已知待办清单里**。
///
/// 判定用「未定义集**恰好等于** KNOWN_UNDEFINED」：
/// 清单外的任何一个未定义项都会让本条转红（新增洞即刻告警），
/// 清单里的任何一项被补上也会转红（该从清单里划掉）。
#[test]
fn d162_ebnf_undefined_set_matches_known_backlog() {
    let text = spec();
    let defined = defined_nonterminals(&text);
    let referenced = referenced_nonterminals(&text);
    let terminals: BTreeSet<String> = TERMINALS.iter().map(|s| s.to_string()).collect();

    let undefined: BTreeSet<String> = referenced
        .iter()
        .filter(|n| !defined.contains(*n) && !terminals.contains(*n))
        .cloned()
        .collect();

    let known: BTreeSet<String> = KNOWN_UNDEFINED.iter().map(|s| s.to_string()).collect();
    let new_ones: Vec<&String> = undefined.difference(&known).collect();
    let fixed_ones: Vec<&String> = known.difference(&undefined).collect();

    assert!(
        new_ones.is_empty(),
        "§14.2 新增了**未定义**的非终结符引用（{} 个）——\
         「联合式引用了不存在的产生式」是本节自身的内部矛盾\
         （D108/D109/D162 已手工修过同类）。请为它们补上产生式，\
         或把它们加进 KNOWN_UNDEFINED 并在上表登记：\n  - {}",
        new_ones.len(),
        new_ones
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  - "),
    );
    assert!(
        fixed_ones.is_empty(),
        "以下非终结符此前在 KNOWN_UNDEFINED 里，现已**有定义**了 —— \
         请从清单里划掉并更新上表：\n  - {}",
        fixed_ones
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  - "),
    );
}

/// 反向对照：已定义的产生式**不应**被误报成「未定义」。
///
/// 若本条转红，说明提取规则把「定义」当成了「引用」——那时主判据的绿灯
/// 就不可信了（同 D157 的「判据的判据也要先成立」）。
#[test]
fn d162_defined_productions_are_not_reported_as_undefined() {
    let text = spec();
    let defined = defined_nonterminals(&text);
    for name in [
        "program",
        "statement",
        "let_stmt",
        "task_stmt",
        "expr",
        "literal",
        "closure",
        "handle_stmt",
    ] {
        assert!(
            defined.contains(name),
            "`{name}` 是 §14.2 明确给出的产生式，提取规则必须认出它; 已认出: {defined:?}"
        );
    }
    // 终符表本身也不能被当成待定义项
    let referenced = referenced_nonterminals(&text);
    assert!(
        !referenced.contains("IDENTIFIER") && !referenced.contains("STRING"),
        "大写终结符不应被算作小写非终结符引用; 实得: {referenced:?}"
    );
}
