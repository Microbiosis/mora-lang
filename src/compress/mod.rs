//! v0.29: 统一压缩原语 — `SubCompressor` trait + `ContentRouter`
//!
//! 灵感: headroom (https://github.com/headroomlabs-ai/headroom)
//! ContentRouter + Kneedle + 异常保留 设计。

use std::sync::Arc;

/// v0.29: 压缩选项 (跨子压缩器共享)
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// "auto" | "head_tail" | "summary" | "lossless" | "json"
    pub strategy: String,
    /// head_tail: 保留首 N% (0.0-1.0); 默认 0.3
    pub head_pct: f32,
    /// head_tail: 保留尾 M% (0.0-1.0); 默认 0.3
    pub tail_pct: f32,
    /// 顶层 builtin 显式传入的字节上限
    pub max_bytes: Option<usize>,
    /// crush_json 异常保留字段; 默认 ["error", "anomaly", "status", "alert"]
    pub anomaly_keys: Vec<String>,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            strategy: "auto".into(),
            head_pct: 0.3,
            tail_pct: 0.3,
            max_bytes: None,
            anomaly_keys: vec![
                "error".into(), "anomaly".into(), "status".into(), "alert".into(),
            ],
        }
    }
}

/// v0.29: 子压缩器 trait
/// 所有 5 个内置子压缩器 (json/code/html/log/text) 必须实现此 trait。
pub trait SubCompressor: std::fmt::Debug + Send + Sync {
    /// 嗅探该子压缩器是否适用于给定内容（≥ 0.6 信心）
    fn sniff(&self, content: &str) -> f32;

    /// 压缩到不超过 max_bytes (UTF-8 字节)。
    /// options 携带 strategy 名称 + head_pct / tail_pct / anomaly_keys 等;
    /// 对不关心的子压缩器 (Json/Code/Html/Log) 忽略 options.
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        options: &CompressOptions,
    ) -> Result<String, String>;

    /// 子压缩器身份: "json" | "code" | "html" | "log" | "text"
    fn origin(&self) -> &'static str;
}

/// v0.29: 内容路由器 — 按 sniff 分数选最佳子压缩器
pub struct ContentRouter {
    compressors: Vec<Arc<dyn SubCompressor>>,
}

impl ContentRouter {
    /// 创建空路由器 (Task 1 中; Task 3-5 完成后改为 default_router)
    pub fn empty() -> Self {
        Self { compressors: vec![] }
    }

    /// 默认路由器 (Task 2: 加入 JsonSubCompressor; Task 3 加入 TextSubCompressor;
    /// Task 4 加入 Code/Html/Log。)
    /// 顺序约定: 置信度高的 SC 优先注册 (router::sniff 在 ≥ 0.6 中再选 max,
    /// 注册顺序不影响最终选择, 但 json/code/html/log 都在 text 之前 —
    /// text sniff 固定 0.5, 故意兜底)。
    pub fn default_router() -> Self {
        let mut r = Self::empty();
        r.add(Arc::new(json::JsonSubCompressor));
        r.add(Arc::new(code::CodeSubCompressor));
        r.add(Arc::new(html::HtmlSubCompressor));
        r.add(Arc::new(log::LogSubCompressor));
        r.add(Arc::new(text::TextSubCompressor));
        r
    }

    /// 注册子压缩器
    pub fn add(&mut self, c: Arc<dyn SubCompressor>) {
        self.compressors.push(c);
    }

    /// 嗅探: 找 sniff 分 ≥ 0.6 的最高分; 无 → None
    pub fn sniff(&self, content: &str) -> Option<Arc<dyn SubCompressor>> {
        self.compressors
            .iter()
            .filter_map(|c| {
                let score = c.sniff(content);
                if score >= 0.6 { Some((score, c.clone())) } else { None }
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, c)| c)
    }
}

