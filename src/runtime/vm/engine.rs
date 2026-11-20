//! Iterative heap-framed stackful VM executing [`ControlProgram`] with native
//! delimited continuations, zero-recursion TCO, isolated ACID transactions, and
//! deterministic `parallel` worker merges (Wave 2 — Stage W2.3).

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::Span;
use crate::error::{MoraError, MoraResult};
use crate::flow::{eval_binary, is_truthy};
use crate::mir::control::{
    CalleeTarget, ControlConst, ControlInst, ControlSelectArm, ControlSpawnTarget,
    ControlTerminator, FnId, OptLevel, ParallelSegment, PatternTarget, UnaryOp, optimize_program,
};
use crate::mir::effect::ambient;
use crate::parser_v3::ParserV3;
use crate::runtime::CoreRuntime;
use crate::typeck::check_mir::check_program_witnesses;
use crate::value::{BuiltinKind, Environment, MergeStrategy, Value};

use super::builtins::{
    CallAction, resolve_builtin_call, resolve_dyn_call, resolve_method_call, resolve_value_call,
};
use super::continuation::{
    CallFrame, CapturedContinuation, CombinatorFrame, HandleBarrierState, ModuleGlobals,
    ParallelFrame, ScriptFrame, TransactionState, UpvalueCell, UpvalueValue,
    clone_isolated_globals, clone_worker_upvalues, materialize_frame, materialize_handler_frame,
    resolve_captures, restore_globals, snapshot_globals,
};
use super::value_ops::{
    eval_assert, eval_quasiquote, index_assign_value, index_value, make_dyn_trait, make_enum_value,
    make_struct_value, match_pattern,
};

/// Result of running a single instruction inside the active [`ScriptFrame`].
enum StepOutcome {
    /// Continue directly to the next instruction in the current basic block.
    NextInst,
    /// Block terminator or control yield already updated `self.frames`; re-dispatch top frame.
    Yielded,
    /// Transaction requested an explicit rollback or an instruction raised a recoverable failure.
    Err(MoraError),
}

/// Heap-framed stackful virtual machine engine.
pub struct VmEngine<'a> {
    pub core: &'a mut CoreRuntime,
    frames: Vec<CallFrame>,
}

impl<'a> VmEngine<'a> {
    /// Create a VM bound to `core`.
    pub fn new(core: &'a mut CoreRuntime) -> Self {
        Self {
            core,
            frames: Vec::new(),
        }
    }

    /// Push `frame` onto the explicit call stack and drive execution until completion.
    pub fn run_frame(&mut self, frame: ScriptFrame) -> MoraResult<Value> {
        self.frames.push(CallFrame::Script(Box::new(frame)));
        self.run_loop()
    }

    /// Push an arbitrary resolved [`CallAction`] (e.g. `Closure`, `Compose`, `Partial`, `Continuation`).
    pub fn run_action(&mut self, action: CallAction) -> MoraResult<Value> {
        self.apply_action(action)?;
        if self.frames.is_empty() {
            // Immediate synchronous action delivered its result into nothing; shouldn't happen unless Done.
            unreachable!("run_action called with immediate action; handled in apply_root_action")
        }
        self.run_loop()
    }

    /// Execute a root [`CallAction`] that may either complete immediately or push frames.
    pub fn execute_root_action(&mut self, action: CallAction) -> MoraResult<Value> {
        match action {
            CallAction::Done(v) => Ok(v),
            other => {
                self.apply_action(other)?;
                self.run_loop()
            }
        }
    }

    fn run_loop(&mut self) -> MoraResult<Value> {
        while let Some(top) = self.frames.pop() {
            match top {
                CallFrame::Script(mut frame) => {
                    match self.step_script(&mut frame) {
                        ScriptStep::Completed(val) => {
                            if let Some(root_val) = self.deliver_value(val)? {
                                return Ok(root_val);
                            }
                        }
                        ScriptStep::Suspended => {
                            // `step_script` already pushed sub-frames above/around or unwound cleanly.
                        }
                        ScriptStep::Err(msg) => {
                            self.handle_error(msg)?;
                        }
                    }
                }
                CallFrame::HandleBarrier(_) => {
                    // Barriers only receive control when unwinding or returning; should not sit bare on top.
                    unreachable!("HandleBarrier popped directly without return delivery")
                }
                CallFrame::AutoResume(_) => {
                    unreachable!("AutoResume popped directly without return delivery")
                }
                CallFrame::Combinator(_) => {
                    unreachable!("Combinator popped directly without return delivery")
                }
                CallFrame::Transaction(_) => {
                    unreachable!("Transaction popped directly without return delivery")
                }
                CallFrame::ImportFinalize { .. } => {
                    unreachable!("ImportFinalize popped directly without return delivery")
                }
                CallFrame::EvalRestore { .. } => {
                    unreachable!("EvalRestore popped directly without return delivery")
                }
                CallFrame::Parallel(_) => {
                    unreachable!("Parallel popped directly without return delivery")
                }
                CallFrame::ErrorContext { .. } => {
                    unreachable!("ErrorContext popped directly without return delivery")
                }
            }
        }
        Ok(Value::Nil)
    }

    /// Deliver a returned `val` into whatever continuation frame sits at the top of `self.frames`.
    /// Returns `Ok(Some(val))` when the stack empties completely at the root.
    fn deliver_value(&mut self, mut val: Value) -> MoraResult<Option<Value>> {
        loop {
            let Some(frame) = self.frames.pop() else {
                return Ok(Some(val));
            };
            match frame {
                CallFrame::Script(mut sf) => {
                    sf.receive_reply(val);
                    self.frames.push(CallFrame::Script(sf));
                    return Ok(None);
                }
                CallFrame::HandleBarrier(_) | CallFrame::ErrorContext { .. } => {
                    // Handled body returned normally or macro finished cleanly; pass `val` down.
                    continue;
                }
                CallFrame::AutoResume(cont) => {
                    self.resume_continuation(&cont, val)?;
                    return Ok(None);
                }
                CallFrame::Transaction(_) => {
                    // Transaction body succeeded cleanly; mutations remain committed.
                    val = Value::Nil;
                    continue;
                }
                CallFrame::EvalRestore { saved_env } => {
                    self.core.environment = saved_env;
                    continue;
                }
                CallFrame::ImportFinalize {
                    saved_env,
                    child_program,
                    child_globals,
                } => {
                    let mut exports: Vec<(String, Value)> = Vec::new();
                    for (idx, meta) in child_program.globals.iter().enumerate() {
                        let guard = child_globals[idx].lock();
                        if guard.1 {
                            exports.push((meta.name.clone(), guard.0.clone()));
                        }
                    }
                    for name in self.core.environment.exported_names() {
                        if let Some(v) = self.core.environment.get(&name) {
                            exports.push((name, v));
                        }
                    }
                    self.core.environment = saved_env;
                    for (name, v) in exports {
                        self.core.environment.define(name, v, false);
                    }
                    val = Value::Nil;
                    continue;
                }
                CallFrame::Parallel(mut pf) => {
                    pf.last_val = val;
                    if self.advance_parallel(&mut pf)? {
                        // A new sequential frame was pushed on top of `pf` (already re-pushed inside).
                        return Ok(None);
                    }
                    // All segments finished!
                    val = pf.last_val;
                    continue;
                }
                CallFrame::Combinator(comb) => match self.step_combinator(comb, val)? {
                    Some(done_val) => {
                        val = done_val;
                        continue;
                    }
                    None => return Ok(None),
                },
            }
        }
    }

