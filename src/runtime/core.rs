//! v0.112.0: `CoreRuntime` — 彻底拆离为只读共享底座 [`EngineShared`]、
//! 写时复制类型元数据注册表 [`TypeMetadataRegistry`] 与轻量纤程会话 [`ExecSession`]。
//!
//! 架构分层：
//! 1. [`EngineShared`]（`Arc` 共享）：进程级不可变运行时底座（`TaskScheduler` /
//!    `AsyncIoReactor` / `DagPlanCache`）。
//! 2. [`TypeMetadataRegistry`]（单一 `Arc` + CoW）：编译期/模块加载期产生的
//!    trait / struct / enum 描述符、方法分派表与 `mora build` 预编译模块缓存；
//!    `spawn` 派发纤程仅做一次 `Arc::clone`（不再逐项克隆 8 个独立 `Arc`）。
//! 3. [`ExecSession`]（轻量按纤程独立持有）：每条纤程的可变执行会话（`gensym_counter` /
//!    `random_state` / `task_context` / `fs_context` / `autograd`），通过
//!    [`CoreRuntime::fork_for_fiber`] O(1) 隔离出新纤程并重置独立 `AutogradContext`。
//! 4. `environment: Environment`：显式收窄为动态 `eval()` / REPL / 模块导出的符号边界。

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
use crate::value::{EnumDefRuntime, Environment, StructDefRuntime, Value};

/// Shared immutable process-level services across tasks and spawned VM workers.
pub struct EngineShared {
    /// Shared zero-dependency native multi-threaded task scheduler.
    pub(crate) scheduler: Arc<TaskScheduler>,
    /// Shared zero-dependency native non-blocking async I/O reactor.
    pub(crate) reactor: Arc<AsyncIoReactor>,
    /// Thread-safe structural compilation cache for [`crate::runtime::compute_dag::TensorComputeDag`].
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