// 子压缩器子模块 (Tasks 3-5 填充)
pub mod json;   // Task 2 填充
pub mod code;   // Task 4 填充
pub mod html;   // Task 4 填充
pub mod log;    // Task 4 填充
pub mod text;   // Task 3 填充

// v0.29: re-export crush_json_core (Task 2 已定义; Task 5 启用 re-export)
pub use json::crush_json_core;

/// v0.29: 从 Value 中提取可压缩的纯文本。
///
/// 支持:
/// - `Value::String(s)` → 直接使用 `s`
/// - `Value::Conversation { messages, .. }` → 每条格式化为 `role: content`, 用 `\n` 连接
/// - `Value::List` 项是 `Value::Dict{role, content}` → 同样格式化为 `role: content`
/// - 其它 → 错误
pub fn extract_text(input: &crate::value::Value) -> Result<String, String> {
    use crate::value::Value;
    match input {
        Value::String(s) => Ok(s.clone()),
        Value::Conversation { messages, .. } => {
            let lines: Vec<String> = messages
                .iter()
                .map(|(role, content)| format!("{}: {}", role, content))
                .collect();
            Ok(lines.join("\n"))
        }
        Value::List(items) => {
            let mut lines: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Dict(d) => {
                        let role = d.get("role").map(|v| v.to_string()).unwrap_or_default();
                        let content = d.get("content").map(|v| v.to_string()).unwrap_or_default();
                        if role.is_empty() && content.is_empty() {
                            // 没 role/content 字段: 退回到整项 to_string()
                            lines.push(item.to_string());
                        } else {
                            lines.push(format!("{}: {}", role, content));
                        }
                    }
                    other => lines.push(other.to_string()),
                }
            }
            Ok(lines.join("\n"))
        }
        other => Err(format!(
            "compress: expected Conversation / list of {{role, content}} / string, got {}",
            value_type_simple(other)
        )),
    }
}

