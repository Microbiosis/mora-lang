//! v0.104.6 D253（v0.110 W2.3 迁至 Control IR）：
//! 纯度白名单与 GVN/CSE 白名单守卫 —— 禁止把**确定有副作用**或**可失败**的指令收进纯白名单。
//!
//! 真正危险的方向只有一个（v0.87 `gensym` 教训）：
//! 对有副作用的指令标纯或合并成一次，会导致副作用被跳过或重复丢失。
//!
//! W2.3 将优化器收敛为单一路线：
//! - `src/mir/control/ir.rs::ControlInst::purity()` 是唯一权威纯度表（LICM / DCE 共用）
//! - `src/mir/control/opt.rs::gvn_cse()` 的 `ExprKey` 匹配决定可 CSE/GVN 合并的指令

use std::path::Path;

/// 从 `ControlInst::purity()` 提取 `=> Purity::Pure` 分支覆盖的所有变体名。
fn pure_purity_variants() -> Vec<String> {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mir/control/ir.rs");
    let src = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("读 ir.rs 失败: {e}"));
    let start = src
        .find("pub fn purity(&self) -> Purity")
        .expect("control/ir.rs 里找不到 ControlInst::purity");
    let tail = &src[start..];
    let end = tail
        .find("=> Purity::Pure")
        .expect("purity() 中找不到 `=> Purity::Pure`");
    let mut out: Vec<String> = tail[..end]
        .split("Self::")
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

/// 从 `gvn_cse`（`src/mir/control/opt.rs`）中抽出参与值编号的 `ControlInst::Xxx` 变体名。
fn cse_variants() -> Vec<String> {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mir/control/opt.rs");
    let src = std::fs::read_to_string(&full).expect("read control/opt.rs");
    let start = src.find("fn gvn_cse").expect("找不到 gvn_cse");
    let tail = &src[start..];
    let end = tail.find("\n}").expect("gvn_cse 结尾找不到");
    let mut out: Vec<String> = tail[..end]
        .split("ControlInst::")
        .skip(1)
        .filter_map(|s| {
            let name: String = s
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            // 排除重写目标 `ControlInst::Copy(dst, leader)`
            if name.is_empty() || name == "Copy" {
                None
            } else {
                Some(name)
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 这些指令**确定有副作用或会读写状态**，绝不能进入 `Purity::Pure` 或 GVN/CSE。
const PROVABLY_EFFECTFUL: &[&str] = &[
    "Call",
    "ModuleCall",
    "MethodCall",
    "Pipe",
    "LoadLocal",
    "StoreLocal",
    "LoadUpvalue",
    "StoreUpvalue",
    "LoadGlobal",
    "StoreGlobal",
    "LoadDyn",
    "DefineDyn",
    "AssignDyn",
    "ExportGlobal",
    "ExportDyn",
    "IndexAssign",
    "MatchPattern",
    "MakeClosure",
    "Handle",
    "Transaction",
    "Parallel",
    "Import",
    "EvalAssert",
    "Commit",
    "Rollback",
];

/// ① **主判据（有牙齿，v0.87 同款）**：确定有副作用的指令不得进 `Purity::Pure`。
#[test]
fn d253_memoizable_whitelist_excludes_provably_effectful_insts() {
    let pure = pure_purity_variants();
    assert!(!pure.is_empty(), "判据自身失效：Pure 白名单解析为空");
    let bad: Vec<&String> = pure
        .iter()
        .filter(|v| PROVABLY_EFFECTFUL.contains(&v.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "Purity::Pure 收进了确定有副作用的指令：{bad:?}。当前 Pure = {pure:?}"
    );
}

/// ② 同一条，对 GVN/CSE 白名单。
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

/// ③ 快照：把允许进入 `Purity::Pure` 与 `gvn_cse` 的指令集合钉死。
#[test]
fn d253_whitelist_contents_are_pinned() {
    assert_eq!(
        pure_purity_variants(),
        vec![
            "Const",
            "Copy",
            "DictLit",
            "ListLit",
            "MakeDynTrait",
            "Unary",
        ],
        "Purity::Pure 白名单内容变了 —— 若是有意的，请在此处留痕并说明理由"
    );
    assert_eq!(
        cse_variants(),
        vec!["Const", "Unary"],
        "GVN/CSE 白名单内容变了 —— 若是有意的，请在此处留痕并说明理由"
    );
}

/// ④ 对照组：证明 ① 的规则本身有判别力（否则「全绿」没有意义）。
#[test]
fn d253_control_group_provable_effect_list_is_real() {
    for inst in ["Call", "MethodCall", "LoadGlobal", "IndexAssign"] {
        assert!(
            PROVABLY_EFFECTFUL.contains(&inst),
            "PROVABLY_EFFECTFUL 漏了 {inst} —— 判据的判别力下降"
        );
    }
    let fake = ["Call".to_string(), "Const".to_string()];
    let caught: Vec<&String> = fake
        .iter()
        .filter(|v| PROVABLY_EFFECTFUL.contains(&v.as_str()))
        .collect();
    assert_eq!(caught.len(), 1, "规则对 Call 竟然没反应 —— ① 恒绿");
    let pure = pure_purity_variants();
    assert!(
        pure.contains(&"Const".to_string()) && !PROVABLY_EFFECTFUL.contains(&"Const"),
        "Const 在 Purity::Pure 中且不应落入 PROVABLY_EFFECTFUL; pure = {pure:?}"
    );
}
