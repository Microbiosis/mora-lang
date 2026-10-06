//! v0.104.6 D314：`max_reg_in_node` 的 `_ => 0` 兜底覆盖了 **50 个 `Node`
//! 变体里的 25 个** ⇒ 9 层管线的 `n_regs` 被少算 ⇒ 管线路径上**运行期越界**。
//!
//! ## 现象（修前）
//!
//! `tests/fixtures/e2e/rel_empty.mora`（`rel edge("a","b")` + `solve { … }`）
//! 在 9 层管线路径上：
//!
//! ```text
//! Runtime error (MIR): internal: instruction at DAG node 1 references
//! register 4 (read/write) but the function only has 1 register(s)
//! — a unit-statement emitter returned an unallocated sentinel register
//! ```
//!
//! 生产路径上不可见，因为差分回落挡住了（见下）。但它是**真实的潜在崩溃**。
//!
//! ## 触发面（实测，56 个真实 `.mora`）
//!
//! **11/56（19.6%）静默回落到 `emit.rs`** —— 9 层管线的产出被丢弃、改用
//! 另一条编译路径，用户只看到一行 stderr。临时加一个「忽略差分强制走管线」
//! 的开关后实测：**11 个里有 7 个一上管线就硬崩**，且报同一个内部错误。
//!
//! ## 根因
//!
//! `fcfg_lower::max_reg_in_node` 逐个 match `Node` 变体求「预分配的最大
//! 寄存器号」，兜底是 `_ => 0`。实测该 match **只覆盖 25/50 个变体**，
//! 另外 25 个全部落进兜底 —— 其中 `Return` / `Solve` / `WithConfig`
//! **确实带顶层寄存器**。
//!
//! `lower_fcfg` 用同一个值做两件事，**都因此出错**：
//!
//! - `ctx.next_reg = max_reg_in_nodes(nodes) + 1`（bump 分配器起点）——
//!   起点过低会**覆盖**预分配寄存器，违反该函数自己的「寄存器安全契约」；
//! - `n_regs = max_pre_alloc.max(ctx.next_reg - 1) + 1` —— 过小 ⇒ 运行期越界。
//!
//! `rel_empty.mora` 的 FCFG 只含 `RelDef` 与 `Solve`，两者都落兜底 ⇒
//! `max_reg_in_nodes` 返回 0 ⇒ `n_regs == 1`，而发射出的
//! `MirInst::Solve { dst: 4, .. }` 引用寄存器 4。
//!
//! ## 修复
//!
//! 1. **删掉 `_ => 0`**，50 个变体逐个显式表态 ⇒ 新增变体从此是**编译错误**
//!    而不是静默继承「不带寄存器」的假设。整族缺陷不再复发。
//! 2. `n_regs` 再取一个下界：扫**已发射的 `MirInst`** 的
//!    `input_regs()` / `written_reg()`。只会让 `n_regs` 变大（多几个
//!    `Value` 槽，无害），不可能变小 —— 纵深防御。
//!
//! ## 修后（实测）
//!
//! - 强制走管线路径：11 个回落程序的行为改变 **7/11 → 0/11**。
//! - 静默回落仍为 **11/56** —— 差分拦的是**指令条数**差异（pipeline 2 vs
//!   emit 3），与 `n_regs` 无关，**本轮没有解决**。见文末「未解决」。

use mora::common::Span;
use mora::mir::fcfg::{Fcfg, FcfgBlock, Node};

fn span() -> Span {
    Span::default()
}

fn block(nodes: Vec<Fcfg>) -> FcfgBlock {
    FcfgBlock {
        nodes,
        result: None,
    }
}

/// 核心不变量：`n_regs` 覆盖**每一条已发射指令**引用的每一个寄存器。
///
/// 修前 `Node::Solve { dst: 7 }` 单独成块时 `n_regs` 结算为 1 ⇒ 越界 ⇒ 本条红。
#[test]
fn n_regs_covers_every_emitted_register() {
    // 三类「修前落进 `_ => 0`、但确实带顶层寄存器」的变体。
    let cases: Vec<(&str, Fcfg, usize)> = vec![
        (
            "Solve",
            Node::Solve {
                dst: 7,
                limit: None,
                query_vars: vec![],
                anon_vars: vec![],
                goal: block(vec![]),
                span: span(),
                meta: (),
            },
            7,
        ),
        (
            "WithConfig",
            Node::WithConfig {
                bindings: vec![("k".to_string(), 9)],
                body: block(vec![]),
                dst: 5,
                span: span(),
                meta: (),
            },
            9,
        ),
        (
            "Return",
            Node::Return {
                value: Some(11),
                span: span(),
                meta: (),
            },
            11,
        ),
    ];

    for (tag, node, highest) in cases {
        let (insts, n_regs) = mora::mir::fcfg_lower::lower_fcfg(std::slice::from_ref(&node));
        assert!(
            n_regs > highest,
            "[{tag}] 节点引用寄存器 {highest}，但 lower_fcfg 结算 n_regs={n_regs}。\n\
             修前这 3 个变体落进 `max_reg_in_node` 的 `_ => 0` ⇒ n_regs 少算 ⇒\
             9 层管线路径上运行期越界（references register N but the function only \
             has M register(s)）。"
        );
        // 更强的不变量：任何已发射指令引用的寄存器都必须在范围内。
        for inst in &insts {
            for r in inst.input_regs() {
                assert!(
                    r < n_regs,
                    "[{tag}] 发射的指令 {inst:?} 引用寄存器 {r}，超出 n_regs={n_regs}"
                );
            }
            if let Some(w) = inst.written_reg() {
                assert!(
                    w < n_regs,
                    "[{tag}] 发射的指令 {inst:?} 写寄存器 {w}，超出 n_regs={n_regs}"
                );
            }
        }
    }
}