/// v0.29: 从 `Value::Dict` 构建 `CompressOptions`。
///
/// 部分字段缺失或类型不匹配时**静默跳过**(不报错) — 调用方可任意选择传入哪些 options。
pub fn options_from_value(v: &crate::value::Value) -> Result<CompressOptions, String> {
    use crate::value::Value;
    let mut opts = CompressOptions::default();
    if let Value::Dict(map) = v {
        if let Some(Value::String(s)) = map.get("strategy") {
            opts.strategy = s.clone();
        }
        if let Some(Value::Number(n)) = map.get("head_pct") {
            opts.head_pct = *n as f32;
        }
        if let Some(Value::Number(n)) = map.get("tail_pct") {
            opts.tail_pct = *n as f32;
        }
        if let Some(Value::Number(n)) = map.get("max_bytes") {
            opts.max_bytes = Some(*n as usize);
        }
        if let Some(Value::List(keys)) = map.get("anomaly_keys") {
            opts.anomaly_keys = keys
                .iter()
                .filter_map(|k| match k {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect();
        }
    }
    Ok(opts)
}

/// v0.29: `compress(input, strategy, options)` 顶层 builtin 的核心实现。
///
/// Strategy 调度 (per spec §6.6):
/// - `"auto"`           → 路由器选最佳子压缩器 (json/code/html/log/text 5 个)
/// - `"head_tail"`      → TextSubCompressor (按 head_pct/tail_pct/max_bytes)
/// - `"summary"`        → TextSubCompressor (mock LLM, 真实 LLM 留 v0.30)
/// - `"lossless"`       → TextSubCompressor (原文本 + original_size marker)
/// - `"json"`           → crush_json_core (input 必须是 List 或 JSON 数组字符串)
/// - 其它               → 报错
pub fn compress_top(
    input: &crate::value::Value,
    strategy: &str,
    options: &CompressOptions,
) -> Result<crate::value::Value, String> {
    // "json" strategy 不走文本路径, 直接用原始 input 调 crush_json_core
    if strategy == "json" {
        let max_items = options.max_bytes.unwrap_or(8192) / 200;
        let max_items = max_items.max(1);
        let v = crush_json_core(input, max_items, &options.anomaly_keys)?;
        let json = value_to_json_simple(&v);
        let item_count = if let crate::value::Value::List(l) = &v {
            l.len()
        } else {
            0
        };
        return Ok(crate::value::Value::String(format!(
            "{}\n<compressed:method=crush_json items={} max={}>",
            json, item_count, max_items
        )));
    }

    // 其余 strategy 都需先提取文本
    let text = extract_text(input)?;
    let max_bytes = options.max_bytes.unwrap_or(8192);

    match strategy {
        "auto" => {
            let router = ContentRouter::default_router();
            let comp = router.sniff(&text).ok_or_else(|| {
                "compress.auto: no compressor matched for content".to_string()
            })?;
            let out = comp.compress(&text, max_bytes, options)?;
            Ok(crate::value::Value::String(out))
        }
        "head_tail" | "summary" | "lossless" => {
            let text_comp = text::TextSubCompressor;
            let out = text_comp.compress(&text, max_bytes, options)?;
            Ok(crate::value::Value::String(out))
        }
        other => Err(format!("compress: unknown strategy '{}'", other)),
    }
}

/// v0.29: 极简 JSON 解析 (stub) — 始终返回 `None`。
///
/// **v0.30 follow-up**: 这里应当委托给 `src/interpreter/dispatch.rs` 的
/// `json_to_value` 真实解析器 (v0.10 已实现, 无新外部依赖)。当前 stub 行为
/// 是设计上的已知 gap: `JsonSubCompressor::compress` 接受任何 JSON 字符串
/// 输入都会返回 `crush_json: json.parse failed`。v0.29 demo 用 Rust 构造的
/// `Value::List` 走 `compress_top(json_strategy)` 路径, 不经此 stub。
///
/// 选择 Option B (document + regression test) 而非 Option A (直接引入
/// `serde_json` 作为直接依赖): v0.29 计划的 Global Constraint 明确禁止
/// "新增外部依赖"。`serde_json` 虽是 transitive (经 `undoc`), 加为直接
/// 依赖会破坏该约束; 通过 `dispatch.rs::json_to_value` 内部委托同样无新
/// 依赖, 留作 v0.30 跟进。
///
/// 相关测试: `parse_json_simple_currently_stub` (锁住当前行为)。
pub fn parse_json_simple(_s: &str) -> Option<crate::value::Value> {
    None
}

/// v0.29: Value → JSON 字符串 (用 Value::Display)
pub fn value_to_json_simple(v: &crate::value::Value) -> String {
    v.to_string()
}

/// v0.29: Value 类型名
pub fn value_type_simple(v: &crate::value::Value) -> &'static str {
    use crate::value::Value;
    match v {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_router_returns_none() {
        let r = ContentRouter::empty();
        assert!(r.sniff("anything").is_none());
    }

    #[test]
    fn compress_options_default() {
        let opts = CompressOptions::default();
        assert_eq!(opts.head_pct, 0.3);
        assert!(opts.anomaly_keys.contains(&"error".to_string()));
    }

    /// v0.29 final review MEDIUM regression test:
    /// 锁住 `parse_json_simple` 当前 stub 行为 (`None`), 防止回归到 v0.30
    /// follow-up 落地前的隐性状态变化。v0.30 实现后此测试应被替换为断言
    /// 真实 JSON 解析的 positive case。
    #[test]
    fn parse_json_simple_currently_stub() {
        assert!(crate::compress::parse_json_simple("[1,2,3]").is_none());
        assert!(crate::compress::parse_json_simple("{\"a\":1}").is_none());
        assert!(crate::compress::parse_json_simple("").is_none());
        assert!(crate::compress::parse_json_simple("not json").is_none());
    }
}
