//! v0.89: EHIR → Core 降维 — 9 层架构桥接第 1 段。
//!
//! 将 Node`<TypeInfo>`（带类型的结构化 CFG）降维为 CoreInst（SSA 基元）。
//!
//! 降维内容：
//! - FnDef → ClosureCreate + EnvStore（函数即闭包）
//! - Handle → EffectInstall + body + EffectRestore
//! - Perform → EffectPerform（EffectLabel 从 TypeInfo.effects 提取）
//! - Match → EnumMatch（模式匹配降维为枚举匹配）
//! - Orchestrate → PregelGraph/MoAPipeline/MoERouter（编排降维）
//! - Let/Assign → EnvStore/EnvMutate（环境操作）
//! - 声明类（TypeAlias/EnumDef/StructDef/TraitDef/ImplDef）→ 编译期消失
//! - TEA（ModelDef/MsgDef/UpdateDef/AppDef）→ 编译期注册

use crate::mir::core::{
    BlockId, CoreBlock, CoreFunction, CoreInst, CoreTerminator, EffectLabel, EnumMatchArm,
    ScopePayload, literal_to_value, pattern_bindings,
};
use crate::mir::effect::EffectRow;
use crate::mir::fcfg::{Ehir, EhirBlock, Node, QuasiquoteSegment as FcfgSegment, Reg};
use crate::typeck::Type;
use crate::value::Value;

/// 将 EHIR 节点列表降维为 CoreFunction。
pub fn ehir_to_core(
    nodes: &[Ehir],
    params: Vec<(String, Type)>,
    effects: EffectRow,
) -> CoreFunction {
    let mut ctx = CoreContext::new(effects);
    // v0.104.6 D482：顶层同样要**跳过** FCFG 预分配的寄存器（见
    // `lower_body_to_core` 的说明）—— 否则 Core 层的 `alloc_reg()` 会与
    // FCFG 的预分配号撞车。
    ctx.next_reg = crate::mir::fcfg_lower::max_reg_in_nodes(nodes) + 1;
    for node in nodes {
        lower_node(&mut ctx, node);
    }
    ctx.finish(params)
}

/// Core 降维上下文。
struct CoreContext {
    next_reg: usize,
    blocks: Vec<CoreBlock>,
    current_insts: Vec<CoreInst>,
    next_block_id: BlockId,
    /// 效果行（函数级；嵌套代码区**继承**外层 —— Core 层不按体拆分效果）。
    effects: EffectRow,
    /// 循环栈（v0.104.6 D481）：`break` / `continue` 需要知道所在循环的
    /// 条件块与出口块。
    loop_stack: Vec<LoopCtx>,
}

/// 循环上下文。
///
/// `continue` 的目标（条件块）在进入循环体时**已知**；`break` 的目标
/// （出口块）要等**循环体降完**才知道 ⇒ 先记下 `break` 终结指令所在的块
/// 下标，循环结束时回填。
struct LoopCtx {
    /// 循环条件块（`continue` 的目标）。
    cond_id: BlockId,
    /// `break` 的终结指令所在块的下标（回填用）。
    breaks: Vec<usize>,
}

impl CoreContext {
    fn new(effects: EffectRow) -> Self {
        Self {
            next_reg: 0,
            blocks: Vec::new(),
            current_insts: Vec::new(),
            next_block_id: 0,
            effects,
            loop_stack: Vec::new(),
        }
    }