/// **`_ => 0` 兜底已被删除** —— 外层 `match node` 的 `Node::` 分支数必须
/// **等于 `Node` 变体总数**（从 `fcfg.rs` 现读，因此自维护）。
///
/// 修前该 match 只有 25 个 `Node::` 分支 + 一个 `_ => 0`。
///
/// **这道判据的最强形式其实是编译器本身**：反向变异（删掉 `Return` / `Solve`
/// / `WithConfig` 三个分支）**根本编译不过** ——
/// `error[E0004]: non-exhaustive patterns`。也就是说，本修复把一个**静默**
/// 的少算变成了**编译期**错误，这是比测试更硬的保证。本条负责覆盖另一种
/// 情况：有人重新引入 `_ =>` 兜底。
///
/// ⚠ 注意：函数**内部**另有 `QuasiquoteSegment` 的 `match`，那里的 `_ => m`
/// 是合法的（另两个变体不带寄存器）。所以本条只数**外层** `match node`
/// 在其区域内的变体名，不做全文 `_ =>` 扫描。
#[test]
fn max_reg_in_node_covers_every_node_variant() {
    let fcfg_src = include_str!("../src/mir/fcfg.rs");
    let lower_src = include_str!("../src/mir/fcfg_lower.rs");

    // 1) Node 变体总数
    let decl = fcfg_src
        .find("pub enum Node<M> {")
        .expect("应能找到 pub enum Node<M>");
    let decl_body = &fcfg_src[decl..];
    let decl_end = decl_body.find("\n}\n").expect("Node 枚举应有结尾");
    let variants: Vec<&str> = decl_body[..decl_end]
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            if !t.starts_with("//") && t.starts_with(|c: char| c.is_ascii_uppercase()) {
                t.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
        .collect();

    // 2) max_reg_in_node 里外层 `match node` 在深度 1 上的 Node:: 分支
    let fn_start = lower_src
        .find("fn max_reg_in_node(node: &Fcfg) -> usize {")
        .expect("应能找到 max_reg_in_node");
    let fn_body = &lower_src[fn_start..];
    let m_at = fn_body
        .find("match node {")
        .expect("max_reg_in_node 应以 match node 开头");
    let mut depth = 0i32;
    let mut match_end = fn_body.len();
    for (i, ch) in fn_body[m_at..].char_indices() {
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                match_end = m_at + i;
                break;
            }
        }
    }
    let outer = &fn_body[m_at..match_end];
    // 数**变体名**而不是 arm 行数 —— 同一个 arm 可以写成
    // `Node::Break { .. } | Node::Continue { .. } => 0`（一行两个变体）。
    // 外层 match 区域内没有嵌套的 `Node::` 引用（嵌套体走 `max_reg_in_nodes`），
    // 所以这样数是精确的。
    let mut arms: Vec<String> = Vec::new();
    for (v, _) in outer.match_indices("Node::") {
        let rest = &outer[v + "Node::".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && !arms.contains(&name) {
            arms.push(name);
        }
    }
    let arms: Vec<&str> = arms.iter().map(|s| s.as_str()).collect();

    assert_eq!(
        arms.len(),
        variants.len(),
        "**D314**：`max_reg_in_node` 的外层 `match node` 有 {} 个 `Node::` 分支，\
         而 `Node<M>` 有 {} 个变体。\n\
         差额来自 `_ => 0` 兜底 —— 它让每个未枚举的变体静默继承「不带寄存器」\
         的假设。`Return` / `Solve` / `WithConfig` 就因此被漏算 ⇒ `n_regs` 少算 ⇒\
         9 层管线路径上运行期越界（实测 11 个静默回落的程序里 7 个一上管线就崩）。\n\
         缺: {}\n\
         变体: {}",
        arms.len(),
        variants.len(),
        variants
            .iter()
            .filter(|v| !arms.contains(v))
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        variants.join(", ")
    );
}
