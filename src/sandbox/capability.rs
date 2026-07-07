//! v0.42.0: Capability Token System (loongclaw-inspired)
//!
//! 灵感: loongclaw `crates/contracts/src/contracts.rs:24-52`
//! - `Capability` enum: 13 variants in loongclaw; mora 子集 13 个
//! - `CapabilityToken { token_id, allowed, denied, expires_at, generation }`
//! - `PolicyEngine` trait: issue / authorize / revoke
//!
//! 设计 vs v0.33 sandbox:
//! - **v0.33**: pattern-based (`allow/deny` BTreeSet + event wildcard match)
//!   适合 builtin 名称过滤
//! - **v0.42.0**: token-based (explicit capability 列表 + expiry + generation)
//!   适合细粒度、生命周期受控的授权 (e.g. "允许 file.read 5 分钟")
//!
//! 两者**并存**: pattern-based 用于 builtin dispatch, token-based 用于
//! runtime `sandbox.key { ... }` 内置调用

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

/// v0.42.0: Capability 类型 (mora 子集, 13 variants 对应 loongclaw 风格)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    FileRead,
    FileWrite,
    WebFetch,
    WebSearch,
    ExecBash,
    ExecParallel,
    MemoryRead,
    MemoryWrite,
    AuditEmit,
    BusSubscribe,
    BusPublish,
    AgentInvoke,
    AgentRegister,
}

impl Capability {
    /// 解析 capability 字符串 (e.g. "file.read")
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file.read" => Some(Self::FileRead),
            "file.write" => Some(Self::FileWrite),
            "web.fetch" => Some(Self::WebFetch),
            "web.search" => Some(Self::WebSearch),
            "exec.bash" => Some(Self::ExecBash),
            "exec.parallel" => Some(Self::ExecParallel),
            "memory.read" => Some(Self::MemoryRead),
            "memory.write" => Some(Self::MemoryWrite),
            "audit.emit" => Some(Self::AuditEmit),
            "bus.subscribe" => Some(Self::BusSubscribe),
            "bus.publish" => Some(Self::BusPublish),
            "agent.invoke" => Some(Self::AgentInvoke),
            "agent.register" => Some(Self::AgentRegister),
            _ => None,
        }
    }

    /// capability 字符串形式 (反向 parse)
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FileRead => "file.read",
            Self::FileWrite => "file.write",
            Self::WebFetch => "web.fetch",
            Self::WebSearch => "web.search",
            Self::ExecBash => "exec.bash",
            Self::ExecParallel => "exec.parallel",
            Self::MemoryRead => "memory.read",
            Self::MemoryWrite => "memory.write",
            Self::AuditEmit => "audit.emit",
            Self::BusSubscribe => "bus.subscribe",
            Self::BusPublish => "bus.publish",
            Self::AgentInvoke => "agent.invoke",
            Self::AgentRegister => "agent.register",
        }
    }

    /// 全部 capabilities (test helper)
    pub fn all() -> &'static [Capability] {
        &[
            Self::FileRead,
            Self::FileWrite,
            Self::WebFetch,
            Self::WebSearch,
            Self::ExecBash,
            Self::ExecParallel,
            Self::MemoryRead,
            Self::MemoryWrite,
            Self::AuditEmit,
            Self::BusSubscribe,
            Self::BusPublish,
            Self::AgentInvoke,
            Self::AgentRegister,
        ]
    }
}

/// v0.42.0: Capability Token (loongclaw CapabilityToken 子集)
///
/// - `token_id`: 单调递增, 全局唯一
/// - `allowed`: 已授权 capability 集合
/// - `denied`: 已拒绝 (覆盖 allowed; 显式 deny 优先)
/// - `expires_at`: None = 永不过期
/// - `generation`: 用于撤销递增; revoke 后旧 token 的 generation 失配
/// - `created_at`: 审计用
#[derive(Debug, Clone)]
pub struct CapabilityToken {
    pub token_id: u64,
    pub allowed: BTreeSet<Capability>,
    pub denied: BTreeSet<Capability>,
    pub expires_at: Option<SystemTime>,
    pub generation: u32,
    pub created_at: SystemTime,
}