    fn step_combinator(
        &mut self,
        comb: CombinatorFrame,
        reply: Value,
    ) -> MoraResult<Option<Value>> {
        match comb {
            CombinatorFrame::Compose { funcs, next_idx } => {
                if next_idx >= funcs.len() {
                    return Ok(Some(reply));
                }
                let next_fn = funcs[next_idx].clone();
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::Compose {
                        funcs,
                        next_idx: next_idx + 1,
                    }));
                let action = resolve_value_call(next_fn, vec![reply], self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
            }
            CombinatorFrame::ListMap {
                func,
                items,
                idx,
                mut results,
            } => {
                results.push(reply);
                let next = idx + 1;
                if next >= items.len() {
                    return Ok(Some(Value::List(results.into())));
                }
                let next_item = items[next].clone();
                let next_fn = func.clone();
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::ListMap {
                        func,
                        items,
                        idx: next,
                        results,
                    }));
                let action = resolve_value_call(next_fn, vec![next_item], self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
            }
            CombinatorFrame::ListFilter {
                pred,
                items,
                idx,
                mut results,
            } => {
                if is_truthy(&reply) {
                    results.push(items[idx].clone());
                }
                let next = idx + 1;
                if next >= items.len() {
                    return Ok(Some(Value::List(results.into())));
                }
                let next_item = items[next].clone();
                let next_pred = pred.clone();
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::ListFilter {
                        pred,
                        items,
                        idx: next,
                        results,
                    }));
                let action = resolve_value_call(next_pred, vec![next_item], self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
            }
            CombinatorFrame::ListReduce { func, items, idx } => {
                let next = idx + 1;
                if next >= items.len() {
                    return Ok(Some(reply));
                }
                let next_item = items[next].clone();
                let next_fn = func.clone();
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::ListReduce {
                        func,
                        items,
                        idx: next,
                    }));
                let action = resolve_value_call(next_fn, vec![reply, next_item], self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
            }
            CombinatorFrame::Into {
                transform,
                items,
                idx,
                mut results,
            } => {
                match reply {
                    Value::List(xs) => results.extend(xs),
                    other => results.push(other),
                }
                let next = idx + 1;
                if next >= items.len() {
                    return Ok(Some(Value::List(results.into())));
                }
                let next_item = items[next].clone();
                let next_tr = transform.clone();
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::Into {
                        transform,
                        items,
                        idx: next,
                        results,
                    }));
                let action = resolve_value_call(next_tr, vec![next_item], self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
            }
            CombinatorFrame::Swap { atom } => {
                *atom.lock() = reply.clone();
                Ok(Some(reply))
            }
        }
    }

    /// Execute `action` expecting its return value to deliver into whatever frame is now on top.
    /// Note: may call `deliver_value` immediately if `action` completes synchronously!
    fn apply_action_chained(&mut self, mut action: CallAction) -> MoraResult<()> {
        loop {
            match action {
                CallAction::Done(v) => {
                    // Deliver synchronously; if `deliver_value` schedules new combinator/frame, exit loop.
                    if let Some(root_val) = self.deliver_value(v)? {
                        // Nothing below on stack; re-wrap as an empty script completion by returning via fake push?
                        // Wait: `step_script` checks stack emptiness cleanly when top returns!
                        // Wait: `apply_action_chained` is ONLY called when a caller/combinator sits below!
                        // However if `step_combinator` returned `Some(v)`, it already bypassed this path.
                        // Still handle `Some(root_val)` robustly by pushing a trivial ScriptFrame? Better:
                        // return an unreachable guard never hit because a parent sits on stack!
                        let _ = root_val;
                    }
                    return Ok(());
                }
                CallAction::PushFrame {
                    frame,
                    error_prefix,
                } => {
                    if let Some(prefix) = error_prefix {
                        self.frames.push(CallFrame::ErrorContext { prefix });
                    }
                    self.frames.push(CallFrame::Script(Box::new(frame)));
                    return Ok(());
                }
                CallAction::PushCombinator {
                    combinator,
                    callee,
                    args,
                } => {
                    self.frames.push(CallFrame::Combinator(combinator));
                    action = resolve_value_call(callee, args, self.core)?;
                }
                CallAction::EvalSource { source } => {
                    self.schedule_eval(&source)?;
                    return Ok(());
                }
                CallAction::MacroExpand { name, args } => {
                    self.schedule_macroexpand(&name, args)?;
                    return Ok(());
                }
                CallAction::AmbientRandom {
                    label,
                    method,
                    args,
                } => {
                    // Ambient `random.*` runs directly without capturing/wiping if unhandled,
                    // or performs to nearest active `HandleBarrier(label)`.
                    if self.find_barrier(label).is_some() {
                        // Push a dummy passthrough continuation? Caller already set resume_dst + pc+1!
                        // Wait: if `Combinator` invoked `random.random` directly (unlikely since
                        // first-class random calls don't exist without wrapper), still safe because
                        // `self.frames` top frame expects reply via `deliver_value`!
                        let Some(barrier_idx) = self.find_barrier(label) else {
                            unreachable!()
                        };
                        self.capture_and_enter_handler(barrier_idx, label, args)?;
                        return Ok(());
                    }
                    let val = self
                        .core
                        .random_state
                        .dispatch_op(label, &args)
                        .map_err(|reason| reason.with_prefix(&format!("random.{method}: ")))?;
                    action = CallAction::Done(val);
                }
                CallAction::ResumeContinuation { cont, val } => {
                    self.resume_continuation(&cont, val)?;
                    return Ok(());
                }
            }
        }
    }

    fn apply_action(&mut self, action: CallAction) -> MoraResult<()> {
        let mut cur = action;
        loop {
            match cur {
                CallAction::Done(_) => return Ok(()),
                CallAction::PushFrame {
                    frame,
                    error_prefix,
                } => {
                    if let Some(prefix) = error_prefix {
                        self.frames.push(CallFrame::ErrorContext { prefix });
                    }
                    self.frames.push(CallFrame::Script(Box::new(frame)));
                    return Ok(());
                }
                CallAction::PushCombinator {
                    combinator,
                    callee,
                    args,
                } => {
                    self.frames.push(CallFrame::Combinator(combinator));
                    cur = resolve_value_call(callee, args, self.core)?;
                }
                CallAction::EvalSource { source } => {
                    self.schedule_eval(&source)?;
                    return Ok(());
                }
                CallAction::MacroExpand { name, args } => {
                    self.schedule_macroexpand(&name, args)?;
                    return Ok(());
                }
                CallAction::AmbientRandom { .. } => {
                    unreachable!(
                        "ambient random is only reachable via method resolution inside a frame"
                    )
                }
                CallAction::ResumeContinuation { cont, val } => {
                    self.resume_continuation(&cont, val)?;
                    return Ok(());
                }
            }
        }
    }

    fn handle_error(&mut self, mut err: MoraError) -> MoraResult<()> {
        while let Some(frame) = self.frames.pop() {
            match frame {
                CallFrame::ErrorContext { prefix } => {
                    err = err.with_prefix(&prefix);
                }
                CallFrame::EvalRestore { saved_env }
                | CallFrame::ImportFinalize { saved_env, .. } => {
                    self.core.environment = saved_env;
                }
                CallFrame::Transaction(tx) if !tx.in_compensation => {
                    self.core.environment = tx.saved_env;
                    restore_globals(&tx.target_globals, &tx.saved_globals_vals);
                    let comp_frame = materialize_frame(
                        tx.comp_program,
                        tx.comp_fn,
                        tx.target_globals,
                        tx.comp_upvalues,
                        &[],
                    );
                    if let Ok(cf) = comp_frame {
                        let mut sub_vm = VmEngine::new(self.core);
                        if let Err(ce) = sub_vm.run_frame(cf) {
                            eprintln!("[warn] transaction compensation failed: {ce}");
                        }
                    }
                    if !err.is_cancellation_or_timeout() {
                        err = MoraError::Runtime("Transaction rolled back".to_string());
                    }
                }
                CallFrame::Script(_)
                | CallFrame::HandleBarrier(_)
                | CallFrame::AutoResume(_)
                | CallFrame::Combinator(_)
                | CallFrame::Transaction(_)
                | CallFrame::Parallel(_) => {}
            }
        }
        Err(err)
    }

    fn schedule_eval(&mut self, source: &str) -> MoraResult<()> {
        let witnesses = ParserV3::parse_witnesses(source)
            .map_err(|e| e.with_prefix("eval: compile error: "))?;
        let mut child_env = Environment::with_parent_of(Arc::new(self.core.environment.clone()));
        // Snapshot any visible lexical globals/locals/upvalues from nearest active ScriptFrame.
        let caller = self.frames.iter().rev().find_map(|f| match f {
            CallFrame::Script(sf) => Some(sf),
            _ => None,
        });
        if let Some(sf) = caller {
            for (idx, meta) in sf.program.globals.iter().enumerate() {
                child_env.define(meta.name.clone(), sf.globals[idx].lock().0.clone(), false);
            }
            let func = sf.func();
            for (idx, uv_desc) in func.upvalues.iter().enumerate() {
                let name = sf.program.symbols.resolve(uv_desc.sym);
                child_env.define(name.to_string(), sf.upvalues[idx].get(), false);
            }
            for (idx, sym) in func.local_names.iter().enumerate() {
                let name = sf.program.symbols.resolve(*sym);
                if let Some(cell) = sf.locals.get(idx) {
                    child_env.define(name.to_string(), cell.get(), false);
                }
            }
        }
        let dyn_names: Vec<String> = collect_env_names(&child_env);
        let mut prog = crate::mir::control::lower_program_with_dyn_globals(&witnesses, &dyn_names)
            .map_err(|e| e.with_prefix("eval: compile error: "))?;
        optimize_program(&mut prog, OptLevel::from_env());
        let prog_arc = Arc::new(prog);
        let entry = prog_arc.entry;
        let globals = super::continuation::init_globals(&prog_arc);
        let saved_env = std::mem::replace(&mut self.core.environment, child_env);
        let frame = materialize_frame(prog_arc, entry, globals, Arc::new(Vec::new()), &[])?;
        self.frames.push(CallFrame::EvalRestore { saved_env });
        self.frames.push(CallFrame::Script(Box::new(frame)));
        Ok(())
    }

    fn schedule_macroexpand(&mut self, name: &str, args: Vec<Value>) -> MoraResult<()> {
        let found = self
            .lookup_visible_binding(name)
            .ok_or_else(|| MoraError::Runtime(format!("macroexpand: undefined macro '{name}'")))?;
        match found {
            Value::Macro {
                name: mname,
                params,
                program,
                fn_id,
                globals,
            } => {
                if args.len() != params.len() {
                    return Err(MoraError::Runtime(format!(
                        "macroexpand: macro '{mname}' expects {} args, got {}",
                        params.len(),
                        args.len()
                    )));
                }
                let frame =
                    materialize_frame(program, fn_id, globals, Arc::new(Vec::new()), &args)?;
                self.frames.push(CallFrame::ErrorContext {
                    prefix: format!("macro '{mname}' expansion error: "),
                });
                self.frames.push(CallFrame::Script(Box::new(frame)));
                Ok(())
            }
            _ => Err(MoraError::Runtime(format!(
                "macroexpand: '{name}' is not a macro"
            ))),
        }
    }

    fn lookup_visible_binding(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.core.environment.get(name) {
            return Some(v);
        }
        let caller = self.frames.iter().rev().find_map(|f| match f {
            CallFrame::Script(sf) => Some(sf),
            _ => None,
        })?;
        for (idx, meta) in caller.program.globals.iter().enumerate() {
            if meta.name == name {
                return Some(caller.globals[idx].lock().0.clone());
            }
        }
        None
    }

    fn find_barrier(&self, effect_name: &str) -> Option<usize> {
        self.frames.iter().rposition(|f| match f {
            CallFrame::HandleBarrier(b) => b.effect_name == effect_name,
            _ => false,
        })
    }

    fn capture_and_enter_handler(
        &mut self,
        barrier_idx: usize,
        effect_name: &str,
        args: Vec<Value>,
    ) -> MoraResult<()> {
        let mut drained = self.frames.split_off(barrier_idx);
        let CallFrame::HandleBarrier(barrier) = drained.remove(0) else {
            unreachable!("find_barrier returned HandleBarrier index");
        };
        let cont = Arc::new(CapturedContinuation {
            effect_name: effect_name.to_string(),
            barrier: barrier.clone(),
            frames: drained,
        });
        let handler_frame = materialize_handler_frame(&barrier, &args, Arc::clone(&cont))?;
        if barrier.auto_resume {
            self.frames.push(CallFrame::AutoResume(cont));
        }
        self.frames.push(CallFrame::Script(Box::new(handler_frame)));
        Ok(())
    }

    fn resume_continuation(
        &mut self,
        cont: &Arc<CapturedContinuation>,
        val: Value,
    ) -> MoraResult<()> {
        self.frames
            .push(CallFrame::HandleBarrier(cont.barrier.clone()));
        let mut restored = cont.frames.clone();
        if let Some(last) = restored.last_mut() {
            match last {
                CallFrame::Script(sf) => sf.receive_reply(val),
                CallFrame::Combinator(_) => {
                    // Combinator waits atop ScriptFrame; resume will deliver directly on pop
                    // if we deliver via sub-call. Even simpler: push `restored`, and if top is not
                    // `Script`, call `deliver_value(val)` immediately!
                    self.frames.extend(restored);
                    if let Some(root_val) = self.deliver_value(val)? {
                        // Should never finish root cleanly when restoring into non-empty combinator,
                        // unless barrier was topmost -> still safe.
                        let _ = root_val;
                    }
                    return Ok(());
                }
                _ => {
                    self.frames.extend(restored);
                    let _ = self.deliver_value(val)?;
                    return Ok(());
                }
            }
        }
        self.frames.extend(restored);
        Ok(())
    }

    /// Drive a [`ParallelFrame`] until it spawns a sequential child frame or completes (`Ok(false)`).
    fn advance_parallel(&mut self, pf: &mut ParallelFrame) -> MoraResult<bool> {
        while pf.next_idx < pf.segments.len() {
            let seg = pf.segments[pf.next_idx].clone();
            pf.next_idx += 1;
            match seg {
                ParallelSegment::Sequential(fn_id, captures) => {
                    let child_func = pf.program.func(fn_id);
                    let upvalues = resolve_captures(
                        &captures,
                        &pf.parent_locals,
                        &pf.parent_upvalues,
                        child_func,
                    );
                    let frame = materialize_frame(
                        Arc::clone(&pf.program),
                        fn_id,
                        Arc::clone(&pf.globals),
                        Arc::new(upvalues),
                        &[],
                    )?;
                    self.frames.push(CallFrame::Parallel(pf.clone()));
                    self.frames.push(CallFrame::Script(Box::new(frame)));
                    return Ok(true);
                }
                ParallelSegment::WorkerBatch(workers) => {
                    self.run_worker_batch(pf, &workers)?;
                    pf.last_val = Value::Nil;
                }
            }
        }
        Ok(false)
    }

    fn run_worker_batch(
        &mut self,
        pf: &mut ParallelFrame,
        workers: &[(
            crate::mir::control::SymId,
            FnId,
            Vec<crate::mir::control::CaptureSource>,
        )],
    ) -> MoraResult<()> {
        let base_globals_vals = snapshot_globals(&pf.globals);

        // Pre-resolve shared upvalues in parent before spawning worker tasks.
        let mut parent_resolved: Vec<Vec<UpvalueValue>> = Vec::with_capacity(workers.len());
        let mut cell_snapshots: HashMap<usize, (String, Value, UpvalueCell)> = HashMap::new();
        for (_wsym, fn_id, captures) in workers {
            let child_func = pf.program.func(*fn_id);
            let uvs =
                resolve_captures(captures, &pf.parent_locals, &pf.parent_upvalues, child_func);
            for (idx, uv) in uvs.iter().enumerate() {
                if let UpvalueValue::Cell(cell) = uv {
                    let key = Arc::as_ptr(cell) as usize;
                    cell_snapshots.entry(key).or_insert_with(|| {
                        let name = child_func
                            .upvalues
                            .get(idx)
                            .map(|d| pf.program.symbols.resolve(d.sym).to_string())
                            .unwrap_or_default();
                        let val = cell.lock().clone();
                        (name, val, Arc::clone(cell))
                    });
                }
            }
            parent_resolved.push(uvs);
        }

        type ParallelWorkerOutput = (CoreRuntime, ModuleGlobals, HashMap<usize, UpvalueCell>);
        let jobs: Vec<_> = workers
            .iter()
            .zip(parent_resolved.iter())
            .map(|((_sym, fn_id, _caps), p_uvs)| {
                let w_core = self.core.clone();
                let w_prog = Arc::clone(&pf.program);
                let w_globals = clone_isolated_globals(&base_globals_vals);
                let mut w_cell_map: HashMap<usize, UpvalueCell> = HashMap::new();
                let w_uvs = clone_worker_upvalues(p_uvs, w_prog.func(*fn_id), &mut w_cell_map);
                let fn_id = *fn_id;
                move |task_ctx: crate::runtime::scheduler::TaskContext| -> MoraResult<ParallelWorkerOutput> {
                    task_ctx.status()?;
                    let mut w_core = w_core.with_task_context(task_ctx);
                    let frame = materialize_frame(
                        w_prog,
                        fn_id,
                        Arc::clone(&w_globals),
                        Arc::new(w_uvs),
                        &[],
                    )?;
                    let mut vm = VmEngine::new(&mut w_core);
                    let _ = vm.run_frame(frame)?;
                    Ok((w_core, w_globals, w_cell_map))
                }
            })
            .collect();

        let scheduler = Arc::clone(self.core.scheduler());
        let results = scheduler.run_indexed_batch(jobs, self.core.task_context.as_ref())?;

        let strategies = self.core.current_merge_strategies.clone();
        for (w_core, w_globals, w_cells) in results {
            match strategies.as_ref() {
                Some(sg) => {
                    self.core.environment.merge_from_with_strategies(
                        &w_core.environment,
                        sg,
                        &MergeStrategy::LastWriteWins,
                    );
                }
                None => self
                    .core
                    .environment
                    .merge_from(&w_core.environment, &MergeStrategy::LastWriteWins),
            }

            // Merge changed module global slots deterministically in worker declaration order.
            for (slot, meta) in pf.program.globals.iter().enumerate() {
                let w_val = w_globals[slot].lock().0.clone();
                if w_val != base_globals_vals[slot].0 {
                    let strat = strategies
                        .as_ref()
                        .and_then(|s| s.get(&meta.name))
                        .unwrap_or(&MergeStrategy::LastWriteWins);
                    let mut guard = pf.globals[slot].lock();
                    guard.0 = Value::merge(guard.0.clone(), w_val, strat);
                }
            }

            // Merge mutated shared local cells (closed over by workers).
            for (key, w_cell) in w_cells {
                if let Some((name, base_val, parent_cell)) = cell_snapshots.get(&key) {
                    let w_val = w_cell.lock().clone();
                    if w_val != *base_val {
                        let strat = strategies
                            .as_ref()
                            .and_then(|s| s.get(name))
                            .unwrap_or(&MergeStrategy::LastWriteWins);
                        let mut guard = parent_cell.lock();
                        *guard = Value::merge(guard.clone(), w_val, strat);
                    }
                }
            }
        }

        Ok(())
    }
}

