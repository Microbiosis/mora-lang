//! v0.59: MirInst handler functions — one per variant.
//!
//! Extracted from the monolithic `run_mir` match loop so that both the
//! linear interpreter (`run_mir`) and the DAG interpreter (`run_dag`)
//! share the same instruction-level logic.
//!
//! Each handler follows a consistent pattern:
//! - Pure value ops: write to `regs[dst]`, return `Flow::Continue`
//! - Side effects: modify `env`/`interp`, return `Flow::Continue` or error
//! - Control flow: return `Flow::Jump(label)` or `Flow::Return(value)`

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::BinaryOp;
use crate::flow::eval_binary;
use crate::mir::expr::{MirOrchestrateKind, MirPregelConfig};
use crate::mir::host::MirHost;
use crate::mir::vm::{index_value, run_mir, self_match_pattern};
use crate::mir::{MirFunction, MirInst, Reg};
use crate::runtime::types::{TraitInfo, TraitMethodSig};
use crate::value::{Environment, Value};

use super::vm::index_assign_value;
use super::vm::is_truthy;
use super::vm::value_to_string;

/// What the linear interpreter should do after a handler runs.
#[derive(Debug)]
pub enum Flow {
    /// Advance pc by 1 (normal).
    Continue,
    /// Jump to the given label.
    Jump(usize),
    /// Return from the function with the given value.
    Return(Value),
    /// v0.70: Vote to halt. In Pregel context, the current agent signals
    /// "I'm done — don't reschedule me unless someone sends me a message."
    /// In linear context, behaves like Return.
    Halt(Option<Value>),
}

pub type HandlerResult = Result<Flow, String>;

// ═══════════════════════════════════════════════════════════════════
// Pure value instructions (write to regs[dst])
// ═══════════════════════════════════════════════════════════════════

pub fn h_const(regs: &mut [Value], dst: Reg, value: &Value) {
    regs[dst] = value.clone();
}

pub fn h_var(regs: &mut [Value], dst: Reg, name: &str, env: &Environment) {
    regs[dst] = env.get(name).unwrap_or(Value::Nil);
}

pub fn h_binary_op(
    regs: &mut [Value],
    dst: Reg,
    lhs: Reg,
    op: &BinaryOp,
    rhs: Reg,
) -> Result<(), String> {
    let lv = regs[lhs].clone();
    let rv = regs[rhs].clone();
    regs[dst] = eval_binary(lv, op, rv)?;
    Ok(())
}

pub fn h_call(
    regs: &mut [Value],
    dst: Reg,
    name: &str,
    args: &[Reg],
    task_registry: &HashMap<&str, (&[String], &MirFunction)>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(), String> {
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    let result = if let Some((params, body)) = task_registry.get(name) {
        let mut child_env = env.clone();
        for (i, param) in params.iter().enumerate() {
            let val = arg_vals.get(i).cloned().unwrap_or(Value::Nil);
            child_env.define(param.clone(), val, false);
        }
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存（task body 借自指令表）
        run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?
    } else {
        interp.mir_call_function(name, arg_vals)?
    };
    regs[dst] = result;
    Ok(())
}

pub fn h_list_lit(regs: &mut [Value], dst: Reg, items: &[Reg]) {
    let vals: Vec<Value> = items.iter().map(|r| regs[*r].clone()).collect();
    regs[dst] = Value::List(vals);
}

pub fn h_dict_lit(regs: &mut [Value], dst: Reg, entries: &[(String, Reg)]) {
    let mut map = HashMap::new();
    for (k, v) in entries {
        map.insert(k.clone(), regs[*v].clone());
    }
    regs[dst] = Value::Dict(map);
}

pub fn h_index(regs: &mut [Value], dst: Reg, obj: Reg, idx: Reg) -> Result<(), String> {
    let obj_val = regs[obj].clone();
    let idx_val = regs[idx].clone();
    regs[dst] = index_value(&obj_val, &idx_val)?;
    Ok(())
}

pub fn h_index_assign(regs: &mut [Value], obj: Reg, idx: Reg, val: Reg) -> Result<(), String> {
    let mut obj_val = regs[obj].clone();
    let idx_val = regs[idx].clone();
    let val_val = regs[val].clone();
    index_assign_value(&mut obj_val, &idx_val, &val_val)?;
    regs[obj] = obj_val;
    Ok(())
}

pub fn h_method_call(
    regs: &mut [Value],
    dst: Reg,
    receiver: Reg,
    method: &str,
    args: &[Reg],
    interp: &mut dyn MirHost,
) -> Result<(), String> {
    let recv_val = regs[receiver].clone();
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    regs[dst] = interp.mir_call_method(recv_val, method, arg_vals)?;
    Ok(())
}

pub fn h_pipe(
    regs: &mut [Value],
    dst: Reg,
    lhs: Reg,
    rhs: Reg,
    interp: &mut dyn MirHost,
) -> Result<(), String> {
    let lhs_val = regs[lhs].clone();
    let rhs_val = regs[rhs].clone();
    regs[dst] = interp.call_value(&rhs_val, vec![lhs_val])?;
    Ok(())
}

pub fn h_prompt(regs: &mut [Value], dst: Reg, parts: &[Reg]) {
    let mut s = String::new();
    for r in parts {
        s.push_str(&value_to_string(&regs[*r]));
    }
    regs[dst] = Value::String(s);
}

pub fn h_closure(
    regs: &mut [Value],
    dst: Reg,
    params: &[String],
    body: &MirFunction,
    interp: &dyn MirHost,
) {
    let closure = Value::Closure {
        params: params.to_vec(),
        env: crate::value::EnvRef::from_arc_mutex(interp.environment()),
        mir_body: Arc::new(body.clone()),
    };
    regs[dst] = closure;
}

pub fn h_dyn_trait(
    regs: &mut [Value],
    dst: Reg,
    src: Reg,
    trait_name: &str,
    trait_generics: &[String],
) {
    let data = regs[src].clone();
    regs[dst] = Value::TraitObject {
        for_generics: Vec::new(),
        trait_generics: trait_generics.to_vec(),
        for_type: String::new(),
        trait_name: trait_name.to_string(),
        data: Box::new(data),
    };
}

// ═══════════════════════════════════════════════════════════════════
// Side-effect instructions (modify env / interp state)
// ═══════════════════════════════════════════════════════════════════

pub fn h_define(env: &mut Environment, name: &str, regs: &[Value], src: Reg) {
    env.define(name.to_string(), regs[src].clone(), false);
}

pub fn h_assign(env: &mut Environment, name: &str, regs: &[Value], src: Reg) {
    env.assign(name, regs[src].clone());
}

pub fn h_type_alias(env: &mut Environment, name: &str, target: &str) {
    env.define(name.to_string(), Value::String(target.to_string()), false);
}

pub fn h_enum_def(env: &mut Environment, name: &str, variants: &[crate::common::EnumVariant]) {
    let mut map = HashMap::new();
    for v in variants {
        map.insert(v.name.clone(), Value::String(v.name.clone()));
    }
    env.define(name.to_string(), Value::Dict(map), false);
}

pub fn h_struct_def(env: &mut Environment, name: &str, fields: &[crate::common::StructField]) {
    let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
    env.define(
        name.to_string(),
        Value::Dict(HashMap::from([(
            "__struct_fields__".to_string(),
            Value::List(
                field_names
                    .iter()
                    .map(|s| Value::String(s.clone()))
                    .collect(),
            ),
        )])),
        false,
    );
}

pub fn h_import(interp: &mut dyn MirHost, env: &mut Environment, path: &str) -> Result<(), String> {
    interp.mir_import(path, env)?;
    Ok(())
}

pub fn h_with_config(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    bindings: &[(String, Reg)],
    body: &MirFunction,
    jit: bool,
) -> Result<(), String> {
    let binding_vals: Vec<(String, Value)> = bindings
        .iter()
        .map(|(k, r)| (k.clone(), regs[*r].clone()))
        .collect();
    interp.mir_with_config(&binding_vals)?;
    let mut child_env = env.clone();

    let _result = if jit {
        // v0.75.43: copy-and-patch JIT（零 LLVM）— 直接编译 MirFunction，
        // 未覆盖指令回落解释器（run_jit Err → run_mir）。
        match crate::mir::jit::run_jit(body, interp, &mut child_env) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "JIT compilation failed ({}), falling back to MIR interpreter",
                    e
                );
                // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?
            }
        }
    } else {
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存
        run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?
    };
    interp.mir_restore_config();
    Ok(())
}

