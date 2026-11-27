//! P6-3B: Path-Sensitive GPU `kernel` Shared-Memory Static Bounds Verifier (`omega`-backed,
//! sound under 32-bit wrapping two's-complement `i32` semantics).
//!
//! Analyzes every `LoadShared` and `StoreShared` IR site inside a compiled [`KernelProgram`]
//! using [`OmegaContext`], enforcing exact **no-overflow side conditions** at every `i32`
//! arithmetic operation so the mathematical integer model ($\mathbb{Z}$) is a strictly sound
//! abstraction of GPU/CPU wrapping `i32` execution (`add.s32` / `sub.s32` / `mul.lo.s32` /
//! `wrapping_add` / `wrapping_sub` / `wrapping_mul`):
//!
//! - **Runtime-enforced coordinate domain invariants** (enforced by
//!   [`crate::runtime::gpu::kernel_compiler::launch_compiled_kernel`] and
//!   [`crate::runtime::gpu::kernel_compiler::emit_spirv_words`]):
//!   - `1 <= block_dim_a <= MAX_KERNEL_BLOCK_DIM (65535)`, `0 <= tid_a < block_dim_a`;
//!   - `1 <= grid_dim_a <= MAX_KERNEL_GRID_DIM (i32::MAX)`, `0 <= bid_a < grid_dim_a`;
//!   - `gid_a` is only constrained to `[i32::MIN, i32::MAX]` because `bid * block_dim + tid`
//!     may wrap `i32` on huge grids unless `grid_dims` is also bounded;
//!   - Every `i32` parameter and unconstrained local is constrained to `[i32::MIN, i32::MAX]`.
//! - **Exact no-overflow side condition (`entails_i32_range`)**:
//!   - `Add`, `Sub`, constant-factor `Mul`, `Shl`, unary `Neg`, and `Abs` only produce an affine
//!     integer abstraction when `omega` proves the exact integer result stays within
//!     `[i32::MIN, i32::MAX]` under the current path context (ruling out wraparound such as
//!     `tid + 2147483647 + 1` or `-i32::MIN`). If overflow is possible, the expression degrades
//!     conservatively to an opaque `i32` (`None`).
//!   - `if`/`else` and `while` guard extraction (`assume_bool_expr`) ONLY assumes linear
//!     comparisons whose operands are proven overflow-free, preventing unsound dead-branch
//!     pruning on wrapping conditions.
//!   - `Div`, `Rem`, and `Shr` by positive constant `d > 0` explicitly verify `0 <= lhs <= i32::MAX`.
//! - **Inductive no-wrap `while` verification**:
//!   - A monotonic `v = v + k` (`k > 0`) loop variable only receives `init <= v_loop` when
//!     `omega` proves the inductive step `init <= v <= i32::MAX ∧ cond(v) |= v + max_iter_delta <= i32::MAX`
//!     (rejecting wrapping loops such as `while i < 2147483647 { i = i + 2 }`), and symmetrically
//!     for downward loops `v = v - k`.
//! - **Order-independent Kernel IR site-identity proof binding ([`SharedBoundsProofs`])**:
//!   - Proofs are keyed by immutable Kernel IR node identity (`Store(*const KernelStmt)` /
//!     `Load(*const KernelExpr)`) tied to `&'a KernelProgram` via lifetime `'a`. Emitters query
//!     each site directly (`safety_for_store` / `safety_for_load`) with zero traversal-order
//!     cursor and fail-closed `NeedsRuntimeGuard` fallback in both debug and release builds.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use crate::common::kernel_ir::{
    KernelBinOp, KernelExpr, KernelParamType, KernelProgram, KernelScalarType, KernelStmt,
    KernelUnaryOp,
};
use crate::typeck::hm::omega::{AffineExpr, DimAtom, OmegaContext, SolveResult};

const I32_MIN: i128 = i32::MIN as i128;
const I32_MAX: i128 = i32::MAX as i128;

/// Maximum per-axis GPU workgroup / block dimension enforced at runtime by
/// [`crate::runtime::gpu::kernel_compiler::launch_compiled_kernel`] and
/// [`crate::runtime::gpu::kernel_compiler::emit_spirv_words`].
///
/// Matches hardware limits across CUDA/Vulkan/Metal while guaranteeing `0 <= tid < block_dim <= 65535`
/// so small constant offsets on `thread_idx()` never wrap `i32`.
pub(crate) const MAX_KERNEL_BLOCK_DIM: u32 = 65_535;

/// Maximum per-axis grid dimension enforced at runtime by
/// [`crate::runtime::gpu::kernel_compiler::launch_compiled_kernel`], ensuring `grid_dim()` and
/// `block_idx()` remain non-negative signed `i32` values.
pub(crate) const MAX_KERNEL_GRID_DIM: u32 = i32::MAX as u32;

/// Static safety classification for one shared-memory access site (`LoadShared` or `StoreShared`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedAccessSafety {
    /// Statically proven `0 <= idx < len` (or statically unreachable without wraparound):
    /// backend may elide the runtime shared-memory bounds guard.
    ProvenInBounds,
    /// Could not statically prove in-bounds under wrapping `i32` semantics:
    /// backend must emit a safe runtime bounds guard.
    NeedsRuntimeGuard,
}

/// Order-independent identity key for a shared-memory access site inside a borrowed [`KernelProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SharedSiteKey {
    Store(usize),
    Load(usize),
}

impl SharedSiteKey {
    fn for_store(stmt: &KernelStmt) -> Self {
        Self::Store(std::ptr::from_ref(stmt) as usize)
    }

    fn for_load(expr: &KernelExpr) -> Self {
        Self::Load(std::ptr::from_ref(expr) as usize)
    }
}

/// Site-indexed shared-memory bounds proof table bound by lifetime `'a` to `&'a KernelProgram`.
///
/// Lookup is completely independent of emitter traversal order and fail-closed
/// (`NeedsRuntimeGuard`) on any unrecognized site in both debug and release builds.
#[derive(Debug, Clone)]
pub(crate) struct SharedBoundsProofs<'a> {
    sites: HashMap<SharedSiteKey, SharedAccessSafety>,
    _program: PhantomData<&'a KernelProgram>,
}

impl<'a> SharedBoundsProofs<'a> {
    pub(crate) fn empty() -> Self {
        Self {
            sites: HashMap::new(),
            _program: PhantomData,
        }
    }

    /// Query proven bounds safety for a specific `KernelStmt::StoreShared` IR node.
    pub(crate) fn safety_for_store(&self, stmt: &'a KernelStmt) -> SharedAccessSafety {
        self.sites
            .get(&SharedSiteKey::for_store(stmt))
            .copied()
            .unwrap_or(SharedAccessSafety::NeedsRuntimeGuard)
    }

    /// Query proven bounds safety for a specific `KernelExpr::LoadShared` IR node.
    pub(crate) fn safety_for_load(&self, expr: &'a KernelExpr) -> SharedAccessSafety {
        self.sites
            .get(&SharedSiteKey::for_load(expr))
            .copied()
            .unwrap_or(SharedAccessSafety::NeedsRuntimeGuard)
    }

    #[cfg(test)]
    pub(crate) fn summary(&self) -> (usize, usize) {
        let proven = self
            .sites
            .values()
            .filter(|&&s| s == SharedAccessSafety::ProvenInBounds)
            .count();
        (proven, self.sites.len())
    }
}

/// Compile-time provable out-of-bounds violation inside a GPU `kernel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KernelBoundsViolation {
    pub(crate) constraint: String,
    pub(crate) context: String,
}

/// Verify all shared-memory accesses in `program` under optional launch `block_dims`.
///
/// Returns `Ok(SharedBoundsProofs)` when no reachable access is provably out of bounds,
/// or `Err(violations)` listing every provably out-of-bounds shared access.
pub(crate) fn verify_kernel_shared_bounds<'a>(
    program: &'a KernelProgram,
    block_dims: Option<[u32; 3]>,
) -> Result<SharedBoundsProofs<'a>, Vec<KernelBoundsViolation>> {
    let mut analyzer = KernelBoundsAnalyzer::new(program, block_dims);
    let mut ctx = analyzer.seed_root_context();
    let mut env = HashMap::new();
    analyzer.analyze_stmts(&program.body, &mut ctx, &mut env);
    if analyzer.violations.is_empty() {
        Ok(SharedBoundsProofs {
            sites: analyzer.sites,
            _program: PhantomData,
        })
    } else {
        Err(analyzer.violations)
    }
}

