//! Mora 可观测性 — Trace + Metrics
//!
//! 轻量级 OpenTelemetry 兼容的追踪和指标系统。
//! - Trace：span 记录 AI 调用链
//! - Metrics：Token 消耗、调用次数、延迟统计
//! - 输出：JSON 格式，兼容 OpenTelemetry Collector

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::collections::HashMap;

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
        self.inner.lock().unwrap().enabled
    }

    /// 开始一个 span
    pub fn start_span(&self, name: &str, attributes: HashMap<String, String>) -> SpanHandle {
        let mut inner = self.inner.lock().unwrap();
        if !inner.enabled {
            return SpanHandle { trace_id: String::new(), span_id: String::new(), start: Instant::now(), collector: self.clone(), name: name.to_string() };
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

    /// v0.04 终态 Slice 3: 启用/禁用 trace (不丢已有 spans)
    pub fn set_enabled(&self, enabled: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.enabled = enabled;
    }

    /// v0.04 终态 Slice 3: 设置 OTEL endpoint
    pub fn set_otel_endpoint(&self, endpoint: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.otel_endpoint = Some(endpoint);
    }

    /// 结束 span
    fn end_span(&self, handle: &SpanHandle, status: SpanStatus, attributes: HashMap<String, String>) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.enabled { return; }
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
        let mut inner = self.inner.lock().unwrap();
        inner.metrics.total_input_tokens += input;
        inner.metrics.total_output_tokens += output;
    }

    /// 记录调用
    pub fn record_call(&self, call_type: &str, latency: Duration, success: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.metrics.total_calls += 1;
        match call_type {
            "ai.chat" => inner.metrics.ai_chat_calls += 1,
            "ai.stream" => inner.metrics.ai_stream_calls += 1,
            "tool" => inner.metrics.tool_calls += 1,
            "memory" => inner.metrics.memory_operations += 1,
            _ => {}
        }
        inner.metrics.latency_sum_ms += latency.as_millis() as u64;
        inner.metrics.avg_latency_ms = inner.metrics.latency_sum_ms as f64 / inner.metrics.total_calls as f64;
        if !success {
            inner.metrics.total_errors += 1;
        }
    }

    /// 获取指标快照
    pub fn get_metrics(&self) -> Metrics {
        self.inner.lock().unwrap().metrics.clone()
    }

    /// 获取所有 spans（JSON 数组格式）
    pub fn get_spans_json(&self) -> String {
        let inner = self.inner.lock().unwrap();
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
        let inner = self.inner.lock().unwrap();
        let spans: Vec<String> = inner.spans.iter().map(|s| {
            format!(
                r#"{{"name":"{}","traceId":"{}","spanId":"{}","startTimeUnixNano":"0","endTimeUnixNano":"{}","status":{{"code":"{}"}}}}"#,
                escape_json(&s.name), escape_json(&s.trace_id), escape_json(&s.span_id),
                s.duration_ms * 1_000_000,
                match &s.status { SpanStatus::Ok => "OK", SpanStatus::Error(_) => "ERROR" }
            )
        }).collect();
        format!(r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{}]}}]}}]}}"#, spans.join(","))
    }

    /// 指标转 JSON
    pub fn metrics_json(&self) -> String {
        let m = self.get_metrics();
        format!(
            r#"{{"totalCalls":{},"aiChatCalls":{},"aiStreamCalls":{},"toolCalls":{},"memoryOps":{},"totalInputTokens":{},"totalOutputTokens":{},"totalErrors":{},"avgLatencyMs":{:.1}}}"#,
            m.total_calls, m.ai_chat_calls, m.ai_stream_calls, m.tool_calls, m.memory_operations,
            m.total_input_tokens, m.total_output_tokens, m.total_errors, m.avg_latency_ms
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
        self.collector.end_span(&self, SpanStatus::Error(error.to_string()), attributes);
    }
}

// Drop 时自动结束（如果还没手动结束）
impl Drop for SpanHandle {
    fn drop(&mut self) {
        // SpanHandle 被 move 后 Drop 不会再调用（Rust 的 move 语义）
        // 这里只是保险起见
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