pub fn h_macro_def(env: &mut Environment, name: &str, params: &[String]) {
    env.define(
        name.to_string(),
        Value::Macro {
            name: name.to_string(),
            params: params.to_vec(),
        },
        false,
    );
}

/// v0.68: Unified isolated-block execution.
///
/// Clones `env`, runs `body` in the clone, then merges the child's changes
/// back into the parent using the interpreter's current merge strategies.
/// Returns the body's final value AND any merge conflicts (currently
/// discarded by callers; reserved for future observability hooks).
fn run_isolated(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(crate::value::Value, Vec<crate::value::Conflict>), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let result = run_mir(&Arc::new((*body).clone()), interp, &mut child_env)?;
    let strategies = interp.current_merge_strategies();
    let conflicts = match strategies.as_ref() {
        Some(s) => env.merge_from_with_strategies(
            &child_env,
            s,
            &crate::value::MergeStrategy::LastWriteWins,
        ),
        None => {
            env.merge_from(&child_env, &crate::value::MergeStrategy::LastWriteWins);
            Vec::new()
        }
    };
    Ok((result, conflicts))
}

pub fn h_transaction(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    compensation: &MirFunction,
) -> Result<Flow, String> {
    match run_isolated(interp, env, body) {
        Ok(_) => Ok(Flow::Continue),
        Err(_) => {
            let mut comp_env = env.clone();
            // v0.75.9: 包裹 Arc 走全局 DAG 缓存
            if let Err(e) = run_mir(&Arc::new((*compensation).clone()), interp, &mut comp_env) {
                eprintln!("[warn] transaction compensation failed: {}", e);
            }
            Err("Transaction rolled back".to_string())
        }
    }
}

pub fn h_send(
    interp: &mut dyn MirHost,
    regs: &[Value],
    value: Reg,
    target: &str,
) -> Result<(), String> {
    let val = regs[value].clone();
    // v0.69: Push to dynamic_sends buffer. h_orchestrate flushes this into
    // the BSP engine's pending_sends before each super-step, so the message
    // reaches its target_node in the next super-step.
    // v0.70: Removed crossbeam worker_channels fallback (was dead code).
    interp.dynamic_sends().push(crate::checkpoint::SendTask {
        target_node: target.to_string(),
        input: val,
    });
    Ok(())
}

/// v0.71: Contribute a value to a per-super-step aggregator.
/// Currently a no-op when no Pregel run is active (aggregators are BSP-only).
pub fn h_aggregate(
    interp: &mut dyn MirHost,
    regs: &[Value],
    value: Reg,
    name: &str,
) -> Result<(), String> {
    // Aggregator contribution requires direct engine access.
    // For now, the BSP engine exposes aggregator values as channels
    // (aggregator_<name>) at the end of each step. We just record the
    // contribution locally so it's available if a worker reads it back.
    let val = regs[value].clone();
    let _ = (interp, name, val);
    Ok(())
}

pub fn h_worker(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    let _ = run_isolated(interp, env, body)?;
    Ok(())
}

pub fn h_observe(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    // v0.68: Bug fix — was discarding child_env mutations. Now merges
    // via run_isolated so observability side-effects (trace vars, span
    // markers) are actually visible.
    let _ = run_isolated(interp, env, body)?;
    Ok(())
}

