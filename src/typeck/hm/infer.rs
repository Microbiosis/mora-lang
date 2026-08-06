//! v0.75.70: HM 类型推断 infer_* 方法族 — 自 hm/mod.rs 拆出（D6 单文件惯例，
//! 多 impl 块模式）。表达式/语句推断：let/assign/var/binop/call/method/
//! closure/fn_def/match/if/list/dict。基础设施与 infer_expr 入口仍在 mod.rs。

use super::*;

impl HMInference {
    pub(super) fn infer_let(
        &mut self,
        name: &str,
        value: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        // v0.75.17: let-generalization — 量化为不在 env 中的自由变量
        // （标准 HM：Γ ⊢ let x = e in body : ∀α₁...αₙ.τ，其中
        // {α₁...αₙ} = FV(τ) \ FV(Γ)）。
        let gen_ty = generalize::generalize(&value_ty, &self.env.free_variables());
        self.env.add(name.to_string(), gen_ty.clone());
        let _ = span;
        Ok(gen_ty)
    }

    pub(super) fn infer_let_typed(
        &mut self,
        name: &str,
        type_hint: &Type,
        value: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        // v0.55: validate the user-supplied `let x: T = ...` annotation
        // against the value's inferred type. Tolerant: Type::Any
        // annotations always succeed.
        if !matches!(type_hint, Type::Any) {
            // v0.75.86: 提前用 span 报不一致——不等 solve_constraints 兜底
            // (原代码只 push Constraint 到 constraints 一致性队列，span 在
            // 合一失败时被丢弃 → typeck 错误统一报 line 0)
            if !value_ty.compatible_with(type_hint) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("{:?}", type_hint),
                    got: format!("{:?}", value_ty),
                    span: Some(span),
                }]);
            }
            self.constraints.push(Constraint::Eq(
                Box::new(type_hint.clone()),
                Box::new(value_ty.clone()),
            ));
        }
        // v0.75.17: 显式注解同样做 let-generalization（注解含自由变量时
        // 量化为 ForAll；`List<int>` 等具体注解无自由变量，原样登记）。
        let gen_hint = generalize::generalize(type_hint, &self.env.free_variables());
        self.env.add(name.to_string(), gen_hint.clone());
        let _ = span;
        Ok(gen_hint)
    }

    pub(super) fn infer_assign(
        &mut self,
        target: &str,
        value: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        let current = self.env.get(target).cloned();
        if let Some(existing) = current {
            // v0.75.17: 命中 ForAll 时先实例化再合一（赋值的 LHS 是单形实例）
            let existing = match existing {
                Type::ForAll(_, _) => self.instantiate_type(&existing),
                other => other,
            };
            self.constraints.push(Constraint::Eq(
                Box::new(existing),
                Box::new(value_ty.clone()),
            ));
        } else {
            return Err(vec![TypeError::UnboundVariable {
                name: target.to_string(),
                span,
            }]);
        }
        Ok(value_ty)
    }

    pub(super) fn infer_var(&mut self, name: &str, span: Span) -> Result<Type, Vec<TypeError>> {
        match self.env.get(name) {
            // v0.75.17: env 命中 ForAll → 实例化（let-polymorphism 展开）。
            // 可变借用问题：先克隆 env 条目，再走 &mut self 的实例化路径。
            Some(ty) if matches!(ty, Type::ForAll(_, _)) => {
                let ty = ty.clone();
                Ok(self.instantiate_type(&ty))
            }
            Some(ty) => Ok(ty.clone()),
            // v0.75.84: 全局内置对象名（ai/web/json/file/memory/agent）——
            // 非变量绑定，typeck 识别为对应模块类型（ai → AiModule 等）。
            // 此前 `ai.chat(...)` 报 "Unbound variable 'ai'"（运行时 arm
            // v0.75.84 补回后 typeck 仍是缺口）。
            None => match name {
                "ai" => Ok(Type::AiModule),
                "agent" => Ok(Type::Agent),
                n if crate::flow::is_builtin_object(n) => Ok(Type::Any),
                _ => Err(vec![TypeError::UnboundVariable {
                    name: name.to_string(),
                    span,
                }]),
            },
        }
    }

    pub(super) fn infer_binop(
        &mut self,
        op: &crate::common::BinaryOp,
        left: &MirWitness,
        right: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        use crate::common::BinaryOp::*;
        let left_ty = self.infer_expr(left)?;
        let right_ty = self.infer_expr(right)?;
        let result_ty = self.fresh_type_var();
        match op {
            Add | Sub | Mul | Div | Mod => {
                self.constraints.push(Constraint::Eq(
                    Box::new(left_ty.clone()),
                    Box::new(result_ty.clone()),
                ));
                self.constraints.push(Constraint::Eq(
                    Box::new(right_ty.clone()),
                    Box::new(result_ty.clone()),
                ));
                let _ = span;
                Ok(result_ty)
            }
            Equal | NotEqual => {
                self.constraints
                    .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                Ok(Type::Bool)
            }
            Greater | Less | GreaterEqual | LessEqual => {
                self.constraints
                    .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                Ok(Type::Bool)
            } // v0.55: Or/And are WitnessKind variants (short-circuit),
              // handled directly in infer_expr, not BinaryOp variants.
              // BinaryOp 已穷尽（11 变体全部覆盖）— 无需 `_` 兜底。
        }
    }

    pub(super) fn infer_call(
        &mut self,
        callee: &WitnessCallee,
        args: &[MirWitness],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let callee_ty = match callee {
            WitnessCallee::Name(name) => self.builtin_callee_ty(name).unwrap_or(Type::Any),
            // v0.75.17: Var 命中 ForAll 时实例化（`let f = fn(x) x; f(1); f("s")`）
            WitnessCallee::Var(var_name) => match self.env.get(var_name) {
                Some(ty) if matches!(ty, Type::ForAll(_, _)) => {
                    let ty = ty.clone();
                    self.instantiate_type(&ty)
                }
                Some(ty) => ty.clone(),
                None => Type::Any,
            },
            WitnessCallee::Evaluated(expr) => self.infer_expr(expr)?,
            WitnessCallee::Builtin(op) => self.builtin_type(op)?,
            // v0.75.16: Method 调用（parser 现产出 WitnessCallee::Method）— 走
            // method_signature 推断（receiver 类型 + 参数约束 + 返回类型）。
            WitnessCallee::Method(_, _) => {
                // 第一个 arg 是 receiver 表达式；构造临时 MethodCall 语义。
                // 直接委托 infer_method_call：receiver = args[0], 后续为参数。
                let (recv, method_args) = match args.split_first() {
                    Some((r, rest)) => (r, rest),
                    None => {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: 0,
                            span,
                        }]);
                    }
                };
                let method = match callee {
                    WitnessCallee::Method(_, m) => m.clone(),
                    _ => unreachable!(),
                };
                return self.infer_method_call(recv, &method, method_args, span);
            }
        };
        let arg_types: Vec<Type> = args
            .iter()
            .map(|a| self.infer_expr(a))
            .collect::<Result<Vec<_>, _>>()?;

        // v0.75.24: merge_with(key, strategy) 的策略名字面量编译期校验 —
        // 非法策略（静态字符串）在 typeck 阶段拦截，不再留到运行时
        // （动态传入的变量仍由运行时 MergeStrategy::from_name 兜底）。
        if let WitnessCallee::Name(name) = callee
            && name == "merge_with"
            && let Some(WitnessKind::Literal(crate::common::Literal::String(s, _))) =
                args.get(1).map(|a| &a.kind)
            && crate::value::MergeStrategy::from_name(s).is_none()
        {
            return Err(vec![TypeError::InvalidLiteral {
                what: "merge_with strategy".to_string(),
                value: s.clone(),
                span: Some(span),
            }]);
        }

        if let Some(sig) = self.closure_sig(&callee_ty).cloned() {
            if sig.arity != arg_types.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: sig.arity,
                    actual: arg_types.len(),
                    span,
                }]);
            }
            for (param_ty, arg_ty) in sig.params.iter().zip(arg_types.iter()) {
                self.constraints.push(Constraint::Eq(
                    Box::new(param_ty.clone()),
                    Box::new(arg_ty.clone()),
                ));
            }
            Ok(sig.return_type)
        } else {
            // Unknown callee type: introduce a fresh return and
            // constrain all argument slots to be compatible with
            // whatever the callee happens to be.
            let ret = self.fresh_type_var();
            for arg_ty in &arg_types {
                self.constraints.push(Constraint::Eq(
                    Box::new(arg_ty.clone()),
                    Box::new(callee_ty.clone()),
                ));
            }
            let _ = ret.clone();
            Ok(ret)
        }
    }

    pub(super) fn infer_method_call(
        &mut self,
        receiver: &MirWitness,
        method: &str,
        args: &[MirWitness],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let recv_ty = self.infer_expr(receiver)?;
        let arg_types: Vec<Type> = args
            .iter()
            .map(|a| self.infer_expr(a))
            .collect::<Result<Vec<_>, _>>()?;

        // v0.55: enforce arity from the dispatch table. The signature
        // already includes `self` as its first parameter, so the user
        // arity we compare against is `sig.params.len() - 1`.
        // v0.75.84: 尾部 dict 配置参数（ai.chat(prompt, {model: ...})）为
        // 可选——arity 下限是签名 user 参数数，多传 dict 不报 ArityMismatch。
        if let Some(sig) = crate::typeck::dispatch::method_signature(&recv_ty, method) {
            let user_arity = sig.params.len().saturating_sub(1);
            let extra_configurable = arg_types
                .iter()
                .skip(user_arity)
                .all(|t| matches!(t, Type::Dict(_, _)));
            if arg_types.len() < user_arity || (arg_types.len() > user_arity && !extra_configurable)
            {
                return Err(vec![TypeError::ArityMismatch {
                    expected: user_arity,
                    actual: arg_types.len(),
                    span,
                }]);
            }
            for (param, arg_ty) in sig.params.iter().skip(1).zip(arg_types.iter()) {
                self.constraints.push(Constraint::Eq(
                    Box::new(param.1.clone()),
                    Box::new(arg_ty.clone()),
                ));
            }
        }

        let return_ty = crate::typeck::dispatch::method_return_type(&recv_ty, method);
        let _ = span;
        Ok(return_ty)
    }

    // v0.75.20: infer_pipe 已删——WitnessKind::Pipe 死变体移除，`|>` 在
    // parse_pipe 脱糖为 Call（right(left)），HM 走 infer_call。

    pub(super) fn infer_closure(
        &mut self,
        params: &[WitnessParam],
        body: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let saved_env = self.env.clone();
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| p.type_hint.clone().unwrap_or_else(|| self.fresh_type_var()))
            .collect();
        for (p, ty) in params.iter().zip(param_types.iter()) {
            self.env.add(p.name.clone(), ty.clone());
        }
        let body_ty = self.infer_expr(body)?;
        self.env = saved_env;
        let id = self.fresh_closure(param_types, body_ty);
        let _ = span;
        Ok(id)
    }

    pub(super) fn infer_fn_def(
        &mut self,
        params: &[WitnessParam],
        body: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        // fn name(params) = body  is treated like an immediately-bound
        // closure; the name registration is the caller's responsibility.
        let _ = span;
        self.infer_closure(params, body, span)
    }

    pub(super) fn infer_match(
        &mut self,
        scrutinee: &MirWitness,
        arms: &[WitnessArm],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let _ = self.infer_expr(scrutinee)?;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            let arm_ty = self.infer_expr(&arm.body)?;
            match result_ty {
                None => result_ty = Some(arm_ty),
                Some(ref mut ty) => {
                    self.constraints
                        .push(Constraint::Eq(Box::new(ty.clone()), Box::new(arm_ty)));
                }
            }
        }
        let _ = span;
        Ok(result_ty.unwrap_or(Type::Any))
    }

    pub(super) fn infer_if(
        &mut self,
        cond: &MirWitness,
        then_branch: &MirWitness,
        else_branch: Option<&MirWitness>,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let _ = self.infer_expr(cond)?;
        let then_ty = self.infer_expr(then_branch)?;
        let result = if let Some(e) = else_branch {
            let else_ty = self.infer_expr(e)?;
            // Both branches must produce the same type.
            self.constraints
                .push(Constraint::Eq(Box::new(then_ty.clone()), Box::new(else_ty)));
            then_ty
        } else {
            // No else branch: the if-expression yields `then_ty | nil`.
            Type::Union(vec![then_ty.clone(), Type::Nil])
        };
        let _ = span;
        Ok(result)
    }

    pub(super) fn infer_list(
        &mut self,
        items: &[MirWitness],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let elem_ty = self.fresh_type_var();
        for item in items {
            let ty = self.infer_expr(item)?;
            self.constraints
                .push(Constraint::Eq(Box::new(elem_ty.clone()), Box::new(ty)));
        }
        let _ = span;
        Ok(Type::List(Box::new(elem_ty)))
    }

    pub(super) fn infer_dict(
        &mut self,
        entries: &[(String, MirWitness)],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let k_ty = Type::String;
        let v_ty = self.fresh_type_var();
        for (_, value) in entries {
            let ty = self.infer_expr(value)?;
            self.constraints
                .push(Constraint::Eq(Box::new(v_ty.clone()), Box::new(ty)));
        }
        let _ = span;
        Ok(Type::Dict(Box::new(k_ty), Box::new(v_ty)))
    }
}