enum ScriptStep {
    Completed(Value),
    Suspended,
    Err(MoraError),
}

impl<'a> VmEngine<'a> {
    fn step_script(&mut self, frame: &mut ScriptFrame) -> ScriptStep {
        let mut cancel_budget: u32 = 0;
        loop {
            if cancel_budget == 0 {
                if let Err(err) = self.core.check_cancelled() {
                    return ScriptStep::Err(err);
                }
                cancel_budget = 256;
            } else {
                cancel_budget -= 1;
            }
            let block_idx = frame.block_id.0 as usize;
            let inst_len = frame.func().blocks[block_idx].insts.len();
            if frame.pc < inst_len {
                let inst = frame.func().blocks[block_idx].insts[frame.pc].clone();
                match self.exec_inst(frame, inst) {
                    StepOutcome::NextInst => {
                        frame.pc += 1;
                    }
                    StepOutcome::Yielded => {
                        // `exec_inst` already updated `frame.pc` / `frame.resume_dst`, re-pushed
                        // `frame` cleanly, and pushed callee frames above it!
                        return ScriptStep::Suspended;
                    }
                    StepOutcome::Err(e) => return ScriptStep::Err(e),
                }
            } else {
                let term = frame.func().blocks[block_idx].terminator.clone();
                match term {
                    ControlTerminator::Jump { target, args } => {
                        let vals: Vec<Value> = args
                            .iter()
                            .map(|r| frame.regs[r.0 as usize].clone())
                            .collect();
                        let target_params = frame.func().blocks[target.0 as usize].params.clone();
                        for (p, v) in target_params.into_iter().zip(vals) {
                            frame.regs[p.0 as usize] = v;
                        }
                        frame.block_id = target;
                        frame.pc = 0;
                    }
                    ControlTerminator::Branch {
                        cond,
                        then_bb,
                        then_args,
                        else_bb,
                        else_args,
                    } => {
                        let truthy = is_truthy(&frame.regs[cond.0 as usize]);
                        let (target, arg_regs) = if truthy {
                            (then_bb, then_args)
                        } else {
                            (else_bb, else_args)
                        };
                        let vals: Vec<Value> = arg_regs
                            .iter()
                            .map(|r| frame.regs[r.0 as usize].clone())
                            .collect();
                        let target_params = frame.func().blocks[target.0 as usize].params.clone();
                        for (p, v) in target_params.into_iter().zip(vals) {
                            frame.regs[p.0 as usize] = v;
                        }
                        frame.block_id = target;
                        frame.pc = 0;
                    }
                    ControlTerminator::Return(opt_r) | ControlTerminator::Halt(opt_r) => {
                        let val = opt_r
                            .map(|r| frame.regs[r.0 as usize].clone())
                            .unwrap_or(Value::Nil);
                        return ScriptStep::Completed(val);
                    }
                    ControlTerminator::Perform {
                        effect,
                        args,
                        resume_bb,
                    } => {
                        let effect_name = frame.program.symbols.resolve(effect).to_string();
                        let arg_vals: Vec<Value> = args
                            .iter()
                            .map(|r| frame.regs[r.0 as usize].clone())
                            .collect();
                        frame.block_id = resume_bb;
                        frame.pc = 0;
                        frame.resume_dst = None;

                        if let Some(barrier_idx) = self.find_barrier(&effect_name) {
                            self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                            if let Err(e) =
                                self.capture_and_enter_handler(barrier_idx, &effect_name, arg_vals)
                            {
                                return ScriptStep::Err(e);
                            }
                            return ScriptStep::Suspended;
                        }
                        if ambient::is_ambient_label(&effect_name) {
                            match self.core.random_state.dispatch_op(&effect_name, &arg_vals) {
                                Ok(v) => {
                                    frame.receive_reply(v);
                                    continue;
                                }
                                Err(e) => return ScriptStep::Err(e),
                            }
                        }
                        return ScriptStep::Err(MoraError::Runtime(format!(
                            "unhandled effect: {effect_name}"
                        )));
                    }
                    ControlTerminator::TailCall { target, args } => {
                        let arg_vals: Vec<Value> = args
                            .iter()
                            .map(|r| frame.regs[r.0 as usize].clone())
                            .collect();
                        let action = match self.resolve_callee(frame, &target, arg_vals) {
                            Ok(a) => a,
                            Err(e) => return ScriptStep::Err(e),
                        };
                        match action {
                            CallAction::Done(val) => return ScriptStep::Completed(val),
                            CallAction::PushFrame {
                                frame: next_frame,
                                error_prefix: None,
                            } => {
                                // Zero-allocation in-place frame reuse for O(1) TCO!
                                *frame = next_frame;
                                continue;
                            }
                            other => {
                                // Tail-call across combinator / eval / macro / continuation:
                                // drop current frame first so stack doesn't grow!
                                if let Err(e) = self.apply_action_chained(other) {
                                    return ScriptStep::Err(e);
                                }
                                return ScriptStep::Suspended;
                            }
                        }
                    }
                    ControlTerminator::Unreachable => {
                        return ScriptStep::Err(MoraError::Runtime(
                            "reached unreachable CFG terminator".to_string(),
                        ));
                    }
                }
            }
        }
    }