pub fn h_span(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    // v0.68: Bug fix — same as h_observe.
    let _ = run_isolated(interp, env, body)?;
    Ok(())
}

pub fn h_save(
    interp: &mut dyn MirHost,
    regs: &[Value],
    path: Reg,
    value: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let value_str = value_to_string(&regs[value]);
    interp.mir_call_function(
        "file.write_text",
        vec![Value::String(path_str), Value::String(value_str)],
    )?;
    Ok(())
}

pub fn h_load(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content = interp.mir_call_function("file.read_text", vec![Value::String(path_str)])?;
    env.define(var.to_string(), content, false);
    Ok(())
}

pub fn h_read_file(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content = interp.mir_call_function("file.read_text", vec![Value::String(path_str)])?;
    env.define(var.to_string(), content, false);
    Ok(())
}

pub fn h_write_file(
    interp: &mut dyn MirHost,
    regs: &[Value],
    path: Reg,
    content: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_str = value_to_string(&regs[content]);
    interp.mir_call_function(
        "file.write_text",
        vec![Value::String(path_str), Value::String(content_str)],
    )?;
    Ok(())
}

pub fn h_append_file(
    interp: &mut dyn MirHost,
    regs: &[Value],
    path: Reg,
    content: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_str = value_to_string(&regs[content]);
    interp.mir_call_function(
        "file.append_text",
        vec![Value::String(path_str), Value::String(content_str)],
    )?;
    Ok(())
}

pub fn h_read_bytes_file(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let bytes = interp.mir_call_function("file.read_bytes", vec![Value::String(path_str)])?;
    env.define(var.to_string(), bytes, false);
    Ok(())
}

pub fn h_write_bytes_file(
    interp: &mut dyn MirHost,
    regs: &[Value],
    path: Reg,
    content: Reg,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_val = regs[content].clone();
    interp.mir_call_function(
        "file.write_bytes",
        vec![Value::String(path_str), content_val],
    )?;
    Ok(())
}

pub fn h_trait_def(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    name: &str,
    parents: &[String],
    methods: &[crate::mir::expr::MirTraitMethod],
    method_bodies: &[MirFunction],
) -> Result<(), String> {
    let sigs: Vec<TraitMethodSig> = methods
        .iter()
        .map(|m| TraitMethodSig {
            name: m.name.clone(),
            params: m
                .params
                .iter()
                .map(|p| (p.name.clone(), p.type_hint.as_ref().map(|t| t.name())))
                .collect(),
            return_type: m.return_type.clone(),
            has_self: m.params.first().map(|p| p.name == "self").unwrap_or(false),
        })
        .collect();
    Arc::make_mut(interp.trait_registry()).insert(
        name.to_string(),
        TraitInfo {
            name: name.to_string(),
            parents: parents.to_vec(),
            methods: sigs,
        },
    );
    for (m, _body) in methods.iter().zip(method_bodies.iter()) {
        if let Some(mfn) = &m.body {
            let key = crate::runtime::types::default_impl_method_key(
                name,
                &Vec::<String>::new(),
                &m.name,
            );
            env.define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: Arc::new(mfn.clone()),
                },
                false,
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn h_impl_def(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    trait_name: &str,
    trait_generics: &[String],
    for_type: &str,
    for_generics: &[String],
    methods: &[crate::mir::expr::MirFnDef],
    method_bodies: &[MirFunction],
) -> Result<(), String> {
    Arc::make_mut(interp.impl_table())
        .entry(trait_name.to_string())
        .or_default()
        .push(for_type.to_string());
    for (m, _body) in methods.iter().zip(method_bodies.iter()) {
        if let Some(mfn) = &m.body {
            let key = crate::runtime::types::impl_method_key(
                trait_name,
                trait_generics,
                for_type,
                for_generics,
                &m.name,
            );
            env.define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: Arc::new(mfn.clone()),
                },
                false,
            );
        }
    }
    Ok(())
}

