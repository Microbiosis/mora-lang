//! v0.104.6 D487：Core 模型的**零死变体**普查。
//!
//! ## 为什么
//!
//! Core 层此前有 **9 个零生产者的变体**：
//!
//! | 变体 | 为什么是死的 |
//! |---|---|
//! | `Var` | 与 `EnvLoad` **重复**（降维发的是 `EnvLoad`） |
//! | `Return` / `Unreachable` | 与**终结指令** `CoreTerminator::Return` / `CoreTerminator::Unreachable` 重复 |
//! | `Phi` | 无人插入（Core 层由 SSA 前的分支汇合承担） |
//! | `EnumConstruct` / `StructConstruct` / `StructAccess` | 语言层**没有**构造语法，MIR 侧也**无对应指令** |
//! | `ThunkCreate` / `ThunkForce` | MIR 侧**根本没有** thunk 指令（Core 的「与 MirInst 同款」是**假的**） |
//!
//! 后果有两处：① Core 模型里摆着**没人会生产**的指令；② 执行器为它们各留了
//! 一个「不支持」分支 ⇒「执行器支持模型的**每一个**变体」这句话**不成立**。
//!
//! D487 把它们全删了 ⇒ 模型的每个变体都**有生产者**。
//!
//! ## 判据
//!
//! `CoreInst` 的**每个**变体都必须在降维（`src/mir/ehir_to_core.rs` —— Core 层
//! 的**唯一**生产点）里**有生产者** ⇒ 零死变体。
//!
//! ## 与编译器的分工
//!
//! - **编译器**保证：执行器的 match **穷尽且无报错分支** ⇒ 执行器覆盖模型的
//!   每个变体；
//! - **本判据**保证：模型的每个变体都**有人生产**。
//!
//! 两者合起来 ⇒ 模型与执行器**互不落空**（没有「有指令但没人发」，也没有
//! 「发了但执行不了」）。

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// 从 `src/mir/core.rs` 抽出 `CoreInst` 的变体名。
///
/// ⚠ **不能用括号配平**来找枚举的结束位置 —— 变体的文档注释里有
/// ``Scope{kind:"orchestrate"}`` 这类**不成对的花括号** ⇒ 配平会提前结束、
/// 只抽到一半的变体。⇒ 改用**行结构**：枚举的结束就是**顶格**的 `}`。
fn core_inst_variants() -> Vec<String> {
    let path = repo_root().join("src").join("mir").join("core.rs");
    let text = fs::read_to_string(&path).expect("读 core.rs");
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("pub enum CoreInst {"))
        .expect("core.rs 里应有 `pub enum CoreInst {`");
    let mut out = Vec::new();
    for l in lines.iter().skip(start + 1) {
        if l.trim_end() == "}" {
            break;
        }
        // 变体形如 `    Name {` / `    Name(` / `    Name,`（**4 空格缩进**）。
        let rest = l.strip_prefix("    ").unwrap_or("");
        if !rest.starts_with(' ') && !rest.is_empty() {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let after = rest[name.len()..].trim_start();
            if !name.is_empty()
                && name.starts_with(|c: char| c.is_uppercase())
                && (after.starts_with('{') || after.starts_with('(') || after.starts_with(','))
            {
                out.push(name);
            }
        }
    }
    out
}

/// Core 层的**唯一**生产点：EHIR → Core 降维。
fn lowering_source() -> String {
    let path = repo_root().join("src").join("mir").join("ehir_to_core.rs");
    fs::read_to_string(&path).expect("读 ehir_to_core.rs")
}

/// 判据：`CoreInst` 的每个变体都**有生产者**（零死变体）。
#[test]
fn every_core_inst_variant_has_a_producer() {
    let variants = core_inst_variants();
    assert!(
        variants.len() >= 20,
        "变体抽取疑似失效：只抽到 {} 个（core.rs 的结构变了？）",
        variants.len()
    );
    let lowering = lowering_source();
    let dead: Vec<&String> = variants
        .iter()
        .filter(|v| !lowering.contains(&format!("CoreInst::{}", v)))
        .collect();
    assert!(
        dead.is_empty(),
        "Core 模型有**零生产者的死变体**：{dead:?} —— \
         它们降维从不产生、MIR 侧也未必有对应指令 ⇒ 是**虚构**的 IR。\
         ⇒ 删掉它们（连同执行器里为它们留的「不支持」分支），\
         或给它们补上**真正的**生产者（那就得先在 witness / FCFG 层有表示）。"
    );
}

/// 判据：Core 执行器**没有**「不支持」的报错分支 —— 它覆盖模型的每个变体。
///
/// 与编译器互补：编译器保证 match **穷尽**；本判据保证没有哪个变体是
/// 「穷尽但报错」的那种（那等于「模型里有、执行器不认」）。
#[test]
fn the_core_executor_supports_every_variant() {
    let path = repo_root().join("src").join("mir").join("core_exec.rs");
    let text = fs::read_to_string(&path).expect("读 core_exec.rs");
    // 只看 `exec_inst` 的函数体（第一个 match 之后那段）—— 文档与模块注释
    // 里可以谈「不支持」的历史。
    let body = text
        .split("fn exec_inst(")
        .nth(1)
        .expect("core_exec.rs 里应有 exec_inst");
    let body = body.split("/// 实参值列表").next().unwrap_or(body);
    // 去掉**注释**（`//` 之后）—— 否则本文件/源码里的说明性文字会误判。
    let code: String = body
        .lines()
        .map(|l| l.split("//").next().unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("不支持"),
        "Core 执行器里仍有「不支持」的分支 ⇒ 「执行器覆盖模型的每个变体」不成立"
    );
}
