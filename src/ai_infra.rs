//! v0.24: AI 基础设施结构体
//!
//! 从 interpreter.rs 提取的独立 AI 基础设施组件：
//! - 自适应温度调整器
//! - 上下文窗口管理器
//! - 模型负载均衡器
//! - 推测解码验证器
//! - 缓存预热器
//! - 智能缓存淘汰
//! - 模型切换策略
//! - 性能基准测试
//! - 调用链路追踪
//! - 自适应批处理
//! - 性能可视化
//! - 成本优化器
//! - 重试策略

use std::collections::HashMap;

/// v0.24: 自适应温度调整器
#[derive(Clone, Debug)]
#[allow(dead_code)] // 未来扩展用
pub struct AdaptiveTemperature {
    pub base: f64,
    pub current: f64,
    pub min: f64,
    pub max: f64,
    pub success_rate: f64,
    pub step: f64,
}

impl Default for AdaptiveTemperature {
    fn default() -> Self {
        Self {
            base: 0.7,
            current: 0.7,
            min: 0.1,
            max: 1.5,
            success_rate: 1.0,
            step: 0.05,
        }
    }
}

impl AdaptiveTemperature {
    #[allow(dead_code)]
    pub fn adjust(&mut self, success: bool) {
        if success {
            self.current = (self.current - self.step).max(self.min);
        } else {
            self.current = (self.current + self.step).min(self.max);
        }
    }

    #[allow(dead_code)]
    pub fn get(&self) -> f64 {
        self.current
    }
}

/// v0.24: 上下文窗口管理器
#[derive(Clone, Debug)]
#[allow(dead_code)] // 未来扩展用
pub struct ContextWindow {
    pub max_tokens: usize,
    pub current_tokens: usize,
    pub messages: Vec<(String, String)>,
    pub compression_threshold: f64,
    pub compression_ratio: f64,
}

impl Default for ContextWindow {
    fn default() -> Self {
        Self {
            max_tokens: 4096,
            current_tokens: 0,
            messages: Vec::new(),
            compression_threshold: 0.8,
            compression_ratio: 0.5,
        }
    }
}

impl ContextWindow {
    #[allow(dead_code)]
    pub fn add_message(&mut self, role: String, content: String) {
        let tokens = content.len() / 4;
        self.messages.push((role, content));
        self.current_tokens += tokens;
        while self.current_tokens > self.max_tokens && self.messages.len() > 1 {
            let removed = self.messages.remove(0);
            self.current_tokens -= removed.1.len() / 4;
        }
    }

    #[allow(dead_code)]
    pub fn get_messages(&self) -> &[(String, String)] {
        &self.messages
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.messages.clear();
        self.current_tokens = 0;
    }

    pub fn compress(&mut self) {
        let threshold = (self.max_tokens as f64 * self.compression_threshold) as usize;
        if self.current_tokens <= threshold {
            return;
        }
        let keep_count = (self.messages.len() as f64 * self.compression_ratio).max(1.0) as usize;
        let start = self.messages.len() - keep_count;
        self.messages = self.messages[start..].to_vec();
        self.current_tokens = self.messages.iter().map(|(_, c)| c.len() / 4).sum();
    }

    pub fn needs_compression(&self) -> bool {
        let threshold = (self.max_tokens as f64 * self.compression_threshold) as usize;
        self.current_tokens > threshold
    }
}

/// v0.24: 模型负载均衡器
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // 未来扩展用
pub struct LoadBalancer {
    pub models: Vec<String>,
    pub current_index: usize,
    pub weights: Vec<f64>,
    pub loads: Vec<f64>,
}

impl LoadBalancer {
    #[allow(dead_code)]
    pub fn add_model(&mut self, model: String, weight: f64) {
        self.models.push(model);
        self.weights.push(weight);
        self.loads.push(0.0);
    }

    #[allow(dead_code)]
    pub fn select(&mut self) -> Option<String> {
        if self.models.is_empty() {
            return None;
        }
        let min_index = self
            .loads
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.loads[min_index] += self.weights[min_index];
        Some(self.models[min_index].clone())
    }