pub fn h_orchestrate(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    input_var: &str,
    result_var: &str,
    kind: &MirOrchestrateKind,
) -> Result<(), String> {
    use crate::pregel::MirPregelEngine;
    match kind {
        MirOrchestrateKind::Pregel {
            agents,
            edges,
            state_schema,
            checkpoint,
            interrupt_points,
            adjacency,
        } => {
            let config = MirPregelConfig {
                agents: agents.clone(),
                edges: edges.clone(),
                state_schema: state_schema.clone(),
                checkpoint: checkpoint.clone(),
                interrupt_points: interrupt_points.clone(),
                adjacency: adjacency.clone(),
                aggregators: Vec::new(),
                master_compute: None,
            };
            let mut engine = MirPregelEngine::new(config);

            // v0.66: Wire PersistRuntime's checkpoint saver into the engine
            // so the auto-save block in BSP ADVANCE actually persists.
            if let Some(saver) = interp.checkpoint_saver() {
                engine = engine.with_checkpoint_saver(saver);
            }

            // v0.63: Resume from checkpoint if available
            let thread_id = "pregel"; // matches build_checkpoint default
            if let Ok(Some(cp)) = interp.load_checkpoint(thread_id) {
                engine.restore_checkpoint(&cp);
            }

            // Only init channels if starting fresh (not restored)
            if engine.current_step == 0 {
                let input_val = env.get(input_var).unwrap_or(Value::Nil);
                let mut initial = HashMap::new();
                initial.insert(input_var.to_string(), input_val);
                engine.init_channels(initial);
            }

            // v0.62: Collect conflicts via callback for exposure in result
            let captured: std::sync::Arc<parking_lot::Mutex<Vec<crate::value::Conflict>>> =
                std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
            let cb_captured = captured.clone();
            engine = engine.with_conflict_callback(std::sync::Arc::new(move |c| {
                cb_captured.lock().push(c.clone());
                true // always continue
            }));

            // v0.69: Flush dynamic_sends into engine before run
            let pending = std::mem::take(interp.dynamic_sends());
            engine.flush_pending_sends(pending);
            let result = engine.run(interp)?;

            // v0.62: Expose conflicts as a structured list
            let conflict_list: Vec<Value> = captured
                .lock()
                .iter()
                .map(|c| {
                    let mut d: HashMap<String, Value> = HashMap::new();
                    d.insert("key".into(), Value::String(c.key.clone()));
                    d.insert("parent_value".into(), c.parent_value.clone());
                    d.insert("child_value".into(), c.child_value.clone());
                    Value::Dict(d)
                })
                .collect();
            env.define(
                format!("{}_conflicts", result_var),
                Value::List(conflict_list),
                false,
            );
            env.define(result_var.to_string(), result, false);
            Ok(())
        }
        MirOrchestrateKind::Sequential { agents } => {
            // v0.75.34: Sequential orchestrate 执行 — 按声明顺序逐个执行
            // agent 的 prelowered task_body，前一个 agent 的输出作为下一个
            // 的输入（pipeline），最终结果写入 result_var。
            // 输入注入沿用 pregel 契约：`input` 变量（input_var 的当前值）。
            let mut input_val = env.get(input_var).unwrap_or(Value::Nil);
            let mut result = Value::Nil;
            for agent in agents {
                if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                    return Err(format!(
                        "orchestrate: agent '{}' has empty task_body (lowering missing)",
                        agent.name
                    ));
                }
                // 每 agent 独立 env（克隆父级）：input 定义在私有副本上，
                // 避免跨 agent 污染；副作用写回见下方合并。
                let mut agent_env = env.clone();
                agent_env.define("input".to_string(), input_val.clone(), false);
                agent_env.clock.tick(&agent.name);
                result = crate::mir::vm::run_mir(
                    &std::sync::Arc::new(agent.task_body.clone()),
                    interp,
                    &mut agent_env,
                )?;
                // agent 期间 define 的变量合并回父 env（与 pregel 引擎
                // reconcile_outcome 的写回语义一致）。
                for (name, val) in agent_env.iter() {
                    if env.get(&name).is_none() {
                        env.define(name, val, false);
                    }
                }
                input_val = result.clone();
            }
            env.define(result_var.to_string(), result, false);
            Ok(())
        }
        other => Err(format!("orchestrate({:?}) not yet supported", other)),
    }
}

pub fn h_eval(
    regs: &[Value],
    env: &mut Environment,
    name: &str,
    given_reg: Reg,
    expects: &[Reg],
    tolerance: &Option<f64>,
) -> Result<(), String> {
    let given_val = regs[given_reg].clone();
    env.define("given".to_string(), given_val.clone(), false);
    for &expect_reg in expects {
        let expect_val = regs[expect_reg].clone();
        let pass = if let Some(tol) = tolerance {
            match (&given_val, &expect_val) {
                (Value::Float(g), Value::Float(e)) => (g - e).abs() <= *tol,
                (Value::Int(g), Value::Int(e)) => (*g as f64 - *e as f64).abs() <= *tol,
                _ => given_val == expect_val,
            }
        } else {
            given_val == expect_val
        };
        if !pass {
            return Err(format!(
                "eval '{}': assertion failed: given {:?}, expected {:?}",
                name, given_val, expect_val
            ));
        }
    }
    eprintln!("eval '{}': PASSED", name);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn h_skill_def(
    env: &mut Environment,
    name: &str,
    description: &Option<String>,
    version: &Option<String>,
    requires: &[String],
    tasks: &[crate::mir::expr::MirSkillTask],
    task_bodies: &[MirFunction],
    verify: &Option<crate::mir::expr::MirSkillVerify>,
    verify_body: &Option<MirFunction>,
) {
    let mut meta = HashMap::new();
    meta.insert("name".to_string(), Value::String(name.to_string()));
    if let Some(d) = description {
        meta.insert("description".to_string(), Value::String(d.clone()));
    }
    if let Some(v) = version {
        meta.insert("version".to_string(), Value::String(v.clone()));
    }
    meta.insert(
        "requires".to_string(),
        Value::List(requires.iter().map(|r| Value::String(r.clone())).collect()),
    );
    for (task, _body) in tasks.iter().zip(task_bodies.iter()) {
        if let Some(mfn) = &task.body {
            meta.insert(
                task.name.clone(),
                Value::Task {
                    name: task.name.clone(),
                    params: task.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: Arc::new(mfn.clone()),
                },
            );
        }
    }
    if let Some(v) = verify {
        let vp: Vec<String> = v.params.iter().map(|p| p.name.clone()).collect();
        let empty = MirFunction {
            params: vp.clone(),
            body: Vec::new(),
            n_regs: 0,
        };
        let verify_mir = v.body.clone().unwrap_or(empty);
        let _ = verify_body;
        meta.insert(
            "verify".to_string(),
            Value::Task {
                name: "verify".to_string(),
                params: vp,
                mir_body: Arc::new(verify_mir),
            },
        );
    }
    env.define(name.to_string(), Value::Dict(meta), false);
}

pub fn h_prompt_section(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let _ = run_mir(&Arc::new((*body).clone()), interp, &mut child_env);
    Ok(())
}

pub fn h_document_section(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
) -> Result<(), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let _ = run_mir(&Arc::new((*body).clone()), interp, &mut child_env);
    Ok(())
}

