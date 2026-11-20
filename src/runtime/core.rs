//! v0.52 ADR-001: CoreRuntime — 语言执行必需的薄核心
//!
//! v0.110.0 瘦身：彻底剥离 `tool_registry` / `current_ai_config` / `config_stack`
//! （Agent/AI 配置残留），并将原 `RegistryRuntime` 中存活的 `trait_registry` 并入 CoreRuntime。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::common::trait_info::TraitInfo;
use crate::error::MoraResult;
use crate::runtime::autograd::AutogradContext;
use crate::runtime::compute_dag::DagPlanCache;
use crate::runtime::fs::FsContext;
use crate::runtime::reactor::AsyncIoReactor;
use crate::runtime::scheduler::{TaskContext, TaskScheduler};
use crate::value::{EnumDefRuntime, Environment, MergeStrategy, StructDefRuntime, Value};

/// Shared immutable process-level services across tasks and spawned VM workers.
pub struct EngineShared {
    /// Wave 2 W2.4: Shared tokio multi-threaded task scheduler.
    pub(crate) scheduler: Arc<TaskScheduler>,
    /// Wave 2 W2.4: Shared tokio async I/O reactor.
    pub(crate) reactor: Arc<AsyncIoReactor>,
    /// Wave 4 P1: Thread-safe structural compilation cache for [`crate::runtime::compute_dag::TensorComputeDag`].
    pub(crate) dag_cache: Arc<DagPlanCache>,
}

impl EngineShared {
    /// Construct a shared runtime backing (`scheduler` + `reactor` + `dag_cache`).
    pub fn new() -> Self {
        let scheduler = TaskScheduler::shared();
        let reactor = Arc::new(AsyncIoReactor::new(Arc::clone(&scheduler)));
        let dag_cache = Arc::new(DagPlanCache::default());
        Self {
            scheduler,
            reactor,
            dag_cache,
        }
    }

    /// Borrow the shared multi-threaded [`TaskScheduler`].
    pub fn scheduler(&self) -> &Arc<TaskScheduler> {
        &self.scheduler
    }

    /// Borrow the shared [`AsyncIoReactor`].
    pub fn reactor(&self) -> &Arc<AsyncIoReactor> {
        &self.reactor
    }

    /// Borrow the shared [`DagPlanCache`].
    pub fn dag_cache(&self) -> &Arc<DagPlanCache> {
        &self.dag_cache
    }
}

impl Default for EngineShared {
    fn default() -> Self {
        Self::new()
    }
}

type TraitImplMethodMap = HashMap<(String, String, String), (Value, bool)>;
type InherentMethodMap = HashMap<(String, String), (Value, bool)>;

/// 语言执行必需的薄核心。
#[derive(Clone)]
pub struct CoreRuntime {
    /// 当前执行环境（v0.75.76 起仅在 run 前由 `take_env` 取出，此后按值穿线）
    pub(crate) environment: Environment,
    /// v0.08: trait 系统注册表（原 RegistryRuntime 存活字段迁入）
    pub(crate) trait_registry: Arc<HashMap<String, TraitInfo>>,
    /// v0.67: 当前 transaction/worker 的合并策略。
    pub(crate) current_merge_strategies: Option<HashMap<String, MergeStrategy>>,
    /// v0.87: gensym 计数器。每次 gensym() 调用递增，保证符号名唯一。
    pub(crate) gensym_counter: usize,
    /// v0.99: ambient random 状态（纯值）。
    pub(crate) random_state: crate::runtime::random::Xoshiro256,
    /// Immutable process-level shared services (`scheduler`, `reactor`).
    pub(crate) shared: Arc<EngineShared>,
    /// Wave 2 W2.4: Optional cooperative cancellation and deadline context for VM tasks.
    pub(crate) task_context: Option<TaskContext>,
    /// Wave 2 W2.5: Filesystem confinement policy and isolated virtual working directory.
    pub(crate) fs_context: FsContext,
    /// Wave 3 Stage 3C: Nominal struct runtime descriptors (`StructName -> StructDefRuntime`).
    pub(crate) struct_registry: Arc<HashMap<String, Arc<StructDefRuntime>>>,
    /// Wave 3 Stage 3C: Nominal enum runtime descriptors (`EnumName -> EnumDefRuntime`).
    pub(crate) enum_registry: Arc<HashMap<String, Arc<EnumDefRuntime>>>,
    /// Wave 3 Stage 3C: Inherent `impl TargetType` methods `(target_type, method_name) -> (Value, has_self)`.
    pub(crate) inherent_methods: Arc<InherentMethodMap>,
    /// Wave 3 Stage 3C: Trait `impl Trait for TargetType` methods `(trait_name, target_type, method_name) -> (Value, has_self)`.
    pub(crate) trait_impls: Arc<TraitImplMethodMap>,
    /// Wave 3 Stage 3C: Registered `(trait_name, target_type)` impl pairs (supports empty default-only impls).
    pub(crate) trait_impl_pairs: Arc<HashSet<(String, String)>>,
    /// Wave 3 Stage 3C: Trait default method implementations `(trait_name, method_name) -> (Value, has_self)`.
    pub(crate) trait_defaults: Arc<HashMap<(String, String), (Value, bool)>>,
    /// Wave 3 Stage 3C: Per-runtime reverse-mode automatic differentiation context (`grad` / `no_grad`).
    pub(crate) autograd: AutogradContext,
    /// Wave 4: Virtual module source table embedded by `mora build` single-binary compilation.
    pub(crate) bundled_modules: Arc<HashMap<String, String>>,
}

