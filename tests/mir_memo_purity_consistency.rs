//! v0.104.6 D253 —— 三个「保守白名单」：禁止把**确定有副作用**的指令收进去。
//!
//! ## 起因：白名单对 `Expr` / `Prompt` 的判定互相矛盾
//!
//! | 位置 | 对 `Expr` / `Prompt` 的判定 |
//! |---|---|
//! | `mir/inst.rs::is_effect()` | 两者都算**有副作用** |
//! | `mir/vm/dag.rs::is_memoizable_pure()` | `Expr` 算**纯**（可 memo 跳过） |
//! | `mir/optimize/dag_rule.rs::same_inst_category()` | `Prompt` 算**可 CSE 合并** |
//!
//! 看起来像缺陷。**实测之后，两个矛盾都是安全的**：
//!
//! - `Expr` 的 dispatch 是 `MirInst::Expr(_) => Ok(Flow::Continue)` —— **no-op**。
//!   跳过它与执行它结果完全相同 ⇒ memo **安全**（只是零价值）。
//! - `h_prompt`（`mir/handlers/values.rs:190`）只是把 `parts` 拼成一个
//!   `Value::String` 写进 `regs[dst]` —— **不发 AI 请求、不读 env**。
//!   ⇒ CSE 合并两个相同 `Prompt` **安全**；`is_effect` 是**保守误标**。
//!
//! ## 那本判据到底守什么
//!
//! 不是「两张名单不得重叠」—— 保守重叠是**安全**的（多排除一些而已），
//! 拿它当规则会逼着人去删本来正确的白名单项。
//!
//! 真正危险的方向只有一个，就是 **v0.87 已经踩过**的那个：
//!
//! > v0.87：`gensym` 全部返回 `g0` 的根因 —— 对同名零参数调用做 CSE，
//! > 把**多次**副作用调用合并成了**一次**。
//!
//! 所以本判据盯「**确定有副作用**的指令绝不能进纯白名单 / CSE 白名单」：
//! 这张表是**可以论证**的（`Call` 调 builtin、`MethodCall` 派发方法、
//! `Pipe` 走 `call_value`、`Var` 读 env、`IndexAssign` 就地改 `regs`、
//! `Send`/`Perform`/`Aggregate` 发消息或触发 effect、`MatchExpr` 执行 arm body）。
//!
//! 而 `Expr` / `Prompt` 之所以可以进白名单，靠的是**读 `dispatch` 读出来的**
//! 事实，不是论证 —— 所以它们由**快照**（判据 ③）钉住。

use std::path::Path;

