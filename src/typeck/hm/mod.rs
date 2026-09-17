//! v0.55: Hindley-Milner Type Inference for MirWitness.
//!
//! This module is the MirWitness-native replacement of the ast_v2-based
//! `typeck::check_program` pipeline. The earlier v0.53 prototype still
//! referenced `NodeId` / `AstArena` and silently no-op'd any closure type
//! check because `Type::Closure` is a unit variant. The current rewrite
//! drives inference directly off `&MirWitness`, tracks closure signatures in
//! a side table keyed by a fresh `Type::TypeVar`, and covers every
//! `WitnessKind` variant.
//!
//! Public entry point: [`check_program_mir`] (re-exported from
//! `crate::typeck`).

use std::collections::{HashMap, HashSet};

use crate::common::Span;
use crate::mir::witness::{
    BuiltinOp, MirWitness, WitnessArm, WitnessCallee, WitnessKind, WitnessParam,
};
use crate::typeck::Type;

mod builtin; // v0.75.70: builtin 类型推断（自 mod.rs 拆出）
pub mod diag; // v0.75.94: DiagFilter + WitnessNodeId（自 mod.rs 抽离）
pub mod env;
pub mod error;
pub mod generalize;
mod infer;
pub mod unify; // v0.75.70: infer_* 方法族（自 mod.rs 拆出）
pub mod util; // v0.75.96: check_union / join_types（自 typeck::mod.rs 抽离）
// v0.80: 行多态 HM unification（Stage 2/4 algebraic effects 落地）
pub mod row;

pub use error::TypeError;

use env::TypeEnv;
use unify::{Constraint, Substitution};

///  Signature for a closure / callable.
///  Stored in a side table keyed by the `Type` (always a fresh
///  `Type::TypeVar`) that represents the closure's identity.
#[derive(Debug, Clone)]
pub struct ClosureSig {
    pub params: Vec<Type>,
    pub return_type: Type,
    /// Number of declared parameters; used to arity-check calls.
    pub arity: usize,
}

/// v0.97: handle 推断帧 —— 一层 handle body 推断期间收集的 perform 位点。
#[derive(Debug)]
pub struct HandleFrame {
    pub effect: String,
    pub sites: Vec<PerformSite>,
}

/// v0.97: 一个 perform 位点的静态类型连接材料。
#[derive(Debug, Clone)]
pub struct PerformSite {
    /// perform 的结果类型（fresh var —— 与 handler 返回类型 Eq 统一）
    pub result_ty: Type,
    /// 各实参的推断类型（派生 handler 的 __arg0..N）
    pub arg_tys: Vec<Type>,
}

/// v0.98: 显式 effect 签名 —— 一个 effect 标签的静态契约。
///
/// 语法 `effect Name(Hint, ...): Hint`（结果缺省 = Any）。
/// 语义：
/// - perform 位点校验：实参数/类型必须符合声明（在位点处报错）；
/// - perform 结果类型取自签名（不再 fresh var）—— fn/task 体内的高阶
///   位点由此静态化（不再需要调用点的 handle 才能精确）；
/// - handler `__arg0..N` 按签名参数类型注册（优先于 v0.97 的位点推导）；
/// - handler 返回类型必须兼容签名结果（resume 契约的声明化）。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectSignature {
    pub params: Vec<Type>,
    pub result: Type,
}

#[derive(Default)]
pub struct HMInference {
    pub env: TypeEnv,
    pub fresh_counter: usize,
    pub constraints: Vec<Constraint>,
    /// Side table keyed by the `char` of a fresh `Type::TypeVar` that
    /// was minted as a closure's identity. The same `char` is also
    /// stored inside the closure's `Type::TypeVar(_)` so callers can
    /// recover the signature by extracting the variable identifier.
    pub closure_sigs: HashMap<char, ClosureSig>,
    /// Stack of in-scope closure names introduced by FnDef so that a
    /// recursive function can refer to itself.
    pub fn_scope: Vec<String>,
    /// v0.80: row-polymorphic fresh var generator (algebraic effects
    /// EffectRow::Var namer; namespace independent from TypeVar(char)).
    pub fresh_vars: row::FreshVars,
    // v0.75.94: 移除 `diagnosed: HashSet<WitnessNodeId>` 字段 + 3 个方法
    // (`mark_diagnosed` / `is_diagnosed` / `is_diagnosed_at`) —— 抽离到
    // `crate::typeck::hm::diag::DiagFilter`（双向定型专用基础设施）。
    // HM 公共 API 回归到 v0.75.86 之前的纯粹 HM 状态。
    /// v0.97: handle 推断帧 —— body 推断期间收集本层 effect 的 perform
    /// 位点（infer_perform 记录到帧顶）。栈语义与运行时 take 栈一致：
    /// 内层同标签 handle 的 body 位点归内层帧，其 handler 体位点归外层帧。
    pub handle_stack: Vec<HandleFrame>,
    /// v0.97: 闭包/fn 体推断深度 —— 深度 > 0 时不记录 perform 位点
    /// （调用点上下文未知，高阶位点的结果连接留给 effect signature）。
    pub closure_depth: usize,
    /// v0.98: 显式 effect 签名表（label → 契约）。由 `EffectSig` witness
    /// 预扫描注册（infer_program 入口，文件全局 —— 先于顺序推断）与
    /// import 通道注入。有签名时 perform 位点/结果与 handler 参数/返回
    /// 全部按契约静态化。
    pub effect_signatures: HashMap<String, EffectSignature>,
    /// v0.89: Shadow type table — captures infer_expr results per witness
    /// node before substitution. Keyed by Span (unique within a file).
    /// Used by `export_type_table` to produce a TypeTable without
    /// modifying infer/unify/bidirectional logic.
    pub shadow_types: HashMap<Span, (Type, crate::mir::effect::EffectRow)>,
    /// v0.96: 顶层 fn/task 定义的效果行登记（name → body 残差行）。
    ///
    /// `task name() ...` 定义在 witness 层是 [`WitnessKind::FnDef`]，但
    /// 定义名不进 [`Self::env`]（无 Arrow 类型）—— 调用点只能靠本表把
    /// 被调函数的效果行传播进调用方残差行，供顶层边界断言
    /// （`infer_program`）判定 unhandled effect。行内含 Var 时为
    /// 多态保守行（`contains` 恒真），不参与具体标签判定。
    pub fn_effect_rows: HashMap<String, crate::mir::effect::EffectRow>,
    /// v0.102: 关系签名表（name → 位置参数类型）。`precompute_rel_sigs`
    /// 不动点预计算（头字面量 + 体关系调用传播）；关系调用点按签名
    /// 校验实参并返回 Goal。
    pub rel_sigs: HashMap<String, Vec<Type>>,
    /// v0.102: 关系效果行登记（name → 子句体残差行）。solve 位点的效果
    /// 经关系调用传播（与 fn_effect_rows 同机制）。
    pub rel_effect_rows: HashMap<String, crate::mir::effect::EffectRow>,
    /// v0.96: 闭包/fn 定义节点（按定义 span）的效果行。零参闭包的类型是
    /// 裸 TypeVar，无 Arrow 层可携带行 —— let 绑定时经本表转登记进
    /// `fn_effect_rows`（key 是定义 witness 的 span，文件内唯一）。
    pub closure_rows: HashMap<Span, crate::mir::effect::EffectRow>,
}

// v0.75.94: 重新导出 WitnessNodeId（抽离到 diag 子模块）以保留外部 API
// 路径兼容。调用方可以继续 `use crate::typeck::hm::WitnessNodeId`。
pub use diag::WitnessNodeId;

impl HMInference {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh type variable identifier.
    pub fn fresh_type_var_id(&mut self) -> char {
        let id = std::char::from_u32(self.fresh_counter as u32).unwrap_or('\u{10FFFF}');
        self.fresh_counter += 1;
        id
    }

    /// Mint a fresh `Type::TypeVar`.
    pub fn fresh_type_var(&mut self) -> Type {
        Type::TypeVar(self.fresh_type_var_id())
    }

    /// v0.80: Mint a fresh `EffectRow::Var` for row-polymorphic unification.
    /// Each call produces a distinct row variable name (rho0, rho1, ...).
    pub fn fresh_row_var(&mut self) -> crate::mir::effect::EffectRow {
        crate::mir::effect::EffectRow::Var(self.fresh_vars.row_var(""))
    }

    /// Record a fresh closure signature and return the type variable
    /// that callers can use to refer to it.
    pub fn fresh_closure(&mut self, params: Vec<Type>, return_type: Type) -> Type {
        let id = self.fresh_type_var_id();
        let ty = Type::TypeVar(id);
        self.closure_sigs.insert(
            id,
            ClosureSig {
                arity: params.len(),
                params,
                return_type,
            },
        );
        ty
    }

    /// Recover a closure signature from its type. Returns `None` if the
    /// type is not a known closure identity.
    pub fn closure_sig(&self, ty: &Type) -> Option<&ClosureSig> {
        match ty {
            Type::TypeVar(c) => self.closure_sigs.get(c),
            _ => None,
        }
    }

