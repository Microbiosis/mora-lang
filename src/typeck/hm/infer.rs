//! v0.75.70: HM 类型推断 infer_* 方法族 — 自 hm/mod.rs 拆出（D6 单文件惯例，
//! 多 impl 块模式）。表达式/语句推断：let/assign/var/binop/call/method/
//! closure/fn_def/match/if/list/dict。基础设施与 infer_expr 入口仍在 mod.rs。

use super::*;
use crate::mir::hint::TypeHint;
use crate::typeck::is_known_type;
use std::collections::HashSet;

/// v0.90.5: 判断类型是否为具体数值类型（Int 或 Float）。
/// TypeVar 不在此列 — Numeric 约束要求两侧均为已解析数值，
/// TypeVar 应通过 Eq 约束路径走 unification。
/// v0.91: BigInt 也是数值类型（Int + BigInt → BigInt promotion）。
fn is_numeric(ty: &crate::typeck::Type) -> bool {
    matches!(
        ty,
        crate::typeck::Type::Int | crate::typeck::Type::Float | crate::typeck::Type::BigInt
    )
}

/// 数值塔的提升结果（与 `unify::solve` 的 `Constraint::Numeric` 同口径）：
/// `Int⊗Int→Int`；含 `Float` → `Float`；含 `BigInt` → `BigInt`。
///
/// v0.104.6 D24：广播算术（spec §12.3.1）算结果元素类型时用。
fn promote_numeric(a: &crate::typeck::Type, b: &crate::typeck::Type) -> crate::typeck::Type {
    use crate::typeck::Type as T;
    match (a, b) {
        (T::Int, T::Int) => T::Int,
        (T::BigInt, T::BigInt) => T::BigInt,
        (T::BigInt, _) | (_, T::BigInt) => T::BigInt,
        (T::Float, _) | (_, T::Float) => T::Float,
        // TypeVar / 未知：按 Float 兜底（与 solve 阶段的宽松策略一致）
        _ => T::Float,
    }
}

/// 剥掉 curried `Arrow` 的全部参数层，返回其**声明的结果类型**。
///
/// v0.104.6 D68：只对**内建**调用点使用。内建的返回类型永远不是函数
/// （`builtin_callee_ty` / `builtin_signatures` 里没有一条返回 `Arrow`），
/// 所以剥到最里层是安全的；而用户闭包可能真的返回函数（`fn(x) fn(y) … end`），
/// 那里绝不能剥。
fn peel_all_arrows(ty: &Type) -> Type {
    let mut t = ty.clone();
    while let Type::Arrow(_, out, _) = t {
        t = *out;
    }
    t
}

impl HMInference {
    pub(super) fn infer_let(
        &mut self,
        name: &str,
        value: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (value_ty, value_row) = self.infer_expr(value)?;
        // v0.75.17: let-generalization — 量化为不在 env 中的自由变量
        // （标准 HM：Γ ⊢ let x = e in body : ∀α₁...αₙ.τ，其中
        // {α₁...αₙ} = FV(τ) \ FV(Γ)）。
        let _span = span; // 保留 span 以便未来错误检查
        let gen_ty = generalize::generalize(&value_ty, &self.env.free_variables());
        self.env.add(name.to_string(), gen_ty.clone());
        // v0.96: let 绑定闭包/函数 —— 把定义行登记进 fn_effect_rows。
        // 零参闭包的类型是裸 TypeVar（无 Arrow 层可携带行），必须按
        // 定义 span 从 closure_rows 取回，否则跨函数效果传播漏检。
        if matches!(
            value.kind,
            WitnessKind::Closure { .. } | WitnessKind::FnDef { .. }
        ) && let Some(row) = self.closure_rows.get(&value.span)
        {
            self.fn_effect_rows.insert(name.to_string(), row.clone());
        }
        let _ = span;
        Ok((gen_ty, value_row))
    }

