//! v0.104.6 D387 —— `TraceCollector` 的**采集侧接上了、开启路径零调用**
//! （否定轮，含一项待裁决）
//!
//! D386 的零引用普查列出 `trace_collector.rs` 的 3 个零引用 `pub fn`
//! （`set_enabled` / `set_otel_endpoint` / `metrics_json`），
//! 只说「库 API 备用」。本轮**查清整条链**，结论比「备用」更具体。
//!
//! ## 采集侧**真的在跑**
//!
//! | 位置 | 用途 |
//! |---|---|
//! | `interpreter/mod.rs:853` | `self.ai.trace = TraceCollector::new(enabled)` |
//! | `ai_chat.rs:315` | `self.ai.trace.start_span("ai.chat", span_attrs)` |
//! | `ai_chat.rs:327/331` | `self.ai.trace.record_call("ai.chat", …)` |
//! | `ai_helpers.rs:163` | `self.ai.trace.record_tokens(…)` |
//!
//! ⇒ 不是死代码，**span / 调用计数 / token 统计都接好了**。
//!
//! ## 但**没有任何生产路径开启它**
//!
//! 唯一开关是 `Interpreter::set_trace_enabled(bool)`（`interpreter/mod.rs:852`），
//! 而它的调用方**只有** `runtime/ai.rs:135` 的**单测**
//! （`ai.rs:137` 里的 `ai.set_trace_enabled(true)`）。
//!
//! ⇒ 生产环境永远是 `TraceCollector::new(false)` ⇒
//! **所有 span / metrics 记录被丢弃**。
//!
//! ## 为什么零引用的是那 3 个
//!
//! | 零引用 | 性质 |
//! |---|---|
//! | `set_enabled` | 实例级开关，被 `Interpreter::set_trace_enabled`（整体替换）取代 |
//! | `set_otel_endpoint` | OTLP 端点配置 —— 但 `export_otel_json` 也只在 host 注释里出现 |
//! | `metrics_json` | 指标导出 —— **没有任何 HTTP 端点或 CLI 暴露它** |
//!
//! ⇒ 读侧（导出/上报）整条链**没有出口**：即使开启采集，
//! 采到的数据也**没有地方能取出来**。
//!
//! ## 判定：属**待裁决**，不擅动
//!
//! 「可观测性设施已实现但未启用/未暴露」是**产品范围**问题
//! （要不要开、要不要暴露 HTTP 端点），不是实现缺陷。
//! 判据只把**现状**钉住。

use std::fs;
use std::path::Path;

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

/// 递归读 `src/` 下所有 `.rs` 并拼成一份。
///
/// ⚠ 首版写成 `read("src")` —— 那是**目录**，`read_to_string` 直接
/// `os error 5`（拒绝访问）。3 条判据一起红在「读 src 失败」，
/// 而第 4 条（只读具体文件）照常通过 ⇒ 又一次「全红=装置问题」。
fn read_src_tree() -> String {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(fs::read_to_string(&p).unwrap_or_default());
            }
        }
    }
    let mut v = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut v);
    v.join("\n")
}

/// **`set_trace_enabled` 在生产代码里零调用**（只在单测里）。
///
/// 这是本文件的核心事实。判定为「现状钉住」：
/// 若将来有人接上 CLI flag 或环境变量，本条会红 —— 那是有意启用，需同步更新。
#[test]
fn d387_set_trace_enabled_has_no_production_caller() {
    let src = read_src_tree();
    let mut hits = Vec::new();
    for line in src.lines() {
        if !line.contains("set_trace_enabled") {
            continue;
        }
        // `#[cfg(test)]` 块内的调用算「单测」
        hits.push(line.trim().to_string());
    }
    // 两处**定义**（`interpreter/mod.rs` 与 `runtime/ai.rs` 各一）+
    // 一处**单测**内的调用；**零**生产调用。
    let defs = hits.iter().filter(|l| l.contains("pub fn")).count();
    let calls: Vec<&str> = hits
        .iter()
        .map(String::as_str)
        .filter(|l| !l.contains("pub fn"))
        // 排除**单测函数自身**的声明行 `fn set_trace_enabled_updates_trace()`
        .filter(|l| !l.trim_start().starts_with("fn "))
        .collect();
    assert_eq!(
        defs, 2,
        "`set_trace_enabled` 应有 2 处定义（Interpreter / AiRuntime）; 实得 {defs}"
    );
    assert_eq!(
        calls.len(),
        1,
        "`set_trace_enabled` 应恰好 1 处**调用**（单测）; 实得 {calls:?} —— \
         多出来的若在生产路径，说明 trace 已被启用，本条结论需重新评估"
    );
    assert!(
        calls[0].contains("(true)"),
        "单测里应是 `set_trace_enabled(true)`; 实得 {:?}",
        calls[0]
    );
}

