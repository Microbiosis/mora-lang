//! v0.75.25: 活 AI 基础设施类型（v0.24 引入，经 ai_chat.rs 实际调用）。
//!
//! 自 `src/ai_infra.rs` 迁入（v0.25 批次中 15 个类型的 3 个活成员：
//! ContextWindow/SpeculativeVerifier/CacheWarmer，被 AiRuntime 持有、
//! `ai.chat` 调用）。其余 12 个（CostOptimizer/LoadBalancer/ModelSwitcher/
//! SpeculativeVerifier 之外的所有规划类型）全仓库零调用，出生即死，随旧
//! 文件删除。

use std::collections::HashMap;

/// v0.24: 上下文窗口管理器 — 维护消息滑动窗口，超阈值时压缩。
/// `ai.chat` 每次调用 add_message，窗口超限时 compress（保留尾部）。
#[derive(Clone, Debug)]
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
    pub fn add_message(&mut self, role: String, content: String) {
        let tokens = content.len() / 4;
        self.messages.push((role, content));
        self.current_tokens += tokens;
        while self.current_tokens > self.max_tokens && self.messages.len() > 1 {
            let removed = self.messages.remove(0);
            self.current_tokens -= removed.1.len() / 4;
        }
    }

    pub fn get_messages(&self) -> &[(String, String)] {
        &self.messages
    }

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

/// v0.24: 推测解码验证器 — draft 响应与验证文本一致性检查。
/// `ai.chat` 在 speculative 路径调用 verify()。
#[derive(Clone, Debug, Default)]
pub struct SpeculativeVerifier {
    pub verification_cache: HashMap<String, bool>,
    pub parallel_count: usize,
    pub verification_queue: Vec<(String, String)>,
}

impl SpeculativeVerifier {
    pub fn verify(&mut self, draft: &str, verification: &str) -> bool {
        let cache_key = format!("{}:{}", draft.len(), verification.len());
        if let Some(&cached) = self.verification_cache.get(&cache_key) {
            return cached;
        }
        let result = verification.contains("VERIFIED");
        self.verification_cache.insert(cache_key, result);
        result
    }

    pub fn clear_cache(&mut self) {
        self.verification_cache.clear();
    }

    pub fn queue_verification(&mut self, draft: String, verification: String) {
        self.verification_queue.push((draft, verification));
    }

    pub fn process_queue(&mut self) {
        let queue = std::mem::take(&mut self.verification_queue);
        for (draft, verification) in queue {
            self.verify(&draft, &verification);
        }
    }

    pub fn queue_len(&self) -> usize {
        self.verification_queue.len()
    }
}

/// v0.24: AI 调用缓存预热器 — prompt → response 缓存。
/// `ai.chat` 通过 get_cached 命中缓存（cache_key 由调用方构造）。
#[derive(Clone, Debug, Default)]
pub struct CacheWarmer {
    pub queue: Vec<String>,
    pub cache: HashMap<String, String>,
    pub warming: bool,
}

impl CacheWarmer {
    pub fn add_request(&mut self, prompt: String) {
        self.queue.push(prompt);
    }

    pub fn next_request(&mut self) -> Option<String> {
        self.queue.pop()
    }

    pub fn cache_result(&mut self, prompt: String, response: String) {
        self.cache.insert(prompt, response);
    }

    pub fn get_cached(&self, prompt: &str) -> Option<&String> {
        self.cache.get(prompt)
    }

    pub fn has_requests(&self) -> bool {
        !self.queue.is_empty()
    }
}
