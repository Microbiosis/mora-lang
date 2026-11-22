//! v0.110.0: `ParserV3` / `MirWitness` LSP 文档解析与符号提取入口。
//!
//! 所有 LSP Provider（hover / completion / definition / references / rename / symbols /
//! semantic tokens / folding / formatting）统一通过本模块获取 `MirWitness` 树与符号位置。

use std::collections::HashMap;

use crate::lsp::json::Value as JsonValue;
use crate::lsp::server::DocumentState;
use crate::mir::witness::{MirWitness, WitnessKind};

///  Look up and parse MirWitness list for `uri`.
///  Returns `None` when the document has not been opened yet or parse fails.
pub fn parsed_doc_v3(
    docs: &HashMap<String, DocumentState>,
    uri: &str,
) -> Option<(String, Vec<MirWitness>)> {
    let doc = docs.get(uri)?;
    let (_, witnesses) = crate::parser_v3::ParserV3::compile(&doc.text).ok()?;
    Some((doc.text.clone(), witnesses))
}

///  Walks the entire MirWitness tree and invokes `visit` for every
///  [`MirWitness`] node. Used by the references, rename, and semantic
///  providers to avoid recursive duplication.
pub fn walk_witness<F: FnMut(&MirWitness)>(expr: &MirWitness, visit: &mut F) {
    visit(expr);
    walk_witness_kind(&expr.kind, visit);
}

/// v0.104.6 D245：入站 `position` / `range` 的**唯一收口** —— `i64 → usize`
/// 的非负转换。
///
/// LSP 规定 `line` / `character` 从 0 起（`uinteger`），但那是**协议约束**，
/// 不是可依赖的输入：畸形或离线的客户端会发负数。此前各 provider 各自写
/// `as_i64().unwrap_or(0) as usize`，负数经整数 `as` 的**回绕**变成
/// `usize::MAX`（实测 `(-1i64) as usize == 18446744073709551615`）。
///
/// ## 实测后果（`hover_v3`，文档 `...let beta: Int = alpha\nbeta`）
///
/// ```text
/// position {"line": -1, "character": 0}
///   → offset 落到 text.len()，在**文件末尾**找到了标识符 `beta`
///   → 返回 `let beta: Int`，range 的 line = 1.8446744073709552e19
/// ```
///
/// 即**静默返回错误内容 + 荒谬的出站 range**（不是「无结果」）。客户端照该
/// range 应用 rename / format 就会**改坏用户文件** —— 正是本文件上方注释里
/// 警告过的失败模式，只是触发路径从「UTF-16 错位」换成了「负数回绕」。
///
/// `server.rs::pos_of` 早已带 `.max(0)`（D194），而 `definition` / `hover` /
/// `formatting` 三处**各写了一遍**、全都没带 ⇒ 仓库内不一致，正是 D244 在
/// `checkpoint` 里遇到的同一种形态。现统一收敛到此函数。
pub fn pos_of(line: Option<&JsonValue>, ch: Option<&JsonValue>) -> (usize, usize) {
    let l = line.and_then(|n| n.as_i64()).unwrap_or(0).max(0) as usize;
    let c = ch.and_then(|n| n.as_i64()).unwrap_or(0).max(0) as usize;
    (l, c)
}