impl CapabilityToken {
    /// 检查 token 是否仍 alive (未过期)
    pub fn is_alive(&self, now: SystemTime) -> bool {
        match self.expires_at {
            None => true,
            Some(exp) => now < exp,
        }
    }

    /// 检查 token 是否授权给定 capability
    /// 规则: deny 优先 (显式拒绝覆盖允许) → alive 检查 → allowed 集合
    pub fn permits(&self, cap: Capability, now: SystemTime) -> bool {
        if self.denied.contains(&cap) {
            return false;
        }
        if !self.is_alive(now) {
            return false;
        }
        self.allowed.contains(&cap)
    }
}

/// v0.42.0: Sandbox 错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// 未知的 capability 字符串
    UnknownCapability(String),
    /// token 已过期
    TokenExpired { token_id: u64 },
    /// token 不存在 (可能被 revoke)
    TokenNotFound { token_id: u64 },
    /// capability 违反 (已 deny 或未 allow)
    CapViolation {
        token_id: u64,
        capability: Capability,
    },
    /// 内部错误 (generation mismatch 等)
    GenerationMismatch {
        token_id: u64,
        expected: u32,
        actual: u32,
    },
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCapability(s) => write!(f, "unknown capability: '{}'", s),
            Self::TokenExpired { token_id } => write!(f, "capability token {} expired", token_id),
            Self::TokenNotFound { token_id } => {
                write!(f, "capability token {} not found (revoked?)", token_id)
            }
            Self::CapViolation {
                token_id,
                capability,
            } => {
                write!(
                    f,
                    "capability token {} does not permit '{}'",
                    token_id,
                    capability.as_str()
                )
            }
            Self::GenerationMismatch {
                token_id,
                expected,
                actual,
            } => write!(
                f,
                "capability token {} generation mismatch: expected {}, got {}",
                token_id, expected, actual
            ),
        }
    }
}

impl std::error::Error for SandboxError {}

/// v0.42.0: Capability Store — token_id → CapabilityToken 映射
///
/// 设计: 单线程同步, 用 Arc<Mutex> 共享 (与 EventBus 一致).
/// `next_id` 单调递增.
/// v0.49.0 (A1+B1): revoke bumps `current_generation` (not token's); `check` requires
/// `token.generation == current_generation` (else TokenNotFound). Previously `check`
/// ignored generation entirely — revoke was a no-op for security checks.
#[derive(Debug, Default, Clone)]
pub struct CapabilityStore {
    tokens: std::sync::Arc<std::sync::Mutex<CapabilityStoreInner>>,
}

#[derive(Debug, Default)]
struct CapabilityStoreInner {
    by_id: BTreeMap<u64, CapabilityToken>,
    next_id: u64,
    /// v0.49.0: current global generation; bumped by `revoke()`.
    /// Tokens with `generation != current_generation` are treated as not-found.
    current_generation: u32,
}