    fn alloc_reg(&mut self) -> usize {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    fn emit(&mut self, inst: CoreInst) {
        self.current_insts.push(inst);
    }

    fn finish(mut self, params: Vec<(String, Type)>) -> CoreFunction {
        self.flush_block(CoreTerminator::Return(None));
        CoreFunction {
            params,
            blocks: self.blocks,
            entry: 0,
            effects: self.effects,
            n_regs: self.next_reg,
        }
    }

    /// 组装**嵌套代码区**（闭包体 / match 分支体）。
    ///
    /// v0.104.6 D480：末块以 `Return(result)` 收尾（体自身的值），
    /// **全部块**都保留 —— 此前只保留首块（其余块被丢弃 ⇒ 体里的嵌套分支
    /// 目标指向不存在的块）。
    fn finish_body(mut self, result: Option<Reg>) -> CoreFunction {
        self.flush_block(CoreTerminator::Return(result));
        CoreFunction {
            // 嵌套代码区不是「函数」：闭包形参在 `ClosureCreate.params` 里，
            // match 分支体没有形参 ⇒ 形参表为空。
            params: vec![],
            blocks: self.blocks,
            entry: 0,
            effects: self.effects,
            n_regs: self.next_reg,
        }
    }

    fn flush_block(&mut self, terminator: CoreTerminator) {
        self.flush_block_at(terminator);
    }

    /// flush 当前块，返回它在 `blocks` 里的下标（供事后回填）。
    ///
    /// v0.104.6 D479：块号**只能顺序分配**（`next_block_id` 单调递增），
    /// 所以「先降维、后回填」是唯一能拿到正确块号的办法 ——
    /// 分支目标要等**两个分支体都降完**才知道（它们各自占用了多少块），
    /// 汇合块更是在那之后才存在。
    fn flush_block_at(&mut self, terminator: CoreTerminator) -> usize {
        let id = self.next_block_id;
        self.next_block_id += 1;
        self.blocks.push(CoreBlock {
            id,
            insts: std::mem::take(&mut self.current_insts),
            terminator,
        });
        self.blocks.len() - 1
    }

    /// 回填某块的分支目标。
    ///
    /// ⚠ Core 的分支信息在**两处**都有：块内的 `CoreInst::Branch` 与块的
    /// `CoreTerminator::Branch`（模型既有设计），两处都要改，否则指令与
    /// 终结指令会指向不同的块。
    fn patch_branch(&mut self, block_idx: usize, true_bb: BlockId, false_bb: BlockId) {
        for inst in self.blocks[block_idx].insts.iter_mut() {
            if let CoreInst::Branch {
                true_bb: t,
                false_bb: f,
                ..
            } = inst
            {
                *t = true_bb;
                *f = false_bb;
            }
        }
        if let CoreTerminator::Branch {
            true_bb: t,
            false_bb: f,
            ..
        } = &mut self.blocks[block_idx].terminator
        {
            *t = true_bb;
            *f = false_bb;
        }
    }

    /// 回填某块的终结指令为 `Jump(target)`。
    fn patch_jump(&mut self, block_idx: usize, target: BlockId) {
        self.blocks[block_idx].terminator = CoreTerminator::Jump(target);
    }
}

/// 降维单个 EHIR 节点。
fn lower_node(ctx: &mut CoreContext, node: &Ehir) {
    match node {
        // ── 值产生 ──
        Node::Literal { reg, value, .. } => {
            ctx.emit(CoreInst::Const(*reg, literal_to_value(value)));
        }
        Node::Variable { reg, name, .. } => {
            ctx.emit(CoreInst::EnvLoad(*reg, name.clone()));
        }
        Node::BinaryOp {
            dst, lhs, op, rhs, ..
        } => {
            ctx.emit(CoreInst::BinaryOp(*dst, *lhs, op.clone(), *rhs));
        }
        // Or/And → Core 层降维为**短路控制流**（Branch + 两块）
        //
        // v0.104.6 D477：此前 Core 层把 or/and 降成**普通 BinaryOp**
        // （`lhs != rhs` / `lhs == rhs`）—— 那**既不是取值短路、也不是求值
        // 短路**（`true or true` 会算成 `false`），与 MIR 层的语义不符。
        // D477 起与 MIR 层同形：短路分支只写 `dst ← lhs`，非短路分支才
        // 求值 rhs。块结构与本文件 `Node::If` 的降级同款。
        Node::Or { dst, lhs, rhs, .. } => {
            // cond 为真 ⇒ 短路（结果 = lhs，rhs 不求值）
            //
            // v0.104.6 D479：块号改为**先降维、后回填**。此前是「预分配」
            // （`next_block_id + 1 / +2 / +3`）⇒ 嵌套分支时必然与内层实际
            // 分配的块号相撞 ⇒ 边指错、甚至成环（实测：`if` 里套 `if` 时
            // 外层 `false_bb` 指向内层的 then 块，3 ↔ 4 成环）。
            ctx.emit(CoreInst::Branch {
                cond: *lhs,
                true_bb: 0,
                false_bb: 0,
            });
            let branch_idx = ctx.flush_block_at(CoreTerminator::Branch {
                cond: *lhs,
                true_bb: 0,
                false_bb: 0,
            });
            // 短路块：结果 = lhs（rhs 不求值）
            let short_start = ctx.next_block_id;
            ctx.emit(CoreInst::Copy(*dst, *lhs));
            let short_join = ctx.flush_block_at(CoreTerminator::Unreachable);
            // 非短路块：求值 rhs，结果 = (lhs != rhs)
            let eval_start = ctx.next_block_id;
            lower_block(ctx, rhs);
            let r = rhs.result.unwrap_or(0);
            ctx.emit(CoreInst::BinaryOp(
                *dst,
                *lhs,
                crate::common::BinaryOp::NotEqual,
                r,
            ));
            let eval_join = ctx.flush_block_at(CoreTerminator::Unreachable);
            // 汇合块 = 本分支之后的当前块（此刻尚未 flush，flush 时拿到这个 id）
            let join = ctx.next_block_id;
            ctx.patch_branch(branch_idx, short_start, eval_start);
            ctx.patch_jump(short_join, join);
            ctx.patch_jump(eval_join, join);
        }
        Node::And { dst, lhs, rhs, .. } => {
            // cond 为假 ⇒ 短路（结果 = lhs，rhs 不求值）
            ctx.emit(CoreInst::Branch {
                cond: *lhs,
                true_bb: 0,
                false_bb: 0,
            });
            let branch_idx = ctx.flush_block_at(CoreTerminator::Branch {
                cond: *lhs,
                true_bb: 0,
                false_bb: 0,
            });
            // 非短路块：求值 rhs，结果 = (lhs == rhs)
            let eval_start = ctx.next_block_id;
            lower_block(ctx, rhs);
            let r = rhs.result.unwrap_or(0);
            ctx.emit(CoreInst::BinaryOp(
                *dst,
                *lhs,
                crate::common::BinaryOp::Equal,
                r,
            ));
            let eval_join = ctx.flush_block_at(CoreTerminator::Unreachable);
            // 短路块
            let short_start = ctx.next_block_id;
            ctx.emit(CoreInst::Copy(*dst, *lhs));
            let short_join = ctx.flush_block_at(CoreTerminator::Unreachable);
            let join = ctx.next_block_id;
            ctx.patch_branch(branch_idx, eval_start, short_start);
            ctx.patch_jump(eval_join, join);
            ctx.patch_jump(short_join, join);
        }
        // DynTrait/Prompt → Core 层降维为透传调用
        Node::DynTrait { dst, src, .. } => {
            ctx.emit(CoreInst::Copy(*dst, *src));
        }
        Node::Prompt { dst, parts, .. } => {
            // 模板求值（镜像 `MirInst::Prompt(dst, parts)`）。
            //
            // v0.104.6 D482：此前被降成 `Call("prompt", …)` ⇒ 一个**根本不存在
            // 的函数名**，既执行不了、也误导读者。
            ctx.emit(CoreInst::Prompt(*dst, parts.clone()));
        }
        Node::ClosureExpr {
            dst, params, body, ..
        } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let core_body = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::ClosureCreate {
                dst: *dst,
                params: param_names,
                // v0.104.6 D481：捕获变量不再恒为空（同 `Node::FnDef`：过近似）。
                captures: capture_names(body),
                body: core_body,
            });
        }
        Node::Call {
            dst,
            callee,
            callee_name,
            args,
            ..
        } => {
            // 已知函数名 → 具名 `Call`（**带名字**），否则 → 值调用 `ClosureCall`。
            //
            // v0.104.6 D482：此前具名调用的**名字被丢了**（`Call` 不带名字）
            // ⇒ Core 层**无法分派具名调用** ⇒ 根本执行不了。名字是分派的唯一
            // 依据，必须留在 IR 里。
            match callee_name {
                Some(name) => {
                    ctx.emit(CoreInst::Call(*dst, name.clone(), args.clone()));
                }
                None => {
                    ctx.emit(CoreInst::ClosureCall(*dst, *callee, args.clone()));
                }
            }
        }
        Node::MethodCall {
            dst,
            receiver,
            method,
            args,
            ..
        } => {
            // 方法调用（镜像 `MirInst::MethodCall(dst, receiver, method, args)`）。
            //
            // v0.104.6 D482：此前被降成**具名** `Call` ⇒ 分派方式错了
            // （方法要走 `mir_call_method`，不是 `mir_call_function`），
            // 而且**方法名被丢了**（`let _ = method;`）。
            ctx.emit(CoreInst::MethodCall(
                *dst,
                *receiver,
                method.clone(),
                args.clone(),
            ));
        }
        Node::ListLit { dst, items, .. } => {
            ctx.emit(CoreInst::ListLit(*dst, items.clone()));
        }
        Node::DictLit { dst, entries, .. } => {
            ctx.emit(CoreInst::DictLit(*dst, entries.clone()));
        }
        Node::Index { dst, obj, idx, .. } => {
            ctx.emit(CoreInst::Index(*dst, *obj, *idx));
        }