fn walk_witness_kind<F: FnMut(&MirWitness)>(kind: &WitnessKind, visit: &mut F) {
    match kind {
        WitnessKind::Literal(_) | WitnessKind::Variable(_) => {}
        WitnessKind::Parallel { body } => walk_witness(body, visit),
        // v0.104.6 D472：4 类新构造 —— 遍历其子 witness，
        // 否则 hover/补全看不到 worker 体 / 事务补偿段 / eval 的 given 与 expects。
        WitnessKind::Worker { body, .. } => walk_witness(body, visit),
        WitnessKind::Transaction {
            body, compensation, ..
        } => {
            walk_witness(body, visit);
            walk_witness(compensation, visit);
        }
        WitnessKind::Eval { given, expects, .. } => {
            walk_witness(given, visit);
            for e in expects {
                walk_witness(e, visit);
            }
        }
        WitnessKind::Aggregate { value, .. } => walk_witness(value, visit),
        // v0.104.6 D472：管道 + commit / rollback
        WitnessKind::Pipe { left, right, .. } => {
            walk_witness(left, visit);
            walk_witness(right, visit);
        }
        WitnessKind::Commit | WitnessKind::Rollback => {}
        WitnessKind::Spawn { target } => match target {
            crate::mir::witness::WitnessSpawnTarget::Call {
                args, named_args, ..
            } => {
                for arg in args {
                    walk_witness(arg, visit);
                }
                for (_, arg) in named_args {
                    walk_witness(arg, visit);
                }
            }
            crate::mir::witness::WitnessSpawnTarget::MethodCall {
                receiver,
                args,
                named_args,
                ..
            } => {
                walk_witness(receiver, visit);
                for arg in args {
                    walk_witness(arg, visit);
                }
                for (_, arg) in named_args {
                    walk_witness(arg, visit);
                }
            }
            crate::mir::witness::WitnessSpawnTarget::Expr(e) => walk_witness(e, visit),
        },
        WitnessKind::Select { arms } => {
            for arm in arms {
                match arm {
                    crate::mir::witness::WitnessSelectArm::Recv { channel, body, .. } => {
                        walk_witness(channel, visit);
                        walk_witness(body, visit);
                    }
                    crate::mir::witness::WitnessSelectArm::Send {
                        channel,
                        value,
                        body,
                    } => {
                        walk_witness(channel, visit);
                        walk_witness(value, visit);
                        walk_witness(body, visit);
                    }
                    crate::mir::witness::WitnessSelectArm::Timeout { ms, body } => {
                        walk_witness(ms, visit);
                        walk_witness(body, visit);
                    }
                    crate::mir::witness::WitnessSelectArm::Default { body } => {
                        walk_witness(body, visit);
                    }
                }
            }
        }
        WitnessKind::Export { decl, .. } => walk_witness(decl, visit),
        WitnessKind::Binary { left, right, .. } => {
            walk_witness(left, visit);
            walk_witness(right, visit);
        }
        WitnessKind::Call {
            args, named_args, ..
        } => {
            for arg in args {
                walk_witness(arg, visit);
            }
            for (_, arg) in named_args {
                walk_witness(arg, visit);
            }
        }
        WitnessKind::MethodCall {
            receiver,
            args,
            named_args,
            ..
        } => {
            walk_witness(receiver, visit);
            for arg in args {
                walk_witness(arg, visit);
            }
            for (_, arg) in named_args {
                walk_witness(arg, visit);
            }
        }
        WitnessKind::Closure { body, .. } => walk_witness(body, visit),
        WitnessKind::FnDef { body, .. } => walk_witness(body, visit),
        WitnessKind::Match { scrutinee, arms } => {
            walk_witness(scrutinee, visit);
            for arm in arms {
                walk_witness(&arm.body, visit);
            }
        }
        WitnessKind::If { cond, then, r#else } => {
            walk_witness(cond, visit);
            walk_witness(then, visit);
            if let Some(e) = r#else {
                walk_witness(e, visit);
            }
        }
        WitnessKind::List(items) => {
            for item in items {
                walk_witness(item, visit);
            }
        }
        WitnessKind::Dict(entries) => {
            for (_, value) in entries {
                walk_witness(value, visit);
            }
        }
        WitnessKind::DynTrait { expr, .. } => walk_witness(expr, visit),
        WitnessKind::LetBinding {
            value, init_body, ..
        } => {
            walk_witness(value, visit);
            walk_witness(init_body, visit);
        }
        WitnessKind::Assign { value, .. } => walk_witness(value, visit),
        WitnessKind::Loop { iterable, body, .. } => {
            walk_witness(iterable, visit);
            walk_witness(body, visit);
        }
        WitnessKind::While { cond, body } => {
            walk_witness(cond, visit);
            walk_witness(body, visit);
        }
        WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
            walk_witness(left, visit);
            walk_witness(right, visit);
        }
        WitnessKind::Return(value) => {
            if let Some(v) = value {
                walk_witness(v, visit);
            }
        }
        WitnessKind::Break(_) | WitnessKind::Continue(_) => {}
        WitnessKind::IndexAssign {
            object,
            index,
            value,
        } => {
            walk_witness(object, visit);
            walk_witness(index, visit);
            walk_witness(value, visit);
        }
        WitnessKind::TensorSlice { target, axes } => {
            walk_witness(target, visit);
            for ax in axes {
                match ax {
                    crate::mir::witness::WitnessSliceAxis::Index(w) => walk_witness(w, visit),
                    crate::mir::witness::WitnessSliceAxis::Range { start, stop, step } => {
                        if let Some(w) = start {
                            walk_witness(w, visit);
                        }
                        if let Some(w) = stop {
                            walk_witness(w, visit);
                        }
                        if let Some(w) = step {
                            walk_witness(w, visit);
                        }
                    }
                    crate::mir::witness::WitnessSliceAxis::Ellipsis
                    | crate::mir::witness::WitnessSliceAxis::NewAxis => {}
                }
            }
        }
        WitnessKind::NoGrad { body } => walk_witness(body, visit),
        WitnessKind::StructDef { fields, .. } => {
            for f in fields {
                if let Some(d) = &f.default {
                    walk_witness(d, visit);
                }
            }
        }
        WitnessKind::TraitDef { methods, .. } => {
            for m in methods {
                for p in &m.params {
                    if let Some(d) = &p.default {
                        walk_witness(d, visit);
                    }
                }
                if let Some(body) = &m.default_body {
                    walk_witness(body, visit);
                }
            }
        }
        WitnessKind::ImplDef { methods, .. } => {
            for m in methods {
                for p in &m.params {
                    if let Some(d) = &p.default {
                        walk_witness(d, visit);
                    }
                }
                walk_witness(&m.body, visit);
            }
        }
        WitnessKind::KernelDef { body, .. } => {
            for s in body {
                walk_witness(s, visit);
            }
        }
        WitnessKind::TypeAlias { .. }
        | WitnessKind::EnumDef { .. }
        | WitnessKind::Import(_)
        | WitnessKind::MacroDef { .. }
        | WitnessKind::EffectSig { .. }
        | WitnessKind::Sequence(_)
        | WitnessKind::Perform { .. }
        | WitnessKind::Handle { .. } => {}
        // v0.88: Quasiquote — 遍历各段（Quote/Unquote/UnquoteSplice）
        WitnessKind::Quasiquote { segments } => {
            for seg in segments {
                walk_witness(seg, visit);
            }
        }
    }
}