    /// Construct a shared runtime backing bound to an explicit [`TaskScheduler`] (e.g. M:N worker-bound pool).
    pub(crate) fn with_scheduler(scheduler: Arc<TaskScheduler>) -> Self {
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
    pub(crate) fn dag_cache(&self) -> &Arc<DagPlanCache> {
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

/// Single copy-on-write immutable metadata bundle shared across spawned fibers via one `Arc`.
#[derive(Clone, Default)]
pub struct TypeMetadataRegistry {
    /// Trait definitions (`trait_name` -> `TraitInfo`).
    pub(crate) trait_registry: HashMap<String, TraitInfo>,
    /// Nominal struct runtime descriptors (`StructName -> StructDefRuntime`).
    pub(crate) struct_registry: HashMap<String, Arc<StructDefRuntime>>,
    /// Nominal enum runtime descriptors (`EnumName -> EnumDefRuntime`).
    pub(crate) enum_registry: HashMap<String, Arc<EnumDefRuntime>>,
    /// Inherent `impl TargetType` methods `(target_type, method_name) -> (Value, has_self)`.
    pub(crate) inherent_methods: InherentMethodMap,
    /// Trait `impl Trait for TargetType` methods `(trait_name, target_type, method_name) -> (Value, has_self)`.
    pub(crate) trait_impls: TraitImplMethodMap,
    /// Registered `(trait_name, target_type)` impl pairs (supports empty default-only impls).
    pub(crate) trait_impl_pairs: HashSet<(String, String)>,
    /// Trait default method implementations `(trait_name, method_name) -> (Value, has_self)`.
    pub(crate) trait_defaults: HashMap<(String, String), (Value, bool)>,
    /// Precompiled & verified SSA `ControlProgram` modules embedded by `mora build` (`MORABC02`).
    pub(crate) precompiled_modules: HashMap<String, Arc<crate::mir::control::ControlProgram>>,
}

/// Lightweight per-fiber mutable execution session cleanly separated from [`EngineShared`]
/// and [`TypeMetadataRegistry`].
#[derive(Clone)]
pub struct ExecSession {
    /// Per-fiber unique symbol counter (`gensym()`).
    pub(crate) gensym_counter: usize,
    /// Per-fiber ambient random state (pure value).
    pub(crate) random_state: crate::runtime::random::Xoshiro256,
    /// Optional cooperative cancellation and deadline context for VM tasks.
    pub(crate) task_context: Option<TaskContext>,
    /// Filesystem confinement policy and isolated virtual working directory.
    pub(crate) fs_context: FsContext,
    /// Per-fiber reverse-mode automatic differentiation tape stack (`grad` / `no_grad`).
    pub(crate) autograd: AutogradContext,
}

impl Default for ExecSession {
    fn default() -> Self {
        Self {
            gensym_counter: 0,
            random_state: crate::runtime::random::Xoshiro256::from_time(),
            task_context: None,
            fs_context: FsContext::unrestricted(),
            autograd: AutogradContext::new(),
        }
    }
}

impl ExecSession {
    /// Fork a lightweight child session for a `spawn`ed fiber: preserves filesystem policy and
    /// task context while starting with a fresh isolated `AutogradContext`.
    pub(crate) fn fork_for_fiber(&self) -> Self {
        Self {
            gensym_counter: self.gensym_counter,
            random_state: self.random_state,
            task_context: self.task_context.clone(),
            fs_context: self.fs_context.clone(),
            autograd: AutogradContext::new(),
        }
    }
}

/// 语言执行必需的薄核心：由不可变 [`EngineShared`] + 单 `Arc` [`TypeMetadataRegistry`] +
/// 轻量每纤程 [`ExecSession`] + 显式动态 `eval`/REPL/导出边界 `environment` 构成。
#[derive(Clone)]
pub struct CoreRuntime {
    /// Immutable process-level shared services (`scheduler`, `reactor`, `dag_cache`).
    pub(crate) shared: Arc<EngineShared>,
    /// Single `Arc` copy-on-write type/method/module metadata bundle.
    pub(crate) registry: Arc<TypeMetadataRegistry>,
    /// Lightweight per-fiber execution session (`gensym`, `random`, `task_context`, `fs_context`, `autograd`).
    pub(crate) session: ExecSession,
    /// Explicit dynamic namespace strictly for `eval()`, REPL sessions, and module export bridges.
    pub(crate) environment: Environment,
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
            shared: Arc::new(EngineShared::new()),
            registry: Arc::new(TypeMetadataRegistry::default()),
            session: ExecSession::default(),
            environment: default_environment(),
        }
    }
}

impl CoreRuntime {
    /// Fork an isolated child runtime for a spawned fiber in O(1) (`EngineShared` + `TypeMetadataRegistry`
    /// shared via two `Arc::clone`s; fresh `AutogradContext` in child [`ExecSession`]).
    pub(crate) fn fork_for_fiber(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            registry: Arc::clone(&self.registry),
            session: self.session.fork_for_fiber(),
            environment: self.environment.clone(),
        }
    }

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

    /// Bind this runtime to a custom [`TaskScheduler`] (e.g. bounded M:N worker thread count).
    pub fn with_scheduler(mut self, scheduler: Arc<TaskScheduler>) -> Self {
        self.shared = Arc::new(EngineShared::with_scheduler(scheduler));
        self
    }

    /// Attach a cooperative cancellation and deadline [`TaskContext`] to this runtime.
    pub fn with_task_context(mut self, ctx: TaskContext) -> Self {
        self.session.task_context = Some(ctx);
        self
    }

    /// Configure a confined filesystem root for `file.*`, `std/os`, `std/io`, and `import`.
    pub fn with_fs_root(mut self, root: impl AsRef<Path>) -> MoraResult<Self> {
        self.session.fs_context = FsContext::confined(root)?;
        Ok(self)
    }

    /// Borrow this runtime's [`FsContext`].
    pub fn fs_context(&self) -> &FsContext {
        &self.session.fs_context
    }

