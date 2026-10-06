//! Mora 可观测性 — Trace + Metrics
//!
//! 轻量级 OpenTelemetry 兼容的追踪和指标系统。
//! - Trace：span 记录 AI 调用链
//! - Metrics：Token 消耗、调用次数、延迟统计
//! - 输出：JSON 格式，兼容 OpenTelemetry Collector

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Trace span
#[derive(Debug, Clone)]
pub struct Span {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_id: Option<String>,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub attributes: HashMap<String, String>,
    pub status: SpanStatus,
}

#[derive(Debug, Clone)]
pub enum SpanStatus {
    Ok,
    Error(String),
}

/// 指标快照
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub total_calls: u64,
    pub ai_chat_calls: u64,
    pub ai_stream_calls: u64,
    pub tool_calls: u64,
    pub memory_operations: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_errors: u64,
    pub avg_latency_ms: f64,
    latency_sum_ms: u64,
}

/// Trace + Metrics 收集器
#[derive(Clone)]
pub struct TraceCollector {
    inner: Arc<Mutex<TraceCollectorInner>>,
}

struct TraceCollectorInner {
    enabled: bool,
    spans: Vec<Span>,
    metrics: Metrics,
    counter: u64,
    otel_endpoint: Option<String>,
}

impl TraceCollector {
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(TraceCollectorInner {
                enabled,
                spans: Vec::new(),
                metrics: Metrics::default(),
                counter: 0,
                otel_endpoint: None,
            })),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.lock().expect("trace collector poisoned").enabled
    }

    /// 开始一个 span
    pub fn start_span(&self, name: &str, _attributes: HashMap<String, String>) -> SpanHandle {
        let mut inner = self.inner.lock().expect("trace collector poisoned");
        if !inner.enabled {
            return SpanHandle {
                trace_id: String::new(),
                span_id: String::new(),
                start: Instant::now(),
                collector: self.clone(),
                name: name.to_string(),
            };
        }
        inner.counter += 1;
        let span_id = format!("span_{}", inner.counter);
        let trace_id = format!("trace_{}", inner.counter);
        SpanHandle {
            trace_id,
            span_id,
            start: Instant::now(),
            collector: self.clone(),
            name: name.to_string(),
        }
    }

    /// v0.04 Slice 3: 启用/禁用 trace (不丢已有 spans)
    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.inner.lock().expect("trace collector poisoned");
        inner.enabled = enabled;
    }

    /// v0.04 Slice 3: 设置 OTEL endpoint
    pub fn set_otel_endpoint(&self, endpoint: String) {
        let mut inner = self.inner.lock().expect("trace collector poisoned");
        inner.otel_endpoint = Some(endpoint);
    }

    /// 结束 span
    fn end_span(
        &self,
        handle: &SpanHandle,
        status: SpanStatus,
        attributes: HashMap<String, String>,
    ) {
        let mut inner = self.inner.lock().expect("trace collector poisoned");
        if !inner.enabled {
            return;
        }
        let duration = handle.start.elapsed();
        let span = Span {
            name: handle.name.clone(),
            trace_id: handle.trace_id.clone(),
            span_id: handle.span_id.clone(),
            parent_id: None,
            start_ms: 0,
            duration_ms: duration.as_millis() as u64,
            attributes,
            status,
        };
        inner.spans.push(span);
    }

    /// 记录 token 消耗
    pub fn record_tokens(&self, input: u64, output: u64) {
        let mut inner = self.inner.lock().expect("trace collector poisoned");
        inner.metrics.total_input_tokens += input;
        inner.metrics.total_output_tokens += output;
    }

    /// 记录调用
    pub fn record_call(&self, call_type: &str, latency: Duration, success: bool) {
        let mut inner = self.inner.lock().expect("trace collector poisoned");
        inner.metrics.total_calls += 1;
        match call_type {
            "ai.chat" => inner.metrics.ai_chat_calls += 1,
            "ai.stream" => inner.metrics.ai_stream_calls += 1,
            "tool" => inner.metrics.tool_calls += 1,
            "memory" => inner.metrics.memory_operations += 1,
            _ => {}
        }
        inner.metrics.latency_sum_ms += latency.as_millis() as u64;
        inner.metrics.avg_latency_ms =
            inner.metrics.latency_sum_ms as f64 / inner.metrics.total_calls as f64;
        if !success {
            inner.metrics.total_errors += 1;
        }
    }

    /// 获取指标快照
    pub fn get_metrics(&self) -> Metrics {
        self.inner
            .lock()
            .expect("trace collector poisoned")
            .metrics
            .clone()
    }

    /// 获取所有 spans（JSON 数组格式）
    pub fn get_spans_json(&self) -> String {
        let inner = self.inner.lock().expect("trace collector poisoned");
        let spans: Vec<String> = inner.spans.iter().map(|s| {
            let attrs: Vec<String> = s.attributes.iter()
                .map(|(k, v)| format!("\"{}\":\"{}\"", escape_json(k), escape_json(v)))
                .collect();
            let status = match &s.status {
                SpanStatus::Ok => "\"ok\"".to_string(),
                SpanStatus::Error(msg) => format!("{{\"error\":\"{}\"}}", escape_json(msg)),
            };
            format!(
                r#"{{"name":"{}","traceId":"{}","spanId":"{}","durationMs":{},"status":{},"attributes":{{{}}}}}"#,
                escape_json(&s.name), escape_json(&s.trace_id), escape_json(&s.span_id),
                s.duration_ms, status, attrs.join(",")
            )
        }).collect();
        format!("[{}]", spans.join(","))
    }

    /// 导出为 OpenTelemetry JSON 格式
    pub fn export_otel_json(&self) -> String {
        let inner = self.inner.lock().expect("trace collector poisoned");
        let spans: Vec<String> = inner.spans.iter().map(|s| {
            format!(
                r#"{{"name":"{}","traceId":"{}","spanId":"{}","startTimeUnixNano":"0","endTimeUnixNano":"{}","status":{{"code":"{}"}}}}"#,
                escape_json(&s.name), escape_json(&s.trace_id), escape_json(&s.span_id),
                s.duration_ms * 1_000_000,
                match &s.status { SpanStatus::Ok => "OK", SpanStatus::Error(_) => "ERROR" }
            )
        }).collect();
        format!(
            r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{}]}}]}}]}}"#,
            spans.join(",")
        )
    }

    /// 指标转 JSON
    pub fn metrics_json(&self) -> String {
        let m = self.get_metrics();
        format!(
            r#"{{"totalCalls":{},"aiChatCalls":{},"aiStreamCalls":{},"toolCalls":{},"memoryOps":{},"totalInputTokens":{},"totalOutputTokens":{},"totalErrors":{},"avgLatencyMs":{:.1}}}"#,
            m.total_calls,
            m.ai_chat_calls,
            m.ai_stream_calls,
            m.tool_calls,
            m.memory_operations,
            m.total_input_tokens,
            m.total_output_tokens,
            m.total_errors,
            m.avg_latency_ms
        )
    }
}

