//! 压力控制基础设施
//!
//! v0.34: 为外部调用（AI/Web）提供配额和熔断能力。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

/// 熔断器三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// 为单个 endpoint 维护熔断状态。
#[derive(Debug)]
pub struct CircuitBreaker {
    state: CircuitState,
    failure_count: u32,
    success_count: u32,
    failure_threshold: u32,
    success_threshold: u32,
    open_until: Option<Instant>,
    open_duration: Duration,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            failure_count: 0,
            success_count: 0,
            failure_threshold: 5,
            success_threshold: 2,
            open_until: None,
            open_duration: Duration::from_secs(30),
        }
    }
}

impl CircuitBreaker {
    pub fn with_thresholds(failure_threshold: u32, success_threshold: u32, open_secs: u64) -> Self {
        Self {
            failure_threshold,
            success_threshold,
            open_duration: Duration::from_secs(open_secs),
            ..Default::default()
        }
    }

    /// 是否允许当前请求通过。
    pub fn allow(&mut self, now: Instant) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if let Some(t) = self.open_until {
                    if now >= t {
                        self.state = CircuitState::HalfOpen;
                        self.failure_count = 0;
                        self.success_count = 0;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// 记录成功。
    pub fn record_success(&mut self) {
        match self.state {
            CircuitState::Closed => {
                self.failure_count = 0;
            }
            CircuitState::HalfOpen => {
                self.success_count += 1;
                if self.success_count >= self.success_threshold {
                    self.state = CircuitState::Closed;
                    self.failure_count = 0;
                    self.success_count = 0;
                }
            }
            CircuitState::Open => {}
        }
    }

    /// 记录失败。
    pub fn record_failure(&mut self, now: Instant) {
        match self.state {
            CircuitState::Closed => {
                self.failure_count += 1;
                if self.failure_count >= self.failure_threshold {
                    self.state = CircuitState::Open;
                    self.open_until = Some(now + self.open_duration);
                }
            }
            CircuitState::HalfOpen => {
                self.state = CircuitState::Open;
                self.open_until = Some(now + self.open_duration);
                self.success_count = 0;
            }
            CircuitState::Open => {}
        }
    }

    pub fn state(&self) -> CircuitState {
        self.state
    }
}

/// 配额管理器：按 key 维护并发数、每分钟请求数。
#[derive(Debug, Default)]
pub struct QuotaManager {
    concurrent: HashMap<String, u32>,
    per_minute: HashMap<String, Vec<Instant>>,
}

impl QuotaManager {
    /// 尝试占用一个配额。成功返回 true，失败返回 false。
    pub fn acquire(
        &mut self,
        key: &str,
        max_concurrent: u32,
        max_per_minute: u32,
        now: Instant,
    ) -> bool {
        // 清理过期的 per-minute 记录
        let window = now - Duration::from_secs(60);
        let entries = self.per_minute.entry(key.to_string()).or_default();
        entries.retain(|t| *t > window);
        if entries.len() as u32 >= max_per_minute {
            return false;
        }

        let current = self.concurrent.entry(key.to_string()).or_insert(0);
        if *current >= max_concurrent {
            return false;
        }

        *current += 1;
        entries.push(now);
        true
    }

    /// 释放一个并发配额。
    pub fn release(&mut self, key: &str) {
        if let Some(c) = self.concurrent.get_mut(key) {
            *c = c.saturating_sub(1);
        }
    }
}

/// 全局压力控制 facade：为每个 endpoint 维护熔断器和配额。
#[derive(Debug, Default, Clone)]
pub struct PressureControl {
    breakers: Arc<Mutex<HashMap<String, CircuitBreaker>>>,
    quotas: Arc<Mutex<QuotaManager>>,
}

impl PressureControl {
    /// 执行一次外部调用。先检查熔断和配额，再执行 `f`，并根据结果更新熔断。
    pub async fn call<F, Fut, T, E>(
        &self,
        endpoint: &str,
        max_concurrent: u32,
        max_per_minute: u32,
        f: F,
    ) -> Result<T, E>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<T, E>> + Send,
        E: From<String>,
    {
        let now = Instant::now();

        // 1. 熔断检查
        {
            let mut breakers = self.breakers.lock().await;
            let breaker = breakers
                .entry(endpoint.to_string())
                .or_insert_with(CircuitBreaker::default);
            if !breaker.allow(now) {
                return Err(E::from(format!(
                    "circuit breaker open for endpoint: {}",
                    endpoint
                )));
            }
        }

        // 2. 配额检查
        {
            let mut quotas = self.quotas.lock().await;
            if !quotas.acquire(endpoint, max_concurrent, max_per_minute, now) {
                return Err(E::from(format!(
                    "quota exceeded for endpoint: {}",
                    endpoint
                )));
            }
        }

        // 3. 执行调用
        let result = f().await;

        // 4. 释放并发配额并更新熔断
        {
            let mut quotas = self.quotas.lock().await;
            quotas.release(endpoint);
        }
        {
            let mut breakers = self.breakers.lock().await;
            let breaker = breakers
                .entry(endpoint.to_string())
                .or_insert_with(CircuitBreaker::default);
            match result {
                Ok(_) => breaker.record_success(),
                Err(_) => breaker.record_failure(Instant::now()),
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn circuit_breaker_opens_after_failures() {
        let mut cb = CircuitBreaker::with_thresholds(3, 2, 1);
        let now = Instant::now();

        assert!(cb.allow(now));
        cb.record_failure(now);
        cb.record_failure(now);
        cb.record_failure(now);
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow(now));
    }

    #[tokio::test]
    async fn quota_limits_concurrent_and_per_minute() {
        let mut qm = QuotaManager::default();
        let now = Instant::now();

        assert!(qm.acquire("ai", 2, 3, now));
        assert!(qm.acquire("ai", 2, 3, now));
        assert!(!qm.acquire("ai", 2, 3, now)); // concurrent 满

        qm.release("ai");
        assert!(qm.acquire("ai", 2, 3, now)); // per-minute 累计 3
        assert!(!qm.acquire("ai", 2, 3, now)); // per-minute 满
    }

    #[tokio::test]
    async fn pressure_control_blocks_when_open() {
        let pc = PressureControl::default();
        // 把熔断器弄开
        {
            let mut breakers = pc.breakers.lock().await;
            let cb = breakers
                .entry("x".to_string())
                .or_insert_with(CircuitBreaker::default);
            let now = Instant::now();
            for _ in 0..5 {
                cb.record_failure(now);
            }
        }

        let r: Result<i32, String> = pc.call("x", 10, 10, || async move { Ok(42) }).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("circuit breaker open"));
    }
}