    /// Mutably borrow this runtime's [`FsContext`] (e.g. isolated `file.chdir`).
    pub fn fs_context_mut(&mut self) -> &mut FsContext {
        &mut self.session.fs_context
    }

    /// Attach precompiled & verified SSA `ControlProgram` modules (used by `MORABC02` bundles/`.morac`).
    pub fn with_precompiled_modules(
        mut self,
        modules: HashMap<String, Arc<crate::mir::control::ControlProgram>>,
    ) -> Self {
        Arc::make_mut(&mut self.registry).precompiled_modules = modules;
        self
    }

    /// Check cooperative cancellation or deadline expiry if a [`TaskContext`] is attached.
    pub fn check_cancelled(&self) -> MoraResult<()> {
        match &self.session.task_context {
            Some(ctx) => ctx.status(),
            None => Ok(()),
        }
    }

    /// 注册 trait。
    pub fn register_trait(&mut self, name: String, info: TraitInfo) {
        Arc::make_mut(&mut self.registry)
            .trait_registry
            .insert(name, info);
    }

    /// Register a nominal struct runtime descriptor.
    pub fn register_struct_def(&mut self, def: Arc<StructDefRuntime>) {
        Arc::make_mut(&mut self.registry)
            .struct_registry
            .insert(def.name.clone(), def);
    }

    /// Register a nominal enum runtime descriptor.
    pub fn register_enum_def(&mut self, def: Arc<EnumDefRuntime>) {
        Arc::make_mut(&mut self.registry)
            .enum_registry
            .insert(def.name.clone(), def);
    }

    /// Register an inherent `impl TargetType` method.
    pub fn register_inherent_method(
        &mut self,
        target_type: String,
        method: String,
        func: Value,
        has_self: bool,
    ) {
        Arc::make_mut(&mut self.registry)
            .inherent_methods
            .insert((target_type, method), (func, has_self));
    }

    /// Register a `(trait_name, target_type)` impl pair (supports empty default-only impls).
    pub fn register_trait_impl_pair(&mut self, trait_name: String, target_type: String) {
        Arc::make_mut(&mut self.registry)
            .trait_impl_pairs
            .insert((trait_name, target_type));
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
        let reg = Arc::make_mut(&mut self.registry);
        reg.trait_impl_pairs
            .insert((trait_name.clone(), target_type.clone()));
        reg.trait_impls
            .insert((trait_name, target_type, method), (func, has_self));
    }

    /// Register a trait default method body.
    pub fn register_trait_default_method(
        &mut self,
        trait_name: String,
        method: String,
        func: Value,
        has_self: bool,
    ) {
        Arc::make_mut(&mut self.registry)
            .trait_defaults
            .insert((trait_name, method), (func, has_self));
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
        assert!(core.registry.trait_registry.is_empty());
    }

    #[test]
    fn register_trait_inserts() {
        let mut core = CoreRuntime::default();
        core.register_trait("T1".to_string(), make_trait_info());
        assert!(core.registry.trait_registry.contains_key("T1"));
    }

    #[test]
    fn fork_for_fiber_shares_engine_and_registry_with_isolated_session() {
        let mut core1 = CoreRuntime::default();
        core1.register_trait("T1".to_string(), make_trait_info());
        core1.session.gensym_counter = 5;
        let core2 = core1.fork_for_fiber();
        assert!(Arc::ptr_eq(&core1.shared, &core2.shared));
        assert!(Arc::ptr_eq(&core1.registry, &core2.registry));
        assert!(core2.registry.trait_registry.contains_key("T1"));
        assert_eq!(core2.session.gensym_counter, 5);
    }

    #[test]
    fn core_clone_preserves_environment_bindings() {
        let mut core = CoreRuntime::default();
        core.environment
            .define("test".to_string(), Value::Int(42), false);
        core.session.gensym_counter = 5;
        let cloned = core.clone();
        assert!(matches!(
            cloned.environment.get("test"),
            Some(Value::Int(42))
        ));
        assert_eq!(cloned.session.gensym_counter, 5);
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