fn default_environment() -> Environment {
    use crate::value::BuiltinKind as Bk;
    let mut env = Environment::new();
    env.define(
        "print".to_string(),
        crate::value::Value::Builtin(Bk::Print),
        false,
    );
    env.define(
        "range".to_string(),
        crate::value::Value::Builtin(Bk::Range),
        false,
    );
    env.define(
        "len".to_string(),
        crate::value::Value::Builtin(Bk::Len),
        false,
    );
    env.define(
        "tail".to_string(),
        crate::value::Value::Builtin(Bk::Tail),
        false,
    );
    for (name, kind) in crate::value::MODULE_OBJECTS {
        env.define(
            (*name).to_string(),
            crate::value::Value::Builtin(*kind),
            false,
        );
    }
    env
}

impl Default for CoreRuntime {
    fn default() -> Self {
        Self {
            environment: default_environment(),
            trait_registry: Arc::new(HashMap::new()),
            current_merge_strategies: None,
            gensym_counter: 0,
            random_state: crate::runtime::random::Xoshiro256::from_time(),
            shared: Arc::new(EngineShared::new()),
            task_context: None,
            fs_context: FsContext::unrestricted(),
            struct_registry: Arc::new(HashMap::new()),
            enum_registry: Arc::new(HashMap::new()),
            inherent_methods: Arc::new(HashMap::new()),
            trait_impls: Arc::new(HashMap::new()),
            trait_impl_pairs: Arc::new(HashSet::new()),
            trait_defaults: Arc::new(HashMap::new()),
            autograd: AutogradContext::new(),
            bundled_modules: Arc::new(HashMap::new()),
        }
    }
}

impl CoreRuntime {
    /// 只读访问当前运行时环境（供测试与诊断检查内置绑定）。
    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Borrow the shared process-level runtime services.
    pub fn shared(&self) -> &Arc<EngineShared> {
        &self.shared
    }

    /// Borrow the shared multi-threaded [`TaskScheduler`].
    pub fn scheduler(&self) -> &Arc<TaskScheduler> {
        self.shared.scheduler()
    }

    /// Borrow the shared [`AsyncIoReactor`].
    pub fn reactor(&self) -> &Arc<AsyncIoReactor> {
        self.shared.reactor()
    }

    /// Attach a cooperative cancellation and deadline [`TaskContext`] to this runtime.
    pub fn with_task_context(mut self, ctx: TaskContext) -> Self {
        self.task_context = Some(ctx);
        self
    }

    /// Configure a confined filesystem root for `file.*` operations and `import`.
    pub fn with_fs_root(mut self, root: impl AsRef<Path>) -> MoraResult<Self> {
        self.fs_context = FsContext::confined(root)?;
        Ok(self)
    }

    /// Borrow this runtime's [`FsContext`].
    pub fn fs_context(&self) -> &FsContext {
        &self.fs_context
    }

    /// Mutably borrow this runtime's [`FsContext`] (e.g. isolated `file.chdir`).
    pub fn fs_context_mut(&mut self) -> &mut FsContext {
        &mut self.fs_context
    }

    /// Attach bundled import module sources (used by `mora build` standalone executables).
    pub fn with_bundled_modules(mut self, modules: HashMap<String, String>) -> Self {
        self.bundled_modules = Arc::new(modules);
        self
    }

    /// Check cooperative cancellation or deadline expiry if a [`TaskContext`] is attached.
    pub fn check_cancelled(&self) -> MoraResult<()> {
        match &self.task_context {
            Some(ctx) => ctx.status(),
            None => Ok(()),
        }
    }