pub fn h_match_expr(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &mut [Value],
    val: Reg,
    arms: &[(String, Option<Reg>, Box<MirFunction>, Reg)],
) -> Result<(), String> {
    let val_val = regs[val].clone();
    let mut matched = false;
    for (pat_str, cond_reg, arm_func, output_reg) in arms {
        if self_match_pattern(&val_val, pat_str, cond_reg.as_ref().map(|r| &regs[*r]), env) {
            // v0.75.9: 包裹 Arc 走全局 DAG 缓存（arm body 借自指令表）
            let result = run_mir(&Arc::new((**arm_func).clone()), interp, env)?;
            regs[*output_reg] = result;
            matched = true;
            break;
        }
    }
    if !matched && let Some((_pat, _cond, _func, output_reg)) = arms.first() {
        regs[*output_reg] = Value::Nil;
    }
    Ok(())
}

// v0.75.26: h_stream_for 已删（StreamFor 死原语移除，见 MirInst 定义注释）。

// ═══════════════════════════════════════════════════════════════════
// Control flow handlers
// ═══════════════════════════════════════════════════════════════════

pub fn h_jump(target: usize) -> Flow {
    Flow::Jump(target)
}

pub fn h_jump_if(regs: &[Value], cond: Reg, target: usize) -> Flow {
    if is_truthy(&regs[cond]) {
        Flow::Jump(target)
    } else {
        Flow::Continue
    }
}

pub fn h_jump_if_not(regs: &[Value], cond: Reg, target: usize) -> Flow {
    if !is_truthy(&regs[cond]) {
        Flow::Jump(target)
    } else {
        Flow::Continue
    }
}

pub fn h_return(regs: &[Value], value: Option<Reg>) -> Flow {
    Flow::Return(value.map_or(Value::Nil, |r| regs[r].clone()))
}

/// v0.70: Vote to halt (Pregel semantics). In a BSP context the engine
/// marks the current vertex as Halted; it won't be rescheduled unless a
/// Send arrives. In a linear context, equivalent to return.
pub fn h_halt(regs: &[Value], value: Option<Reg>) -> Flow {
    Flow::Halt(value.map(|r| regs[r].clone()))
}

pub fn h_break(target: usize) -> Flow {
    Flow::Jump(target)
}

pub fn h_continue(target: usize) -> Flow {
    Flow::Jump(target)
}

// ─── MirInst metadata — single source of truth ──────────────────────
// v0.59: dst(), input_regs(), is_effect() + dispatch() all in one file.
// All matches are exhaustive — compiler enforces updates on new variants.

impl MirInst {
    /// Destination register, if this instruction produces a value.
    pub fn dst(&self) -> Option<Reg> {
        match self {
            MirInst::Const(r, _) => Some(*r),
            MirInst::Var(r, _) => Some(*r),
            MirInst::BinaryOp(r, _, _, _) => Some(*r),
            MirInst::Call(r, _, _) => Some(*r),
            MirInst::MethodCall(r, _, _, _) => Some(*r),
            MirInst::ListLit(r, _) => Some(*r),
            MirInst::DictLit(r, _) => Some(*r),
            MirInst::Index(r, _, _) => Some(*r),
            MirInst::IndexAssign(r, _, _) => Some(*r),
            MirInst::Pipe(r, _, _) => Some(*r),
            MirInst::Prompt(r, _) => Some(*r),
            MirInst::MatchExpr { arms, .. } => arms.last().map(|a| a.3),
            MirInst::Closure { dst, .. } => Some(*dst),
            MirInst::DynTrait { dst, .. } => Some(*dst),
            _ => None,
        }
    }

    pub fn input_regs(&self) -> Vec<Reg> {
        match self {
            MirInst::Const(_, _) => vec![],
            MirInst::Var(_, _) => vec![],
            MirInst::BinaryOp(_, lhs, _, rhs) => vec![*lhs, *rhs],
            MirInst::Call(_, _, args) => args.clone(),
            MirInst::MethodCall(_, receiver, _, args) => {
                let mut v = vec![*receiver];
                v.extend(args);
                v
            }
            MirInst::ListLit(_, items) => items.clone(),
            MirInst::DictLit(_, entries) => entries.iter().map(|(_, r)| *r).collect(),
            MirInst::Index(_, obj, idx) => vec![*obj, *idx],
            MirInst::IndexAssign(obj, idx, val) => vec![*obj, *idx, *val],
            MirInst::Pipe(_, lhs, rhs) => vec![*lhs, *rhs],
            MirInst::Prompt(_, parts) => parts.clone(),
            MirInst::MatchExpr { val, arms } => {
                let mut v = vec![*val];
                for arm in arms {
                    if let Some(g) = arm.1 {
                        v.push(g);
                    }
                }
                v
            }
            MirInst::MatchArm { cond_reg, .. } => cond_reg.map(|r| vec![r]).unwrap_or_default(),
            MirInst::Closure { .. } => vec![],
            MirInst::DynTrait { src, .. } => vec![*src],
            MirInst::Define(_, r) => vec![*r],
            MirInst::Assign(_, r) => vec![*r],
            MirInst::Expr(r) => vec![*r],
            MirInst::JumpIf(cond, _) | MirInst::JumpIfNot(cond, _) => vec![*cond],
            MirInst::Return(Some(r)) => vec![*r],
            MirInst::Return(None) => vec![],
            MirInst::Halt(Some(r)) => vec![*r],
            MirInst::Halt(None) => vec![],
            MirInst::Send { value, .. } => vec![*value],
            MirInst::Save { path, value } => vec![*path, *value],
            MirInst::Load { path, .. } => vec![*path],
            MirInst::ReadFile { path, .. } => vec![*path],
            MirInst::WriteFile { path, content } => vec![*path, *content],
            MirInst::AppendFile { path, content } => vec![*path, *content],
            MirInst::ReadBytesFile { path, .. } => vec![*path],
            MirInst::WriteBytesFile { path, content } => vec![*path, *content],
            MirInst::Eval { given_reg, .. } => vec![*given_reg],
            MirInst::WithConfig { bindings, .. } => bindings.iter().map(|(_, r)| *r).collect(),
            MirInst::TaskDef { .. }
            | MirInst::ToolDef { .. }
            | MirInst::Import(_)
            | MirInst::TypeAlias { .. }
            | MirInst::EnumDef { .. }
            | MirInst::StructDef { .. }
            | MirInst::MacroDef { .. }
            | MirInst::Transaction { .. }
            | MirInst::Rollback
            | MirInst::Worker { .. }
            | MirInst::Commit
            | MirInst::Route(_)
            | MirInst::Observe { .. }
            | MirInst::Span { .. }
            | MirInst::RecordTokens { .. }
            | MirInst::TraitDef { .. }
            | MirInst::ImplDef { .. }
            | MirInst::Orchestrate { .. }
            | MirInst::SkillDef { .. }
            | MirInst::PromptSection { .. }
            | MirInst::DocumentSection { .. }
            | MirInst::Label(_)
            | MirInst::Jump(_)
            | MirInst::Break(_)
            | MirInst::Continue(_) => vec![],
        }
    }

