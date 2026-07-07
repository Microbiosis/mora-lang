//! v0.75.x: Runtime 层共享数据类型
//!
//! v0.52 ADR-001 拆出 6 Domain Facade 时，这些类型仍留在
//! `interpreter/mod.rs`，导致 `runtime/*` 反向 `use crate::interpreter::*`
//! 的隐式耦合。本模块把它们下沉到拥有者一侧（runtime facade），
//! 使 `runtime/` 成为自包含的 Kernel Services 层。
//!
//! 类型可见性：字段用 `pub(crate)`（而非 `pub`）——仅在 crate 内部消费，
//! 不构成公共 API。`interpreter/` 及其子模块经 `interpreter/mod.rs` 的
//! re-export 继续引用，路径不变。

use crate::value::Value;

/// v0.49.0 (C1+C2): 简单 LRU cache (insertion-order, no hash map to keep 0 deps).
/// `cap` 上限, 超过 evict 最旧. `Arc<Mutex<>>` for thread-safe shared access.
pub struct LruCache<V> {
    cap: usize,
    /// ordered entries (oldest first) for O(1) pop_front on evict
    order: std::collections::VecDeque<String>,
    map: std::collections::HashMap<String, V>,
}

impl<V: Clone> LruCache<V> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            order: std::collections::VecDeque::new(),
            map: std::collections::HashMap::new(),
        }
    }

    /// 真正的 LRU get：命中后把 key 移到 order 末尾（最新）
    pub fn get(&mut self, key: &str) -> Option<V> {
        if self.map.contains_key(key) {
            // 刷新访问顺序
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                self.order.remove(pos);
                self.order.push_back(key.to_string());
            }
            self.map.get(key).cloned()
        } else {
            None
        }
    }

    /// 插入或更新. 超 cap 时 evict 最旧.
    pub fn put(&mut self, key: String, value: V) {
        if self.map.contains_key(&key) {
            // 更新 — 刷新 order 为最新
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
            self.order.push_back(key.clone());
            self.map.insert(key, value);
            return;
        }
        if self.map.len() >= self.cap {
            // evict oldest
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn cap(&self) -> usize {
        self.cap
    }
}

/// v0.06: with 块字段 (不经过 env 变量)
/// v0.52 ADR-001: pub 让 CoreRuntime (runtime/core.rs) 可引用
/// v0.75.x: 下沉到 runtime/types.rs，字段 pub(crate) 供 interpreter 子模块构造/读取
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct AiConfigValue {
    pub(crate) model: Option<String>,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<usize>,
    pub(crate) budget: Option<usize>,
    pub(crate) per_call: Option<usize>,
    pub(crate) system: Option<String>,
    /// v0.15: mock 响应队列 (with mock_llm = ["resp1", "resp2"])
    pub(crate) mock_responses: Option<Vec<String>>,
    /// v0.24: 投机执行配置
    pub(crate) speculative: Option<bool>,
    pub(crate) draft_model: Option<String>,
    /// v0.54: tool 绑定 — with tools: ["read_file", "run_cmd"]
    pub(crate) tool_names: Option<Vec<String>>,
}

/// Token 预算配置
// v0.52 ADR-001: pub 让 src/runtime/ai.rs 可访问（抽 AiRuntime 用）
#[derive(Debug, Clone)]
pub struct TokenBudget {
    pub(crate) total: usize,
    /// 每次调用 token 上限（v0.15 接入 track_tokens）
    pub(crate) per_call: Option<usize>,
    pub(crate) alert_threshold: f64, // 0.0-1.0，超过此比例时告警
}

/// Token 消耗统计
// v0.52 ADR-001: pub struct + pub fields 让 src/runtime/ai.rs 可访问
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input: usize,
    pub output: usize,
}

/// 模型路由配置
// v0.52 ADR-001: pub 让 src/runtime/ai.rs 可访问
#[derive(Debug, Clone)]
pub struct RouteConfig {
    pub(crate) model: String,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    /// 单次请求 max_tokens（v0.15 接入 real_ai_chat_with_tools）
    pub(crate) max_tokens: Option<usize>,
    /// 系统提示词覆盖（v0.15 接入 real_ai_chat_with_tools）
    pub(crate) system: Option<String>,
    /// 温度覆盖（v0.15 接入 real_ai_chat_with_tools）
    pub(crate) temperature: Option<f64>,
    /// v0.24: 路由优先级 (越小越优先)
    #[allow(dead_code)] // 未来扩展用
    pub(crate) priority: u32,
    /// v0.24: 路由健康状态
    #[allow(dead_code)] // 未来扩展用
    pub(crate) healthy: bool,
}

/// 工具定义（注册时存储）
#[derive(Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: String, // JSON Schema 字符串
    pub handler: Value,     // Closure
}

/// v0.08: trait 注册条目
/// v0.08.4: 加 parents 字段实现 trait 继承
#[derive(Clone, Debug)]
pub struct TraitInfo {
    pub name: String,
    pub parents: Vec<String>,
    pub methods: Vec<TraitMethodSig>,
}

/// v0.08: trait 方法签名
/// v0.08.5 任务 1: 加 has_self 字段——trait method 第一个参数是 self 时为 true，
/// 否则为 false（self-less 方法）。self-less 调度时不传 receiver。
#[derive(Clone, Debug)]
pub struct TraitMethodSig {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    /// 第一个参数是否为 `self`（决定 dispatch 时是否传 receiver.clone()）
    pub has_self: bool,
}

/// v0.09: 注册 impl method 时用的 key（含泛型签名）
/// 格式: __impl_<Trait>_<TraitGen>_<ForType>_<ForGen>_<method>
///   TraitGen / ForGen 用类型名（如 "Number" / "String"），简化版（v0.09 不含 typeck 类型）
///
/// 重要: 同一 trait 不同实例化产生不同 key，避免冲突
///   Container<number> vs Container<string> → 不同 key
pub(crate) fn impl_method_key(
    trait_name: &str,
    trait_generics: &[String], // v0.09 新增：trait 实例化的泛型
    for_type: &str,
    for_generics: &[String], // v0.09 新增：for_type 的泛型
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    let fg = for_generics.join(",");
    format!(
        "__impl_{}_{}_{}_{}_{}",
        trait_name, tg, for_type, fg, method
    )
}

/// v0.09: 默认实现的 key（self 类型 = trait 名）
/// 格式: __impl_<Trait>_<TraitGen>_<method>
pub(crate) fn default_impl_method_key(
    trait_name: &str,
    trait_generics: &[String], // v0.09 新增
    method: &str,
) -> String {
    let tg = trait_generics.join(",");
    format!("__impl_{}_{}_{}", trait_name, tg, method)
}