    pub(super) fn infer_let_typed(
        &mut self,
        name: &str,
        type_hint: &TypeHint,
        value: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let ty_inner = type_hint.to_type();
        // v0.83: 提前 field-by-field 验证（如果是 let x: TeaModel = {field: val, ...}）
        // 必须在 infer_expr 前做检查（infer_expr 把 field 名丢失到 Dict<String, V>）
        if let (
            Type::TeaModel {
                name: target_name,
                fields: target_fields,
            },
            WitnessKind::Dict(entries),
        ) = (&ty_inner, &value.kind)
        {
            self.check_dict_against_teamodel(entries, target_name, target_fields, span)?;
        }
        // v0.83: 同样支持 let x: TeaMsg = {tag: "Increment"}
        if let (
            Type::TeaMsg {
                name: target_name,
                variants: target_variants,
            },
            WitnessKind::Dict(entries),
        ) = (&ty_inner, &value.kind)
        {
            self.check_dict_against_teamsg(entries, target_name, target_variants, span)?;
        }
        let (value_ty, value_row) = self.infer_expr(value)?;
        // v0.75.93: TypeHint 边界 → to_type() 取回 typeck::Type
        // v0.55: validate the user-supplied `let x: T = ...` annotation
        // against the value's inferred type. Tolerant: Type::Any
        // annotations always succeed.
        //
        // v0.104.6 D82：`Type::TraitObject` 标注同样**不容��值侧**。
        //
        // `let x: dyn Any = 42` 的语义是「把 42 **强制转换**成 TraitObject」，
        // emit 侧也确实这么做了（`emit_definitions.rs` 会 emit
        // `MirInst::DynTrait { src, dst, trait_name }` 把值包成
        // `Value::TraitObject`）。但本函数拿**转换前**的 `value_ty`
        // （这里是 `Float`）去比对 `TraitObject` 标注
        // → `expected TraitObject { … }, got Float`。
        //
        // 后果：`dyn` 这条路径**整条走不通** —— emit 侧写好的 coercion
        // 永远到不了运行期。spec §13.1 把它列进类型文法、`mir_dyntrait.rs`
        // 还断言了 `MirInst::DynTrait` 的存在，而真实 CLI 里
        // `let x: dyn Foo = 1` 直接 exit 2。
        //
        // （`mir_dyntrait.rs` 之所以没抓��：它只查编译出的 **MIR**，
        // **从不跑 typeck** —— 与 D56 / D70 同型的假阳性测试。）
        //
        // 修法：`dyn Trait` 标注意味着「**任何**值都会被强制转换」，
        // 故值侧不加约束 —— 与 `Type::Any` 同等对待。变量本身仍按标注
        // （`TraitObject`）记进 env，这与转换后的实际值一致。
        if !matches!(ty_inner, Type::Any | Type::TraitObject { .. }) {
            // v0.75.86: 提前用 span 报不一致——不等 solve_constraints 兜底
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
        let gen_hint = generalize::generalize(ty_inner, &self.env.free_variables());
        self.env.add(name.to_string(), gen_hint.clone());
        // v0.96: 同 infer_let —— 类型标注的 let 绑定闭包也登记效果行。
        if matches!(
            value.kind,
            WitnessKind::Closure { .. } | WitnessKind::FnDef { .. }
        ) && let Some(row) = self.closure_rows.get(&value.span)
        {
            self.fn_effect_rows.insert(name.to_string(), row.clone());
        }
        let _ = span;
        Ok((gen_hint, value_row))
    }

    /// v0.83: 验证 Dict literal 字面量是否匹配 TeaModel 类型
    /// —— 每个 field 必须在 target 中存在，type 必须 compatible
    fn check_dict_against_teamodel(
        &mut self,
        entries: &[(String, MirWitness)],
        target_name: &str,
        target_fields: &[(String, Box<Type>)],
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        for (target_field_name, target_field_ty) in target_fields {
            // target field 必须在 entries 中存在
            let source_match = entries.iter().find(|(n, _)| n == target_field_name);
            match source_match {
                None => {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "field `{}` in model `{}`",
                            target_field_name, target_name
                        ),
                        got: "<missing>".to_string(),
                        span: Some(span),
                    }]);
                }
                Some((_, source_value_witness)) => {
                    // v0.83: 验证 source field 的类型
                    let (source_field_ty, _) = self.infer_expr(source_value_witness)?;
                    if !source_field_ty.compatible_with(target_field_ty) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "field `{}: {}` in model `{}`",
                                target_field_name,
                                target_field_ty.name(),
                                target_name
                            ),
                            got: source_field_ty.name(),
                            span: Some(span),
                        }]);
                    }
                }
            }
        }
        Ok(())
    }

    /// v0.83: 验证 Dict literal 是否匹配 TeaMsg 类型
    /// —— tag 字段必须匹配某个 variant，payload 字段 type 必须 compatible
    fn check_dict_against_teamsg(
        &mut self,
        entries: &[(String, MirWitness)],
        target_name: &str,
        target_variants: &[(String, Option<Box<Type>>)],
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        // 提取 tag 字段
        let tag_entry = entries.iter().find(|(n, _)| n == "tag");
        let tag_value = match tag_entry {
            Some((_, tag_witness)) => {
                if let WitnessKind::Literal(crate::common::Literal::String(s, _)) =
                    &tag_witness.kind
                {
                    s.clone()
                } else {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("msg `{}` tag field (String literal)", target_name),
                        got: format!("{:?}", tag_witness.kind),
                        span: Some(span),
                    }]);
                }
            }
            None => {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("msg `{}` requires `tag` field", target_name),
                    got: "<missing>".to_string(),
                    span: Some(span),
                }]);
            }
        };
        // 查找匹配的 variant
        let variant = target_variants.iter().find(|(n, _)| n == &tag_value);
        match variant {
            None => Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "variant `{}` in msg `{}` (variants: {})",
                    tag_value,
                    target_name,
                    target_variants
                        .iter()
                        .map(|(n, _)| n.clone())
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                got: "<unknown>".to_string(),
                span: Some(span),
            }]),
            Some((_, None)) => Ok(()), // unit variant
            Some((_, Some(payload_ty))) => {
                // 验证 payload 字段
                let payload_entry = entries.iter().find(|(n, _)| n == "payload");
                match payload_entry {
                    None => Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "msg `{}` variant `{}` requires `payload` field of type `{}`",
                            target_name,
                            tag_value,
                            payload_ty.name()
                        ),
                        got: "<missing>".to_string(),
                        span: Some(span),
                    }]),
                    Some((_, payload_witness)) => {
                        let (source_payload_ty, _) = self.infer_expr(payload_witness)?;
                        if !source_payload_ty.compatible_with(payload_ty) {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: format!(
                                    "msg `{}` variant `{}` payload: `{}`",
                                    target_name,
                                    tag_value,
                                    payload_ty.name()
                                ),
                                got: source_payload_ty.name(),
                                span: Some(span),
                            }]);
                        }
                        Ok(())
                    }
                }
            }
        }
    }

    pub(super) fn infer_assign(
        &mut self,
        target: &str,
        value: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (value_ty, value_row) = self.infer_expr(value)?;
        let current = self.env.get(target).cloned();
        if let Some(existing) = current {
            // v0.75.97: 命中 ForAll 时先实例化再合一（赋值的 LHS 是单形实例）
            let existing = self.instantiate_if_forall(&existing);
            // v0.104: **数值塔在赋值位点同样成立**（Int ⊂ Float）。
            //
            // 缺陷：此前无条件推 `Eq(existing, value_ty)`。`unify` 没有任何
            // Int/Float 交叉 arm（它只做同构合一；提升专门由 `Numeric` 约束
            // 负责），于是
            //   let total = 0i
            //   for i in [1, 2, 3]        -- 无后缀字面量是 Float
            //     total = total + i       -- `total + i` 按塔提升为 Float
            //   end
            // 报 "expected float, got int"。同一表达式写在 `let` 右侧
            // （`let z = total + i`）却合法 —— 赋值位点与 let 位点语义分叉。
            //
            // 运行期 `h_assign` 原样写入值、绑定表示随值变化，本就允许这种
            // 加宽；语言自身的 fixtures（loop_break / loop_continue /
            // loop_for_break / loop_beyond_dag_limit）也都用这个形状。
            // 因此数值目标走 `Numeric`（把两侧归一到提升类型，`value_ty`
            // 为 TypeVar 时留到 solve 阶段解析），非数值目标仍走严格 `Eq`。
            if !existing.compatible_with(&value_ty) && !value_ty.compatible_with(&existing) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: existing.name().to_string(),
                    got: value_ty.name().to_string(),
                    span: Some(span),
                }]);
            }
            if is_numeric(&existing) {
                self.constraints
                    .push(Constraint::Numeric(super::unify::BinaryConstraint {
                        left: Box::new(existing),
                        right: Box::new(value_ty.clone()),
                        result: None,
                    }));
            } else {
                self.constraints.push(Constraint::Eq(
                    Box::new(existing),
                    Box::new(value_ty.clone()),
                ));
            }
        } else {
            return Err(vec![TypeError::UnboundVariable {
                name: target.to_string(),
                span,
            }]);
        }
        Ok((value_ty, value_row))
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
            // v0.103: 全局模块对象 —— 从未绑定标识符解析为对象类型（而非
            // UnboundVariable）。名单唯一事实源是 `crate::value::MODULE_OBJECTS`
            // （与 Interpreter::new 的 globals 注册、flow::is_builtin_object
            // 同源）。此前硬编码 6 个名字，导致 globals 已注册的
            // bus/sandbox/schedule/ccr/mock/exec/tool/skill/plan/mora/
            // document/tea/xform 共 13 个模块在类型检查阶段被拒。
            None => match name {
                // 精确分型：AI 相关（方法推断有专门分支）
                "ai" => Ok(Type::AiModule),
                "agent" => Ok(Type::Agent),
                // v0.99: random 模块 — 方法调用 = ambient effect perform。
                // 精确分型（每操作标签 + 预置签名 + 效果行）在
                // infer_method_call 的 RandomModule 分支。
                "random" => Ok(Type::RandomModule),
                // 其余模块对象：方法分派走 dispatch.rs，类型视为不透明客体。
                // Unknown 允许与任意约束继续推进（v0.75.92 起 Unknown 在
                // unify 中 fail-fast，故方法调用路径不产生 Unknown 约束）。
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
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::common::BinaryOp::*;
        let (left_ty, left_row) = self.infer_expr(left)?;
        let (right_ty, right_row) = self.infer_expr(right)?;
        let merged_row = self.merge_rows(left_row, right_row);
        let result_ty = self.fresh_type_var();
        match op {
            Add | Sub | Mul | Div | Mod => {
                // v0.90.5: Numeric constraint — 允许 Int/Float 混合运算，
                // 结果类型由 numeric promotion 规则决定（Int+Int→Int, 含 Float→Float）。
                // TypeVar 视为潜在数值类型，推迟到 solve 阶段判定。
                //
                // v0.104: **「推迟」必须真的推迟** —— 此前 TypeVar 落到非数值
                // 分支推 `Eq(left, right)`，把未解析变量**当场钉到**另一侧的
                // 具体类型上，之后该变量若被别处约束为可提升类型就冲突。
                //
                // 实例（语言自身 fixtures 的形状）：
                //   let total = 0i
                //   for i in [1, 2, 3]        -- 元素类型是 fresh TypeVar，
                //     total = total + i       --   Eq(elem, Float) 待解
                //   end
                // `total + i` 里 `i` 的类型此刻是未解析 TypeVar：Eq(Int, α)
                // 把 α 钉成 Int，随后列表的 Eq(α, Float) 冲突 →
                // "expected float, got int"。而按数值塔 `Int + Float` 应提升
                // 为 Float。判据：**恰有一侧是具体数值**、另一侧是 TypeVar 时
                // 走 `Numeric`（solve 阶段按当时解析结果提升）；两侧都是
                // TypeVar 时保持 `Eq`（泛型算术仍需把两侧合一，Numeric 会因
                // 无法解析两侧而误报）。
                let left_num = is_numeric(&left_ty);
                let right_num = is_numeric(&right_ty);
                let left_tv = matches!(left_ty, crate::typeck::Type::TypeVar(_));
                let right_tv = matches!(right_ty, crate::typeck::Type::TypeVar(_));
                let defer_numeric = (left_num && right_tv) || (right_num && left_tv);

                // v0.104.6 D24：spec §12.3.1「广播算术 (v0.17, APL 启发)」
                // 明确定义 `list ⊗ scalar`（两个方向）与 `list ⊗ list`（等长
                // 逐元素）。运行期 `flow::eval_binary` / `numeric_op` 一直
                // 实现着这些臂，但**此前这里只判 `is_numeric`**，List 一律
                // 落严格 `Eq` → 整节特性从源码不可达。真实 CLI 实测：
                //
                // ```text
                // [1,2,3] * 2          → exit 2  Type error   ← spec 逐字例子
                // 1 + [10,20,30]        → exit 2  Type error   ← spec 逐字例子
                // [1,2,3] + [10,20,30]  → exit 0  [11.0, 22.0, 33.0]  （list⊗list 通）
                // [10,20,30] - [1,2,3]  → exit 0  [9.0, 18.0, 27.0]   （list⊗list 通）
                // ```
                //
                // spec 举的 4 个例子里有 2 个跑不了。放行时结果类型为
                // `List(提升后的元素类型)`，提升规则与标量塔一致
                // （Int⊗Int→Int、含 Float→Float、含 BigInt→BigInt）。
                let bcast_elem: Option<crate::typeck::Type> = match (&left_ty, &right_ty) {
                    (crate::typeck::Type::List(e), n)
                        if is_numeric(n) || matches!(n, crate::typeck::Type::TypeVar(_)) =>
                    {
                        Some(promote_numeric(e, n))
                    }
                    (n, crate::typeck::Type::List(e))
                        if is_numeric(n) || matches!(n, crate::typeck::Type::TypeVar(_)) =>
                    {
                        Some(promote_numeric(n, e))
                    }
                    (crate::typeck::Type::List(a), crate::typeck::Type::List(b)) => {
                        Some(promote_numeric(a, b))
                    }
                    _ => None,
                };
                let is_broadcast = bcast_elem.is_some();

                if (!left_num || !right_num) && !defer_numeric && !is_broadcast {
                    // 非数值类型：检查 symmetric compatible_with（如 String+String 拼接）
                    if !left_ty.compatible_with(&right_ty) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!("{:?}", left_ty),
                            got: format!("{:?}", right_ty),
                            span: Some(span),
                        }]);
                    }
                    // 同类型非数值运算（如 String+String）用 Eq 约束
                    self.constraints.push(Constraint::Eq(
                        Box::new(left_ty.clone()),
                        Box::new(result_ty.clone()),
                    ));
                    self.constraints.push(Constraint::Eq(
                        Box::new(right_ty.clone()),
                        Box::new(result_ty.clone()),
                    ));
                } else {
                    // 数值类型：用 Numeric 约束（Int/Float promotion 由 solver 处理）
                    // result 字段让 solver 在校验后自动将 result_ty 与 promotion 类型合一
                    // v0.104.6 D24：广播 —— 结果类型定为 `List(提升后元素)`。
                    // 约束两件事：① `result_ty = List(提升元素)`（`result_ty` 是
                    // 表达式对外的类型，不绑它则调用方拿到未解析 TypeVar）；
                    // ② 列表那侧的 `List(e)` 与之合一，把元素 TypeVar 解析掉。
                    // `List ⊗ List` 两侧都是列表，故两侧都约束。
                    // 注意**不要**给标量那侧压 `Eq(scalar, List(..))` —— 那会得到
                    // "expected float, got list<float>"（把标量当成了列表元素）。
                    let bcast_target: Option<crate::typeck::Type> =
                        bcast_elem.map(|p| crate::typeck::Type::List(Box::new(p)));
                    let list_sides: Vec<crate::typeck::Type> =
                        match (&left_ty, &right_ty, &bcast_target) {
                            (
                                crate::typeck::Type::List(_),
                                crate::typeck::Type::List(_),
                                Some(_),
                            ) => vec![left_ty.clone(), right_ty.clone()],
                            _ => match (&left_ty, &right_ty) {
                                (crate::typeck::Type::List(_), _) => vec![left_ty.clone()],
                                (_, crate::typeck::Type::List(_)) => vec![right_ty.clone()],
                                _ => Vec::new(),
                            },
                        };
                    if let Some(target) = bcast_target {
                        self.constraints.push(Constraint::Eq(
                            Box::new(result_ty.clone()),
                            Box::new(target.clone()),
                        ));
                        for side in &list_sides {
                            self.constraints.push(Constraint::Eq(
                                Box::new(side.clone()),
                                Box::new(target.clone()),
                            ));
                        }
                    } else {
                        self.constraints.push(Constraint::Numeric(
                            super::unify::BinaryConstraint {
                                left: Box::new(left_ty.clone()),
                                right: Box::new(right_ty.clone()),
                                result: Some(Box::new(result_ty.clone())),
                            },
                        ));
                    }
                }
                Ok((result_ty, merged_row))
            }
            Equal | NotEqual => {
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("{:?}", left_ty),
                        got: format!("{:?}", right_ty),
                        span: Some(span),
                    }]);
                }
                // v0.103: **数值对走 Numeric 约束，而非严格 Eq**。
                // `compatible_with` 自 v0.90.5 起明确承认 Int/Float 互相兼容
                // （其注释即以 `42 == 3.14` 为例），但此处紧接着要求
                // Eq(Int, Float) —— `unify` 没有 Int/Float 交叉 arm，于是
                // `4i == 4.0` 被判类型错误：类型系统自相矛盾。Numeric 约束
                // 会把两操作数绑到提升类型（Float），与 `compatible_with`
                // 的规则一致。
                if is_numeric(&left_ty) && is_numeric(&right_ty) {
                    self.constraints
                        .push(Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(left_ty),
                            right: Box::new(right_ty),
                            result: None,
                        }));
                } else if (is_numeric(&left_ty)
                    && matches!(right_ty, crate::typeck::Type::TypeVar(_)))
                    || (is_numeric(&right_ty) && matches!(left_ty, crate::typeck::Type::TypeVar(_)))
                {
                    // v0.104: 未解析变量同样推迟 —— 直接把 TypeVar 钉到对侧
                    // 具体类型会让它无法参与数值塔提升。实例：
                    //   for i in [1, 2, 3]        -- i 的类型是 fresh TypeVar
                    //     if i == 6i …            -- Eq(α, Int) 把 α 钉成 Int，
                    //   end                       --   随后列表的 Eq(α, Float)
                    // 冲突 → "expected float, got int"。走 Numeric 由 solve
                    // 阶段按已解析结果提升。
                    //
                    // v0.104.6 D53：此前的条件是「**任一侧**是 TypeVar 就压
                    // Numeric」，但 Numeric 的定义就是「两侧都必须是数值」——
                    // 于是一个**永远不会被解析**的 TypeVar 会在 solve 阶段
                    // 被按「必须是数值」拒绝。索引表达式正是这种 TypeVar：
                    // witness 侧把 `xs[i]` 编码成 `Call("[]")`，而 `[]` 在
                    // typeck 里**没有签名**，故其结果类型恒为 fresh TypeVar、
                    // 没有任何约束会去解析它。
                    //
                    // 真实 CLI 实测（修前）：
                    //   [1,2][0] == [1,2][1]   → Type error: expected numeric
                    //                                     type (int or float)
                    //   "ab"[0] == 'a'          → 同上
                    // 而 `xs[0] == 1`、`xs[0] > 0`、`xs[0] + xs[1]` 都正常
                    // （后两者走的是别的分支）。
                    //
                    // 修法：**仅当对侧确实是数值**时才延迟到 Numeric（保住
                    // v0.104 的提升场景），否则退回 Eq —— TypeVar 与任意类型
                    // 合一是安全的，且运行期 `index_value` 本就支持任意类型。
                    self.constraints
                        .push(Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(left_ty),
                            right: Box::new(right_ty),
                            result: None,
                        }));
                } else {
                    self.constraints
                        .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                }
                Ok((Type::Bool, merged_row))
            }
            Greater | Less | GreaterEqual | LessEqual => {
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("{:?}", left_ty),
                        got: format!("{:?}", right_ty),
                        span: Some(span),
                    }]);
                }
                // v0.103: 同 Equal —— 数值比较经 Numeric 约束（Int/Float 提升），
                // 非数值（如字符串）走 Eq。此前严格 Eq 使 `4i < 4.0` 被拒，
                // 与 `compatible_with` 的 numeric 规则矛盾。
                // v0.104: 含未解析 TypeVar 时同样推迟（理由见 Equal 分支）。
                if is_numeric(&left_ty) && is_numeric(&right_ty) {
                    self.constraints
                        .push(Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(left_ty),
                            right: Box::new(right_ty),
                            result: None,
                        }));
                } else if matches!(left_ty, crate::typeck::Type::TypeVar(_))
                    || matches!(right_ty, crate::typeck::Type::TypeVar(_))
                {
                    // v0.104: 含未解析 TypeVar → 推迟到 solve（同 Equal 分支）。
                    self.constraints
                        .push(Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(left_ty),
                            right: Box::new(right_ty),
                            result: None,
                        }));
                } else {
                    self.constraints
                        .push(Constraint::Eq(Box::new(left_ty), Box::new(right_ty)));
                }
                Ok((Type::Bool, merged_row))
            } // v0.55: Or/And are WitnessKind variants (short-circuit),
              // handled directly in infer_expr, not BinaryOp variants.
              // BinaryOp 已穷尽（11 变体全部覆盖）— 无需 `_` 兜底。
        }
    }

    /// v0.80: 函数调用推断 — 用 unification 消解 curried Arrow。
    ///
    /// 每个参数消耗一层 Arrow：callee_ty 必须与 Arrow(arg_ty, fresh_ret, fresh_eff)
    /// 合一，然后 callee_ty 更新为 fresh_ret，fresh_eff 累积到总 effect row。
    pub(super) fn infer_call(
        &mut self,
        callee: &WitnessCallee,
        args: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        // v0.104.6 D68：内建被调体**声明的结果类型**（剥掉全部参数层）。
        // 少传实参时循环留下的是残差 `Arrow`（且只在 solver 之后才显现），
        // 直接返回它会让 `let v: String = range(0, 3)` 报
        // "expected string, got fn ('') -> list<float>"。内建的返回类型
        // 永远不是函数，故对内建一律用声明结果。见 `peel_all_arrows`。
        let mut builtin_declared_ret: Option<Type> = None;
        let (mut callee_ty, mut acc_row) = match callee {
            WitnessCallee::Name(name) => {
                // v0.84: 先查 env（用户定义函数/闭包），再查 builtin，最后 fresh TypeVar。
                // 此前只查 builtin_callee_ty，导致 `let f = fn(x) x * 2 end; f(5)` 中
                // `f` 在 env 里有 Arrow 类型，但 infer_call 查不到 → 兜底 Unknown。
                // 兜底 Unknown 有副作用：约束 Unknown = Arrow(...) 在 unification 中
                // fail-fast 报错（line 0）。改为 fresh TypeVar：让 Arrow 分解约束
                // 自然传播 ret 类型。
                // v0.96: 效果行直接从 Arrow 类型/登记表提取（数据流，不靠
                // 约束消解）—— 顶层边界断言用的是**消解前**的残差行。
                // Arrow 行为空时回落 fn_effect_rows（零参闭包的类型是裸
                // TypeVar，行只在其定义 span 的登记表中）。
                let ty_opt = self.env.get(name).cloned();
                let (ty, callee_row) = if let Some(t) = ty_opt {
                    let inst = self.instantiate_if_forall(&t);
                    let row = match Self::arrow_row_of(&inst) {
                        crate::mir::effect::EffectRow::Empty => self
                            .fn_effect_rows
                            .get(name)
                            .cloned()
                            .unwrap_or(crate::mir::effect::EffectRow::Empty),
                        r => r,
                    };
                    (Some(inst), row)
                } else if let Some(t) = self.builtin_callee_ty(name) {
                    let inst = self.instantiate_if_forall(&t);
                    let row = Self::arrow_row_of(&inst);
                    builtin_declared_ret = Some(peel_all_arrows(&inst));
                    (Some(inst), row)
                } else {
                    let row = self
                        .fn_effect_rows
                        .get(name)
                        .cloned()
                        .unwrap_or(crate::mir::effect::EffectRow::Empty);
                    (None, row)
                };
                let ty = ty.unwrap_or_else(|| self.fresh_type_var());
                (ty, callee_row)
            }
            // v0.75.97: Var 命中 ForAll 时实例化
            WitnessCallee::Var(var_name) => {
                let ty_opt = self.env.get(var_name).cloned();
                let (ty, callee_row) = match ty_opt {
                    Some(t) => {
                        let inst = self.instantiate_if_forall(&t);
                        // v0.96: Arrow 行为空时回落登记表（零参闭包）
                        let row = match Self::arrow_row_of(&inst) {
                            crate::mir::effect::EffectRow::Empty => self
                                .fn_effect_rows
                                .get(var_name)
                                .cloned()
                                .unwrap_or(crate::mir::effect::EffectRow::Empty),
                            r => r,
                        };
                        (inst, row)
                    }
                    None => {
                        let row = self
                            .fn_effect_rows
                            .get(var_name)
                            .cloned()
                            .unwrap_or(crate::mir::effect::EffectRow::Empty);
                        (self.fresh_type_var(), row)
                    }
                };
                (ty, callee_row)
            }
            WitnessCallee::Evaluated(expr) => self.infer_expr(expr)?,
            WitnessCallee::Builtin(op) => {
                let ty = self.builtin_type(op)?;
                let row = Self::arrow_row_of(&ty);
                (ty, row)
            }
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

        // v0.104.6 D55：**下标表达式 `xs[i]` 特判** —— 按接收者推断元素类型。
        //
        // 缺陷：witness 侧把读索引编码成 `Call { callee: Name("[]"), args:
        // [obj, i] }`（见 `tests/parser_v3_coverage.rs::index_expr_parses`
        // 断言的形状），而 `[]` 在 `builtin_callee_ty` 里**没有登记**，于是
        // 走 `unwrap_or_else(|| self.fresh_type_var())` —— 结果类型恒为
        // **fresh TypeVar**，没有任何约束会去解析它。后果：
        //
        // ```mora
        // let y = "a"
        // y + 1                       → Type error ✅（String + Float 被拒）
        //
        // let xs = ["a", "b"]
        // let y = xs[0]
        // y + 1                       → **不报错**，运行期得 "a1.0"
        // ```
        //
        // 元素类型就此**丢失**，后续一切推断都建立在「未知」之上。
        // `tests/tier1_typeck_mir.rs::list_get_exposes_element_type_error`
        // 断言「String 元素 + Int 应报类型错」，但它此前是**因错误的原因**
        // 而通过的 —— `xs.get(0)` 的下标被拒（那是 D52），不是被测的 `+`。
        //
        // 修法：按接收者类型给出结果类型，与运行期 `mir/vm.rs::index_value`
        // 的三个分支**逐条对齐**（List → 元素、Dict → 值 | nil、String → Char）。
        if matches!(callee, WitnessCallee::Name(n) | WitnessCallee::Var(n) if n == "[]") {
            let Some((recv, _idx)) = args.split_first() else {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 2,
                    actual: args.len(),
                    span,
                }]);
            };
            let (recv_ty, recv_row) = self.infer_expr(recv)?;
            acc_row = self.merge_rows(acc_row, recv_row);
            if let Some(rest) = args.get(1) {
                let (_idx_ty, idx_row) = self.infer_expr(rest)?;
                acc_row = self.merge_rows(acc_row, idx_row);
            }
            // 索引实参**不收紧**：此前 `[]` 无签名，索引类型完全不被检查
            // （越界/类型错一律留到运行期 `index_value` 报）。这里保持同样的
            // 宽松度，只把**结果**类型接上 —— 避免「修一个坏两个」。
            //
            // v0.104.6 D55：**当时**只有 String 分支真正生效。List / Dict 的
            // 元素/值类型在 `infer_list` / `infer_dict` 里是 fresh TypeVar，
            // 要到**后续 solver 阶段**才被替换；此处 `Eq(result, elem)`
            // 约束在 solve 时与 `+` 产生的 `Eq(result, Float)` 相撞而**未
            // 报错**（D55 当时实测 `let y: Int = xs[0]` 在 `xs = ["a","b"]`
            // 上仍被接受）。故 D55 时不写「看似生效实则无效」的 List/Dict
            // 分支 —— 留不留不住的代码比没有更糟。
            //
            // ⚠ v0.104.6 D67 已修好根因：`infer_list` / `infer_dict` 现在
            // **直接返回已解析的公共元素类型**（不再返回未解算的 TypeVar），
            // 所以下面那段「fresh TypeVar + 压约束」的绕行**已过期**，
            // 由紧随其后的 `直接返回元素类型` 分支取代。D55 当时记下的
            // 「实测仍被接受」正是那个缺陷的**症状**，不是 List/Dict 的固有限制。
            // 复核（真实 CLI）：
            //     let xs = ["a", "b"]
            //     let y: Int = xs[0]   → expected Int, got String   （被拒）
            //
            // 字符串下标是**具体**的 `Char`，无需等 solver，可直接给。
            // 实测生效：`let y: String = "ab"[0]` 现在正确报
            // `expected String, got Char`（修前不报）。
            match &recv_ty {
                // 字符串下标是**具体**的 `Char`，无需等 solver，可直接给。
                // 实测生效：`let y: String = "ab"[0]` 现在正确报
                // `expected String, got Char`（修前不报）。
                crate::typeck::Type::String => return Ok((crate::typeck::Type::Char, acc_row)),
                // List / Dict：v0.104.6 D67 起，元素 / 值类型**已是解析后的具体
                // 类型**（`infer_list` / `infer_dict` 不再返回未解算的
                // `TypeVar`），所以这里**直接返回**即可。
                //
                // 沿革（留着是为了说明「为什么不是更复杂的方案」）：
                //   * D55 时期此处绕了一圈 fresh TypeVar + `Eq(r, elem)`，注释
                //     写着「不能直接返回 `(**e).clone()`，那拿到的仍是未解析的
                //     TypeVar」—— 那句话的**前提已被 D67 消灭**。
                //   * D67 之前 `[]` 返回的 TypeVar 与元素 TypeVar 之间**没有
                //     任何约束**，而 `let` 注解检查也**从不产生约束**
                //     （`bidirectional.rs` 的 `check_against` 是纯比较，且
                //     `TypeVar::subtype_of` 对任何类型都返回 true，见
                //     `typeck/mod.rs` v0.84）—— **两端同时落空**，这才是
                //     「`let y: Int = xs[0]` 在 `xs = ["a","b"]` 上仍被接受」
                //     的真正原因。
                //   * D67 修好根因（容器字面量直接推具体元素类型）后，直接返回
                //     即可；继续绕圈只会把已经具体的类型重新退化成宽松的
                //     TypeVar，并让 `m[0][0]` 这类**链式索引**在外层丢掉类型
                //     （`let y: String = m[0][0]` 静默通过，拆成中间变量
                //     `let inner = m[0]` 同样失效）。
                //
                // 类型本身仍可能是 TypeVar（空列表 `[]`、未解析的形参等），
                // 那时透传 TypeVar 与修前行为一致，不引入新的宽松。
                crate::typeck::Type::List(e) => {
                    return Ok(((**e).clone(), acc_row));
                }
                crate::typeck::Type::Dict(_, v) => {
                    // **只接值类型、不并 `Nil`** —— 这是一处有意识的取舍。
                    //
                    // 运行期 `index_value` 对**缺失键**返回 `Nil`（D14 定的
                    // 契约），类型上如实写应是 `Union(V, Nil)`；但那样
                    // `t + d[ks[i]]`（遍历字典求和 —— 最常见的字典用法）会因为
                    // `Float + Union(Float, Nil)` 而**编译不过**，实测确为回归。
                    //
                    // 故此处按「键存在」推断，缺失键导致的 `Nil` 交给运行期。
                    // 代价：`let y: String = d["zz"]`（`d = {a: 1}`，键不存在、
                    // 运行期得 `Nil`）会因 `Eq(Float, String)` 报错 —— 结论对
                    // 但理由是「值类型不符」而非「键缺失」。要表达「可能缺失」
                    // 应显式写 `Union(String, Nil)`。
                    return Ok((v.as_ref().clone(), acc_row));
                }
                // 其他接收者：留 fresh TypeVar（与本修复前的宽松行为一致）
                _ => {}
            }
            return Ok((self.fresh_type_var(), acc_row));
        }

        // v0.102: 关系调用特判 —— 按关系签名（precompute_rel_sigs 不动点）
        // 逐位置校验实参并返回 Goal；关系体的效果行在调用点并入。
        if let WitnessCallee::Name(n) | WitnessCallee::Var(n) = callee
            && let Some(sig) = self.rel_sigs.get(n).cloned()
        {
            let mut arg_tys: Vec<Type> = Vec::new();
            for a in args {
                let (t, r) = self.infer_expr(a)?;
                arg_tys.push(t);
                acc_row = self.merge_rows(acc_row, r);
            }
            if arg_tys.len() != sig.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: sig.len(),
                    actual: arg_tys.len(),
                    span,
                }]);
            }
            for (at, st) in arg_tys.iter().zip(sig.iter()) {
                self.constraints
                    .push(Constraint::Eq(Box::new(at.clone()), Box::new(st.clone())));
            }
            let rel_row = self
                .rel_effect_rows
                .get(n)
                .cloned()
                .unwrap_or(crate::mir::effect::EffectRow::Empty);
            return Ok((Type::Goal, self.merge_rows(acc_row, rel_row)));
        }
        // v0.102: project(f, args..., result) —— 首个实参是函数名引用（非变量
        // 使用，不查 env）；并入被投函数 f 的效果行（行必须传播到 solve 位点，
        // 否则 project 内的 perform 漏检），并推断其余实参与结果项。
        if let WitnessCallee::Name(n) = callee
            && n == "project"
        {
            let rest = if args.is_empty() { args } else { &args[1..] };
            for a in rest {
                let (_, r) = self.infer_expr(a)?;
                acc_row = self.merge_rows(acc_row, r);
            }
            if let Some(first) = args.first() {
                let fname = match &first.kind {
                    WitnessKind::Variable(f) | WitnessKind::FnDef { name: f, .. } => {
                        Some(f.clone())
                    }
                    WitnessKind::Literal(crate::common::Literal::String(f, _)) => Some(f.clone()),
                    _ => None,
                };
                if let Some(f) = fname
                    && let Some(r) = self.fn_effect_rows.get(&f).cloned()
                {
                    acc_row = self.merge_rows(acc_row, r);
                }
            }
            return Ok((Type::Goal, acc_row));
        }
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

        // v0.103: **变参 builtin** —— 签名表声明 variadic 时（`print`），
        // 逐实参按声明的参数类型校验，返回声明的结果类型。
        //
        // 缺陷：签名表此前无法表达「可重复末参」，`builtin_callee_ty` 于是按
        // `params.len()` 生成固定 arity 的 curried arrow —— 下面的 curried 循环
        // 每个实参消耗一层 Arrow，多余实参无处消耗，`print("a", b)` 报
        // "expected nil, got fn(string) -> …"。运行期 `call_builtin_print`
        // 本就 join 全部实参，类型系统却在拒绝。
        //
        // 判据来自**签名表**（`Signature::variadic`）而非名字硬编码 ——
        // arity 契约的唯一事实源就是该表。
        if let WitnessCallee::Name(n) | WitnessCallee::Var(n) = callee
            && let Some(sig) = crate::typeck::dispatch::lookup_builtin(n)
            && sig.variadic
        {
            let param_ty = sig
                .params
                .last()
                .map(|(_, t)| t.clone())
                .unwrap_or(Type::Any);
            for arg in args {
                let (arg_ty, arg_row) = self.infer_expr(arg)?;
                acc_row = self.merge_rows(acc_row, arg_row);
                // 每个实参只需满足末位参数类型（可重复）。
                if !arg_ty.compatible_with(&param_ty) && !param_ty.compatible_with(&arg_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: param_ty.name().to_string(),
                        got: arg_ty.name().to_string(),
                        span: Some(span),
                    }]);
                }
            }
            return Ok((sig.return_type.clone(), acc_row));
        }

        // v0.104.6 D161：**闭包 arity 的编译期检查**（本会话长期待办的收口）。
        //
        // 多参数闭包 `fn(a, b) -> c` 的类型是 `Arrow(A, Arrow(B, C, eff), eff)`
        // （见 `wrap_curried_arrow` 的注释）—— arity 编码为 **Arrow 嵌套深度**。
        // 而下面的 curried 循环是「每个实参消耗一层」：实参**不足**时，
        // 剩下的 `Arrow` 层根本不进任何约束，**不会产生任何错误**。
        //
        // 实测（修复前）：
        //
        // ```text
        // let f2 = fn(a, b) => a + b
        // f2(1)
        //   mora --check → exit 0「No type errors found. (3 expressions)」  ❌
        //   运行期        → exit 1「closure expects 2 args, got 1」        ← 运行期才炸
        // ```
        //
        // 即 `--check` 明确告诉用户「代码没问题」，而它**根本跑不起来**。
        // 运行期是权威：它**不支持**部分应用（`f2(1)` 直接报错），所以类型层
        // 也不该放行。部分应用在本语言里有**显式**写法（`curry(f, n)`，D148），
        // 不靠少传实参隐式获得。
        //
        // 反方向（多传）此前**会**被查，但走的是「多余实参与**返回类型**合一」
        // 这条歪路，产出一条看不懂且**泄露内部变量**的消息：
        //
        // ```text
        // f1(1, 2)  where f1 = fn(a) => a + 1
        //   → Type mismatch: expected float, got fn (float) -> ' ! { rho1 }
        //                            ↑ `rho1` 是内部 effect-row 变量，用户无从理解
        // ```
        //
        // 两侧一并用 `ArityMismatch` 表达，语义直白且与运行期措辞一致。
        //
        // ⚠ 只在 callee 是**具体的 Arrow 链**时检查：`TypeVar`（递归/尚未定型的
        // 被调）、`Any`、`Unknown` 的 `arrow_arity` 都是 0，跳过。
        // ⚠ 变参内建在**上面**已提前返回（`sig.variadic` → :961），
        // 它们的 `print(1, 2, 3)` 不受本检查影响。
        //
        // ⚠ **内建一律豁免**：它们的签名带**可选尾参**（类型含 `Nil`，见 :1084
        // 的 `min_arity` 约定），如 `compress(input, strategy, opts?)` 声明 3 个
        // 形参却允许只传 2 个。第一版用 `dispatch::lookup_builtin(n)` 判定，
        // 全量红 16 条 —— 因为 `compress` / `crush_json` 只登记在
        // `hm/builtin.rs::builtin_type`（经 `builtin_callee_ty`）而**不在**那张表里。
        //
        // 改用 `builtin_declared_ret.is_some()`：它**恰好**在
        // `builtin_callee_ty(name)` 命中时被置位（:673），是「本 callee 是内建」
        // 的权威信号，且已在作用域内，不必另写一套判定。
        let is_registered_builtin = builtin_declared_ret.is_some();
        let mut want_arity = if is_registered_builtin {
            0
        } else {
            arrow_arity(&callee_ty)
        };
        // v0.104.6 D167：Arrow 链给不出 arity 时（**定义名不进 env**，调用点
        // 解析成 `TypeVar` → arity 0），回退到「顶层 task/fn 形参个数」表。
        // 故 `task add(a, b) … end; add(1)` 也会在 `--check` 下被拦下，
        // 而不再只报运行期错。
        if want_arity == 0
            && let WitnessCallee::Name(n) | WitnessCallee::Var(n) = &callee
            && let Some(n) = self.fn_arities.get(n)
        {
            want_arity = *n;
        }
        if want_arity > 0 && args.len() != want_arity {
            return Err(vec![TypeError::ArityMismatch {
                expected: want_arity,
                actual: args.len(),
                span,
            }]);
        }

        // v0.80: curried Arrow 消解 — 每个参数消耗一层 Arrow。
        for arg in args {
            let (arg_ty, arg_row) = self.infer_expr(arg)?;
            acc_row = self.merge_rows(acc_row, arg_row);
            let fresh_ret = self.fresh_type_var();
            let fresh_eff = self.fresh_row_var();
            let expected = Type::Arrow(
                Box::new(arg_ty),
                Box::new(fresh_ret.clone()),
                fresh_eff.clone(),
            );
            // v0.99: 被调体行以**值**并入调用行（被调箭头的行是具体行时
            // 直接并）；行仍是 Var（未知被调）才保持 RowEq 推迟。旧行为
            // 无条件 `RowEq(fresh_eff, acc_row)` —— solve 时 fresh_eff 已被
            // 上面的 Eq 绑定到被调体的行，等式于是把「被调体行」与「实参行」
            // 强行画等号，纯被调 + 内联 effectful 实参被误拒
            // （`print(random.rand_int(1, 10))` 报 expected pure）。
            let known_callee_row = match &callee_ty {
                Type::Arrow(_, _, row) if !matches!(row, crate::mir::effect::EffectRow::Var(_)) => {
                    Some(row.clone())
                }
                _ => None,
            };
            self.constraints.push(Constraint::Eq(
                Box::new(callee_ty.clone()),
                Box::new(expected),
            ));
            callee_ty = fresh_ret;
            acc_row = match known_callee_row {
                Some(row) => self.merge_rows(acc_row, row),
                None => self.merge_rows(acc_row, fresh_eff),
            };
        }
        // v0.104.6 D161：数 `Arrow` 链 = 该被调的 arity。
        //
        // `wrap_curried_arrow` **从最后一个参数向前包裹**，所以多参数闭包是
        // `Arrow(A, Arrow(B, C, …), …)` —— 嵌套发生在**输出**侧，不在输入侧
        // （我第一版沿 `input` 数，恒得 1，于是少传实参**根本没被拦下**）。
        //
        // | 定义 | 类型 | arity |
        // |---|---|---|
        // | `fn(a) => …`     | `Arrow(A, Float)`            | 1 |
        // | `fn(a, b) => …`  | `Arrow(A, Arrow(B, Float))`  | 2 |
        //
        // 非 `Arrow`（TypeVar / Any / Unknown / 值类型）返回 0，调用点据此跳过
        // —— 递归函数、尚未定型的被调、动态值都不该被误伤。
        //
        // 已知取舍：返回闭包的函数（`fn() => fn(x) => x`）会被算成 arity 2。
        // 但它**本来就不可用** —— D123 已把 `c(1)(2)` 链式调用改成明确报错，
        // 取不到内层闭包。故此处的误判方向是「更早地告知」，不是放行错误。
        fn arrow_arity(ty: &Type) -> usize {
            match ty {
                Type::Arrow(_input, out, _row) => 1 + arrow_arity(out),
                _ => 0,
            }
        }
        // v0.104.6 D68：内建被调一律返回**声明的**结果类型（见函数头注释）。
        // 实参类型校验仍由上面循环压的约束完成，这里只定结果类型。
        if let Some(declared) = builtin_declared_ret {
            return Ok((declared, acc_row));
        }
        Ok((callee_ty, acc_row))
    }

    /// v0.96: 提取 curried Arrow 外层携带的效果行（非 Arrow → 纯）。
    ///
    /// wrap_curried_arrow 在每层 Arrow 都填同一个 body 行，取最外层即可。
    /// 直接数据提取（而非约束消解）是关键：顶层边界断言消费的是**消解前**
    /// 的残差行，行若以未消解 Var 形式上浮就会被宽松跳过（漏检）。
    fn arrow_row_of(ty: &Type) -> crate::mir::effect::EffectRow {
        match ty {
            Type::Arrow(_, _, row) => row.clone(),
            _ => crate::mir::effect::EffectRow::Empty,
        }
    }

    pub(super) fn infer_method_call(
        &mut self,
        receiver: &MirWitness,
        method: &str,
        args: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (recv_ty, recv_row) = self.infer_expr(receiver)?;
        let mut acc_row = recv_row;
        let mut arg_types: Vec<Type> = Vec::new();
        for a in args {
            let (t, r) = self.infer_expr(a)?;
            arg_types.push(t);
            acc_row = self.merge_rows(acc_row, r);
        }

        // v0.99: random 模块 —— ambient effect 分型（每操作一个标签 +
        // 预置签名：arity/逐参类型约束、结果静态化、效果行并入标签）。
        if matches!(recv_ty, Type::RandomModule) {
            return self.infer_random_method(method, arg_types, acc_row, span);
        }

        // v0.55: enforce arity from the dispatch table. The signature
        // already includes `self` as its first parameter, so the user
        // arity we compare against is `sig.params.len() - 1`.
        // v0.75.84: 尾部 dict 配置参数（ai.chat(prompt, {model: ...})）为
        // 可选——arity 下限是签名 user 参数数，多传 dict 不报 ArityMismatch。
        // v0.104.6 D69：**模块对象**的方法签名按**模块名**查，而非按
        // `recv_ty`。
        //
        // 根因：`infer_var`（:356）把 23 个模块对象里的 **20 个**解析成
        // `Type::Unknown`（只有 `ai` / `agent` / `random` 有精确变体），
        // 而 `method_signature` 没有 `Unknown` 分支 —— 于是**每一个模块方法
        // 调用**的结果类型都退化成永不解算的 TypeVar，标注形同虚设：
        //
        // ```mora
        // let v: String = math.floor(1.5)        → 修前被接受  ❌
        // let v: String = exec.parallel([…], 2) → 修前被接受  ❌
        // ```
        //
        // 拼错的方法名仍由**运行期**兜住（`math.flor(…)` 报 unknown method），
        // 所以这不是「静默错误」，是**类型检查层的盲区**。
        //
        // 为什么不新增 `Type::MathModule` 等 20 个变体：那是 v1.0 方向
        // （形式化语义）的设计决定，而此处只需「按名字查表」即可闭合。
        let module_sig = match &receiver.kind {
            WitnessKind::Variable(module) => {
                crate::typeck::dispatch::module_method_signature(module, method)
            }
            _ => None,
        };
        let resolved_sig = module_sig
            .clone()
            .or_else(|| crate::typeck::dispatch::method_signature(&recv_ty, method));
        // 结果类型优先取模块表 —— 下方 `method_return_type(&recv_ty, method)`
        // 按 `Type` 索引，对 `Unknown` 接收者恒为 None，故必须在这里抢先。
        // **同一张表必须同时驱动 arity 与结果类型**，否则只修一半：元数生效了
        // 而标注仍放行（这正是第一版只接 arity 那条路时的实测结果）。
        let module_return_ty = module_sig.map(|s| s.return_type);
        if let Some(sig) = resolved_sig {
            let user_arity = sig.params.len().saturating_sub(1);
            // v0.103: 支持**可选尾参** —— 签名中类型含 `Nil` 的尾部参数可省略。
            // 此前 arity 是「恰好 user_arity」，使 spec 标注为可选（`ctx?`）
            // 的参数在省略时报 "Expected 2 arguments, got 1"
            // （`ai.critic(answer)` 因此不可用）。可选性用「类型含 Nil」表达
            // （Nil 是该参数可缺席的既有编码），最小必需参数数 = 去掉连续
            // 尾部可选参数后的数量。
            let min_arity = {
                let mut min = user_arity;
                while min > 0 {
                    let ty = &sig.params[min].1;
                    let optional = match ty {
                        Type::Nil => true,
                        Type::Union(members) => members.iter().any(|m| matches!(m, Type::Nil)),
                        _ => false,
                    };
                    if optional {
                        min -= 1;
                    } else {
                        break;
                    }
                }
                min
            };
            // v0.104.6 D69：**变参**的模块方法（`file.join(a, b, c)` ——
            // 运行期 `for arg in args`）只校验下限，不设上限。`params` 里的
            // 形参类型全是 `Any`，逐实参的 Eq 约束没有信息量，跳过。
            if sig.variadic {
                if arg_types.len() < min_arity {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: min_arity,
                        actual: arg_types.len(),
                        span,
                    }]);
                }
            } else {
                let extra_configurable = arg_types
                    .iter()
                    .skip(min_arity)
                    .all(|t| matches!(t, Type::Dict(_, _) | Type::Nil))
                    || arg_types
                        .iter()
                        .skip(user_arity)
                        .all(|t| matches!(t, Type::Dict(_, _)));
                if arg_types.len() < min_arity
                    || (arg_types.len() > user_arity && !extra_configurable)
                {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: min_arity,
                        actual: arg_types.len(),
                        span,
                    }]);
                }
            }
            // v0.104.6 D156：此前**只**把实参类型压成 `Constraint::Eq` 交给
            // `solve_constraints` 兜底，而 `unify()` 的每个失败分支都写死
            // `span: None`（`unify.rs:221/229/321/…`）—— 于是
            //
            //   let xs = [1, 2, 3]
            //   xs.take("one")
            //
            // 报出 `line=0` 的「位置未跟踪」，且**逐字段完全相同**。D155 已实测：
            // witness 树里那个实参的 span 是**正确**的（`line 2 column 17`）——
            // span 一直都在，只是没被带进错误。
            //
            // 修法沿用本文件 `let x: T = v` 路径的既有范式（:137「提前用 span
            // 报不一致——不等 solve_constraints 兜底」）：先用 `compatible_with`
            // 快速判一次，不兼容就**带着实参的 span** 立即返回；兼容的照旧压约束
            // 交给求解器（TypeVar 绑定 / 提升仍由 unify 负责，本改动不碰）。
            //
            // 附带收益：每条诊断**指向自己的实参**，于是 D128 的
            // `(line, expected, actual)` 去重键**重新有了区分度** ——
            // D154 判定「任何按消息的去重都不健全」正是因为当时它们全都一样。
            for ((_param_name, param_ty), (arg, arg_ty)) in sig
                .params
                .iter()
                .skip(1)
                .zip(args.iter().zip(arg_types.iter()))
            {
                if !arg_ty.compatible_with(param_ty) {
                    // 用 `Type::name()` 而非 `{:?}`：本仓库的诊断一律小写
                    // （`string` / `float`），`{:?}` 会给出 `String` / `Float`
                    // 破坏一致性。而 `name()` 对 Union 给出 `int | float` ——
                    // 恰好修正了此前「expected int」的**欠报**（形参其实声明的是
                    // `Union(Int, Float)`，只说 int 会让用户以为传个 int 就行）。
                    return Err(vec![TypeError::UnificationFailure {
                        expected: param_ty.name(),
                        got: arg_ty.name(),
                        span: Some(arg.span),
                    }]);
                }
                self.constraints.push(Constraint::Eq(
                    Box::new(param_ty.clone()),
                    Box::new(arg_ty.clone()),
                ));
            }
        }

        // v0.103: Dict 字段访问 —— `d.count`（无实参的 `.name`）在运行期由
        // method_dispatch 的 Dict 兜底分支解析为「取该键的值」，但 typeck 的
        // 签名表只登记了 get/set/keys/values/len，此形态落到
        // `method_return_type_fallback` → `Type::Unknown`，而 Unknown 是
        // fail-fast 标签 → 类型检查失败的代码在运行期本可正常工作。
        // 规则：无实参的 Dict 字段访问返回**值类型参数** `v`（与运行期
        // 「非 callable 值直接返回」一致；调用形态另由签名/实参表处理）。
        // TeaModel 字段同理由 `model.count` 触发，各字段类型不同 —— 返回其
        // 字段表中该字段的类型。
        //
        // v0.104.6 D54：**必须先确认「这个名字不是真方法」**。
        // `dict_field_type` 对 `Type::Dict(_, v)` **无条件**返回 `Some(v)`，
        // 于是 `d.len()` / `d.keys()` 这类**真方法**被当成字段访问，返回的是
        // dict 的**值类型**，把上面签名表算出的 `ret = Int` 整个覆盖掉。
        // 实测（插桩）：
        //   @@LEN  recv=Dict(String, TypeVar('\0'))  ret=Int   ← 签名算对了
        //   @@CHK  synth=TypeVar('\0')  expected=String        ← 返回的却是值类型
        // 后果：`let n: String = d.len()` 被**静默接受**（List / String 接收者
        // 都正常报错）；而 `d.keys()` 同样退化成值 TypeVar，于是链式
        // `d.keys().len()` 的 receiver 成了 TypeVar、落到
        // `method_return_type` 兜底 → **返回 `Float`**（实测报 `got Float`）。
        if arg_types.is_empty()
            && crate::typeck::dispatch::method_signature(&recv_ty, method).is_none()
            && let Some(field_ty) = self.dict_field_type(&recv_ty, method)
        {
            return Ok((field_ty, acc_row));
        }

        let return_ty = module_return_ty
            .unwrap_or_else(|| crate::typeck::dispatch::method_return_type(&recv_ty, method));
        // v0.103: `Unknown` 是 fail-fast 逃逸标签（v0.75.92：与任何类型合一
        // 都失败），且 v0.75.91 明确「Unknown 不算已知签名」。方法结果无法
        // 判定时应交给**待推断变量**，而不是让 Unknown 流进下游约束
        // —— 否则类型检查期报错、运行期却正常的代码会被误杀。
        //
        // 实例：`task t(model) model.count + 1i end` —— `model` 无注解
        // （TypeVar），字段访问无签名 → 兜底 Unknown → `Unknown + Int` 触发
        // Eq(Unknown, Int) → 报 "expected Int, got TypeVar"。这与 v0.96 给
        // infer_call 未知被调者改用 fresh TypeVar 是同一处修正
        //（见本文件 infer_call 的注释）。
        let return_ty = if matches!(return_ty, Type::Unknown) {
            self.fresh_type_var()
        } else {
            return_ty
        };
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok((return_ty, acc_row))
    }

    /// v0.103: `receiver.field`（无实参字段访问）的静态类型。
    ///
    /// - `Dict<K, V>` → `V`（运行期返回该键对应的值）
    /// - `TeaModel { fields }` → 该字段在 `fields` 中的声明类型
    ///
    /// 未知字段返回 `None`（交回常规路径，由签名表/fail-fast 处理）。
    fn dict_field_type(&self, recv_ty: &Type, field: &str) -> Option<Type> {
        match recv_ty {
            Type::Dict(_, v) => Some(v.as_ref().clone()),
            Type::TeaModel { fields, .. } => fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, t)| t.as_ref().clone()),
            _ => None,
        }
    }

    /// v0.99: `random.<method>(...)` 的 ambient effect 分型。
    ///
    /// 每个操作一个 ambient 标签（`crate::mir::effect::ambient`），签名由
    /// `infer_program` 入口 `seed_ambient_effect_signatures` 预置进
    /// `effect_signatures` —— 实参数量与逐参类型按签名约束，结果类型
    /// 静态化（此前 `random.*` 一律 `Any`，副作用对类型系统不可见），
    /// 效果行并入标签随调用传播（v0.96/0.97 机制接手：handle 吸收、
    /// 跨函数传播、根边界断言放行 ambient）。
    fn infer_random_method(
        &mut self,
        method: &str,
        arg_types: Vec<Type>,
        mut row: crate::mir::effect::EffectRow,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        // 注：此处形状对齐 CI 的 rustfmt（1.98，`dtolnay/rust-toolchain@stable`
        // 浮动到的最新 stable）—— 它把「RHS 过长的 `let x = match`」拆成
        // `let x =` + 缩进的 `match`，并把臂内的 `{ return Err(…) }` 折成
        // 单行 `=> return Err(…)`。本机 rustfmt 1.96 对同一构造的折法不同
        //（不拆 `let`、保留块）。CI 的 `cargo fmt --all -- --check` 用的是
        // 1.98，故以 1.98 的输出为准。
        let label = match crate::mir::effect::ambient::random_label_for_method(method) {
            Some(l) => l,
            None => {
                return Err(vec![TypeError::UnificationFailure {
                    // v0.104.6 D175：从 `RANDOM_METHODS` **派生**，不再手写一遍。
                    // 原先这里是硬编码字符串 `random/rand_int/rand_float/
                    // rand_choice/seed/shuffle` —— 与 `module_method_names("random")`
                    // 是两份独立清单，改一处忘另一处就会让「报错列出的方法」
                    // 与「自省报出的方法」互相矛盾（agent 按前者写代码，
                    // 按后者做能力判断，两边对不上）。
                    expected: format!(
                        "known random method: random/{}",
                        crate::typeck::dispatch::RANDOM_METHODS.join("/")
                    ),
                    got: format!("random.{}", method),
                    span: Some(span),
                }]);
            }
        };
        let result_ty = match self.effect_signatures.get(label) {
            Some(sig) => {
                if sig.params.len() != arg_types.len() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: sig.params.len(),
                        actual: arg_types.len(),
                        span,
                    }]);
                }
                // 与 infer_perform 同机制：compatible_with 校验（携带 span，
                // 错误定位到调用点）而非延迟 Eq 约束 —— Int<:Float 数字塔、
                // Any/TypeVar 宽容均由 compatible_with 统一处理。
                for (i, (a_ty, p_ty)) in arg_types.iter().zip(sig.params.iter()).enumerate() {
                    if !a_ty.compatible_with(p_ty) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!("random.{} arg {} : {}", method, i, p_ty.name()),
                            got: a_ty.name(),
                            span: Some(span),
                        }]);
                    }
                }
                sig.result.clone()
            }
            // 签名缺失理论上不可达（ambient 预置先于推断）；保守 Any。
            None => Type::Any,
        };
        row = self.merge_rows(
            row,
            crate::mir::effect::EffectRow::Cons(
                label.to_string(),
                Box::new(crate::mir::effect::EffectRow::Empty),
            ),
        );
        Ok((result_ty, row))
    }

    // v0.75.20: infer_pipe 已删——WitnessKind::Pipe 死变体移除，`|>` 在
    // parse_pipe 脱糖为 Call（right(left)），HM 走 infer_call。

    /// v0.80: 闭包推断 — 返回 curried Arrow 类型。
    ///
    /// 多参数闭包 `fn(a, b) -> c` 类型为 `Arrow(A, Arrow(B, C, eff), eff)`。
    /// 闭包定义本身是 pure 的（EffectRow::Empty）——body 的 effects 被捕获
    /// 到 Arrow 类型的 effect row 字段中。
    pub(super) fn infer_sequence(
        &mut self,
        exprs: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        // v0.84: Sequence 推断 — 依次推断每个子表达式，合并 effect row，
        // 返回最后一个表达式的类型（类似 let-expr / do-notation 语义）。
        // 空 Sequence → Type::Nil, EffectRow::Empty。
        if exprs.is_empty() {
            let _span = span;
            let _ = _span;
            return Ok((Type::Nil, crate::mir::effect::EffectRow::Empty));
        }
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut last_ty: Option<Type> = None;
        for expr in exprs {
            let (ty, row) = self.infer_expr(expr)?;
            acc_row = self.merge_rows(acc_row, row);
            last_ty = Some(ty);
        }
        let _span = span;
        let _ = _span;
        Ok((last_ty.unwrap_or(Type::Nil), acc_row))
    }

    /// v0.96: 闭包/函数推断公共核心 —— 参数类型 + body 类型/效果行。
    fn infer_closure_core(
        &mut self,
        params: &[WitnessParam],
        body: &MirWitness,
        _span: Span,
    ) -> Result<(Vec<Type>, Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        // v0.84: 检查重复参数名——闭包/函数参数不允许同名。
        let mut seen_names: HashSet<String> = HashSet::new();
        for p in params {
            if !seen_names.insert(p.name.clone()) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: "distinct parameter names".to_string(),
                    got: format!("duplicate parameter `{}`", p.name),
                    span: Some(_span),
                }]);
            }
        }
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
        // v0.97: 闭包体推断深度 —— 深度 > 0 时 infer_perform 不记录位点
        //（闭包体内的 perform 在调用点发生，上下文未知）。
        self.closure_depth += 1;
        let (body_ty, body_row) = self.infer_expr(body)?;
        self.closure_depth -= 1;
        self.env = saved_env;
        Ok((param_types, body_ty, body_row))
    }

    /// curried Arrow 包装 —— 从最后一个参数向前包裹，每层携带 body 效果行。
    fn wrap_curried_arrow(
        param_types: &[Type],
        body_ty: Type,
        body_row: &crate::mir::effect::EffectRow,
    ) -> Type {
        let mut ty = body_ty;
        for param_ty in param_types.iter().rev() {
            ty = Type::Arrow(Box::new(param_ty.clone()), Box::new(ty), body_row.clone());
        }
        ty
    }

    pub(super) fn infer_closure(
        &mut self,
        params: &[WitnessParam],
        body: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (param_types, body_ty, body_row) = self.infer_closure_core(params, body, span)?;
        let ty = Self::wrap_curried_arrow(&param_types, body_ty, &body_row);
        // v0.96: 按 span 登记定义行 —— 零参闭包类型是裸 TypeVar，
        // let 绑定侧只能经此表取回行（fn_effect_rows 转登记）。
        self.closure_rows.insert(span, body_row);
        // 闭包定义是 pure 的 — body 的 effects 被捕获到 Arrow 类型里。
        Ok((ty, crate::mir::effect::EffectRow::Empty))
    }

    pub(super) fn infer_fn_def(
        &mut self,
        name: Option<&str>,
        params: &[WitnessParam],
        body: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        // fn/task 定义：与闭包同构（immediately-bound closure；名字注册是
        // 调用方的责任 —— 定义名不进 env）。
        // v0.96: body 效果行登记进 `fn_effect_rows`，调用点（infer_call）
        // 据此把被调效果传播进调用方残差行 —— 跨函数效果得以静态可见。
        // （定义本身是纯的：perform 发生在调用时、由调用点上下文负责。）
        let (param_types, body_ty, body_row) = self.infer_closure_core(params, body, span)?;
        let ty = Self::wrap_curried_arrow(&param_types, body_ty, &body_row);
        if let Some(n) = name {
            // v0.97: 合并而非覆盖 —— precompute_fn_effect_rows 的不动点
            // 预计算可能已登记前向引用带来的行；HM 推断行与之取并集。
            let merged = match self.fn_effect_rows.get(n) {
                Some(prev) => Self::union_effect_rows(prev, &body_row),
                None => body_row.clone(),
            };
            self.fn_effect_rows.insert(n.to_string(), merged);
        }
        // v0.96: 同 infer_closure —— 支持 `let g = task f()` 别名绑定的
        // 行转登记（key 是 FnDef witness 的 span）。
        self.closure_rows.insert(span, body_row);
        Ok((ty, crate::mir::effect::EffectRow::Empty))
    }

    pub(super) fn infer_match(
        &mut self,
        scrutinee: &MirWitness,
        arms: &[WitnessArm],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (scrutinee_ty, scrutinee_row) = self.infer_expr(scrutinee)?;
        let mut acc_row = scrutinee_row;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            self.infer_pattern(&arm.pattern, &scrutinee_ty, span)?;

            // v0.87: 将模式绑定变量加入 env，再 infer arm body（与 infer_fn_def
            // 的 save_env / restore 同构）。不添加则 [a, b, ..rest] => a + b 中
            // a 被当作 UnboundVariable。
            let saved_env = self.env.clone();
            self.add_pattern_bindings(&arm.pattern, &scrutinee_ty, span)?;
            let (arm_ty, arm_row) = self.infer_expr(&arm.body)?;
            self.env = saved_env;
            acc_row = self.merge_rows(acc_row, arm_row);
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
        Ok((result_ty.unwrap_or(Type::Unknown), acc_row))
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
                let rest_list_ty = Type::List(Box::new(elem_ty));
                self.infer_pattern(tail, &rest_list_ty, span)?;
                Ok(())
            }
            WitnessPattern::ListVec { elements, rest } => {
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
                for e in elements {
                    self.infer_pattern(e, &elem_ty, span)?;
                }
                if let Some(r) = rest {
                    let rest_list_ty = Type::List(Box::new(elem_ty));
                    self.infer_pattern(r, &rest_list_ty, span)?;
                }
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

    // v0.87: 递归提取模式绑定变量并加入 env。
    // 每个绑定使用 fresh type var（与 infer_fn_def line 516 一致）——
    // 避免从 scrutinee_ty 复制 TypeVar 导致多绑定共享同一变量，unify 时
    // 产生"Cannot unify type variable X with type containing itself"循环。
    // 约束求解器会根据 arm body 的实际用法将 fresh var 合一到正确类型。
    // v0.104.6 D165：放开到 `pub(crate)` —— **双向层**（`bidirectional.rs`
    // 的 Phase D）也必须在 `check_against(arm.body)` 之前注册 arm 的模式绑定。
    //
    // 此前只有 HM 侧的 `infer_match` 会注册，于是：
    //   HM          → 绑定就位，body 推断正确
    //   双向层 Phase D → 直接 `check_against(&arm.body, …)`，env 里**没有**绑定
    //                  → `Unbound variable 'x'`，被 `check_against` 包成
    //                    「type inference failed: …」并 push 进 errors
    // 而 D128 的去重键是 `(line, expected, actual)` —— HM 侧**根本没报错**，
    // 无键可匹配，于是这条**假错误**一路留在最终诊断里：
    // 一段 parser 接受、运行期正常的程序被 `mora --check` 拒绝。
    pub(crate) fn add_pattern_bindings(
        &mut self,
        pattern: &crate::mir::witness::WitnessPattern,
        _scrutinee_ty: &Type,
        _span: Span,
    ) -> Result<(), Vec<TypeError>> {
        use crate::mir::witness::WitnessPattern;
        match pattern {
            WitnessPattern::Wildcard | WitnessPattern::Literal(_) => Ok(()),
            WitnessPattern::Variable(name) => {
                let ty = self.fresh_type_var();
                self.env.add(name.clone(), ty);
                Ok(())
            }
            WitnessPattern::Tuple(items) => {
                for item in items {
                    self.add_pattern_bindings(item, _scrutinee_ty, _span)?;
                }
                Ok(())
            }
            WitnessPattern::List { head, tail } => {
                self.add_pattern_bindings(head, _scrutinee_ty, _span)?;
                self.add_pattern_bindings(tail, _scrutinee_ty, _span)?;
                Ok(())
            }
            WitnessPattern::ListVec { elements, rest } => {
                for e in elements {
                    self.add_pattern_bindings(e, _scrutinee_ty, _span)?;
                }
                if let Some(r) = rest {
                    self.add_pattern_bindings(r, _scrutinee_ty, _span)?;
                }
                Ok(())
            }
            WitnessPattern::Dict { required, rest: _ } => {
                for (_key, value_pat) in required {
                    self.add_pattern_bindings(value_pat, _scrutinee_ty, _span)?;
                }
                Ok(())
            }
            WitnessPattern::TypeAscription {
                name: _name,
                pattern,
            } => {
                self.add_pattern_bindings(pattern, _scrutinee_ty, _span)?;
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
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (_, cond_row) = self.infer_expr(cond)?;
        let (then_ty, then_row) = self.infer_expr(then_branch)?;
        let mut acc_row = self.merge_rows(cond_row, then_row);
        let result = if let Some(e) = else_branch {
            let (else_ty, else_row) = self.infer_expr(e)?;
            acc_row = self.merge_rows(acc_row, else_row);
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
        Ok((result, acc_row))
    }

    pub(super) fn infer_list(
        &mut self,
        items: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let elem_ty = self.fresh_type_var();
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut first_ty: Option<Type> = None;
        // v0.104.6 D67：累加**已解析**的公共元素类型。空列表（无元素可推）
        // 仍回落到 `elem_ty`，行为不变。
        let mut acc_elem: Option<Type> = None;
        for (idx, item) in items.iter().enumerate() {
            let (ty, item_row) = self.infer_expr(item)?;
            acc_row = self.merge_rows(acc_row, item_row);
            // v0.75.86: 提前报 list elem type 不一致（避免 line 0）
            //
            // v0.104.6 D125：改用专门的 `ListElementTypeMismatch` ——
            // 旧的通用 `UnificationFailure` 消息不说明「元素必须同质」这条
            // 约束（`list<T>` 需要单一 T），且 span 指向整个列表的 `[`。
            // 现在带上下标并把 span 指向出错的那个元素。
            if let Some(prev) = &first_ty
                && !ty.compatible_with(prev)
            {
                return Err(vec![TypeError::ListElementTypeMismatch {
                    index: idx,
                    expected: format!("{prev:?}"),
                    got: format!("{ty:?}"),
                    span: Some(item.span),
                }]);
            }
            if first_ty.is_none() {
                first_ty = Some(ty.clone());
            }
            // 数值侧按数值塔提升（`[1i, 2.5]` 的元素类型是 Float 而非首个的
            // Int）；非数值侧沿用首个元素类型 —— 上面的 `compatible_with`
            // 已经保证了「后者能装进前者」，这里只负责挑出对外暴露的那个类型。
            acc_elem = Some(match acc_elem {
                None => ty.clone(),
                Some(prev) => {
                    if is_numeric(&prev) && is_numeric(&ty) {
                        promote_numeric(&prev, &ty)
                    } else {
                        prev
                    }
                }
            });
            self.constraints
                .push(Constraint::Eq(Box::new(elem_ty.clone()), Box::new(ty)));
        }
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok((Type::List(Box::new(acc_elem.unwrap_or(elem_ty))), acc_row))
    }

    pub(super) fn infer_dict(
        &mut self,
        entries: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let k_ty = Type::String;
        let v_ty = self.fresh_type_var();
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut first_v: Option<Type> = None;
        // v0.104.6 D67：同 infer_list —— 累加已解析的公共 value 类型，
        // 空 dict 仍回落到 `v_ty`。
        let mut acc_val: Option<Type> = None;
        for (key, value) in entries {
            let (ty, val_row) = self.infer_expr(value)?;
            acc_row = self.merge_rows(acc_row, val_row);
            // v0.75.86: 提前报 dict value type 不一致（避免 line 0）
            //
            // v0.104.6 D124：改用专门的 `DictValueTypeMismatch` ——
            // 旧的通用 `UnificationFailure` 消息是「Type mismatch: expected
            // Float, got String」，**没说清为什么这里必须同一种类型**（`dict<K,V>`
            // 需要单一 V），而 span 指向整个 dict 的 `{` 而不是出问题的值。
            // 现在带上键名并把 span 指向该值本身。
            if let Some(prev) = &first_v
                && !ty.compatible_with(prev)
            {
                return Err(vec![TypeError::DictValueTypeMismatch {
                    key: key.clone(),
                    expected: format!("{prev:?}"),
                    got: format!("{ty:?}"),
                    span: Some(value.span),
                }]);
            }
            if first_v.is_none() {
                first_v = Some(ty.clone());
            }
            acc_val = Some(match acc_val {
                None => ty.clone(),
                Some(prev) => {
                    if is_numeric(&prev) && is_numeric(&ty) {
                        promote_numeric(&prev, &ty)
                    } else {
                        prev
                    }
                }
            });
            self.constraints
                .push(Constraint::Eq(Box::new(v_ty.clone()), Box::new(ty)));
        }
        // v0.75.86: 不报错路径，保留 _span 备未来错误检查扩展点
        let _span = span;
        let _ = _span;
        Ok((
            Type::Dict(Box::new(k_ty), Box::new(acc_val.unwrap_or(v_ty))),
            acc_row,
        ))
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
    fn pattern_listvec_rest_succeeds() {
        // v0.87: ListVec [a, b, ..rest] on List<Int>
        let mut hm = HMInference::new();
        let elem_ty = Type::Int;
        let list_ty = Type::List(Box::new(elem_ty.clone()));
        let r = hm.infer_pattern(
            &WitnessPattern::ListVec {
                elements: vec![WitnessPattern::Wildcard, WitnessPattern::Wildcard],
                rest: Some(Box::new(WitnessPattern::Variable("rest".to_string()))),
            },
            &list_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_listvec_no_rest_succeeds() {
        // v0.87: ListVec [a, b] (fixed-length) on List<Int>
        let mut hm = HMInference::new();
        let elem_ty = Type::Int;
        let list_ty = Type::List(Box::new(elem_ty.clone()));
        let r = hm.infer_pattern(
            &WitnessPattern::ListVec {
                elements: vec![WitnessPattern::Wildcard, WitnessPattern::Wildcard],
                rest: None,
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

    // v0.83: Dict literal field-by-field 验证

    // v0.92: 测试助手改为 witness-native（MirExpr 路径已删除）。
    fn make_int_lit(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(crate::common::Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    fn make_str_lit(s: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(crate::common::Literal::String(
                s.to_string(),
                Span::default(),
            )),
            span: Span::default(),
        }
    }

    fn make_typed_let(
        type_hint_name: &str,
        type_hint_fields: Vec<(String, Type)>,
        dict_entries: Vec<(String, MirWitness)>,
    ) -> (String, TypeHint, MirWitness) {
        let ty = Type::TeaModel {
            name: type_hint_name.to_string(),
            fields: type_hint_fields
                .into_iter()
                .map(|(n, t)| (n, Box::new(t)))
                .collect(),
        };
        let hint = TypeHint::from_type(ty);
        (
            type_hint_name.to_string(),
            hint,
            MirWitness {
                kind: WitnessKind::Dict(dict_entries),
                span: Span::default(),
            },
        )
    }

    #[test]
    fn teamodel_dict_literal_validates_field_by_field() {
        // v0.83: let x: Counter = {count: 0, step: 1} 应该通过
        let (name, hint, value) = make_typed_let(
            "Counter",
            vec![
                ("count".to_string(), Type::Int),
                ("step".to_string(), Type::Int),
            ],
            vec![
                ("count".to_string(), make_int_lit(0)),
                ("step".to_string(), make_int_lit(1)),
            ],
        );
        let mut hm = HMInference::new();
        let result = hm.infer_let_typed(&name, &hint, &value, Span::default());
        assert!(result.is_ok(), "完整字段匹配应该通过: {:?}", result);
    }

    #[test]
    fn teamodel_dict_literal_missing_field_fails() {
        // v0.83: let x: Counter = {count: 0} 缺 step 字段 —— 失败
        let (name, hint, value) = make_typed_let(
            "Counter",
            vec![
                ("count".to_string(), Type::Int),
                ("step".to_string(), Type::Int),
            ],
            vec![("count".to_string(), make_int_lit(0))],
        );
        let mut hm = HMInference::new();
        let result = hm.infer_let_typed(&name, &hint, &value, Span::default());
        assert!(result.is_err(), "缺 step 字段应该失败");
    }

    #[test]
    fn teamodel_dict_literal_field_type_mismatch_fails() {
        // v0.83: let x: Counter = {count: "0", step: 1} 字段类型不匹配 —— 失败
        let (name, hint, value) = make_typed_let(
            "Counter",
            vec![
                ("count".to_string(), Type::Int),
                ("step".to_string(), Type::Int),
            ],
            vec![
                ("count".to_string(), make_str_lit("0")),
                ("step".to_string(), make_int_lit(1)),
            ],
        );
        let mut hm = HMInference::new();
        let result = hm.infer_let_typed(&name, &hint, &value, Span::default());
        assert!(result.is_err(), "字段类型不匹配应该失败");
    }

    #[test]
    fn teamsg_dict_literal_validates_variant() {
        // v0.83: let x: CounterMsg = {tag: "Increment"} 应该通过
        let ty = Type::TeaMsg {
            name: "CounterMsg".to_string(),
            variants: vec![
                ("Increment".to_string(), None),
                ("Decrement".to_string(), None),
                ("SetStep".to_string(), Some(Box::new(Type::Int))),
            ],
        };
        let hint = TypeHint::from_type(ty);
        let value = MirWitness {
            kind: WitnessKind::Dict(vec![("tag".to_string(), make_str_lit("Increment"))]),
            span: Span::default(),
        };
        let mut hm = HMInference::new();
        let result = hm.infer_let_typed("x", &hint, &value, Span::default());
        assert!(result.is_ok(), "已知 variant 应该通过: {:?}", result);
    }

    #[test]
    fn teamsg_dict_literal_unknown_variant_fails() {
        // v0.83: let x: CounterMsg = {tag: "NonExist"} —— 失败
        let ty = Type::TeaMsg {
            name: "CounterMsg".to_string(),
            variants: vec![
                ("Increment".to_string(), None),
                ("Decrement".to_string(), None),
            ],
        };
        let hint = TypeHint::from_type(ty);
        let value = MirWitness {
            kind: WitnessKind::Dict(vec![("tag".to_string(), make_str_lit("NonExist"))]),
            span: Span::default(),
        };
        let mut hm = HMInference::new();
        let result = hm.infer_let_typed("x", &hint, &value, Span::default());
        assert!(result.is_err(), "未知 variant 应该失败");
    }

    #[test]
    fn teamsg_dict_literal_missing_tag_fails() {
        // v0.83: let x: CounterMsg = {payload: 0} 缺 tag 字段 —— 失败
        let ty = Type::TeaMsg {
            name: "CounterMsg".to_string(),
            variants: vec![("Increment".to_string(), None)],
        };
        let hint = TypeHint::from_type(ty);
        let value = MirWitness {
            kind: WitnessKind::Dict(vec![("payload".to_string(), make_int_lit(0))]),
            span: Span::default(),
        };
        let mut hm = HMInference::new();
        let result = hm.infer_let_typed("x", &hint, &value, Span::default());
        assert!(result.is_err(), "缺 tag 字段应该失败");
    }

    // v0.84: Layer 2a — Sequence 推断

    fn make_lit_w(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(crate::common::Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    fn make_var_w(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::default(),
        }
    }

    #[test]
    fn sequence_empty_returns_nil() {
        let mut hm = HMInference::new();
        let result = hm.infer_sequence(&[], Span::default());
        assert!(result.is_ok());
        let (ty, row) = result.unwrap();
        assert_eq!(ty, Type::Nil);
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn sequence_returns_last_expr_type() {
        let mut hm = HMInference::new();
        let exprs = vec![make_lit_w(1), make_lit_w(2), make_lit_w(42)];
        let result = hm.infer_sequence(&exprs, Span::default());
        assert!(result.is_ok(), "all-int sequence should succeed");
        // Solve constraints to get concrete types
        let (ty, _row) = result.unwrap();
        // After constraint solving, all ints unify to Int
        let result = hm.instantiate_type(&ty);
        // The inferred type is a TypeVar that got constrained to Int via
        // the literal 42. With Unknown-compatible-with, the literals
        // produce concrete Int types.
        assert!(
            matches!(result, Type::Int) || matches!(result, Type::TypeVar(_)),
            "expected Int or TypeVar, got {:?}",
            result
        );
    }

    #[test]
    fn sequence_merges_effect_rows() {
        let mut hm = HMInference::new();
        // Two int literals — pure, no effects. Row stays Empty.
        let exprs = vec![make_lit_w(1), make_lit_w(2)];
        let result = hm.infer_sequence(&exprs, Span::default());
        assert!(result.is_ok());
        let (_ty, row) = result.unwrap();
        assert!(
            matches!(row, crate::mir::effect::EffectRow::Empty),
            "pure sequence should have Empty row"
        );
    }

    #[test]
    fn sequence_with_variable_infer_ok() {
        let mut hm = HMInference::new();
        // Register x as Int in env
        hm.env.add("x".to_string(), crate::typeck::Type::Int);
        let exprs = vec![make_var_w("x")];
        let result = hm.infer_sequence(&exprs, Span::default());
        assert!(result.is_ok(), "variable lookup should succeed");
        let (ty, _row) = result.unwrap();
        assert_eq!(ty, Type::Int);
    }

    // v0.84: Layer 2b — Closure 重复参数检测

    #[test]
    fn closure_duplicate_param_names_error() {
        use crate::mir::witness::WitnessParam;
        let params = vec![
            WitnessParam {
                name: "x".to_string(),
                type_hint: None,
                default: None,
            },
            WitnessParam {
                name: "x".to_string(), // duplicate
                type_hint: None,
                default: None,
            },
        ];
        let body = make_lit_w(1);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&params, &body, Span::default());
        assert!(result.is_err(), "duplicate param 'x' should error");
        let errors = result.unwrap_err();
        assert!(
            errors.iter().any(|e| {
                let msg = format!("{:?}", e);
                msg.contains("duplicate") || msg.contains("x")
            }),
            "error should mention duplicate param, got: {:?}",
            errors
        );
    }

    #[test]
    fn closure_distinct_params_ok() {
        use crate::mir::witness::WitnessParam;
        let params = vec![
            WitnessParam {
                name: "x".to_string(),
                type_hint: None,
                default: None,
            },
            WitnessParam {
                name: "y".to_string(),
                type_hint: None,
                default: None,
            },
        ];
        let body = make_lit_w(1);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&params, &body, Span::default());
        assert!(result.is_ok(), "distinct params should succeed");
        // Result is Arrow(TypeVar, Arrow(TypeVar, TypeVar, Empty), Empty)
        let (ty, row) = result.unwrap();
        assert!(matches!(ty, Type::Arrow(_, _, _)));
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn closure_single_param_ok() {
        use crate::mir::witness::WitnessParam;
        let params = vec![WitnessParam {
            name: "x".to_string(),
            type_hint: None,
            default: None,
        }];
        let body = make_lit_w(42);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&params, &body, Span::default());
        assert!(result.is_ok());
        let (ty, row) = result.unwrap();
        // Single param → Arrow(param, body_ty, row)
        assert!(matches!(ty, Type::Arrow(_, _, _)));
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn closure_type_hint_param_ok() {
        use crate::mir::witness::WitnessParam;
        let hint = TypeHint::from_type(Type::Float);
        let params = vec![WitnessParam {
            name: "x".to_string(),
            type_hint: Some(hint),
            default: None,
        }];
        let body = make_lit_w(1);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&params, &body, Span::default());
        assert!(result.is_ok(), "typed param should succeed");
    }
}