    fn resolve_callee(
        &mut self,
        frame: &ScriptFrame,
        target: &CalleeTarget,
        arg_vals: Vec<Value>,
    ) -> MoraResult<CallAction> {
        match target {
            CalleeTarget::Builtin(id) => resolve_builtin_call(*id, self.core, arg_vals),
            CalleeTarget::Fn(fn_id) => {
                let next_frame = materialize_frame(
                    Arc::clone(&frame.program),
                    *fn_id,
                    Arc::clone(&frame.globals),
                    Arc::new(Vec::new()),
                    &arg_vals,
                )?;
                Ok(CallAction::PushFrame {
                    frame: next_frame,
                    error_prefix: None,
                })
            }
            CalleeTarget::Value(r) => {
                let val = frame.regs[r.0 as usize].clone();
                resolve_value_call(val, arg_vals, self.core)
            }
            CalleeTarget::DynGlobal(sym) => {
                let name = frame.program.symbols.resolve(*sym);
                resolve_dyn_call(
                    name,
                    arg_vals,
                    self.core,
                    Some(&(Arc::clone(&frame.program), Arc::clone(&frame.globals))),
                )
            }
        }
    }

    fn suspend_and_dispatch(
        &mut self,
        frame: &mut ScriptFrame,
        dst: crate::mir::control::VReg,
        action: CallAction,
    ) -> StepOutcome {
        match action {
            CallAction::Done(val) => {
                frame.regs[dst.0 as usize] = val;
                StepOutcome::NextInst
            }
            CallAction::AmbientRandom {
                label,
                method,
                args,
            } => {
                if let Some(barrier_idx) = self.find_barrier(label) {
                    frame.pc += 1;
                    frame.resume_dst = Some(dst);
                    self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                    if let Err(e) = self.capture_and_enter_handler(barrier_idx, label, args) {
                        return StepOutcome::Err(e);
                    }
                    StepOutcome::Yielded
                } else {
                    match self.core.random_state.dispatch_op(label, &args) {
                        Ok(val) => {
                            frame.regs[dst.0 as usize] = val;
                            StepOutcome::NextInst
                        }
                        Err(reason) => {
                            StepOutcome::Err(reason.with_prefix(&format!("random.{method}: ")))
                        }
                    }
                }
            }
            other => {
                frame.pc += 1;
                frame.resume_dst = Some(dst);
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                if let Err(e) = self.apply_action_chained(other) {
                    // Frame is on self.frames, handle_error will unwind it!
                    return StepOutcome::Err(e);
                }
                StepOutcome::Yielded
            }
        }
    }