    /// 输入寄存器重映射 — CSE 合并不同 dst 的节点后，把消费者的输入
    /// 寄存器引用从旧 dst 改写为新 dst（dag_interp 按 input_regs 取数，
    /// 不按 Data 边）。只映射输入位置，dst 不参与。嵌套函数体
    /// （Closure/TaskDef/... 的 Box<MirFunction>）寄存器空间独立，不改写。
    pub fn map_regs(&self, f: &mut impl FnMut(Reg) -> Reg) -> MirInst {
        let mut m = |r: Reg| f(r);
        match self {
            MirInst::Const(r, v) => MirInst::Const(*r, v.clone()),
            MirInst::Var(r, name) => MirInst::Var(*r, name.clone()),
            MirInst::BinaryOp(r, l, op, rr) => MirInst::BinaryOp(*r, m(*l), op.clone(), m(*rr)),
            MirInst::Call(r, name, args) => {
                MirInst::Call(*r, name.clone(), args.iter().map(|a| m(*a)).collect())
            }
            MirInst::ListLit(r, items) => {
                MirInst::ListLit(*r, items.iter().map(|i| m(*i)).collect())
            }
            MirInst::DictLit(r, entries) => MirInst::DictLit(
                *r,
                entries.iter().map(|(k, v)| (k.clone(), m(*v))).collect(),
            ),
            MirInst::Index(r, obj, idx) => MirInst::Index(*r, m(*obj), m(*idx)),
            MirInst::IndexAssign(obj, idx, val) => MirInst::IndexAssign(m(*obj), m(*idx), m(*val)),
            MirInst::MethodCall(r, recv, name, args) => MirInst::MethodCall(
                *r,
                m(*recv),
                name.clone(),
                args.iter().map(|a| m(*a)).collect(),
            ),
            MirInst::Pipe(r, lhs, rhs) => MirInst::Pipe(*r, m(*lhs), m(*rhs)),
            MirInst::Prompt(r, parts) => MirInst::Prompt(*r, parts.iter().map(|p| m(*p)).collect()),
            MirInst::MatchExpr { val, arms } => MirInst::MatchExpr {
                val: m(*val),
                arms: arms
                    .iter()
                    .map(|(p, g, body, out)| (p.clone(), g.map(&mut m), body.clone(), *out))
                    .collect(),
            },
            MirInst::Define(name, r) => MirInst::Define(name.clone(), m(*r)),
            MirInst::Assign(name, r) => MirInst::Assign(name.clone(), m(*r)),
            MirInst::Expr(r) => MirInst::Expr(m(*r)),
            MirInst::MatchArm { cond_reg, body } => MirInst::MatchArm {
                cond_reg: cond_reg.map(m),
                body: body.clone(),
            },
            MirInst::TaskDef { .. } => self.clone(),
            MirInst::Closure { .. } => self.clone(),
            MirInst::DynTrait {
                dst,
                src,
                trait_generics,
                trait_name,
            } => MirInst::DynTrait {
                dst: *dst,
                src: m(*src),
                trait_generics: trait_generics.clone(),
                trait_name: trait_name.clone(),
            },
            MirInst::ToolDef { .. } => self.clone(),
            MirInst::Import(_) => self.clone(),
            MirInst::WithConfig {
                bindings,
                body,
                jit,
            } => MirInst::WithConfig {
                bindings: bindings.iter().map(|(k, v)| (k.clone(), m(*v))).collect(),
                body: body.clone(),
                jit: *jit,
            },
            MirInst::TypeAlias { .. } => self.clone(),
            MirInst::EnumDef { .. } => self.clone(),
            MirInst::StructDef { .. } => self.clone(),
            MirInst::MacroDef { .. } => self.clone(),
            MirInst::Transaction { .. } => self.clone(),
            MirInst::Send { value, target } => MirInst::Send {
                value: m(*value),
                target: target.clone(),
            },
            MirInst::Rollback => MirInst::Rollback,
            MirInst::Worker { .. } => self.clone(),
            MirInst::Commit => MirInst::Commit,
            MirInst::Route(_) => self.clone(),
            MirInst::Observe { .. } => self.clone(),
            MirInst::Span { .. } => self.clone(),
            MirInst::RecordTokens { .. } => self.clone(),
            MirInst::Save { path, value } => MirInst::Save {
                path: m(*path),
                value: m(*value),
            },
            MirInst::Load { path, var } => MirInst::Load {
                path: m(*path),
                var: var.clone(),
            },
            MirInst::ReadFile { path, var } => MirInst::ReadFile {
                path: m(*path),
                var: var.clone(),
            },
            MirInst::WriteFile { path, content } => MirInst::WriteFile {
                path: m(*path),
                content: m(*content),
            },
            MirInst::AppendFile { path, content } => MirInst::AppendFile {
                path: m(*path),
                content: m(*content),
            },
            MirInst::ReadBytesFile { path, var } => MirInst::ReadBytesFile {
                path: m(*path),
                var: var.clone(),
            },
            MirInst::WriteBytesFile { path, content } => MirInst::WriteBytesFile {
                path: m(*path),
                content: m(*content),
            },
            MirInst::TraitDef { .. } => self.clone(),
            MirInst::ImplDef { .. } => self.clone(),
            MirInst::Orchestrate { .. } => self.clone(),
            MirInst::Eval {
                name,
                given_reg,
                expects,
                tolerance,
                replay_path,
            } => MirInst::Eval {
                name: name.clone(),
                given_reg: m(*given_reg),
                expects: expects.iter().map(|e| m(*e)).collect(),
                tolerance: *tolerance,
                replay_path: replay_path.clone(),
            },
            MirInst::SkillDef { .. } => self.clone(),
            MirInst::PromptSection { .. } => self.clone(),
            MirInst::DocumentSection { .. } => self.clone(),
            MirInst::Label(l) => MirInst::Label(*l),
            MirInst::Jump(l) => MirInst::Jump(*l),
            MirInst::JumpIf(cond, l) => MirInst::JumpIf(m(*cond), *l),
            MirInst::JumpIfNot(cond, l) => MirInst::JumpIfNot(m(*cond), *l),
            MirInst::Return(r) => MirInst::Return(r.map(m)),
            MirInst::Halt(r) => MirInst::Halt(r.map(m)),
            MirInst::Break(l) => MirInst::Break(*l),
            MirInst::Continue(l) => MirInst::Continue(*l),
        }
    }