///  Collect every `(name, span)` pair introduced by `let` bindings or
///  `fn` definitions in the program. Used by completion/definition/hover.
pub fn collect_definitions_v3(exprs: &[MirWitness]) -> Vec<(String, crate::common::Span)> {
    let mut out = Vec::new();
    for expr in exprs {
        collect_definitions_in_expr(expr, &mut out);
    }
    out
}

fn collect_definitions_in_expr(expr: &MirWitness, out: &mut Vec<(String, crate::common::Span)>) {
    // v0.104.6 D132：删掉外层那段 `match expr.kind` —— 它记录**根节点**，
    // 而紧接着的 `walk_witness(expr, …)` **也会遍历根节点**（`visit` 闭包被
    // 施加于整棵子树，含 expr 自身）。于是每个 `let` / `fn` 定义被 push **两次**，
    // `definition` / `documentSymbol` 的结果里每个符号都出现两遍：
    //   实测 `[{"range":…line 1,char 1-2}, {"range":…line 1,char 1-2}]`
    // （`walk_witness` 已覆盖根节点，故这里只留它一个来源。）
    walk_witness(expr, &mut |e| match &e.kind {
        WitnessKind::LetBinding { name, .. } => {
            out.push((name.clone(), e.span));
        }
        WitnessKind::FnDef { name, .. } => {
            out.push((name.clone(), e.span));
        }
        _ => {}
    });
}

///  Collect every read site of `name` across the program.
pub fn collect_references_v3(exprs: &[MirWitness], name: &str) -> Vec<crate::common::Span> {
    let mut out = Vec::new();
    for expr in exprs {
        collect_references_in_expr(expr, name, &mut out);
    }
    out
}

fn collect_references_in_expr(expr: &MirWitness, name: &str, out: &mut Vec<crate::common::Span>) {
    walk_witness(expr, &mut |e| {
        if let WitnessKind::Variable(n) = &e.kind
            && n == name
        {
            out.push(e.span);
        }
    });
}

use std::collections::BTreeMap;

