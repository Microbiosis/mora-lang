//! v0.30: 统一压缩原语 — `SubCompressor` trait + `ContentRouter` + SmartCrusher
//!
//! 灵感: headroom (<https://github.com/headroomlabs-ai/headroom>)
//! ContentRouter + Kneedle + 异常保留 设计。
//!
//! v0.30 变更:
//! - `CompressOptions` 完全重定义（11 字段，含 5 策略 + 3 约束开关）
//! - 删除 v0.29 `anomaly_keys` 字段名兜底（由 SmartCrusher 按值分布自动检测）
//! - 删除 v0.29 `parse_json_simple` stub（改用 `flow::json_to_value`）

use std::sync::Arc;

/// v0.30: 压缩选项 (跨子压缩器共享)
#[derive(Debug, Clone)]
pub struct CompressOptions {
    /// 策略名:
    ///   "auto" (default) | "topn" | "timeseries" | "cluster"
    ///   | "lossless" | "smart_sample" | "head_tail"
    pub strategy: String,
    /// 顶层 builtin 显式传入的字节上限
    pub max_bytes: Option<usize>,
    /// 压缩到 N * ratio 项 (0.0-1.0)
    pub target_ratio: Option<f32>,
    /// 头尾边界比例 (0.0-1.0), 默认 0.15
    pub head_pct: f32,
    /// 尾边界比例 (0.0-1.0), 默认 0.15
    pub tail_pct: f32,
    /// 显式覆盖头数
    pub k_first: Option<usize>,
    /// 显式覆盖尾数
    pub k_last: Option<usize>,
    /// Lossless 短路阈值: 节省率 ≥ 此值才用 lossless (默认 0.15)
    pub lossless_min_savings_ratio: f32,
    /// 保留含错误关键词的项 (默认 true)
    pub preserve_errors: bool,
    /// 保留统计 outlier (>2σ) 项 (默认 true)
    pub preserve_outliers: bool,
    /// 保留 Id 字段 (注: 仅作标注, 不强制保留以避免破坏压缩率)
    pub preserve_ids: bool,
    /// 输出格式: "json" | "markdown_kv" | "csv_schema"
    pub output_format: String,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            strategy: "auto".into(),
            max_bytes: None,
            target_ratio: None,
            head_pct: 0.15,
            tail_pct: 0.15,
            k_first: None,
            k_last: None,
            lossless_min_savings_ratio: 0.15,
            preserve_errors: true,
            preserve_outliers: true,
            preserve_ids: true,
            output_format: "json".into(),
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
        Self {
            compressors: vec![],
        }
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
                if score >= 0.6 {
                    Some((score, c.clone()))
                } else {
                    None
                }
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, c)| c)
    }
}

// 子压缩器子模块 (Tasks 3-5 填充)
pub mod code; // Task 4 填充
pub mod html; // Task 4 填充
pub mod json; // Task 2 填充
pub mod log; // Task 4 填充
pub mod text; // Task 3 填充

// v0.30: re-export SmartCrusher 主入口
pub use json::{ArrayType, CrushResult, FieldRole, FieldStats, crush_json, crush_json_string};

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

/// v0.30: 从 `Value::Dict` 构建 `CompressOptions`。
///
/// 部分字段缺失或类型不匹配时**静默跳过**(不报错) — 调用方可任意选择传入哪些 options。
pub fn options_from_value(v: &crate::value::Value) -> Result<CompressOptions, String> {
    use crate::value::Value;
    let mut opts = CompressOptions::default();
    if let Value::Dict(map) = v {
        if let Some(Value::String(s)) = map.get("strategy") {
            opts.strategy = s.clone();
        }
        if let Some(Value::Number(n)) = map.get("max_bytes") {
            opts.max_bytes = Some(*n as usize);
        }
        if let Some(Value::Number(n)) = map.get("target_ratio") {
            opts.target_ratio = Some(*n as f32);
        }
        if let Some(Value::Number(n)) = map.get("head_pct") {
            opts.head_pct = *n as f32;
        }
        if let Some(Value::Number(n)) = map.get("tail_pct") {
            opts.tail_pct = *n as f32;
        }
        if let Some(Value::Number(n)) = map.get("k_first") {
            opts.k_first = Some(*n as usize);
        }
        if let Some(Value::Number(n)) = map.get("k_last") {
            opts.k_last = Some(*n as usize);
        }
        if let Some(Value::Number(n)) = map.get("lossless_min_savings_ratio") {
            opts.lossless_min_savings_ratio = *n as f32;
        }
        if let Some(Value::Bool(b)) = map.get("preserve_errors") {
            opts.preserve_errors = *b;
        }
        if let Some(Value::Bool(b)) = map.get("preserve_outliers") {
            opts.preserve_outliers = *b;
        }
        if let Some(Value::Bool(b)) = map.get("preserve_ids") {
            opts.preserve_ids = *b;
        }
        if let Some(Value::String(s)) = map.get("output_format") {
            opts.output_format = s.clone();
        }
        // 注: v0.29 的 anomaly_keys 字段不再解析 (无兼容)
    }
    Ok(opts)
}

