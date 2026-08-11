//! v0.75.56: MirInst metadata（dst/input_regs/map_regs/is_effect）与 dispatch
//! 分派 — 自 handlers.rs 拆出（D6 单文件惯例）。
//!
//! - dst()/input_regs()/map_regs()/is_effect()：指令元数据，单一事实源
//! - dispatch()：线性解释器与 DAG 解释器共享的指令级分派（v0.59 起）

use std::collections::HashMap;

use super::handlers::{
    Flow, h_aggregate, h_append_file, h_assign, h_binary_op, h_break, h_call, h_closure, h_const,
    h_continue, h_define, h_dict_lit, h_document_section, h_dyn_trait, h_enum_def, h_eval, h_halt,
    h_handle, h_impl_def, h_import, h_index, h_index_assign, h_jump, h_jump_if, h_jump_if_not,
    h_list_lit, h_load, h_macro_def, h_match_expr, h_method_call, h_observe, h_orchestrate,
    h_perform, h_pipe, h_prompt, h_prompt_section, h_read_bytes_file, h_read_file, h_return,
    h_save, h_send, h_skill_def, h_span, h_struct_def, h_trait_def, h_transaction, h_type_alias,
    h_var, h_with_config, h_worker, h_write_bytes_file, h_write_file,
};

use crate::mir::host::MirHost;

use crate::mir::{MirFunction, MirInst, Reg};

use crate::value::{Environment, Value};

// ─── MirInst metadata — single source of truth ──────────────────────
// v0.59: dst(), input_regs(), is_effect() + dispatch() all in one file.
// All matches are exhaustive — compiler enforces updates on new variants.

impl MirInst {
    /// Destination register, if this instruction produces a value.
    pub fn dst(&self) -> Option<Reg> {
        match self {
            MirInst::Const(r, _) => Some(*r),
            MirInst::Var(r, _) => Some(*r),
            MirInst::Copy(r, _) => Some(*r),
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
            MirInst::Perform { dst, .. } => Some(*dst),
            // Handle 是语句（不产生值），唯一写入 dst 的是 handler 末尾的 resume 续
            // 续名——该续名由 h_handle 自己 emit Const(dst, ...) 而非 Handle 自身。
            MirInst::Handle { .. } => None,
            MirInst::Closure { dst, .. } => Some(*dst),
            MirInst::DynTrait { dst, .. } => Some(*dst),
            _ => None,
        }
    }

    pub fn input_regs(&self) -> Vec<Reg> {
        match self {
            MirInst::Const(_, _) => vec![],
            MirInst::Var(_, _) => vec![],
            MirInst::Copy(_, src) => vec![*src],
            MirInst::BinaryOp(_, lhs, _, rhs) => vec![*lhs, *rhs],
            MirInst::Call(_, _, args) => args.clone(),
            MirInst::Perform { args, .. } => args.clone(),
            // Handle: body 与 handler 用独立 reg 空间（已 box 化），不读外层任何 reg。
            // k_dst 是 output reg（handler 末尾 resume 续 的返回值），由 h_handle 写。
            MirInst::Handle { .. } => vec![],
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
            MirInst::Aggregate { value, .. } => vec![*value],
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
            | MirInst::Observe { .. }
            | MirInst::Span { .. }
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
            MirInst::Copy(r, src) => MirInst::Copy(*r, m(*src)),
            MirInst::BinaryOp(r, l, op, rr) => MirInst::BinaryOp(*r, m(*l), op.clone(), m(*rr)),
            MirInst::Call(r, name, args) => {
                MirInst::Call(*r, name.clone(), args.iter().map(|a| m(*a)).collect())
            }
            // Perform: dst 不映射（SSA 自行管理），args 全部映射。
            MirInst::Perform { dst, effect, args } => MirInst::Perform {
                dst: *dst,
                effect: effect.clone(),
                args: args.iter().map(|a| m(*a)).collect(),
            },
            // Handle: body 和 handler 是独立 reg 空间（不递归 map）。
            // k_param 是 handler 内的 resume 续名（handler body 内部管理）。
            MirInst::Handle { effect, body, handler, k_param, k_dst } => MirInst::Handle {
                effect: effect.clone(),
                body: body.clone(),
                handler: handler.clone(),
                k_param: k_param.clone(),
                k_dst: *k_dst,
            },
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
            MirInst::Aggregate { name, value } => MirInst::Aggregate {
                name: name.clone(),
                value: m(*value),
            },
            MirInst::Rollback => MirInst::Rollback,
            MirInst::Worker { .. } => self.clone(),
            MirInst::Commit => MirInst::Commit,
            MirInst::Observe { .. } => self.clone(),
            MirInst::Span { .. } => self.clone(),
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
            | MirInst::Aggregate { .. }
            | MirInst::Rollback
            | MirInst::Commit
            | MirInst::Save { .. }
            | MirInst::Load { .. }
            // Perform 触发 effect — 必然有副作用（即便 handler 解释为纯函数）。
            | MirInst::Perform { .. }
            // Handle 是语句（安装/卸载 handler），但本身不产生外部副作用。
            // 真正的 effect 来源是 body 内的 Perform 指令。
            | MirInst::Handle { .. }
            | MirInst::ReadFile { .. }
            | MirInst::WriteFile { .. }
            | MirInst::AppendFile { .. }
            | MirInst::ReadBytesFile { .. }
            | MirInst::WriteBytesFile { .. }
            | MirInst::Orchestrate { .. }
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
            | MirInst::Copy(_, _)
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
        MirInst::Copy(dst, src) => {
            regs[*dst] = regs[*src].clone();
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
            h_closure(regs, *dst, params, body, env);
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
        MirInst::Perform { dst, effect, args } => {
            h_perform(regs, *dst, effect, args, interp)?;
            Ok(Flow::Continue)
        }
        MirInst::Handle {
            effect,
            body,
            handler,
            k_param,
            k_dst,
        } => {
            h_handle(interp, env, regs, effect, body, handler, k_param.as_str(), *k_dst)?;
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
        MirInst::Aggregate { name, value } => {
            h_aggregate(interp, regs, *value, name)?;
            Ok(Flow::Continue)
        }
        MirInst::Rollback => Err("Transaction rolled back".to_string()),
        MirInst::Commit => Ok(Flow::Continue),
        MirInst::Worker { name: _, body } => {
            h_worker(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::Observe { config: _, body } => {
            h_observe(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::Span { name: _, body } => {
            h_span(interp, env, body)?;
            Ok(Flow::Continue)
        }
        MirInst::Save { path, value } => {
            h_save(interp, env, regs, *path, *value)?;
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
            h_write_file(interp, env, regs, *path, *content)?;
            Ok(Flow::Continue)
        }
        MirInst::AppendFile { path, content } => {
            h_append_file(interp, env, regs, *path, *content)?;
            Ok(Flow::Continue)
        }
        MirInst::ReadBytesFile { path, var } => {
            h_read_bytes_file(interp, env, regs, *path, var)?;
            Ok(Flow::Continue)
        }
        MirInst::WriteBytesFile { path, content } => {
            h_write_bytes_file(interp, env, regs, *path, *content)?;
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