    #[allow(dead_code)]
    pub fn complete(&mut self, model: &str) {
        if let Some(index) = self.models.iter().position(|m| m == model) {
            self.loads[index] = (self.loads[index] - self.weights[index]).max(0.0);
        }
    }
}

/// v0.24: 推测解码并行验证器
#[derive(Clone, Debug)]
#[allow(dead_code)] // 未来扩展用
pub struct SpeculativeVerifier {
    pub verification_cache: HashMap<String, bool>,
    pub parallel_count: usize,
    pub verification_queue: Vec<(String, String)>,
}

impl Default for SpeculativeVerifier {
    fn default() -> Self {
        Self {
            verification_cache: HashMap::new(),
            parallel_count: 4,
            verification_queue: Vec::new(),
        }
    }
}

impl SpeculativeVerifier {
    #[allow(dead_code)]
    pub fn verify(&mut self, draft: &str, verification: &str) -> bool {
        let cache_key = format!("{}:{}", draft.len(), verification.len());
        if let Some(&cached) = self.verification_cache.get(&cache_key) {
            return cached;
        }
        let result = verification.contains("VERIFIED");
        self.verification_cache.insert(cache_key, result);
        result
    }

    #[allow(dead_code)]
    pub fn clear_cache(&mut self) {
        self.verification_cache.clear();
    }

    #[allow(dead_code)]
    pub fn queue_verification(&mut self, draft: String, verification: String) {
        self.verification_queue.push((draft, verification));
    }

    #[allow(dead_code)]
    pub fn process_queue(&mut self) {
        let queue = std::mem::take(&mut self.verification_queue);
        for (draft, verification) in queue {
            self.verify(&draft, &verification);
        }
    }

    #[allow(dead_code)]
    pub fn queue_len(&self) -> usize {
        self.verification_queue.len()
    }
}

/// v0.24: AI 调用缓存预热器
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // 未来扩展用
pub struct CacheWarmer {
    pub queue: Vec<String>,
    pub cache: HashMap<String, String>,
    pub warming: bool,
}

impl CacheWarmer {
    #[allow(dead_code)]
    pub fn add_request(&mut self, prompt: String) {
        self.queue.push(prompt);
    }
    #[allow(dead_code)]
    pub fn next_request(&mut self) -> Option<String> {
        self.queue.pop()
    }
    #[allow(dead_code)]
    pub fn cache_result(&mut self, prompt: String, response: String) {
        self.cache.insert(prompt, response);
    }
    #[allow(dead_code)]
    pub fn get_cached(&self, prompt: &str) -> Option<&String> {
        self.cache.get(prompt)
    }
    #[allow(dead_code)]
    pub fn has_requests(&self) -> bool {
        !self.queue.is_empty()
    }
}

/// v0.24: 智能缓存淘汰策略
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SmartCacheEviction {
    pub entries: HashMap<String, (String, usize, u64, f64)>,
    pub max_size: usize,
    pub strategy: EvictionStrategy,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum EvictionStrategy {
    Lru,
    Lfu,
    CostAware,
    Hybrid,
}

impl Default for SmartCacheEviction {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            max_size: 1000,
            strategy: EvictionStrategy::Hybrid,
        }
    }
}

impl SmartCacheEviction {
    #[allow(dead_code)]
    pub fn insert(&mut self, key: String, value: String, cost: f64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if self.entries.len() >= self.max_size {
            self.evict();
        }
        self.entries.insert(key, (value, 1, now, cost));
    }

    #[allow(dead_code)]
    pub fn get(&mut self, key: &str) -> Option<String> {
        if let Some((value, access_count, last_access, _)) = self.entries.get_mut(key) {
            *access_count += 1;
            *last_access = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            Some(value.clone())
        } else {
            None
        }
    }