    /// v0.75.17: 展开 env 中命中的 ForAll（标准 HM let-polymorphism 展开）。
    ///
    /// v0.80: 移除 closure 身份复制逻辑（ClosureSig 侧表已删除，Arrow 是
    /// 自包含类型）。remap 改为 &mut HashMap — 首次遇到量化变量时 mint fresh
    /// 并插入 remap；后续遇到同一变量时从 remap 取值，保证 param/return 一致。
    pub fn instantiate_type(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(vars, inner) => {
                let quantified: HashSet<char> = vars.iter().cloned().collect();
                let mut remap: HashMap<char, char> = HashMap::new();
                self.instantiate_ty(inner, &quantified, &mut remap)
            }
            _ => ty.clone(),
        }
    }

    /// v0.75.97: 如果 `ty` 是 `Type::ForAll` 则实例化（每次 fresh 副本），
    /// 否则原样返回。封装「命中 env 后实例化」通用模式——
    /// `infer_var`（赋值 LHS 单形化）与 `infer_call`（Var callee 单形化）
    /// 两个调用点共享此语义。
    ///
    /// 与 [`instantiate_type`](Self::instantiate_type) 区别：
    ///   - `instantiate_type`: 无条件 match ForAll（已是 ForAll 才展开）
    ///   - `instantiate_if_forall`: 若 ForAll 则实例化，否则直接 clone 返回
    ///     （省去 caller 写 match 的开销 + 集中单形化语义）
    pub fn instantiate_if_forall(&mut self, ty: &Type) -> Type {
        match ty {
            Type::ForAll(_, _) => self.instantiate_type(ty),
            _ => ty.clone(),
        }
    }

    /// 把 ForAll 内层 τ 中被量化的 TypeVar 替换为 fresh 变量（未量化的保留）。
    ///
    /// v0.80: remap 从 &HashMap 改为 &mut HashMap — 修复「同一量化变量出现
    /// 多次时拿到不同 fresh var」的 bug。首次遇到量化变量时 mint fresh 并插入
    /// remap；后续遇到同一变量时从 remap 取值，保证 param/return 一致性。
    pub(super) fn instantiate_ty(
        &mut self,
        ty: &Type,
        quantified: &HashSet<char>,
        remap: &mut HashMap<char, char>,
    ) -> Type {
        match ty {
            Type::TypeVar(c) => {
                if quantified.contains(c) {
                    match remap.get(c) {
                        // 已映射的量化变量 → 复用 fresh 身份
                        Some(fresh) => Type::TypeVar(*fresh),
                        // 首次遇到 → mint fresh 并记录映射
                        None => {
                            let fresh = self.fresh_type_var_id();
                            remap.insert(*c, fresh);
                            Type::TypeVar(fresh)
                        }
                    }
                } else {
                    Type::TypeVar(*c)
                }
            }
            Type::List(elem) => Type::List(Box::new(self.instantiate_ty(elem, quantified, remap))),
            Type::Dict(k, v) => Type::Dict(
                Box::new(self.instantiate_ty(k, quantified, remap)),
                Box::new(self.instantiate_ty(v, quantified, remap)),
            ),
            Type::Result_(ok, err) => Type::Result_(
                Box::new(self.instantiate_ty(ok, quantified, remap)),
                Box::new(self.instantiate_ty(err, quantified, remap)),
            ),
            Type::Union(members) => Type::Union(
                members
                    .iter()
                    .map(|m| self.instantiate_ty(m, quantified, remap))
                    .collect(),
            ),
            // v0.75.17: 嵌套 ForAll — 内层量化变量冻结（遮蔽外层，不展开）
            Type::ForAll(inner_vars, inner) => {
                let shadowed: HashSet<char> = inner_vars.iter().cloned().collect();
                let active: HashSet<char> = quantified
                    .iter()
                    .filter(|c| !shadowed.contains(c))
                    .cloned()
                    .collect();
                Type::ForAll(
                    inner_vars.clone(),
                    Box::new(self.instantiate_ty(inner, &active, remap)),
                )
            }
            // v0.80: Arrow — 递归实例化 input/output，effect row 走 rename_row。
            Type::Arrow(input, output, row) => Type::Arrow(
                Box::new(self.instantiate_ty(input, quantified, remap)),
                Box::new(self.instantiate_ty(output, quantified, remap)),
                crate::typeck::hm::row::rename_row(row, &mut self.fresh_vars),
            ),
            _ => ty.clone(),
        }
    }

    /// Solve all collected constraints, returning the final Substitution
    /// and any diagnostics. The Substitution is needed by `export_type_table`
    /// to resolve type variables in the shadow table.
    pub fn solve_constraints(&mut self) -> (Substitution, Vec<TypeError>) {
        let mut subst = Substitution::new();
        let mut errors: Vec<TypeError> = Vec::new();
        for constraint in self.constraints.drain(..) {
            match unify::solve(&constraint, &subst) {
                Ok(new_subst) => subst = new_subst,
                Err(err) => {
                    errors.push(err);
                    // Continue with a fresh substitution so a single bad
                    // program does not abort the whole analysis.
                    subst = Substitution::new();
                }
            }
        }
        (subst, errors)
    }

    /// Drive inference across an entire MirWitness program. Returns the
    /// list of collected diagnostics. The internal Substitution is consumed
    /// to resolve type variables in the shadow type table.
    ///
    /// v0.96: 顶层**边界断言** —— 每个顶层 witness 的残差效果行不允许含
    /// 具名标签。这是 algebraic effects 的编译期强制闭环：Perform 产生
    /// 标签、Handle 用行差吸收、跨函数调用经 `fn_effect_rows` 传播，
    /// 最终没有任何 handle 兜住的效果在程序边界在此报错（运行时
    /// "unhandled effect" 兜底自此只防御动态生成代码）。
    /// 残差行仅含 Var（多态未知行）时宽松放行 —— 不做错误拒绝。
    pub fn infer_program(&mut self, exprs: &[MirWitness]) -> Vec<TypeError> {
        let mut errors: Vec<TypeError> = Vec::new();
        // v0.99: ambient effect 签名预置 —— 先于文件级 EffectSig 预扫描。
        // 用户对同一标签的重复声明走既有重复规则（一致幂等放行，不一致
        // 报错指向声明处）。
        self.seed_ambient_effect_signatures();
        // v0.98: effect 签名预扫描 —— EffectSig 是文件全局声明（效果标签
        // 的契约），先于顺序推断全量注册，perform/handle 在任意位置可查。
        // 重复声明且签名不一致 → 报错（重复声明且一致 → 幂等放行）。
        self.precompute_effect_signatures(exprs, &mut errors);
        // v0.97: 不动点预计算 —— fn/task 定义的 witness 树行走器在**顺序
        // 推断开始前**把全部定义（含嵌套、含前向引用）的效果行算到不动点，
        // mutual recursion / 后文定义的调用得以传播效果。行单调增长
        // （标签只增不减），有限标签集保证终止。
        // v0.102: 关系签名/效果行不动点预计算与 fn 行预计算交错两轮 ——
        // 含 solve 的 fn 体需要 rel 行进表；调用 fn 的 rel 子句体需要 fn 行。
        self.precompute_rel_sigs(exprs);
        self.precompute_fn_effect_rows(exprs);
        self.precompute_rel_sigs(exprs);
        for expr in exprs {
            match self.infer_expr(expr) {
                Err(mut errs) => errors.append(&mut errs),
                Ok((_, row)) => {
                    // v0.99: ambient 标签由运行时根状态兜底（查找顺序：
                    // 用户 handle 注册表 → CoreRuntime.random_state）——
                    // 根边界放行纯 ambient 残差。定位第一个**非 ambient**
                    // 标签精准报错；只跳过 ambient，不吞其他 unhandled。
                    if let Some(label) = row
                        .labels()
                        .into_iter()
                        .find(|l| !crate::mir::effect::ambient::is_ambient_label(l))
                        .map(|s| s.to_string())
                    {
                        errors.push(self.localize_unhandled_effect(expr, &label));
                    }
                }
            }
        }
        let (_subst, mut solve_errors) = self.solve_constraints();
        errors.append(&mut solve_errors);
        errors
    }

    /// v0.99: ambient effect 签名预置 —— random 模块的每操作契约
    /// （标签 ↔ `random.<method>` 映射与运行时分发同源）。
    ///
    /// 这些签名在文件级 `effect` 声明预扫描**之前**入表：用户 `handle
    /// random_* { ... }` / `perform random_*(...)` 直接获得契约校验；
    /// 用户重声明同标签走 `precompute_effect_signatures` 的重复规则
    /// （一致幂等、不一致报错 —— expected 侧显示的就是 ambient 契约）。
    fn seed_ambient_effect_signatures(&mut self) {
        use crate::mir::effect::ambient;
        // 参数按语言现实声明：**无后缀数字字面量在词法层一律是 Float**
        // （lexer.rs number_from 无后缀分支 → TokenType::Float），因此
        // `random.seed(42)` 的实参类型是 Float 而非 Int；真正的 Int 值
        // （如 len(x)）经 Int<:Float 数字塔 widening 由 compatible_with
        // 放行。rand_int 语义上是整数区间，但实参按语言现实收 Float。
        let entries: [(&str, Vec<Type>, Type); ambient::RANDOM_LABELS.len()] = [
            ("random_random", vec![], Type::Float),
            (
                "random_rand_int",
                vec![Type::Float, Type::Float],
                Type::Float,
            ),
            (
                "random_rand_float",
                vec![Type::Float, Type::Float],
                Type::Float,
            ),
            (
                "random_rand_choice",
                vec![Type::List(Box::new(Type::Any))],
                Type::Any,
            ),
            ("random_seed", vec![Type::Float], Type::Nil),
            (
                "random_shuffle",
                vec![Type::List(Box::new(Type::Any))],
                Type::List(Box::new(Type::Any)),
            ),
        ];
        for (label, params, result) in entries {
            debug_assert!(ambient::is_ambient_label(label));
            self.effect_signatures
                .entry(label.to_string())
                .or_insert(EffectSignature { params, result });
        }
    }

    /// v0.98: effect 签名预扫描 —— 收集全部 EffectSig witness（含嵌套）
    /// 并注册进 `effect_signatures`。重复声明且签名不一致 → 报错指向
    /// 第二处声明；一致 → 幂等放行（模块合并场景）。
    fn precompute_effect_signatures(&mut self, exprs: &[MirWitness], errors: &mut Vec<TypeError>) {
        fn collect<'a>(w: &'a MirWitness, out: &mut Vec<&'a MirWitness>) {
            if let WitnessKind::EffectSig { .. } = &w.kind {
                out.push(w);
            }
            for c in w.child_witnesses() {
                collect(c, out);
            }
        }
        let mut sigs: Vec<&MirWitness> = Vec::new();
        for e in exprs {
            collect(e, &mut sigs);
        }
        for w in sigs {
            if let WitnessKind::EffectSig {
                name,
                params,
                result,
            } = &w.kind
            {
                let sig = EffectSignature {
                    params: params.iter().map(|h| h.to_type().clone()).collect(),
                    result: result
                        .as_ref()
                        .map(|h| h.to_type().clone())
                        .unwrap_or(Type::Any),
                };
                match self.effect_signatures.get(name) {
                    Some(prev) if prev != &sig => {
                        errors.push(TypeError::UnificationFailure {
                            expected: format!("effect `{}` signature {:?}", name, prev),
                            got: format!("{:?}", sig),
                            span: Some(w.span),
                        });
                    }
                    _ => {
                        self.effect_signatures.insert(name.clone(), sig);
                    }
                }
            }
        }
    }

    /// v0.97: fn/task 定义效果行的不动点预计算。
    ///
    /// 顺序推断的限制：`task a() { b() }` 在 `task b()` 之前定义时，推断
    /// 到 a 的 body 时 `fn_effect_rows` 还没有 b 的行 —— mutual recursion
    /// 与前向引用的效果静默漏检。本方法用**树行走器**（不跑完整 HM，纯
    /// 数据操作）把全部定义的效果行迭代到不动点后并入登记表：
    /// - 行单调增长（标签只增不减），标签集来自树内有限的 Perform 节点，
    ///   迭代必然终止（另设 64 轮防御上限）；
    /// - 定义节点本身是纯的（行在调用时发生），但全部定义（含嵌套）都被
    ///   收集并各自计算行 —— 前向引用由此可见。
    pub fn precompute_fn_effect_rows(&mut self, exprs: &[MirWitness]) {
        fn collect_fn_defs<'a>(w: &'a MirWitness, out: &mut Vec<&'a MirWitness>) {
            if let WitnessKind::FnDef { .. } = &w.kind {
                out.push(w);
            }
            for c in w.child_witnesses() {
                collect_fn_defs(c, out);
            }
        }
        let mut defs: Vec<&MirWitness> = Vec::new();
        for e in exprs {
            collect_fn_defs(e, &mut defs);
        }
        if defs.is_empty() {
            return;
        }
        let mut table: HashMap<String, crate::mir::effect::EffectRow> = self.fn_effect_rows.clone();
        // v0.102: 关系行并入同一查找表（solve 目标树里的关系调用与
        // fn 调用同表解析；名字空间不重叠 —— 同名绑定 env 里只有一个）
        for (n, r) in self.rel_effect_rows.clone() {
            table.insert(n, r);
        }
        for round in 0..64u32 {
            let mut changed = false;
            for def in &defs {
                if let WitnessKind::FnDef { name, body, .. } = &def.kind {
                    let mut ambient = std::collections::HashSet::new();
                    let row = Self::tree_effect_row(body, &mut ambient, &table);
                    let merged = match table.get(name) {
                        Some(prev) => Self::union_effect_rows(prev, &row),
                        None => row,
                    };
                    if table.get(name) != Some(&merged) {
                        table.insert(name.clone(), merged);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
            let _ = round;
        }
        for (name, row) in table {
            self.fn_effect_rows.insert(name, row);
        }
    }

    /// v0.97: witness 树的效果行直接计算（与 HM 推断同行代数的数据形态）。
    ///
    /// - Perform：环境已处理 → 无贡献；否则产生标签（实参行并入）。
    /// - Handle：body 行去掉被捕获标签（吸收）∪ handler 行。
    /// - Call：被调效果行（登记表 / 立即调用闭包内联 body 行）∪ 实参行。
    /// - Closure/FnDef/UpdateDef 定义节点：纯（行被捕获，不并入外层）。
    /// - LetBinding 绑定闭包/函数：绑定处纯；其他值行上浮。
    fn tree_effect_row(
        w: &MirWitness,
        ambient: &mut std::collections::HashSet<String>,
        table: &HashMap<String, crate::mir::effect::EffectRow>,
    ) -> crate::mir::effect::EffectRow {
        use crate::mir::effect::EffectRow;
        let union3 = |a: EffectRow, b: EffectRow| Self::union_effect_rows(&a, &b);
        match &w.kind {
            WitnessKind::Perform { effect, args } => {
                let mut row = if ambient.contains(effect) {
                    EffectRow::Empty
                } else {
                    EffectRow::Cons(effect.clone(), Box::new(EffectRow::Empty))
                };
                for a in args {
                    row = union3(row, Self::tree_effect_row(a, ambient, table));
                }
                row
            }
            WitnessKind::Handle {
                effect,
                body,
                handler,
                ..
            } => {
                ambient.insert(effect.clone());
                let b = Self::tree_effect_row(body, ambient, table).remove(effect);
                ambient.remove(effect);
                let h = Self::tree_effect_row(handler, ambient, table);
                union3(b, h)
            }
            // v0.102: 关系定义节点纯（行经 rel_effect_rows 登记表在
            // solve 调用点生效 —— 与 FnDef/Closure 同规则）
            WitnessKind::RelDef { .. } => EffectRow::Empty,
            WitnessKind::Call { callee, args } => {
                let mut row = match callee {
                    WitnessCallee::Var(n) | WitnessCallee::Name(n) => {
                        // v0.102: project(fn, ...) 的行 = 被投函数的登记行
                        if n == "project" {
                            match args.first().map(|a| &a.kind) {
                                Some(WitnessKind::Variable(f))
                                | Some(WitnessKind::FnDef { name: f, .. }) => {
                                    table.get(f).cloned().unwrap_or(EffectRow::Empty)
                                }
                                _ => EffectRow::Empty,
                            }
                        } else {
                            table.get(n).cloned().unwrap_or(EffectRow::Empty)
                        }
                    }
                    WitnessCallee::Evaluated(e) => match &e.kind {
                        // 立即调用的闭包：body 效果在调用点发生
                        WitnessKind::Closure { body, .. } => {
                            Self::tree_effect_row(body, ambient, table)
                        }
                        _ => Self::tree_effect_row(e, ambient, table),
                    },
                    WitnessCallee::Builtin(_) | WitnessCallee::Method(_, _) => EffectRow::Empty,
                };
                for a in args {
                    row = union3(row, Self::tree_effect_row(a, ambient, table));
                }
                row
            }
            WitnessKind::Closure { .. }
            | WitnessKind::FnDef { .. }
            | WitnessKind::UpdateDef { .. } => EffectRow::Empty,
            WitnessKind::LetBinding {
                value, init_body, ..
            } => {
                let v = match &value.kind {
                    WitnessKind::Closure { .. } | WitnessKind::FnDef { .. } => EffectRow::Empty,
                    _ => Self::tree_effect_row(value, ambient, table),
                };
                union3(v, Self::tree_effect_row(init_body, ambient, table))
            }
            WitnessKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                // v0.99: random 模块方法调用 = ambient perform（与 HM 推断
                // 同一行代数 —— 否则 mutual recursion / 前向引用经本行走器
                // 传播时漏掉 random 标签）。纯数据行走器无类型信息，按接收
                // 者变量名 `random`（语言级绑定名）分类；别名接收者
                // （`let m = random; m.foo()`）保守按纯处理 —— 欠近似只可
                // 能漏报，不会误拒；主推断路径的 RandomModule 分支仍覆盖。
                let mut row = match &receiver.kind {
                    WitnessKind::Variable(n) if n == "random" => {
                        match crate::mir::effect::ambient::random_label_for_method(method) {
                            Some(l) => EffectRow::Cons(l.to_string(), Box::new(EffectRow::Empty)),
                            None => EffectRow::Empty,
                        }
                    }
                    _ => Self::tree_effect_row(receiver, ambient, table),
                };
                for a in args {
                    row = union3(row, Self::tree_effect_row(a, ambient, table));
                }
                row
            }
            _ => {
                let mut row = EffectRow::Empty;
                for c in w.child_witnesses() {
                    row = union3(row, Self::tree_effect_row(c, ambient, table));
                }
                row
            }
        }
    }

    /// v0.97: 效果行并集 —— 具名标签的集合并（保序去重）；Var（多态未知
    /// 行）不贡献具体标签，仅在全空时保留未知性。
    fn union_effect_rows(
        a: &crate::mir::effect::EffectRow,
        b: &crate::mir::effect::EffectRow,
    ) -> crate::mir::effect::EffectRow {
        let mut labels: Vec<String> = a.labels().into_iter().map(String::from).collect();
        for l in b.labels() {
            if !labels.iter().any(|x| x == l) {
                labels.push(l.to_string());
            }
        }
        let mut row = crate::mir::effect::EffectRow::Empty;
        for l in labels {
            row.extend(&l);
        }
        if matches!(row, crate::mir::effect::EffectRow::Empty)
            && (matches!(a, crate::mir::effect::EffectRow::Var(_))
                || matches!(b, crate::mir::effect::EffectRow::Var(_)))
        {
            row = crate::mir::effect::EffectRow::Var("rho".to_string());
        }
        row
    }

    /// v0.96: 把顶层残差行中的未处理效果定位到发起 witness（精准 span）。
    fn localize_unhandled_effect(&self, w: &MirWitness, label: &str) -> TypeError {
        let mut ambient = std::collections::HashSet::new();
        if let Some((span, via)) = self.find_unhandled(w, label, &mut ambient) {
            TypeError::EffectRowMismatch {
                expected: "no unhandled effects — wrap in a matching `handle` block".to_string(),
                got: match via {
                    Some(callee) => format!("{{ {label} }} (perform inside `{callee}`)"),
                    None => format!("{{ {label} }}"),
                },
                span: Some(span),
            }
        } else {
            TypeError::EffectRowMismatch {
                expected: "no unhandled effects".to_string(),
                got: format!("{{ {label} }}"),
                span: Some(w.span),
            }
        }
    }

    /// v0.96: 在 witness 树内找第一个发起未处理 `label` 效果的位置。
    ///
    /// - Handle 扩展环境已处理集（body 内的 perform 被它吸收）；
    /// - Closure/FnDef/UpdateDef 体是独立效果上下文（行已捕获进 Arrow /
    ///   `fn_effect_rows`）—— 不下潜，否则会用外层环境误判定义体内的
    ///   perform（这正是「词法检查错误拒绝合法程序」的陷阱）；
    /// - Call 到已登记效果行的 callee：行含未处理标签 → 报在调用点。
    fn find_unhandled(
        &self,
        w: &MirWitness,
        label: &str,
        ambient: &mut std::collections::HashSet<String>,
    ) -> Option<(Span, Option<String>)> {
        match &w.kind {
            WitnessKind::Perform { effect, args } => {
                if effect == label && !ambient.contains(effect) {
                    return Some((w.span, None));
                }
                for a in args {
                    if let Some(f) = self.find_unhandled(a, label, ambient) {
                        return Some(f);
                    }
                }
                None
            }
            WitnessKind::Handle {
                effect,
                body,
                handler,
                ..
            } => {
                ambient.insert(effect.clone());
                let found = self
                    .find_unhandled(body, label, ambient)
                    .or_else(|| self.find_unhandled(handler, label, ambient));
                ambient.remove(effect);
                found
            }
            WitnessKind::Call { callee, args } => {
                for a in args {
                    if let Some(f) = self.find_unhandled(a, label, ambient) {
                        return Some(f);
                    }
                }
                if let WitnessCallee::Var(name) | WitnessCallee::Name(name) = callee
                    && let Some(row) = self
                        .fn_effect_rows
                        .get(name)
                        .or_else(|| self.rel_effect_rows.get(name))
                    && row.labels().contains(&label)
                    && !ambient.contains(label)
                {
                    return Some((w.span, Some(name.clone())));
                }
                None
            }
            WitnessKind::Closure { .. }
            | WitnessKind::FnDef { .. }
            | WitnessKind::UpdateDef { .. } => None,
            _ => {
                for c in w.child_witnesses() {
                    if let Some(f) = self.find_unhandled(c, label, ambient) {
                        return Some(f);
                    }
                }
                None
            }
        }
    }

    /// v0.80: 推断表达式类型 + effect row。
    ///
    /// 每个表达式产生 `(Type, EffectRow)` — 类型 + 该表达式可能产生的
    /// side effect 集合。EffectRow::Empty 表示纯表达式。
    pub fn infer_expr(
        &mut self,
        expr: &MirWitness,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let result = match &expr.kind {
            WitnessKind::Literal(lit) => Ok((infer_lit(lit), crate::mir::effect::EffectRow::Empty)),
            WitnessKind::Variable(name) => {
                let ty = self.infer_var(name, expr.span)?;
                Ok((ty, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::Binary { left, op, right } => {
                self.infer_binop(op, left.as_ref(), right.as_ref(), expr.span)
            }
            WitnessKind::Call { callee, args } => self.infer_call(callee, args, expr.span),
            // ── v0.102: 声明式范式（逻辑式/关系式）──
            WitnessKind::RelDef {
                name,
                clauses,
                clause_wits,
                ..
            } => self.infer_rel_def(name, clauses, clause_wits, expr.span),
            // v0.103: 命名 section 声明 —— body 求值并把 section 名登记到
            // env（运行时 h_prompt_section/h_document_section 把同名值绑定
            // 进环境，typeck 侧必须一致，否则 `compose_prompt("name")` 的
            // 引用会被判 Unbound variable）。定义点是纯的（值绑定非 effect），
            // body 的效果行传播。
            WitnessKind::PromptSection { name, body } => {
                let (_text_ty, body_row) = self.infer_expr(body)?;
                self.env.add(name.clone(), Type::PromptSection);
                Ok((Type::Nil, body_row))
            }
            WitnessKind::DocumentSection { name, body } => {
                let (_text_ty, body_row) = self.infer_expr(body)?;
                self.env.add(name.clone(), Type::Document);
                Ok((Type::Nil, body_row))
            }
            // v0.103: 可观测性块 —— 包一层 body，效果行传播（span 不改类型）
            WitnessKind::Observe { config: _, body }
            | WitnessKind::Span {
                name: _,
                tags: _,
                body,
            }
            | WitnessKind::Parallel { body } => {
                let (_ty, body_row) = self.infer_expr(body)?;
                Ok((Type::Nil, body_row))
            }
            // v0.103: export 内部声明 —— 类型/效果与不加 export 时相同
            // （export 只影响跨模块可见性，属 import 收集侧的静态判定）。
            WitnessKind::Export { decl, .. } => self.infer_expr(decl),
            WitnessKind::Solve {
                limit: _,
                query_vars,
                anon_vars,
                goal,
            } => {
                // 查询变量注册新作用域（?x → fresh TypeVar），目标构建体
                // 推断后其类型必须收敛为 Goal；投影类型 = 查询变量类型
                // 按序组成（0 个 → nil，1 个 → 值，n ≥ 2 → 元组）。
                let saved = self.env.clone();
                let mut var_tys: Vec<Type> = Vec::new();
                for q in query_vars {
                    let tv = self.fresh_type_var();
                    self.env.add(q.clone(), tv.clone());
                    var_tys.push(tv);
                }
                // 匿名变量入作用域但不投影
                for a in anon_vars {
                    let tv = self.fresh_type_var();
                    self.env.add(a.clone(), tv);
                }
                let outcome = self.infer_expr(goal).map(|(gty, row)| {
                    self.constraints
                        .push(Constraint::Eq(Box::new(gty), Box::new(Type::Goal)));
                    let elem = match var_tys.len() {
                        0 => Type::Nil,
                        1 => var_tys[0].clone(),
                        _ => Type::Tuple(var_tys.iter().map(|t| Box::new(t.clone())).collect()),
                    };
                    (Type::List(Box::new(elem)), row)
                });
                self.env = saved;
                outcome
            }
            WitnessKind::MethodCall {
                receiver,
                method,
                args,
            } => self.infer_method_call(receiver, method, args, expr.span),
            WitnessKind::Closure { params, body, .. } => {
                self.infer_closure(params, body.as_ref(), expr.span)
            }
            WitnessKind::FnDef {
                name, params, body, ..
            } => self.infer_fn_def(Some(name.as_str()), params, body.as_ref(), expr.span),
            WitnessKind::Match { scrutinee, arms } => {
                self.infer_match(scrutinee.as_ref(), arms, expr.span)
            }
            WitnessKind::If { cond, then, r#else } => {
                self.infer_if(cond.as_ref(), then.as_ref(), r#else.as_deref(), expr.span)
            }
            WitnessKind::List(items) => self.infer_list(items, expr.span),
            WitnessKind::Dict(entries) => self.infer_dict(entries, expr.span),
            WitnessKind::DynTrait { expr, .. } => {
                // v0.55: dyn Trait is opaque; defer to inner expression.
                self.infer_expr(expr)
            }
            WitnessKind::Prompt { parts } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for p in parts {
                    let (_, r) = self.infer_expr(p)?;
                    row = self.merge_rows(row, r);
                }
                Ok((Type::String, row))
            }
            WitnessKind::LetBinding {
                name,
                type_hint,
                value,
                ..
            } => match type_hint {
                Some(hint) => self.infer_let_typed(name, hint, value.as_ref(), expr.span),
                None => self.infer_let(name, value.as_ref(), expr.span),
            },
            WitnessKind::Assign { target, value } => {
                self.infer_assign(target, value.as_ref(), expr.span)
            }
            WitnessKind::Orchestrate {
                input_var,
                result_var,
                ..
            } => {
                // v0.75.34: orchestrate 在语义上声明 input_var / result_var
                //（`orchestrate ... input -> result`）— 登记为 Any 类型，
                // 避免后续引用 result 报 UnboundVariable。此前返回 Nil 但
                // 不登记变量，pregel/sequential 路径经 CLI 都会撞此缺口
                //（测试走 run_mir 绕过 typeck 未暴露）。
                self.env.add(input_var.clone(), Type::Unknown);
                self.env.add(result_var.clone(), Type::Unknown);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.104: `for x in xs … end` 的真实推断 —— 之前是 v0.55 的桩
            // `Ok((Type::Nil, Empty))`，**完全不推断 iterable 与 body**，
            // 于是循环体内的一切错误被静默吞掉：
            //   for i in [1,2,3] / print(nosuchvar) end   -- 不报 Unbound，
            //                                             运行期得 nil
            //   for i in [1,2,3] / perform Ai("x") end    -- 效果行不进残差，
            //                                             根边界断言漏检
            //   for x in 5i …                             -- iterable 非列表
            //                                             到运行期才 len() 报错
            // 现在：迭代变量按 iterable 的元素类型绑定（list<T> → T；
            // string → char；TypeVar/dict 宽容绑定），body 在子作用域推断，
            // 效果行并入，循环整体为 Nil。
            WitnessKind::Loop {
                var,
                iterable,
                body,
            } => {
                let (iter_ty, iter_row) = self.infer_expr(iterable)?;
                let elem_ty = match &iter_ty {
                    Type::List(elem) => elem.as_ref().clone(),
                    Type::String => Type::Char,
                    // TypeVar（待推断）/ Any / Unknown / dict → 宽容：元素
                    // 类型取 fresh 变量，让 body 内的用法自然约束它。
                    _ => self.fresh_type_var(),
                };
                let saved_env = self.env.clone();
                self.env.add(var.clone(), elem_ty);
                let (_, body_row) = self.infer_expr(body)?;
                self.env = saved_env;
                let row = self.merge_rows(iter_row, body_row);
                Ok((Type::Nil, row))
            }
            // v0.104: `while cond … end` 的真实推断 —— 同上，桩改为：
            // 条件必须是 Bool（与 `if` 同一契约，此前 `while 1i` 静默放行），
            // body 在子作用域推断，效果行并入，循环整体为 Nil。
            WitnessKind::While { cond, body } => {
                let (cond_ty, cond_row) = self.infer_expr(cond)?;
                if !matches!(cond_ty, Type::Bool)
                    && !matches!(cond_ty, Type::TypeVar(_) | Type::Any | Type::Unknown)
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "bool".to_string(),
                        got: cond_ty.name().to_string(),
                        span: Some(cond.span),
                    }]);
                }
                let saved_env = self.env.clone();
                let (_, body_row) = self.infer_expr(body)?;
                self.env = saved_env;
                let row = self.merge_rows(cond_row, body_row);
                Ok((Type::Nil, row))
            }
            WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
                let (left_ty, left_row) = self.infer_expr(left)?;
                let (right_ty, right_row) = self.infer_expr(right)?;
                let merged = self.merge_rows(left_row, right_row);
                if !matches!(left_ty, Type::Bool) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "bool".to_string(),
                        got: left_ty.name(),
                        span: Some(expr.span),
                    }]);
                }
                if !matches!(right_ty, Type::Bool) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "bool".to_string(),
                        got: right_ty.name(),
                        span: Some(expr.span),
                    }]);
                }
                Ok((Type::Bool, merged))
            }
            // v0.103 修复：`return <expr>` 的类型 = 被返回表达式的类型。
            //
            // 此前一律返回 `Nil` —— 后果是**任何使用显式 `return` 的函数**
            // 推断出的 Arrow 返回类型都是 Nil。常规调用点因 FnDef 名不入 env
            // （`infer_call` 对未知名产出 fresh TypeVar）而掩盖了该错误；
            // 一旦函数类型被**物化**使用（跨模块 import 的精确签名），
            // 立刻显形（import 后调用导出 task 报 "expected nil, got string"）。
            // `return` 无值时仍为 Nil。
            WitnessKind::Return(Some(v)) => self.infer_expr(v),
            WitnessKind::Return(None) | WitnessKind::Break(_) | WitnessKind::Continue(_) => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::IndexAssign { .. } => {
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.55: top-level declarations — no scalar result type.
            //
            // v0.103: AppDef 从本组移出（见下方独立分支）—— 它必须在 env 中
            // 注册 app 名（运行时 h_app_def 会注册 Value::TeaApp，typeck 此前
            // 不注册 → `app Counter ... end` 后的 `Counter` 被判 Unbound
            // variable），并推断真实的 update/view 体。
            WitnessKind::TypeAlias { .. }
            | WitnessKind::EnumDef { .. }
            | WitnessKind::StructDef { .. }
            | WitnessKind::Import(_)
            | WitnessKind::MacroDef { .. } => Ok((Type::Nil, crate::mir::effect::EffectRow::Empty)),
            // v0.103: TEA 独立 `update(params) ... end` 声明（spec §9.6）——
            // 注册 `update` 名到 env（运行时 h_update_def 注册同名
            // `Value::Dict`），并推断真实的体。此前它在上面那组「纯声明」里
            // 只返回 Nil、不注册任何名 → 同一文件里 `app ... update: update`
            // 或 `print(type_of(update))` 都报 Unbound variable。
            WitnessKind::UpdateDef {
                name, params, body, ..
            } => {
                // 先用声明的参数类型构造函数类型；无注解的形参取 fresh
                // TypeVar（与调用点推断合一）。
                let param_tys: Vec<Type> = params
                    .iter()
                    .map(|p| match &p.type_hint {
                        Some(h) => h.to_type().clone(),
                        None => self.fresh_type_var(),
                    })
                    .collect();
                // 体在子作用域推断（形参入 env，退出时整体还原 —— 与
                // handle 的 __argN 注册同一 clone/restore 契约）。
                let saved_env = self.env.clone();
                for (p, pty) in params.iter().zip(param_tys.iter()) {
                    self.env.add(p.name.clone(), pty.clone());
                }
                let (body_ty, body_row) = self.infer_expr(body)?;
                self.env = saved_env;
                // 多参按 curried Arrow 逐层包裹 —— 与 infer_call 的消解
                // 方向一致（每个实参消耗一层）。体残差行挂最内层，供
                // 调用点传播 unhandled effect。
                let mut ty = body_ty;
                for pty in param_tys.into_iter().rev() {
                    ty = Type::Arrow(Box::new(pty), Box::new(ty), body_row.clone());
                }
                self.env.add(name.clone(), ty);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.103: App 定义 —— 注册 `app 名` 的 TeaApp 类型到 env（运行时
            // h_app_def 注册同名 Value::TeaApp，两侧必须一致），并推断
            // init/update/view 体（v0.103 前 emit 端丢弃 update/view witness，
            // 此处只能看到伪造占位 —— 修复后用户写的体参与推断与诊断）。
            WitnessKind::AppDef {
                name,
                model_name: _,
                msg_name: _,
                init_w,
                update_w,
                view_w,
            } => {
                // init 在定义点执行（h_app_def 调 app.initialized）→ 其效果
                // 行传播到外层；update/view 是延迟闭包 → 行登记进
                // fn_effect_rows（与 FnDef 同规则），不并入定义点行。
                let (init_ty, init_row) = self.infer_expr(init_w)?;
                let (update_ty, _update_row) = self.infer_expr(update_w)?;
                let (view_ty, _view_row) = self.infer_expr(view_w)?;
                if let WitnessKind::Closure { body, .. } = &update_w.kind {
                    let mut ambient = std::collections::HashSet::new();
                    let r = Self::tree_effect_row(body, &mut ambient, &self.fn_effect_rows.clone());
                    self.fn_effect_rows.insert(format!("{}.update", name), r);
                }
                if let WitnessKind::Closure { body, .. } = &view_w.kind {
                    let mut ambient = std::collections::HashSet::new();
                    let r = Self::tree_effect_row(body, &mut ambient, &self.fn_effect_rows.clone());
                    self.fn_effect_rows.insert(format!("{}.view", name), r);
                }
                let msg_ty = self.fresh_type_var();
                self.env.add(
                    name.clone(),
                    Type::TeaApp {
                        name: name.clone(),
                        model: Box::new(init_ty),
                        msg: Box::new(msg_ty),
                        update: Box::new(update_ty),
                        view: Box::new(view_ty),
                    },
                );
                Ok((Type::Nil, init_row))
            }
            // v0.84: Sequence 推断 — 依次推断子表达式，合并 effect row，
            // 返回最后一个表达式的类型（do-notation 语义）。空 Sequence → Nil。
            WitnessKind::Sequence(exprs) => self.infer_sequence(exprs, expr.span),
            // v0.85: with 块 — 推断 bindings（声明/丢弃），body 的 effect row
            // 直接传播（配置桥接不产生 effect，但 body 可能产生）。
            WitnessKind::WithConfig { bindings, body } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for (_, v) in bindings {
                    let (_, r) = self.infer_expr(v)?;
                    row = self.merge_rows(row, r);
                }
                let (body_ty, body_row) = self.infer_expr(body)?;
                let merged = self.merge_rows(row, body_row);
                Ok((body_ty, merged))
            }
            // v0.83: TEA definitions — 注册 Type 到 env（typeck 路径可查）
            WitnessKind::ModelDef { name, fields } => {
                use crate::typeck::Type;
                let ty = Type::TeaModel {
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .map(|(n, t)| (n.clone(), Box::new(t.clone().into_type())))
                        .collect(),
                };
                self.env.add(name.clone(), ty);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            WitnessKind::MsgDef { name, variants } => {
                use crate::typeck::Type;
                let ty = Type::TeaMsg {
                    name: name.clone(),
                    variants: variants
                        .iter()
                        .map(|v| {
                            let payload = v.payload_type.as_ref().map(|t| {
                                Box::new(Type::Concrete {
                                    name: t.clone(),
                                    generics: vec![],
                                    traits: vec![],
                                })
                            });
                            (v.name.clone(), payload)
                        })
                        .collect(),
                };
                self.env.add(name.clone(), ty);
                Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
            }
            // v0.80: Perform — 产生 effect，返回 fresh type var（由 handler 决定具体类型）。
            WitnessKind::Perform { effect, args } => self.infer_perform(effect, args, expr.span),
            // v0.80: Handle — 捕获 effect，body 的 effect row 中移除被捕获的 effect。
            WitnessKind::Handle {
                effect,
                body,
                handler,
                ..
            } => self.infer_handle(effect, body.as_ref(), handler.as_ref(), expr.span),
            // v0.88: Quasiquote — 与 quote 对称，返回 String 类型（源码字符串）。
            // 推断各子表达式（Unquote/UnquoteSplice），合并 effect row；
            // Quote 段为静态常量不产生 effect。
            WitnessKind::Quasiquote { segments } => {
                let mut row = crate::mir::effect::EffectRow::Empty;
                for seg in segments {
                    let (_, r) = self.infer_expr(seg)?;
                    row = self.merge_rows(row, r);
                }
                Ok((Type::String, row))
            }
            // v0.98: effect 签名声明 —— 纯类型层，无类型/效果贡献。
            //（签名本身已在 infer_program 预扫描注册进 effect_signatures。）
            WitnessKind::EffectSig { .. } => Ok((Type::Nil, crate::mir::effect::EffectRow::Empty)),
        };
        // Shadow capture: store pre-substitution (Type, EffectRow) per witness node.
        // Keyed by Span (unique within a file). Used by export_type_table.
        if let Ok(ref pair) = result {
            self.shadow_types.insert(expr.span, pair.clone());
        }
        result
    }

    /// v0.80: 合并两个 effect row（用于二元运算、if 分支等）。
    ///
    /// 规则：
    /// - Empty + x = x（恒等元）
    /// - Cons(h, t) + x = Cons(h, merge(t, x))（若 h 不在 x 中）
    /// - Var(v) + x = x + Constraint::RowEq(Var(v), x)（推迟到 solve 阶段）
    pub(crate) fn merge_rows(
        &mut self,
        a: crate::mir::effect::EffectRow,
        b: crate::mir::effect::EffectRow,
    ) -> crate::mir::effect::EffectRow {
        use crate::mir::effect::EffectRow;
        match (a, b) {
            (EffectRow::Empty, b) => b,
            (a, EffectRow::Empty) => a,
            (EffectRow::Cons(h, t), b) => {
                if row_contains_concrete(&b, &h) {
                    self.merge_rows(*t, b)
                } else {
                    EffectRow::Cons(h, Box::new(self.merge_rows(*t, b)))
                }
            }
            (EffectRow::Var(v), b) => {
                self.constraints
                    .push(Constraint::RowEq(EffectRow::Var(v), b.clone()));
                b
            }
        }
    }

    /// v0.80: Perform 推断 — 产生 effect，返回 fresh type var。
    ///
    /// v0.98: 签名生效 —— effect 已声明签名时：
    /// - 实参数/类型必须符合契约（ArityMismatch / 逐参 compatible_with）；
    /// - 结果类型取自签名（不再 fresh var）—— fn/task 体内的高阶位点
    ///   由此静态化，无需调用点的 handle 在场。
    fn infer_perform(
        &mut self,
        effect: &str,
        args: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let mut arg_rows = crate::mir::effect::EffectRow::Empty;
        let mut arg_tys = Vec::with_capacity(args.len());
        for arg in args {
            let (t, row) = self.infer_expr(arg)?;
            arg_tys.push(t.clone());
            arg_rows = self.merge_rows(arg_rows, row);
        }
        let sig = self.effect_signatures.get(effect).cloned();
        if let Some(sig) = &sig {
            if sig.params.len() != args.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: sig.params.len(),
                    actual: args.len(),
                    span,
                }]);
            }
            for (i, (a_ty, p_ty)) in arg_tys.iter().zip(sig.params.iter()).enumerate() {
                if !a_ty.compatible_with(p_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("perform `{}` arg {} : {:?}", effect, i, p_ty),
                        got: format!("{:?}", a_ty),
                        span: Some(span),
                    }]);
                }
            }
        }
        let result_ty = match sig {
            Some(s) => s.result,
            None => self.fresh_type_var(),
        };
        // v0.97: 位点记录到 handle 帧顶（栈顶 = 运行时将接管它的 handler）。
        // 闭包/fn 体内不记录 —— 调用点上下文未知，高阶连接由签名覆盖。
        if self.closure_depth == 0
            && let Some(frame) = self.handle_stack.last_mut()
            && frame.effect == effect
        {
            frame.sites.push(PerformSite {
                result_ty: result_ty.clone(),
                arg_tys,
            });
        }
        let perform_row = crate::mir::effect::EffectRow::Cons(
            effect.to_string(),
            Box::new(crate::mir::effect::EffectRow::Empty),
        );
        Ok((result_ty, self.merge_rows(arg_rows, perform_row)))
    }

    /// v0.80: Handle 推断 — 捕获 effect，body 的 effect row 中移除被捕获的 effect。
    ///
    /// handler 体内的 `__arg0`, `__arg1`, ... 是 perform 传参的运行时约定
    /// （见 `src/runtime/effect.rs`）。类型检查时在 handler 作用域内注册
    /// 这些变量，让 handler body 的引用通过 typeck。
    ///
    /// v0.96: 吸收语义改为**直接行差**（[`EffectRow::remove`]）—— 残差 =
    /// body 行去掉全部被捕获标签。此前用严格等式约束
    /// `RowEq(body_row, Cons(effect, residual))`，当 body 不 perform 被
    /// 捕获效果时（`handle X { 纯 body }`，定义了未触发的 handler ——
    /// 合法程序），Empty-vs-Cons 的 unify_row 分支误报 "pure vs {X}"。
    /// 未知行（Var，来自未注册 callee 的调用）保留约束推迟消解。
    ///
    /// v0.97: perform↔handler 静态连接 ——
    /// - `__arg0..__argN`（N = 位点最大实参数）按位点实参的推断类型注册，
    ///   不再一律 Any；位点异质时退化为 Any（不误拒，精度优雅降级）；
    /// - 每个 perform 位点的结果类型（fresh var）与 handler 返回类型
    ///   Eq 统一 —— runtime 契约「handler 返回值 = resume 值」由此静态化。
    ///
    /// 位点经 **handle 帧栈**原位收集（infer_perform 记录到帧顶）：推断
    /// Perform 时栈顶恰好是运行时将接管它的 handler —— 内层同标签 handle
    /// 的 body 归内层、内层 handler 体归外层，归属天然正确且零 span 依赖。
    /// 闭包/fn 体内不下记录（调用点上下文未知 —— 高阶位点的连接留给
    /// 后续 effect signature 声明）。
    fn infer_handle(
        &mut self,
        effect: &str,
        body: &MirWitness,
        handler: &MirWitness,
        _span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        self.handle_stack.push(HandleFrame {
            effect: effect.to_string(),
            sites: Vec::new(),
        });
        let (body_ty, body_row) = self.infer_expr(body)?;
        let frame = self.handle_stack.pop().expect("handle frame pushed above");
        let sites = frame.sites;
        let residual_row = match &body_row {
            crate::mir::effect::EffectRow::Var(_) => {
                let residual = self.fresh_row_var();
                self.constraints.push(Constraint::RowEq(
                    body_row.clone(),
                    crate::mir::effect::EffectRow::Cons(
                        effect.to_string(),
                        Box::new(residual.clone()),
                    ),
                ));
                residual
            }
            _ => body_row.remove(effect),
        };
        // __arg0..__argN 注册：v0.98 签名优先（声明的参数契约，覆盖位点
        // 推导 —— 位点已对签名校验）；无签名回退 v0.97 位点推导。
        let sig = self.effect_signatures.get(effect).cloned();
        let saved_env = self.env.clone();
        let max_arity = match &sig {
            Some(s) => s.params.len().max(1),
            None => sites
                .iter()
                .map(|s| s.arg_tys.len())
                .max()
                .unwrap_or(1)
                .max(1),
        };
        for i in 0..max_arity {
            let registered = if let Some(s) = &sig {
                s.params.get(i).cloned().unwrap_or(Type::Any)
            } else {
                let mut arg_ty: Option<Type> = None;
                let mut heterogeneous = false;
                for s in &sites {
                    if let Some(t) = s.arg_tys.get(i) {
                        match &arg_ty {
                            None => arg_ty = Some(t.clone()),
                            Some(prev) => {
                                if !prev.compatible_with(t) && !t.compatible_with(prev) {
                                    heterogeneous = true;
                                }
                            }
                        }
                    }
                }
                if heterogeneous {
                    Type::Any
                } else {
                    arg_ty.unwrap_or(Type::Any)
                }
            };
            self.env.add(format!("__arg{}", i), registered);
        }
        let (_handler_ty, handler_row) = self.infer_expr(handler)?;
        self.env = saved_env;
        // perform 结果 ≡ handler 返回值（resume 契约的静态化）。
        // v0.98: 有签名时 handler 返回必须兼容声明结果（直接检查，报错
        // 落在 handle span）；无签名走 v0.97 位点 Eq 统一。
        match &sig {
            Some(s) => {
                if !_handler_ty.compatible_with(&s.result) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("handler for `{}` returns {:?}", effect, s.result),
                        got: format!("{:?}", _handler_ty),
                        span: Some(_span),
                    }]);
                }
            }
            None => {
                for s in &sites {
                    self.constraints.push(Constraint::Eq(
                        Box::new(s.result_ty.clone()),
                        Box::new(_handler_ty.clone()),
                    ));
                }
            }
        }
        Ok((body_ty, self.merge_rows(residual_row, handler_row)))
    }
}

