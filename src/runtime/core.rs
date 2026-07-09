//! v0.52 ADR-001: CoreRuntime — 语言执行必需的薄核心
//!
//! 从 Interpreter god object 抽出的 8 个核心执行字段（globals/environment/tool_registry/
//! v2_arena/current_ai_config/config_stack/worker_channels/worker_receivers），
//! 是解释器运行所必需的最小状态容器。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use crate::ast_v2::AstArena;
use crate::interpreter::{AiConfigValue, ToolDef};
use crate::value::{Environment, Value};

/// 语言执行必需的薄核心（8 字段）。
/// 注：ToolDef 不含 Debug，所以 CoreRuntime 不 derive Debug。
#[derive(Clone)]
pub struct CoreRuntime {
    /// 全局变量环境
    pub globals: Arc<Mutex<Environment>>,
    /// 当前执行环境（可嵌套）
    pub environment: Arc<Mutex<Environment>>,
    /// 工具注册表（MCP / builtin tool 的运行时注册）
    pub tool_registry: Arc<HashMap<String, ToolDef>>,
    /// v2 AST arena — 在 interpret 期间存储，供 call_value 执行 v2 闭包
    pub v2_arena: Option<Arc<AstArena>>,
    /// 当前 with 块 set 的 AiConfig 值
    pub current_ai_config: Option<AiConfigValue>,
    /// with 块 config 保存/恢复栈（MIR 解释器用）
    pub config_stack: Vec<Option<AiConfigValue>>,
    /// Worker 并发 channels（sender 端）
    pub worker_channels: HashMap<String, crossbeam_channel::Sender<Value>>,
    /// Worker 并发 channels（receiver 端）
    pub worker_receivers: HashMap<String, crossbeam_channel::Receiver<Value>>,
}

impl Default for CoreRuntime {
    fn default() -> Self {
        let env = Arc::new(Mutex::new(Environment::default()));
        Self {
            globals: env.clone(),
            environment: env,
            tool_registry: Arc::new(HashMap::new()),
            v2_arena: None,
            current_ai_config: None,
            config_stack: Vec::new(),
            worker_channels: HashMap::new(),
            worker_receivers: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_default_globals_and_env_share() {
        let core = CoreRuntime::default();
        // globals 和 environment 初始指向同一个 Arc
        assert!(Arc::ptr_eq(&core.globals, &core.environment));
    }

    #[test]
    fn core_tool_registry_empty() {
        let core = CoreRuntime::default();
        assert!(core.tool_registry.is_empty());
    }

    #[test]
    fn core_v2_arena_default_none() {
        let core = CoreRuntime::default();
        assert!(core.v2_arena.is_none());
    }

    #[test]
    fn core_ai_config_default_none() {
        let core = CoreRuntime::default();
        assert!(core.current_ai_config.is_none());
    }

    #[test]
    fn core_config_stack_default_empty() {
        let core = CoreRuntime::default();
        assert!(core.config_stack.is_empty());
    }

    #[test]
    fn core_worker_channels_empty() {
        let core = CoreRuntime::default();
        assert!(core.worker_channels.is_empty());
        assert!(core.worker_receivers.is_empty());
    }

    #[test]
    fn core_clone_preserves_globals_identity() {
        let core = CoreRuntime::default();
        {
            let mut env = core.environment.lock();
            env.define("test".to_string(), Value::Int(42), false);
        }
        let cloned = core.clone();
        let val = cloned.environment.lock().get("test").clone();
        assert!(matches!(val, Some(Value::Int(42))));
    }
}