    pub fn evict(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let key_to_remove = match self.strategy {
            EvictionStrategy::Lru => self
                .entries
                .iter()
                .min_by_key(|(_, (_, _, last_access, _))| *last_access)
                .map(|(key, _)| key.clone()),
            EvictionStrategy::Lfu => self
                .entries
                .iter()
                .min_by_key(|(_, (_, access_count, _, _))| *access_count)
                .map(|(key, _)| key.clone()),
            EvictionStrategy::CostAware => self
                .entries
                .iter()
                .min_by(|(_, (_, _, _, a)), (_, (_, _, _, b))| {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(key, _)| key.clone()),
            EvictionStrategy::Hybrid => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.entries
                    .iter()
                    .min_by(
                        |(_, (.., a_count, a_time, a_cost)), (_, (.., b_count, b_time, b_cost))| {
                            let score_a = (*a_count as f64)
                                * (1.0 / (now - a_time + 1) as f64)
                                * (1.0 / (a_cost + 0.001));
                            let score_b = (*b_count as f64)
                                * (1.0 / (now - b_time + 1) as f64)
                                * (1.0 / (b_cost + 0.001));
                            score_a
                                .partial_cmp(&score_b)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        },
                    )
                    .map(|(key, _)| key.clone())
            }
        };
        if let Some(key) = key_to_remove {
            self.entries.remove(&key);
        }
    }

    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        self.entries.len()
    }
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// v0.24: 模型切换策略
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct ModelSwitcher {
    pub task_model_map: HashMap<String, String>,
    pub model_stats: HashMap<String, (f64, f64)>,
}

impl ModelSwitcher {
    #[allow(dead_code)]
    pub fn register_task(&mut self, task_type: String, model: String) {
        self.task_model_map.insert(task_type, model);
    }
    #[allow(dead_code)]
    pub fn select_model(&self, task_type: &str) -> Option<String> {
        self.task_model_map.get(task_type).cloned()
    }

    #[allow(dead_code)]
    pub fn update_stats(&mut self, model: &str, latency: f64, success: bool) {
        let stats = self
            .model_stats
            .entry(model.to_string())
            .or_insert((0.0, 1.0));
        stats.0 = (stats.0 + latency) / 2.0;
        if success {
            stats.1 = (stats.1 + 1.0) / 2.0;
        } else {
            stats.1 /= 2.0;
        }
    }

    #[allow(dead_code)]
    pub fn best_model(&self) -> Option<String> {
        self.model_stats
            .iter()
            .max_by(|(_, a), (_, b)| {
                let score_a = a.1 / (a.0 + 1.0);
                let score_b = b.1 / (b.0 + 1.0);
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(model, _)| model.clone())
    }
}

/// v0.24: 模型性能基准测试
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ModelBenchmark {
    pub results: HashMap<String, (f64, f64, f64)>,
    pub test_cases: Vec<String>,
}

impl Default for ModelBenchmark {
    fn default() -> Self {
        Self {
            results: HashMap::new(),
            test_cases: vec![
                "What is 2+2?".to_string(),
                "Explain quantum computing in one sentence.".to_string(),
                "Write a haiku about programming.".to_string(),
            ],
        }
    }
}

impl ModelBenchmark {
    #[allow(dead_code)]
    pub fn record_result(
        &mut self,
        model: &str,
        latency_ms: f64,
        tokens_per_sec: f64,
        success: bool,
    ) {
        let entry = self
            .results
            .entry(model.to_string())
            .or_insert((0.0, 0.0, 0.0));
        entry.0 = (entry.0 + latency_ms) / 2.0;
        entry.1 = (entry.1 + tokens_per_sec) / 2.0;
        if success {
            entry.2 = (entry.2 + 1.0) / 2.0;
        } else {
            entry.2 /= 2.0;
        }
    }

