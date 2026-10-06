//! v0.75.25: 活 AI 基础设施类型（v0.24 引入，经 ai_chat.rs 实际调用）。
//!
//! 自 `src/ai_infra.rs` 迁入（v0.25 批次中 15 个类型的 3 个活成员：
//! ContextWindow/SpeculativeVerifier/CacheWarmer）。其余 12 个（CostOptimizer/
//! LoadBalancer/ModelSwitcher 之外的所有规划类型）全仓库零调用，出生即死，
//! 随旧文件删除。
//!
//! ⚠ v0.104.6 D90 更正：本文件头原称三个类型「被 `ai.chat` 调用」，**已过期**。
//! 可达性普查（2026-10-02）确认生产路径上 `ai.chat` 实际只用到
//! `ContextWindow::{add_message, compress, needs_compression}`、
//! `SpeculativeVerifier::verify`、`CacheWarmer::get_cached`；
//! **队列与缓存预热整族从未在生产运行**（`queue_verification` / `process_queue` /
//! `queue_len` / `add_request` / `next_request` / `cache_result` / `has_requests` /
//! `clear_cache` / `get_messages` 的生产引用数均为 0）。
//! 另：`verify` 自身经 `ai.chat` 也走不到 —— 推测路径门控 `cfg.speculative` 全仓
//! 唯一赋值是 `None`，语言侧 `with` 块设不了（落 `other =>` 报错）。
//! **未清理**：保留这些 API 供 v1.0 接线，清理属重构而非缺陷修复。

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
        // v0.104.6 D90：缓存键原先是 `"{draft.len()}:{verification.len()}"` ——
        // **只记长度、不记内容**。于是长度相同的不同输入会命中同一条缓存，
        // 第二次直接拿到第一次的判定。
        //
        // 与 `mir/vm/dag.rs` 的 dict 指纹缺陷**同型**（那里也是把
        // 「长度相同」当成「内容相同」）。
        //
        // ⚠ **当前不可达**：`ai_chat.rs` 的推测路径门控是
        // `cfg.speculative == Some(true)`，而 `speculative` 全仓唯一赋值是
        // `None`、无 setter，语言侧 `with` 块也设不了（会落 `other =>` 报错）。
        // 即**该路径是死代码**。若将来接上 `speculative`，
        // `is_verified == true` 就会 `return Ok(draft_response)` 直接把
        // **未验证的 draft 响应返回给用户** —— 这才是本修复要防的后果。
        // 保留修复是因为：`pub` 方法上留着一个会返回错误判定的地雷，
        // 修法只有一行。
        let cache_key = format!("{}\u{1}{}", draft, verification);
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

#[cfg(test)]
mod d90_tests {
    use super::*;

    /// v0.104.6 D90：`verify` 的缓存键是 `"{draft.len()}:{verification.len()}"`
    /// —— **只用长度，不用内容**。于是**长度相同、内容不同**的两对参数会命中
    /// 同一条缓存，第二次直接返回第一次的判定。
    ///
    /// 后果不是"统计不准"而是**返回未验证的 draft 响应**：
    /// `ai_chat.rs:397-400` 里 `is_verified == true` 就 `return Ok(draft_response)`。
    ///
    /// 修前：`verify("abc", "VERIFIED")` 缓存 `3:8 → true`，随后
    /// `verify("xyz", "NO WAY!!")`（同样 3 / 8 长）**命中缓存返回 true** ——
    /// 而后者本该是 `false`。
    #[test]
    fn d90_verify_cache_key_must_not_collide_on_equal_lengths() {
        let mut v = SpeculativeVerifier::default();
        assert!(v.verify("abc", "VERIFIED"), "第一次应判 true");
        // 长度完全相同、内容完全不同、且**不含** VERIFIED
        assert_eq!("NO WAY!!".len(), "VERIFIED".len(), "前提：两串等长");
        assert!(
            !v.verify("xyz", "NO WAY!!"),
            "等长但内容不同的验证不得命中上一条缓存 —— 修前会错误返回 true"
        );
    }

    /// 反向：先缓存 `false`，等长的真 VERIFIED 也不得被误判为 `false`。
    #[test]
    fn d90_verify_cache_key_must_not_pin_a_false_verdict() {
        let mut v = SpeculativeVerifier::default();
        assert!(!v.verify("abc", "WRONG!!!")); // 7 == 7
        assert!(
            v.verify("xyz", "VERIFIED"),
            "等长的真 VERIFIED 不得命中上一条 false —— 修前会错误返回 false"
        );
    }

    /// 队列批处理走的是同一个 `verify`，因此同一缺陷同样存在。
    #[test]
    fn d90_process_queue_shares_the_same_defect() {
        let mut v = SpeculativeVerifier::default();
        v.queue_verification("abc".into(), "VERIFIED".into());
        v.process_queue();
        assert_eq!(v.queue_len(), 0, "process_queue 应清空队列");
        v.queue_verification("xyz".into(), "NO WAY!!".into());
        v.process_queue();
        assert!(
            !v.verify("xyz", "NO WAY!!"),
            "队列里等长异内容的验证同样不得被上一条缓存污染"
        );
    }

    /// 相同内容重复验证仍应命中缓存（修法不应把缓存整个废掉）。
    #[test]
    fn d90_verify_still_caches_identical_input() {
        let mut v = SpeculativeVerifier::default();
        assert!(v.verify("draft-abc", "VERIFIED ok"));
        let before = v.verification_cache.len();
        assert!(v.verify("draft-abc", "VERIFIED ok"));
        assert_eq!(
            v.verification_cache.len(),
            before,
            "完全相同的输入应复用缓存条目"
        );
    }

    /// `ContextWindow` 的滑动窗口：超限时丢最老的消息，且至少保留 1 条。
    #[test]
    fn d90_context_window_slides_and_keeps_at_least_one() {
        let mut w = ContextWindow {
            max_tokens: 16, // 约 64 字节内容
            ..Default::default()
        };
        for _ in 0..8 {
            w.add_message("user".into(), "x".repeat(16)); // 4 tokens / 条
        }
        assert!(w.current_tokens <= 16 || w.messages.len() == 1);
        assert!(!w.messages.is_empty(), "窗口不得被清空");
        // 最新一条必须还在
        assert_eq!(w.messages.last().unwrap().0, "user");
    }

    /// `compress` 之后 `current_tokens` 必须与实际内容重新对齐，
    /// 否则会与 `messages` 不一致（`add_message` 的 while 会立刻再触发一轮）。
    #[test]
    fn d90_compress_realigns_current_tokens() {
        let mut w = ContextWindow::default();
        for _ in 0..10 {
            w.add_message("user".into(), "y".repeat(40)); // 10 tokens / 条
        }
        w.compress();
        let recomputed: usize = w.get_messages().iter().map(|(_, c)| c.len() / 4).sum();
        assert_eq!(
            w.current_tokens, recomputed,
            "compress 后 current_tokens 必须等于现存消息的 token 和"
        );
    }
}