impl CapabilityStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// v0.49.0 (A1): 当前 global generation (public for test assertions)
    pub fn current_generation(&self) -> u32 {
        self.tokens
            .lock()
            .expect("capability store mutex poisoned")
            .current_generation
    }

    /// 发放新 token
    pub fn issue(
        &self,
        allowed: BTreeSet<Capability>,
        ttl: Option<Duration>,
    ) -> Result<u64, SandboxError> {
        let mut inner = self.tokens.lock().expect("capability store mutex poisoned");
        let token_id = inner.next_id;
        inner.next_id += 1;
        let now = SystemTime::now();
        let expires_at = ttl.map(|d| now + d);
        let token = CapabilityToken {
            token_id,
            allowed,
            denied: BTreeSet::new(),
            expires_at,
            generation: 0,
            created_at: now,
        };
        inner.by_id.insert(token_id, token);
        Ok(token_id)
    }

    /// v0.49.0 (A5): 单锁内 get + check (避免双锁)
    /// 查询 token (返回 clone 用于无锁使用)
    pub fn get(&self, token_id: u64) -> Option<CapabilityToken> {
        let inner = self.tokens.lock().expect("capability store mutex poisoned");
        inner.by_id.get(&token_id).cloned()
    }

    /// v0.49.0 (A5 + A1): 检查 capability (返回 Ok(()) 或 Err(SandboxError))
    /// 单锁内 get + check; 同时校验 generation (A1) — revoked token 返回 TokenNotFound.
    pub fn check(&self, token_id: u64, capability: Capability) -> Result<(), SandboxError> {
        let inner = self.tokens.lock().expect("capability store mutex poisoned");
        let token = inner
            .by_id
            .get(&token_id)
            .ok_or(SandboxError::TokenNotFound { token_id })?;
        // v0.49.0 (A1): revoked token 视为不存在
        if token.generation != inner.current_generation {
            return Err(SandboxError::TokenNotFound { token_id });
        }
        let now = SystemTime::now();
        if token.permits(capability, now) {
            Ok(())
        } else if !token.is_alive(now) {
            Err(SandboxError::TokenExpired { token_id })
        } else {
            Err(SandboxError::CapViolation {
                token_id,
                capability,
            })
        }
    }

    /// v0.49.0 (B1): 撤销 token — bump GLOBAL `current_generation`.
    /// 旧持有者的 token 仍携带旧 generation, `check` 会视为 TokenNotFound.
    /// 这样 revoke 在并发场景下立即生效 (无需遍历所有 token).
    pub fn revoke(&self, _token_id: u64) -> Result<(), SandboxError> {
        // 检查 token 存在 (保持 API 兼容, 返回 TokenNotFound if not)
        let mut inner = self.tokens.lock().expect("capability store mutex poisoned");
        if !inner.by_id.contains_key(&_token_id) {
            return Err(SandboxError::TokenNotFound {
                token_id: _token_id,
            });
        }
        inner.current_generation = inner.current_generation.wrapping_add(1);
        Ok(())
    }

    /// 当前 token 数 (test helper)
    pub fn token_count(&self) -> usize {
        self.tokens
            .lock()
            .expect("capability store mutex poisoned")
            .by_id
            .len()
    }

    /// 下次发放的 token_id (test helper)
    pub fn next_id(&self) -> u64 {
        self.tokens
            .lock()
            .expect("capability store mutex poisoned")
            .next_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_parse_roundtrip() {
        for cap in Capability::all() {
            assert_eq!(Capability::parse(cap.as_str()), Some(*cap));
        }
    }

    #[test]
    fn capability_parse_unknown_returns_none() {
        assert_eq!(Capability::parse("not.a.cap"), None);
        assert_eq!(Capability::parse(""), None);
        assert_eq!(Capability::parse("file.delete"), None); // 不存在的
    }

    #[test]
    fn token_with_single_capability_authorizes_correctly() {
        let store = CapabilityStore::new();
        let mut allowed = BTreeSet::new();
        allowed.insert(Capability::FileRead);
        let id = store.issue(allowed, None).unwrap();
        assert_eq!(id, 0);

        assert!(store.check(id, Capability::FileRead).is_ok());
        assert!(store.check(id, Capability::FileWrite).is_err());
        assert!(store.check(id, Capability::WebFetch).is_err());
    }

    #[test]
    fn token_with_multiple_capabilities_authorizes_all() {
        let store = CapabilityStore::new();
        let allowed: BTreeSet<_> = [
            Capability::FileRead,
            Capability::WebFetch,
            Capability::MemoryRead,
        ]
        .into_iter()
        .collect();
        let id = store.issue(allowed, None).unwrap();

        for cap in [
            Capability::FileRead,
            Capability::WebFetch,
            Capability::MemoryRead,
        ] {
            assert!(store.check(id, cap).is_ok(), "should allow {:?}", cap);
        }
        // 不在 allowed 中的应拒绝
        assert!(store.check(id, Capability::FileWrite).is_err());
    }

    #[test]
    fn expired_token_denies_even_if_capability_granted() {
        let store = CapabilityStore::new();
        let mut allowed = BTreeSet::new();
        allowed.insert(Capability::FileRead);
        // 用负 TTL 让 token 立即过期 (用 None + 手动构造 expired token 不可能,
        // 因为 issue 总是 now+ttl; 改用 ttl=0 让 now == expires_at, 此时 is_alive 严格 < 比较)
        let id = store.issue(allowed, Some(Duration::from_secs(0))).unwrap();
        // sleep 1ms 确保 now > expires_at
        std::thread::sleep(Duration::from_millis(10));
        let err = store.check(id, Capability::FileRead).unwrap_err();
        assert!(matches!(err, SandboxError::TokenExpired { .. }));
    }

    #[test]
    fn deny_overrides_allow() {
        let store = CapabilityStore::new();
        let mut allowed = BTreeSet::new();
        allowed.insert(Capability::FileRead);
        let id = store.issue(allowed, None).unwrap();

        // 在 token 内部 deny 该 cap (store 不暴露 deny 设置, 直接构造测试 token)
        let token = store.get(id).unwrap();
        let mut denied_token = token;
        denied_token.denied.insert(Capability::FileRead);

        // 直接验证 permits() 逻辑
        assert!(!denied_token.permits(Capability::FileRead, SystemTime::now()));
        // 其它 cap 不受影响
        let mut allowed2 = BTreeSet::new();
        allowed2.insert(Capability::WebFetch);
        denied_token.allowed = allowed2;
        denied_token.denied.clear();
        assert!(denied_token.permits(Capability::WebFetch, SystemTime::now()));
    }

    #[test]
    fn revoke_invalidates_token_immediately() {
        let store = CapabilityStore::new();
        let mut allowed = BTreeSet::new();
        allowed.insert(Capability::FileRead);
        let id = store.issue(allowed, None).unwrap();

        // revoke 前允许
        assert!(store.check(id, Capability::FileRead).is_ok());

        store.revoke(id).unwrap();

        // v0.49.0 (A1+B1): revoke 真的让 token 失效 (return TokenNotFound)
        let err = store.check(id, Capability::FileRead).unwrap_err();
        assert!(
            matches!(err, SandboxError::TokenNotFound { .. }),
            "revoked token should return TokenNotFound, got: {:?}",
            err
        );

        // current_generation bumped from 0 to 1
        assert_eq!(store.current_generation(), 1);

        // token 仍存在 (loongclaw 风格: revoke 不删 token, 但失配)
        assert!(store.get(id).is_some());
    }

    #[test]
    fn unknown_capability_string_errors() {
        let store = CapabilityStore::new();
        let allowed: BTreeSet<_> = [Capability::FileRead].into_iter().collect();
        let _id = store.issue(allowed, None).unwrap();

        // check 时只能用合法 Capability (编译期保证)
        // parse 阶段用 parse() 测试
        assert_eq!(Capability::parse("file.read"), Some(Capability::FileRead));
        assert_eq!(Capability::parse("unknown"), None);
    }

    #[test]
    fn issue_returns_monotonic_ids() {
        let store = CapabilityStore::new();
        let id1 = store.issue(BTreeSet::new(), None).expect("first issue");
        let id2 = store.issue(BTreeSet::new(), None).expect("second issue");
        let id3 = store.issue(BTreeSet::new(), None).expect("third issue");
        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(id3, 2);
        assert_eq!(store.token_count(), 3);
    }

    #[test]
    fn revoke_unknown_token_returns_error() {
        let store = CapabilityStore::new();
        let err = store.revoke(999).unwrap_err();
        assert!(matches!(err, SandboxError::TokenNotFound { token_id: 999 }));
    }

    #[test]
    fn check_unknown_token_returns_error() {
        let store = CapabilityStore::new();
        let err = store.check(42, Capability::FileRead).unwrap_err();
        assert!(matches!(err, SandboxError::TokenNotFound { token_id: 42 }));
    }
}
