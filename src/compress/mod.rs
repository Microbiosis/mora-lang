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

// crush_json 顶层算法 — 在 Task 2 完成后才 re-export:
//   pub use json::crush_json_core;
// 现在不写 re-export, 避免 Task 1 引用未定义符号 (json::crush_json_core)

// Task 5 之前 compress_top 留作 stub, Task 5 填实
pub fn compress_top(
    _input: &crate::value::Value,
    _strategy: &str,
    _options: &CompressOptions,
) -> Result<crate::value::Value, String> {
    // Task 5 实装
    Err("compress_top: not yet implemented (v0.29 Task 5)".to_string())
}

/// v0.29: 极简 JSON 解析 (复用 v0.28 json.parse 风格 — 这里是简化版)
/// MVP 实现: 暂返回 None, Task 5 之前所有 crush_json 测试用 Rust 构造的 Value
/// Task 5 之后从 json.parse builtin 借路径
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
}