        // ── 控制流 ──
        Node::If {
            cond,
            then,
            else_,
            dst,
            ..
        } => {
            // 降维为 Branch + 两个基本块
            //
            // v0.104.6 D479：块号改为**先降维、后回填**（见 `Node::Or` 处的
            // 说明）：预分配的 `+1/+2/+3` 在嵌套分支下必然撞车。
            ctx.emit(CoreInst::Branch {
                cond: *cond,
                true_bb: 0,
                false_bb: 0,
            });
            let branch_idx = ctx.flush_block_at(CoreTerminator::Branch {
                cond: *cond,
                true_bb: 0,
                false_bb: 0,
            });

            // then block
            let then_start = ctx.next_block_id;
            lower_block(ctx, then);
            ctx.emit(CoreInst::Copy(*dst, then.result.unwrap_or(0)));
            let then_join = ctx.flush_block_at(CoreTerminator::Unreachable);

            // else block
            let else_start = ctx.next_block_id;
            match else_ {
                Some(else_block) => {
                    lower_block(ctx, else_block);
                    ctx.emit(CoreInst::Copy(*dst, else_block.result.unwrap_or(0)));
                }
                // 无 else ⇒ 物化隐式 else（`dst ← Nil`），与 MIR 层
                // `fcfg_lower` 的 else-less `if` 降级同款。
                None => {
                    ctx.emit(CoreInst::Const(*dst, Value::Nil));
                }
            }
            let else_join = ctx.flush_block_at(CoreTerminator::Unreachable);

            // 汇合块 = 本 if 之后的当前块（此刻尚未 flush）
            let join = ctx.next_block_id;
            ctx.patch_branch(branch_idx, then_start, else_start);
            ctx.patch_jump(then_join, join);
            ctx.patch_jump(else_join, join);
        }
        Node::While {
            cond, body, dst, ..
        } => {
            // v0.104.6 D482：**条件必须独立成块**。
            //
            // 此前条件指令与「循环之前的语句」落在**同一块**里，而回边跳回
            // 那一块 ⇒ 循环之前的语句（如 `let j = 0`）被**反复重跑** ⇒
            // 条件恒真 ⇒ **死循环**。
            //
            // MIR 层没有这个问题：它是**线性指令流 + pc 定位循环头**
            // （`loop_start` = 条件指令之前的位置），回边只重跑条件本身。
            // Core 是块结构 ⇒ 跳不进块中间 ⇒ 条件必须自己一个块。
            let pre_idx = ctx.flush_block_at(CoreTerminator::Jump(0)); // 占位，稍后回填

            // cond block
            let cond_id = ctx.next_block_id;
            lower_block(ctx, cond);
            let cond_reg = cond.result.unwrap_or(0);
            ctx.emit(CoreInst::Branch {
                cond: cond_reg,
                true_bb: 0,
                false_bb: 0,
            });
            let cond_idx = ctx.flush_block_at(CoreTerminator::Branch {
                cond: cond_reg,
                true_bb: 0,
                false_bb: 0,
            });

            // body block
            let body_id = ctx.next_block_id;
            // v0.104.6 D481：进入循环上下文（`break`/`continue` 需要它）。
            ctx.loop_stack.push(LoopCtx {
                cond_id,
                breaks: Vec::new(),
            });
            lower_block(ctx, body);
            let loop_ctx = ctx.loop_stack.pop().expect("循环上下文已压栈");
            // 回边：跳回条件块
            ctx.emit(CoreInst::Jump(cond_id));
            ctx.flush_block_at(CoreTerminator::Jump(cond_id));

            // 出口块 = 循环之后的当前块
            let end_id = ctx.next_block_id;
            ctx.patch_branch(cond_idx, body_id, end_id);
            // 循环之前的块 → 条件块
            ctx.patch_jump(pre_idx, cond_id);
            // `break` 的目标是**出口块**，它此刻才确定 ⇒ 回填（D481）
            for b in loop_ctx.breaks {
                ctx.patch_jump(b, end_id);
            }

            // v0.103: 循环作为表达式的结果 = Nil，写节点自带 dst（emit.rs
            // emit_while_w / fcfg_lower 的同一契约）。
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }
        Node::For {
            var,
            iter,
            body,
            dst,
            ..
        } => {
            // index-based loop
            //
            // v0.104.6 D482：**三块结构** —— 循环之前的语句 / 初始化 / 条件
            // 各占一块。否则回边跳回条件块时会把 `idx = 0` 一起重跑 ⇒ 索引永远
            // 归零 ⇒ 死循环（与 `Node::While` 的「条件独立成块」同款理由）。
            //
            //   K  : [循环之前的语句]      → Jump(K+1)
            //   K+1: [初始化 idx/len/one]  → Jump(K+2)
            //   K+2: [条件 idx >= len]     → Branch(出口 / K+3)
            //   K+3: [循环体]              → Jump(K+2)
            let pre_idx = ctx.flush_block_at(CoreTerminator::Jump(0)); // 占位，稍后回填
            let idx_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::Const(idx_reg, Value::Int(0)));
            let len_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::Call(len_reg, "len".to_string(), vec![*iter]));
            // 步长常量与 fcfg_lower/emit.rs 同序（循环标签前发射）。
            let one = ctx.alloc_reg();
            ctx.emit(CoreInst::Const(one, Value::Int(1)));
            let init_id = ctx.next_block_id;
            let init_idx = ctx.flush_block_at(CoreTerminator::Jump(0)); // 占位，稍后回填

            // 循环头块：条件计算 + 分支（v0.104.6 D479：先降维、后回填）
            let loop_start = ctx.next_block_id;
            let cond_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::BinaryOp(
                cond_reg,
                idx_reg,
                crate::common::BinaryOp::GreaterEqual,
                len_reg,
            ));
            ctx.emit(CoreInst::Branch {
                cond: cond_reg,
                true_bb: 0,
                false_bb: 0,
            });
            let cond_idx = ctx.flush_block_at(CoreTerminator::Branch {
                cond: cond_reg,
                true_bb: 0,
                false_bb: 0,
            });

            // body
            let body_id = ctx.next_block_id;
            let val_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::Index(val_reg, *iter, idx_reg));
            ctx.emit(CoreInst::EnvStore(var.clone(), val_reg));
            // v0.104.6 D481：进入循环上下文（`break`/`continue` 需要它）。
            ctx.loop_stack.push(LoopCtx {
                cond_id: loop_start,
                breaks: Vec::new(),
            });
            lower_block(ctx, body);
            let loop_ctx = ctx.loop_stack.pop().expect("循环上下文已压栈");
            ctx.emit(CoreInst::BinaryOp(
                idx_reg,
                idx_reg,
                crate::common::BinaryOp::Add,
                one,
            ));
            ctx.emit(CoreInst::Jump(loop_start));
            ctx.flush_block_at(CoreTerminator::Jump(loop_start));

            // 出口块 = 循环之后的当前块
            let end_id = ctx.next_block_id;
            ctx.patch_branch(cond_idx, end_id, body_id);
            // 循环之前的块 → 初始化块；初始化块 → 循环头块
            ctx.patch_jump(pre_idx, init_id);
            ctx.patch_jump(init_idx, loop_start);
            // `break` 的目标是**出口块**，它此刻才确定 ⇒ 回填（D481）
            for b in loop_ctx.breaks {
                ctx.patch_jump(b, end_id);
            }

            // v0.103: 循环作为表达式的结果 = Nil，写节点自带 dst（emit.rs
            // emit_loop_w / fcfg_lower 的同一契约）。
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }
        Node::Match {
            dst,
            scrutinee,
            arms,
            ..
        } => {
            // 降维为 EnumMatch（dst 作为统一结果寄存器透传）
            //
            // v0.104.6 D481：两处**说谎**修掉 ——
            // - `variant` 此前是 `format!("{:?}", pattern)` ⇒ 把**源码位置**
            //   也塞进了变体名（`Literal(Float(3.0, Span { line: 3, column: 5 }))`）；
            //   改为只留模式的可读形式。
            // - `bindings` 此前**恒为空** ⇒ 模式里的绑定名**全丢**
            //   （`x when x > 0` 的 `x` 在产物里根本不存在）。改为从模式里
            //   取出绑定名。
            // v0.104.6 D482：可读标签改为**模式源码文本**（与
            // `MirInst::MatchArmInst` 同形 ⇒ 执行器可直接复用
            // `self_match_pattern` ⇒ 与 MIR 层**逐字同一套**匹配语义），
            // arm 里不再存 Debug 串（那会把 `Span` 也塞进去）。
            let core_arms: Vec<EnumMatchArm> = arms
                .iter()
                .map(|arm| EnumMatchArm {
                    pattern: crate::mir::fcfg_lower::fcfg_pattern_to_string(&arm.pattern),
                    bindings: pattern_bindings(&arm.pattern),
                    // 守卫是独立代码区（在自己的寄存器空间里求值），与 MIR 层同形。
                    guard: arm.guard.as_ref().map(|g| lower_body_to_core(ctx, g)),
                    body: lower_body_to_core(ctx, &arm.body),
                })
                .collect();
            ctx.emit(CoreInst::EnumMatch {
                dst: *dst,
                scrutinee: *scrutinee,
                arms: core_arms,
            });
        }
        Node::Return { value, .. } => {
            ctx.flush_block(CoreTerminator::Return(*value));
        }
        Node::Break { .. } => {
            // v0.104.6 D481：此前写成 `CoreInst::Jump(0)` —— 一条**假跳转**
            // （目标是块 0，与循环出口毫无关系），而且它只是块里的**一条指令**，
            // 块本身照常往下走 ⇒ 产物里看不出「这里会跳出循环」。
            //
            // 正确形状：`break` **结束当前块**，其终结指令是
            // `Jump(循环出口块)`。出口块要等循环体降完才知道 ⇒ 先占位、
            // 循环结束时回填（见 `Node::While` / `Node::For`）。
            let idx = ctx.flush_block_at(CoreTerminator::Jump(0)); // 占位，稍后回填
            match ctx.loop_stack.last_mut() {
                Some(l) => l.breaks.push(idx),
                None => {
                    // 循环外的 `break`（parser 拒绝出现）⇒ 该块不可达。
                    // 用 `Return` 而不是 `Unreachable` —— 后者与「未回填的占位」
                    // 同形，会被守卫误判。
                    ctx.blocks[idx].terminator = CoreTerminator::Return(None);
                }
            }
        }
        Node::Continue { .. } => {
            // `continue` 的目标是所在循环的**条件块**（进入循环体时已知）
            // ⇒ 直接终结当前块，无需回填。
            let target = ctx.loop_stack.last().map(|l| l.cond_id);
            match target {
                Some(t) => {
                    ctx.flush_block_at(CoreTerminator::Jump(t));
                }
                None => {
                    ctx.flush_block_at(CoreTerminator::Return(None));
                }
            }
        }

        // ── 绑定 ──
        Node::Let {
            name, value, body, ..
        } => {
            ctx.emit(CoreInst::EnvStore(name.clone(), *value));
            lower_block(ctx, body);
        }
        Node::Assign { name, value, .. } => {
            ctx.emit(CoreInst::EnvMutate(name.clone(), *value));
        }
        Node::IndexAssign {
            obj, idx, value, ..
        } => {
            ctx.emit(CoreInst::IndexAssign(*obj, *idx, *value));
        }

        // ── 闭包/函数 ──
        Node::FnDef {
            name, params, body, ..
        } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            // v0.104.6 D481：捕获变量此前恒为空，且注释谎称「由 EHIR 分析
            // 确定」—— EHIR **并没有**这项分析。改为**过近似**：体内被读取
            // （`Node::Variable`）且未被体内绑定（`Node::Let`）的名字。
            // 过近似可能多含（体内嵌套闭包读到的名字也会被算进来），但不再
            // 是「空 + 假话」。
            let captures = capture_names(body);
            let core_body = lower_body_to_core(ctx, body);
            let closure_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::ClosureCreate {
                dst: closure_reg,
                params: param_names,
                captures,
                body: core_body,
            });
            ctx.emit(CoreInst::EnvStore(name.clone(), closure_reg));
        }

        // ── 效果 ──
        Node::Perform {
            dst, effect, args, ..
        } => {
            ctx.emit(CoreInst::EffectPerform {
                dst: *dst,
                label: EffectLabel::from_name(effect),
                args: args.clone(),
            });
        }
        Node::Handle {
            effect,
            body,
            handler,
            ..
        } => {
            let handler_reg = ctx.alloc_reg();
            // handler 降维为闭包
            let handler_body = lower_body_to_core(ctx, handler);
            ctx.emit(CoreInst::ClosureCreate {
                dst: handler_reg,
                params: vec!["__arg0".to_string()],
                // v0.104.6 D481：同上（过近似，不再是空 + 假注释）。
                captures: capture_names(handler),
                body: handler_body,
            });
            ctx.emit(CoreInst::EffectInstall {
                label: EffectLabel::from_name(effect),
                handler: handler_reg,
            });
            lower_block(ctx, body);
            ctx.emit(CoreInst::EffectRestore {
                label: EffectLabel::from_name(effect),
            });
        }

        // ── 声明类 → 编译期消失 ──
        Node::TypeAlias { .. }
        | Node::EnumDef { .. }
        | Node::StructDef { .. }
        | Node::TraitDef { .. }
        | Node::ImplDef { .. }
        | Node::Import { .. }
        | Node::MacroDef { .. } => {
            // 编译期注册，Core 层无操作
        }

        // ── TEA → 编译期注册 ──
        Node::ModelDef { .. }
        | Node::MsgDef { .. }
        | Node::UpdateDef { .. }
        | Node::AppDef { .. } => {
            // TEA 定义在 EHIR 阶段已注册到 env，Core 层无操作
        }

        // ── v0.104.6 D481：区域构造 → `CoreInst::Scope` ──
        //
        // 此前这 10 类构造在 Core 层是**原地摊平**的：只把体内容降进外层
        // CFG，构造本身**完全不可见**。D481 起统一表示成「区域」——
        // 体是**独立的嵌套代码区**（自己的块集合 + 入口 + 寄存器空间），
        // 形状**镜像 MIR 层**的嵌套 `MirFunction`
        // （`MirInst::Parallel { body: Box<MirFunction> }` 等）。
        //
        // 区域**内部**的并发 / 事务 / 可观测性语义仍在执行层（MIR），
        // Core 层只保证**结构可见**。
        Node::Export { names, decl, .. } => {
            let b = lower_body_to_core(ctx, decl);
            ctx.emit(CoreInst::Scope {
                kind: "export".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D485：被导出的**名字**（MIR 侧在声明体之后发
                // `ExportMark`，同序）。
                payload: ScopePayload::Export(names.clone()),
            });
        }
        Node::Parallel { body, .. } => {
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "parallel".to_string(),
                body: b,
                compensation: None,
                payload: ScopePayload::None,
            });
        }
        Node::Observe { config, body, .. } => {
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "observe".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D485：span 名（`observe` 的 kind 作前缀，与 MIR 侧
                // `h_observe` 的 `observe/{config}` 逐字一致）。
                payload: ScopePayload::Trace {
                    name: format!("observe/{}", config),
                    tags: Vec::new(),
                },
            });
        }
        Node::Span {
            name, tags, body, ..
        } => {
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "span".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D485：span 名 + 属性（此前全丢 ⇒ 区域看得见但
                // 执行不了）。
                payload: ScopePayload::Trace {
                    name: name.clone(),
                    tags: tags.clone(),
                },
            });
        }
        // ── v0.103: 命名 section 声明 → 与 TEA 定义同为编译期注册 ──
        Node::PromptSection { name, body, .. } => {
            // section 的构建在 MIR 执行层（`h_prompt_section` 绑定值到 env）；
            // Core 层只把 body 作为**区域**表示出来（D481）。
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "prompt_section".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D486：section 的**名字**（建值时要绑到环境）。
                payload: ScopePayload::Section { name: name.clone() },
            });
        }
        Node::DocumentSection { name, body, .. } => {
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "document_section".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D486：同上。
                payload: ScopePayload::Section { name: name.clone() },
            });
        }
        // ── v0.102: 声明式范式 ──
        Node::RelDef { name, clauses, .. } => {
            // v0.104.6 D486：关系定义**进** Core 产物了 —— 此前这里是**无操作**
            // ⇒ 关系根本没进 Core ⇒ `solve` 的目标构建体拿不到关系 ⇒
            // 关系搜索在 Core 侧跑不了（与 D486 的 solve 缺口同源）。
            //
            // 子句是**数据** ⇒ 原样带过（与 `Node::RelDef` 同款）。
            ctx.emit(CoreInst::RelDef {
                name: name.clone(),
                clauses: clauses.clone(),
            });
        }
        Node::Solve {
            dst,
            limit,
            query_vars,
            anon_vars,
            goal,
            ..
        } => {
            // 目标构建体作为**区域**表示（D481）；搜索语义在 MIR 执行层。
            let b = lower_body_to_core(ctx, goal);
            ctx.emit(CoreInst::Scope {
                kind: "solve".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D486：查询参数 + **解列表的结果寄存器**
                // （`dst` 在外层寄存器空间）—— 此前全丢 ⇒ solve 的值在 Core
                // 产物里**根本不存在**（与 D35/D58/D120 同族：结果寄存器没人写）。
                payload: ScopePayload::Solve {
                    dst: *dst,
                    limit: *limit,
                    query_vars: query_vars.clone(),
                    anon_vars: anon_vars.clone(),
                },
            });
        }

        // ── 编排 → Core 原语（载荷原样透传，D483）──
        Node::Orchestrate {
            input_var,
            result_var,
            kind,
            dst,
            ..
        } => {
            // v0.104.6 D483：Core 层**有了编排原语** —— 载荷**原样**透传
            // （witness 形态，与 `Node::Orchestrate` 同款，理由见 D473：
            // 编排是**数据构造语句**，载荷是数据而非 CFG 结构）。
            //
            // 此前这里是**空区域**（`Scope{kind:"orchestrate"}`，体为空）
            // ⇒ 产物里只看得见「这里有编排」，**载荷全丢**（agent 图 / 边条件 /
            // state schema / MoA / MoE 的 prompt）。
            ctx.emit(CoreInst::Orchestrate {
                input_var: input_var.clone(),
                result_var: result_var.clone(),
                kind: kind.clone(),
            });
            // 编排语句的值恒为 Nil（与 MIR 层同形）。
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }

        // ── 配置块 → 区域（D481）──
        Node::WithConfig {
            bindings,
            body,
            dst,
            ..
        } => {
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "with_config".to_string(),
                body: b,
                compensation: None,
                // v0.104.6 D485：配置**绑定**（名 + 值所在寄存器；值在外层
                // 寄存器空间）—— 此前全丢 ⇒ 区域看得见但执行不了。
                payload: ScopePayload::WithConfig(bindings.clone()),
            });
            // `with` 块的值恒为 Nil（与 MIR 层 D58 的同形契约）。
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }

        // ── 元编程 → quasiquote 原语（运行期重组为源码文本，D483）──
        Node::Quasiquote { dst, segments, .. } => {
            // v0.104.6 D483：Core 层**有了模板原语** —— 片段（静态文字 +
            // 寄存器引用）逐段带过。此前写 `Const(dst, Nil)` ⇒ **片段全丢**。
            //
            // 片段类型**复用** `crate::mir::QuasiquoteSegment`（不另立一份）：
            // 拼接语义由 `h_quasiquote` 唯一定义 ⇒ Core 执行器与 MIR 执行器
            // 逐字同一套规则（与 `EnumMatchArm.pattern` 复用 `self_match_pattern`
            // 同款理由）。
            let segments = segments
                .iter()
                .map(|seg| match seg {
                    FcfgSegment::Quote(s) => crate::mir::QuasiquoteSegment::Quote(s.clone()),
                    FcfgSegment::Unquote(r) => crate::mir::QuasiquoteSegment::Unquote(*r),
                    FcfgSegment::UnquoteSplice(r) => {
                        crate::mir::QuasiquoteSegment::UnquoteSplice(*r)
                    }
                })
                .collect();
            ctx.emit(CoreInst::Quasiquote {
                dst: *dst,
                segments,
            });
        }

        // ── 序列 ──
        Node::Sequence { nodes, .. } => {
            for n in nodes {
                lower_node(ctx, n);
            }
        }

        // ── 表达式语句 ──
        Node::Expr { reg, .. } => {
            ctx.emit(CoreInst::Expr(*reg));
        }

        // ── v0.104.6 D472：4 类新构造 → Core 层 ──
        //
        // Core 层是**影子/统计层**（产出不进生产路径，见 `mir/pipeline.rs`：
        // 管线产出的 MirFunction 来自 `lower_fcfg(&fcfg)`），且其基元集
        // 本就不覆盖全部 MirInst 变体（见 `core.rs` 的「CoreInst vs MirInst
        // 覆盖范围」表）。
        //
        // v0.104.6 D481 重新归类，D483 补齐：
        // - **worker / transaction** ⇒ 区域构造（体是独立嵌套代码区，
        //   形状镜像 MIR 层的嵌套 `MirFunction`）⇒ `CoreInst::Scope`；
        // - **eval / aggregate** ⇒ D483 起各有**自己的原语**
        //   （`CoreInst::Eval` / `CoreInst::Aggregate`），载荷原样带过。
        Node::Worker { body, dst, .. } => {
            let b = lower_body_to_core(ctx, body);
            ctx.emit(CoreInst::Scope {
                kind: "worker".to_string(),
                body: b,
                compensation: None,
                payload: ScopePayload::None,
            });
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }
        Node::Transaction {
            body,
            compensation,
            dst,
            ..
        } => {
            let b = lower_body_to_core(ctx, body);
            let c = lower_body_to_core(ctx, compensation);
            ctx.emit(CoreInst::Scope {
                kind: "transaction".to_string(),
                body: b,
                compensation: Some(c),
                payload: ScopePayload::None,
            });
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }
        Node::Eval {
            name,
            given,
            expects,
            tolerance,
            replay_path,
            dst,
            ..
        } => {
            // v0.104.6 D483：Core 层**有了断言原语** —— 断言名 / given /
            // expects / tolerance / replay_path 全部带过。此前写
            // `Const(dst, Nil)` ⇒ **载荷全丢**（连断言名都看不见）。
            //
            // **不补 Nil**：语句的值 = **given 表达式的值**（`dst` 与 `given`
            // 是同一个寄存器）—— 与 MIR 层逐字一致。补 Nil 会把那个寄存器
            // **覆盖成 Nil**，使语句的值与 MIR 层分叉。
            debug_assert_eq!(
                *dst, *given,
                "eval 语句的结果寄存器必须就是 given 的寄存器（与 MIR 层同契约）"
            );
            ctx.emit(CoreInst::Eval {
                name: name.clone(),
                given: *given,
                expects: expects.clone(),
                tolerance: *tolerance,
                replay_path: replay_path.clone(),
            });
        }
        Node::Aggregate {
            name, value, dst, ..
        } => {
            // v0.104.6 D483：同上 —— 聚合器名与被贡献的值都带过。
            //
            // **不补 Nil**：语句的值 = **被贡献表达式的值**（`dst` 与 `value`
            // 是同一个寄存器）—— 与 MIR 层逐字一致。
            debug_assert_eq!(
                *dst, *value,
                "aggregate 语句的结果寄存器必须就是被贡献值的寄存器（与 MIR 层同契约）"
            );
            ctx.emit(CoreInst::Aggregate {
                name: name.clone(),
                value: *value,
            });
        }
        // v0.104.6 D472：管道 + commit / rollback → Core 层透传
        Node::Pipe {
            dst, left, right, ..
        } => {
            // 管道 = 「用 right 调 left」（与 MIR 层 `h_pipe` 同形：
            // `call_value(&rhs_val, vec![lhs_val])`）⇒ **值调用**。
            //
            // v0.104.6 D482 更正：D481 把它写成 `Call(dst, left, [right])`
            // —— **左右 operand 反了**（成了「用 left 调 right」）。影子层里
            // 不会被执行，所以当时没暴露；现在 Core 层要能执行，语义必须对。
            ctx.emit(CoreInst::ClosureCall(*dst, *right, vec![*left]));
        }
        Node::Commit { dst, .. } => {
            // v0.104.6 D483：Core 层**有了事务原语** —— 提交这件事在产物里
            // **看得见**了。此前只有 `Const(dst, Nil)` ⇒ 与「别的语句返回 Nil」
            // **完全无法区分**。
            ctx.emit(CoreInst::Commit);
            // unit 语句 ⇒ 值恒为 Nil（与 MIR 层同形）。
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }
        Node::Rollback { dst, .. } => {
            // 同 commit：回滚在产物里看得见。
            ctx.emit(CoreInst::Rollback);
            ctx.emit(CoreInst::Const(*dst, Value::Nil));
        }
    }
}