/// v0.30: `compress(input, strategy, options)` 顶层 builtin 的核心实现。
///
/// Strategy 调度:
/// - `"json"`           → SmartCrusher `crush_json` (input 必须是 List 或 JSON 数组字符串)
/// - `"auto"`           → 路由器选最佳子压缩器 (json/code/html/log/text 5 个)
/// - `"head_tail"`      → TextSubCompressor (按 head_pct/tail_pct/max_bytes)
/// - `"summary"`        → TextSubCompressor (mock LLM, 真实 LLM 留 v0.30)
/// - `"lossless"`       → TextSubCompressor (原文本 + original_size marker)
/// - 其它               → 报错
pub fn compress_top(
    input: &crate::value::Value,
    strategy: &str,
    options: &CompressOptions,
) -> Result<crate::value::Value, String> {
    // "json" strategy 不走文本路径, 直接用原始 input 调 SmartCrusher
    if strategy == "json" {
        // 优先按 max_bytes 推 target; 否则按 target_ratio 推; 兜底 N/2
        let target = if let Some(mb) = options.max_bytes {
            (mb / 200).max(1)
        } else if let Some(ratio) = options.target_ratio {
            let n = match input {
                crate::value::Value::List(l) => l.len(),
                _ => 1,
            };
            ((n as f32 * ratio).max(1.0)) as usize
        } else {
            match input {
                crate::value::Value::List(l) => (l.len() as f32 * 0.2).max(1.0) as usize,
                _ => 1,
            }
        };
        let items = match input {
            crate::value::Value::List(l) => l.clone(),
            _ => {
                return Err(format!(
                    "compress.json: expected List, got {}",
                    value_type_simple(input)
                ));
            }
        };
        let result = crush_json(&items, target, options);
        let json = crate::flow::value_to_json(&crate::value::Value::List(result.items.clone()));
        return Ok(crate::value::Value::String(format!(
            "{}\n<compressed:method=smart_crusher strategy={} items={} total={} savings={:.2}>",
            json, result.strategy_used, result.items_kept, result.items_total, result.savings_ratio
        )));
    }

    // 其余 strategy 都需先提取文本
    let text = extract_text(input)?;
    let max_bytes = options.max_bytes.unwrap_or(8192);

    match strategy {
        "auto" => {
            let router = ContentRouter::default_router();
            let comp = router
                .sniff(&text)
                .ok_or_else(|| "compress.auto: no compressor matched for content".to_string())?;
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

/// v0.30: 极简 JSON 解析 — 委托 `flow::json_to_value` 真实实现 (v0.10 已存在)。
pub fn parse_json_simple(s: &str) -> Option<crate::value::Value> {
    crate::flow::json_to_value(s).ok()
}

/// v0.30: Value → JSON 字符串 (委托 `flow::value_to_json` 真实实现)
pub fn value_to_json_simple(v: &crate::value::Value) -> String {
    crate::flow::value_to_json(v)
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
        assert_eq!(opts.head_pct, 0.15);
        assert_eq!(opts.tail_pct, 0.15);
        assert_eq!(opts.strategy, "auto");
        assert!(opts.preserve_errors);
        assert!(opts.preserve_outliers);
    }

    /// v0.30: parse_json_simple 现在是真实实现 (委托 flow::json_to_value)
    #[test]
    fn parse_json_simple_now_real() {
        // 真实 JSON 解析
        let v = crate::compress::parse_json_simple("[1,2,3]").unwrap();
        assert!(matches!(v, crate::value::Value::List(_)));
        let v = crate::compress::parse_json_simple("{\"a\":1}").unwrap();
        assert!(matches!(v, crate::value::Value::Dict(_)));
        // 无效输入
        assert!(crate::compress::parse_json_simple("").is_none());
        assert!(crate::compress::parse_json_simple("not json").is_none());
    }
}
