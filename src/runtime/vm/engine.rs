//! v0.110.0: Iterative heap-framed stackful VM executing [`ControlProgram`] with native
//! delimited continuations, zero-recursion TCO, isolated ACID transactions, and
//! deterministic `parallel` worker merges.
//!
//! Invariants:
//! - `self.frames` is the only Mora call stack. A Mora-level call never consumes a Rust
//!   stack frame, so nesting depth is bounded by heap memory, not by the C stack.
//! - A tail call reuses the current frame in place (`ControlTerminator::TailCall`), so a
//!   self-recursive tail-call loop keeps the frame stack length constant.
//! - `parallel` worker batches run on the shared zero-dependency `TaskScheduler`; worker
//!   environments, module globals, and shared closure cells are merged back in
//!   deterministic declaration order under the configured [`MergeStrategy`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::trait_info::{
    TraitInfo, TraitMethodSig, default_impl_method_key, impl_method_key,
};
use crate::common::{BinaryOp, DType, ScalarValue, Span};
use crate::error::{MoraError, MoraResult};
use crate::flow::{eval_binary_tracked, eval_unary_neg_tracked, is_truthy};
use crate::mir::control::{
    CalleeTarget, ControlConst, ControlInst, ControlSelectArm, ControlSliceAxis,
    ControlSpawnTarget, ControlTerminator, FnId, OptLevel, ParallelSegment, PatternTarget, UnaryOp,
    optimize_program,
};
use crate::mir::effect::ambient;
use crate::parser_v3::ParserV3;
use crate::runtime::CoreRuntime;
use crate::value::tensor::RuntimeSliceAxis;
use crate::value::{
    BuiltinKind, EnumDefRuntime, Environment, MergeStrategy, StructDefRuntime, StructFieldRuntime,
    Value,
};

use super::builtins::{
    CallAction, VmFuture, resolve_builtin_call_named, resolve_dyn_call_named,
    resolve_method_call_named, resolve_value_call, resolve_value_call_named,
};
use super::continuation::{
    CallFrame, CapturedContinuation, CapturedFrames, CombinatorFrame, HandleBarrierState,
    ModuleGlobals, ParallelFrame, RegSlot, ScriptFrame, SelectResumeState, TransactionState,
    UpvalueCell, UpvalueValue, clone_isolated_globals, clone_worker_upvalues, materialize_frame,
    materialize_frame_named, materialize_handler_frame, resolve_captures, restore_globals,
    snapshot_globals,
};
use super::value_ops::{
    eval_assert, eval_quasiquote, index_assign_value, index_value_tracked, make_dyn_trait,
    match_pattern,
};

/// P4-2: Reason why a cooperative M:N VM execution suspended and yielded its heap call stack.
pub enum VmYield {
    /// Instruction step-budget quantum exhausted; fiber yields cooperatively so sibling fibers
    /// on the M:N `TaskScheduler` pool never starve on tight CPU loops.
    Preempt,
    /// Fiber suspended awaiting an async operation (`sleep`, `channel`, `select`, `join`,
    /// TCP/TLS I/O) without blocking any OS worker thread.
    AwaitFuture(VmFuture),
}

/// P4-2: Outcome of driving [`VmEngine`] with a cooperative instruction step budget.
pub enum VmOutcome {
    /// Execution finished with a final return value.
    Completed(Value),
    /// Execution yielded its captured heap call stack (`CapturedFrames` — shared with delimited
    /// continuations) for cooperative preemption or non-blocking async I/O resumption.
    Suspended {
        frames: CapturedFrames,
        yield_reason: VmYield,
    },
}

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
    frames: CapturedFrames,
    /// Remaining step budget when running in cooperative M:N mode (`None` = unbounded sync run).
    step_budget: Option<usize>,
    /// Deferred cooperative yield (preemption or async future) emitted by the most recent step.
    pending_yield: Option<VmYield>,
}

impl<'a> VmEngine<'a> {
    /// Create a VM bound to `core`.
    pub fn new(core: &'a mut CoreRuntime) -> Self {
        Self {
            core,
            frames: Vec::new(),
            step_budget: None,
            pending_yield: None,
        }
    }