    fn exec_inst(&mut self, frame: &mut ScriptFrame, inst: ControlInst) -> StepOutcome {
        match inst {
            ControlInst::Const(dst, c) => {
                frame.regs[dst.0 as usize] = c.to_value();
                StepOutcome::NextInst
            }
            ControlInst::Copy(dst, src) => {
                frame.regs[dst.0 as usize] = frame.regs[src.0 as usize].clone();
                StepOutcome::NextInst
            }
            ControlInst::Unary(dst, op, src) => {
                let v = &frame.regs[src.0 as usize];
                let out = match op {
                    UnaryOp::Truthy => Value::Bool(is_truthy(v)),
                    UnaryOp::Not => Value::Bool(!is_truthy(v)),
                    UnaryOp::Neg => match crate::flow::eval_unary_neg(v) {
                        Ok(negated) => negated,
                        Err(e) => return StepOutcome::Err(e),
                    },
                };
                frame.regs[dst.0 as usize] = out;
                StepOutcome::NextInst
            }
            ControlInst::Binary(dst, lhs, op, rhs) => {
                let l = frame.regs[lhs.0 as usize].clone();
                let r = frame.regs[rhs.0 as usize].clone();
                match eval_binary(l, &op, r) {
                    Ok(v) => {
                        frame.regs[dst.0 as usize] = v;
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(MoraError::Runtime(e.to_string())),
                }
            }
            ControlInst::ListLit(dst, items) => {
                let list: crate::value::list::List = items
                    .into_iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                frame.regs[dst.0 as usize] = Value::List(list);
                StepOutcome::NextInst
            }
            ControlInst::DictLit(dst, pairs) => {
                let mut map = HashMap::with_capacity(pairs.len());
                for (sym, r) in pairs {
                    let key = frame.program.symbols.resolve(sym).to_string();
                    map.insert(key, frame.regs[r.0 as usize].clone());
                }
                frame.regs[dst.0 as usize] = Value::Dict(map);
                StepOutcome::NextInst
            }
            ControlInst::Quasiquote(dst, segs) => match eval_quasiquote(&segs, &frame.regs) {
                Ok(v) => {
                    frame.regs[dst.0 as usize] = v;
                    StepOutcome::NextInst
                }
                Err(e) => StepOutcome::Err(e),
            },
            ControlInst::MakeClosure {
                dst,
                fn_id,
                captures,
            } => {
                let child_func = frame.program.func(fn_id);
                let upvalues =
                    resolve_captures(&captures, &frame.locals, &frame.upvalues, child_func);
                let params: Vec<String> = child_func
                    .params
                    .iter()
                    .map(|s| frame.program.symbols.resolve(*s).to_string())
                    .collect();
                frame.regs[dst.0 as usize] = Value::Closure {
                    params,
                    program: Arc::clone(&frame.program),
                    fn_id,
                    globals: Arc::clone(&frame.globals),
                    upvalues: Arc::new(upvalues),
                };
                StepOutcome::NextInst
            }
            ControlInst::MakeDynTrait {
                dst,
                src,
                trait_name,
                generics,
            } => {
                let tname = frame.program.symbols.resolve(trait_name).to_string();
                let val = frame.regs[src.0 as usize].clone();
                frame.regs[dst.0 as usize] = make_dyn_trait(val, &tname, &generics);
                StepOutcome::NextInst
            }
            ControlInst::LoadLocal(dst, slot) => {
                frame.regs[dst.0 as usize] = frame.locals[slot.0 as usize].get();
                StepOutcome::NextInst
            }
            ControlInst::StoreLocal(slot, src) => {
                let val = frame.regs[src.0 as usize].clone();
                frame.locals[slot.0 as usize].set(val);
                StepOutcome::NextInst
            }
            ControlInst::LoadUpvalue(dst, slot) => {
                frame.regs[dst.0 as usize] = frame.upvalues[slot.0 as usize].get();
                StepOutcome::NextInst
            }
            ControlInst::StoreUpvalue(slot, src) => {
                let val = frame.regs[src.0 as usize].clone();
                match frame.upvalues[slot.0 as usize].set(val) {
                    Ok(()) => StepOutcome::NextInst,
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::LoadGlobal(dst, slot) => {
                frame.regs[dst.0 as usize] = frame.globals[slot.0 as usize].lock().0.clone();
                StepOutcome::NextInst
            }
            ControlInst::StoreGlobal(slot, src) => {
                let val = frame.regs[src.0 as usize].clone();
                frame.globals[slot.0 as usize].lock().0 = val;
                StepOutcome::NextInst
            }
            ControlInst::LoadDyn(dst, sym) => {
                let name = frame.program.symbols.resolve(sym);
                let val = self
                    .core
                    .environment
                    .get(name)
                    .or_else(|| BuiltinKind::from_name(name).map(Value::Builtin))
                    .unwrap_or(Value::Nil);
                frame.regs[dst.0 as usize] = val;
                StepOutcome::NextInst
            }
            ControlInst::DefineDyn(sym, src, exported) => {
                let name = frame.program.symbols.resolve(sym).to_string();
                let val = frame.regs[src.0 as usize].clone();
                self.core.environment.define(name, val, exported);
                StepOutcome::NextInst
            }
            ControlInst::AssignDyn(sym, src) => {
                let name = frame.program.symbols.resolve(sym);
                let val = frame.regs[src.0 as usize].clone();
                let _ = self.core.environment.assign(name, val);
                StepOutcome::NextInst
            }
            ControlInst::ExportGlobal(slot) => {
                frame.globals[slot.0 as usize].lock().1 = true;
                StepOutcome::NextInst
            }
            ControlInst::ExportDyn(sym) => {
                let name = frame.program.symbols.resolve(sym);
                self.core.environment.mark_exported(name);
                StepOutcome::NextInst
            }
            ControlInst::Index(dst, obj, idx) => {
                match index_value(&frame.regs[obj.0 as usize], &frame.regs[idx.0 as usize]) {
                    Ok(v) => {
                        frame.regs[dst.0 as usize] = v;
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::IndexAssign(dst, obj, idx, val) => {
                let mut container = frame.regs[obj.0 as usize].clone();
                let idx_val = frame.regs[idx.0 as usize].clone();
                let elem_val = frame.regs[val.0 as usize].clone();
                match index_assign_value(&mut container, &idx_val, &elem_val) {
                    Ok(()) => {
                        frame.regs[dst.0 as usize] = container;
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Call { dst, target, args } => {
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                match self.resolve_callee(frame, &target, arg_vals) {
                    Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::ModuleCall {
                dst,
                module,
                method,
                args,
            } => {
                let mname = frame.program.symbols.resolve(method).to_string();
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                match resolve_method_call(
                    self.core,
                    Value::Builtin(module),
                    &mname,
                    arg_vals,
                    Span::default(),
                ) {
                    Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::MethodCall {
                dst,
                receiver,
                method,
                args,
            } => {
                let mname = frame.program.symbols.resolve(method).to_string();
                let recv = frame.regs[receiver.0 as usize].clone();
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                match resolve_method_call(self.core, recv, &mname, arg_vals, Span::default()) {
                    Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Pipe { dst, lhs, rhs } => {
                let l = frame.regs[lhs.0 as usize].clone();
                let r = frame.regs[rhs.0 as usize].clone();
                match resolve_value_call(r, vec![l], self.core) {
                    Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::MatchPattern {
                dst_ok,
                val,
                pattern,
            } => {
                let scrutinee = &frame.regs[val.0 as usize];
                if let Some(bindings) = match_pattern(scrutinee, &pattern) {
                    for (target, bound_val) in bindings {
                        match target {
                            PatternTarget::Local(slot) => {
                                frame.locals[slot.0 as usize].set(bound_val);
                            }
                            PatternTarget::Global(slot) => {
                                frame.globals[slot.0 as usize].lock().0 = bound_val;
                            }
                            PatternTarget::Dyn(sym) => {
                                let name = frame.program.symbols.resolve(sym).to_string();
                                self.core.environment.define(name, bound_val, false);
                            }
                            PatternTarget::Discard => {}
                        }
                    }
                    frame.regs[dst_ok.0 as usize] = Value::Bool(true);
                } else {
                    frame.regs[dst_ok.0 as usize] = Value::Bool(false);
                }
                StepOutcome::NextInst
            }
            ControlInst::Handle {
                dst,
                effect,
                body_fn,
                body_captures,
                handler_fn,
                handler_captures,
                auto_resume,
            } => {
                let effect_name = frame.program.symbols.resolve(effect).to_string();
                let h_uvs = resolve_captures(
                    &handler_captures,
                    &frame.locals,
                    &frame.upvalues,
                    frame.program.func(handler_fn),
                );
                let b_uvs = resolve_captures(
                    &body_captures,
                    &frame.locals,
                    &frame.upvalues,
                    frame.program.func(body_fn),
                );
                let body_frame = match materialize_frame(
                    Arc::clone(&frame.program),
                    body_fn,
                    Arc::clone(&frame.globals),
                    Arc::new(b_uvs),
                    &[],
                ) {
                    Ok(f) => f,
                    Err(e) => return StepOutcome::Err(e),
                };
                let barrier = HandleBarrierState {
                    effect_name,
                    handler_program: Arc::clone(&frame.program),
                    handler_fn,
                    handler_globals: Arc::clone(&frame.globals),
                    handler_upvalues: Arc::new(h_uvs),
                    auto_resume,
                };
                frame.pc += 1;
                frame.resume_dst = Some(dst);
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                self.frames.push(CallFrame::HandleBarrier(barrier));
                self.frames.push(CallFrame::Script(Box::new(body_frame)));
                StepOutcome::Yielded
            }
            ControlInst::Transaction {
                dst,
                body_fn,
                body_captures,
                comp_fn,
                comp_captures,
            } => {
                let b_uvs = resolve_captures(
                    &body_captures,
                    &frame.locals,
                    &frame.upvalues,
                    frame.program.func(body_fn),
                );
                let c_uvs = resolve_captures(
                    &comp_captures,
                    &frame.locals,
                    &frame.upvalues,
                    frame.program.func(comp_fn),
                );
                let body_frame = match materialize_frame(
                    Arc::clone(&frame.program),
                    body_fn,
                    Arc::clone(&frame.globals),
                    Arc::new(b_uvs),
                    &[],
                ) {
                    Ok(f) => f,
                    Err(e) => return StepOutcome::Err(e),
                };
                let tx = TransactionState {
                    saved_env: self.core.environment.clone(),
                    saved_globals_vals: snapshot_globals(&frame.globals),
                    target_globals: Arc::clone(&frame.globals),
                    comp_program: Arc::clone(&frame.program),
                    comp_fn,
                    comp_upvalues: Arc::new(c_uvs),
                    in_compensation: false,
                };
                frame.pc += 1;
                frame.resume_dst = Some(dst);
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                self.frames.push(CallFrame::Transaction(tx));
                self.frames.push(CallFrame::Script(Box::new(body_frame)));
                StepOutcome::Yielded
            }
            ControlInst::Parallel { dst, segments } => {
                let mut pf = ParallelFrame {
                    program: Arc::clone(&frame.program),
                    globals: Arc::clone(&frame.globals),
                    parent_upvalues: Arc::clone(&frame.upvalues),
                    parent_locals: frame.locals.clone(),
                    segments,
                    next_idx: 0,
                    last_val: Value::Nil,
                };
                frame.pc += 1;
                frame.resume_dst = Some(dst);
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                match self.advance_parallel(&mut pf) {
                    Ok(true) => StepOutcome::Yielded,
                    Ok(false) => {
                        self.frames.pop();
                        frame.pc -= 1;
                        frame.resume_dst = None;
                        frame.regs[dst.0 as usize] = pf.last_val;
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Spawn { dst, target } => self.exec_spawn(frame, dst, target),
            ControlInst::Select { dst, arms } => self.exec_select(frame, dst, arms),
            ControlInst::Import { path } => {
                let checked = match self.core.fs_context().resolve_path(&path, "import") {
                    Ok(p) => p,
                    Err(e) => return StepOutcome::Err(e),
                };
                let ctx = self.core.task_context.as_ref();
                let source = match self.core.reactor().run_fs(ctx, move || {
                    std::fs::read_to_string(checked.as_path()).map_err(MoraError::from)
                }) {
                    Ok(s) => s,
                    Err(e) => return StepOutcome::Err(e.with_prefix("import error: ")),
                };
                let (mut child_prog, witnesses) = match ParserV3::compile(&source) {
                    Ok(v) => v,
                    Err(e) => return StepOutcome::Err(e),
                };
                let type_errs = check_program_witnesses(&witnesses);
                if !type_errs.is_empty() {
                    let msg = type_errs
                        .iter()
                        .map(|e| format!("line {}: {}", e.line, e.message))
                        .collect::<Vec<_>>()
                        .join("; ");
                    return StepOutcome::Err(MoraError::Typeck(format!(
                        "import '{}' type error(s): {}",
                        path, msg
                    )));
                }
                optimize_program(&mut child_prog, OptLevel::from_env());
                let child_prog = Arc::new(child_prog);
                let entry = child_prog.entry;
                let child_globals = super::continuation::init_globals(&child_prog);
                let child_frame = match materialize_frame(
                    Arc::clone(&child_prog),
                    entry,
                    Arc::clone(&child_globals),
                    Arc::new(Vec::new()),
                    &[],
                ) {
                    Ok(f) => f,
                    Err(e) => return StepOutcome::Err(e),
                };
                let saved_env = self.core.environment.clone();
                frame.pc += 1;
                frame.resume_dst = None;
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                self.frames.push(CallFrame::ImportFinalize {
                    saved_env,
                    child_program: child_prog,
                    child_globals,
                });
                self.frames.push(CallFrame::Script(Box::new(child_frame)));
                StepOutcome::Yielded
            }
            ControlInst::EvalAssert {
                name,
                given,
                expects,
                tolerance,
                replay_path: _,
            } => {
                let given_val = frame.regs[given.0 as usize].clone();
                let expect_vals: Vec<Value> = expects
                    .iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                let name_str = frame.program.symbols.resolve(name);
                match eval_assert(name_str, &given_val, &expect_vals, tolerance) {
                    Ok(()) => StepOutcome::NextInst,
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Assert(cond, msg) => {
                if is_truthy(&frame.regs[cond.0 as usize]) {
                    StepOutcome::NextInst
                } else {
                    StepOutcome::Err(MoraError::Runtime(format!(
                        "assertion failed: {}",
                        frame.regs[msg.0 as usize]
                    )))
                }
            }
            ControlInst::Commit => StepOutcome::NextInst,
            ControlInst::Rollback => {
                StepOutcome::Err(MoraError::Runtime("Transaction rolled back".to_string()))
            }
            ControlInst::RegisterTypeAlias { name, target, dst } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let v = Value::String(target);
                self.store_registered_decl(frame, dst, n, v);
                StepOutcome::NextInst
            }
            ControlInst::RegisterEnum {
                name,
                variants,
                dst,
            } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let evs: Vec<crate::common::EnumVariant> = variants
                    .into_iter()
                    .map(|sym| crate::common::EnumVariant {
                        name: frame.program.symbols.resolve(sym).to_string(),
                        data: None,
                    })
                    .collect();
                let v = make_enum_value(&evs);
                self.store_registered_decl(frame, dst, n, v);
                StepOutcome::NextInst
            }
            ControlInst::RegisterStruct { name, fields, dst } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let fnames: Vec<String> = fields
                    .into_iter()
                    .map(|(sym, _)| frame.program.symbols.resolve(sym).to_string())
                    .collect();
                let v = make_struct_value(&fnames);
                self.store_registered_decl(frame, dst, n, v);
                StepOutcome::NextInst
            }
            ControlInst::RegisterMacro {
                name,
                params,
                body_fn,
                dst,
            } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let pnames: Vec<String> = params
                    .into_iter()
                    .map(|s| frame.program.symbols.resolve(s).to_string())
                    .collect();
                let v = Value::Macro {
                    name: n.clone(),
                    params: pnames,
                    program: Arc::clone(&frame.program),
                    fn_id: body_fn,
                    globals: Arc::clone(&frame.globals),
                };
                self.store_registered_decl(frame, dst, n, v);
                StepOutcome::NextInst
            }
            ControlInst::RegisterTask {
                name,
                params,
                fn_id,
                dst,
            } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let pnames: Vec<String> = params
                    .into_iter()
                    .map(|s| frame.program.symbols.resolve(s).to_string())
                    .collect();
                let v = Value::Task {
                    name: n.clone(),
                    params: pnames,
                    program: Arc::clone(&frame.program),
                    fn_id,
                    globals: Arc::clone(&frame.globals),
                };
                self.store_registered_decl(frame, dst, n, v);
                StepOutcome::NextInst
            }
            ControlInst::KeepAlive(_) => StepOutcome::NextInst,
        }
    }

    fn exec_spawn(
        &mut self,
        frame: &mut ScriptFrame,
        dst: crate::mir::control::VReg,
        target: ControlSpawnTarget,
    ) -> StepOutcome {
        if let Err(err) = self.core.check_cancelled() {
            return StepOutcome::Err(err);
        }
        let base_ctx = self
            .core
            .task_context
            .clone()
            .unwrap_or_else(crate::runtime::scheduler::TaskContext::root);
        let scheduler = Arc::clone(self.core.scheduler());
        let handle = match target {
            ControlSpawnTarget::Call { target, args } => {
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                let callee_val = match target {
                    CalleeTarget::Builtin(id) => ControlConst::BuiltinFn(id).to_value(),
                    CalleeTarget::Fn(fn_id) => {
                        let f = frame.program.func(fn_id);
                        let name = f
                            .name
                            .map(|s| frame.program.symbols.resolve(s).to_string())
                            .unwrap_or_else(|| format!("task#{}", fn_id.0));
                        let params = f
                            .params
                            .iter()
                            .map(|s| frame.program.symbols.resolve(*s).to_string())
                            .collect();
                        Value::Task {
                            name,
                            params,
                            program: Arc::clone(&frame.program),
                            fn_id,
                            globals: Arc::clone(&frame.globals),
                        }
                    }
                    CalleeTarget::Value(r) => frame.regs[r.0 as usize].clone(),
                    CalleeTarget::DynGlobal(sym) => {
                        let name = frame.program.symbols.resolve(sym);
                        if let Some(id) = crate::mir::control::BuiltinFnId::from_name(name) {
                            ControlConst::BuiltinFn(id).to_value()
                        } else if let Some(val) = self.lookup_visible_binding(name) {
                            val
                        } else {
                            return StepOutcome::Err(MoraError::Runtime(format!(
                                "Undefined function or task: {name}"
                            )));
                        }
                    }
                };
                scheduler.spawn_mora_value(callee_val, arg_vals, self.core, &base_ctx)
            }
            ControlSpawnTarget::MethodCall {
                receiver,
                method,
                args,
            } => {
                let recv_val = frame.regs[receiver.0 as usize].clone();
                let method_name = frame.program.symbols.resolve(method).to_string();
                let arg_vals: Vec<Value> = args
                    .iter()
                    .map(|r| frame.regs[r.0 as usize].clone())
                    .collect();
                scheduler.spawn_mora_method(recv_val, method_name, arg_vals, self.core, &base_ctx)
            }
            ControlSpawnTarget::Callable(reg) => {
                let callee_val = frame.regs[reg.0 as usize].clone();
                scheduler.spawn_mora_value(callee_val, Vec::new(), self.core, &base_ctx)
            }
        };
        frame.regs[dst.0 as usize] = Value::TaskHandle(handle);
        StepOutcome::NextInst
    }

    fn exec_select(
        &mut self,
        frame: &mut ScriptFrame,
        dst: crate::mir::control::VReg,
        arms: Vec<ControlSelectArm>,
    ) -> StepOutcome {
        use crate::runtime::scheduler::{SelectBranch, select_arms};
        if let Err(err) = self.core.check_cancelled() {
            return StepOutcome::Err(err);
        }
        let mut branches = Vec::with_capacity(arms.len());
        for arm in &arms {
            match arm {
                ControlSelectArm::Recv { channel, .. } => {
                    let Value::Channel(ch) = &frame.regs[channel.0 as usize] else {
                        return StepOutcome::Err(MoraError::Runtime(format!(
                            "select recv arm requires a channel, got {}",
                            crate::flow::type_name(&frame.regs[channel.0 as usize])
                        )));
                    };
                    branches.push(SelectBranch::Recv(Arc::clone(ch)));
                }
                ControlSelectArm::Send { channel, value, .. } => {
                    let Value::Channel(ch) = &frame.regs[channel.0 as usize] else {
                        return StepOutcome::Err(MoraError::Runtime(format!(
                            "select send arm requires a channel, got {}",
                            crate::flow::type_name(&frame.regs[channel.0 as usize])
                        )));
                    };
                    let val = frame.regs[value.0 as usize].clone();
                    branches.push(SelectBranch::Send(Arc::clone(ch), val));
                }
                ControlSelectArm::Timeout { ms, .. } => {
                    let dur = match crate::runtime::builtins::duration_from_ms(
                        &frame.regs[ms.0 as usize],
                        "select timeout",
                    ) {
                        Ok(d) => d,
                        Err(e) => return StepOutcome::Err(e),
                    };
                    branches.push(SelectBranch::Timeout(dur));
                }
                ControlSelectArm::Default { .. } => branches.push(SelectBranch::Default),
            }
        }

        let start_hint = self.core.random_state.next_u64() as usize;
        let scheduler = Arc::clone(self.core.scheduler());
        let (arm_idx, received_val) = match scheduler.block_on(select_arms(
            branches,
            start_hint,
            self.core.task_context.as_ref(),
        )) {
            Ok(winner) => winner,
            Err(e) => return StepOutcome::Err(e),
        };

        let (body_fn, captures, arm_args) = match &arms[arm_idx] {
            ControlSelectArm::Recv {
                has_binding,
                body_fn,
                captures,
                ..
            } => (
                *body_fn,
                captures,
                if *has_binding {
                    vec![received_val]
                } else {
                    Vec::new()
                },
            ),
            ControlSelectArm::Send {
                body_fn, captures, ..
            }
            | ControlSelectArm::Timeout {
                body_fn, captures, ..
            }
            | ControlSelectArm::Default { body_fn, captures } => (*body_fn, captures, Vec::new()),
        };

        let target_func = frame.program.func(body_fn);
        let upvalues = resolve_captures(captures, &frame.locals, &frame.upvalues, target_func);
        let body_frame = match materialize_frame(
            Arc::clone(&frame.program),
            body_fn,
            Arc::clone(&frame.globals),
            Arc::new(upvalues),
            &arm_args,
        ) {
            Ok(f) => f,
            Err(e) => return StepOutcome::Err(e),
        };
        frame.pc += 1;
        frame.resume_dst = Some(dst);
        self.frames.push(CallFrame::Script(Box::new(frame.clone())));
        self.frames.push(CallFrame::Script(Box::new(body_frame)));
        StepOutcome::Yielded
    }

    fn store_registered_decl(
        &mut self,
        frame: &ScriptFrame,
        dst: Option<crate::mir::control::GlobalSlot>,
        name: String,
        val: Value,
    ) {
        if let Some(slot) = dst {
            frame.globals[slot.0 as usize].lock().0 = val;
        } else {
            self.core.environment.define(name, val, false);
        }
    }
}

/// Collect all visible symbol names across the `Environment` parent chain.
pub fn collect_env_names(env: &Environment) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = Some(env);
    while let Some(layer) = cur {
        for (k, _) in layer.iter() {
            if seen.insert(k.clone()) {
                names.push(k);
            }
        }
        cur = layer.parent.as_deref();
    }
    names
}