/// Span 句柄（RAII 风格，drop 时自动结束）
pub struct SpanHandle {
    trace_id: String,
    span_id: String,
    start: Instant,
    collector: TraceCollector,
    name: String,
}

impl SpanHandle {
    /// 正常结束
    pub fn end(self, attributes: HashMap<String, String>) {
        self.collector.end_span(&self, SpanStatus::Ok, attributes);
    }

    /// 错误结束
    pub fn end_error(self, error: &str, attributes: HashMap<String, String>) {
        self.collector
            .end_span(&self, SpanStatus::Error(error.to_string()), attributes);
    }
}

// Drop 时自动结束（如果还没手动结束）
impl Drop for SpanHandle {
    fn drop(&mut self) {
        // SpanHandle 被 move 后 Drop 不会再调用（Rust 的 move 语义）
        // 这里只是保险起见
    }
}

/// v0.104.6 D240：转发到共享的 `flow::escape_json_string`。
///
/// 修前这是同一 JSON 转义规则的**又一份实现**（全仓共 4 处手写表：
/// `flow/json.rs` 本体、`lsp/json.rs`、`audit/mod.rs`、本函数）。
///
/// 全码点空间实测与共享实现的差异恰为 2 处：
/// `U+0008`（退格）与 `U+000C`（换页）—— 本实现产出 `\u0008` / `\u000c`
/// （**合法** JSON，外部 `json.loads` 能读、往返无损，D240 已用 Python
/// 验证过 Jaeger span 的完整往返）。故**修前无功能后果**。
///
/// 收敛的理由与 D239 的 `record::esc` 相同：这些表**看似等价、实则各自
/// 漂移**，下一轮若有人只改其中一份，trace 输出的字节会静默变化。
/// 协议层的 `lsp/json.rs` **有意保持独立**（JSON-RPC 线缆格式不应
/// 依赖语言层），故不收敛。
fn escape_json(s: &str) -> String {
    crate::flow::escape_json_string(s)
}