/// Analyze shared-memory accesses for backend codegen (never fails: any provably-OOB site that
/// bypassed HM — e.g. dynamically-supplied launch block dims — conservatively receives
/// [`SharedAccessSafety::NeedsRuntimeGuard`] so GPU hardware never faults).
pub(crate) fn analyze_kernel_shared_bounds_for_codegen<'a>(
    program: &'a KernelProgram,
    block_dims: Option<[u32; 3]>,
) -> SharedBoundsProofs<'a> {
    if program.shared.is_empty() {
        return SharedBoundsProofs::empty();
    }
    let mut analyzer = KernelBoundsAnalyzer::new(program, block_dims);
    let mut ctx = analyzer.seed_root_context();
    let mut env = HashMap::new();
    analyzer.analyze_stmts(&program.body, &mut ctx, &mut env);
    SharedBoundsProofs {
        sites: analyzer.sites,
        _program: PhantomData,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InductionStep {
    Up(i128),
    Down(i128),
    Arbitrary,
}

#[derive(Default)]
struct UsedAtoms {
    tid_or_bdim: [bool; 3],
    bid_or_gdim: [bool; 3],
    gid: [bool; 3],
    i32_params: HashSet<usize>,
}

fn collect_used_atoms_stmts(stmts: &[KernelStmt], out: &mut UsedAtoms) {
    for s in stmts {
        match s {
            KernelStmt::Let { expr, .. } | KernelStmt::Assign { expr, .. } => {
                collect_used_atoms_expr(expr, out);
            }
            KernelStmt::Store { index, value, .. }
            | KernelStmt::StoreShared { index, value, .. } => {
                collect_used_atoms_expr(index, out);
                collect_used_atoms_expr(value, out);
            }
            KernelStmt::SyncThreads => {}
            KernelStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                collect_used_atoms_expr(cond, out);
                collect_used_atoms_stmts(then_body, out);
                collect_used_atoms_stmts(else_body, out);
            }
            KernelStmt::While { cond, body } => {
                collect_used_atoms_expr(cond, out);
                collect_used_atoms_stmts(body, out);
            }
        }
    }
}

fn collect_used_atoms_expr(expr: &KernelExpr, out: &mut UsedAtoms) {
    match expr {
        KernelExpr::Param(idx, KernelScalarType::I32) => {
            out.i32_params.insert(*idx);
        }
        KernelExpr::ThreadIdx | KernelExpr::BlockDim => out.tid_or_bdim[0] = true,
        KernelExpr::ThreadIdxY | KernelExpr::BlockDimY => out.tid_or_bdim[1] = true,
        KernelExpr::ThreadIdxZ | KernelExpr::BlockDimZ => out.tid_or_bdim[2] = true,
        KernelExpr::BlockIdx | KernelExpr::GridDim => out.bid_or_gdim[0] = true,
        KernelExpr::BlockIdxY | KernelExpr::GridDimY => out.bid_or_gdim[1] = true,
        KernelExpr::BlockIdxZ | KernelExpr::GridDimZ => out.bid_or_gdim[2] = true,
        KernelExpr::GlobalId => out.gid[0] = true,
        KernelExpr::GlobalIdY => out.gid[1] = true,
        KernelExpr::GlobalIdZ => out.gid[2] = true,
        KernelExpr::Load { index, .. } | KernelExpr::LoadShared { index, .. } => {
            collect_used_atoms_expr(index, out);
        }
        KernelExpr::Unary { arg, .. } => collect_used_atoms_expr(arg, out),
        KernelExpr::Binary { lhs, rhs, .. } => {
            collect_used_atoms_expr(lhs, out);
            collect_used_atoms_expr(rhs, out);
        }
        _ => {}
    }
}

fn assume_i32_range(ctx: &mut OmegaContext, expr: &AffineExpr) {
    ctx.assume_le(&AffineExpr::constant(I32_MIN), expr);
    ctx.assume_le(expr, &AffineExpr::constant(I32_MAX));
}

fn entails_i32_range(ctx: &OmegaContext, expr: &AffineExpr) -> bool {
    if let Some(c) = expr.as_constant() {
        return (I32_MIN..=I32_MAX).contains(&c);
    }
    ctx.entails_le(&AffineExpr::constant(I32_MIN), expr) == SolveResult::True
        && ctx.entails_le(expr, &AffineExpr::constant(I32_MAX)) == SolveResult::True
}

fn entails_non_neg_i32(ctx: &OmegaContext, expr: &AffineExpr) -> bool {
    if let Some(c) = expr.as_constant() {
        return (0..=I32_MAX).contains(&c);
    }
    ctx.entails_le(&AffineExpr::constant(0), expr) == SolveResult::True
        && ctx.entails_le(expr, &AffineExpr::constant(I32_MAX)) == SolveResult::True
}

struct KernelBoundsAnalyzer<'a> {
    program: &'a KernelProgram,
    block_dims: Option<[u32; 3]>,
    next_var_id: u32,
    sites: HashMap<SharedSiteKey, SharedAccessSafety>,
    violations: Vec<KernelBoundsViolation>,
}

impl<'a> KernelBoundsAnalyzer<'a> {
    fn new(program: &'a KernelProgram, block_dims: Option<[u32; 3]>) -> Self {
        // Ignore out-of-range block_dims conservatively (launch rejects > MAX_KERNEL_BLOCK_DIM).
        let validated_block = block_dims.and_then(|b| {
            let b = [b[0].max(1), b[1].max(1), b[2].max(1)];
            if b.iter().all(|&d| d <= MAX_KERNEL_BLOCK_DIM) {
                Some(b)
            } else {
                None
            }
        });
        Self {
            program,
            block_dims: validated_block,
            next_var_id: 0,
            sites: HashMap::new(),
            violations: Vec::new(),
        }
    }

    fn sym(name: &str) -> AffineExpr {
        AffineExpr::atom(DimAtom::Symbolic(name.to_string()))
    }

    fn fresh_i32_atom(&mut self, prefix: &str, ctx: &mut OmegaContext) -> AffineExpr {
        let id = self.next_var_id;
        self.next_var_id = self.next_var_id.saturating_add(1);
        let aff = AffineExpr::atom(DimAtom::Symbolic(format!("${prefix}_{id}")));
        assume_i32_range(ctx, &aff);
        aff
    }

    fn fresh_unseeded_atom(&mut self, prefix: &str) -> AffineExpr {
        let id = self.next_var_id;
        self.next_var_id = self.next_var_id.saturating_add(1);
        AffineExpr::atom(DimAtom::Symbolic(format!("${prefix}_{id}")))
    }

    fn seed_root_context(&self) -> OmegaContext {
        let mut used = UsedAtoms::default();
        collect_used_atoms_stmts(&self.program.body, &mut used);

        let mut ctx = OmegaContext::new();
        for (axis_idx, axis) in ["x", "y", "z"].iter().enumerate() {
            if used.tid_or_bdim[axis_idx] {
                let tid = DimAtom::Symbolic(format!("$tid_{axis}"));
                let bdim = DimAtom::Symbolic(format!("$bdim_{axis}"));
                ctx.assume_positive(bdim.clone());
                ctx.assume_le(
                    &AffineExpr::atom(bdim.clone()),
                    &AffineExpr::constant(i128::from(MAX_KERNEL_BLOCK_DIM)),
                );
                ctx.assume_non_negative(tid.clone());
                ctx.assume_lt(&AffineExpr::atom(tid), &AffineExpr::atom(bdim.clone()));

                if let Some(bd) = self.block_dims {
                    ctx.assume_eq(
                        &AffineExpr::atom(bdim),
                        &AffineExpr::constant(i128::from(bd[axis_idx])),
                    );
                }
            }
            if used.bid_or_gdim[axis_idx] {
                let bid = DimAtom::Symbolic(format!("$bid_{axis}"));
                let gdim = DimAtom::Symbolic(format!("$gdim_{axis}"));
                ctx.assume_positive(gdim.clone());
                ctx.assume_le(
                    &AffineExpr::atom(gdim.clone()),
                    &AffineExpr::constant(i128::from(MAX_KERNEL_GRID_DIM)),
                );
                ctx.assume_non_negative(bid.clone());
                ctx.assume_lt(&AffineExpr::atom(bid), &AffineExpr::atom(gdim));
            }
            if used.gid[axis_idx] {
                // `gid = bid * bdim + tid` can wrap `i32` for large grids, so only assume `[I32_MIN, I32_MAX]`.
                let gid = Self::sym(&format!("$gid_{axis}"));
                assume_i32_range(&mut ctx, &gid);
            }
        }
        let mut params: Vec<usize> = used.i32_params.into_iter().collect();
        params.sort_unstable();
        for idx in params {
            if matches!(
                self.program.params.get(idx).map(|p| p.ty),
                Some(KernelParamType::I32)
            ) {
                assume_i32_range(&mut ctx, &Self::sym(&format!("$param_{idx}")));
            }
        }
        ctx
    }

