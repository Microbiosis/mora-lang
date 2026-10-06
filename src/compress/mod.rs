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

use crate::error::MoraError;

/// SmartCrusher json 策略的目标大小除数：`target = max_bytes / 200`
/// （即目标 ≈ 原文的 1/200，经验压缩比兜底）。与 target_ratio 二选一。
const SMART_CRUSHER_TARGET_DIVISOR: usize = 200;

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
    /// v0.32: 递归 compact 整棵 Value 树 (Headroom DocumentCompactor 风格)
    pub recursive: bool,
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
            recursive: false,
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
    ///
    /// ⚠ v0.104.6 D227：这条契约此前**没有任何一处强制**，5 个子压缩器
    /// **全部**违反（网格普查 220 处）。现在由 [`finish_within_budget`]
    /// 统一收口：各子压缩器只管**选内容**，字节上限由收口函数保证。
    /// 新增子压缩器**必须**经它收尾，否则同样违约。
    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        options: &CompressOptions,
    ) -> Result<String, String>;

    /// 子压缩器身份: "json" | "code" | "html" | "log" | "text"
    fn origin(&self) -> &'static str;
}

/// 把字节索引向下对齐到最近的 UTF-8 字符边界。
///
/// v0.104.6 D227：从 `text::floor_char_boundary` 提上来共用 —— 收口函数
/// 要在任意字节位置截断，全部 5 个子压缩器都要用到。此前只有 text.rs 有
/// 一份私有副本，另外 4 个子压缩器要截就得各写一遍。
pub fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// v0.104.6 D227：**唯一**保证「输出不超过 max_bytes」的收口点。
///
/// `body` 是子压缩器选出的正文，`marker` 是尾部的元信息行
/// （`\n<compressed:method=…>` / `... bytes elided ...` 之类）。
///
/// 契约（`SubCompressor::compress` 文档明文）：
/// **返回的 UTF-8 字节数不超过 `max_bytes`**。
///
/// 修前的现实：5 个子压缩器**全部**违反，且 head_tail 还能把输入**放大**
/// （实测 6500 → 13052 字节，pct 之和 > 1 时 head/tail 段重叠，同一内容
/// 被输出两次）。放大是比超限更坏的一类 —— 「压缩」函数让数据变大。
///
/// 收口规则（按顺序）：
/// 1. `body + marker` 已 ≤ `max_bytes` → 原样返回。
/// 2. 否则把 `body` 截到 `max_bytes - marker.len()`（落到字符边界），
///    再拼 marker。marker 放不下时（`marker.len() >= max_bytes`），
///    只保留 marker 的前 `max_bytes` 字节。
/// 3. **正文绝不放大**：若 `body` 本身已比 `content` 长（子压缩器选内容
///    选错了），退回原文并**丢掉 marker**。
///
/// 关于第 3 条：判据最初写成「输出不得比原文长」，结果把 marker 也一起
/// 吞掉了 —— 实测 1241 字节日志配 1332 字节的 body+marker，整个结果退回
/// 原文，`ERROR lines preserved` marker 消失。marker 是**元信息**，
/// 不是内容；为了 91 字节的元信息丢掉全部「保留了哪些错误行」的信息，
/// 代价远大于收益。故第 3 条只约束正文，marker 在能装下时始终保留。
pub fn finish_within_budget(content: &str, body: String, marker: &str, max_bytes: usize) -> String {
    let candidate_len = body.len() + marker.len();

    // 规则 3（前置）：正文不得比原文长。marker 豁免（它是元信息，不是内容）。
    //
    // 必须在构造 `out` **之前**判：若 body 已比原文长，说明子压缩器选内容
    // 选错了，此时无论后面怎么截断，正文都不可能缩到原文以内。
    if body.len() > content.len() {
        return content.to_string();
    }

    // 规则 1：装得下就不截断。
    if candidate_len <= max_bytes {
        return body + marker;
    }
    if marker.len() >= max_bytes {
        // 规则 2a：marker 本身就超预算：只留前 max_bytes 字节（字符边界对齐）。
        return marker[..floor_char_boundary(marker, max_bytes)].to_string();
    }
    // 规则 2b：给 marker 留出预算，body 按剩余空间截断。
    let room = max_bytes - marker.len();
    // ⚠ `floor_char_boundary` 返回的是**字节索引**，不是切片。
    //   写成 `format!("{keep}{marker}")` 会把索引 47 当字符串输出，
    //   得到 `"47<M>"`（5 字节）而不是 47 字节正文 —— 输出既没被截断，
    //   也丢了内容。必须显式切片。
    let keep = &body[..floor_char_boundary(&body, room)];
    format!("{keep}{marker}")
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

    /// 嗅探: 找 sniff 分最高的子压缩器; 无 → `text` 兜底。
    ///
    /// v0.104.6 D228：此前门槛是 `score >= 0.6`，而 `TextSubCompressor::sniff`
    /// **固定返回 0.5**（注释明写「任何文本都至少 0.5」）。于是兜底者永远
    /// 过不了门槛，`sniff` 对任何非 json/code/html/log 的内容返回 `None`，
    /// `compress(x, "auto")` 报 `no compressor matched` 并 exit 1 ——
    /// **兜底压缩器实际不存在**。实测 6500 字节纯文本直接失败。
    ///
    /// 修法：把门槛降到 `> 0.0`（即「谁都不匹配时也要给一个压缩器」），
    /// 并保留 `max_by` 的选优逻辑 —— 命中者仍是分数最高的那个（json 0.9 >
    /// code/html/log 0.8 > text 0.5），匹配不上时才落到 text。
    pub fn sniff(&self, content: &str) -> Option<Arc<dyn SubCompressor>> {
        self.compressors
            .iter()
            .filter_map(|c| {
                let score = c.sniff(content);
                if score > 0.0 {
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
mod constraints; // v0.75.55: JSON 压缩分层拆分子模块
mod detect; // v0.75.55: JSON 压缩分层拆分子模块
pub mod html; // Task 4 填充
pub mod json; // Task 2 填充
pub mod log; // Task 4 填充
mod strategies; // v0.75.55: JSON 压缩分层拆分子模块
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
///
/// v0.75.99: 返回 `Result<String, MoraError>`。错误归类为 MoraError::Other
/// （extract 通用，不属 typeck/io/serialization）。
pub fn extract_text(input: &crate::value::Value) -> Result<String, MoraError> {
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
        other => Err(MoraError::Other(format!(
            "compress: expected Conversation / list of {{role, content}} / string, got {}",
            value_type_simple(other)
        ))),
    }
}

/// options 解析：**字段缺失**合法跳过；**字段存在但类型不对**报错。
///
/// v0.75.99: 返回 `Result<CompressOptions, MoraError>`。
///
/// ⚠ v0.104.6 D149：此前所有字段都是 `if let Some(Value::Float(n)) = …`，
/// 两种输入被**静默吞掉**（都是 exit 0、零诊断）：
///
/// | 输入 | 修复前 |
/// |---|---|
/// | `{max_bytes: "big"}`（字符串） | 静默忽略 → 用默认上限 → 原样返回 |
/// | `json.parse('{"max_bytes": 1}')`（**合法 Int**） | 静默忽略 → 同上 |
///
/// 第二条尤其隐蔽：dict **字面量**里的数字是 `Float`（D98），但
/// `json.parse` 产出的是 `Int`（D129 实测）—— 代码只匹配 `Float`，于是
/// **完全合法的数字被丢弃**，用户设的上限悄悄不生效。
///
/// 修法与 D39（`with temperature` 静默失效）、D148（`Int`/`Float` 两侧都要管）
/// 同一套：**Int 与 Float 都接受，其余类型报错**。
pub fn options_from_value(v: &crate::value::Value) -> Result<CompressOptions, MoraError> {
    use crate::value::Value;
    let mut opts = CompressOptions::default();
    // v0.104.6 D160：**形参本身**不是 dict 时，此前 `if let Value::Dict(map) = v`
    // 整块落空、直接跳到末尾的 `Ok(opts)` —— 即**静默返回全默认**：
    //
    //   compress("abcdefgh", "head_tail", "notadict")  → 原样返回，exit 0
    //   compress("abcdefgh", "head_tail", 5)            → 原样返回，exit 0
    //
    // 用户设的压缩参数**整份消失**且零诊断 —— 与 D155 的 `json.parse(5)`
    // （把数字静默字符串化后解析出 5.0）**同型**。
    //
    // D149 已把 dict **内部字段**的取值收口（字段类型错要报错），但**外层**
    // 形参类型一直没人查 —— 同一个函数里 `strategy` 形参是查了的
    // （「compress: strategy must be a string」），10 行之外的 options 没查。
    //
    // `Nil` 仍按「没传」处理（与 D150 起统一的 `optional_*_arg` 约定一致：
    // `nil` 是 Mora 的 null、可选实参传 `nil` 是惯用法）。
    if !matches!(v, Value::Dict(_) | Value::Nil) {
        return Err(MoraError::Other(format!(
            "compress: options 期望 dict，得到 {}",
            value_type_simple(v)
        )));
    }
    if let Value::Dict(map) = v {
        // 数字字段取值：`Int` / `Float` 都接受（dict 字面量给 Float，
        // `json.parse` 给 Int），其余类型报错。
        fn num(
            map: &std::collections::HashMap<String, Value>,
            key: &str,
        ) -> Result<Option<f64>, MoraError> {
            match map.get(key) {
                None => Ok(None),
                Some(Value::Int(i)) => Ok(Some(*i as f64)),
                Some(Value::Float(n)) => Ok(Some(*n)),
                Some(other) => Err(MoraError::Other(format!(
                    "compress: `{key}` 期望数字，得到 {}",
                    value_type_simple(other)
                ))),
            }
        }
        if let Some(Value::String(s)) = map.get("strategy") {
            opts.strategy = s.clone();
        } else if let Some(other) = map.get("strategy") {
            return Err(MoraError::Other(format!(
                "compress: `strategy` 期望字符串，得到 {}",
                value_type_simple(other)
            )));
        }
        if let Some(n) = num(map, "max_bytes")? {
            // v0.104.6 D145：负数上限 —— Rust 的 float→int `as` 是**饱和转换**，
            // `-5.0 as usize == 0`，于是负数上限被静默当成 0，8 字节输入里
            // 删掉 6 字节却 exit 0、零诊断。
            if n < 0.0 {
                return Err(MoraError::Other(format!(
                    "compress: max_bytes 不能为负数（得到 {n}）"
                )));
            }
            opts.max_bytes = Some(n as usize);
        }
        if let Some(n) = num(map, "target_ratio")? {
            opts.target_ratio = Some(n as f32);
        }
        if let Some(n) = num(map, "head_pct")? {
            opts.head_pct = n as f32;
        }
        if let Some(n) = num(map, "tail_pct")? {
            opts.tail_pct = n as f32;
        }
        if let Some(n) = num(map, "k_first")? {
            // v0.104.6 D146：与 `max_bytes` **同函数同型**的饱和转换
            //（`-5.0 as usize == 0`）。实测 `compress(..., {k_first: -5, k_last: -5})`
            // **完全不压缩**（原样返回全部元素）却 exit 0 —— 「压缩」静默失效。
            if n < 0.0 {
                return Err(MoraError::Other(format!(
                    "compress: k_first 不能为负数（得到 {n}）"
                )));
            }
            opts.k_first = Some(n as usize);
        }
        if let Some(n) = num(map, "k_last")? {
            if n < 0.0 {
                return Err(MoraError::Other(format!(
                    "compress: k_last 不能为负数（得到 {n}）"
                )));
            }
            opts.k_last = Some(n as usize);
        }
        if let Some(n) = num(map, "lossless_min_savings_ratio")? {
            opts.lossless_min_savings_ratio = n as f32;
        }
        // 布尔字段：缺失合法跳过，类型错报错（与数字字段同一原则）。
        fn flag(
            map: &std::collections::HashMap<String, Value>,
            key: &str,
        ) -> Result<Option<bool>, MoraError> {
            match map.get(key) {
                None => Ok(None),
                Some(Value::Bool(b)) => Ok(Some(*b)),
                Some(other) => Err(MoraError::Other(format!(
                    "compress: `{key}` 期望布尔值，得到 {}",
                    value_type_simple(other)
                ))),
            }
        }
        for (key, slot) in [
            ("preserve_errors", &mut opts.preserve_errors),
            ("preserve_outliers", &mut opts.preserve_outliers),
            ("preserve_ids", &mut opts.preserve_ids),
            ("recursive", &mut opts.recursive),
        ] {
            if let Some(b) = flag(map, key)? {
                *slot = b;
            }
        }
        if let Some(Value::String(s)) = map.get("output_format") {
            opts.output_format = s.clone();
        } else if let Some(other) = map.get("output_format") {
            return Err(MoraError::Other(format!(
                "compress: `output_format` 期望字符串，得到 {}",
                value_type_simple(other)
            )));
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
///
/// v0.75.99: 返回 `Result<Value, MoraError>`（MoraError 统一计划推进）。
pub fn compress_top(
    input: &crate::value::Value,
    strategy: &str,
    options: &CompressOptions,
) -> Result<crate::value::Value, MoraError> {
    // "json" strategy 不走文本路径, 直接用原始 input 调 SmartCrusher
    if strategy == "json" {
        // 优先按 max_bytes 推 target; 否则按 target_ratio 推; 兜底 N/2
        let target = if let Some(mb) = options.max_bytes {
            (mb / SMART_CRUSHER_TARGET_DIVISOR).max(1)
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
                return Err(MoraError::Other(format!(
                    "compress.json: expected List, got {}",
                    value_type_simple(input)
                )));
            }
        };
        let result = crush_json(&items.to_vec(), target, options);
        let json =
            crate::flow::value_to_json(&crate::value::Value::List(result.items.clone().into()));
        let marker = format!(
            "\n<compressed:method=smart_crusher strategy={} items={} total={} savings={:.2}>",
            result.strategy_used, result.items_kept, result.items_total, result.savings_ratio
        );
        // v0.104.6 D227：这条路径**不经过** `SubCompressor::compress`，
        // 所以收口不会自动生效 —— 必须显式调，否则 builtin 入口仍超限。
        // `max_bytes` 缺省时用 8192，与文本路径的缺省一致。
        let budget = options.max_bytes.unwrap_or(8192);
        let body = crate::flow::value_to_json(&crate::value::Value::List(items));
        return Ok(crate::value::Value::String(finish_within_budget(
            &body, json, &marker, budget,
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
        other => Err(MoraError::Other(format!(
            "compress: unknown strategy '{}'",
            other
        ))),
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
///
/// v0.104.6 D249：补齐 `Int` / `BigInt` / `Char` 三个分支。此前它们全部落进
/// `_ => "other"`，于是**用户可见的错误信息失真**：
///
/// ```text
/// some_builtin(42)      → "must be a string, got other"   ← 「other」毫无信息量
/// compress(…, {max_bytes: 1}) → "期望数字，得到 other"
/// ```
///
/// 数字字面量是 `Float`（D98），所以这些错误**极少被触发** —— 正因如此
/// 「报 other」长期没被发现：只有当整数来自 `len()` 这类**返回 Int 的表达式**
/// 时才会撞上。
///
/// ⚠ 彻底修法是把本函数**转发**到 `flow::type_name`（那份覆盖完整，含
/// relation / goal / task 等声明式值）。但那会改变**所有**既有错误信息的
/// 措辞（`Char` 从 "other" 变 "char"、`Value::Closure` 变 "closure" …），
/// 影响面超出缺陷本身，需单独评估后再做。此处先补齐三种常见值。
pub fn value_type_simple(v: &crate::value::Value) -> &'static str {
    use crate::value::Value;
    match v {
        Value::String(_) => "string",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::BigInt(_) => "bigint",
        Value::Char(_) => "char",
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

    // ── v0.104.6 D227：`finish_within_budget` 的直接单测 ──
    //
    // 为什么必须直接测：回退验证时发现，把「规则 2b 截断」和「规则 3
    // 正文不放大」两条**单独**回退，集成判据全部**照常变绿** ——
    // 因为 5 个子压缩器现在各自算对了预算，收口成了冗余保险。
    // 也就是说集成判据对收口函数**没有判别力**。
    //
    // 这不是「收口没用」，而是「集成判据测不到它」：收口的价值恰恰在于
    // **下一个**新增的子压缩器算错时兜底。直接测它才能证明这一点。

    /// 规则 2b：body 超预算时按剩余空间截断，且 marker 完整保留。
    #[test]
    fn finish_within_budget_truncates_body_to_fit() {
        let content = "0123456789".repeat(20); // 200 bytes
        let body = "abcdefghij".repeat(20); // 200 bytes
        let marker = "<M>"; // 3 bytes
        let out = finish_within_budget(&content, body, marker, 50);
        assert_eq!(out.len(), 50, "输出必须恰好等于预算: {out:?}");
        assert!(
            out.ends_with(marker),
            "marker 必须完整保留（不能被截掉）: {out:?}"
        );
        assert_eq!(
            out.len() - marker.len(),
            47,
            "body 应被截到恰好填满剩余预算"
        );
    }

    /// 规则 2a：marker 本身就超预算时，输出被截到 max_bytes（且落在字符边界）。
    #[test]
    fn finish_within_budget_marker_alone_exceeds_budget() {
        let content = "0123456789".repeat(20);
        let body = "x".repeat(100);
        let marker = "<a very long marker that is definitely longer than max_bytes>";
        let out = finish_within_budget(&content, body, marker, 10);
        assert!(
            out.len() <= 10,
            "marker 超预算时输出仍须 ≤ max_bytes; 实得 {} 字节: {out:?}",
            out.len()
        );
        assert!(out.starts_with("<a very"), "应保留 marker 的开头: {out:?}");
    }

    /// 规则 3：body 比原文长时退回原文（marker 豁免）。
    #[test]
    fn finish_within_budget_rejects_oversized_body() {
        let content = "short";
        let body = "x".repeat(1000); // 远超原文
        let marker = "<M>";
        let out = finish_within_budget(content, body, marker, 8192);
        assert_eq!(out, content, "body 比原文长时必须退回原文（不放大输入）");
    }

    /// 规则 1：装得下就原样返回，不做任何截断。
    #[test]
    fn finish_within_budget_passes_through_when_it_fits() {
        let content = "0123456789".repeat(10);
        let body = "abc";
        let marker = "<M>";
        let out = finish_within_budget(&content, body.to_string(), marker, 8192);
        assert_eq!(out, "abc<M>", "装得下时不应有任何改动");
    }

    /// 多字节 UTF-8：截断点必须落在字符边界上（否则 panic 或产生乱码）。
    #[test]
    fn finish_within_budget_respects_utf8_boundaries() {
        let content = "中".repeat(50); // 150 bytes
        let body = "中".repeat(50);
        let marker = "<M>";
        for mb in [0usize, 1, 2, 3, 4, 5, 7, 11, 13, 50, 99, 151, 152, 153] {
            let out = finish_within_budget(&content, body.clone(), marker, mb);
            assert!(
                out.len() <= mb,
                "max_bytes={mb}: 输出 {} 字节超限",
                out.len()
            );
            // 不是 panic 就说明切在了合法边界上（Rust 的 str 切片会拒绝非法边界）
            assert!(
                out.chars()
                    .all(|c| c == '中' || c == '<' || c == 'M' || c == '>'),
                "max_bytes={mb}: 截断产生了非法字符: {out:?}"
            );
        }
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