    pub fn is_effect(&self) -> bool {
        match self {
            MirInst::Define(_, _)
            | MirInst::Assign(_, _)
            | MirInst::Expr(_)
            | MirInst::IndexAssign(_, _, _)
            | MirInst::Send { .. }
            | MirInst::Rollback
            | MirInst::Commit
            | MirInst::Save { .. }
            | MirInst::Load { .. }
            | MirInst::ReadFile { .. }
            | MirInst::WriteFile { .. }
            | MirInst::AppendFile { .. }
            | MirInst::ReadBytesFile { .. }
            | MirInst::WriteBytesFile { .. }
            | MirInst::Orchestrate { .. }
            | MirInst::RecordTokens { .. }
            | MirInst::Eval { .. }
            | MirInst::Import(_)
            | MirInst::TypeAlias { .. }
            | MirInst::EnumDef { .. }
            | MirInst::StructDef { .. }
            | MirInst::MacroDef { .. }
            | MirInst::TraitDef { .. }
            | MirInst::ImplDef { .. }
            | MirInst::TaskDef { .. }
            | MirInst::ToolDef { .. }
            | MirInst::SkillDef { .. }
            | MirInst::Route(_)
            | MirInst::WithConfig { .. }
            | MirInst::Transaction { .. }
            | MirInst::Worker { .. }
            | MirInst::Observe { .. }
            | MirInst::Span { .. }
            | MirInst::PromptSection { .. }
            | MirInst::DocumentSection { .. }
            | MirInst::Return(_)
            | MirInst::Halt(_) => true,
            MirInst::Const(_, _)
            | MirInst::Var(_, _)
            | MirInst::BinaryOp(_, _, _, _)
            | MirInst::Call(_, _, _)
            | MirInst::MethodCall(_, _, _, _)
            | MirInst::ListLit(_, _)
            | MirInst::DictLit(_, _)
            | MirInst::Index(_, _, _)
            | MirInst::Pipe(_, _, _)
            | MirInst::Prompt(_, _)
            | MirInst::MatchExpr { .. }
            | MirInst::MatchArm { .. }
            | MirInst::Closure { .. }
            | MirInst::DynTrait { .. }
            | MirInst::Label(_)
            | MirInst::Jump(_)
            | MirInst::JumpIf(_, _)
            | MirInst::JumpIfNot(_, _)
            | MirInst::Break(_)
            | MirInst::Continue(_) => false,
        }
    }
}

// ─── Unified dispatch ──────────────────────────────────────────────
// v0.59: Single exhaustive match over all MirInst variants.
// The compiler enforces that every variant is handled.