/// v0.80: 检查 effect row 是否包含具体 label（不对 Var 返回 true，
/// 与 EffectRow::contains 的多态语义不同）。
fn row_contains_concrete(row: &crate::mir::effect::EffectRow, label: &str) -> bool {
    match row {
        crate::mir::effect::EffectRow::Empty => false,
        crate::mir::effect::EffectRow::Var(_) => false,
        crate::mir::effect::EffectRow::Cons(h, t) => h == label || row_contains_concrete(t, label),
    }
}

fn infer_lit(lit: &crate::common::Literal) -> Type {
    use crate::common::Literal;
    match lit {
        Literal::Int(_, _) => Type::Int,
        Literal::Float(_, _) => Type::Float,
        Literal::BigInt(_, _) => Type::BigInt,
        Literal::String(_, _) => Type::String,
        Literal::Char(_, _) => Type::Char,
        Literal::Bool(_, _) => Type::Bool,
        Literal::Nil(_) => Type::Nil,
    }
}

// ══════════ v0.102: 声明式范式（逻辑式/关系式）typeck ══════════

impl HMInference {
    /// v0.102: 关系签名/效果行不动点预计算。
    ///
    /// 纯数据行走器（不污染主推断状态）：从子句**头字面量**精化位置签名
    /// 格，从子句**体的关系调用**传播签名约束（`rel path(x,y) edge(x,y) end`
    /// 的 x/y 经 edge 的签名获得 string），project 并入被投函数行。
    /// 签名格单调（Unknown/TypeVar → 具体类型），轮数有界 8，保证收敛。
    pub fn precompute_rel_sigs(&mut self, exprs: &[MirWitness]) {
        fn collect_rel_defs<'a>(w: &'a MirWitness, out: &mut Vec<&'a MirWitness>) {
            if let WitnessKind::RelDef { .. } = &w.kind {
                out.push(w);
            }
            for c in w.child_witnesses() {
                collect_rel_defs(c, out);
            }
        }
        let mut defs: Vec<&MirWitness> = Vec::new();
        for e in exprs {
            collect_rel_defs(e, &mut defs);
        }
        if defs.is_empty() {
            return;
        }
        let mut sigs: HashMap<String, Vec<Type>> = self.rel_sigs.clone();
        let mut rows: HashMap<String, crate::mir::effect::EffectRow> = self.rel_effect_rows.clone();
        for _round in 0..8u32 {
            let mut changed = false;
            for def in &defs {
                if let WitnessKind::RelDef { name, clauses, .. } = &def.kind {
                    let (sig, row) =
                        Self::rel_clause_sigs(name, clauses, &sigs, &rows, &self.fn_effect_rows);
                    match sigs.get(name) {
                        Some(prev) if prev == &sig => {}
                        _ => changed = true,
                    }
                    let merged_row = match rows.get(name) {
                        Some(prev) => Self::union_effect_rows(prev, &row),
                        None => row.clone(),
                    };
                    if rows.get(name) != Some(&merged_row) {
                        changed = true;
                    }
                    sigs.insert(name.clone(), sig);
                    rows.insert(name.clone(), merged_row);
                }
            }
            if !changed {
                break;
            }
        }
        self.rel_sigs = sigs;
        self.rel_effect_rows = rows;
    }

    /// 精化格（单调向上）。
    ///
    /// - `Unknown`/`TypeVar` 让位于具体类型（首次确定位置签名）；
    /// - 两个不同的具体类型 → 合并为 `Union`（Prolog 多模式子句的合法
    ///   情形：`appendo(nil, ys, ys)` 与 `appendo(cons(h,t), ...)` 是同一
    ///   关系在不同模式下的两条子句，位置签名是 Nil ∪ Cons）；
    /// - 已是 Union 则追加新成员（去重）。
    ///
    /// 调用点实参与 union 合一任一成员即通过（`hm::unify` 的 Union 分支）。
    fn refine_cell(cell: &mut Type, new: &Type) {
        if &*cell == new {
            return;
        }
        let cell_is_open = matches!(cell, Type::Unknown | Type::TypeVar(_));
        let new_is_open = matches!(new, Type::Unknown | Type::TypeVar(_));
        if cell_is_open && !new_is_open {
            *cell = new.clone();
            return;
        }
        if cell_is_open || new_is_open {
            // 都还是未定 → 保持（等后续轮次精化）
            return;
        }
        // 两个具体类型：并入 union（展平 + 去重）
        let mut members: Vec<Type> = match &*cell {
            Type::Union(existing) => existing.clone(),
            other => vec![other.clone()],
        };
        let to_add: Vec<Type> = match new {
            Type::Union(extra) => extra.clone(),
            other => vec![other.clone()],
        };
        let mut changed = false;
        for t in to_add {
            if !members.contains(&t) {
                members.push(t);
                changed = true;
            }
        }
        if changed {
            *cell = Type::Union(members);
        }
    }

    /// 子句项模板 → 静态类型。模板内的逻辑变量位置（`Param`）用 `Any`
    /// 占位：`Any` 是 top type（与任意类型合一成功）且可判等，因此签名格
    /// 在不动点迭代中稳定（`TypeVar` 每轮 fresh，会破坏 union 去重）。
    fn term_static_ty(t: &crate::rel::Term) -> Type {
        use crate::rel::Term;
        match t {
            Term::Param(_) => Type::Any,
            Term::Val(v) => Self::ground_value_ty(v).unwrap_or(Type::Any),
            Term::Cons(a, b) => Type::Cons(
                Box::new(Self::term_static_ty(a)),
                Box::new(Self::term_static_ty(b)),
            ),
            Term::List(xs) => {
                let elem = xs.first().map(Self::term_static_ty).unwrap_or(Type::Any);
                Type::List(Box::new(elem))
            }
            Term::Dict(entries) => Type::Dict(
                Box::new(Type::String),
                Box::new(
                    entries
                        .first()
                        .map(|(_, v)| Self::term_static_ty(v))
                        .unwrap_or(Type::Any),
                ),
            ),
        }
    }

    /// ground 字面量值 → 静态类型。
    fn ground_value_ty(v: &crate::value::Value) -> Option<Type> {
        Some(match v {
            crate::value::Value::String(_) => Type::String,
            crate::value::Value::Char(_) => Type::Char,
            crate::value::Value::Int(_) => Type::Int,
            crate::value::Value::Float(_) => Type::Float,
            crate::value::Value::BigInt(_) => Type::BigInt,
            crate::value::Value::Bool(_) => Type::Bool,
            crate::value::Value::Nil => Type::Nil,
            crate::value::Value::List(items) => {
                let elem = items
                    .first()
                    .and_then(Self::ground_value_ty)
                    .unwrap_or(Type::Unknown);
                Type::List(Box::new(elem))
            }
            crate::value::Value::Cons { car, cdr } => Type::Cons(
                Box::new(Self::ground_value_ty(car).unwrap_or(Type::Unknown)),
                Box::new(Self::ground_value_ty(cdr).unwrap_or(Type::Unknown)),
            ),
            _ => return None,
        })
    }

    /// 单个关系的签名格 + 效果行（全部子句累积）。
    fn rel_clause_sigs(
        name: &str,
        clauses: &[crate::rel::Clause],
        sigs: &HashMap<String, Vec<Type>>,
        rows: &HashMap<String, crate::mir::effect::EffectRow>,
        fn_rows: &HashMap<String, crate::mir::effect::EffectRow>,
    ) -> (Vec<Type>, crate::mir::effect::EffectRow) {
        let mut row = crate::mir::effect::EffectRow::Empty;
        let mut cells: Vec<Type> = match sigs.get(name) {
            Some(s) => s.clone(),
            None => vec![Type::Unknown; clauses.first().map(|c| c.head.len()).unwrap_or(0)],
        };
        for clause in clauses {
            // 头项精化（字面量 / cons / 列表模板 → 静态类型）
            for (i, t) in clause.head.iter().enumerate() {
                if let Some(cell) = cells.get_mut(i) {
                    Self::refine_cell(cell, &Self::term_static_ty(t));
                }
            }
            Self::rel_body_walk(
                &clause.body,
                name,
                &mut cells,
                sigs,
                rows,
                fn_rows,
                &mut row,
            );
        }
        (cells, row)
    }

    /// 子句体目标走（纯数据）：关系调用把 Param(i) 格向签名精化；
    /// unify(Param, ground) 同向精化；project 并入被投函数行。
    fn rel_body_walk(
        g: &crate::rel::Goal,
        self_name: &str,
        cells: &mut Vec<Type>,
        sigs: &HashMap<String, Vec<Type>>,
        rows: &HashMap<String, crate::mir::effect::EffectRow>,
        fn_rows: &HashMap<String, crate::mir::effect::EffectRow>,
        row: &mut crate::mir::effect::EffectRow,
    ) {
        use crate::rel::Goal as G;
        match g {
            G::Conj(gs) | G::Disj(gs) => {
                for gi in gs {
                    Self::rel_body_walk(gi, self_name, cells, sigs, rows, fn_rows, row);
                }
            }
            G::Invoke { name, args, .. } => {
                if let Some(r) = rows.get(name) {
                    *row = Self::union_effect_rows(row, r);
                }
                let sig = if name == self_name {
                    sigs.get(self_name)
                } else {
                    sigs.get(name)
                };
                if let Some(sig) = sig {
                    Self::refine_from_terms(args, sig, cells);
                }
            }
            G::Project { func, .. } => {
                if let crate::rel::ProjectFn::Name(f) = func
                    && let Some(r) = fn_rows.get(f)
                {
                    *row = Self::union_effect_rows(row, r);
                }
            }
            G::Unify(a, b) => {
                Self::refine_unify_pair(a, b, cells);
                Self::refine_unify_pair(b, a, cells);
            }
            _ => {}
        }
    }

    /// unify(Param(i), ground) → 精化位置 i 的格（单向：literal 侧为 ground）。
    fn refine_unify_pair(a: &crate::rel::Term, b: &crate::rel::Term, cells: &mut [Type]) {
        // unify(Param(i), <任何具体项>) → 精化位置 i；模板内变量位置为 Any，
        // 不参与精化（Any 会让 refine_cell 的 open/fixed 判定保持格不变）。
        if let crate::rel::Term::Param(i) = a {
            let ty = Self::term_static_ty(b);
            if !matches!(ty, Type::Any)
                && let Some(cell) = cells.get_mut(*i)
            {
                Self::refine_cell(cell, &ty);
            }
        }
    }

    /// 调用实参（子句体项）与签名的格精化：Param(i) 位置吸收签名类型。
    fn refine_from_terms(args: &[crate::rel::Term], sig: &[Type], cells: &mut [Type]) {
        for (i, t) in args.iter().enumerate() {
            if let Some(st) = sig.get(i)
                && let crate::rel::Term::Param(p) = t
                && let Some(cell) = cells.get_mut(*p)
            {
                Self::refine_cell(cell, st);
            }
        }
    }

    /// v0.102: 主推断路径的关系定义检查。
    ///
    /// 预计算已产出签名/行；本臂做**错误诊断**：子句参数入作用域、头项
    /// 与签名 Eq 约束、体必须收敛为 Goal。定义本身纯（Nil, Empty）。
    fn infer_rel_def(
        &mut self,
        name: &str,
        clauses: &[crate::rel::Clause],
        clause_wits: &[crate::mir::witness::RelClauseWit],
        _span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let known_sig = self.rel_sigs.get(name).cloned();
        let mut row = crate::mir::effect::EffectRow::Empty;
        for (clause, cw) in clauses.iter().zip(clause_wits.iter()) {
            let saved = self.env.clone();
            // 槽位类型表：先按头项结构把签名类型映射到 Param 槽位，
            // 未映射的槽位（体独有变量 / 匿名变量）取 fresh TypeVar。
            let mut slot_tys: Vec<Type> = clause
                .params
                .iter()
                .map(|_| self.fresh_type_var())
                .collect();
            if let Some(sig) = &known_sig {
                for (j, ht) in clause.head.iter().enumerate() {
                    if let (crate::rel::Term::Param(i), Some(st)) = (ht, sig.get(j))
                        && let Some(cell) = slot_tys.get_mut(*i)
                    {
                        *cell = st.clone();
                    }
                }
            }
            for (i, pname) in clause.params.iter().enumerate() {
                let t = slot_tys.get(i).cloned().unwrap_or(Type::Unknown);
                self.env.add(pname.clone(), t);
            }
            for (i, hw) in cw.head.iter().enumerate() {
                let (hty, hrow) = self.infer_expr(hw)?;
                row = self.merge_rows(row, hrow);
                if let Some(st) = known_sig.as_ref().and_then(|s| s.get(i)) {
                    self.constraints
                        .push(Constraint::Eq(Box::new(hty), Box::new(st.clone())));
                }
            }
            let (bty, brow) = self.infer_expr(&cw.body)?;
            self.constraints
                .push(Constraint::Eq(Box::new(bty), Box::new(Type::Goal)));
            row = self.merge_rows(row, brow);
            self.env = saved;
        }
        let merged = match self.rel_effect_rows.get(name) {
            Some(prev) => self.merge_rows(prev.clone(), row.clone()),
            None => row.clone(),
        };
        self.rel_effect_rows.insert(name.to_string(), merged);
        Ok((Type::Nil, crate::mir::effect::EffectRow::Empty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Literal, Span};
    use crate::mir::witness::{
        MirWitness, WitnessArm, WitnessCallee, WitnessKind, WitnessParam, WitnessPattern,
    };

    // ─── v0.96: 效果边界断言（编译期 unhandled effect 强制）───

    fn wit_perform(effect: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Perform {
                effect: effect.to_string(),
                args: vec![],
            },
            span: Span::new(7, 3),
        }
    }

    fn wit_handler() -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::String("handled".to_string(), Span::default())),
            span: Span::default(),
        }
    }

    fn wit_handle(effect: &str, body: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Handle {
                effect: effect.to_string(),
                body: Box::new(body),
                handler: Box::new(wit_handler()),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }
    }

    fn wit_fn_def(name: &str, body: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::FnDef {
                name: name.to_string(),
                params: vec![],
                return_type: None,
                body: Box::new(body),
            },
            span: Span::default(),
        }
    }

    fn wit_call_var(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var(name.to_string()),
                args: vec![],
            },
            span: Span::new(9, 1),
        }
    }

    #[test]
    fn program_boundary_rejects_unhandled_perform() {
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[wit_perform("Ai")]);
        assert_eq!(errors.len(), 1, "顶层未处理 perform 必须报错");
        let msg = errors[0].to_string();
        assert!(msg.contains("Ai"), "错误应指名效果标签: {}", msg);
        assert!(msg.contains("handle"), "错误应提示 handle 补救: {}", msg);
        // 定位到 perform 发起点（span 7:3）
        assert!(msg.contains("line 7"), "错误应定位 perform: {}", msg);
    }

    #[test]
    fn program_boundary_accepts_handled_perform() {
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[wit_handle("Ai", wit_perform("Ai"))]);
        assert!(
            errors.is_empty(),
            "handle 兜住的 perform 不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn program_boundary_rejects_mismatched_handle() {
        let mut hm = HMInference::new();
        // handle Fs 兜不住 perform Ai
        let errors = hm.infer_program(&[wit_handle("Fs", wit_perform("Ai"))]);
        assert_eq!(errors.len(), 1, "不匹配的 handle 必须报错");
        assert!(errors[0].to_string().contains("Ai"));
    }

    #[test]
    fn handle_with_pure_body_is_accepted() {
        let mut hm = HMInference::new();
        // v0.96 修复的误拒：定义了未触发的 handler（纯 body）是合法程序。
        // 此前 RowEq(Empty, Cons(Ai, residual)) 走 unify_row 的
        // Empty-vs-Cons 分支误报 "pure vs { Ai }"。
        let errors = hm.infer_program(&[wit_handle("Ai", lit_int(42))]);
        assert!(
            errors.is_empty(),
            "纯 body 的 handle 不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn task_effect_propagates_to_call_outside_handle() {
        // task doIt() = perform "Ai" …（witness 层是 FnDef）
        let def = wit_fn_def("doIt", wit_perform("Ai"));
        let call = wit_call_var("doIt");
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def, call]);
        assert_eq!(errors.len(), 1, "跨函数 perform 无 handle 必须报错");
        let msg = errors[0].to_string();
        assert!(msg.contains("Ai"), "错误应指名标签: {}", msg);
        assert!(msg.contains("doIt"), "错误应指出效果来自哪个调用: {}", msg);
    }

    #[test]
    fn task_effect_absorbed_by_matching_handle_at_call_site() {
        // 定义时 perform 合法 —— 调用点在 handle 内即可（非词法检查）。
        let def = wit_fn_def("doIt", wit_perform("Ai"));
        let call_under_handle = wit_handle("Ai", wit_call_var("doIt"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def, call_under_handle]);
        assert!(
            errors.is_empty(),
            "调用点被 handle 兜住不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn let_bound_closure_effect_propagates() {
        // let f = fn() = perform "Ai" …；f() —— env 里 Arrow 携带行
        let closure = MirWitness {
            kind: WitnessKind::Closure {
                params: vec![],
                body: Box::new(wit_perform("Ai")),
            },
            span: Span::default(),
        };
        let binding = MirWitness {
            kind: WitnessKind::LetBinding {
                name: "f".to_string(),
                type_hint: None,
                value: Box::new(closure),
                init_body: Box::new(MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(Span::default())),
                    span: Span::default(),
                }),
            },
            span: Span::default(),
        };
        let call = wit_call_var("f");
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[binding, call]);
        assert_eq!(errors.len(), 1, "let 绑定闭包的效果调用必须报错");
        assert!(errors[0].to_string().contains("Ai"));
    }

    #[test]
    fn nested_handles_absorb_own_effects() {
        // 内层 handle Fs 吸收 perform Fs；外层 handle Ai 吸收（残差已纯）。
        let inner = wit_handle("Fs", wit_perform("Fs"));
        let outer = wit_handle("Ai", inner);
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[outer]);
        assert!(errors.is_empty(), "嵌套 handle 各吸收各的: {:?}", errors);
    }

    // ─── v0.97: mutual recursion / 前向引用的效果传播 ───

    use crate::common::BinaryOp;

    fn wit_call(name: &str, span: Span) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var(name.to_string()),
                args: vec![],
            },
            span,
        }
    }

    fn wit_fn_named(name: &str, body: MirWitness) -> MirWitness {
        wit_fn_def(name, body)
    }

    #[test]
    fn mutual_recursion_effect_propagates() {
        // a 调 b、b perform X：不动点后 a 的行含 X —— a() 无 handle 报错。
        let def_a = wit_fn_named("a", wit_call("b", Span::new(2, 1)));
        let def_b = wit_fn_named("b", wit_perform("X"));
        let call = wit_call("a", Span::new(4, 1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def_a, def_b, call]);
        assert_eq!(
            errors.len(),
            1,
            "mutual recursion 的效果必须传播: {:?}",
            errors
        );
        let msg = errors[0].to_string();
        assert!(msg.contains("X"), "错误应指名标签: {}", msg);
    }

    #[test]
    fn forward_task_reference_inside_handle_clean() {
        // 前向引用：main 定义时 helper 尚未出现，调用点在 handle 内 → 合法。
        let def_main = wit_fn_named("main", wit_handle("X", wit_call("helper", Span::new(2, 3))));
        let def_helper = wit_fn_named("helper", wit_perform("X"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def_main, def_helper]);
        assert!(
            errors.is_empty(),
            "前向引用 + handle 兜住不应报错: {:?}",
            errors
        );
    }

    #[test]
    fn self_recursive_effect_propagates() {
        // a 递归调用自己且某分支 perform X → 不动点后 a 的行含 X。
        let def_a = wit_fn_named(
            "a",
            MirWitness {
                kind: WitnessKind::If {
                    cond: Box::new(MirWitness {
                        kind: WitnessKind::Literal(Literal::Bool(true, Span::default())),
                        span: Span::default(),
                    }),
                    then: Box::new(wit_call("a", Span::default())),
                    r#else: Some(Box::new(wit_perform("X"))),
                },
                span: Span::default(),
            },
        );
        let call = wit_call("a", Span::new(3, 1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[def_a, call]);
        assert_eq!(errors.len(), 1, "自递归效果必须传播: {:?}", errors);
        assert!(errors[0].to_string().contains("X"));
    }

    // ─── v0.97: perform ↔ handler 静态连接 ───

    fn wit_str(s: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::String(s.to_string(), Span::default())),
            span: Span::default(),
        }
    }

    fn wit_var(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::default(),
        }
    }

    fn wit_add(left: MirWitness, right: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Binary {
                left: Box::new(left),
                op: BinaryOp::Add,
                right: Box::new(right),
            },
            span: Span::default(),
        }
    }

    fn wit_perform_args(effect: &str, args: Vec<MirWitness>) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Perform {
                effect: effect.to_string(),
                args,
            },
            span: Span::default(),
        }
    }

    fn wit_let_typed(name: &str, hint: Type, value: MirWitness) -> MirWitness {
        MirWitness {
            kind: WitnessKind::LetBinding {
                name: name.to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(hint)),
                value: Box::new(value),
                init_body: Box::new(MirWitness {
                    kind: WitnessKind::Literal(Literal::Nil(Span::default())),
                    span: Span::default(),
                }),
            },
            span: Span::default(),
        }
    }

    #[test]
    fn perform_result_unifies_with_handler_return() {
        // handler 返回 String；perform 结果标注为 Int 使用 → 连接后
        // Eq(Int, fresh) + Eq(fresh, String) 消解冲突报错。无连接时
        // perform 是自由 fresh var，Int 标注静默通过（漏检）。
        let body = wit_let_typed("v", Type::Int, wit_perform_args("X", vec![wit_str("s")]));
        let program = [wit_handle("X", body)];
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&program);
        assert_eq!(
            errors.len(),
            1,
            "perform 结果应与 handler 返回类型统一: {:?}",
            errors
        );
    }

    #[test]
    fn handler_arg_typed_from_perform_site() {
        // 位点传 Int → __arg0 静态为 Int → handler 内 Int + Int 合法。
        let body = wit_perform_args("X", vec![lit_int(41)]);
        let handler = wit_add(wit_var("__arg0"), lit_int(1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert!(errors.is_empty(), "类型化 __arg0 应通过: {:?}", errors);
    }

    #[test]
    fn handler_arg_mismatch_reports() {
        // 位点传 String、handler 内将 __arg0 标注为 Int 使用 → 报错
        //（连接生效：__arg0 静态为 String 而非 Any）。
        let body = wit_perform_args("X", vec![wit_str("s")]);
        let handler = wit_let_typed("v", Type::Int, wit_var("__arg0"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert_eq!(errors.len(), 1, "__arg0 类型不匹配必须报错: {:?}", errors);
    }

    #[test]
    fn multi_arg_perform_registers_all_handler_params() {
        // 两参 perform → __arg0/__arg1 都注册（此前只注册 __arg0，
        // handler 引用 __arg1 误报 UnboundVariable）。
        let body = wit_perform_args("X", vec![lit_int(1), lit_int(2)]);
        let handler = wit_add(wit_var("__arg0"), wit_var("__arg1"));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert!(
            errors.is_empty(),
            "__arg1 必须被注册（运行时按 __arg0..N 注入）: {:?}",
            errors
        );
    }

    #[test]
    fn heterogeneous_sites_degrade_to_any() {
        // 两个位点 __arg0 分别 String / Int → 异质退化为 Any，
        // handler 内不做错误拒绝（精度优雅降级，不误拒）。
        let body = MirWitness {
            kind: WitnessKind::Sequence(vec![
                wit_perform_args("X", vec![wit_str("a")]),
                wit_perform_args("X", vec![lit_int(1)]),
            ]),
            span: Span::default(),
        };
        let handler = wit_add(wit_var("__arg0"), lit_int(1));
        let mut hm = HMInference::new();
        let errors = hm.infer_program(&[MirWitness {
            kind: WitnessKind::Handle {
                effect: "X".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        }]);
        assert!(
            errors.is_empty(),
            "异质位点退化为 Any 不应误拒: {:?}",
            errors
        );
    }

    // ─── v0.98: 显式 effect 签名声明（全管线：parse → witness → typeck）───

    fn typecheck_src(src: &str) -> Vec<TypeError> {
        let (_, witnesses) = crate::parser_v3::ParserV3::compile(src).expect("parse");
        HMInference::new().infer_program(&witnesses)
    }

    #[test]
    fn effect_signature_validates_perform_arity() {
        let errs = typecheck_src("effect Ask(string): string\nperform Ask(\"hi\", \"extra\")");
        assert_eq!(errs.len(), 1, "签名 arity 校验: {:?}", errs);
        assert!(
            errs[0].to_string().contains("Expected 1 arguments"),
            "ArityMismatch 应指向签名声明的参数数: {:?}",
            errs
        );
    }

    #[test]
    fn effect_signature_validates_perform_arg_type() {
        let errs = typecheck_src("effect Ask(string): string\nperform Ask(42)");
        assert_eq!(errs.len(), 1, "签名实参类型校验: {:?}", errs);
        assert!(
            errs[0].to_string().contains("Ask"),
            "错误应指名效果与参数: {:?}",
            errs
        );
    }

    #[test]
    fn effect_signature_static_result_in_task_body() {
        // 高阶位点的结果静态化：perform 在 task 体内、无 handle 在场，
        // 结果类型取自签名 → let 标注 Int 与签名 String 冲突报错。
        // （无签名时结果是自由 fresh var，此程序静默通过 = 漏检。）
        let errs = typecheck_src(
            "effect Ask(string): string\ntask doIt()\n  let v: number = perform Ask(\"hi\")\nend",
        );
        assert_eq!(errs.len(), 1, "签名结果静态连接: {:?}", errs);
    }

    #[test]
    fn effect_signature_static_result_positive() {
        let errs = typecheck_src(
            "effect Ask(string): string\ntask doIt()\n  let v: string = perform Ask(\"hi\")\nend",
        );
        assert!(errs.is_empty(), "结果类型匹配签名应通过: {:?}", errs);
    }

    #[test]
    fn effect_signature_handler_params_pass() {
        let errs = typecheck_src(
            "effect Ask(number): number\nlet r = handle Ask {\n  perform Ask(42)\n} {\n  let n: number = __arg0\n  n\n}",
        );
        assert!(errs.is_empty(), "__arg0 按签名参数类型化应通过: {:?}", errs);
    }

    #[test]
    fn effect_signature_handler_params_mismatch() {
        let errs = typecheck_src(
            "effect Ask(string): string\nlet r = handle Ask {\n  perform Ask(\"q\")\n} {\n  let n: number = __arg0\n  n\n}",
        );
        assert_eq!(errs.len(), 1, "__arg0 与签名参数类型不符应报错: {:?}", errs);
    }

    #[test]
    fn effect_signature_handler_return_mismatch() {
        // handler 返回 String、签名结果 number → 返回值必须兼容声明契约。
        let errs = typecheck_src(
            "effect Ask(string): number\nlet r = handle Ask {\n  perform Ask(\"q\")\n} {\n  \"text\"\n}",
        );
        assert_eq!(errs.len(), 1, "handler 返回失配签名结果应报错: {:?}", errs);
        assert!(
            errs[0].to_string().contains("handler"),
            "错误应指明是 handler 返回失配: {:?}",
            errs
        );
    }

    #[test]
    fn effect_signature_no_result_is_any() {
        // 缺省返回标注 = Any（仅载荷契约）：handler 返回任意值均合法。
        let errs = typecheck_src(
            "effect Log(string)\nlet r = handle Log {\n  perform Log(\"x\")\n} {\n  \"ok\"\n}",
        );
        assert!(
            errs.is_empty(),
            "无结果签名的 handler 返回不应受限: {:?}",
            errs
        );
    }

    #[test]
    fn duplicate_effect_signature_conflicts() {
        let errs = typecheck_src("effect Ask(string): string\neffect Ask(number): number");
        assert_eq!(errs.len(), 1, "签名不一致的重复声明应报错: {:?}", errs);
    }

    #[test]
    fn duplicate_effect_signature_identical_is_idempotent() {
        let errs = typecheck_src("effect Ask(string): string\neffect Ask(string): string");
        assert!(errs.is_empty(), "一致的重复声明应幂等放行: {:?}", errs);
    }

    pub(super) fn lit_int(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    #[test]
    pub(super) fn literal_int_infers_to_int() {
        let mut hm = HMInference::new();
        let (ty, row) = hm.infer_expr(&lit_int(7)).unwrap();
        assert_eq!(ty, Type::Int);
        assert_eq!(row, crate::mir::effect::EffectRow::Empty);
    }

    #[test]
    pub(super) fn unbound_variable_produces_diagnostic() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::Variable("missing".to_string()),
            span: Span::default(),
        };
        let err = hm.infer_expr(&expr).unwrap_err();
        assert!(matches!(
            err.as_slice(),
            [TypeError::UnboundVariable { .. }]
        ));
    }

    #[test]
    pub(super) fn let_binding_registers_env() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::LetBinding {
                name: "x".to_string(),
                type_hint: None,
                value: Box::new(lit_int(1)),
                init_body: Box::new(MirWitness {
                    kind: WitnessKind::Variable("x".to_string()),
                    span: Span::default(),
                }),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        assert_eq!(hm.env.get("x"), Some(&Type::Int));
    }

    #[test]
    pub(super) fn if_branches_unify() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::If {
                cond: Box::new(lit_int(1)),
                then: Box::new(lit_int(2)),
                r#else: Some(Box::new(lit_int(3))),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        let (_subst, errors) = hm.solve_constraints();
        assert!(errors.is_empty(), "if(int,int,int) should unify cleanly");
    }

    #[test]
    pub(super) fn match_arms_unify() {
        let mut hm = HMInference::new();
        let arms = vec![
            WitnessArm {
                pattern: WitnessPattern::Literal(Literal::Int(1, Span::default())),
                guard: None,
                body: lit_int(10),
            },
            WitnessArm {
                pattern: WitnessPattern::Wildcard,
                guard: None,
                body: lit_int(20),
            },
        ];
        let expr = MirWitness {
            kind: WitnessKind::Match {
                scrutinee: Box::new(lit_int(1)),
                arms,
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        let (_subst, errors) = hm.solve_constraints();
        assert!(
            errors.is_empty(),
            "match with uniform arm types should unify"
        );
    }

    #[test]
    pub(super) fn closure_call_arity_check() {
        let mut hm = HMInference::new();
        let param = WitnessParam {
            name: "x".to_string(),
            type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::Int)),
            default: None,
        };
        let closure_ty = hm.infer_closure(
            &[param],
            &MirWitness {
                kind: WitnessKind::Variable("x".to_string()),
                span: Span::default(),
            },
            Span::default(),
        );
        let closure_ty = closure_ty.unwrap();
        let call = MirWitness {
            kind: WitnessKind::Call {
                callee: WitnessCallee::Var("c".to_string()),
                args: vec![lit_int(7), lit_int(8)],
            },
            span: Span::default(),
        };
        let _ = closure_ty; // Just check the compile
        let _ = call;
    }

    // ─── v0.80: perform/handle inference tests ───

    #[test]
    fn perform_produces_effect_row() {
        let mut hm = HMInference::new();
        let expr = MirWitness {
            kind: WitnessKind::Perform {
                effect: "Ai".to_string(),
                args: vec![],
            },
            span: Span::default(),
        };
        let (ty, row) = hm.infer_expr(&expr).unwrap();
        // perform returns a fresh type var (concrete type determined by handler)
        assert!(matches!(ty, Type::TypeVar(_)));
        // effect row contains "Ai"
        assert!(row.contains("Ai"));
    }

    #[test]
    fn handle_captures_effect() {
        let mut hm = HMInference::new();
        let body = MirWitness {
            kind: WitnessKind::Perform {
                effect: "Ai".to_string(),
                args: vec![],
            },
            span: Span::default(),
        };
        let handler = MirWitness {
            kind: WitnessKind::Literal(Literal::String("handled".to_string(), Span::default())),
            span: Span::default(),
        };
        let expr = MirWitness {
            kind: WitnessKind::Handle {
                effect: "Ai".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        };
        let (ty, row) = hm.infer_expr(&expr).unwrap();
        // handle returns body type (fresh var from perform)
        assert!(matches!(ty, Type::TypeVar(_)));
        // v0.96: 吸收语义改为直接行差 —— effect 被捕获后残差**精确为纯**，
        // 不再是宽松的 fresh row var（后者会向上泄漏未消解的未知行）。
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn handle_solves_row_constraint() {
        let mut hm = HMInference::new();
        let body = MirWitness {
            kind: WitnessKind::Perform {
                effect: "Ai".to_string(),
                args: vec![],
            },
            span: Span::default(),
        };
        let handler = MirWitness {
            kind: WitnessKind::Literal(Literal::String("handled".to_string(), Span::default())),
            span: Span::default(),
        };
        let expr = MirWitness {
            kind: WitnessKind::Handle {
                effect: "Ai".to_string(),
                body: Box::new(body),
                handler: Box::new(handler),
                k_param: "k".to_string(),
            },
            span: Span::default(),
        };
        hm.infer_expr(&expr).unwrap();
        // solve_constraints should succeed — RowEq(body_row, Cons("Ai", residual))
        // unifies body_row (Cons("Ai", Empty)) with Cons("Ai", residual),
        // binding residual to Empty.
        let (_subst, errors) = hm.solve_constraints();
        assert!(
            errors.is_empty(),
            "handle should solve row constraint cleanly"
        );
    }

    #[test]
    fn closure_returns_curried_arrow() {
        let mut hm = HMInference::new();
        let param = WitnessParam {
            name: "x".to_string(),
            type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::Int)),
            default: None,
        };
        let (ty, row) = hm
            .infer_closure(
                &[param],
                &MirWitness {
                    kind: WitnessKind::Variable("x".to_string()),
                    span: Span::default(),
                },
                Span::default(),
            )
            .unwrap();
        // Closure type should be Arrow(Int, Int, Empty)
        assert!(
            matches!(ty, Type::Arrow(_, _, _)),
            "closure should return Arrow, got {:?}",
            ty
        );
        // Closure definition is pure
        assert_eq!(row, crate::mir::effect::EffectRow::Empty);
    }

    #[test]
    fn closure_two_params_curried() {
        let mut hm = HMInference::new();
        let params = vec![
            WitnessParam {
                name: "x".to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::Int)),
                default: None,
            },
            WitnessParam {
                name: "y".to_string(),
                type_hint: Some(crate::mir::hint::TypeHint::from_type(Type::String)),
                default: None,
            },
        ];
        let (ty, _row) = hm
            .infer_closure(
                &params,
                &MirWitness {
                    kind: WitnessKind::Variable("x".to_string()),
                    span: Span::default(),
                },
                Span::default(),
            )
            .unwrap();
        // fn(x: Int, y: String) -> Int 应该是 Arrow(Int, Arrow(String, Int, Empty), Empty)
        match &ty {
            Type::Arrow(input, output, row) => {
                assert_eq!(**input, Type::Int);
                assert_eq!(*row, crate::mir::effect::EffectRow::Empty);
                match output.as_ref() {
                    Type::Arrow(inner_input, inner_output, inner_row) => {
                        assert_eq!(**inner_input, Type::String);
                        assert_eq!(**inner_output, Type::Int);
                        assert_eq!(*inner_row, crate::mir::effect::EffectRow::Empty);
                    }
                    other => panic!("expected inner Arrow, got {:?}", other),
                }
            }
            other => panic!("expected Arrow, got {:?}", other),
        }
    }

    // v0.75.97: instantiate_if_forall helper 测试
    #[test]
    fn instantiate_if_forall_returns_input_when_not_forall() {
        // 非 ForAll 类型直接 clone 返回（no-op）
        let mut hm = HMInference::new();
        let int_ty = Type::Int;
        let result = hm.instantiate_if_forall(&int_ty);
        assert_eq!(result, Type::Int);
        // 复合类型（List/Dict）也不变
        let list_ty = Type::List(Box::new(Type::Int));
        let result = hm.instantiate_if_forall(&list_ty);
        assert_eq!(result, Type::List(Box::new(Type::Int)));
    }

    #[test]
    fn instantiate_if_forall_produces_fresh_copy_when_forall() {
        // ForAll 输入 → 每次实例化一份 fresh 副本（标准 HM let-polymorphism）
        // 关键不变量：内层 TypeVar 名字必须 fresh（不等同于原 vars）
        let mut hm = HMInference::new();
        // 构造 ForAll['a].TypeVar('a) — 最简单的泛型量化
        let forall = Type::ForAll(vec!['a'], Box::new(Type::TypeVar('a')));
        let result1 = hm.instantiate_if_forall(&forall);
        let result2 = hm.instantiate_if_forall(&forall);
        // 两次调用产出独立 TypeVar（名字不同）
        match (&result1, &result2) {
            (Type::TypeVar(c1), Type::TypeVar(c2)) => {
                assert_ne!(c1, c2, "两次实例化必须产出 fresh TypeVar");
            }
            _ => panic!("expected TypeVar after instantiate"),
        }
    }
}