    #[allow(dead_code)]
    pub fn best_model(&self) -> Option<String> {
        self.results
            .iter()
            .max_by(|(_, a), (_, b)| {
                let score_a = a.2 * a.1 / (a.0 + 1.0);
                let score_b = b.2 * b.1 / (b.0 + 1.0);
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(model, _)| model.clone())
    }

    #[allow(dead_code)]
    pub fn get_test_cases(&self) -> &[String] {
        &self.test_cases
    }
}

/// v0.24: AI 调用链路追踪
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AiCallTracer {
    pub traces: HashMap<String, Vec<CallSpan>>,
    pub current_trace: Option<String>,
    pub max_depth: usize,
}

/// v0.24: 调用跨度
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CallSpan {
    pub id: String,
    pub parent_id: Option<String>,
    pub model: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub success: bool,
    pub error: Option<String>,
}

impl Default for AiCallTracer {
    fn default() -> Self {
        Self {
            traces: HashMap::new(),
            current_trace: None,
            max_depth: 10,
        }
    }
}

impl AiCallTracer {
    #[allow(dead_code)]
    pub fn start_trace(&mut self) -> String {
        let trace_id = format!(
            "trace_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        self.current_trace = Some(trace_id.clone());
        self.traces.insert(trace_id.clone(), Vec::new());
        trace_id
    }

    #[allow(dead_code)]
    pub fn record_span(&mut self, span: CallSpan) {
        if let Some(trace_id) = &self.current_trace
            && let Some(trace) = self.traces.get_mut(trace_id)
        {
            trace.push(span);
        }
    }

    #[allow(dead_code)]
    pub fn end_trace(&mut self) -> Option<String> {
        self.current_trace.take()
    }

    #[allow(dead_code)]
    pub fn get_trace(&self) -> Option<&Vec<CallSpan>> {
        self.current_trace
            .as_ref()
            .and_then(|id| self.traces.get(id))
    }
}

/// v0.24: 自适应批处理大小
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AdaptiveBatchSize {
    pub current_size: usize,
    pub min_size: usize,
    pub max_size: usize,
    pub success_threshold: f64,
    pub recent_success_rate: f64,
    pub step: usize,
}

impl Default for AdaptiveBatchSize {
    fn default() -> Self {
        Self {
            current_size: 10,
            min_size: 1,
            max_size: 100,
            success_threshold: 0.9,
            recent_success_rate: 1.0,
            step: 5,
        }
    }
}

impl AdaptiveBatchSize {
    #[allow(dead_code)]
    pub fn adjust(&mut self, success_rate: f64) {
        self.recent_success_rate = success_rate;
        if success_rate < self.success_threshold {
            self.current_size = (self.current_size - self.step).max(self.min_size);
        } else {
            self.current_size = (self.current_size + self.step).min(self.max_size);
        }
    }

    #[allow(dead_code)]
    pub fn get(&self) -> usize {
        self.current_size
    }
}

/// v0.24: 模型性能可视化
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ModelPerformanceVisualizer {
    pub performance_data: HashMap<String, Vec<(u64, f64, f64)>>,
    pub max_data_points: usize,
}

impl Default for ModelPerformanceVisualizer {
    fn default() -> Self {
        Self {
            performance_data: HashMap::new(),
            max_data_points: 100,
        }
    }
}

impl ModelPerformanceVisualizer {
    #[allow(dead_code)]
    pub fn record(&mut self, model: &str, latency_ms: f64, tokens_per_sec: f64) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let entry = self.performance_data.entry(model.to_string()).or_default();
        entry.push((timestamp, latency_ms, tokens_per_sec));
        if entry.len() > self.max_data_points {
            entry.remove(0);
        }
    }

    #[allow(dead_code)]
    pub fn avg_latency(&self, model: &str) -> Option<f64> {
        self.performance_data
            .get(model)
            .map(|data| data.iter().map(|(_, latency, _)| latency).sum::<f64>() / data.len() as f64)
    }

    #[allow(dead_code)]
    pub fn avg_speed(&self, model: &str) -> Option<f64> {
        self.performance_data
            .get(model)
            .map(|data| data.iter().map(|(_, _, speed)| speed).sum::<f64>() / data.len() as f64)
    }