    /// Record all shared accesses in a statically unreachable subtree (proven unreachable under
    /// overflow-checked `i32` semantics) as `ProvenInBounds` without reporting OOB errors.
    fn mark_dead_stmts(&mut self, stmts: &'a [KernelStmt]) {
        for s in stmts {
            match s {
                KernelStmt::Let { expr, .. } | KernelStmt::Assign { expr, .. } => {
                    self.mark_dead_expr(expr);
                }
                KernelStmt::Store { index, value, .. } => {
                    self.mark_dead_expr(index);
                    self.mark_dead_expr(value);
                }
                KernelStmt::StoreShared { index, value, .. } => {
                    self.mark_dead_expr(index);
                    self.mark_dead_expr(value);
                    self.sites.insert(
                        SharedSiteKey::for_store(s),
                        SharedAccessSafety::ProvenInBounds,
                    );
                }
                KernelStmt::SyncThreads => {}
                KernelStmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    self.mark_dead_expr(cond);
                    self.mark_dead_stmts(then_body);
                    self.mark_dead_stmts(else_body);
                }
                KernelStmt::While { cond, body } => {
                    self.mark_dead_expr(cond);
                    self.mark_dead_stmts(body);
                }
            }
        }
    }

    fn mark_dead_expr(&mut self, expr: &'a KernelExpr) {
        match expr {
            KernelExpr::Load { index, .. } => self.mark_dead_expr(index),
            KernelExpr::LoadShared { index, .. } => {
                self.mark_dead_expr(index);
                self.sites.insert(
                    SharedSiteKey::for_load(expr),
                    SharedAccessSafety::ProvenInBounds,
                );
            }
            KernelExpr::Unary { arg, .. } => self.mark_dead_expr(arg),
            KernelExpr::Binary { lhs, rhs, .. } => {
                self.mark_dead_expr(lhs);
                self.mark_dead_expr(rhs);
            }
            _ => {}
        }
    }

    fn analyze_stmts(
        &mut self,
        stmts: &'a [KernelStmt],
        ctx: &mut OmegaContext,
        env: &mut HashMap<String, AffineExpr>,
    ) {
        for stmt in stmts {
            match stmt {
                KernelStmt::Let { name, ty, expr } => {
                    let aff = self.analyze_expr(expr, ctx, env);
                    if *ty == KernelScalarType::I32 {
                        let bound = aff.unwrap_or_else(|| self.fresh_i32_atom(name, ctx));
                        env.insert(name.clone(), bound);
                    }
                }
                KernelStmt::Assign { name, expr } => {
                    let aff = self.analyze_expr(expr, ctx, env);
                    if expr.ty() == KernelScalarType::I32 {
                        let bound = aff.unwrap_or_else(|| self.fresh_i32_atom(name, ctx));
                        env.insert(name.clone(), bound);
                    }
                }
                KernelStmt::Store { index, value, .. } => {
                    let _ = self.analyze_expr(index, ctx, env);
                    let _ = self.analyze_expr(value, ctx, env);
                }
                KernelStmt::StoreShared {
                    shared_idx,
                    index,
                    value,
                } => {
                    let idx_aff = self.analyze_expr(index, ctx, env);
                    let _ = self.analyze_expr(value, ctx, env);
                    self.check_shared_access(
                        SharedSiteKey::for_store(stmt),
                        true,
                        *shared_idx,
                        idx_aff.as_ref(),
                        ctx,
                    );
                }
                KernelStmt::SyncThreads => {}
                KernelStmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    let _ = self.analyze_expr(cond, ctx, env);

                    let mut then_ctx = ctx.clone();
                    self.assume_bool_expr(cond, true, &mut then_ctx, env);
                    if then_ctx.check_sat() == SolveResult::False {
                        self.mark_dead_stmts(then_body);
                    } else {
                        let mut then_env = env.clone();
                        self.analyze_stmts(then_body, &mut then_ctx, &mut then_env);
                    }

                    let mut else_ctx = ctx.clone();
                    self.assume_bool_expr(cond, false, &mut else_ctx, env);
                    if else_ctx.check_sat() == SolveResult::False {
                        self.mark_dead_stmts(else_body);
                    } else {
                        let mut else_env = env.clone();
                        self.analyze_stmts(else_body, &mut else_ctx, &mut else_env);
                    }

                    // Sound join after `if`: invalidate any variable mutated in either branch.
                    let mut mutated = HashSet::new();
                    let mut declared = HashSet::new();
                    collect_written_vars(then_body, &mut mutated, &mut declared);
                    collect_written_vars(else_body, &mut mutated, &mut declared);
                    let mut joined: Vec<String> = mutated
                        .into_iter()
                        .chain(declared)
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    joined.sort();
                    for name in joined {
                        if env.contains_key(&name) {
                            let fresh = self.fresh_i32_atom(&name, ctx);
                            env.insert(name, fresh);
                        }
                    }
                }
                KernelStmt::While { cond, body } => {
                    let mut assigned = HashSet::new();
                    let mut declared = HashSet::new();
                    collect_written_vars(body, &mut assigned, &mut declared);

                    let mut all_written: Vec<String> = assigned
                        .iter()
                        .chain(declared.iter())
                        .cloned()
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    all_written.sort();

                    // Build base loop/post environments where every mutated outer i32 var gets a
                    // fresh atom bounded in `[I32_MIN, I32_MAX]`.
                    let mut loop_ctx = ctx.clone();
                    let mut loop_env = env.clone();
                    let mut post_env = env.clone();
                    let mut candidates: Vec<(String, AffineExpr, InductionStep)> = Vec::new();

                    for name in &all_written {
                        if !env.contains_key(name) {
                            continue;
                        }
                        let loop_atom = self.fresh_i32_atom(name, &mut loop_ctx);
                        let post_atom = self.fresh_i32_atom(name, ctx);
                        if let Some(init_aff) = env.get(name).cloned()
                            && !declared.contains(name)
                        {
                            let step = classify_induction_var(name, body);
                            if !matches!(step, InductionStep::Arbitrary) {
                                candidates.push((name.clone(), init_aff, step));
                            }
                        }
                        loop_env.insert(name.clone(), loop_atom);
                        post_env.insert(name.clone(), post_atom);
                    }

                    // Inductive no-wrap verification for each monotonic candidate independently:
                    // only assume `init <= v` (or `v <= init`) if `cond(v)` proves the per-iteration
                    // step cannot wrap `i32`.
                    for (name, init_aff, step) in candidates {
                        let v_atom = loop_env[&name].clone();
                        let mut hyp_ctx = loop_ctx.clone();
                        match step {
                            InductionStep::Up(max_delta) => {
                                hyp_ctx.assume_le(&init_aff, &v_atom);
                                self.assume_bool_expr(cond, true, &mut hyp_ctx, &loop_env);
                                if let Some(next_hi) = v_atom.add(&AffineExpr::constant(max_delta))
                                    && hyp_ctx.entails_le(&next_hi, &AffineExpr::constant(I32_MAX))
                                        == SolveResult::True
                                {
                                    loop_ctx.assume_le(&init_aff, &v_atom);
                                    ctx.assume_le(&init_aff, &post_env[&name]);
                                }
                            }
                            InductionStep::Down(max_dec) => {
                                hyp_ctx.assume_le(&v_atom, &init_aff);
                                self.assume_bool_expr(cond, true, &mut hyp_ctx, &loop_env);
                                if let Some(next_lo) = v_atom.sub(&AffineExpr::constant(max_dec))
                                    && hyp_ctx.entails_le(&AffineExpr::constant(I32_MIN), &next_lo)
                                        == SolveResult::True
                                {
                                    loop_ctx.assume_le(&v_atom, &init_aff);
                                    ctx.assume_le(&post_env[&name], &init_aff);
                                }
                            }
                            InductionStep::Arbitrary => {}
                        }
                    }

                    // Loop header `cond` executes under `loop_ctx` / `loop_env`.
                    let _ = self.analyze_expr(cond, &mut loop_ctx, &loop_env);

                    // Loop body executes under `loop_ctx ∧ cond` (overflow-checked).
                    let mut body_ctx = loop_ctx;
                    self.assume_bool_expr(cond, true, &mut body_ctx, &loop_env);
                    if body_ctx.check_sat() == SolveResult::False {
                        self.mark_dead_stmts(body);
                    } else {
                        let mut body_env = loop_env;
                        self.analyze_stmts(body, &mut body_ctx, &mut body_env);
                    }

                    // Post-loop state: loop exited with `cond == false` on `post_env`.
                    *env = post_env;
                    self.assume_bool_expr(cond, false, ctx, env);
                }
            }
        }
    }

    /// Analyze subexpressions (recording any nested `LoadShared` sites by Kernel IR node identity) and
    /// return an exact affine integer abstraction ONLY when `expr` is `i32` AND proven free of
    /// 32-bit signed wraparound under `ctx`.
    fn analyze_expr(
        &mut self,
        expr: &'a KernelExpr,
        ctx: &mut OmegaContext,
        env: &HashMap<String, AffineExpr>,
    ) -> Option<AffineExpr> {
        match expr {
            KernelExpr::ConstI32(v) => Some(AffineExpr::constant(i128::from(*v))),
            KernelExpr::ConstF32(_) | KernelExpr::ConstF64(_) | KernelExpr::ConstBool(_) => None,
            KernelExpr::Param(idx, KernelScalarType::I32) => {
                Some(Self::sym(&format!("$param_{idx}")))
            }
            KernelExpr::Param(_, _) => None,
            KernelExpr::Var(name, KernelScalarType::I32) => env.get(name).cloned(),
            KernelExpr::Var(_, _) => None,
            KernelExpr::GlobalId => Some(Self::sym("$gid_x")),
            KernelExpr::GlobalIdY => Some(Self::sym("$gid_y")),
            KernelExpr::GlobalIdZ => Some(Self::sym("$gid_z")),
            KernelExpr::BlockIdx => Some(Self::sym("$bid_x")),
            KernelExpr::BlockIdxY => Some(Self::sym("$bid_y")),
            KernelExpr::BlockIdxZ => Some(Self::sym("$bid_z")),
            KernelExpr::ThreadIdx => Some(Self::sym("$tid_x")),
            KernelExpr::ThreadIdxY => Some(Self::sym("$tid_y")),
            KernelExpr::ThreadIdxZ => Some(Self::sym("$tid_z")),
            KernelExpr::BlockDim => Some(Self::sym("$bdim_x")),
            KernelExpr::BlockDimY => Some(Self::sym("$bdim_y")),
            KernelExpr::BlockDimZ => Some(Self::sym("$bdim_z")),
            KernelExpr::GridDim => Some(Self::sym("$gdim_x")),
            KernelExpr::GridDimY => Some(Self::sym("$gdim_y")),
            KernelExpr::GridDimZ => Some(Self::sym("$gdim_z")),
            KernelExpr::Load { index, .. } => {
                let _ = self.analyze_expr(index, ctx, env);
                None
            }
            KernelExpr::LoadShared {
                shared_idx, index, ..
            } => {
                let idx_aff = self.analyze_expr(index, ctx, env);
                self.check_shared_access(
                    SharedSiteKey::for_load(expr),
                    false,
                    *shared_idx,
                    idx_aff.as_ref(),
                    ctx,
                );
                None
            }
            KernelExpr::Unary { op, arg, ty } => {
                let inner = self.analyze_expr(arg, ctx, env);
                if *ty != KernelScalarType::I32 {
                    return None;
                }
                match op {
                    KernelUnaryOp::Neg => {
                        let neg = inner?.scale(-1)?;
                        entails_i32_range(ctx, &neg).then_some(neg)
                    }
                    KernelUnaryOp::Abs => {
                        let a = inner?;
                        if entails_non_neg_i32(ctx, &a) {
                            Some(a)
                        } else if ctx.entails_le(&a, &AffineExpr::constant(0)) == SolveResult::True
                        {
                            let neg = a.scale(-1)?;
                            entails_i32_range(ctx, &neg).then_some(neg)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            KernelExpr::Binary { op, lhs, rhs, ty } => {
                let l = self.analyze_expr(lhs, ctx, env);
                let r = self.analyze_expr(rhs, ctx, env);
                if *ty != KernelScalarType::I32 {
                    return None;
                }
                match op {
                    KernelBinOp::Add => {
                        let sum = l?.add(&r?)?;
                        entails_i32_range(ctx, &sum).then_some(sum)
                    }
                    KernelBinOp::Sub => {
                        let diff = l?.sub(&r?)?;
                        entails_i32_range(ctx, &diff).then_some(diff)
                    }
                    KernelBinOp::Mul => {
                        let la = l?;
                        let ra = r?;
                        let prod = if let Some(c) = la.as_constant() {
                            ra.scale(c)?
                        } else {
                            let c = ra.as_constant()?;
                            la.scale(c)?
                        };
                        entails_i32_range(ctx, &prod).then_some(prod)
                    }
                    KernelBinOp::Div => {
                        let la = l?;
                        let d = r?.as_constant()?;
                        if !(1..=I32_MAX).contains(&d) {
                            return None;
                        }
                        if let Some(exact) = la.div_exact(d) {
                            return entails_i32_range(ctx, &exact).then_some(exact);
                        }
                        if entails_non_neg_i32(ctx, &la) {
                            let q = self.fresh_unseeded_atom("div");
                            ctx.assume_le(&AffineExpr::constant(0), &q);
                            ctx.assume_le(&q, &AffineExpr::constant(I32_MAX));
                            if let Some(dq) = q.scale(d) {
                                ctx.assume_le(&dq, &la);
                                if let Some(hi) = dq.add(&AffineExpr::constant(d - 1)) {
                                    ctx.assume_le(&la, &hi);
                                }
                            }
                            return Some(q);
                        }
                        None
                    }
                    KernelBinOp::Rem => {
                        let la = l?;
                        let d = r?.as_constant()?;
                        if !(1..=I32_MAX).contains(&d) {
                            return None;
                        }
                        if entails_non_neg_i32(ctx, &la) {
                            let rem = self.fresh_unseeded_atom("rem");
                            let q = self.fresh_unseeded_atom("q");
                            ctx.assume_le(&AffineExpr::constant(0), &q);
                            ctx.assume_le(&AffineExpr::constant(0), &rem);
                            ctx.assume_lt(&rem, &AffineExpr::constant(d));
                            ctx.assume_le(&rem, &la);
                            ctx.assume_le(&rem, &AffineExpr::constant(I32_MAX));
                            if let Some(dq) = q.scale(d)
                                && let Some(recon) = dq.add(&rem)
                            {
                                ctx.assume_eq(&la, &recon);
                            }
                            return Some(rem);
                        }
                        None
                    }
                    KernelBinOp::BitAnd => {
                        // For any wrapping `i32` value `x` and constant `0 <= mask <= i32::MAX`,
                        // bit 31 is cleared and no bit outside `mask` can be set: `0 <= (x & mask) <= mask`.
                        let mask = l
                            .as_ref()
                            .and_then(AffineExpr::as_constant)
                            .filter(|&c| (0..=I32_MAX).contains(&c))
                            .or_else(|| {
                                r.as_ref()
                                    .and_then(AffineExpr::as_constant)
                                    .filter(|&c| (0..=I32_MAX).contains(&c))
                            })?;
                        let masked = self.fresh_unseeded_atom("and");
                        ctx.assume_le(&AffineExpr::constant(0), &masked);
                        ctx.assume_le(&masked, &AffineExpr::constant(mask));
                        Some(masked)
                    }
                    KernelBinOp::Shl => {
                        let la = l?;
                        let shift = r?.as_constant()?;
                        if (0..31).contains(&shift) {
                            let scaled = la.scale(1i128 << shift)?;
                            entails_i32_range(ctx, &scaled).then_some(scaled)
                        } else {
                            None
                        }
                    }
                    KernelBinOp::Shr => {
                        let la = l?;
                        let shift = r?.as_constant()?;
                        if (0..31).contains(&shift) && entails_non_neg_i32(ctx, &la) {
                            let d = 1i128 << shift;
                            if let Some(exact) = la.div_exact(d) {
                                return entails_i32_range(ctx, &exact).then_some(exact);
                            }
                            let q = self.fresh_unseeded_atom("shr");
                            ctx.assume_le(&AffineExpr::constant(0), &q);
                            ctx.assume_le(&q, &AffineExpr::constant(I32_MAX));
                            if let Some(dq) = q.scale(d) {
                                ctx.assume_le(&dq, &la);
                                if let Some(hi) = dq.add(&AffineExpr::constant(d - 1)) {
                                    ctx.assume_le(&la, &hi);
                                }
                            }
                            return Some(q);
                        }
                        None
                    }
                    KernelBinOp::Min => {
                        // Even an opaque wrapping `i32` operand lives in `[I32_MIN, I32_MAX]`.
                        let la = l.unwrap_or_else(|| self.fresh_i32_atom("min_l", ctx));
                        let ra = r.unwrap_or_else(|| self.fresh_i32_atom("min_r", ctx));
                        if !entails_i32_range(ctx, &la) || !entails_i32_range(ctx, &ra) {
                            return None;
                        }
                        if ctx.entails_le(&la, &ra) == SolveResult::True {
                            return Some(la);
                        }
                        if ctx.entails_le(&ra, &la) == SolveResult::True {
                            return Some(ra);
                        }
                        let m = self.fresh_i32_atom("min", ctx);
                        ctx.assume_le(&m, &la);
                        ctx.assume_le(&m, &ra);
                        if entails_non_neg_i32(ctx, &la) && entails_non_neg_i32(ctx, &ra) {
                            ctx.assume_le(&AffineExpr::constant(0), &m);
                        }
                        Some(m)
                    }
                    KernelBinOp::Max => {
                        let la = l.unwrap_or_else(|| self.fresh_i32_atom("max_l", ctx));
                        let ra = r.unwrap_or_else(|| self.fresh_i32_atom("max_r", ctx));
                        if !entails_i32_range(ctx, &la) || !entails_i32_range(ctx, &ra) {
                            return None;
                        }
                        if ctx.entails_le(&la, &ra) == SolveResult::True {
                            return Some(ra);
                        }
                        if ctx.entails_le(&ra, &la) == SolveResult::True {
                            return Some(la);
                        }
                        let m = self.fresh_i32_atom("max", ctx);
                        ctx.assume_le(&la, &m);
                        ctx.assume_le(&ra, &m);
                        // Preserve known constant upper bound across `max(min(x, hi), lo)` when `lo <= hi`.
                        if let Some((hi, lo)) = extract_clamp_hi_lo(lhs, rhs, &la, &ra)
                            && (I32_MIN..=I32_MAX).contains(&hi)
                            && lo <= hi
                        {
                            ctx.assume_le(&m, &AffineExpr::constant(hi));
                        }
                        Some(m)
                    }
                    _ => None,
                }
            }
        }
    }

    /// Evaluate an `i32` expression purely (no side effects on `sites`) ONLY when every arithmetic
    /// step is proven overflow-free under `ctx` (`[I32_MIN, I32_MAX]`).
    fn eval_sound_i32_affine(
        expr: &KernelExpr,
        ctx: &OmegaContext,
        env: &HashMap<String, AffineExpr>,
    ) -> Option<AffineExpr> {
        match expr {
            KernelExpr::ConstI32(v) => Some(AffineExpr::constant(i128::from(*v))),
            KernelExpr::Param(idx, KernelScalarType::I32) => {
                Some(Self::sym(&format!("$param_{idx}")))
            }
            KernelExpr::Var(name, KernelScalarType::I32) => env.get(name).cloned(),
            KernelExpr::GlobalId => Some(Self::sym("$gid_x")),
            KernelExpr::GlobalIdY => Some(Self::sym("$gid_y")),
            KernelExpr::GlobalIdZ => Some(Self::sym("$gid_z")),
            KernelExpr::BlockIdx => Some(Self::sym("$bid_x")),
            KernelExpr::BlockIdxY => Some(Self::sym("$bid_y")),
            KernelExpr::BlockIdxZ => Some(Self::sym("$bid_z")),
            KernelExpr::ThreadIdx => Some(Self::sym("$tid_x")),
            KernelExpr::ThreadIdxY => Some(Self::sym("$tid_y")),
            KernelExpr::ThreadIdxZ => Some(Self::sym("$tid_z")),
            KernelExpr::BlockDim => Some(Self::sym("$bdim_x")),
            KernelExpr::BlockDimY => Some(Self::sym("$bdim_y")),
            KernelExpr::BlockDimZ => Some(Self::sym("$bdim_z")),
            KernelExpr::GridDim => Some(Self::sym("$gdim_x")),
            KernelExpr::GridDimY => Some(Self::sym("$gdim_y")),
            KernelExpr::GridDimZ => Some(Self::sym("$gdim_z")),
            KernelExpr::Unary {
                op: KernelUnaryOp::Neg,
                arg,
                ty: KernelScalarType::I32,
            } => {
                let neg = Self::eval_sound_i32_affine(arg, ctx, env)?.scale(-1)?;
                entails_i32_range(ctx, &neg).then_some(neg)
            }
            KernelExpr::Binary {
                op,
                lhs,
                rhs,
                ty: KernelScalarType::I32,
            } => {
                let l = Self::eval_sound_i32_affine(lhs, ctx, env)?;
                let r = Self::eval_sound_i32_affine(rhs, ctx, env)?;
                let out = match op {
                    KernelBinOp::Add => l.add(&r)?,
                    KernelBinOp::Sub => l.sub(&r)?,
                    KernelBinOp::Mul => {
                        if let Some(c) = l.as_constant() {
                            r.scale(c)?
                        } else {
                            let c = r.as_constant()?;
                            l.scale(c)?
                        }
                    }
                    KernelBinOp::Div => {
                        let d = r.as_constant()?;
                        if !(1..=I32_MAX).contains(&d) {
                            return None;
                        }
                        l.div_exact(d)?
                    }
                    KernelBinOp::Shl => {
                        let shift = r.as_constant()?;
                        if !(0..31).contains(&shift) {
                            return None;
                        }
                        l.scale(1i128 << shift)?
                    }
                    _ => return None,
                };
                entails_i32_range(ctx, &out).then_some(out)
            }
            _ => None,
        }
    }

    /// Assume boolean condition `expr == truth` in `ctx` (sound conjunctive over-approximation
    /// using overflow-checked `eval_sound_i32_affine` so wrapping comparisons never prune live paths).
    fn assume_bool_expr(
        &self,
        expr: &KernelExpr,
        truth: bool,
        ctx: &mut OmegaContext,
        env: &HashMap<String, AffineExpr>,
    ) {
        match expr {
            KernelExpr::ConstBool(b) => {
                if *b != truth {
                    ctx.assume_le(&AffineExpr::constant(1), &AffineExpr::constant(0));
                }
            }
            KernelExpr::Unary {
                op: KernelUnaryOp::Not,
                arg,
                ..
            } => {
                self.assume_bool_expr(arg, !truth, ctx, env);
            }
            KernelExpr::Binary { op, lhs, rhs, .. } => match op {
                KernelBinOp::And if truth => {
                    self.assume_bool_expr(lhs, true, ctx, env);
                    self.assume_bool_expr(rhs, true, ctx, env);
                }
                KernelBinOp::Or if !truth => {
                    // De Morgan: !(a || b)  <=>  !a && !b
                    self.assume_bool_expr(lhs, false, ctx, env);
                    self.assume_bool_expr(rhs, false, ctx, env);
                }
                KernelBinOp::Lt
                | KernelBinOp::Le
                | KernelBinOp::Gt
                | KernelBinOp::Ge
                | KernelBinOp::Eq
                | KernelBinOp::Ne => {
                    if lhs.ty() != KernelScalarType::I32 || rhs.ty() != KernelScalarType::I32 {
                        return;
                    }
                    let (Some(l), Some(r)) = (
                        Self::eval_sound_i32_affine(lhs, ctx, env),
                        Self::eval_sound_i32_affine(rhs, ctx, env),
                    ) else {
                        return;
                    };
                    match (op, truth) {
                        (KernelBinOp::Lt, true) | (KernelBinOp::Ge, false) => ctx.assume_lt(&l, &r),
                        (KernelBinOp::Le, true) | (KernelBinOp::Gt, false) => ctx.assume_le(&l, &r),
                        (KernelBinOp::Gt, true) | (KernelBinOp::Le, false) => ctx.assume_lt(&r, &l),
                        (KernelBinOp::Ge, true) | (KernelBinOp::Lt, false) => ctx.assume_le(&r, &l),
                        (KernelBinOp::Eq, true) | (KernelBinOp::Ne, false) => ctx.assume_eq(&l, &r),
                        // `l != r` is disjunctive (`l < r || r < l`): soundly skipped in conjunctive context.
                        (KernelBinOp::Eq, false) | (KernelBinOp::Ne, true) => {}
                        _ => {}
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn check_shared_access(
        &mut self,
        site_key: SharedSiteKey,
        is_store: bool,
        shared_idx: usize,
        idx_aff: Option<&AffineExpr>,
        ctx: &OmegaContext,
    ) {
        let Some(sh) = self.program.shared.get(shared_idx) else {
            self.sites
                .insert(site_key, SharedAccessSafety::NeedsRuntimeGuard);
            return;
        };
        let len_aff = AffineExpr::constant(i128::from(sh.len));
        let Some(idx) = idx_aff else {
            self.sites
                .insert(site_key, SharedAccessSafety::NeedsRuntimeGuard);
            return;
        };

        if ctx.entails_in_bounds(idx, &len_aff) == SolveResult::True {
            self.sites
                .insert(site_key, SharedAccessSafety::ProvenInBounds);
            return;
        }

        let op_kind = if is_store { "write" } else { "read" };
        let launch_note = match self.block_dims {
            Some([bx, by, bz]) => format!(" with launch block [{bx}, {by}, {bz}]"),
            None => String::new(),
        };

        if ctx.entails_lt(idx, &AffineExpr::constant(0)) == SolveResult::True {
            self.violations.push(KernelBoundsViolation {
                constraint: format!("0 <= {idx} < {}", sh.len),
                context: format!(
                    "GPU kernel '{}' shared array '{}' (len {}) {op_kind} has provably negative index `{idx}`{launch_note}",
                    self.program.name, sh.name, sh.len
                ),
            });
        } else if ctx.entails_le(&len_aff, idx) == SolveResult::True {
            self.violations.push(KernelBoundsViolation {
                constraint: format!("0 <= {idx} < {}", sh.len),
                context: format!(
                    "GPU kernel '{}' shared array '{}' (len {}) {op_kind} has provably out-of-bounds index `{idx}` (>= {}){launch_note}",
                    self.program.name, sh.name, sh.len, sh.len
                ),
            });
        }

        self.sites
            .insert(site_key, SharedAccessSafety::NeedsRuntimeGuard);
    }
}

/// Detect `max(min(x, hi), lo)` or `max(lo, min(x, hi))` returning `Some((hi, lo))` for constant `hi, lo`.
fn extract_clamp_hi_lo(
    lhs: &KernelExpr,
    rhs: &KernelExpr,
    la: &AffineExpr,
    ra: &AffineExpr,
) -> Option<(i128, i128)> {
    if let Some(lo) = ra.as_constant()
        && let Some(hi) = extract_min_const_bound(lhs)
    {
        return Some((hi, lo));
    }
    if let Some(lo) = la.as_constant()
        && let Some(hi) = extract_min_const_bound(rhs)
    {
        return Some((hi, lo));
    }
    None
}

fn extract_min_const_bound(expr: &KernelExpr) -> Option<i128> {
    let KernelExpr::Binary {
        op: KernelBinOp::Min,
        lhs,
        rhs,
        ty: KernelScalarType::I32,
    } = expr
    else {
        return None;
    };
    if let KernelExpr::ConstI32(c) = lhs.as_ref() {
        return Some(i128::from(*c));
    }
    if let KernelExpr::ConstI32(c) = rhs.as_ref() {
        return Some(i128::from(*c));
    }
    None
}

fn collect_written_vars(
    stmts: &[KernelStmt],
    assigned: &mut HashSet<String>,
    declared: &mut HashSet<String>,
) {
    for s in stmts {
        match s {
            KernelStmt::Let { name, .. } => {
                declared.insert(name.clone());
            }
            KernelStmt::Assign { name, .. } => {
                assigned.insert(name.clone());
            }
            KernelStmt::If {
                then_body,
                else_body,
                ..
            } => {
                collect_written_vars(then_body, assigned, declared);
                collect_written_vars(else_body, assigned, declared);
            }
            KernelStmt::While { body, .. } => {
                collect_written_vars(body, assigned, declared);
            }
            KernelStmt::Store { .. } | KernelStmt::StoreShared { .. } | KernelStmt::SyncThreads => {
            }
        }
    }
}

/// Classify whether all `Assign` statements to `var` inside a single iteration of `stmts` move
/// `var` strictly monotonically upward (`Up(max_iter_delta)`) or downward (`Down(max_iter_dec)`),
/// rejecting updates inside nested `While` loops whose per-iteration total delta is unbounded.
fn classify_induction_var(var: &str, stmts: &[KernelStmt]) -> InductionStep {
    let mut saw_up = false;
    let mut saw_down = false;
    let mut arbitrary = false;
    let max_mag = inspect_var_updates(
        var,
        stmts,
        false,
        &mut saw_up,
        &mut saw_down,
        &mut arbitrary,
    );
    if arbitrary || (saw_up && saw_down) || max_mag <= 0 {
        InductionStep::Arbitrary
    } else if saw_up {
        InductionStep::Up(max_mag)
    } else if saw_down {
        InductionStep::Down(max_mag)
    } else {
        InductionStep::Arbitrary
    }
}

fn inspect_var_updates(
    var: &str,
    stmts: &[KernelStmt],
    in_nested_loop: bool,
    saw_up: &mut bool,
    saw_down: &mut bool,
    arbitrary: &mut bool,
) -> i128 {
    let mut total: i128 = 0;
    for s in stmts {
        if *arbitrary {
            return 0;
        }
        match s {
            KernelStmt::Assign { name, expr } if name == var => {
                if in_nested_loop {
                    *arbitrary = true;
                    return 0;
                }
                match extract_step_delta(var, expr) {
                    Some(delta) if delta > 0 => {
                        *saw_up = true;
                        match total.checked_add(delta) {
                            Some(next) => total = next,
                            None => {
                                *arbitrary = true;
                                return 0;
                            }
                        }
                    }
                    Some(delta) if delta < 0 => {
                        *saw_down = true;
                        let Some(mag) = delta.checked_neg() else {
                            *arbitrary = true;
                            return 0;
                        };
                        match total.checked_add(mag) {
                            Some(next) => total = next,
                            None => {
                                *arbitrary = true;
                                return 0;
                            }
                        }
                    }
                    Some(_) => {} // `v = v + 0`
                    None => {
                        *arbitrary = true;
                        return 0;
                    }
                }
            }
            KernelStmt::If {
                then_body,
                else_body,
                ..
            } => {
                let t = inspect_var_updates(
                    var,
                    then_body,
                    in_nested_loop,
                    saw_up,
                    saw_down,
                    arbitrary,
                );
                let e = inspect_var_updates(
                    var,
                    else_body,
                    in_nested_loop,
                    saw_up,
                    saw_down,
                    arbitrary,
                );
                let branch_max = t.max(e);
                match total.checked_add(branch_max) {
                    Some(next) => total = next,
                    None => {
                        *arbitrary = true;
                        return 0;
                    }
                }
            }
            KernelStmt::While { body, .. } => {
                let _ = inspect_var_updates(var, body, true, saw_up, saw_down, arbitrary);
            }
            _ => {}
        }
    }
    total
}

/// Match `var + k`, `k + var`, or `var - k` for constant integer `k`, returning signed delta.
fn extract_step_delta(var: &str, expr: &KernelExpr) -> Option<i128> {
    let KernelExpr::Binary {
        op,
        lhs,
        rhs,
        ty: KernelScalarType::I32,
    } = expr
    else {
        return None;
    };
    let is_var =
        |e: &KernelExpr| matches!(e, KernelExpr::Var(n, KernelScalarType::I32) if n == var);
    match op {
        KernelBinOp::Add => {
            if is_var(lhs)
                && let KernelExpr::ConstI32(k) = rhs.as_ref()
            {
                return Some(i128::from(*k));
            }
            if is_var(rhs)
                && let KernelExpr::ConstI32(k) = lhs.as_ref()
            {
                return Some(i128::from(*k));
            }
            None
        }
        KernelBinOp::Sub => {
            if is_var(lhs)
                && let KernelExpr::ConstI32(k) = rhs.as_ref()
            {
                return Some(-i128::from(*k));
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::kernel_ir::{KernelParam, KernelSharedArray};

    fn mk_program(shared: Vec<KernelSharedArray>, body: Vec<KernelStmt>) -> KernelProgram {
        KernelProgram {
            name: "test_k".to_string(),
            params: vec![KernelParam {
                name: "out".to_string(),
                ty: KernelParamType::BufF32,
            }],
            shared,
            body,
        }
    }

    fn sh_f32(name: &str, len: u32) -> KernelSharedArray {
        KernelSharedArray {
            name: name.to_string(),
            elem_ty: KernelScalarType::F32,
            len,
        }
    }

    #[test]
    fn proves_guarded_thread_idx_and_rejects_else_branch_oob() {
        let prog = mk_program(
            vec![sh_f32("tile", 16)],
            vec![KernelStmt::If {
                cond: KernelExpr::Binary {
                    op: KernelBinOp::Lt,
                    lhs: Box::new(KernelExpr::ThreadIdx),
                    rhs: Box::new(KernelExpr::ConstI32(16)),
                    ty: KernelScalarType::Bool,
                },
                then_body: vec![KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::ThreadIdx,
                    value: KernelExpr::ConstF32(1.0),
                }],
                else_body: vec![],
            }],
        );
        let proofs = verify_kernel_shared_bounds(&prog, None).expect("guarded access must pass");
        assert_eq!(proofs.summary(), (1, 1));

        let bad_prog = mk_program(
            vec![sh_f32("tile", 16)],
            vec![KernelStmt::If {
                cond: KernelExpr::Binary {
                    op: KernelBinOp::Lt,
                    lhs: Box::new(KernelExpr::ThreadIdx),
                    rhs: Box::new(KernelExpr::ConstI32(16)),
                    ty: KernelScalarType::Bool,
                },
                then_body: vec![],
                else_body: vec![KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::ThreadIdx,
                    value: KernelExpr::ConstF32(0.0),
                }],
            }],
        );
        let errs = verify_kernel_shared_bounds(&bad_prog, None).expect_err("else branch is OOB");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].context.contains("provably out-of-bounds"));
    }

    #[test]
    fn proves_tiled_gemm_4x4_induction_loop_100_percent_in_bounds_with_block_4x4() {
        let idx_store = KernelExpr::Binary {
            op: KernelBinOp::Add,
            lhs: Box::new(KernelExpr::Binary {
                op: KernelBinOp::Mul,
                lhs: Box::new(KernelExpr::ThreadIdxY),
                rhs: Box::new(KernelExpr::ConstI32(4)),
                ty: KernelScalarType::I32,
            }),
            rhs: Box::new(KernelExpr::ThreadIdx),
            ty: KernelScalarType::I32,
        };
        let idx_loop = KernelExpr::Binary {
            op: KernelBinOp::Add,
            lhs: Box::new(KernelExpr::Binary {
                op: KernelBinOp::Mul,
                lhs: Box::new(KernelExpr::ThreadIdxY),
                rhs: Box::new(KernelExpr::ConstI32(4)),
                ty: KernelScalarType::I32,
            }),
            rhs: Box::new(KernelExpr::Var("i".to_string(), KernelScalarType::I32)),
            ty: KernelScalarType::I32,
        };
        let prog = mk_program(
            vec![sh_f32("tile_a", 16)],
            vec![
                KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: idx_store,
                    value: KernelExpr::ConstF32(1.0),
                },
                KernelStmt::Let {
                    name: "i".to_string(),
                    ty: KernelScalarType::I32,
                    expr: KernelExpr::ConstI32(0),
                },
                KernelStmt::While {
                    cond: KernelExpr::Binary {
                        op: KernelBinOp::Lt,
                        lhs: Box::new(KernelExpr::Var("i".to_string(), KernelScalarType::I32)),
                        rhs: Box::new(KernelExpr::ConstI32(4)),
                        ty: KernelScalarType::Bool,
                    },
                    body: vec![
                        KernelStmt::Store {
                            buf_param: 0,
                            index: KernelExpr::ConstI32(0),
                            value: KernelExpr::LoadShared {
                                shared_idx: 0,
                                index: Box::new(idx_loop),
                                elem_ty: KernelScalarType::F32,
                            },
                        },
                        KernelStmt::Assign {
                            name: "i".to_string(),
                            expr: KernelExpr::Binary {
                                op: KernelBinOp::Add,
                                lhs: Box::new(KernelExpr::Var(
                                    "i".to_string(),
                                    KernelScalarType::I32,
                                )),
                                rhs: Box::new(KernelExpr::ConstI32(1)),
                                ty: KernelScalarType::I32,
                            },
                        },
                    ],
                },
            ],
        );

        let unspec = verify_kernel_shared_bounds(&prog, None).expect("valid generic kernel");
        assert_eq!(unspec.summary(), (0, 2));

        let proven =
            verify_kernel_shared_bounds(&prog, Some([4, 4, 1])).expect("4x4 block proves bounds");
        assert_eq!(proven.summary(), (2, 2));
    }

    #[test]
    fn proves_bitmask_rem_div_and_clamp_in_bounds() {
        let prog = mk_program(
            vec![sh_f32("tile", 16)],
            vec![
                KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::Binary {
                        op: KernelBinOp::BitAnd,
                        lhs: Box::new(KernelExpr::ThreadIdx),
                        rhs: Box::new(KernelExpr::ConstI32(15)),
                        ty: KernelScalarType::I32,
                    },
                    value: KernelExpr::ConstF32(1.0),
                },
                KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::Binary {
                        op: KernelBinOp::Rem,
                        lhs: Box::new(KernelExpr::ThreadIdx),
                        rhs: Box::new(KernelExpr::ConstI32(16)),
                        ty: KernelScalarType::I32,
                    },
                    value: KernelExpr::ConstF32(2.0),
                },
                KernelStmt::If {
                    cond: KernelExpr::Binary {
                        op: KernelBinOp::Lt,
                        lhs: Box::new(KernelExpr::ThreadIdx),
                        rhs: Box::new(KernelExpr::ConstI32(32)),
                        ty: KernelScalarType::Bool,
                    },
                    then_body: vec![KernelStmt::StoreShared {
                        shared_idx: 0,
                        index: KernelExpr::Binary {
                            op: KernelBinOp::Div,
                            lhs: Box::new(KernelExpr::ThreadIdx),
                            rhs: Box::new(KernelExpr::ConstI32(2)),
                            ty: KernelScalarType::I32,
                        },
                        value: KernelExpr::ConstF32(3.0),
                    }],
                    else_body: vec![],
                },
                KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::Binary {
                        op: KernelBinOp::Min,
                        lhs: Box::new(KernelExpr::Binary {
                            op: KernelBinOp::Max,
                            lhs: Box::new(KernelExpr::ThreadIdx),
                            rhs: Box::new(KernelExpr::ConstI32(0)),
                            ty: KernelScalarType::I32,
                        }),
                        rhs: Box::new(KernelExpr::ConstI32(15)),
                        ty: KernelScalarType::I32,
                    },
                    value: KernelExpr::ConstF32(4.0),
                },
            ],
        );
        let proofs = verify_kernel_shared_bounds(&prog, None).expect("all 4 idioms are safe");
        assert_eq!(proofs.summary(), (4, 4));
    }

    #[test]
    fn catches_post_increment_loop_oob_soundly() {
        let prog = mk_program(
            vec![sh_f32("tile", 4)],
            vec![
                KernelStmt::Let {
                    name: "i".to_string(),
                    ty: KernelScalarType::I32,
                    expr: KernelExpr::ConstI32(0),
                },
                KernelStmt::While {
                    cond: KernelExpr::Binary {
                        op: KernelBinOp::Lt,
                        lhs: Box::new(KernelExpr::Var("i".to_string(), KernelScalarType::I32)),
                        rhs: Box::new(KernelExpr::ConstI32(4)),
                        ty: KernelScalarType::Bool,
                    },
                    body: vec![
                        KernelStmt::Assign {
                            name: "i".to_string(),
                            expr: KernelExpr::Binary {
                                op: KernelBinOp::Add,
                                lhs: Box::new(KernelExpr::Var(
                                    "i".to_string(),
                                    KernelScalarType::I32,
                                )),
                                rhs: Box::new(KernelExpr::ConstI32(1)),
                                ty: KernelScalarType::I32,
                            },
                        },
                        KernelStmt::StoreShared {
                            shared_idx: 0,
                            index: KernelExpr::Var("i".to_string(), KernelScalarType::I32),
                            value: KernelExpr::ConstF32(1.0),
                        },
                    ],
                },
            ],
        );
        let proofs = verify_kernel_shared_bounds(&prog, None).expect("not always OOB");
        assert_eq!(proofs.summary(), (0, 1));
    }

    #[test]
    fn wrapping_i32_condition_is_never_unsoundly_pruned_as_dead_branch() {
        // Reproducer: `if tid + 2147483647 + 1 < 4 { tile[tid + 300] = 1.0 }` on len 16.
        // Under wrapping i32, tid=0 gives `-2147483648 < 4` (true!), so branch IS reachable
        // and `tile[tid + 300]` MUST be caught as OOB (and guarded in codegen, never elided!).
        let wrap_sum = KernelExpr::Binary {
            op: KernelBinOp::Add,
            lhs: Box::new(KernelExpr::Binary {
                op: KernelBinOp::Add,
                lhs: Box::new(KernelExpr::ThreadIdx),
                rhs: Box::new(KernelExpr::ConstI32(i32::MAX)),
                ty: KernelScalarType::I32,
            }),
            rhs: Box::new(KernelExpr::ConstI32(1)),
            ty: KernelScalarType::I32,
        };
        let prog = mk_program(
            vec![sh_f32("tile", 16)],
            vec![KernelStmt::If {
                cond: KernelExpr::Binary {
                    op: KernelBinOp::Lt,
                    lhs: Box::new(wrap_sum),
                    rhs: Box::new(KernelExpr::ConstI32(4)),
                    ty: KernelScalarType::Bool,
                },
                then_body: vec![KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::Binary {
                        op: KernelBinOp::Add,
                        lhs: Box::new(KernelExpr::ThreadIdx),
                        rhs: Box::new(KernelExpr::ConstI32(300)),
                        ty: KernelScalarType::I32,
                    },
                    value: KernelExpr::ConstF32(1.0),
                }],
                else_body: vec![],
            }],
        );
        let errs = verify_kernel_shared_bounds(&prog, None)
            .expect_err("wrapping branch is reachable and tile[tid + 300] is OOB");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].context.contains("provably out-of-bounds"));

        // Even for codegen (bypassing HM), the store MUST remain `NeedsRuntimeGuard`!
        let proofs = analyze_kernel_shared_bounds_for_codegen(&prog, None);
        assert_eq!(proofs.summary(), (0, 1));
    }

    #[test]
    fn wrapping_while_step_refuses_unsound_non_negative_invariant() {
        // `let i = 0; while i < i32::MAX { if i < 16 { tile[i] = 1.0 }; i = i + 2 }`
        // At `i = i32::MAX - 1`, `i + 2` wraps to `i32::MIN < 16`, so `tile[i]` can be negative!
        // Sound verifier MUST NOT assume `0 <= i` across iterations, so `tile[i]` must stay guarded.
        let prog = mk_program(
            vec![sh_f32("tile", 16)],
            vec![
                KernelStmt::Let {
                    name: "i".to_string(),
                    ty: KernelScalarType::I32,
                    expr: KernelExpr::ConstI32(0),
                },
                KernelStmt::While {
                    cond: KernelExpr::Binary {
                        op: KernelBinOp::Lt,
                        lhs: Box::new(KernelExpr::Var("i".to_string(), KernelScalarType::I32)),
                        rhs: Box::new(KernelExpr::ConstI32(i32::MAX)),
                        ty: KernelScalarType::Bool,
                    },
                    body: vec![
                        KernelStmt::If {
                            cond: KernelExpr::Binary {
                                op: KernelBinOp::Lt,
                                lhs: Box::new(KernelExpr::Var(
                                    "i".to_string(),
                                    KernelScalarType::I32,
                                )),
                                rhs: Box::new(KernelExpr::ConstI32(16)),
                                ty: KernelScalarType::Bool,
                            },
                            then_body: vec![KernelStmt::StoreShared {
                                shared_idx: 0,
                                index: KernelExpr::Var("i".to_string(), KernelScalarType::I32),
                                value: KernelExpr::ConstF32(1.0),
                            }],
                            else_body: vec![],
                        },
                        KernelStmt::Assign {
                            name: "i".to_string(),
                            expr: KernelExpr::Binary {
                                op: KernelBinOp::Add,
                                lhs: Box::new(KernelExpr::Var(
                                    "i".to_string(),
                                    KernelScalarType::I32,
                                )),
                                rhs: Box::new(KernelExpr::ConstI32(2)),
                                ty: KernelScalarType::I32,
                            },
                        },
                    ],
                },
            ],
        );
        let proofs = verify_kernel_shared_bounds(&prog, None).expect("not always OOB");
        assert_eq!(proofs.summary(), (0, 1));
    }

    #[test]
    fn site_identity_proof_lookup_is_order_independent_and_fail_closed() {
        // First store is ProvenInBounds (`tile[0]`), second store is NeedsRuntimeGuard (`tile[tid]`).
        let prog = mk_program(
            vec![sh_f32("tile", 16)],
            vec![
                KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::ConstI32(0),
                    value: KernelExpr::ConstF32(1.0),
                },
                KernelStmt::StoreShared {
                    shared_idx: 0,
                    index: KernelExpr::ThreadIdx,
                    value: KernelExpr::ConstF32(2.0),
                },
            ],
        );
        let proofs = verify_kernel_shared_bounds(&prog, None).expect("valid");
        // Query in REVERSE order and repeatedly — must return exact per-site result!
        assert_eq!(
            proofs.safety_for_store(&prog.body[1]),
            SharedAccessSafety::NeedsRuntimeGuard
        );
        assert_eq!(
            proofs.safety_for_store(&prog.body[0]),
            SharedAccessSafety::ProvenInBounds
        );
        assert_eq!(
            proofs.safety_for_store(&prog.body[1]),
            SharedAccessSafety::NeedsRuntimeGuard
        );
        // Unrelated node fails closed to NeedsRuntimeGuard without affecting any other site.
        let foreign = KernelStmt::StoreShared {
            shared_idx: 0,
            index: KernelExpr::ConstI32(0),
            value: KernelExpr::ConstF32(1.0),
        };
        assert_eq!(
            proofs.safety_for_store(&foreign),
            SharedAccessSafety::NeedsRuntimeGuard
        );
        assert_eq!(
            proofs.safety_for_store(&prog.body[0]),
            SharedAccessSafety::ProvenInBounds
        );
    }
}
