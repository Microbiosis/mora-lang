//! P4-4: Compute & Service Nanosecond Trace Profiler (`TraceProfiler`).
//!
//! Records nanosecond monotonic spans across `ComputeDag.run_profiled()`, GPU custom kernel
//! launches, KV-cache page allocations, distributed `ProcessGroup` collectives, and HTTP/MCP
//! service handlers (via pure-Mora `std/trace`), exporting standard Chrome Trace Event JSON
//! (`chrome://tracing` / Perfetto) and structured summary reports.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::common::sync::Mutex;
use crate::value::Value;

/// One completed nanosecond trace span in Chrome Trace Event format (`ph = "X"`).
#[derive(Debug, Clone)]
pub(crate) struct TraceEvent {
    pub(crate) cat: String,
    pub(crate) name: String,
    pub(crate) start_ns: u64,
    pub(crate) dur_ns: u64,
    pub(crate) tid: u64,
    pub(crate) args: Vec<(String, String)>,
}

struct ProfilerInner {
    epoch: Instant,
    events: Vec<TraceEvent>,
}

struct GlobalProfiler {
    active: AtomicBool,
    inner: Mutex<ProfilerInner>,
}

static GLOBAL_PROFILER: OnceLock<Arc<GlobalProfiler>> = OnceLock::new();

fn global_profiler() -> &'static Arc<GlobalProfiler> {
    GLOBAL_PROFILER.get_or_init(|| {
        Arc::new(GlobalProfiler {
            active: AtomicBool::new(false),
            inner: Mutex::new(ProfilerInner {
                epoch: Instant::now(),
                events: Vec::new(),
            }),
        })
    })
}

/// First-class handle held by `Value::TraceProfiler(Arc<TraceProfilerHandle>)`.
#[derive(Debug)]
pub struct TraceProfilerHandle {
    state: Arc<GlobalProfiler>,
}

impl std::fmt::Debug for GlobalProfiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalProfiler")
            .field("active", &self.active.load(Ordering::Relaxed))
            .finish()
    }
}

/// Obtain a handle to the runtime's unified [`TraceProfilerHandle`].
pub(crate) fn global_handle() -> Arc<TraceProfilerHandle> {
    Arc::new(TraceProfilerHandle {
        state: Arc::clone(global_profiler()),
    })
}

/// Fast lock-free check for whether profiling is currently active.
#[inline]
pub(crate) fn is_active() -> bool {
    GLOBAL_PROFILER
        .get()
        .is_some_and(|p| p.active.load(Ordering::Relaxed))
}

/// Current thread identifier hashed into a compact integer `tid` for Chrome Trace JSON.
pub(crate) fn current_tid() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut h);
    h.finish() & 0xFFFF_FFFF
}

/// Return monotonic nanoseconds since profiler epoch.
pub(crate) fn now_ns() -> u64 {
    let p = global_profiler();
    let g = p.inner.lock();
    g.epoch.elapsed().as_nanos() as u64
}

/// Record a span into the global profiler if profiling is active (or unconditionally if `force`).
pub(crate) fn record_span(
    cat: &str,
    name: &str,
    start_ns: u64,
    dur_ns: u64,
    args: Vec<(String, String)>,
    force: bool,
) {
    if !force && !is_active() {
        return;
    }
    let p = global_profiler();
    let mut g = p.inner.lock();
    g.events.push(TraceEvent {
        cat: cat.to_string(),
        name: name.to_string(),
        start_ns,
        dur_ns,
        tid: current_tid(),
        args,
    });
}

/// RAII scope guard that records a nanosecond span into `TraceProfiler` on drop if active.
pub(crate) struct TraceScope {
    active: bool,
    cat: &'static str,
    name: String,
    start_ns: u64,
    start_inst: Instant,
    args: Vec<(String, String)>,
}

impl TraceScope {
    #[inline]
    pub(crate) fn enter(
        cat: &'static str,
        name: impl Into<String>,
        args: Vec<(String, String)>,
    ) -> Self {
        let active = is_active();
        let start_inst = Instant::now();
        let start_ns = if active { now_ns() } else { 0 };
        Self {
            active,
            cat,
            name: name.into(),
            start_ns,
            start_inst,
            args,
        }
    }
}

impl Drop for TraceScope {
    fn drop(&mut self) {
        if self.active {
            let dur_ns = self.start_inst.elapsed().as_nanos() as u64;
            record_span(
                self.cat,
                &self.name,
                self.start_ns,
                dur_ns,
                std::mem::take(&mut self.args),
                true,
            );
        }
    }
}

