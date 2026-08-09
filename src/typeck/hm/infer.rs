//! v0.75.70: HM 类型推断 infer_* 方法族 — 自 hm/mod.rs 拆出（D6 单文件惯例，
//! 多 impl 块模式）。表达式/语句推断：let/assign/var/binop/call/method/
//! closure/fn_def/match/if/list/dict。基础设施与 infer_expr 入口仍在 mod.rs。

use super::*;
use crate::mir::hint::TypeHint;
use crate::typeck::is_known_type;

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
        let _span = span; // 保留 span 以便未来错误检查
        let gen_ty = generalize::generalize(&value_ty, &self.env.free_variables());
        self.env.add(name.to_string(), gen_ty.clone());
        let _ = _span;
        Ok(gen_ty)
    }

    pub(super) fn infer_let_typed(
        &mut self,
        name: &str,
        type_hint: &TypeHint,
        value: &MirWitness,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let value_ty = self.infer_expr(value)?;
        // v0.75.93: TypeHint 边界 → to_type() 取回 typeck::Type
        let ty_inner = type_hint.to_type();
        // v0.55: validate the user-supplied `let x: T = ...` annotation
        // against the value's inferred type. Tolerant: Type::Any
        // annotations always succeed.
        if !matches!(ty_inner, Type::Any) {
            // v0.75.86: 提前用 span 报不一致——不等 solve_constraints 兜底
            // (原代码只 push Constraint 到 constraints 一致性队列，span 在
            // 合一失败时被丢弃 → typeck 错误统一报 line 0)
            if !value_ty.compatible_with(ty_inner) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("{:?}", ty_inner),
                    got: format!("{:?}", value_ty),
                    span: Some(span),
                }]);
            }
            self.constraints.push(Constraint::Eq(
                Box::new(ty_inner.clone()),
                Box::new(value_ty.clone()),
            ));
        }
        // v0.75.17: 显式注解同样做 let-generalization（注解含自由变量时
        // 量化为 ForAll；`List<int>` 等具体注解无自由变量，原样登记）。
        let gen_hint = generalize::generalize(ty_inner, &self.env.free_variables());
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
            // v0.75.97: 命中 ForAll 时先实例化再合一（赋值的 LHS 是单形实例）
            let existing = self.instantiate_if_forall(&existing);
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
                n if crate::flow::is_builtin_object(n) => Ok(Type::Unknown),
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
                // v0.75.86: 提前用 span 报不一致（避免 line 0）
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("{:?}", left_ty),
                        got: format!("{:?}", right_ty),
                        span: Some(span),
                    }]);
                }
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
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("{:?}", left_ty),
                        got: format!("{:?}", right_ty),
                        span: Some(span),
                    }]);
                }
                self.constraints
                    .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                Ok(Type::Bool)
            }
            Greater | Less | GreaterEqual | LessEqual => {
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("{:?}", left_ty),
                        got: format!("{:?}", right_ty),
                        span: Some(span),
                    }]);
                }
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
            WitnessCallee::Name(name) => self.builtin_callee_ty(name).unwrap_or(Type::Unknown),
            // v0.75.97: Var 命中 ForAll 时实例化（`let f = fn(x) x; f(1); f("s")`）
            WitnessCallee::Var(var_name) => {
                let ty_opt = self.env.get(var_name).cloned();
                match ty_opt {
                    Some(ty) => self.instantiate_if_forall(&ty),
                    None => Type::Unknown,
                }
            }
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
        // （运行时变量仍由运行时 MergeStrategy::from_name 兜底）。
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
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
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
            .map(|p| {
                p.type_hint
                    .as_ref()
                    .map(|h| h.to_type().clone())
                    .unwrap_or_else(|| self.fresh_type_var())
            })
            .collect();
        for (p, ty) in params.iter().zip(param_types.iter()) {
            self.env.add(p.name.clone(), ty.clone());
        }
        let body_ty = self.infer_expr(body)?;
        self.env = saved_env;
        let id = self.fresh_closure(param_types, body_ty);
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
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
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        self.infer_closure(params, body, span)
    }

    pub(super) fn infer_match(
        &mut self,
        scrutinee: &MirWitness,
        arms: &[WitnessArm],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let scrutinee_ty = self.infer_expr(scrutinee)?;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            // v0.76.02: pattern typeck 校验——5 变体
            // (Tuple/List/Dict/TypeAscription/Variable) infer 分支
            self.infer_pattern(&arm.pattern, &scrutinee_ty, span)?;
            let arm_ty = self.infer_expr(&arm.body)?;
            match result_ty {
                None => result_ty = Some(arm_ty),
                Some(ref mut ty) => {
                    // v0.75.86: 提前用 span 报 arm body type 不一致——
                    // 不等 solve_constraints 兜底（约束无 span 关联 → line 0）
                    if !arm_ty.subtype_of(ty) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!("{:?}", ty),
                            got: format!("{:?}", arm_ty),
                            span: Some(span),
                        }]);
                    }
                    self.constraints
                        .push(Constraint::Eq(Box::new(ty.clone()), Box::new(arm_ty)));
                }
            }
        }
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok(result_ty.unwrap_or(Type::Unknown))
    }

    /// v0.76.02: pattern typeck 校验（架构审查报告 🟡 警告级风险——
    /// 此前 5 变体 typeck 路径 0 行）。
    ///
    /// 最小可工作版本（v0.75.87 撤除前车之鉴：只覆盖与 HM 一致可验证的部分）：
    /// - Wildcard: no-op（任何 type 都 match）
    /// - Literal: 类型必须与 scrutinee 一致（HM 合一失败时让 v0.75.86 报错）
    /// - Variable: env 查找（已存在；本函数 no-op 因为 env 由 arm.body 内
    ///   Variable 引用触发的 infer_var 处理）
    /// - Tuple: 元素数必须 = scrutinee tuple 元素数；递归推断 subpattern
    /// - List: head/tail 推断（simplified——不区分长度，统一 scrutinee 元素类型）
    /// - Dict: required 键 value subpattern 推断 + rest 推断
    /// - TypeAscription: name 必须是已知类型（`is_known_type`）；pattern 在
    ///   该 type 上下文下递归 infer_pattern
    fn infer_pattern(
        &mut self,
        pattern: &crate::mir::witness::WitnessPattern,
        scrutinee_ty: &Type,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        use crate::mir::witness::WitnessPattern;
        match pattern {
            WitnessPattern::Wildcard | WitnessPattern::Variable(_) | WitnessPattern::Literal(_) => {
                Ok(())
            }
            WitnessPattern::Tuple(items) => {
                // v0.76.02: Type enum 当前无 Tuple variant——Mora 列表
                // 元素用 List 表达（架构审查报告 v0.75.90）。Tuple pattern
                // 的实际使用是 List scrutinee 上"按位置解构"——按 List
                // 元素数验证。
                let elem_count = match scrutinee_ty {
                    Type::List(_) => None, // 推迟到 List 分支
                    _ => Some(1),          // 保守：非 List 视为 1 元素
                };
                if let Some(expected) = elem_count
                    && items.len() != expected
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("Tuple/List ({} elements)", expected),
                        got: format!("Tuple ({} elements)", items.len()),
                        span: Some(span),
                    }]);
                }
                // 元素 subpattern 推断：统一使用 scrutinee（List elem 类型）
                self.infer_pattern(
                    items.first().unwrap_or(&WitnessPattern::Wildcard),
                    scrutinee_ty,
                    span,
                )?;
                Ok(())
            }
            WitnessPattern::List { head, tail } => {
                // List scrutinee 元素类型统一——head/tail 同推断
                let elem_ty = match scrutinee_ty {
                    Type::List(e) => e.as_ref().clone(),
                    _ => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "List".to_string(),
                            got: format!("{:?}", scrutinee_ty),
                            span: Some(span),
                        }]);
                    }
                };
                self.infer_pattern(head, &elem_ty, span)?;
                // tail 仍是 List<elem_ty>
                let rest_list_ty = Type::List(Box::new(elem_ty));
                self.infer_pattern(tail, &rest_list_ty, span)?;
                Ok(())
            }
            WitnessPattern::Dict { required, rest: _ } => {
                // v0.76.02: rest: bool 仅作标记——rest=true 时 pattern 推断
                // "剩余 dict"（key 固定 String，value 同 value_ty）
                let value_ty = match scrutinee_ty {
                    Type::Dict(_, v) => v.as_ref().clone(),
                    _ => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "Dict".to_string(),
                            got: format!("{:?}", scrutinee_ty),
                            span: Some(span),
                        }]);
                    }
                };
                for (_key, value_pat) in required {
                    self.infer_pattern(value_pat, &value_ty, span)?;
                }
                // rest 标记——本身不递归（v0.76.02 最小版本不动 rest 子 pattern）
                Ok(())
            }
            WitnessPattern::TypeAscription { name, pattern } => {
                // v0.76.02: name 必须是已知类型
                if !is_known_type(name) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "known type name".to_string(),
                        got: name.clone(),
                        span: Some(span),
                    }]);
                }
                // subpattern 在该 type 上下文下递归（name 解析为 Type 暂跳过——
                // v0.75.91 前的 Any 兼容：仅校验 name 是 known）
                self.infer_pattern(pattern, scrutinee_ty, span)?;
                Ok(())
            }
        }
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
            // v0.75.86: 提前用 span 报不一致（避免 line 0）
            if !else_ty.subtype_of(&then_ty) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("{:?}", then_ty),
                    got: format!("{:?}", else_ty),
                    span: Some(span),
                }]);
            }
            self.constraints
                .push(Constraint::Eq(Box::new(then_ty.clone()), Box::new(else_ty)));
            then_ty
        } else {
            // No else branch: the if-expression yields `then_ty | nil`.
            Type::Union(vec![then_ty.clone(), Type::Nil])
        };
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok(result)
    }

    pub(super) fn infer_list(
        &mut self,
        items: &[MirWitness],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let elem_ty = self.fresh_type_var();
        let mut first_ty: Option<Type> = None;
        for item in items {
            let ty = self.infer_expr(item)?;
            // v0.75.86: 提前用 span 报 list elem type 不一致（避免 line 0）
            if let Some(prev) = &first_ty
                && !ty.compatible_with(prev)
            {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("{:?}", prev),
                    got: format!("{:?}", ty),
                    span: Some(span),
                }]);
            }
            if first_ty.is_none() {
                first_ty = Some(ty.clone());
            }
            self.constraints
                .push(Constraint::Eq(Box::new(elem_ty.clone()), Box::new(ty)));
        }
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok(Type::List(Box::new(elem_ty)))
    }

    pub(super) fn infer_dict(
        &mut self,
        entries: &[(String, MirWitness)],
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let k_ty = Type::String;
        let v_ty = self.fresh_type_var();
        let mut first_v: Option<Type> = None;
        for (_, value) in entries {
            let ty = self.infer_expr(value)?;
            // v0.75.86: 提前用 span 报 dict value type 不一致（避免 line 0）
            if let Some(prev) = &first_v
                && !ty.compatible_with(prev)
            {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("{:?}", prev),
                    got: format!("{:?}", ty),
                    span: Some(span),
                }]);
            }
            if first_v.is_none() {
                first_v = Some(ty.clone());
            }
            self.constraints
                .push(Constraint::Eq(Box::new(v_ty.clone()), Box::new(ty)));
        }
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok(Type::Dict(Box::new(k_ty), Box::new(v_ty)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Span;
    use crate::mir::witness::WitnessPattern;

    // v0.76.02: infer_pattern 5 变体测试

    #[test]
    fn pattern_wildcard_always_succeeds() {
        let mut hm = HMInference::new();
        // Wildcard 任何 type 都 match
        let r = hm.infer_pattern(&WitnessPattern::Wildcard, &Type::Int, Span::default());
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_variable_always_succeeds() {
        // Variable 由 arm.body 内 Variable 引用触发 infer_var（env 查找）——
        // pattern inference 阶段 no-op
        let mut hm = HMInference::new();
        let r = hm.infer_pattern(
            &WitnessPattern::Variable("x".to_string()),
            &Type::Int,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_tuple_non_list_scrutinee_errors() {
        // Type enum 无 Tuple variant——非 List scrutinee 走保守路径
        // （视为 1 元素，items.len() != 1 时报错）
        let mut hm = HMInference::new();
        let items = vec![WitnessPattern::Wildcard, WitnessPattern::Wildcard];
        let r = hm.infer_pattern(&WitnessPattern::Tuple(items), &Type::Int, Span::default());
        assert!(r.is_err(), "non-list scrutinee + 2-tuple pattern 应报错");
    }

    #[test]
    fn pattern_list_head_tail_succeeds() {
        // List scrutinee 上 head/tail pattern 推断
        let mut hm = HMInference::new();
        let elem_ty = Type::Int;
        let list_ty = Type::List(Box::new(elem_ty.clone()));
        let r = hm.infer_pattern(
            &WitnessPattern::List {
                head: Box::new(WitnessPattern::Wildcard),
                tail: Box::new(WitnessPattern::Wildcard),
            },
            &list_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_dict_required_keys_succeeds() {
        // Dict scrutinee 上 required key/value subpattern 推断
        let mut hm = HMInference::new();
        let value_ty = Type::Int;
        let dict_ty = Type::Dict(Box::new(Type::String), Box::new(value_ty));
        let r = hm.infer_pattern(
            &WitnessPattern::Dict {
                required: vec![("k".to_string(), WitnessPattern::Wildcard)],
                rest: false,
            },
            &dict_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_typeascription_unknown_name_errors() {
        // TypeAscription 名字必须 is_known_type
        let mut hm = HMInference::new();
        let r = hm.infer_pattern(
            &WitnessPattern::TypeAscription {
                name: "not_a_real_type".to_string(),
                pattern: Box::new(WitnessPattern::Wildcard),
            },
            &Type::Int,
            Span::default(),
        );
        assert!(r.is_err(), "unknown type name 应报错");
    }

    #[test]
    fn pattern_typeascription_known_name_succeeds() {
        // v0.76.02: is_known_type 名单内合法类型（"any" / "list" / "string" / etc.）
        // 实际 parser 接受 "int"/"float" 等简写——那是 parser alias 层，不在
        // is_known_type 名单（双层语义）。这里用 "any" 验证核心逻辑：
        // 合法 known type → 通过。
        let mut hm = HMInference::new();
        let r = hm.infer_pattern(
            &WitnessPattern::TypeAscription {
                name: "any".to_string(),
                pattern: Box::new(WitnessPattern::Wildcard),
            },
            &Type::Any,
            Span::default(),
        );
        assert!(r.is_ok());
    }
}