pub fn dispatch(
    inst: &MirInst,
    regs: &mut [Value],
    interp: &mut dyn MirHost,
    env: &mut Environment,
    task_registry: &HashMap<&str, (&[String], &MirFunction)>,
) -> Result<Flow, String> {
    match inst {
        // ── Pure value ──
        MirInst::Const(dst, v) => {
            h_const(regs, *dst, v);
            Ok(Flow::Continue)
        }
        MirInst::Var(dst, name) => {
            h_var(regs, *dst, name, env);
            Ok(Flow::Continue)
        }
        MirInst::BinaryOp(dst, l, op, r) => {
            h_binary_op(regs, *dst, *l, op, *r)?;
            Ok(Flow::Continue)
        }
        MirInst::Call(dst, name, args) => {
            h_call(regs, *dst, name, args, task_registry, interp, env)?;
            Ok(Flow::Continue)
        }
        MirInst::ListLit(dst, items) => {
            h_list_lit(regs, *dst, items);
            Ok(Flow::Continue)
        }
        MirInst::DictLit(dst, entries) => {
            h_dict_lit(regs, *dst, entries);
            Ok(Flow::Continue)
        }
        MirInst::Index(dst, obj, idx) => {
            h_index(regs, *dst, *obj, *idx)?;
            Ok(Flow::Continue)
        }
        MirInst::MethodCall(dst, recv, method, args) => {
            h_method_call(regs, *dst, *recv, method, args, interp)?;
            Ok(Flow::Continue)
        }
        MirInst::Pipe(dst, lhs, rhs) => {
            h_pipe(regs, *dst, *lhs, *rhs, interp)?;
            Ok(Flow::Continue)
        }
        MirInst::Prompt(dst, parts) => {
            h_prompt(regs, *dst, parts);
            Ok(Flow::Continue)
        }
        MirInst::Closure { dst, params, body } => {
            h_closure(regs, *dst, params, body, interp);
            Ok(Flow::Continue)
        }
        MirInst::DynTrait {
            dst,
            src,
            trait_generics,
            trait_name,
        } => {
            h_dyn_trait(regs, *dst, *src, trait_name, trait_generics);
            Ok(Flow::Continue)
        }
        MirInst::MatchExpr { val, arms } => {
            h_match_expr(interp, env, regs, *val, arms)?;
            Ok(Flow::Continue)
        }

        // ── Side effects ──
        MirInst::Define(name, src) => {
            h_define(env, name, regs, *src);
            Ok(Flow::Continue)
        }
        MirInst::Assign(name, src) => {
            h_assign(env, name, regs, *src);
            Ok(Flow::Continue)
        }
        MirInst::Expr(_) => Ok(Flow::Continue),
        MirInst::IndexAssign(obj, idx, val) => {
            h_index_assign(regs, *obj, *idx, *val)?;
            Ok(Flow::Continue)
        }
        MirInst::TypeAlias { name, target } => {
            h_type_alias(env, name, target);
            Ok(Flow::Continue)
        }
        MirInst::EnumDef { name, variants } => {
            h_enum_def(env, name, variants);
            Ok(Flow::Continue)
        }
        MirInst::StructDef { name, fields } => {
            h_struct_def(env, name, fields);
            Ok(Flow::Continue)
        }
        MirInst::Import(path) => {
            h_import(interp, env, path)?;
            Ok(Flow::Continue)
        }
        MirInst::WithConfig {
            bindings,
            body,
            jit,
        } => {
            h_with_config(interp, env, regs, bindings, body, *jit)?;
            Ok(Flow::Continue)
        }
        MirInst::MacroDef { name, params } => {
            h_macro_def(env, name, params);
            Ok(Flow::Continue)
        }
        MirInst::Transaction { body, compensation } => {
            h_transaction(interp, env, body, compensation)
        }
        MirInst::Send { value, target } => {
            h_send(interp, regs, *value, target)?;
            Ok(Flow::Continue)
        }
        MirInst::Rollback => Err("Transaction rolled back".to_string()),
        MirInst::Commit => Ok(Flow::Continue),
        MirInst::Worker { name: _, body } => {
            h_worker(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::Route(name) => Err(format!("route '{}' not implemented", name)),
        MirInst::Observe { config: _, body } => {
            h_observe(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::Span { name: _, body } => {
            h_span(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::RecordTokens { .. } => Ok(Flow::Continue),
        MirInst::Save { path, value } => {
            h_save(interp, regs, *path, *value)?;
            Ok(Flow::Continue)
        }
        MirInst::Load { path, var } => {
            h_load(interp, env, regs, *path, var)?;
            Ok(Flow::Continue)
        }
        MirInst::ReadFile { path, var } => {
            h_read_file(interp, env, regs, *path, var)?;
            Ok(Flow::Continue)
        }
        MirInst::WriteFile { path, content } => {
            h_write_file(interp, regs, *path, *content)?;
            Ok(Flow::Continue)
        }
        MirInst::AppendFile { path, content } => {
            h_append_file(interp, regs, *path, *content)?;
            Ok(Flow::Continue)
        }
        MirInst::ReadBytesFile { path, var } => {
            h_read_bytes_file(interp, env, regs, *path, var)?;
            Ok(Flow::Continue)
        }
        MirInst::WriteBytesFile { path, content } => {
            h_write_bytes_file(interp, regs, *path, *content)?;
            Ok(Flow::Continue)
        }
        MirInst::TraitDef {
            name,
            parents,
            methods,
            method_bodies,
        } => {
            h_trait_def(interp, env, name, parents, methods, method_bodies)?;
            Ok(Flow::Continue)
        }
        MirInst::ImplDef {
            trait_name,
            trait_generics,
            for_type,
            for_generics,
            methods,
            method_bodies,
        } => {
            h_impl_def(
                interp,
                env,
                trait_name,
                trait_generics,
                for_type,
                for_generics,
                methods,
                method_bodies,
            )?;
            Ok(Flow::Continue)
        }
        MirInst::Orchestrate {
            input_var,
            result_var,
            kind,
        } => {
            h_orchestrate(interp, env, input_var, result_var, kind)?;
            Ok(Flow::Continue)
        }
        MirInst::Eval {
            name,
            given_reg,
            expects,
            tolerance,
            ..
        } => {
            h_eval(regs, env, name, *given_reg, expects, tolerance)?;
            Ok(Flow::Continue)
        }
        MirInst::SkillDef {
            name,
            description,
            version,
            requires,
            tasks,
            task_bodies,
            verify,
            verify_body,
        } => {
            h_skill_def(
                env,
                name,
                description,
                version,
                requires,
                tasks,
                task_bodies,
                verify,
                verify_body,
            );
            Ok(Flow::Continue)
        }
        MirInst::PromptSection { name: _, body } => {
            h_prompt_section(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::DocumentSection { name: _, body } => {
            h_document_section(interp, env, body)?;
            Ok(Flow::Continue)
        }

        // ── Control flow + no-ops ──
        MirInst::TaskDef { .. }
        | MirInst::ToolDef { .. }
        | MirInst::MatchArm { .. }
        | MirInst::Label(_) => Ok(Flow::Continue),
        MirInst::Jump(lbl) => Ok(h_jump(*lbl)),
        MirInst::JumpIf(cond, lbl) => Ok(h_jump_if(regs, *cond, *lbl)),
        MirInst::JumpIfNot(cond, lbl) => Ok(h_jump_if_not(regs, *cond, *lbl)),
        MirInst::Return(r) => Ok(h_return(regs, *r)),
        MirInst::Halt(r) => Ok(h_halt(regs, *r)),
        MirInst::Break(lbl) => Ok(h_break(*lbl)),
        MirInst::Continue(lbl) => Ok(h_continue(*lbl)),
    }
}