fn lower_block(ctx: &mut CoreContext, block: &EhirBlock) {
    for node in &block.nodes {
        lower_node(ctx, node);
    }
}

/// 闭包体里**可能捕获**的变量名（过近似）。
///
/// v0.104.6 D481：`ClosureCreate.captures` 此前恒为空，且注释谎称
/// 「捕获变量由 EHIR 分析确定」—— EHIR **没有**这项分析。
///
/// 这里做**过近似**的自由变量分析：体内被**读取**（`Node::Variable`）或被
/// **改写**（`Node::Assign`，写外层绑定也需要捕获）的名字，减去体内
/// **绑定**（`Node::Let`）的名字。
///
/// 边界：嵌套闭包 / handler 的体**不**计入 —— 它们有自己的捕获集。
fn capture_names(body: &EhirBlock) -> Vec<String> {
    let mut reads: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    collect_captures(body, &mut reads, &mut binds);
    reads.retain(|n| !binds.contains(n));
    reads.dedup();
    reads
}

fn collect_captures(block: &EhirBlock, reads: &mut Vec<String>, binds: &mut Vec<String>) {
    for node in &block.nodes {
        collect_captures_node(node, reads, binds);
    }
}

fn collect_captures_node(node: &Ehir, reads: &mut Vec<String>, binds: &mut Vec<String>) {
    match node {
        Node::Variable { name, .. } => reads.push(name.clone()),
        // 赋值是对**外层绑定**的改写 ⇒ 同样需要捕获该名字。
        Node::Assign { name, .. } => reads.push(name.clone()),
        Node::Let { name, body, .. } => {
            binds.push(name.clone());
            collect_captures(body, reads, binds);
        }
        Node::Sequence { nodes, .. } => {
            for n in nodes {
                collect_captures_node(n, reads, binds);
            }
        }
        Node::If { then, else_, .. } => {
            collect_captures(then, reads, binds);
            if let Some(e) = else_ {
                collect_captures(e, reads, binds);
            }
        }
        Node::While { cond, body, .. } => {
            collect_captures(cond, reads, binds);
            collect_captures(body, reads, binds);
        }
        Node::For { body, .. } => collect_captures(body, reads, binds),
        Node::Match { arms, .. } => {
            for arm in arms {
                collect_captures(&arm.body, reads, binds);
            }
        }
        // 嵌套闭包 / handler / 宏体的捕获集是**它们自己的**，不计入外层。
        _ => {}
    }
}