    /// 注册 trait（原 RegistryRuntime 存活行为迁入）。
    pub fn register_trait(&mut self, name: String, info: TraitInfo) {
        let mut map = (*self.trait_registry).clone();
        map.insert(name, info);
        self.trait_registry = Arc::new(map);
    }

    /// Register a nominal struct runtime descriptor.
    pub fn register_struct_def(&mut self, def: Arc<StructDefRuntime>) {
        let mut map = (*self.struct_registry).clone();
        map.insert(def.name.clone(), def);
        self.struct_registry = Arc::new(map);
    }

    /// Register a nominal enum runtime descriptor.
    pub fn register_enum_def(&mut self, def: Arc<EnumDefRuntime>) {
        let mut map = (*self.enum_registry).clone();
        map.insert(def.name.clone(), def);
        self.enum_registry = Arc::new(map);
    }

    /// Register an inherent `impl TargetType` method.
    pub fn register_inherent_method(
        &mut self,
        target_type: String,
        method: String,
        func: Value,
        has_self: bool,
    ) {
        let mut map = (*self.inherent_methods).clone();
        map.insert((target_type, method), (func, has_self));
        self.inherent_methods = Arc::new(map);
    }

    /// Register a `(trait_name, target_type)` impl pair (supports empty default-only impls).
    pub fn register_trait_impl_pair(&mut self, trait_name: String, target_type: String) {
        let mut set = (*self.trait_impl_pairs).clone();
        set.insert((trait_name, target_type));
        self.trait_impl_pairs = Arc::new(set);
    }

    /// Register a trait `impl Trait for TargetType` method.
    pub fn register_trait_impl_method(
        &mut self,
        trait_name: String,
        target_type: String,
        method: String,
        func: Value,
        has_self: bool,
    ) {
        self.register_trait_impl_pair(trait_name.clone(), target_type.clone());
        let mut map = (*self.trait_impls).clone();
        map.insert((trait_name, target_type, method), (func, has_self));
        self.trait_impls = Arc::new(map);
    }

    /// Register a trait default method body.
    pub fn register_trait_default_method(
        &mut self,
        trait_name: String,
        method: String,
        func: Value,
        has_self: bool,
    ) {
        let mut map = (*self.trait_defaults).clone();
        map.insert((trait_name, method), (func, has_self));
        self.trait_defaults = Arc::new(map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn make_trait_info() -> TraitInfo {
        TraitInfo {
            name: "T".to_string(),
            type_params: Vec::new(),
            parents: Vec::new(),
            methods: Vec::new(),
        }
    }

    #[test]
    fn default_core_empty_traits() {
        let core = CoreRuntime::default();
        assert!(core.trait_registry.is_empty());
    }

    #[test]
    fn register_trait_inserts() {
        let mut core = CoreRuntime::default();
        core.register_trait("T1".to_string(), make_trait_info());
        assert!(core.trait_registry.contains_key("T1"));
    }

    #[test]
    fn clone_shares_arc_traits() {
        let mut core1 = CoreRuntime::default();
        core1.register_trait("T1".to_string(), make_trait_info());
        let core2 = core1.clone();
        assert!(core2.trait_registry.contains_key("T1"));
    }

    #[test]
    fn core_clone_preserves_environment_bindings() {
        let mut core = CoreRuntime::default();
        core.environment
            .define("test".to_string(), Value::Int(42), false);
        core.gensym_counter = 5;
        let cloned = core.clone();
        // 克隆保留绑定
        assert!(matches!(
            cloned.environment.get("test"),
            Some(Value::Int(42))
        ));
        // gensym 计数器按值复制（v0.95: 纯 usize，无锁）
        assert_eq!(cloned.gensym_counter, 5);
        // 纯值语义：克隆侧写不穿透原侧（无共享可变 cell）
        let mut cloned = cloned;
        cloned
            .environment
            .define("test".to_string(), Value::Int(43), false);
        assert!(matches!(core.environment.get("test"), Some(Value::Int(42))));
    }

    #[test]
    fn core_scheduler_reactor_and_task_context() {
        let core = CoreRuntime::default();
        assert!(core.check_cancelled().is_ok());
        let ctx = TaskContext::root();
        let scoped = core.clone().with_task_context(ctx.clone());
        assert!(Arc::ptr_eq(core.scheduler(), scoped.scheduler()));
        assert!(Arc::ptr_eq(core.reactor().scheduler(), scoped.scheduler()));
        assert!(scoped.check_cancelled().is_ok());
        ctx.cancel("core stopped");
        assert_eq!(
            scoped.check_cancelled().unwrap_err().kind_name(),
            "cancelled"
        );
    }
}