/// 从 `matches!` / `|` 链里抽出 `MirInst::Xxx` 变体名。
fn whitelist(path: &str, fn_name: &str) -> Vec<String> {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    let src = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("读 {path} 失败: {e}"));
    let start = src
        .find(&format!("fn {fn_name}"))
        .unwrap_or_else(|| panic!("{path} 里找不到 fn {fn_name}"));
    let tail = &src[start..];
    // **顶格** `}` 才是函数自己的结尾。早期版本用 "\n    }" 会匹配到函数体
    // 之外很远的地方，把 11 项的白名单解析成 51 项。
    let end = tail
        .find("\n}")
        .unwrap_or_else(|| panic!("{path}::{fn_name} 的结尾找不到"));
    // Scan every `MirInst::Xxx` inside the function body.
    //
    // An earlier version split on `|` and stripped a `MirInst::` prefix from
    // each piece — which silently drops the FIRST arm, because it is glued to
    // the macro head (`matches!(inst, MirInst::Const(..)`). That made the
    // snapshot read 5 variants instead of 6.
    //
    // Safe here because both whitelists we scan are single `matches!` bodies
    // with no nested `MirInst::` mentions.
    let mut out = Vec::new();
    for part in tail[..end].split("MirInst::").skip(1) {
        let name: String = part
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 这些指令**确定有副作用**（dispatch 走 handler，handler 触碰
/// env / regs / effects / 宿主状态）。进白名单 = 静默丢副作用。
const PROVABLY_EFFECTFUL: &[&str] = &[
    "Call",        // 调 builtin（gensym / print / eval 都有副作用）
    "MethodCall",  // 派发方法
    "Pipe",        // 走 call_value
    "Var",         // 读 env
    "Define",      // 写 env
    "Assign",      // 写 env
    "IndexAssign", // 就地改 regs
    "Send",        // 发消息
    "Perform",     // 触发 effect
    "Aggregate",   // 跨节点聚合
    "MatchExpr",   // 执行 arm body
];

fn memoizable() -> Vec<String> {
    whitelist("src/mir/vm/dag.rs", "is_memoizable_pure")
}

/// `same_inst_category` 用**元组**模式 `(A(..), A(..)) => …`，不是 `|` 链。
fn cse_variants() -> Vec<String> {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mir/optimize/dag_rule.rs");
    let src = std::fs::read_to_string(&full).expect("read dag_rule.rs");
    let start = src
        .find("fn same_inst_category")
        .expect("找不到 same_inst_category");
    let end = start + src[start..].find("\n}").expect("结尾");
    let mut out: Vec<String> = src[start..end]
        .split("(MirInst::")
        .skip(1)
        .filter_map(|s| {
            let name: String = s
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() { None } else { Some(name) }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// ① **主判据（有牙齿，v0.87 同款）**：确定有副作用的指令不得进纯白名单。
#[test]
fn d253_memoizable_whitelist_excludes_provably_effectful_insts() {
    let memo = memoizable();
    assert!(!memo.is_empty(), "判据自身失效：纯白名单解析为空");
    let bad: Vec<&String> = memo
        .iter()
        .filter(|v| PROVABLY_EFFECTFUL.contains(&v.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "纯白名单收进了确定有副作用的指令：{bad:?} —— memo 会在输入未变时**跳过**它们，\
         副作用静默丢失（v0.87「gensym 全部返回 g0」的同款根因）。当前白名单 = {memo:?}"
    );
}

/// ② 同一条，对 CSE 白名单。
#[test]
fn d253_cse_whitelist_excludes_provably_effectful_insts() {
    let c = cse_variants();
    assert!(!c.is_empty(), "判据自身失效：CSE 白名单解析为空");
    let bad: Vec<&String> = c
        .iter()
        .filter(|v| PROVABLY_EFFECTFUL.contains(&v.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "CSE 白名单收进了确定有副作用的指令：{bad:?} —— 两次副作用调用会被合并成一次。\
         当前 CSE 白名单 = {c:?}"
    );
}

/// ③ 快照：把「靠读实现才敢放进白名单」的那两项钉死。
///
/// `Expr` / `Prompt` 之所以能进白名单，靠的是**读 dispatch 读出来的**事实
/// （`Expr` no-op、`h_prompt` 纯拼接），不是论证。实现一变，这里就会红。
#[test]
fn d253_whitelist_contents_are_pinned() {
    assert_eq!(
        memoizable(),
        vec!["BinaryOp", "Const", "DictLit", "Expr", "Index", "ListLit"],
        "纯白名单内容变了 —— 若是有意的，请在此处留痕并说明理由"
    );
    assert_eq!(
        cse_variants(),
        vec!["BinaryOp", "Const", "DictLit", "ListLit", "Prompt"],
        "CSE 白名单内容变了 —— 若是有意的，请在此处留痕并说明理由"
    );
}

/// ④ 对照组：证明 ① 的规则本身有判别力（否则「全绿」没有意义）。
#[test]
fn d253_control_group_provable_effect_list_is_real() {
    for inst in ["Call", "MethodCall", "Send", "Var"] {
        assert!(
            PROVABLY_EFFECTFUL.contains(&inst),
            "PROVABLY_EFFECTFUL 漏了 {inst} —— 判据的判别力下降"
        );
    }
    // 规则本身对「确定有副作用的项」确实会拦：拿 Call 试一次。
    let fake = ["Call".to_string(), "Const".to_string()];
    let caught: Vec<&String> = fake
        .iter()
        .filter(|v| PROVABLY_EFFECTFUL.contains(&v.as_str()))
        .collect();
    assert_eq!(caught.len(), 1, "规则对 Call 竟然没反应 —— ① 恒绿");
    // 且「保守重叠」是被允许的（本判据不靠交集为空来守）：Prompt 在 CSE 白名单
    // 里，而它不在 PROVABLY_EFFECTFUL 里。
    let cse = cse_variants();
    assert!(
        cse.contains(&"Prompt".to_string()) && !PROVABLY_EFFECTFUL.contains(&"Prompt"),
        "对照组前提变了：CSE 白名单不再收录 Prompt"
    );
}