/// 把一个**块体**降维成独立的 Core 代码区（子函数形态）。
///
/// v0.104.6 D480：此前只返回**首块**（`sub.blocks.into_iter().next()`）——
/// 体里的嵌套分支产生的其余块被**丢弃** ⇒ 分支目标指向**并不存在**的块。
/// D480 起返回**全部块**（`CoreFunction` 形态：块集合 + 入口 + 寄存器数）。
///
/// 子代码区**继承**外层的效果行（Core 层不按体拆分效果），但有**自己的
/// 寄存器空间**（闭包按名捕获，体内局部量不与外层撞号）。
fn lower_body_to_core(ctx: &mut CoreContext, block: &EhirBlock) -> CoreFunction {
    let mut sub = CoreContext::new(ctx.effects.clone());
    // v0.104.6 D482：**寄存器安全契约** —— 子体的 bump 分配器必须**跳过** FCFG
    // 预分配的寄存器（否则会覆盖它们）。此前 Core 层**没有**这一步 ⇒ 顶层
    // 降维的 `alloc_reg()` 与 FCFG 的预分配号**撞车** ⇒ 运行期越界
    // （`index out of bounds: the len is 1 but the index is 1`）。
    //
    // 与 MIR 层 `fcfg_lower::lower_fcfg` 的同一契约对齐。
    sub.next_reg = crate::mir::fcfg_lower::max_reg_in_nodes(&block.nodes) + 1;
    for node in &block.nodes {
        lower_node(&mut sub, node);
    }
    sub.finish_body(block.result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Literal, Span};
    use crate::mir::effect::EffectRow;
    use crate::mir::fcfg::{Block, TypeInfo};
    use crate::typeck::Type;

    const S: Span = Span { line: 1, column: 0 };

    fn lit_int_ehir(n: i64, reg: usize) -> Ehir {
        Node::Literal {
            reg,
            value: Literal::Int(n, S),
            span: S,
            meta: TypeInfo::new(Type::Int, EffectRow::Empty, S),
        }
    }

    #[test]
    fn ehir_literal_to_core_const() {
        let nodes = vec![lit_int_ehir(42, 0)];
        let func = ehir_to_core(&nodes, vec![], EffectRow::Empty);
        assert_eq!(func.blocks.len(), 1);
        assert_eq!(func.blocks[0].insts.len(), 1);
        match &func.blocks[0].insts[0] {
            CoreInst::Const(r, Value::Int(v)) => {
                assert_eq!(*r, 0);
                assert_eq!(*v, 42);
            }
            _ => panic!("expected Const"),
        }
    }

    #[test]
    fn ehir_binary_op() {
        let nodes = vec![Node::BinaryOp {
            dst: 2,
            lhs: 0,
            op: crate::common::BinaryOp::Add,
            rhs: 1,
            span: S,
            meta: TypeInfo::new(Type::Int, EffectRow::Empty, S),
        }];
        let func = ehir_to_core(&nodes, vec![], EffectRow::Empty);
        assert_eq!(func.blocks[0].insts.len(), 1);
        assert!(matches!(
            &func.blocks[0].insts[0],
            CoreInst::BinaryOp(2, 0, _, 1)
        ));
    }

    #[test]
    fn ehir_let_binding() {
        let nodes = vec![Node::Let {
            name: "x".to_string(),
            type_ann: None,
            value: 0,
            body: Block {
                nodes: vec![lit_int_ehir(1, 1)],
                result: Some(1),
            },
            span: S,
            meta: TypeInfo::unknown(S),
        }];
        let func = ehir_to_core(&nodes, vec![], EffectRow::Empty);
        assert!(func.blocks[0].insts.len() >= 2); // EnvStore + Const
    }
}