    #[allow(dead_code)]
    pub fn generate_report(&self) -> String {
        let mut report = String::from("Model Performance Report\n========================\n\n");
        for (model, data) in &self.performance_data {
            if data.is_empty() {
                continue;
            }
            let avg_latency: f64 = data.iter().map(|(_, l, _)| l).sum::<f64>() / data.len() as f64;
            let avg_speed: f64 = data.iter().map(|(_, _, s)| s).sum::<f64>() / data.len() as f64;
            report.push_str(&format!("Model: {}\n  Data points: {}\n  Avg latency: {:.1}ms\n  Avg speed: {:.1} tokens/sec\n\n", model, data.len(), avg_latency, avg_speed));
        }
        report
    }
}

/// v0.24: AI 调用成本优化器
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CostOptimizer {
    pub pricing: HashMap<String, (f64, f64)>,
    pub total_cost: f64,
    pub budget: Option<f64>,
    pub warning_threshold: f64,
}

impl Default for CostOptimizer {
    fn default() -> Self {
        let mut pricing = HashMap::new();
        pricing.insert("gpt-4o".to_string(), (0.005, 0.015));
        pricing.insert("gpt-4o-mini".to_string(), (0.00015, 0.0006));
        pricing.insert("gpt-3.5-turbo".to_string(), (0.0005, 0.0015));
        pricing.insert("claude-3-opus".to_string(), (0.015, 0.075));
        pricing.insert("claude-3-sonnet".to_string(), (0.003, 0.015));
        pricing.insert("claude-3-haiku".to_string(), (0.00025, 0.00125));
        Self {
            pricing,
            total_cost: 0.0,
            budget: None,
            warning_threshold: 0.8,
        }
    }
}

impl CostOptimizer {
    #[allow(dead_code)]
    pub fn calculate_cost(&self, model: &str, tokens_in: usize, tokens_out: usize) -> f64 {
        let (input_price, output_price) = self.pricing.get(model).unwrap_or(&(0.001, 0.002));
        (tokens_in as f64 / 1000.0) * input_price + (tokens_out as f64 / 1000.0) * output_price
    }

    #[allow(dead_code)]
    pub fn record_cost(&mut self, model: &str, tokens_in: usize, tokens_out: usize) -> f64 {
        let cost = self.calculate_cost(model, tokens_in, tokens_out);
        self.total_cost += cost;
        cost
    }

    #[allow(dead_code)]
    pub fn is_over_budget(&self) -> bool {
        self.budget.is_some_and(|b| self.total_cost >= b)
    }
    #[allow(dead_code)]
    pub fn needs_warning(&self) -> bool {
        self.budget
            .is_some_and(|b| self.total_cost >= b * self.warning_threshold)
    }
    #[allow(dead_code)]
    pub fn get_total_cost(&self) -> f64 {
        self.total_cost
    }
    #[allow(dead_code)]
    pub fn set_budget(&mut self, budget: f64) {
        self.budget = Some(budget);
    }

    #[allow(dead_code)]
    pub fn cheapest_model(&self, tokens_in: usize, tokens_out: usize) -> Option<String> {
        self.pricing
            .iter()
            .min_by(|(_, a), (_, b)| {
                let cost_a = (tokens_in as f64 / 1000.0) * a.0 + (tokens_out as f64 / 1000.0) * a.1;
                let cost_b = (tokens_in as f64 / 1000.0) * b.0 + (tokens_out as f64 / 1000.0) * b.1;
                cost_a
                    .partial_cmp(&cost_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(model, _)| model.clone())
    }
}

/// v0.24: 重试策略
#[derive(Clone, Debug)]
#[allow(dead_code)] // 未来扩展用
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_factor: f64,
    pub current_retry: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_factor: 2.0,
            current_retry: 0,
        }
    }
}

impl RetryPolicy {
    #[allow(dead_code)]
    pub fn next_delay(&mut self) -> u64 {
        let delay = (self.base_delay_ms as f64
            * self.backoff_factor.powi(self.current_retry as i32)) as u64;
        self.current_retry += 1;
        delay.min(self.max_delay_ms)
    }

    #[allow(dead_code)]
    pub fn should_retry(&self) -> bool {
        self.current_retry < self.max_retries
    }
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.current_retry = 0;
    }

    #[allow(dead_code)]
    pub fn should_retry_for_error(&self, error: &str) -> bool {
        if error.contains("connection") || error.contains("timeout") || error.contains("network") {
            return true;
        }
        if error.contains("429") || error.contains("rate limit") {
            return true;
        }
        if error.contains("500") || error.contains("502") || error.contains("503") {
            return true;
        }
        if error.contains("400") || error.contains("401") || error.contains("403") {
            return false;
        }
        true
    }
}
