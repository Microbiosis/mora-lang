//! v0.110.0: Canonical Heap-Framed Stackful Virtual Machine (`src/runtime/vm/`).
//!
//! The single execution kernel of the language: one explicit `Vec<CallFrame>` engine running
//! [`ControlProgram`] SSA IR, reached only through the single production path
//! `ParserV3::compile → MirWitness[] → mir::control::lower_program → ControlProgram →
//! typeck::check_mir::check_program_witnesses → runtime::vm::execute_program`.
//! - Zero C-stack recursion across nested Mora calls, combinators (`map`/`filter`/`reduce`),
//!   `eval`, `macroexpand`, and `import`
//! - True O(1)-frame tail call optimization (`ControlTerminator::TailCall`)
//! - Native deep multi-shot delimited continuations (`ControlTerminator::Perform` + `HandleBarrier`)
//! - Exactly one compilation tier and one execution kernel: there is no AST, interpreter, or
//!   JIT tier left to fall back to.

pub mod builtins;
pub mod continuation;
pub mod engine;
pub mod value_ops;

use std::sync::Arc;

use crate::error::MoraResult;
use crate::mir::control::{
    ControlProgram, GlobalKind, OptLevel, lower_program_with_dyn_globals, optimize_program,
};
use crate::mir::witness::MirWitness;
use crate::runtime::CoreRuntime;
use crate::value::Value;

pub use builtins::{
    CallAction, resolve_dyn_call, resolve_dyn_call_named, resolve_method_call,
    resolve_method_call_named, resolve_value_call, resolve_value_call_named,
};
pub use continuation::{
    CallFrame, CapturedContinuation, ModuleGlobals, UpvalueValue, init_globals, materialize_frame,
};
pub use engine::{VmEngine, collect_env_names};
pub use value_ops::{index_assign_value, index_value};

/// Execute a compiled [`ControlProgram`] entry function (`<main>`) and return `(result, globals)`.
pub fn execute_program(
    prog: Arc<ControlProgram>,
    core: &mut CoreRuntime,
) -> MoraResult<(Value, ModuleGlobals)> {
    crate::mir::control::verify_program(&prog)?;
    let globals = init_globals(&prog);
    let entry = prog.entry;
    let frame = materialize_frame(prog, entry, Arc::clone(&globals), Arc::new(Vec::new()), &[])?;
    let mut vm = VmEngine::new(core);
    let val = vm.run_frame(frame)?;
    Ok((val, globals))
}

/// Find and execute a zero-argument `task main()` inside `prog` sharing the module's `globals`.
pub fn run_main_task(
    prog: Arc<ControlProgram>,
    globals: ModuleGlobals,
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    let main_fn_id = prog.globals.iter().find_map(|meta| match meta.kind {
        GlobalKind::Task(fn_id) if meta.name == "main" && prog.func(fn_id).params.is_empty() => {
            Some(fn_id)
        }
        _ => None,
    });
    let Some(fn_id) = main_fn_id else {
        return Ok(Value::Nil);
    };
    let frame = materialize_frame(prog, fn_id, globals, Arc::new(Vec::new()), &[])?;
    let mut vm = VmEngine::new(core);
    vm.run_frame(frame)
}

/// Invoke an arbitrary first-class callable [`Value`] on `core`'s stackful VM.
pub fn execute_value(callee: Value, args: Vec<Value>, core: &mut CoreRuntime) -> MoraResult<Value> {
    execute_value_named(callee, args, Vec::new(), core)
}

/// Invoke an arbitrary first-class callable [`Value`] with named keyword arguments.
pub fn execute_value_named(
    callee: Value,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    let action = resolve_value_call_named(callee, args, named_args, core)?;
    let mut vm = VmEngine::new(core);
    vm.execute_root_action(action)
}

/// Invoke `receiver.method(args...)` on `core`'s stackful VM.
pub fn execute_method(
    receiver: Value,
    method: &str,
    args: Vec<Value>,
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    execute_method_named(receiver, method, args, Vec::new(), core)
}

/// Invoke `receiver.method(args..., kw=val...)` on `core`'s stackful VM.
pub fn execute_method_named(
    receiver: Value,
    method: &str,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    let action = resolve_method_call_named(
        core,
        receiver,
        method,
        args,
        named_args,
        crate::common::Span::default(),
    )?;
    let mut vm = VmEngine::new(core);
    vm.execute_root_action(action)
}

/// Invoke a named function/task/builtin/macro in `core`'s dynamic environment.
pub fn execute_named(name: &str, args: Vec<Value>, core: &mut CoreRuntime) -> MoraResult<Value> {
    let action = resolve_dyn_call(name, args, core, None)?;
    let mut vm = VmEngine::new(core);
    vm.execute_root_action(action)
}

/// Compile and execute an incremental REPL witness chunk while persisting declared bindings
/// into `core.environment` for subsequent REPL turns.
pub fn execute_repl_chunk(
    witnesses: &[MirWitness],
    opt_level: OptLevel,
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    let dyn_names = collect_env_names(&core.environment);
    let mut prog = lower_program_with_dyn_globals(witnesses, &dyn_names)?;
    optimize_program(&mut prog, opt_level);
    let prog_arc = Arc::new(prog);
    let (val, globals) = execute_program(Arc::clone(&prog_arc), core)?;
    for (idx, meta) in prog_arc.globals.iter().enumerate() {
        let guard = globals[idx].lock();
        core.environment
            .define(meta.name.clone(), guard.0.clone(), guard.1);
    }
    Ok(val)
}