/// 文本位置 → 字节偏移
///
/// ⚠ **v0.104.6 D211**：`col` 是 **LSP `Position.character`，即 UTF-16 码元**，
/// 不是 char 索引。BMP 内两者相等，故此前一直没暴露；一旦一行里有
/// 星平面字符（emoji、部分汉字扩展、某些符号），两者就分叉。
///
/// 真实 `mora-lsp.exe` 会话实测（客户端发的是**正确**的 UTF-16 位置）：
///
/// ```text
/// 源码          print("😀", total)
/// `total`       char 索引 11 / UTF-16 12
/// didChange 在 UTF-16 12 处插入 "X"  →  文档变成 tXotal（期望 Xtotal）
/// rename total→sum（range 11..16）    →  print("😀",suml)（期望 sum）
/// ```
///
/// 而 rename 那条**把文件改坏了**：客户端按 UTF-16 应用我们发出的
/// `character`，于是覆盖到 `' '` + `tota` 上。didChange 那条更狠 ——
/// 客户端每敲一个键，**服务器自己的文档缓冲**就被写坏一次，而
/// hover / definition / 诊断全都基于这个缓冲。
///
/// 本函数是**全部入站位置的唯一入口**（hover / definition / references /
/// rename / completion / **增量同步**都走它），故改这一处即修全部入站方向。
/// 出站方向（我们发出的 `character`）见 `char_to_utf16_col`。
pub fn position_to_offset(text: &str, line: usize, col: usize) -> usize {
    let line_text = text.lines().nth(line).unwrap_or("");
    let col = utf16_to_char_col(line_text, col);
    let mut current_line = 0;
    let mut current_col = 0;
    for (i, c) in text.char_indices() {
        if current_line == line && current_col == col {
            return i;
        }
        if c == '\n' {
            current_line += 1;
            current_col = 0;
        } else {
            current_col += 1;
        }
    }
    text.len()
}

/// LSP `Position.character`（**UTF-16 码元**）→ 本行的 **char 索引**。
///
/// `col` 越过行尾时**夹到行尾**（规范允许客户端给行尾之外的位置，
/// 服务端不得 panic）。空行返回 0。
pub fn utf16_to_char_col(line: &str, col: usize) -> usize {
    let mut units = 0usize;
    for (i, c) in line.chars().enumerate() {
        if units >= col {
            return i;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

/// 本行的 **char 索引** → LSP `Position.character`（**UTF-16 码元**）。
///
/// 供**出站**方向使用：凡是向客户端发出 `Position.character` 的地方，
/// 都必须过这一层，否则带 emoji 的行上客户端会把范围解释错
/// （highlight 偏位、rename 改坏文件）。`col` 越过行尾时夹到行尾。
pub fn char_to_utf16_col(line: &str, col: usize) -> usize {
    let mut units = 0usize;
    for (i, c) in line.chars().enumerate() {
        if i >= col {
            break;
        }
        units += c.len_utf16();
    }
    units
}

/// 创建 LSP completion item
pub(super) fn make_completion(label: &str, kind: f64, detail: Option<&str>) -> JsonValue {
    let mut m = BTreeMap::new();
    m.insert("label".to_string(), JsonValue::String_(label.to_string()));
    m.insert("kind".to_string(), JsonValue::Number(kind));
    if let Some(d) = detail {
        m.insert("detail".to_string(), JsonValue::String_(d.to_string()));
    }
    JsonValue::Object(m)
}

/// Resolve `offset` to `(ident, ident_start_col_1based)` if the cursor touches an identifier.
pub(super) fn ident_with_col_at_offset(text: &str, offset: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return None;
    }
    let mut start = offset;
    while start > 0 {
        let prev = bytes[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = offset;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            end += 1;
        } else {
            break;
        }
    }
    if start == end {
        return None;
    }
    let ident = std::str::from_utf8(&bytes[start..end]).ok()?.to_string();
    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col_1based = text[line_start..start].chars().count() + 1;
    Some((ident, col_1based))
}

/// 在某 offset 取一个标识符（变量名）
pub(super) fn ident_at_offset(text: &str, offset: usize) -> Option<String> {
    ident_with_col_at_offset(text, offset).map(|(ident, _)| ident)
}

/// Format a resolved HM type for LSP hover/completion, returning `None` when unresolved.
pub(super) fn format_hm_type(ty: &crate::typeck::Type) -> Option<String> {
    use crate::typeck::{Type, TypeVarId};
    match ty {
        Type::Error | Type::Unknown | Type::TypeVar(TypeVarId::Meta(_)) => None,
        Type::ForAll(vars, inner) if vars.is_empty() => format_hm_type(inner),
        other => Some(other.name()),
    }
}