    /// Create a VM initialized with a previously suspended [`CapturedFrames`] stack.
    pub fn with_frames(core: &'a mut CoreRuntime, frames: CapturedFrames) -> Self {
        Self {
            core,
            frames,
            step_budget: None,
            pending_yield: None,
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
        if self.frames.is_empty() && self.pending_yield.is_none() {
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

    /// P4-2: Drive the current `self.frames` stack cooperatively for up to `max_steps`
    /// instructions, returning [`VmOutcome::Suspended`] on budget exhaustion or async I/O.
    pub fn run_with_budget(&mut self, max_steps: usize) -> MoraResult<VmOutcome> {
        self.step_budget = Some(max_steps.max(1));
        self.run_loop_budgeted()
    }

    /// P4-2: Start executing a root [`CallAction`] cooperatively under `max_steps`.
    pub(crate) fn run_action_with_budget(
        &mut self,
        action: CallAction,
        max_steps: usize,
    ) -> MoraResult<VmOutcome> {
        match action {
            CallAction::Done(v) => Ok(VmOutcome::Completed(v)),
            other => {
                self.apply_action(other)?;
                self.run_with_budget(max_steps)
            }
        }
    }

    /// P4-2: Resume a suspended [`VmEngine`] by delivering a resolved async `value` into the
    /// top captured frame and continuing cooperatively for up to `max_steps` instructions.
    pub(crate) fn resume_with_value(
        &mut self,
        value: Value,
        max_steps: usize,
    ) -> MoraResult<VmOutcome> {
        self.step_budget = Some(max_steps.max(1));
        if let Some(root_val) = self.deliver_value(value)? {
            return Ok(VmOutcome::Completed(root_val));
        }
        self.run_loop_budgeted()
    }

    /// P4-2: Resume a suspended [`VmEngine`] by propagating an async error through active
    /// `transaction` / `ErrorContext` / `NoGradGuard` unwind handlers.
    pub(crate) fn resume_with_error(
        &mut self,
        err: MoraError,
        max_steps: usize,
    ) -> MoraResult<VmOutcome> {
        self.step_budget = Some(max_steps.max(1));
        self.handle_error(err)?;
        self.run_loop_budgeted()
    }

    /// Synchronous execution driver (top-level `execute_program` / REPL): drives any emitted
    /// [`VmYield::AwaitFuture`] inline via `scheduler().block_on(...)` until completion.
    fn run_loop(&mut self) -> MoraResult<Value> {
        self.step_budget = None;
        loop {
            match self.run_loop_budgeted()? {
                VmOutcome::Completed(v) => return Ok(v),
                VmOutcome::Suspended {
                    frames,
                    yield_reason,
                } => {
                    self.frames = frames;
                    match yield_reason {
                        VmYield::Preempt => continue,
                        VmYield::AwaitFuture(fut) => {
                            let res = self.core.scheduler().block_on(fut);
                            match res {
                                Ok(val) => {
                                    if let Some(root_val) = self.deliver_value(val)? {
                                        return Ok(root_val);
                                    }
                                }
                                Err(err) => {
                                    self.handle_error(err)?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn run_loop_budgeted(&mut self) -> MoraResult<VmOutcome> {
        loop {
            if let Some(yield_reason) = self.pending_yield.take() {
                return Ok(VmOutcome::Suspended {
                    frames: std::mem::take(&mut self.frames),
                    yield_reason,
                });
            }
            let Some(top) = self.frames.pop() else {
                return Ok(VmOutcome::Completed(Value::Nil));
            };
            match top {
                CallFrame::Script(mut frame) => match self.step_script(&mut frame) {
                    ScriptStep::Completed(val) => {
                        if let Some(root_val) = self.deliver_value(val)? {
                            return Ok(VmOutcome::Completed(root_val));
                        }
                    }
                    ScriptStep::Suspended => {
                        // `step_script` already pushed sub-frames or set `self.pending_yield`.
                    }
                    ScriptStep::Preempted => {
                        self.frames.push(CallFrame::Script(frame));
                        return Ok(VmOutcome::Suspended {
                            frames: std::mem::take(&mut self.frames),
                            yield_reason: VmYield::Preempt,
                        });
                    }
                    ScriptStep::Err(msg) => {
                        self.handle_error(msg)?;
                    }
                },
                CallFrame::HandleBarrier(_) => {
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
                CallFrame::NoGradGuard => {
                    unreachable!("NoGradGuard popped directly without return delivery")
                }
            }
        }
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
                CallFrame::NoGradGuard => {
                    self.core.autograd.exit_no_grad();
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
            CombinatorFrame::AutogradEval {
                tape_id,
                leaf_nodes,
                scalar_flags,
                return_value,
            } => {
                let tape = self.core.autograd.pop_tape(tape_id).ok_or_else(|| {
                    MoraError::Runtime("autograd: active tape missing on completion".to_string())
                })?;
                let higher_order = self.core.autograd.is_recording();
                let (display_val, grad_tensors) = match &reply {
                    Value::Tensor(t) => {
                        if t.numel() != 1 {
                            return Err(MoraError::Runtime(format!(
                                "grad: differentiated function must return a scalar (0-D or 1-element tensor), got shape {:?}",
                                t.shape
                            )));
                        }
                        let disp = if t.shape.is_empty() && !higher_order {
                            let scalar_v = t.get_linear(0);
                            if t.dtype == DType::F64 {
                                Value::Float(scalar_v)
                            } else {
                                ScalarValue::from_f64_quantized(scalar_v, t.dtype)
                                    .map(Value::Scalar)
                                    .unwrap_or(Value::Float(scalar_v))
                            }
                        } else {
                            Value::Tensor(Arc::clone(t))
                        };
                        let grads = if let Some(out_idx) = t.node_on_tape(tape_id) {
                            if higher_order {
                                tape.backward_tracked(
                                    out_idx,
                                    &leaf_nodes,
                                    &mut self.core.autograd,
                                )?
                            } else {
                                tape.backward(out_idx, &leaf_nodes)?
                            }
                        } else {
                            tape.zero_grads_for_leaves(&leaf_nodes)?
                        };
                        (disp, grads)
                    }
                    Value::Float(_) | Value::Int(_) | Value::Scalar(_) => {
                        (reply.clone(), tape.zero_grads_for_leaves(&leaf_nodes)?)
                    }
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "grad: differentiated function must return a scalar float or 0-D tensor, got {}",
                            crate::flow::type_name(other)
                        )));
                    }
                };
                let mut grad_vals = Vec::with_capacity(grad_tensors.len());
                for (gt, scalar_flag) in grad_tensors.into_iter().zip(scalar_flags) {
                    if let Some(dt) = scalar_flag
                        && !higher_order
                    {
                        let v = gt.get_linear(0);
                        if dt == DType::F64 {
                            grad_vals.push(Value::Float(v));
                        } else {
                            grad_vals.push(
                                ScalarValue::from_f64_quantized(v, dt)
                                    .map(Value::Scalar)
                                    .unwrap_or(Value::Float(v)),
                            );
                        }
                    } else {
                        grad_vals.push(Value::Tensor(gt));
                    }
                }
                let grad_result = if grad_vals.len() == 1 {
                    grad_vals.pop().expect("grad_vals has length 1")
                } else {
                    Value::List(grad_vals.into())
                };
                if return_value {
                    Ok(Some(Value::List(vec![display_val, grad_result].into())))
                } else {
                    Ok(Some(grad_result))
                }
            }
            CombinatorFrame::JvpEval {
                tape_id,
                leaf_nodes,
                leaf_tangents,
                scalar_out_dtype,
            } => {
                let tape = self.core.autograd.pop_tape(tape_id).ok_or_else(|| {
                    MoraError::Runtime("jvp: active tape missing on completion".to_string())
                })?;
                let higher_order = self.core.autograd.is_recording();
                let (primal_val, tangent_val) = match &reply {
                    Value::Tensor(t) => {
                        let tangent_t = if let Some(out_idx) = t.node_on_tape(tape_id) {
                            if higher_order {
                                tape.forward_jvp_tracked(
                                    out_idx,
                                    &leaf_nodes,
                                    &leaf_tangents,
                                    &mut self.core.autograd,
                                )?
                            } else {
                                tape.forward_jvp(out_idx, &leaf_nodes, &leaf_tangents)?
                            }
                        } else {
                            crate::value::tensor::Tensor::zeros(
                                t.shape.clone(),
                                t.dtype,
                                t.device.clone(),
                                false,
                            )?
                        };
                        if t.shape.is_empty() && scalar_out_dtype.is_some() && !higher_order {
                            let pv = t.get_linear(0);
                            let tv = tangent_t.get_linear(0);
                            let p_out = if t.dtype == DType::F64 {
                                Value::Float(pv)
                            } else {
                                ScalarValue::from_f64_quantized(pv, t.dtype)
                                    .map(Value::Scalar)
                                    .unwrap_or(Value::Float(pv))
                            };
                            let t_out = if t.dtype == DType::F64 {
                                Value::Float(tv)
                            } else {
                                ScalarValue::from_f64_quantized(tv, t.dtype)
                                    .map(Value::Scalar)
                                    .unwrap_or(Value::Float(tv))
                            };
                            (p_out, t_out)
                        } else {
                            (Value::Tensor(Arc::clone(t)), Value::Tensor(tangent_t))
                        }
                    }
                    Value::Float(_) | Value::Int(_) | Value::Scalar(_) => {
                        (reply.clone(), Value::Float(0.0))
                    }
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "jvp: differentiated function must return a Tensor or float scalar, got {}",
                            crate::flow::type_name(other)
                        )));
                    }
                };
                Ok(Some(Value::List(vec![primal_val, tangent_val].into())))
            }
            CombinatorFrame::TraceDagEval {
                tape_id,
                leaf_nodes,
            } => {
                let tape = self.core.autograd.pop_tape(tape_id).ok_or_else(|| {
                    MoraError::Runtime("trace_dag: active tape missing on completion".to_string())
                })?;
                let Value::Tensor(t) = &reply else {
                    return Err(MoraError::Runtime(format!(
                        "trace_dag: traced function must return a Tensor, got {}",
                        crate::flow::type_name(&reply)
                    )));
                };
                let Some(out_idx) = t.node_on_tape(tape_id) else {
                    return Err(MoraError::Runtime(
                        "trace_dag: output tensor is disconnected from traced inputs".to_string(),
                    ));
                };
                let dag = crate::runtime::compute_dag::TensorComputeDag::from_tape(
                    &tape,
                    out_idx,
                    &leaf_nodes,
                )?;
                Ok(Some(Value::ComputeDag(Arc::new(dag))))
            }
            CombinatorFrame::StructInit(mut st) => {
                st.fields.insert(st.current_field, reply);
                if st.pending_defaults.is_empty() {
                    let final_consts = super::builtins::finalize_struct_const_args(
                        &st.def,
                        &st.const_args,
                        &st.fields,
                    )?;
                    let field_order = st.def.fields.iter().map(|f| f.name.clone()).collect();
                    return Ok(Some(Value::StructInstance {
                        type_name: st.def.name.clone(),
                        fields: st.fields,
                        field_order,
                        const_args: final_consts,
                        def: st.def,
                    }));
                }
                let (next_field, next_fn_id) = st.pending_defaults.remove(0);
                let thunk = Value::Task {
                    name: format!("{}::<default>", st.def.name),
                    params: Vec::new(),
                    program: Arc::clone(&st.def.program),
                    fn_id: next_fn_id,
                    globals: Arc::clone(&st.def.globals),
                };
                st.current_field = next_field;
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::StructInit(st)));
                let action = resolve_value_call(thunk, Vec::new(), self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
            }
            CombinatorFrame::BoolNot => Ok(Some(Value::Bool(!is_truthy(&reply)))),
            CombinatorFrame::SelectResume(st) => {
                let (arm_idx, received_val) = match reply {
                    Value::List(items) if items.len() == 2 => {
                        let idx = match &items[0] {
                            Value::Int(i) => *i as usize,
                            _ => {
                                return Err(MoraError::Runtime(
                                    "internal select resume received invalid arm index".into(),
                                ));
                            }
                        };
                        (idx, items[1].clone())
                    }
                    _ => {
                        return Err(MoraError::Runtime(
                            "internal select resume received malformed winner payload".into(),
                        ));
                    }
                };
                let (body_fn, captures, arm_args) = match &st.arms[arm_idx] {
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
                    | ControlSelectArm::Default { body_fn, captures } => {
                        (*body_fn, captures, Vec::new())
                    }
                };
                let target_func = st.program.func(body_fn);
                let upvalues = resolve_captures(
                    captures,
                    &st.parent_locals,
                    &st.parent_upvalues,
                    target_func,
                );
                let body_frame = materialize_frame(
                    Arc::clone(&st.program),
                    body_fn,
                    Arc::clone(&st.globals),
                    Arc::new(upvalues),
                    &arm_args,
                )?;
                self.frames.push(CallFrame::Script(Box::new(body_frame)));
                Ok(None)
            }
            CombinatorFrame::WatermarkCallbacks {
                final_result,
                mut pending,
            } => {
                let _ = reply;
                if pending.is_empty() {
                    return Ok(Some(final_result));
                }
                let (next_cb, next_event) = pending.remove(0);
                self.frames
                    .push(CallFrame::Combinator(CombinatorFrame::WatermarkCallbacks {
                        final_result,
                        pending,
                    }));
                let action = resolve_value_call(next_cb, vec![Value::Dict(next_event)], self.core)?;
                self.apply_action_chained(action)?;
                Ok(None)
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
                CallAction::AwaitFuture(fut) => {
                    self.pending_yield = Some(VmYield::AwaitFuture(fut));
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
                    self.frames.push(CallFrame::Combinator(*combinator));
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
                CallAction::AwaitFuture(fut) => {
                    self.pending_yield = Some(VmYield::AwaitFuture(fut));
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
                    self.frames.push(CallFrame::Combinator(*combinator));
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
                CallFrame::NoGradGuard => {
                    self.core.autograd.exit_no_grad();
                }
                CallFrame::Combinator(
                    CombinatorFrame::AutogradEval { tape_id, .. }
                    | CombinatorFrame::JvpEval { tape_id, .. }
                    | CombinatorFrame::TraceDagEval { tape_id, .. },
                ) => {
                    let _ = self.core.autograd.pop_tape(tape_id);
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
        for f in &drained {
            if matches!(f, CallFrame::NoGradGuard) {
                self.core.autograd.exit_no_grad();
            }
        }
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
        for f in &restored {
            if matches!(f, CallFrame::NoGradGuard) {
                self.core.autograd.enter_no_grad();
            }
        }
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

        let strategies = self.core.environment.merge_strategies().cloned();
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
    Preempted,
    Err(MoraError),
}

impl<'a> VmEngine<'a> {
    fn step_script(&mut self, frame: &mut ScriptFrame) -> ScriptStep {
        let mut cancel_budget: u32 = 0;
        loop {
            if let Some(ref mut rem) = self.step_budget {
                if *rem == 0 {
                    return ScriptStep::Preempted;
                }
                *rem -= 1;
            }
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
                        // `frame` cleanly, and pushed callee frames or set `self.pending_yield`!
                        return ScriptStep::Suspended;
                    }
                    StepOutcome::Err(e) => return ScriptStep::Err(e),
                }
            } else {
                let term = frame.func().blocks[block_idx].terminator.clone();
                match term {
                    ControlTerminator::Jump { target, args } => {
                        if args.len() == 1 {
                            let p = frame.func().blocks[target.0 as usize].params[0];
                            let slot = frame.regs[args[0].0 as usize].clone();
                            frame.regs[p.0 as usize] = slot;
                        } else if !args.is_empty() {
                            let slots: Vec<RegSlot> = args
                                .iter()
                                .map(|r| frame.regs[r.0 as usize].clone())
                                .collect();
                            let target_params =
                                frame.func().blocks[target.0 as usize].params.clone();
                            for (p, s) in target_params.into_iter().zip(slots) {
                                frame.regs[p.0 as usize] = s;
                            }
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
                        // Zero-boxing inline truthiness check on 16B RegSlot!
                        let truthy = frame.regs[cond.0 as usize].is_truthy();
                        let (target, arg_regs) = if truthy {
                            (then_bb, then_args)
                        } else {
                            (else_bb, else_args)
                        };
                        if arg_regs.len() == 1 {
                            let p = frame.func().blocks[target.0 as usize].params[0];
                            let slot = frame.regs[arg_regs[0].0 as usize].clone();
                            frame.regs[p.0 as usize] = slot;
                        } else if !arg_regs.is_empty() {
                            let slots: Vec<RegSlot> = arg_regs
                                .iter()
                                .map(|r| frame.regs[r.0 as usize].clone())
                                .collect();
                            let target_params =
                                frame.func().blocks[target.0 as usize].params.clone();
                            for (p, s) in target_params.into_iter().zip(slots) {
                                frame.regs[p.0 as usize] = s;
                            }
                        }
                        frame.block_id = target;
                        frame.pc = 0;
                    }
                    ControlTerminator::Return(opt_r) | ControlTerminator::Halt(opt_r) => {
                        let val = opt_r.map(|r| frame.get_val(r)).unwrap_or(Value::Nil);
                        return ScriptStep::Completed(val);
                    }
                    ControlTerminator::Perform {
                        effect,
                        args,
                        resume_bb,
                    } => {
                        let effect_name = frame.program.symbols.resolve(effect).to_string();
                        let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
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
                        let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                        let action = match self.resolve_callee(frame, &target, arg_vals) {
                            Ok(a) => a,
                            Err(e) => return ScriptStep::Err(e),
                        };
                        match action {
                            CallAction::Done(val) => return ScriptStep::Completed(val),
                            CallAction::AwaitFuture(fut) => {
                                self.pending_yield = Some(VmYield::AwaitFuture(fut));
                                return ScriptStep::Suspended;
                            }
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
        self.resolve_callee_named(frame, target, arg_vals, Vec::new())
    }

    fn resolve_callee_named(
        &mut self,
        frame: &ScriptFrame,
        target: &CalleeTarget,
        arg_vals: Vec<Value>,
        named_vals: Vec<(String, Value)>,
    ) -> MoraResult<CallAction> {
        match target {
            CalleeTarget::Builtin(id) => {
                resolve_builtin_call_named(*id, self.core, arg_vals, named_vals)
            }
            CalleeTarget::Fn(fn_id) => {
                let next_frame = materialize_frame_named(
                    Arc::clone(&frame.program),
                    *fn_id,
                    Arc::clone(&frame.globals),
                    Arc::new(Vec::new()),
                    &arg_vals,
                    &named_vals,
                )?;
                Ok(CallAction::PushFrame {
                    frame: next_frame,
                    error_prefix: None,
                })
            }
            CalleeTarget::Value(r) => {
                let val = frame.get_val(*r);
                resolve_value_call_named(val, arg_vals, named_vals, self.core)
            }
            CalleeTarget::DynGlobal(sym) => {
                let name = frame.program.symbols.resolve(*sym);
                resolve_dyn_call_named(
                    name,
                    arg_vals,
                    named_vals,
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
                frame.set_val(dst, val);
                StepOutcome::NextInst
            }
            CallAction::AwaitFuture(fut) => {
                frame.pc += 1;
                frame.resume_dst = Some(dst);
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                self.pending_yield = Some(VmYield::AwaitFuture(fut));
                StepOutcome::Yielded
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
                            frame.set_val(dst, val);
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
                // Zero-boxing inline constant materialization into 16B RegSlot.
                let slot = match c {
                    ControlConst::Nil => RegSlot::Nil,
                    ControlConst::Bool(b) => RegSlot::Bool(b),
                    ControlConst::Int(i) => RegSlot::Int(i),
                    ControlConst::Float(f) => RegSlot::Float(f),
                    other => RegSlot::from_value(other.to_value()),
                };
                frame.set_slot(dst, slot);
                StepOutcome::NextInst
            }
            ControlInst::Copy(dst, src) => {
                frame.regs[dst.0 as usize] = frame.regs[src.0 as usize].clone();
                StepOutcome::NextInst
            }
            ControlInst::Unary(dst, op, src) => {
                match op {
                    UnaryOp::Truthy => {
                        frame.set_slot(dst, RegSlot::Bool(frame.regs[src.0 as usize].is_truthy()));
                        StepOutcome::NextInst
                    }
                    UnaryOp::Not => {
                        frame.set_slot(dst, RegSlot::Bool(!frame.regs[src.0 as usize].is_truthy()));
                        StepOutcome::NextInst
                    }
                    UnaryOp::Neg => {
                        // Zero-boxing inline fast paths for Int / Float negation.
                        match &frame.regs[src.0 as usize] {
                            RegSlot::Int(i) => match i.checked_neg() {
                                Some(neg) => {
                                    frame.set_slot(dst, RegSlot::Int(neg));
                                    return StepOutcome::NextInst;
                                }
                                None => {
                                    return StepOutcome::Err(MoraError::Runtime(format!(
                                        "integer overflow negating {i}"
                                    )));
                                }
                            },
                            RegSlot::Float(f) => {
                                frame.set_slot(dst, RegSlot::Float(-*f));
                                return StepOutcome::NextInst;
                            }
                            _ => {}
                        }
                        let v = frame.get_val(src);
                        if let Some((func, all_args)) = super::builtins::resolve_operator_impl(
                            self.core,
                            &v,
                            "Neg",
                            "neg",
                            Vec::new(),
                        ) {
                            return match resolve_value_call(func, all_args, self.core) {
                                Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                                Err(e) => StepOutcome::Err(e),
                            };
                        }
                        match eval_unary_neg_tracked(&v, Some(&mut self.core.autograd)) {
                            Ok(negated) => {
                                frame.set_val(dst, negated);
                                StepOutcome::NextInst
                            }
                            Err(e) => StepOutcome::Err(e),
                        }
                    }
                }
            }
            ControlInst::Binary(dst, lhs, op, rhs) => {
                // P4-2: Zero-allocation inline unboxed fast paths for primitive Int / Float / Bool.
                match (&frame.regs[lhs.0 as usize], &frame.regs[rhs.0 as usize]) {
                    (&RegSlot::Int(a), &RegSlot::Int(b)) => {
                        let fast = match op {
                            BinaryOp::Add => crate::flow::numeric::int::arith(&BinaryOp::Add, a, b)
                                .map(RegSlot::Int)
                                .map(Some),
                            BinaryOp::Sub => crate::flow::numeric::int::arith(&BinaryOp::Sub, a, b)
                                .map(RegSlot::Int)
                                .map(Some),
                            BinaryOp::Mul => crate::flow::numeric::int::arith(&BinaryOp::Mul, a, b)
                                .map(RegSlot::Int)
                                .map(Some),
                            BinaryOp::Div => crate::flow::numeric::int::arith(&BinaryOp::Div, a, b)
                                .map(RegSlot::Int)
                                .map(Some),
                            BinaryOp::Mod => crate::flow::numeric::int::arith(&BinaryOp::Mod, a, b)
                                .map(RegSlot::Int)
                                .map(Some),
                            BinaryOp::Equal => Ok(Some(RegSlot::Bool(a == b))),
                            BinaryOp::NotEqual => Ok(Some(RegSlot::Bool(a != b))),
                            BinaryOp::Less => Ok(Some(RegSlot::Bool(a < b))),
                            BinaryOp::LessEqual => Ok(Some(RegSlot::Bool(a <= b))),
                            BinaryOp::Greater => Ok(Some(RegSlot::Bool(a > b))),
                            BinaryOp::GreaterEqual => Ok(Some(RegSlot::Bool(a >= b))),
                            _ => Ok(None),
                        };
                        match fast {
                            Ok(Some(slot)) => {
                                frame.set_slot(dst, slot);
                                return StepOutcome::NextInst;
                            }
                            Err(e) => return StepOutcome::Err(MoraError::Runtime(e.to_string())),
                            Ok(None) => {}
                        }
                    }
                    (&RegSlot::Float(a), &RegSlot::Float(b)) => {
                        let fast = match op {
                            BinaryOp::Add => Some(RegSlot::Float(a + b)),
                            BinaryOp::Sub => Some(RegSlot::Float(a - b)),
                            BinaryOp::Mul => Some(RegSlot::Float(a * b)),
                            BinaryOp::Div => Some(RegSlot::Float(a / b)),
                            BinaryOp::Mod => Some(RegSlot::Float(if b == 0.0 {
                                f64::NAN
                            } else {
                                a - (a / b).trunc() * b
                            })),
                            BinaryOp::Pow => Some(RegSlot::Float(a.powf(b))),
                            BinaryOp::Equal => Some(RegSlot::Bool(a == b)),
                            BinaryOp::NotEqual => Some(RegSlot::Bool(a != b)),
                            BinaryOp::Less => Some(RegSlot::Bool(a < b)),
                            BinaryOp::LessEqual => Some(RegSlot::Bool(a <= b)),
                            BinaryOp::Greater => Some(RegSlot::Bool(a > b)),
                            BinaryOp::GreaterEqual => Some(RegSlot::Bool(a >= b)),
                            _ => None,
                        };
                        if let Some(slot) = fast {
                            frame.set_slot(dst, slot);
                            return StepOutcome::NextInst;
                        }
                    }
                    (&RegSlot::Bool(a), &RegSlot::Bool(b)) => {
                        let fast = match op {
                            BinaryOp::Equal => Some(RegSlot::Bool(a == b)),
                            BinaryOp::NotEqual => Some(RegSlot::Bool(a != b)),
                            _ => None,
                        };
                        if let Some(slot) = fast {
                            frame.set_slot(dst, slot);
                            return StepOutcome::NextInst;
                        }
                    }
                    _ => {}
                }

                let l = frame.get_val(lhs);
                let r = frame.get_val(rhs);
                // P4-1A: Stackful trait-driven operator overloading on nominal types.
                let trait_op = match op {
                    BinaryOp::Add => Some(("Add", "add", false)),
                    BinaryOp::Sub => Some(("Sub", "sub", false)),
                    BinaryOp::Mul => Some(("Mul", "mul", false)),
                    BinaryOp::Div => Some(("Div", "div", false)),
                    BinaryOp::Mod => Some(("Mod", "rem", false)),
                    BinaryOp::Pow => Some(("Pow", "pow", false)),
                    BinaryOp::MatMul => Some(("MatMul", "matmul", false)),
                    BinaryOp::Equal => Some(("Eq", "eq", false)),
                    BinaryOp::NotEqual => Some(("Eq", "eq", true)),
                    _ => None,
                };
                if let Some((tr_name, m_name, negate)) = trait_op
                    && let Some((func, all_args)) = super::builtins::resolve_operator_impl(
                        self.core,
                        &l,
                        tr_name,
                        m_name,
                        vec![r.clone()],
                    )
                {
                    let action = if negate {
                        Ok(CallAction::PushCombinator {
                            combinator: Box::new(CombinatorFrame::BoolNot),
                            callee: func,
                            args: all_args,
                        })
                    } else {
                        resolve_value_call(func, all_args, self.core)
                    };
                    return match action {
                        Ok(a) => self.suspend_and_dispatch(frame, dst, a),
                        Err(e) => StepOutcome::Err(e),
                    };
                }
                match eval_binary_tracked(l, &op, r, Some(&mut self.core.autograd)) {
                    Ok(v) => {
                        frame.set_val(dst, v);
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(MoraError::Runtime(e.to_string())),
                }
            }
            ControlInst::ListLit(dst, items) => {
                let list: crate::value::list::List =
                    items.into_iter().map(|r| frame.get_val(r)).collect();
                frame.set_val(dst, Value::List(list));
                StepOutcome::NextInst
            }
            ControlInst::DictLit(dst, pairs) => {
                let mut map = HashMap::with_capacity(pairs.len());
                for (sym, r) in pairs {
                    let key = frame.program.symbols.resolve(sym).to_string();
                    map.insert(key, frame.get_val(r));
                }
                frame.set_val(dst, Value::Dict(map));
                StepOutcome::NextInst
            }
            ControlInst::Quasiquote(dst, segs) => match eval_quasiquote(&segs, &frame.regs) {
                Ok(v) => {
                    frame.set_val(dst, v);
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
                frame.set_val(
                    dst,
                    Value::Closure {
                        params,
                        program: Arc::clone(&frame.program),
                        fn_id,
                        globals: Arc::clone(&frame.globals),
                        upvalues: Arc::new(upvalues),
                    },
                );
                StepOutcome::NextInst
            }
            ControlInst::MakeDynTrait {
                dst,
                src,
                trait_name,
                generics,
            } => {
                let tname = frame.program.symbols.resolve(trait_name).to_string();
                let val = frame.get_val(src);
                frame.set_val(dst, make_dyn_trait(val, &tname, &generics));
                StepOutcome::NextInst
            }
            ControlInst::LoadLocal(dst, slot) => {
                // Zero-boxing unshared local load directly into 16B RegSlot!
                frame.set_slot(dst, frame.locals[slot.0 as usize].get_slot());
                StepOutcome::NextInst
            }
            ControlInst::StoreLocal(slot, src) => {
                // Zero-boxing unshared local store directly from 16B RegSlot!
                let s = frame.regs[src.0 as usize].clone();
                frame.locals[slot.0 as usize].set_slot(s);
                StepOutcome::NextInst
            }
            ControlInst::FreshLocalCell(slot) => {
                // P5-1A: Per-iteration loop closure scoping — re-box shared local cells so
                // previously spawned closures keep their own iteration's independent UpvalueCell,
                // while unshared 16B unboxed slots remain zero-allocation!
                let cell = &mut frame.locals[slot.0 as usize];
                if matches!(cell, crate::runtime::vm::continuation::LocalCell::Shared(_)) {
                    *cell = crate::runtime::vm::continuation::LocalCell::Shared(Arc::new(
                        crate::common::sync::Mutex::new(Value::Nil),
                    ));
                }
                StepOutcome::NextInst
            }
            ControlInst::IsMissingArg(dst, slot) => {
                let missing = matches!(frame.locals[slot.0 as usize].get(), Value::MissingArg);
                frame.set_slot(dst, RegSlot::Bool(missing));
                StepOutcome::NextInst
            }
            ControlInst::LoadUpvalue(dst, slot) => {
                frame.set_val(dst, frame.upvalues[slot.0 as usize].get());
                StepOutcome::NextInst
            }
            ControlInst::StoreUpvalue(slot, src) => {
                let val = frame.get_val(src);
                match frame.upvalues[slot.0 as usize].set(val) {
                    Ok(()) => StepOutcome::NextInst,
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::LoadGlobal(dst, slot) => {
                let val = frame.globals[slot.0 as usize].lock().0.clone();
                frame.set_val(dst, val);
                StepOutcome::NextInst
            }
            ControlInst::StoreGlobal(slot, src) => {
                let val = frame.get_val(src);
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
                frame.set_val(dst, val);
                StepOutcome::NextInst
            }
            ControlInst::DefineDyn(sym, src, exported) => {
                let name = frame.program.symbols.resolve(sym).to_string();
                let val = frame.get_val(src);
                self.core.environment.define(name, val, exported);
                StepOutcome::NextInst
            }
            ControlInst::AssignDyn(sym, src) => {
                let name = frame.program.symbols.resolve(sym);
                let val = frame.get_val(src);
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
                let obj_val = frame.get_val(obj);
                let idx_val = frame.get_val(idx);
                match index_value_tracked(&obj_val, &idx_val, Some(&mut self.core.autograd)) {
                    Ok(v) => {
                        frame.set_val(dst, v);
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::IndexAssign(dst, obj, idx, val) => {
                let mut container = frame.get_val(obj);
                let idx_val = frame.get_val(idx);
                let elem_val = frame.get_val(val);
                match index_assign_value(&mut container, &idx_val, &elem_val) {
                    Ok(()) => {
                        frame.set_val(dst, container);
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::TensorSlice { dst, target, axes } => {
                let target_val = frame.get_val(target);
                let mut rt_axes = Vec::with_capacity(axes.len());
                for ax in &axes {
                    match ax {
                        ControlSliceAxis::Index(r) => {
                            let i = match value_to_slice_isize(&frame.get_val(*r)) {
                                Ok(v) => v,
                                Err(e) => return StepOutcome::Err(e),
                            };
                            rt_axes.push(RuntimeSliceAxis::Index(i));
                        }
                        ControlSliceAxis::Range { start, stop, step } => {
                            let s = match start {
                                Some(r) => match value_to_slice_isize(&frame.get_val(*r)) {
                                    Ok(v) => Some(v),
                                    Err(e) => return StepOutcome::Err(e),
                                },
                                None => None,
                            };
                            let e = match stop {
                                Some(r) => match value_to_slice_isize(&frame.get_val(*r)) {
                                    Ok(v) => Some(v),
                                    Err(e) => return StepOutcome::Err(e),
                                },
                                None => None,
                            };
                            let st = match step {
                                Some(r) => match value_to_slice_isize(&frame.get_val(*r)) {
                                    Ok(v) => Some(v),
                                    Err(e) => return StepOutcome::Err(e),
                                },
                                None => None,
                            };
                            rt_axes.push(RuntimeSliceAxis::Range {
                                start: s,
                                stop: e,
                                step: st,
                            });
                        }
                        ControlSliceAxis::Ellipsis => rt_axes.push(RuntimeSliceAxis::Ellipsis),
                        ControlSliceAxis::NewAxis => rt_axes.push(RuntimeSliceAxis::NewAxis),
                    }
                }
                let sliced = match &target_val {
                    Value::Tensor(t) => t.slice_nd(&rt_axes, Some(&mut self.core.autograd)),
                    other => slice_sequence_value(other, &rt_axes),
                };
                match sliced {
                    Ok(v) => {
                        frame.set_val(dst, v);
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Call {
                dst,
                target,
                args,
                named_args,
            } => {
                let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                let named_vals: Vec<(String, Value)> = named_args
                    .iter()
                    .map(|(sym, r)| {
                        (
                            frame.program.symbols.resolve(*sym).to_string(),
                            frame.get_val(*r),
                        )
                    })
                    .collect();
                match self.resolve_callee_named(frame, &target, arg_vals, named_vals) {
                    Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::CallGeneric {
                dst,
                target,
                const_args,
                args,
                named_args,
            } => {
                let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                let named_vals: Vec<(String, Value)> = named_args
                    .iter()
                    .map(|(sym, r)| {
                        (
                            frame.program.symbols.resolve(*sym).to_string(),
                            frame.get_val(*r),
                        )
                    })
                    .collect();
                let const_vals: Vec<(String, i64)> = const_args
                    .iter()
                    .map(|(sym, v)| (frame.program.symbols.resolve(*sym).to_string(), *v))
                    .collect();
                let action = match &target {
                    CalleeTarget::Value(r) => match frame.get_val(*r) {
                        Value::StructType(def) => {
                            super::builtins::construct_struct_instance_with_consts(
                                def, const_vals, arg_vals, named_vals,
                            )
                        }
                        other => resolve_value_call_named(other, arg_vals, named_vals, self.core),
                    },
                    CalleeTarget::DynGlobal(sym) => {
                        let name = frame.program.symbols.resolve(*sym);
                        if let Some((type_name, member)) = name.split_once("::") {
                            if member == "new"
                                && let Some(def) = self.core.struct_registry.get(type_name).cloned()
                            {
                                super::builtins::construct_struct_instance_with_consts(
                                    def, const_vals, arg_vals, named_vals,
                                )
                            } else if let Some(edef) =
                                self.core.enum_registry.get(type_name).cloned()
                                && edef.variants.iter().any(|(v, _)| v == member)
                            {
                                super::builtins::construct_enum_variant_with_consts(
                                    &edef, member, const_vals, arg_vals, named_vals,
                                )
                            } else {
                                self.resolve_callee_named(frame, &target, arg_vals, named_vals)
                            }
                        } else if let Some(def) = self.core.struct_registry.get(name).cloned() {
                            super::builtins::construct_struct_instance_with_consts(
                                def, const_vals, arg_vals, named_vals,
                            )
                        } else {
                            self.resolve_callee_named(frame, &target, arg_vals, named_vals)
                        }
                    }
                    _ => self.resolve_callee_named(frame, &target, arg_vals, named_vals),
                };
                match action {
                    Ok(a) => self.suspend_and_dispatch(frame, dst, a),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::GetConstGeneric { dst, src, name } => {
                let cname = frame.program.symbols.resolve(name);
                match frame.regs[src.0 as usize].as_boxed() {
                    Some(
                        Value::StructInstance {
                            type_name,
                            const_args,
                            ..
                        }
                        | Value::EnumVariant {
                            enum_name: type_name,
                            const_args,
                            ..
                        },
                    ) => {
                        if let Some((_, val)) = const_args.iter().find(|(k, _)| k == cname) {
                            frame.set_slot(dst, RegSlot::Int(*val));
                            StepOutcome::NextInst
                        } else {
                            StepOutcome::Err(MoraError::Runtime(format!(
                                "const generic parameter '{cname}' is not bound on instance of '{type_name}'"
                            )))
                        }
                    }
                    _ => {
                        let other = frame.get_val(src);
                        StepOutcome::Err(MoraError::Runtime(format!(
                            "cannot read const generic '{cname}' from non-nominal receiver {}",
                            crate::flow::type_name(&other)
                        )))
                    }
                }
            }
            ControlInst::ModuleCall {
                dst,
                module,
                method,
                args,
                named_args,
            } => {
                let mname = frame.program.symbols.resolve(method).to_string();
                let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                let named_vals: Vec<(String, Value)> = named_args
                    .iter()
                    .map(|(sym, r)| {
                        (
                            frame.program.symbols.resolve(*sym).to_string(),
                            frame.get_val(*r),
                        )
                    })
                    .collect();
                match resolve_method_call_named(
                    self.core,
                    Value::Builtin(module),
                    &mname,
                    arg_vals,
                    named_vals,
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
                named_args,
            } => {
                let mname = frame.program.symbols.resolve(method).to_string();
                let recv = frame.get_val(receiver);
                let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                let named_vals: Vec<(String, Value)> = named_args
                    .iter()
                    .map(|(sym, r)| {
                        (
                            frame.program.symbols.resolve(*sym).to_string(),
                            frame.get_val(*r),
                        )
                    })
                    .collect();
                match resolve_method_call_named(
                    self.core,
                    recv,
                    &mname,
                    arg_vals,
                    named_vals,
                    Span::default(),
                ) {
                    Ok(action) => self.suspend_and_dispatch(frame, dst, action),
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Pipe { dst, lhs, rhs } => {
                let l = frame.get_val(lhs);
                let r = frame.get_val(rhs);
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
                let scrutinee = frame.get_val(val);
                if let Some(bindings) = match_pattern(&scrutinee, &pattern) {
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
                    frame.set_slot(dst_ok, RegSlot::Bool(true));
                } else {
                    frame.set_slot(dst_ok, RegSlot::Bool(false));
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
                        frame.set_val(dst, pf.last_val);
                        StepOutcome::NextInst
                    }
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Spawn { dst, target } => self.exec_spawn(frame, dst, target),
            ControlInst::Select { dst, arms } => self.exec_select(frame, dst, arms),
            ControlInst::NoGrad {
                dst,
                body_fn,
                body_captures,
            } => {
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
                self.core.autograd.enter_no_grad();
                frame.pc += 1;
                frame.resume_dst = Some(dst);
                self.frames.push(CallFrame::Script(Box::new(frame.clone())));
                self.frames.push(CallFrame::NoGradGuard);
                self.frames.push(CallFrame::Script(Box::new(body_frame)));
                StepOutcome::Yielded
            }
            ControlInst::Import { path } => {
                let child_prog = if let Some(precompiled) =
                    self.core.precompiled_modules.get(&path).cloned()
                {
                    precompiled
                } else {
                    let source = if let Some(std_src) = crate::resolve_std_module(&path) {
                        std_src.to_string()
                    } else {
                        let checked = match self.core.fs_context().resolve_path(&path, "import") {
                            Ok(p) => p,
                            Err(e) => return StepOutcome::Err(e),
                        };
                        let ctx = self.core.task_context.as_ref();
                        match self.core.reactor().run_fs(ctx, move || {
                            std::fs::read_to_string(checked.as_path()).map_err(MoraError::from)
                        }) {
                            Ok(s) => s,
                            Err(e) => return StepOutcome::Err(e.with_prefix("import error: ")),
                        }
                    };
                    let (mut compiled, witnesses) = match ParserV3::compile(&source) {
                        Ok(v) => v,
                        Err(e) => return StepOutcome::Err(e),
                    };
                    let type_errs = crate::typeck::check_mir::check_program_witnesses(&witnesses);
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
                    optimize_program(&mut compiled, OptLevel::from_env());
                    Arc::new(compiled)
                };
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
                let given_val = frame.get_val(given);
                let expect_vals: Vec<Value> = expects.iter().map(|r| frame.get_val(*r)).collect();
                let name_str = frame.program.symbols.resolve(name);
                match eval_assert(name_str, &given_val, &expect_vals, tolerance) {
                    Ok(()) => StepOutcome::NextInst,
                    Err(e) => StepOutcome::Err(e),
                }
            }
            ControlInst::Assert(cond, msg) => {
                if frame.regs[cond.0 as usize].is_truthy() {
                    StepOutcome::NextInst
                } else {
                    StepOutcome::Err(MoraError::Runtime(format!(
                        "assertion failed: {}",
                        frame.get_val(msg)
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
                type_params,
                const_params,
                const_shape_bindings,
                variants,
                dst,
            } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let tp_names: Vec<String> = type_params
                    .into_iter()
                    .map(|s| frame.program.symbols.resolve(s).to_string())
                    .collect();
                let cp_names: Vec<String> = const_params
                    .into_iter()
                    .map(|s| frame.program.symbols.resolve(s).to_string())
                    .collect();
                let csb: Vec<(String, String, usize, usize)> = const_shape_bindings
                    .into_iter()
                    .map(|(vs, cs, pidx, aidx)| {
                        (
                            frame.program.symbols.resolve(vs).to_string(),
                            frame.program.symbols.resolve(cs).to_string(),
                            pidx,
                            aidx,
                        )
                    })
                    .collect();
                let evs: Vec<(String, Vec<String>)> = variants
                    .into_iter()
                    .map(|(sym, payload_tys)| {
                        (frame.program.symbols.resolve(sym).to_string(), payload_tys)
                    })
                    .collect();
                let def = Arc::new(EnumDefRuntime {
                    name: n.clone(),
                    type_params: tp_names,
                    const_params: cp_names,
                    const_shape_bindings: csb,
                    variants: evs,
                });
                self.core.register_enum_def(Arc::clone(&def));
                self.store_registered_decl(frame, dst, n, Value::EnumType(def));
                StepOutcome::NextInst
            }
            ControlInst::RegisterStruct {
                name,
                type_params,
                const_params,
                const_shape_bindings,
                fields,
                dst,
            } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let tp_names: Vec<String> = type_params
                    .into_iter()
                    .map(|s| frame.program.symbols.resolve(s).to_string())
                    .collect();
                let cp_names: Vec<String> = const_params
                    .into_iter()
                    .map(|s| frame.program.symbols.resolve(s).to_string())
                    .collect();
                let csb: Vec<(String, usize, usize)> = const_shape_bindings
                    .into_iter()
                    .map(|(s, fi, ai)| (frame.program.symbols.resolve(s).to_string(), fi, ai))
                    .collect();
                let rt_fields: Vec<StructFieldRuntime> = fields
                    .into_iter()
                    .map(|f| StructFieldRuntime {
                        name: frame.program.symbols.resolve(f.name).to_string(),
                        type_name: f.type_name,
                        default_fn: f.default_fn,
                        tags: f.tags,
                    })
                    .collect();
                let def = Arc::new(StructDefRuntime {
                    name: n.clone(),
                    type_params: tp_names,
                    const_params: cp_names,
                    const_shape_bindings: csb,
                    fields: rt_fields,
                    program: Arc::clone(&frame.program),
                    globals: Arc::clone(&frame.globals),
                });
                self.core.register_struct_def(Arc::clone(&def));
                self.store_registered_decl(frame, dst, n, Value::StructType(def));
                StepOutcome::NextInst
            }
            ControlInst::RegisterTrait {
                name,
                type_params,
                parents,
                methods,
                dst,
            } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let tp_names: Vec<String> = type_params
                    .iter()
                    .map(|s| frame.program.symbols.resolve(*s).to_string())
                    .collect();
                let parent_names: Vec<String> = parents
                    .iter()
                    .map(|s| frame.program.symbols.resolve(*s).to_string())
                    .collect();
                let mut method_sigs = Vec::with_capacity(methods.len());
                for m in methods {
                    let mname = frame.program.symbols.resolve(m.name).to_string();
                    let params: Vec<(String, Option<String>)> = m
                        .params
                        .iter()
                        .map(|(ps, ty)| {
                            (frame.program.symbols.resolve(*ps).to_string(), ty.clone())
                        })
                        .collect();
                    if let Some(def_fn) = m.default_fn {
                        let f = frame.program.func(def_fn);
                        let pnames: Vec<String> = f
                            .params
                            .iter()
                            .map(|s| frame.program.symbols.resolve(*s).to_string())
                            .collect();
                        let task_val = Value::Task {
                            name: format!("{n}::{mname}"),
                            params: pnames,
                            program: Arc::clone(&frame.program),
                            fn_id: def_fn,
                            globals: Arc::clone(&frame.globals),
                        };
                        self.core.register_trait_default_method(
                            n.clone(),
                            mname.clone(),
                            task_val.clone(),
                            m.has_self,
                        );
                        let def_key = default_impl_method_key(&n, &tp_names, &mname);
                        self.core.environment.define(def_key, task_val, false);
                    }
                    method_sigs.push(TraitMethodSig {
                        name: mname,
                        params,
                        return_type: m.return_type,
                        has_self: m.has_self,
                        has_default: m.default_fn.is_some(),
                    });
                }
                self.core.register_trait(
                    n.clone(),
                    TraitInfo {
                        name: n.clone(),
                        type_params: tp_names,
                        parents: parent_names,
                        methods: method_sigs,
                    },
                );
                let v = Value::String(format!("<trait {n}>"));
                self.store_registered_decl(frame, dst, n, v);
                StepOutcome::NextInst
            }
            ControlInst::RegisterImpl {
                trait_name,
                trait_generics,
                target_type,
                target_generics,
                methods,
            } => {
                let target_name = frame.program.symbols.resolve(target_type).to_string();
                if let Some(tr_sym) = trait_name {
                    let tr_name = frame.program.symbols.resolve(tr_sym).to_string();
                    self.core
                        .register_trait_impl_pair(tr_name.clone(), target_name.clone());
                    for m in methods {
                        let mname = frame.program.symbols.resolve(m.name).to_string();
                        let f = frame.program.func(m.fn_id);
                        let pnames: Vec<String> = f
                            .params
                            .iter()
                            .map(|s| frame.program.symbols.resolve(*s).to_string())
                            .collect();
                        let task_val = Value::Task {
                            name: format!("{target_name}::{mname}"),
                            params: pnames,
                            program: Arc::clone(&frame.program),
                            fn_id: m.fn_id,
                            globals: Arc::clone(&frame.globals),
                        };
                        self.core.register_trait_impl_method(
                            tr_name.clone(),
                            target_name.clone(),
                            mname.clone(),
                            task_val.clone(),
                            m.has_self,
                        );
                        let key = impl_method_key(
                            &tr_name,
                            &trait_generics,
                            &target_name,
                            &target_generics,
                            &mname,
                        );
                        self.core.environment.define(key, task_val, false);
                    }
                } else {
                    for m in methods {
                        let mname = frame.program.symbols.resolve(m.name).to_string();
                        let f = frame.program.func(m.fn_id);
                        let pnames: Vec<String> = f
                            .params
                            .iter()
                            .map(|s| frame.program.symbols.resolve(*s).to_string())
                            .collect();
                        let task_val = Value::Task {
                            name: format!("{target_name}::{mname}"),
                            params: pnames,
                            program: Arc::clone(&frame.program),
                            fn_id: m.fn_id,
                            globals: Arc::clone(&frame.globals),
                        };
                        self.core.register_inherent_method(
                            target_name.clone(),
                            mname,
                            task_val,
                            m.has_self,
                        );
                    }
                }
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
            ControlInst::RegisterKernel { name, program, dst } => {
                let n = frame.program.symbols.resolve(name).to_string();
                let compiled = Arc::new(
                    crate::runtime::gpu::kernel_compiler::CompiledGpuKernel::from_program(*program),
                );
                self.store_registered_decl(frame, dst, n, Value::GpuKernel(compiled));
                StepOutcome::NextInst
            }
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
            ControlSpawnTarget::Call {
                target,
                args,
                named_args,
            } => {
                let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                let named_vals: Vec<(String, Value)> = named_args
                    .iter()
                    .map(|(sym, r)| {
                        (
                            frame.program.symbols.resolve(*sym).to_string(),
                            frame.get_val(*r),
                        )
                    })
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
                    CalleeTarget::Value(r) => frame.get_val(r),
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
                scheduler
                    .spawn_mora_value_named(callee_val, arg_vals, named_vals, self.core, &base_ctx)
            }
            ControlSpawnTarget::MethodCall {
                receiver,
                method,
                args,
                named_args,
            } => {
                let recv_val = frame.get_val(receiver);
                let method_name = frame.program.symbols.resolve(method).to_string();
                let arg_vals: Vec<Value> = args.iter().map(|r| frame.get_val(*r)).collect();
                let named_vals: Vec<(String, Value)> = named_args
                    .iter()
                    .map(|(sym, r)| {
                        (
                            frame.program.symbols.resolve(*sym).to_string(),
                            frame.get_val(*r),
                        )
                    })
                    .collect();
                scheduler.spawn_mora_method_named(
                    recv_val,
                    method_name,
                    arg_vals,
                    named_vals,
                    self.core,
                    &base_ctx,
                )
            }
            ControlSpawnTarget::Callable(reg) => {
                let callee_val = frame.get_val(reg);
                scheduler.spawn_mora_value(callee_val, Vec::new(), self.core, &base_ctx)
            }
        };
        frame.set_val(dst, Value::TaskHandle(handle));
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
                    let Some(Value::Channel(ch)) = frame.regs[channel.0 as usize].as_boxed() else {
                        let got = frame.get_val(*channel);
                        return StepOutcome::Err(MoraError::Runtime(format!(
                            "select recv arm requires a channel, got {}",
                            crate::flow::type_name(&got)
                        )));
                    };
                    branches.push(SelectBranch::Recv(Arc::clone(ch)));
                }
                ControlSelectArm::Send { channel, value, .. } => {
                    let Some(Value::Channel(ch)) = frame.regs[channel.0 as usize].as_boxed() else {
                        let got = frame.get_val(*channel);
                        return StepOutcome::Err(MoraError::Runtime(format!(
                            "select send arm requires a channel, got {}",
                            crate::flow::type_name(&got)
                        )));
                    };
                    let val = frame.get_val(*value);
                    branches.push(SelectBranch::Send(Arc::clone(ch), val));
                }
                ControlSelectArm::Timeout { ms, .. } => {
                    let ms_val = frame.get_val(*ms);
                    let dur =
                        match crate::runtime::builtins::duration_from_ms(&ms_val, "select timeout")
                        {
                            Ok(d) => d,
                            Err(e) => return StepOutcome::Err(e),
                        };
                    branches.push(SelectBranch::Timeout(dur));
                }
                ControlSelectArm::Default { .. } => branches.push(SelectBranch::Default),
            }
        }

        let start_hint = self.core.random_state.next_u64() as usize;
        let ctx = self.core.task_context.clone();
        let select_state = SelectResumeState {
            program: Arc::clone(&frame.program),
            globals: Arc::clone(&frame.globals),
            parent_locals: frame.locals.clone(),
            parent_upvalues: Arc::clone(&frame.upvalues),
            arms,
        };
        frame.pc += 1;
        frame.resume_dst = Some(dst);
        self.frames.push(CallFrame::Script(Box::new(frame.clone())));
        self.frames
            .push(CallFrame::Combinator(CombinatorFrame::SelectResume(
                Box::new(select_state),
            )));
        self.pending_yield = Some(VmYield::AwaitFuture(Box::pin(async move {
            let (arm_idx, received_val) = select_arms(branches, start_hint, ctx.as_ref()).await?;
            Ok(Value::List(
                vec![Value::Int(arm_idx as i64), received_val].into(),
            ))
        })));
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

fn value_to_slice_isize(val: &Value) -> MoraResult<isize> {
    match val {
        Value::Int(i) => Ok(*i as isize),
        Value::Float(f) if f.is_finite() && f.fract() == 0.0 => Ok(*f as isize),
        Value::Scalar(s) if s.dtype().is_int() => Ok(s.as_f64() as isize),
        other => Err(MoraError::Runtime(format!(
            "slice index must be an integer, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

/// Normalize Python-style `(start, stop, step)` bounds over sequence length `len`.
fn normalize_seq_slice(
    len: usize,
    start: Option<isize>,
    stop: Option<isize>,
    step: Option<isize>,
) -> MoraResult<Vec<usize>> {
    let st = step.unwrap_or(1);
    if st == 0 {
        return Err(MoraError::Runtime("slice step cannot be zero".to_string()));
    }
    let n = len as isize;
    let mut indices = Vec::new();
    if st > 0 {
        let mut s = start.unwrap_or(0);
        let mut e = stop.unwrap_or(n);
        if s < 0 {
            s += n;
        }
        if e < 0 {
            e += n;
        }
        let s = s.clamp(0, n);
        let e = e.clamp(0, n);
        let mut cur = s;
        while cur < e {
            indices.push(cur as usize);
            cur += st;
        }
    } else {
        let mut s = start.unwrap_or(n - 1);
        let mut e = stop.unwrap_or(-n - 1);
        if s < 0 {
            s += n;
        }
        if stop.is_some() && e < 0 {
            e += n;
        }
        let s = s.clamp(-1, n - 1);
        let e = e.clamp(-1, n - 1);
        let mut cur = s;
        while cur > e {
            indices.push(cur as usize);
            cur += st;
        }
    }
    Ok(indices)
}

fn slice_sequence_value(target: &Value, axes: &[RuntimeSliceAxis]) -> MoraResult<Value> {
    if axes.len() != 1 {
        return Err(MoraError::Runtime(format!(
            "multi-dimensional slicing requires a Tensor, got {}",
            crate::flow::type_name(target)
        )));
    }
    match (target, &axes[0]) {
        (Value::List(list), RuntimeSliceAxis::Range { start, stop, step }) => {
            let idxs = normalize_seq_slice(list.len(), *start, *stop, *step)?;
            let out: crate::value::list::List = idxs.into_iter().map(|i| list[i].clone()).collect();
            Ok(Value::List(out))
        }
        (Value::String(s), RuntimeSliceAxis::Range { start, stop, step }) => {
            let chars: Vec<char> = s.chars().collect();
            let idxs = normalize_seq_slice(chars.len(), *start, *stop, *step)?;
            let out: String = idxs.into_iter().map(|i| chars[i]).collect();
            Ok(Value::String(out))
        }
        _ => Err(MoraError::Runtime(format!(
            "cannot slice value of type {}",
            crate::flow::type_name(target)
        ))),
    }
}
