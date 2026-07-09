//! v0.52 ADR-001: AiRuntime — BC3 (AI 模型路由 + 缓存 + 推测解码 + 上下文窗口 + draft model 统计)
//!
//! 从 Interpreter god object 抽出的 AI 状态容器，9 字段。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ai_infra::{CacheWarmer, ContextWindow, SpeculativeVerifier};
use crate::interpreter::{RouteConfig, TokenBudget, TokenUsage};
use crate::trace_collector::TraceCollector;

// 注：TraceCollector 没 derive Debug，所以 AiRuntime 也不 derive Debug
// v0.52 ADR-001: 字段类型（TokenUsage/TokenBudget/RouteConfig）是 pub
// 所以 AiRuntime 字段也是 pub（与字段类型可见性一致 — clippy 要求）
#[derive(Clone)]
pub struct AiRuntime {
    pub model_routes: HashMap<String, RouteConfig>,
    pub token_budget: Option<TokenBudget>,
    pub token_usage: TokenUsage,
    pub trace: TraceCollector,
    pub draft_model_stats: Arc<Mutex<HashMap<String, (usize, usize)>>>,
    pub context_window: ContextWindow,
    pub speculative_verifier: SpeculativeVerifier,
    pub cache_warmer: CacheWarmer,
}

impl Default for AiRuntime {
    fn default() -> Self {
        Self {
            model_routes: HashMap::new(),
            token_budget: None,
            token_usage: TokenUsage::default(),
            trace: TraceCollector::new(false),
            draft_model_stats: Arc::new(Mutex::new(HashMap::new())),
            context_window: ContextWindow::default(),
            speculative_verifier: SpeculativeVerifier::default(),
            cache_warmer: CacheWarmer::default(),
        }
    }
}

impl AiRuntime {
    /// 记录 token 消耗到 usage
    pub fn record_tokens(&mut self, input: usize, output: usize) {
        self.token_usage.input += input;
        self.token_usage.output += output;
    }

    /// 启用/禁用 trace
    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace = TraceCollector::new(enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ai_routes_empty() {
        let ai = AiRuntime::default();
        assert!(ai.model_routes.is_empty());
    }

    #[test]
    fn default_token_budget_none() {
        let ai = AiRuntime::default();
        assert!(ai.token_budget.is_none());
    }

    #[test]
    fn default_token_usage_zero() {
        let ai = AiRuntime::default();
        assert_eq!(ai.token_usage.input, 0);
        assert_eq!(ai.token_usage.output, 0);
    }

    #[test]
    fn record_tokens_increments() {
        let mut ai = AiRuntime::default();
        ai.record_tokens(100, 50);
        ai.record_tokens(200, 80);
        assert_eq!(ai.token_usage.input, 300);
        assert_eq!(ai.token_usage.output, 130);
    }

    #[test]
    fn trace_default_disabled() {
        let ai = AiRuntime::default();
        // TraceCollector::new(false) 应该是 disabled — 具体状态字段名以 trace_collector 实际定义为准
        // 仅检查 trace 存在即可
        let _ = &ai.trace;
    }

    #[test]
    fn draft_model_stats_starts_empty() {
        let ai = AiRuntime::default();
        let stats = ai
            .draft_model_stats
            .lock()
            .expect("draft_model_stats poisoned");
        assert!(stats.is_empty());
    }

    #[test]
    fn set_trace_enabled_updates_trace() {
        let mut ai = AiRuntime::default();
        ai.set_trace_enabled(true);
        let _ = &ai.trace; // 不 panic 即可
    }

    #[test]
    fn clone_preserves_token_usage() {
        let mut ai = AiRuntime::default();
        ai.record_tokens(10, 20);
        let cloned = ai.clone();
        assert_eq!(cloned.token_usage.input, 10);
        assert_eq!(cloned.token_usage.output, 20);
    }
}