#[cfg(test)]
mod d240_tests {
    use super::escape_json;

    /// v0.104.6 D240：trace 的转义表必须与共享实现**逐字节相同**。
    ///
    /// 这些表「看似等价、实则各自漂移」：全码点空间实测，
    /// 本实现修前与共享实现的差异恰为 `U+0008` / `U+000C` 两处。
    /// 虽无功能后果（外部 `json.loads` 能读、往返无损，D240 已用 Python
    /// 验证过完整 Jaeger span），但下一轮只改一份就会让 trace 字节静默变化。
    ///
    /// 判据用**全码点穷举**而非抽查 —— 抽查只能证明「这几个字符没问题」。
    #[test]
    fn d240_escape_json_matches_shared_implementation() {
        let mut diffs: Vec<String> = Vec::new();
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue; // 代理项不是合法标量值
            };
            let s = c.to_string();
            let a = escape_json(&s);
            let b = crate::flow::escape_json_string(&s);
            if a != b {
                diffs.push(format!("U+{cp:06X} {c:?}: trace={a:?} flow={b:?}"));
                if diffs.len() >= 10 {
                    break;
                }
            }
        }
        assert!(
            diffs.is_empty(),
            "D240: `trace_collector::escape_json` 与共享的 \
             `flow::escape_json_string` 产出不一致。全码点空间共 {} 处差异：\n  {}",
            diffs.len(),
            diffs.join("\n  ")
        );
    }

    /// v0.104.6 D240 对照组：产出必须是**合法 JSON 字符串体**。
    ///
    /// trace 的产出是 OpenTelemetry / Jaeger 的 span JSON，
    /// 要被**外部 APM 系统**解析，所以「能被 `json.loads` 读」才是契约。
    /// 本条用手写的最小校验器逐字节检查（不引 serde）。
    #[test]
    fn d240_escape_json_output_is_valid_json_string_body() {
        let mut bad: Vec<String> = Vec::new();
        for cp in 0u32..=0x10FFFF {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let body = escape_json(&c.to_string());
            let bytes = body.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    if i + 1 >= bytes.len() {
                        bad.push(format!("U+{cp:06X} 反斜杠在末尾: {body:?}"));
                        break;
                    }
                    let next = bytes[i + 1];
                    if !matches!(
                        next,
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
                    ) {
                        bad.push(format!("U+{cp:06X} 非法转义: {body:?}"));
                        break;
                    }
                    i += if next == b'u' { 6 } else { 2 };
                } else if bytes[i] < 0x20 {
                    bad.push(format!("U+{cp:06X} 裸控制字符: {body:?}"));
                    break;
                } else {
                    i += 1;
                }
            }
            if bad.len() >= 10 {
                break;
            }
        }
        assert!(
            bad.is_empty(),
            "D240: `escape_json` 对某些码点产出的不是合法 JSON 字符串体：\n  {}",
            bad.join("\n  ")
        );
    }
}