/// **采集侧真的在跑**（`start_span` / `record_call` / `record_tokens` 有调用方）。
///
/// 这条是**反向对照** —— 若采集侧也没接上，
/// 「未启用」就只是「整个设施不存在」，是另一回事。
#[test]
fn d387_collection_side_is_wired() {
    let chat = read("src/interpreter/ai_chat.rs");
    assert!(
        chat.contains("self.ai.trace.start_span(\"ai.chat\""),
        "`ai_chat.rs` 应调 `start_span`"
    );
    assert!(
        chat.contains("self.ai.trace.record_call(\"ai.chat\""),
        "`ai_chat.rs` 应调 `record_call`"
    );
    let helpers = read("src/interpreter/ai_helpers.rs");
    assert!(
        helpers.contains("self.ai.trace.record_tokens("),
        "`ai_helpers.rs` 应调 `record_tokens`"
    );
    let interp = read("src/interpreter/mod.rs");
    assert!(
        interp.contains("self.ai.trace = TraceCollector::new(enabled);"),
        "`Interpreter` 应持有 `TraceCollector`"
    );
}

/// **读侧整条链没有出口** —— 采到的数据没有任何地方能取出来。
///
/// `get_spans_json` / `export_otel_json` / `metrics_json` 只在
/// `host.rs` 的 **doc comment** 里被提及，**没有真实调用方**。
#[test]
fn d387_export_side_has_no_caller() {
    let host = read("src/mir/host.rs");
    for name in ["get_spans_json", "export_otel_json"] {
        // 允许出现在 doc comment，但**不能**是真实调用
        let real_call = host
            .lines()
            .any(|l| l.contains(name) && !l.trim_start().starts_with("//"));
        assert!(
            !real_call,
            "`host.rs` 里 `{name}` 出现了真实调用行 —— 导出侧可能已接通，需重新评估"
        );
    }
    // `metrics_json` 全仓（src 内）零引用
    let all = read_src_tree();
    assert_eq!(
        all.matches("metrics_json").count(),
        1,
        "`metrics_json` 只应在 trace_collector.rs 出现 1 次（定义）; \
         多出来说明导出侧已接通"
    );
}

/// **`set_otel_endpoint` 与 `set_enabled` 是实例级 API**，零引用。
///
/// 它们被 `Interpreter::set_trace_enabled` 的「整体替换」语义取代，
/// 是**有意的 API 冗余**（保留给未来直接操作 collector 的场景）。
#[test]
fn d387_instance_level_toggles_stay_unreferenced() {
    let tc = read("src/trace_collector.rs");
    assert!(
        tc.contains("pub fn set_enabled(&self, enabled: bool)"),
        "`set_enabled` 定义应仍在（API 备用）"
    );
    assert!(
        tc.contains("pub fn set_otel_endpoint(&self, endpoint: String)"),
        "`set_otel_endpoint` 定义应仍在（API 备用）"
    );
    let all = read_src_tree();
    assert_eq!(
        all.matches("set_otel_endpoint").count(),
        1,
        "`set_otel_endpoint` 应只有定义那一处；若被接入请重新评估"
    );
}