fn json_escape_into(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Render a slice of [`TraceEvent`]s into standard Chrome Trace Event JSON (`chrome://tracing` / Perfetto).
pub(crate) fn format_chrome_trace_json(events: &[TraceEvent]) -> String {
    let mut out = String::with_capacity(64 + events.len() * 128);
    out.push_str("{\"traceEvents\":[");
    for (idx, ev) in events.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        json_escape_into(&mut out, &ev.name);
        out.push_str(",\"cat\":");
        json_escape_into(&mut out, &ev.cat);
        let ts_us = ev.start_ns as f64 / 1000.0;
        let dur_us = ev.dur_ns as f64 / 1000.0;
        let _ = write!(
            out,
            ",\"ph\":\"X\",\"ts\":{ts_us:.3},\"dur\":{dur_us:.3},\"pid\":1,\"tid\":{}",
            ev.tid
        );
        if !ev.args.is_empty() {
            out.push_str(",\"args\":{");
            for (ai, (k, v)) in ev.args.iter().enumerate() {
                if ai > 0 {
                    out.push(',');
                }
                json_escape_into(&mut out, k);
                out.push(':');
                json_escape_into(&mut out, v);
            }
            out.push('}');
        }
        out.push('}');
    }
    out.push_str("],\"displayTimeUnit\":\"ms\"}");
    out
}

/// Convert a slice of [`TraceEvent`]s into Mora `list<dict<string, any>>`.
pub(crate) fn events_to_mora_value(events: &[TraceEvent]) -> Value {
    let list: Vec<Value> = events
        .iter()
        .map(|ev| {
            let mut m = HashMap::new();
            m.insert("cat".to_string(), Value::String(ev.cat.clone()));
            m.insert("name".to_string(), Value::String(ev.name.clone()));
            m.insert("start_ns".to_string(), Value::Int(ev.start_ns as i64));
            m.insert("dur_ns".to_string(), Value::Int(ev.dur_ns as i64));
            m.insert(
                "dur_us".to_string(),
                Value::Float(ev.dur_ns as f64 / 1000.0),
            );
            m.insert("tid".to_string(), Value::Int(ev.tid as i64));
            let mut am = HashMap::new();
            for (k, v) in &ev.args {
                am.insert(k.clone(), Value::String(v.clone()));
            }
            m.insert("args".to_string(), Value::Dict(am));
            Value::Dict(m)
        })
        .collect();
    Value::List(list.into())
}

/// Summarize trace events into a structured Mora `dict<string, any>`.
pub(crate) fn summary_to_mora_value(events: &[TraceEvent]) -> Value {
    let mut total_dur_ns: u64 = 0;
    let mut cat_counts: HashMap<String, i64> = HashMap::new();
    let mut cat_dur_us: HashMap<String, f64> = HashMap::new();
    let mut slowest_name = String::new();
    let mut slowest_ns: u64 = 0;

    for ev in events {
        total_dur_ns = total_dur_ns.saturating_add(ev.dur_ns);
        *cat_counts.entry(ev.cat.clone()).or_insert(0) += 1;
        *cat_dur_us.entry(ev.cat.clone()).or_insert(0.0) += ev.dur_ns as f64 / 1000.0;
        if ev.dur_ns >= slowest_ns {
            slowest_ns = ev.dur_ns;
            slowest_name = format!("{}:{}", ev.cat, ev.name);
        }
    }

    let mut m = HashMap::new();
    m.insert("event_count".to_string(), Value::Int(events.len() as i64));
    m.insert("active".to_string(), Value::Bool(is_active()));
    m.insert("total_dur_ns".to_string(), Value::Int(total_dur_ns as i64));
    m.insert(
        "total_dur_us".to_string(),
        Value::Float(total_dur_ns as f64 / 1000.0),
    );
    m.insert("slowest_span".to_string(), Value::String(slowest_name));
    m.insert(
        "slowest_dur_us".to_string(),
        Value::Float(slowest_ns as f64 / 1000.0),
    );
    let counts_map: HashMap<String, Value> = cat_counts
        .into_iter()
        .map(|(k, v)| (k, Value::Int(v)))
        .collect();
    let dur_map: HashMap<String, Value> = cat_dur_us
        .into_iter()
        .map(|(k, v)| (k, Value::Float(v)))
        .collect();
    m.insert("categories".to_string(), Value::Dict(counts_map));
    m.insert("category_dur_us".to_string(), Value::Dict(dur_map));
    Value::Dict(m)
}

impl TraceProfilerHandle {
    pub(crate) fn start(&self) {
        let mut g = self.state.inner.lock();
        g.epoch = Instant::now();
        g.events.clear();
        self.state.active.store(true, Ordering::Release);
    }

    pub(crate) fn stop(&self) {
        self.state.active.store(false, Ordering::Release);
    }

    pub(crate) fn clear(&self) {
        let mut g = self.state.inner.lock();
        g.events.clear();
        g.epoch = Instant::now();
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state.active.load(Ordering::Acquire)
    }

    pub(crate) fn now_ns(&self) -> u64 {
        let g = self.state.inner.lock();
        g.epoch.elapsed().as_nanos() as u64
    }

    pub(crate) fn record(
        &self,
        cat: String,
        name: String,
        start_ns: u64,
        dur_ns: u64,
        args: Vec<(String, String)>,
    ) {
        let mut g = self.state.inner.lock();
        g.events.push(TraceEvent {
            cat,
            name,
            start_ns,
            dur_ns,
            tid: current_tid(),
            args,
        });
    }

    pub(crate) fn snapshot(&self) -> Vec<TraceEvent> {
        self.state.inner.lock().events.clone()
    }

    pub(crate) fn event_count(&self) -> usize {
        self.state.inner.lock().events.len()
    }
}
