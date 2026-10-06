//! v0.104.6 D386 —— 全仓 **`pub fn` 零引用普查**：37 / 815 是「完整但未接入」的库能力
//!
//! 「功能未接通」是本会话最常见的结论类型（D346 `xform` / D356 / D360
//! `orchestrate loop` / D361 `IndexAssign` / D363 / D385）。
//! 但那些都是**个案** —— 本轮做**全仓普查**，把这类结论一次性量化。
//!
//! ## 普查方法
//!
//! `#[allow(dead_code)]` 全仓 **0 处** —— 编译器视角没有死代码
//! （`pub` 项本来就不算 dead）。所以只能**人工判调用链**：
//! 对每个 `pub fn`，数它在全仓 `src/` 出现几次；只出现 1 次 ⇒ 只有定义、零引用。
//!
//! ## 结果：815 个 `pub fn` 里 **37 个**零引用
//!
//! 按模块分组：
//!
//! | 模块 | 零引用数 | 代表 |
//! |---|---|---|
//! | `value.rs` / `value/list.rs` | 6 | `is_exported` / `is_moved` / `reversed` / `sorted_by_key` / `to_mut_vec` / `into_value` |
//! | `typeck/` | 7 | `is_empty_union` / `enter_scope` / `exit_scope` / `push_scope` / `pop_scope` / `fresh_closure` / `is_builtin_type_name` |
//! | `pregel/` + `trace_collector/` | 6 | `with_step_timeout` / `set_otel_endpoint` / `metrics_json` |
//! | `mir/` + `flow.rs` + `orchestrate_dag/` | 8 | `run_mir_with_signal_cached` / `label_index` / `is_pipe_method` |
//! | `interpreter/` + `tea/` + `toolplane/` | 6 | `save_checkpoint` / `dispatch_many` / `shared_default_registry` |
//!
//! ## 抽查结论：全是**库 API 备用**，不是「写残的半成品」
//!
//! - `is_moved(&self) -> bool { matches!(self, Binding::Moved) }` —— 语义完整
//! - `reversed(&self) -> List { let mut v = self.to_vec(); … }` —— 完整实现
//! - `to_mut_vec` 带着**解释性注释**（不可变 list 语义下如何取回可改副本）
//! - `is_empty_union` 判 `Type::Union(m) if m.is_empty()` —— 完整
//!
//! ⇒ 属 D385 建立的**三层**分层里的「底层实现 + 上层无入口」，
//! **不是** D361 那种「定义了但零调用」的真死代码。
//!
//! **判定：全部保留，不擅动。**
//! 库 API 备用是正常设计（未来接线、或给下游用）；
//! 删掉它们的风险高于收益。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 递归收集 `src/` 下所有 `.rs`。
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 全仓 `pub fn` 名字 → (文件相对路径, 全仓出现次数)。
fn pub_fn_census() -> BTreeMap<String, (String, usize)> {
    let mut files = Vec::new();
    collect(&src_root(), &mut files);
    let mut sources = Vec::new();
    for p in &files {
        sources.push(fs::read_to_string(p).unwrap_or_default());
    }
    let all: String = sources.join("\n");

    let mut out = BTreeMap::new();
    for (i, p) in files.iter().enumerate() {
        let rel = p
            .strip_prefix(src_root())
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        for line in sources[i].lines() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("pub fn ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let n = all.matches(&name).count();
            out.insert(name, (rel.clone(), n));
        }
    }
    out
}

/// **普查规模**：`pub fn` 总数落在 `[600, 1200]`。
///
/// 低于下界说明抽样的源码树变了；高于上界说明**新增了大量 public API**，
/// 应重新做一次「零引用」分类。
#[test]
fn d386_pub_fn_census_scale_is_stable() {
    let c = pub_fn_census();
    assert!(
        (600..=1200).contains(&c.len()),
        "`pub fn` 总数 {} 超出 [600, 1200] —— 源码树结构变了？",
        c.len()
    );
}

/// **零引用的 `pub fn` 数量**落在 `[20, 60]`。
///
/// 这是「完整但未接入」的量化基线。突破上界说明**新增了一批未接线的库能力**
/// （值得逐个看）；跌破下界说明大批能力**被接上了**（好消息）。
#[test]
fn d386_zero_reference_pub_fn_count_is_bounded() {
    let c = pub_fn_census();
    let zero: Vec<&String> = c
        .iter()
        .filter(|(_, (_, n))| *n <= 1)
        .map(|(k, _)| k)
        .collect();
    assert!(
        (20..=60).contains(&zero.len()),
        "零引用 `pub fn` = {} 个，超出 [20, 60]。\
         清单：{:?}",
        zero.len(),
        zero
    );
}

/// **几个代表性条目**确实零引用（钉住具体事实，而非总数）。
#[test]
fn d386_representative_zero_reference_functions() {
    let c = pub_fn_census();
    for name in [
        // D385 确认过的「底层齐全、上层无入口」
        "is_exported",
        "is_moved",
        "is_borrowed_mut",
        "into_value",
        "reversed",
        "sorted_by_key",
        "to_mut_vec",
        // typeck
        "is_empty_union",
        "enter_scope",
        "exit_scope",
        // mir / pregel / trace
        "label_index",
        "has_passthrough_inst",
        "with_step_timeout",
        "set_otel_endpoint",
        "metrics_json",
    ] {
        let Some((rel, n)) = c.get(name) else {
            panic!("`pub fn {name}` 不在普查结果里 —— 源码树可能变了（被删或改名）");
        };
        assert!(
            *n <= 1,
            "`{name}`（{rel}）现在全仓出现 {n} 次 —— 它**已被接入**了，\
             请重新分类（可能是好改动，也可能是新暴露的未接线 API）"
        );
    }
}

/// **`#[allow(dead_code)]` 保持 0 处**。
///
/// 若将来有人用 `#[allow(dead_code)]` 压掉新警告，
/// 编译器视角的死代码就会**静默** —— 本条守住这个信号。
#[test]
fn d386_no_dead_code_allow_attributes() {
    let mut files = Vec::new();
    collect(&src_root(), &mut files);
    let mut hits = Vec::new();
    for p in &files {
        let s = fs::read_to_string(p).unwrap_or_default();
        if s.contains("#[allow(dead_code)]") {
            hits.push(
                p.strip_prefix(src_root())
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    assert!(
        hits.is_empty(),
        "出现 `#[allow(dead_code)]` 于 {hits:?} —— 编译器视角的死代码会**静默**，\
         本判据的「零引用普查」将失去交叉验证能力"
    );
}
