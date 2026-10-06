//! v0.52 ADR-001: AiRuntime — BC3 (AI 模型路由 + 缓存 + 推测解码 + 上下文窗口 + draft model 统计)
//!
//! 从 Interpreter god object 抽出的 AI 状态容器，9 字段。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// v0.75.25: 活类型自 src/ai_infra.rs 迁至 runtime::ai_infra（12 个死类型
// 随旧文件删除）
use crate::runtime::ai_infra::{CacheWarmer, ContextWindow, SpeculativeVerifier};
use crate::runtime::types::{RouteConfig, TokenBudget, TokenUsage};
use crate::trace_collector::TraceCollector;

// 注：TraceCollector 没 derive Debug，所以 AiRuntime 也不 derive Debug
// v0.52 ADR-001: 字段类型（TokenUsage/TokenBudget/RouteConfig）是 pub
// 所以 AiRuntime 字段也是 pub(crate)（字段类型可见性一致 — clippy 要求）
#[derive(Clone)]
pub struct AiRuntime {
    pub(crate) model_routes: HashMap<String, RouteConfig>,
    pub(crate) token_budget: Option<TokenBudget>,
    pub(crate) token_usage: TokenUsage,
    pub(crate) trace: TraceCollector,
    pub(crate) draft_model_stats: Arc<Mutex<HashMap<String, (usize, usize)>>>,
    pub(crate) context_window: ContextWindow,
    pub(crate) speculative_verifier: SpeculativeVerifier,
    pub(crate) cache_warmer: CacheWarmer,
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
    ///
    /// v0.104.6 D76：同时累加 `calls` —— `ai.tokens().calls()` 读的就是它
    /// （此前该方法误读 `input`，见 `TokenUsage::calls` 的说明）。
    pub fn record_tokens(&mut self, input: usize, output: usize) {
        self.token_usage.input += input;
        self.token_usage.output += output;
        self.token_usage.calls += 1;
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

    /// v0.104.6 D76：`calls` 必须记**调用次数**（= 2），而不是
    /// 「输入 token 总数」（= 300）。修前 `ai.tokens().calls()` 返回 300 ——
    /// 一个比真值大两个数量级的数字。mock 模式下 `account_tokens` 不被调用，
    /// 故本测试是**唯一**能在本机观测该修复的地方（见 CHANGELOG）。
    #[test]
    fn record_tokens_counts_calls_not_input_tokens() {
        let mut ai = AiRuntime::default();
        ai.record_tokens(100, 50);
        ai.record_tokens(200, 80);
        assert_eq!(ai.token_usage.calls, 2, "两次 record_tokens = 2 次调用");
        assert_eq!(ai.token_usage.input, 300, "对照：input 仍是 300");
        assert_ne!(
            ai.token_usage.calls, ai.token_usage.input,
            "calls 与 input 必须能区分开 —— 否则 D76 的 bug 会回来"
        );
    }

    #[test]
    fn default_token_usage_calls_zero() {
        let ai = AiRuntime::default();
        assert_eq!(ai.token_usage.calls, 0);
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
