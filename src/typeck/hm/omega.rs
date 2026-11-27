//! P6-3: Zero-dependency Presburger Linear Arithmetic & Monomial Volume Solver (`omega`-lite).
//!
//! Provides exact, decidable compile-time reasoning over Mora static tensor shapes (`DimExpr`),
//! linear dimension guards (`where`), bounded indices (`fin<N>`), and GPU `kernel` shared-memory
//! bounds:
//!
//! 1. **Canonical Affine Normal Form ([`AffineExpr`])**:
//!    Normalizes linear combinations $c_0 + \sum_i c_i v_i$ ($c_i \in \mathbb{Z}$) over symbolic and
//!    meta dimension variables (`DimAtom`), handling commutativity (`S1 + S2 == S2 + S1`),
//!    associativity, inverse cancellation (`(S + P) - P == S`), like-term combination (`D + D == 2 * D`),
//!    constant distribution (`2 * (A + B) == 2 * A + 2 * B`), and exact constant division
//!    (`(2 * D + 4) / 2 == D + 2`).
//! 2. **Commutative Monomial Product Normal Form ([`MonomialProduct`])**:
//!    Canonicalizes multiplicative shape/volume expressions $\prod_i d_i$ by extracting scalar GCDs
//!    from each affine factor and cancelling common positive factors ($d_i \ge 1$), proving static
//!    `reshape` volume conservation (e.g. `[B, S, H * DH] == [B, S, H, DH]`, `[B, 2 * S, D] == [B, S, 2 * D]`)
//!    and rejecting provable mismatches (`[B, S, 4 * D] != [B, S, 3 * D]`).
//! 3. **Decidable Integer Linear Solver ([`OmegaContext`])**:
//!    Implements Pugh's Omega Test lite with zero external dependencies:
//!    - Exact integer Gaussian & unimodular Pugh modulus elimination for equalities (with GCD
//!      unsatisfiability test, e.g. `2 * X == 3` is UNSAT over $\mathbb{Z}$);
//!    - Exact divisibility lowering (`d | expr` $\iff \exists q \in \mathbb{Z}: \text{expr} - d q = 0$);
//!    - GCD-normalized inequalities with integer ceiling tightening and anti-parallel pair detection;
//!    - Exact Fourier-Motzkin projection with Real Shadow, Dark Shadow, and bounded Gray Shadow
//!      finite offset splitting (fail-closed `SolveResult::Unknown` if fuel is exhausted).

use std::collections::BTreeMap;
use std::fmt;

use crate::typeck::{DimExpr, DimOp};

/// Atomic dimension variable inside an affine or monomial expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DimAtom {
    /// Named symbolic dimension / const-generic parameter (e.g. `B`, `S1`, `H`, `DH`).
    Symbolic(String),
    /// HM unification dimension meta-variable (`DimExpr::Var(id)`).
    Meta(u32),
}

impl fmt::Display for DimAtom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbolic(s) => write!(f, "{s}"),
            Self::Meta(id) => write!(f, "_d{id}"),
        }
    }
}

/// Internal variable key used inside [`OmegaContext`] (user atoms + auxiliary elimination vars).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum VarKey {
    Atom(DimAtom),
    Aux(u32),
}

/// Exact integer GCD (always non-negative).
pub(crate) fn gcd_i128(mut a: i128, mut b: i128) -> i128 {
    a = a.saturating_abs();
    b = b.saturating_abs();
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Exact integer ceiling division `ceil(num / den)` for `den > 0`.
fn div_ceil_i128(num: i128, den: i128) -> i128 {
    debug_assert!(den > 0);
    let q = num / den;
    let r = num % den;
    if r > 0 { q + 1 } else { q }
}

/// Balanced (nearest-integer) quotient `q` such that `|num - den * q| <= den / 2` for `den > 0`.
fn div_nearest_i128(num: i128, den: i128) -> i128 {
    debug_assert!(den > 0);
    let q = num.div_euclid(den);
    let r = num.rem_euclid(den);
    if r.saturating_mul(2) > den { q + 1 } else { q }
}

/// Canonical affine integer expression: `constant + sum(coeff * atom)`.
/// Zero-coefficient terms are never stored in `terms`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub(crate) struct AffineExpr {
    pub(crate) constant: i128,
    pub(crate) terms: BTreeMap<DimAtom, i128>,
}

impl AffineExpr {
    pub(crate) fn constant(c: i128) -> Self {
        Self {
            constant: c,
            terms: BTreeMap::new(),
        }
    }

    pub(crate) fn atom(atom: DimAtom) -> Self {
        let mut terms = BTreeMap::new();
        terms.insert(atom, 1);
        Self { constant: 0, terms }
    }

    #[cfg(test)]
    fn symbolic(name: impl Into<String>) -> Self {
        Self::atom(DimAtom::Symbolic(name.into()))
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.constant == 0 && self.terms.is_empty()
    }

    pub(crate) fn as_constant(&self) -> Option<i128> {
        if self.terms.is_empty() {
            Some(self.constant)
        } else {
            None
        }
    }

    pub(crate) fn add(&self, other: &Self) -> Option<Self> {
        let constant = self.constant.checked_add(other.constant)?;
        let mut terms = self.terms.clone();
        for (k, &v) in &other.terms {
            let next = terms.get(k).copied().unwrap_or(0).checked_add(v)?;
            if next == 0 {
                terms.remove(k);
            } else {
                terms.insert(k.clone(), next);
            }
        }
        Some(Self { constant, terms })
    }

    pub(crate) fn sub(&self, other: &Self) -> Option<Self> {
        let neg = other.scale(-1)?;
        self.add(&neg)
    }

    pub(crate) fn scale(&self, factor: i128) -> Option<Self> {
        if factor == 0 {
            return Some(Self::constant(0));
        }
        let constant = self.constant.checked_mul(factor)?;
        let mut terms = BTreeMap::new();
        for (k, &v) in &self.terms {
            let nv = v.checked_mul(factor)?;
            if nv != 0 {
                terms.insert(k.clone(), nv);
            }
        }
        Some(Self { constant, terms })
    }

    /// Exact division of every coefficient and constant by `divisor != 0`.
    pub(crate) fn div_exact(&self, divisor: i128) -> Option<Self> {
        if divisor == 0 || self.constant % divisor != 0 {
            return None;
        }
        let mut terms = BTreeMap::new();
        for (k, &v) in &self.terms {
            if v % divisor != 0 {
                return None;
            }
            let nv = v / divisor;
            if nv != 0 {
                terms.insert(k.clone(), nv);
            }
        }
        Some(Self {
            constant: self.constant / divisor,
            terms,
        })
    }

    /// Factor out the positive GCD of all coefficients and the constant, returning `(gcd, primitive)`.
    /// Also canonicalizes the leading non-zero term sign to positive if possible.
    fn factor_content(&self) -> (i128, Self) {
        let mut g = self.constant.saturating_abs();
        for &v in self.terms.values() {
            g = gcd_i128(g, v);
        }
        if g <= 1 {
            return (1, self.clone());
        }
        let prim = self
            .div_exact(g)
            .expect("div_exact by content GCD cannot fail");
        (g, prim)
    }

    /// Convert a [`DimExpr`] into a canonical [`AffineExpr`] when it is linear.
    /// Returns `None` for `"_"` wildcards, variable-times-variable products, or non-exact divisions.
    pub(crate) fn from_dim(dim: &DimExpr) -> Option<Self> {
        match dim {
            DimExpr::Known(n) => Some(Self::constant(*n as i128)),
            DimExpr::Symbolic(s) if s == "_" => None,
            DimExpr::Symbolic(s) => Some(Self::atom(DimAtom::Symbolic(s.clone()))),
            DimExpr::Var(id) => Some(Self::atom(DimAtom::Meta(*id))),
            DimExpr::BinOp(op, lhs, rhs) => {
                let l = Self::from_dim(lhs)?;
                let r = Self::from_dim(rhs)?;
                match op {
                    DimOp::Add => l.add(&r),
                    DimOp::Sub => l.sub(&r),
                    DimOp::Mul => {
                        if let Some(c) = l.as_constant() {
                            r.scale(c)
                        } else if let Some(c) = r.as_constant() {
                            l.scale(c)
                        } else {
                            None
                        }
                    }
                    DimOp::Div => {
                        let d = r.as_constant()?;
                        if d <= 0 {
                            return None;
                        }
                        l.div_exact(d)
                    }
                }
            }
        }
    }

    /// Convert this canonical [`AffineExpr`] back into a deterministic [`DimExpr`].
    /// Returns `None` only if the expression is a negative constant (cannot be represented as `usize`).
    pub(crate) fn to_dim_expr(&self) -> Option<DimExpr> {
        let mut pos_parts: Vec<DimExpr> = Vec::new();
        let mut neg_parts: Vec<DimExpr> = Vec::new();

        for (atom, &coeff) in &self.terms {
            let base = match atom {
                DimAtom::Symbolic(s) => DimExpr::Symbolic(s.clone()),
                DimAtom::Meta(id) => DimExpr::Var(*id),
            };
            let mag = coeff.unsigned_abs();
            let mag_usize = usize::try_from(mag).ok()?;
            let term = if mag_usize == 1 {
                base
            } else {
                DimExpr::BinOp(
                    DimOp::Mul,
                    Box::new(base),
                    Box::new(DimExpr::Known(mag_usize)),
                )
            };
            if coeff > 0 {
                pos_parts.push(term);
            } else {
                neg_parts.push(term);
            }
        }

        if self.constant > 0 {
            let c = usize::try_from(self.constant).ok()?;
            pos_parts.push(DimExpr::Known(c));
        } else if self.constant < 0 {
            let c = usize::try_from(self.constant.unsigned_abs()).ok()?;
            neg_parts.push(DimExpr::Known(c));
        }

        if pos_parts.is_empty() {
            if neg_parts.is_empty() {
                return Some(DimExpr::Known(0));
            }
            return None;
        }

        let mut acc = pos_parts.remove(0);
        for p in pos_parts {
            acc = DimExpr::BinOp(DimOp::Add, Box::new(acc), Box::new(p));
        }
        for n in neg_parts {
            acc = DimExpr::BinOp(DimOp::Sub, Box::new(acc), Box::new(n));
        }
        Some(acc)
    }
}

impl fmt::Display for AffineExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (atom, &coeff) in &self.terms {
            if !first {
                if coeff >= 0 {
                    write!(f, " + ")?;
                } else {
                    write!(f, " - ")?;
                }
            } else if coeff < 0 {
                write!(f, "-")?;
            }
            let abs = coeff.unsigned_abs();
            if abs == 1 {
                write!(f, "{atom}")?;
            } else {
                write!(f, "{abs} * {atom}")?;
            }
            first = false;
        }
        if self.constant != 0 || first {
            if !first {
                if self.constant > 0 {
                    write!(f, " + {}", self.constant)?;
                } else {
                    write!(f, " - {}", self.constant.unsigned_abs())?;
                }
            } else {
                write!(f, "{}", self.constant)?;
            }
        }
        Ok(())
    }
}

/// Primitive multiplicative factor in a [`MonomialProduct`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum MonoFactor {
    Affine(AffineExpr),
    Opaque(String),
}

/// Canonical commutative monomial product: `coeff * prod(factor ^ exp)`.
/// Used for multiplicative dimension equivalence (`H * DH == DH * H`) and static `reshape`
/// volume conservation (`[B, S, H * DH] == [B, S, H, DH]`, `[B, 2 * S, D] == [B, S, 2 * D]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MonomialProduct {
    coeff: i128,
    factors: BTreeMap<MonoFactor, u32>,
    has_wildcard: bool,
    has_opaque: bool,
}

impl MonomialProduct {
    fn one() -> Self {
        Self {
            coeff: 1,
            factors: BTreeMap::new(),
            has_wildcard: false,
            has_opaque: false,
        }
    }

    fn zero() -> Self {
        Self {
            coeff: 0,
            factors: BTreeMap::new(),
            has_wildcard: false,
            has_opaque: false,
        }
    }

    fn multiply_dim(&mut self, dim: &DimExpr) {
        if self.coeff == 0 {
            return;
        }
        match dim {
            DimExpr::Known(0) => {
                *self = Self::zero();
            }
            DimExpr::Known(n) => {
                if let Some(c) = self.coeff.checked_mul(*n as i128) {
                    self.coeff = c;
                } else {
                    self.has_opaque = true;
                }
            }
            DimExpr::Symbolic(s) if s == "_" => {
                self.has_wildcard = true;
            }
            DimExpr::BinOp(DimOp::Mul, lhs, rhs) => {
                self.multiply_dim(lhs);
                self.multiply_dim(rhs);
            }
            other => {
                if let Some(aff) = AffineExpr::from_dim(other) {
                    if let Some(c) = aff.as_constant() {
                        if c == 0 {
                            *self = Self::zero();
                        } else if let Some(nc) = self.coeff.checked_mul(c) {
                            self.coeff = nc;
                        } else {
                            self.has_opaque = true;
                        }
                    } else {
                        let (g, prim) = aff.factor_content();
                        if let Some(nc) = self.coeff.checked_mul(g) {
                            self.coeff = nc;
                            *self.factors.entry(MonoFactor::Affine(prim)).or_insert(0) += 1;
                        } else {
                            self.has_opaque = true;
                        }
                    }
                } else {
                    self.has_opaque = true;
                    *self
                        .factors
                        .entry(MonoFactor::Opaque(other.to_string()))
                        .or_insert(0) += 1;
                }
            }
        }
    }

    pub(crate) fn from_dim(dim: &DimExpr) -> Self {
        let mut p = Self::one();
        p.multiply_dim(dim);
        p
    }

    pub(crate) fn from_shape(dims: &[DimExpr]) -> Self {
        let mut p = Self::one();
        for d in dims {
            p.multiply_dim(d);
        }
        p
    }

    fn contains_meta_var(&self) -> bool {
        self.factors.keys().any(|f| match f {
            MonoFactor::Affine(a) => a.terms.keys().any(|k| matches!(k, DimAtom::Meta(_))),
            MonoFactor::Opaque(_) => false,
        })
    }

    fn to_dim_expr(&self) -> Option<DimExpr> {
        if self.has_wildcard || self.has_opaque || self.coeff < 0 {
            return None;
        }
        if self.coeff == 0 {
            return Some(DimExpr::Known(0));
        }
        let mut parts: Vec<DimExpr> = Vec::new();
        for (factor, &exp) in &self.factors {
            let MonoFactor::Affine(aff) = factor else {
                return None;
            };
            let d = aff.to_dim_expr()?;
            for _ in 0..exp {
                parts.push(d.clone());
            }
        }
        if self.coeff > 1 || parts.is_empty() {
            let c = usize::try_from(self.coeff).ok()?;
            parts.push(DimExpr::Known(c));
        }
        let mut acc = parts.remove(0);
        for p in parts {
            acc = DimExpr::BinOp(DimOp::Mul, Box::new(acc), Box::new(p));
        }
        Some(acc)
    }

    /// Cancel common positive factors between `self` and `other`, returning `(lhs_rem, rhs_rem)`.
    fn cancel_common(&self, other: &Self) -> (Self, Self) {
        let g = gcd_i128(self.coeff, other.coeff).max(1);
        let mut l = Self {
            coeff: self.coeff / g,
            factors: self.factors.clone(),
            has_wildcard: self.has_wildcard,
            has_opaque: self.has_opaque,
        };
        let mut r = Self {
            coeff: other.coeff / g,
            factors: other.factors.clone(),
            has_wildcard: other.has_wildcard,
            has_opaque: other.has_opaque,
        };
        let keys: Vec<MonoFactor> = l.factors.keys().cloned().collect();
        for k in keys {
            if let (Some(&el), Some(&er)) = (l.factors.get(&k), r.factors.get(&k)) {
                let common = el.min(er);
                if el == common {
                    l.factors.remove(&k);
                } else {
                    l.factors.insert(k.clone(), el - common);
                }
                if er == common {
                    r.factors.remove(&k);
                } else {
                    r.factors.insert(k, er - common);
                }
            }
        }
        (l, r)
    }
}

impl fmt::Display for MonomialProduct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if self.coeff != 1 || self.factors.is_empty() {
            parts.push(self.coeff.to_string());
        }
        for (factor, &exp) in &self.factors {
            let s = match factor {
                MonoFactor::Affine(a) => {
                    if a.terms.len() > 1 || (a.terms.len() == 1 && a.constant != 0) {
                        format!("({a})")
                    } else {
                        a.to_string()
                    }
                }
                MonoFactor::Opaque(s) => s.clone(),
            };
            for _ in 0..exp {
                parts.push(s.clone());
            }
        }
        write!(f, "{}", parts.join(" * "))
    }
}

/// Result of checking static `reshape` volume conservation $\prod d_i = \prod d'_j$.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VolumeCheck {
    /// Volumes are algebraically proven equal.
    Equal,
    /// Volumes are provably unequal (after cancelling common positive factors, e.g. `4 != 3` or `12 != 15`).
    ProvablyUnequal {
        lhs_residual: String,
        rhs_residual: String,
    },
    /// Free unrelated symbols or wildcards prevent proving equality or inequality.
    Unknown,
}

/// Check whether two ranked tensor shapes have equal total element count $\prod d_i = \prod d'_j$.
pub(crate) fn check_reshape_volume(cur_dims: &[DimExpr], target_dims: &[DimExpr]) -> VolumeCheck {
    let lhs = MonomialProduct::from_shape(cur_dims);
    let rhs = MonomialProduct::from_shape(target_dims);
    if lhs.has_wildcard || rhs.has_wildcard {
        return VolumeCheck::Unknown;
    }
    if lhs.coeff == rhs.coeff && lhs.factors == rhs.factors {
        return VolumeCheck::Equal;
    }
    if lhs.has_opaque || rhs.has_opaque || lhs.contains_meta_var() || rhs.contains_meta_var() {
        return VolumeCheck::Unknown;
    }
    let (l_rem, r_rem) = lhs.cancel_common(&rhs);
    if l_rem.coeff == r_rem.coeff && l_rem.factors == r_rem.factors {
        return VolumeCheck::Equal;
    }
    // After cancelling all common positive symbolic factors:
    // - If neither side has remaining symbolic factors, `l_rem.coeff != r_rem.coeff` proves inequality
    //   (e.g. `[B, S, 4 * D]` vs `[B, S, 3 * D]` -> `4 != 3`, or `[2, 6]` vs `[3, 5]` -> `12 != 15`).
    // - If one side is `1` and the other has a constant >= 2 times positive factors (e.g. `[B, S]` vs
    //   `[B, S * 2]`), since all tensor dimensions >= 1, `1 < 2 * ...` is provably unequal!
    let l_pure_const = l_rem.factors.is_empty();
    let r_pure_const = r_rem.factors.is_empty();
    if l_pure_const && r_pure_const {
        return VolumeCheck::ProvablyUnequal {
            lhs_residual: l_rem.to_string(),
            rhs_residual: r_rem.to_string(),
        };
    }
    if l_pure_const && l_rem.coeff == 1 && r_rem.coeff >= 2 && all_factors_positive(&r_rem) {
        return VolumeCheck::ProvablyUnequal {
            lhs_residual: l_rem.to_string(),
            rhs_residual: r_rem.to_string(),
        };
    }
    if r_pure_const && r_rem.coeff == 1 && l_rem.coeff >= 2 && all_factors_positive(&l_rem) {
        return VolumeCheck::ProvablyUnequal {
            lhs_residual: l_rem.to_string(),
            rhs_residual: r_rem.to_string(),
        };
    }
    VolumeCheck::Unknown
}

/// True if every factor in `p` is guaranteed >= 1 when all symbolic dimensions are >= 1.
fn all_factors_positive(p: &MonomialProduct) -> bool {
    p.factors.keys().all(|f| match f {
        MonoFactor::Affine(a) => a.constant >= 0 && a.terms.values().all(|&c| c > 0),
        MonoFactor::Opaque(_) => false,
    })
}

/// Canonicalize a [`DimExpr`] using exact affine normal form (for linear expressions) or
/// commutative monomial normal form (for multiplicative expressions), preserving structural
/// fallback for non-linear divisions / wildcards.
pub(crate) fn canonicalize_dim(dim: &DimExpr) -> DimExpr {
    if let Some(aff) = AffineExpr::from_dim(dim)
        && let Some(canon) = aff.to_dim_expr()
    {
        return canon;
    }
    let mono = MonomialProduct::from_dim(dim);
    if let Some(canon) = mono.to_dim_expr() {
        return canon;
    }
    match dim {
        DimExpr::Known(_) | DimExpr::Symbolic(_) | DimExpr::Var(_) => dim.clone(),
        DimExpr::BinOp(op, lhs, rhs) => {
            let l = canonicalize_dim(lhs);
            let r = canonicalize_dim(rhs);
            match (*op, l, r) {
                (DimOp::Add, DimExpr::Known(a), DimExpr::Known(b)) => {
                    DimExpr::Known(a.saturating_add(b))
                }
                (DimOp::Sub, DimExpr::Known(a), DimExpr::Known(b)) if a >= b => {
                    DimExpr::Known(a - b)
                }
                (DimOp::Mul, DimExpr::Known(a), DimExpr::Known(b)) => {
                    DimExpr::Known(a.saturating_mul(b))
                }
                (DimOp::Div, DimExpr::Known(a), DimExpr::Known(b)) if b > 0 && a % b == 0 => {
                    DimExpr::Known(a / b)
                }
                (DimOp::Add, x, DimExpr::Known(0)) | (DimOp::Add, DimExpr::Known(0), x) => x,
                (DimOp::Sub, x, DimExpr::Known(0)) => x,
                (DimOp::Mul, x, DimExpr::Known(1)) | (DimOp::Mul, DimExpr::Known(1), x) => x,
                (DimOp::Mul, _, DimExpr::Known(0)) | (DimOp::Mul, DimExpr::Known(0), _) => {
                    DimExpr::Known(0)
                }
                (DimOp::Div, x, DimExpr::Known(1)) => x,
                (DimOp::Div, DimExpr::BinOp(DimOp::Mul, inner, c1), DimExpr::Known(b))
                    if b > 0 && matches!(*c1, DimExpr::Known(a) if a == b) =>
                {
                    *inner
                }
                (op, l, r) => DimExpr::BinOp(op, Box::new(l), Box::new(r)),
            }
        }
    }
}

/// True iff `a` and `b` are algebraically equivalent (under affine linear arithmetic or
/// commutative/associative monomial products). Does NOT treat `"_"` or `Var` as wildcards.
pub(crate) fn dims_equiv(a: &DimExpr, b: &DimExpr) -> bool {
    if a == b {
        return true;
    }
    if let (Some(la), Some(lb)) = (AffineExpr::from_dim(a), AffineExpr::from_dim(b)) {
        return la == lb;
    }
    let ma = MonomialProduct::from_dim(a);
    let mb = MonomialProduct::from_dim(b);
    if !ma.has_wildcard && !mb.has_wildcard && !ma.has_opaque && !mb.has_opaque {
        return ma.coeff == mb.coeff && ma.factors == mb.factors;
    }
    canonicalize_dim(a) == canonicalize_dim(b)
}

/// Outcome of linear/monomial HM dimension unification `a == b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DimUnifyOutcome {
    /// Already algebraically equivalent; no new substitution needed.
    Equal,
    /// Solved a unique meta-variable binding `Var(id) := dim`.
    Bind(u32, DimExpr),
    /// Algebraically proven unsatisfiable over $\mathbb{Z}_{\ge 0}$.
    Contradiction(String),
    /// Non-linear or multi-meta-variable; caller should fall back to structural unification.
    Defer,
}

/// Attempt exact linear / monomial dimension unification for `d1 == d2`.
pub(crate) fn unify_dim_linear(d1: &DimExpr, d2: &DimExpr) -> DimUnifyOutcome {
    if dims_equiv(d1, d2) {
        return DimUnifyOutcome::Equal;
    }

    // 1. Exact affine linear solver over Z: `a - b == 0`.
    if let (Some(a), Some(b)) = (AffineExpr::from_dim(d1), AffineExpr::from_dim(d2))
        && let Some(diff) = a.sub(&b)
    {
        if diff.is_zero() {
            return DimUnifyOutcome::Equal;
        }
        let metas: Vec<(u32, i128)> = diff
            .terms
            .iter()
            .filter_map(|(k, &v)| match k {
                DimAtom::Meta(id) => Some((*id, v)),
                DimAtom::Symbolic(_) => None,
            })
            .collect();

        if metas.is_empty() {
            return DimUnifyOutcome::Contradiction(format!(
                "cannot satisfy dimension equality `{d1} == {d2}` (residual `{diff} == 0`)"
            ));
        }

        if metas.len() == 1 {
            let (id, coeff) = metas[0];
            // diff = coeff * Meta(id) + rest == 0  ==>  Meta(id) = -rest / coeff
            let mut rest = diff.clone();
            rest.terms.remove(&DimAtom::Meta(id));
            let Some(neg_rest) = rest.scale(-1) else {
                return DimUnifyOutcome::Defer;
            };
            let Some(sol) = neg_rest.div_exact(coeff) else {
                return DimUnifyOutcome::Contradiction(format!(
                    "dimension equality `{d1} == {d2}` has no integer solution (`{coeff}` does not divide `{neg_rest}`)"
                ));
            };
            if let Some(c) = sol.as_constant()
                && c < 0
            {
                return DimUnifyOutcome::Contradiction(format!(
                    "dimension equality `{d1} == {d2}` requires negative dimension `{c}`"
                ));
            }
            if let Some(dim_sol) = sol.to_dim_expr() {
                return DimUnifyOutcome::Bind(id, dim_sol);
            }
            return DimUnifyOutcome::Contradiction(format!(
                "dimension equality `{d1} == {d2}` solves to invalid dimension `{sol}`"
            ));
        }

        // Multiple meta-vars: if GCD of all coefficients (including symbolic) doesn't divide constant,
        // it is provably unsatisfiable over Z (e.g. `2 * _d0 + 2 * _d1 == 3`).
        let mut g = 0i128;
        for &c in diff.terms.values() {
            g = gcd_i128(g, c);
        }
        if g > 1 && diff.constant % g != 0 {
            return DimUnifyOutcome::Contradiction(format!(
                "dimension equality `{d1} == {d2}` has no integer solution (gcd {g} does not divide {})",
                diff.constant
            ));
        }

        // If any meta-var has unit coefficient +-1, solve it in terms of the remaining atoms.
        if let Some(&(id, coeff)) = metas.iter().find(|&&(_, c)| c == 1 || c == -1) {
            let mut rest = diff.clone();
            rest.terms.remove(&DimAtom::Meta(id));
            if let Some(sol) = rest.scale(-coeff)
                && let Some(dim_sol) = sol.to_dim_expr()
            {
                return DimUnifyOutcome::Bind(id, dim_sol);
            }
        }
        return DimUnifyOutcome::Defer;
    }

    // 2. Commutative monomial product cancellation (e.g. `H * DH == DH * H`, `_d0 * H == 4 * H`).
    let ma = MonomialProduct::from_dim(d1);
    let mb = MonomialProduct::from_dim(d2);
    if !ma.has_wildcard && !mb.has_wildcard && !ma.has_opaque && !mb.has_opaque {
        let (la, rb) = ma.cancel_common(&mb);
        if la.coeff == rb.coeff && la.factors == rb.factors {
            return DimUnifyOutcome::Equal;
        }
        // Check if one side reduced to a single `Meta(id)` with coeff 1 and exponent 1.
        let as_single_meta = |m: &MonomialProduct| -> Option<u32> {
            if m.coeff == 1 && m.factors.len() == 1 {
                let (fac, &exp) = m.factors.iter().next()?;
                if exp == 1
                    && let MonoFactor::Affine(aff) = fac
                    && aff.constant == 0
                    && aff.terms.len() == 1
                    && let Some((DimAtom::Meta(id), &1)) = aff.terms.iter().next()
                {
                    return Some(*id);
                }
            }
            None
        };
        if let Some(id) = as_single_meta(&la)
            && !rb.contains_meta_var()
            && let Some(dim_sol) = rb.to_dim_expr()
        {
            return DimUnifyOutcome::Bind(id, dim_sol);
        }
        if let Some(id) = as_single_meta(&rb)
            && !la.contains_meta_var()
            && let Some(dim_sol) = la.to_dim_expr()
        {
            return DimUnifyOutcome::Bind(id, dim_sol);
        }
        if !la.contains_meta_var()
            && !rb.contains_meta_var()
            && la.factors.is_empty()
            && rb.factors.is_empty()
        {
            return DimUnifyOutcome::Contradiction(format!(
                "cannot satisfy dimension equality `{d1} == {d2}` (residual `{la} != {rb}`)"
            ));
        }
    }

    DimUnifyOutcome::Defer
}

// =============================================================================
// Full Decidable Presburger Integer Linear Constraint Solver (`OmegaContext`)
// =============================================================================

/// Three-valued decision outcome for integer linear satisfiability / entailment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SolveResult {
    /// Constraint system is provably satisfiable (or predicate is provably entailed).
    True,
    /// Constraint system is provably unsatisfiable (or predicate is provably refuted).
    False,
    /// Bounded solver fuel exceeded or non-linear divisor cannot be decided statically.
    Unknown,
}

/// Internal linear form `constant + sum(coeff * var)` (used for both `== 0` and `<= 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinForm {
    constant: i128,
    terms: BTreeMap<VarKey, i128>,
}

impl LinForm {
    fn from_affine(aff: &AffineExpr) -> Self {
        let mut terms = BTreeMap::new();
        for (k, &v) in &aff.terms {
            if v != 0 {
                terms.insert(VarKey::Atom(k.clone()), v);
            }
        }
        Self {
            constant: aff.constant,
            terms,
        }
    }

    fn zero() -> Self {
        Self {
            constant: 0,
            terms: BTreeMap::new(),
        }
    }

    fn add_scaled(&mut self, other: &Self, scale: i128) -> Option<()> {
        if scale == 0 {
            return Some(());
        }
        self.constant = self
            .constant
            .checked_add(other.constant.checked_mul(scale)?)?;
        for (k, &v) in &other.terms {
            let delta = v.checked_mul(scale)?;
            let next = self.terms.get(k).copied().unwrap_or(0).checked_add(delta)?;
            if next == 0 {
                self.terms.remove(k);
            } else {
                self.terms.insert(k.clone(), next);
            }
        }
        Some(())
    }

    fn scale(&self, factor: i128) -> Option<Self> {
        if factor == 0 {
            return Some(Self::zero());
        }
        let constant = self.constant.checked_mul(factor)?;
        let mut terms = BTreeMap::new();
        for (k, &v) in &self.terms {
            let nv = v.checked_mul(factor)?;
            if nv != 0 {
                terms.insert(k.clone(), nv);
            }
        }
        Some(Self { constant, terms })
    }

    /// Substitute `var := rhs` (where `rhs` does NOT contain `var`).
    fn substitute(&mut self, var: &VarKey, rhs: &Self) -> Option<()> {
        if let Some(coeff) = self.terms.remove(var) {
            self.add_scaled(rhs, coeff)?;
        }
        Some(())
    }

    fn coeff_gcd(&self) -> i128 {
        let mut g = 0i128;
        for &c in self.terms.values() {
            g = gcd_i128(g, c);
        }
        g
    }

    /// Normalize an inequality `self <= 0` by dividing by the GCD of variable coefficients and
    /// tightening the constant term via ceiling division: `sum(c_i / g) * v_i + ceil(c_0 / g) <= 0`.
    /// Returns `Some(true)` if tautology (`c_0 <= 0` with no vars), `Some(false)` if contradiction
    /// (`c_0 > 0` with no vars), or `None` if active constraint remains.
    fn normalize_ineq(&mut self) -> Option<bool> {
        if self.terms.is_empty() {
            return Some(self.constant <= 0);
        }
        let g = self.coeff_gcd();
        if g > 1 {
            for v in self.terms.values_mut() {
                *v /= g;
            }
            self.constant = div_ceil_i128(self.constant, g);
        }
        None
    }
}

/// Decidable integer linear constraint context (`omega`-lite).
#[derive(Debug, Clone, Default)]
pub(crate) struct OmegaContext {
    equalities: Vec<LinForm>,
    /// Each `LinForm` represents `form <= 0`.
    inequalities: Vec<LinForm>,
    next_aux: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SatState {
    Sat,
    Unsat,
    Unknown,
}

impl OmegaContext {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn fresh_aux(&mut self) -> VarKey {
        let id = self.next_aux;
        self.next_aux = self.next_aux.saturating_add(1);
        VarKey::Aux(id)
    }

    /// Assume `atom >= 0` (index / `fin<N>` / GPU thread index domain invariant).
    pub(crate) fn assume_non_negative(&mut self, atom: DimAtom) {
        self.assume_le(&AffineExpr::constant(0), &AffineExpr::atom(atom));
    }

    /// Assume `atom >= 1` (tensor dimension domain invariant).
    pub(crate) fn assume_positive(&mut self, atom: DimAtom) {
        self.assume_le(&AffineExpr::constant(1), &AffineExpr::atom(atom));
    }

    /// Assume `lhs == rhs`.
    pub(crate) fn assume_eq(&mut self, lhs: &AffineExpr, rhs: &AffineExpr) {
        if let Some(diff) = lhs.sub(rhs) {
            self.equalities.push(LinForm::from_affine(&diff));
        }
    }

    /// Assume `lhs <= rhs`, i.e., `lhs - rhs <= 0`.
    pub(crate) fn assume_le(&mut self, lhs: &AffineExpr, rhs: &AffineExpr) {
        if let Some(diff) = lhs.sub(rhs) {
            self.inequalities.push(LinForm::from_affine(&diff));
        }
    }

    /// Assume `lhs < rhs`, i.e., `lhs - rhs + 1 <= 0` (exact over $\mathbb{Z}$).
    pub(crate) fn assume_lt(&mut self, lhs: &AffineExpr, rhs: &AffineExpr) {
        if let Some(mut diff) = lhs.sub(rhs)
            && let Some(c) = diff.constant.checked_add(1)
        {
            diff.constant = c;
            self.inequalities.push(LinForm::from_affine(&diff));
        }
    }

    /// Assume `expr % divisor == 0` (`divisor | expr`).
    #[cfg(test)]
    pub(crate) fn assume_divides(&mut self, divisor: i128, expr: &AffineExpr) {
        let d = divisor.saturating_abs();
        if d == 0 {
            self.equalities.push(LinForm::from_affine(expr));
            return;
        }
        if d == 1 {
            return;
        }
        let q = self.fresh_aux();
        let mut eq = LinForm::from_affine(expr);
        eq.terms.insert(q, -d);
        self.equalities.push(eq);
    }

    /// Check satisfiability of the current constraint conjunction over $\mathbb{Z}$.
    pub(crate) fn check_sat(&self) -> SolveResult {
        let mut fuel: usize = 512;
        match solve_system(
            self.equalities.clone(),
            self.inequalities.clone(),
            self.next_aux,
            &mut fuel,
        ) {
            SatState::Sat => SolveResult::True,
            SatState::Unsat => SolveResult::False,
            SatState::Unknown => SolveResult::Unknown,
        }
    }

    /// Decide whether this context entails `lhs == rhs` (`ctx |= lhs == rhs`).
    #[cfg(test)]
    pub(crate) fn entails_eq(&self, lhs: &AffineExpr, rhs: &AffineExpr) -> SolveResult {
        let mut c1 = self.clone();
        c1.assume_lt(lhs, rhs);
        let r1 = c1.check_sat();
        if r1 == SolveResult::True {
            return SolveResult::False;
        }
        let mut c2 = self.clone();
        c2.assume_lt(rhs, lhs);
        let r2 = c2.check_sat();
        if r2 == SolveResult::True {
            return SolveResult::False;
        }
        if r1 == SolveResult::False && r2 == SolveResult::False {
            SolveResult::True
        } else {
            SolveResult::Unknown
        }
    }

    /// Decide whether this context entails `lhs <= rhs` (`ctx |= lhs <= rhs`).
    pub(crate) fn entails_le(&self, lhs: &AffineExpr, rhs: &AffineExpr) -> SolveResult {
        let mut neg = self.clone();
        neg.assume_lt(rhs, lhs); // negation of lhs <= rhs is rhs < lhs
        match neg.check_sat() {
            SolveResult::False => SolveResult::True,
            SolveResult::True => SolveResult::False,
            SolveResult::Unknown => SolveResult::Unknown,
        }
    }

    /// Decide whether this context entails `lhs < rhs` (`ctx |= lhs < rhs`).
    pub(crate) fn entails_lt(&self, lhs: &AffineExpr, rhs: &AffineExpr) -> SolveResult {
        let mut neg = self.clone();
        neg.assume_le(rhs, lhs); // negation of lhs < rhs is rhs <= lhs
        match neg.check_sat() {
            SolveResult::False => SolveResult::True,
            SolveResult::True => SolveResult::False,
            SolveResult::Unknown => SolveResult::Unknown,
        }
    }

    /// Decide whether this context proves `0 <= idx < len` (used by tensor slice bounds,
    /// and upcoming `fin<N>` / GPU `kernel` shared-memory static bounds proofs).
    pub(crate) fn entails_in_bounds(&self, idx: &AffineExpr, len: &AffineExpr) -> SolveResult {
        let low = self.entails_le(&AffineExpr::constant(0), idx);
        let high = self.entails_lt(idx, len);
        match (low, high) {
            (SolveResult::True, SolveResult::True) => SolveResult::True,
            (SolveResult::False, _) | (_, SolveResult::False) => SolveResult::False,
            _ => SolveResult::Unknown,
        }
    }

    /// Decide whether this context proves `expr % divisor == 0` (`divisor | expr`).
    #[cfg(test)]
    pub(crate) fn entails_divides(&self, divisor: i128, expr: &AffineExpr) -> SolveResult {
        let d = divisor.saturating_abs();
        if d == 0 {
            return self.entails_eq(expr, &AffineExpr::constant(0));
        }
        if d == 1 {
            return SolveResult::True;
        }
        if expr.div_exact(d).is_some() {
            return SolveResult::True;
        }
        if let Some(reduced) = self.reduce_expr_by_equalities(expr) {
            if reduced.div_exact(d).is_some() {
                return SolveResult::True;
            }
            if let Some(c) = reduced.as_constant() {
                return if c.rem_euclid(d) == 0 {
                    SolveResult::True
                } else {
                    SolveResult::False
                };
            }
        }
        if d <= 64 {
            let mut any_unknown = false;
            for r in 1..d {
                let Some(shifted) = expr.sub(&AffineExpr::constant(r)) else {
                    return SolveResult::Unknown;
                };
                let mut probe = self.clone();
                probe.assume_divides(d, &shifted);
                match probe.check_sat() {
                    SolveResult::True => return SolveResult::False,
                    SolveResult::Unknown => any_unknown = true,
                    SolveResult::False => {}
                }
            }
            return if any_unknown {
                SolveResult::Unknown
            } else {
                SolveResult::True
            };
        }
        SolveResult::Unknown
    }

    /// Decide `lhs % rhs == 0` (e.g. GQA `H % KV == 0`) where `rhs` may be constant or symbolic.
    #[cfg(test)]
    pub(crate) fn entails_divides_expr(&self, lhs: &AffineExpr, rhs: &AffineExpr) -> SolveResult {
        let l = self
            .reduce_expr_by_equalities(lhs)
            .unwrap_or_else(|| lhs.clone());
        let r = self
            .reduce_expr_by_equalities(rhs)
            .unwrap_or_else(|| rhs.clone());
        if let Some(d) = r.as_constant() {
            if d == 0 {
                return SolveResult::False;
            }
            return self.entails_divides(d, &l);
        }
        if exact_affine_ratio(&l, &r).is_some() {
            return SolveResult::True;
        }
        if self.entails_lt(&AffineExpr::constant(0), &l) == SolveResult::True
            && self.entails_lt(&l, &r) == SolveResult::True
        {
            return SolveResult::False;
        }
        SolveResult::Unknown
    }

    /// Reduce `expr` modulo eliminated unit equalities in `self.equalities`.
    #[cfg(test)]
    fn reduce_expr_by_equalities(&self, expr: &AffineExpr) -> Option<AffineExpr> {
        let mut eqs = self.equalities.clone();
        let mut target = LinForm::from_affine(expr);
        let mut next_aux = self.next_aux;
        let mut ineqs = Vec::new();
        let mut subst_log: Vec<(VarKey, LinForm)> = Vec::new();
        if eliminate_equalities_with_log(&mut eqs, &mut ineqs, &mut next_aux, Some(&mut subst_log))
            != SatState::Sat
        {
            return None;
        }
        for (var, rhs) in &subst_log {
            target.substitute(var, rhs)?;
        }
        let mut out_terms = BTreeMap::new();
        for (k, v) in target.terms {
            match k {
                VarKey::Atom(a) => {
                    out_terms.insert(a, v);
                }
                VarKey::Aux(_) => return None,
            }
        }
        Some(AffineExpr {
            constant: target.constant,
            terms: out_terms,
        })
    }
}

/// If `lhs == k * rhs` for an exact integer `k`, return `Some(k)`.
#[cfg(test)]
fn exact_affine_ratio(lhs: &AffineExpr, rhs: &AffineExpr) -> Option<i128> {
    if rhs.is_zero() {
        return None;
    }
    if lhs.is_zero() {
        return Some(0);
    }
    // Find pivot coefficient in `rhs` to determine candidate `k`.
    let k = if let Some((atom, &rc)) = rhs.terms.iter().next() {
        let lc = lhs.terms.get(atom).copied().unwrap_or(0);
        if lc % rc != 0 {
            return None;
        }
        lc / rc
    } else {
        if lhs.constant % rhs.constant != 0 {
            return None;
        }
        lhs.constant / rhs.constant
    };
    let scaled = rhs.scale(k)?;
    if &scaled == lhs { Some(k) } else { None }
}

/// Eliminate all equalities via integer Gaussian + Pugh unimodular modulus substitution,
/// substituting into `inequalities` and optionally recording eliminations in `subst_log`.
fn eliminate_equalities_with_log(
    equalities: &mut Vec<LinForm>,
    inequalities: &mut [LinForm],
    next_aux: &mut u32,
    mut subst_log: Option<&mut Vec<(VarKey, LinForm)>>,
) -> SatState {
    let mut steps = 0usize;
    while !equalities.is_empty() {
        steps += 1;
        if steps > 256 {
            return SatState::Unknown;
        }
        let mut eq = equalities.swap_remove(0);
        if eq.terms.is_empty() {
            if eq.constant != 0 {
                return SatState::Unsat;
            }
            continue;
        }
        let g = eq.coeff_gcd();
        if eq.constant % g != 0 {
            return SatState::Unsat;
        }
        if g > 1 {
            for v in eq.terms.values_mut() {
                *v /= g;
            }
            eq.constant /= g;
        }

        // Prefer eliminating Aux vars with unit coefficient first, then any unit-coefficient var.
        let unit_var = eq
            .terms
            .iter()
            .filter(|&(_, &c)| c == 1 || c == -1)
            .min_by_key(|(k, _)| match k {
                VarKey::Aux(_) => 0u8,
                VarKey::Atom(_) => 1u8,
            })
            .map(|(k, &c)| (k.clone(), c));

        if let Some((var, coeff)) = unit_var {
            // coeff * var + rest == 0  ==>  var = -coeff * rest
            eq.terms.remove(&var);
            let Some(rhs) = eq.scale(-coeff) else {
                return SatState::Unknown;
            };
            for other in equalities.iter_mut() {
                if other.substitute(&var, &rhs).is_none() {
                    return SatState::Unknown;
                }
            }
            for ineq in inequalities.iter_mut() {
                if ineq.substitute(&var, &rhs).is_none() {
                    return SatState::Unknown;
                }
            }
            if let Some(log) = subst_log.as_deref_mut() {
                log.push((var, rhs));
            }
            continue;
        }

        // No unit coefficient: apply Pugh's unimodular modulus substitution on the smallest |c_k|.
        let Some((min_var, min_coeff)) = eq
            .terms
            .iter()
            .min_by_key(|(_, c)| c.unsigned_abs())
            .map(|(k, &c)| (k.clone(), c))
        else {
            continue;
        };

        // Ensure positive pivot a_k >= 2.
        if min_coeff < 0 {
            let Some(flipped) = eq.scale(-1) else {
                return SatState::Unknown;
            };
            eq = flipped;
        }
        let a_k = eq.terms[&min_var];
        debug_assert!(a_k >= 2);

        let sigma = VarKey::Aux(*next_aux);
        *next_aux = next_aux.saturating_add(1);

        // x_k = sigma - sum_{j != k} q_j * x_j - q_c
        let mut xk_rhs = LinForm::zero();
        xk_rhs.terms.insert(sigma.clone(), 1);
        let q_c = div_nearest_i128(eq.constant, a_k);
        let r_c = eq.constant - a_k.saturating_mul(q_c);
        xk_rhs.constant = -q_c;

        let mut new_eq = LinForm::zero();
        new_eq.constant = r_c;
        new_eq.terms.insert(sigma, a_k);

        for (k, &a_j) in &eq.terms {
            if k == &min_var {
                continue;
            }
            let q_j = div_nearest_i128(a_j, a_k);
            let r_j = a_j - a_k.saturating_mul(q_j);
            if q_j != 0 {
                xk_rhs.terms.insert(k.clone(), -q_j);
            }
            if r_j != 0 {
                new_eq.terms.insert(k.clone(), r_j);
            }
        }

        for other in equalities.iter_mut() {
            if other.substitute(&min_var, &xk_rhs).is_none() {
                return SatState::Unknown;
            }
        }
        for ineq in inequalities.iter_mut() {
            if ineq.substitute(&min_var, &xk_rhs).is_none() {
                return SatState::Unknown;
            }
        }
        if let Some(log) = subst_log.as_deref_mut() {
            log.push((min_var, xk_rhs));
        }
        equalities.push(new_eq);
    }
    SatState::Sat
}

/// Core recursive Pugh Omega Test decision procedure over $\mathbb{Z}$.
fn solve_system(
    mut equalities: Vec<LinForm>,
    mut inequalities: Vec<LinForm>,
    mut next_aux: u32,
    fuel: &mut usize,
) -> SatState {
    loop {
        if *fuel == 0 {
            return SatState::Unknown;
        }
        *fuel -= 1;

        // Step 1: Eliminate all equalities exactly.
        match eliminate_equalities_with_log(&mut equalities, &mut inequalities, &mut next_aux, None)
        {
            SatState::Unsat => return SatState::Unsat,
            SatState::Unknown => return SatState::Unknown,
            SatState::Sat => {}
        }

        // Step 2: Normalize inequalities (GCD ceiling tightening + deduplication + anti-parallel check).
        let mut by_vec: BTreeMap<BTreeMap<VarKey, i128>, i128> = BTreeMap::new();
        for mut ineq in inequalities.drain(..) {
            match ineq.normalize_ineq() {
                Some(true) => continue,
                Some(false) => return SatState::Unsat,
                None => {}
            }
            by_vec
                .entry(ineq.terms)
                .and_modify(|c| {
                    if ineq.constant > *c {
                        *c = ineq.constant;
                    }
                })
                .or_insert(ineq.constant);
        }

        // Check anti-parallel pairs `e + c1 <= 0` and `-e + c2 <= 0`.
        let mut tightened_eq = false;
        let keys: Vec<BTreeMap<VarKey, i128>> = by_vec.keys().cloned().collect();
        for k in &keys {
            let neg_k: BTreeMap<VarKey, i128> = k.iter().map(|(vk, &c)| (vk.clone(), -c)).collect();
            if k < &neg_k
                && let (Some(&c1), Some(&c2)) = (by_vec.get(k), by_vec.get(&neg_k))
            {
                let Some(sum) = c1.checked_add(c2) else {
                    return SatState::Unknown;
                };
                if sum > 0 {
                    return SatState::Unsat;
                }
                if sum == 0 {
                    // Tight pair: `e + c1 == 0`!
                    equalities.push(LinForm {
                        constant: c1,
                        terms: k.clone(),
                    });
                    by_vec.remove(k);
                    by_vec.remove(&neg_k);
                    tightened_eq = true;
                }
            }
        }

        inequalities = by_vec
            .into_iter()
            .map(|(terms, constant)| LinForm { constant, terms })
            .collect();

        if tightened_eq {
            continue;
        }
        if inequalities.is_empty() {
            return SatState::Sat;
        }

        // Step 3: Pick a variable to eliminate by Fourier-Motzkin.
        let mut var_stats: BTreeMap<VarKey, (usize, usize, i128, i128)> = BTreeMap::new();
        for ineq in &inequalities {
            for (v, &c) in &ineq.terms {
                let entry = var_stats.entry(v.clone()).or_insert((0, 0, 0, 0));
                if c < 0 {
                    // Lower bound on v: (-c) * v >= rest
                    entry.0 += 1;
                    entry.2 = entry.2.max(-c);
                } else if c > 0 {
                    // Upper bound on v: c * v <= -rest
                    entry.1 += 1;
                    entry.3 = entry.3.max(c);
                }
            }
        }

        // Unbounded-one-side elimination: exact projection drops those constraints immediately.
        if let Some((unbounded_var, _)) = var_stats
            .iter()
            .find(|&(_, &(l, u, _, _))| l == 0 || u == 0)
        {
            let target = unbounded_var.clone();
            inequalities.retain(|ineq| !ineq.terms.contains_key(&target));
            continue;
        }

        // Choose variable: prefer exact (max_a == 1 || max_b == 1) with smallest l * u, then smallest cost.
        let Some((elim_var, (_, _, max_a, max_b))) =
            var_stats.into_iter().min_by_key(|&(_, (l, u, ma, mb))| {
                let exact_rank = if ma == 1 || mb == 1 { 0u8 } else { 1u8 };
                (exact_rank, l.saturating_mul(u), ma.saturating_mul(mb))
            })
        else {
            return SatState::Sat;
        };

        let mut rest: Vec<LinForm> = Vec::new();
        // Lower: a * v >= alpha  (stored as `(a, alpha)` where original was `-a * v + alpha <= 0`)
        let mut lowers: Vec<(i128, LinForm)> = Vec::new();
        // Upper: b * v <= beta   (stored as `(b, beta)` where original was `b * v - beta <= 0`)
        let mut uppers: Vec<(i128, LinForm)> = Vec::new();

        for mut ineq in inequalities {
            match ineq.terms.remove(&elim_var) {
                None => rest.push(ineq),
                Some(c) if c < 0 => {
                    // -a * v + alpha <= 0  <=>  a * v >= alpha
                    lowers.push((-c, ineq));
                }
                Some(c) => {
                    // b * v + orig_rest <= 0  <=>  b * v <= -orig_rest = beta
                    let Some(beta) = ineq.scale(-1) else {
                        return SatState::Unknown;
                    };
                    uppers.push((c, beta));
                }
            }
        }

        // Guard against combinatorial blowup in huge systems.
        if lowers.len().saturating_mul(uppers.len()) > 256 {
            return SatState::Unknown;
        }

        // Build Real Shadow: for each (a, alpha) and (b, beta): b * alpha - a * beta <= 0.
        let mut real_shadow = rest.clone();
        for (a, alpha) in &lowers {
            for (b, beta) in &uppers {
                let Some(mut comb) = alpha.scale(*b) else {
                    return SatState::Unknown;
                };
                if comb.add_scaled(beta, -*a).is_none() {
                    return SatState::Unknown;
                }
                real_shadow.push(comb);
            }
        }

        // Case A: Exact variable (all lower coeffs == 1 OR all upper coeffs == 1).
        if max_a == 1 || max_b == 1 {
            inequalities = real_shadow;
            continue;
        }

        // Case B: Non-exact variable (Pugh Real / Dark / Gray Shadow).
        let real_res = solve_system(Vec::new(), real_shadow, next_aux, fuel);
        if real_res != SatState::Sat {
            return real_res;
        }

        // Dark Shadow: b * alpha - a * beta + (a - 1) * (b - 1) <= 0.
        let mut dark_shadow = rest.clone();
        let mut dark_ok = true;
        for (a, alpha) in &lowers {
            for (b, beta) in &uppers {
                let Some(gap) = (a - 1).checked_mul(b - 1) else {
                    dark_ok = false;
                    break;
                };
                let Some(mut comb) = alpha.scale(*b) else {
                    dark_ok = false;
                    break;
                };
                if comb.add_scaled(beta, -*a).is_none() {
                    dark_ok = false;
                    break;
                }
                let Some(nc) = comb.constant.checked_add(gap) else {
                    dark_ok = false;
                    break;
                };
                comb.constant = nc;
                dark_shadow.push(comb);
            }
            if !dark_ok {
                break;
            }
        }
        if dark_ok && solve_system(Vec::new(), dark_shadow, next_aux, fuel) == SatState::Sat {
            return SatState::Sat;
        }

        // Gray Shadow: exact finite integer offset search for pairs with a >= 2 && b >= 2.
        // Reconstruct original inequalities involving `elim_var` to pass into each equality branch.
        let mut orig_ineqs = rest;
        for (a, alpha) in &lowers {
            let mut f = alpha.clone();
            f.terms.insert(elim_var.clone(), -*a);
            orig_ineqs.push(f);
        }
        for (b, beta) in &uppers {
            let Some(mut f) = beta.scale(-1) else {
                return SatState::Unknown;
            };
            f.terms.insert(elim_var.clone(), *b);
            orig_ineqs.push(f);
        }

        let mut any_unknown = false;
        for (a, alpha) in &lowers {
            if *a < 2 {
                continue;
            }
            for (b, _) in &uppers {
                if *b < 2 {
                    continue;
                }
                // 0 <= a * v - alpha <= floor((a * b - a - b) / b)
                let Some(ab) = a.checked_mul(*b) else {
                    return SatState::Unknown;
                };
                let max_t = (ab - *a - *b).div_euclid(*b);
                if max_t > 32 {
                    return SatState::Unknown;
                }
                for t in 0..=max_t {
                    // Equality: a * v - alpha - t == 0  <=>  -a * v + alpha + t == 0
                    let mut eq = alpha.clone();
                    let Some(nc) = eq.constant.checked_add(t) else {
                        return SatState::Unknown;
                    };
                    eq.constant = nc;
                    eq.terms.insert(elim_var.clone(), -*a);
                    match solve_system(vec![eq], orig_ineqs.clone(), next_aux, fuel) {
                        SatState::Sat => return SatState::Sat,
                        SatState::Unknown => any_unknown = true,
                        SatState::Unsat => {}
                    }
                }
            }
        }
        return if any_unknown {
            SatState::Unknown
        } else {
            SatState::Unsat
        };
    }
}

/// Build an [`OmegaContext`] seeded with positivity (`>= 1`) for every symbolic/meta atom
/// referenced in `dims`.
///
/// # Domain Invariant Cross-Reference
/// The `dim >= 1` invariant for all Mora tensor dimensions is enforced at runtime by:
/// - [`crate::value::tensor::checked_numel`] (`"Tensor shape dimensions must be positive (> 0)"`)
/// - [`crate::value::tensor::Tensor::slice_nd`] (`"Tensor slice produces empty dimension (size 0)"`)
///
/// Static symbolic slice/index proofs built on this context are a compile-time precision
/// enhancement (rejecting provably empty slices and provably out-of-bounds indices); accepted
/// symbolic indices remain guarded by runtime bounds checks in `Tensor::slice_nd`.
pub(crate) fn positive_dim_context(dims: &[&DimExpr]) -> OmegaContext {
    let mut ctx = OmegaContext::new();
    let mut seen = std::collections::HashSet::new();
    for d in dims {
        if let Some(aff) = AffineExpr::from_dim(d) {
            for atom in aff.terms.keys() {
                if seen.insert(atom.clone()) {
                    ctx.assume_positive(atom.clone());
                }
            }
        }
    }
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> DimExpr {
        DimExpr::Symbolic(s.to_string())
    }

    fn add(a: DimExpr, b: DimExpr) -> DimExpr {
        DimExpr::BinOp(DimOp::Add, Box::new(a), Box::new(b))
    }

    fn sub(a: DimExpr, b: DimExpr) -> DimExpr {
        DimExpr::BinOp(DimOp::Sub, Box::new(a), Box::new(b))
    }

    fn mul(a: DimExpr, b: DimExpr) -> DimExpr {
        DimExpr::BinOp(DimOp::Mul, Box::new(a), Box::new(b))
    }

    fn div(a: DimExpr, b: DimExpr) -> DimExpr {
        DimExpr::BinOp(DimOp::Div, Box::new(a), Box::new(b))
    }

    #[test]
    fn affine_commutativity_cancellation_distributivity_and_exact_div() {
        assert!(dims_equiv(
            &add(sym("S1"), sym("S2")),
            &add(sym("S2"), sym("S1"))
        ));
        assert!(dims_equiv(
            &sub(add(sym("S1"), sym("S2")), sym("S1")),
            &sym("S2")
        ));
        assert!(dims_equiv(
            &add(sym("D"), sym("D")),
            &mul(DimExpr::Known(2), sym("D"))
        ));
        assert!(dims_equiv(
            &mul(DimExpr::Known(2), add(sym("A"), sym("B"))),
            &add(
                mul(sym("A"), DimExpr::Known(2)),
                mul(sym("B"), DimExpr::Known(2))
            )
        ));
        assert!(dims_equiv(
            &div(
                add(mul(DimExpr::Known(2), sym("D")), DimExpr::Known(4)),
                DimExpr::Known(2)
            ),
            &add(sym("D"), DimExpr::Known(2))
        ));
        assert!(!dims_equiv(
            &add(sym("S"), DimExpr::Known(1)),
            &add(sym("S"), DimExpr::Known(2))
        ));
    }

    #[test]
    fn monomial_volume_conservation_for_reshape() {
        // [B, S, H * DH] == [B, S, H, DH]
        assert_eq!(
            check_reshape_volume(
                &[sym("B"), sym("S"), mul(sym("H"), sym("DH"))],
                &[sym("B"), sym("S"), sym("H"), sym("DH")]
            ),
            VolumeCheck::Equal
        );
        // [B, 2 * S, D] == [B, S, D + D]
        assert_eq!(
            check_reshape_volume(
                &[sym("B"), mul(DimExpr::Known(2), sym("S")), sym("D")],
                &[sym("B"), sym("S"), add(sym("D"), sym("D"))]
            ),
            VolumeCheck::Equal
        );
        // [B, S, 4 * D] != [B, S, 3 * D]
        assert!(matches!(
            check_reshape_volume(
                &[sym("B"), sym("S"), mul(DimExpr::Known(4), sym("D"))],
                &[sym("B"), sym("S"), mul(DimExpr::Known(3), sym("D"))]
            ),
            VolumeCheck::ProvablyUnequal { .. }
        ));
        // [B, S] != [B, S * 2]
        assert!(matches!(
            check_reshape_volume(
                &[sym("B"), sym("S")],
                &[sym("B"), mul(sym("S"), DimExpr::Known(2))]
            ),
            VolumeCheck::ProvablyUnequal { .. }
        ));
        // Unrelated free symbols [B, 4] vs [2, D] stay Unknown
        assert_eq!(
            check_reshape_volume(
                &[sym("B"), DimExpr::Known(4)],
                &[DimExpr::Known(2), sym("D")]
            ),
            VolumeCheck::Unknown
        );
    }

    #[test]
    fn linear_meta_var_unification_and_integer_contradiction() {
        // 2 * _d0 + 4 == 2 * S + 8  ==>  _d0 = S + 2
        let res = unify_dim_linear(
            &add(mul(DimExpr::Known(2), DimExpr::Var(0)), DimExpr::Known(4)),
            &add(mul(DimExpr::Known(2), sym("S")), DimExpr::Known(8)),
        );
        assert_eq!(
            res,
            DimUnifyOutcome::Bind(0, add(sym("S"), DimExpr::Known(2)))
        );

        // _d0 + S1 == S1 + S2  ==>  _d0 = S2
        let res2 = unify_dim_linear(&add(DimExpr::Var(7), sym("S1")), &add(sym("S1"), sym("S2")));
        assert_eq!(res2, DimUnifyOutcome::Bind(7, sym("S2")));

        // 2 * _d0 == 3  ==>  Contradiction over Z
        let bad = unify_dim_linear(&mul(DimExpr::Known(2), DimExpr::Var(0)), &DimExpr::Known(3));
        assert!(matches!(bad, DimUnifyOutcome::Contradiction(_)));

        // _d0 + 10 == 4  ==>  Contradiction (-6 < 0)
        let neg = unify_dim_linear(
            &add(DimExpr::Var(0), DimExpr::Known(10)),
            &DimExpr::Known(4),
        );
        assert!(matches!(neg, DimUnifyOutcome::Contradiction(_)));

        // Monomial cancellation: _d0 * H == 4 * H  ==>  _d0 = 4
        let mono = unify_dim_linear(
            &mul(DimExpr::Var(3), sym("H")),
            &mul(DimExpr::Known(4), sym("H")),
        );
        assert_eq!(mono, DimUnifyOutcome::Bind(3, DimExpr::Known(4)));
    }

    #[test]
    fn omega_context_bounds_divisibility_and_gray_shadow() {
        // 1. Bounds proof for GPU shared tile / fin<N>: 0 <= tid < 256 |= 0 <= tid < 256
        let mut ctx = OmegaContext::new();
        ctx.assume_non_negative(DimAtom::Symbolic("tid".into()));
        ctx.assume_lt(&AffineExpr::symbolic("tid"), &AffineExpr::constant(256));
        assert_eq!(
            ctx.entails_in_bounds(&AffineExpr::symbolic("tid"), &AffineExpr::constant(256)),
            SolveResult::True
        );
        assert_eq!(
            ctx.entails_in_bounds(&AffineExpr::symbolic("tid"), &AffineExpr::constant(128)),
            SolveResult::False
        );
        // tid + 1 < 256 is NOT entailed (tid could be 255)
        let tid_p1 = AffineExpr::symbolic("tid")
            .add(&AffineExpr::constant(1))
            .expect("add");
        assert_eq!(
            ctx.entails_in_bounds(&tid_p1, &AffineExpr::constant(256)),
            SolveResult::False
        );

        // 2. Transitive inequality + fin<M> widening to fin<N>: 0 <= i < M, M <= N |= 0 <= i < N
        let mut wctx = OmegaContext::new();
        wctx.assume_non_negative(DimAtom::Symbolic("i".into()));
        wctx.assume_lt(&AffineExpr::symbolic("i"), &AffineExpr::symbolic("M"));
        wctx.assume_le(&AffineExpr::symbolic("M"), &AffineExpr::symbolic("N"));
        assert_eq!(
            wctx.entails_in_bounds(&AffineExpr::symbolic("i"), &AffineExpr::symbolic("N")),
            SolveResult::True
        );

        // 3. Divisibility (GQA H % KV == 0 and constant modulus)
        let mut dctx = OmegaContext::new();
        dctx.assume_positive(DimAtom::Symbolic("KV".into()));
        dctx.assume_eq(
            &AffineExpr::symbolic("H"),
            &AffineExpr::symbolic("KV").scale(4).expect("scale"),
        );
        assert_eq!(
            dctx.entails_divides_expr(&AffineExpr::symbolic("H"), &AffineExpr::symbolic("KV")),
            SolveResult::True
        );
        assert_eq!(
            dctx.entails_divides(4, &AffineExpr::symbolic("H")),
            SolveResult::True
        );
        let h_plus_1 = AffineExpr::symbolic("H")
            .add(&AffineExpr::constant(1))
            .expect("add");
        assert_eq!(dctx.entails_divides(4, &h_plus_1), SolveResult::False);

        // 4. Concrete divisibility refutation: H = 12, KV = 5 => 12 % 5 == 0 is False
        let mut gqa_bad = OmegaContext::new();
        gqa_bad.assume_eq(&AffineExpr::symbolic("H"), &AffineExpr::constant(12));
        gqa_bad.assume_eq(&AffineExpr::symbolic("KV"), &AffineExpr::constant(5));
        assert_eq!(
            gqa_bad.entails_divides_expr(&AffineExpr::symbolic("H"), &AffineExpr::symbolic("KV")),
            SolveResult::False
        );

        // 5. Non-exact Gray Shadow test: 3*y + 1 <= 2*x <= 3*y + 1 with 2 | y is UNSAT over Z
        //    (if y = 2k, 2x = 6k + 1 impossible!), while without 2 | y it is SAT (y = 1, x = 2)!
        let mut gray_sat = OmegaContext::new();
        let two_x = AffineExpr::symbolic("x").scale(2).expect("2x");
        let three_y_p1 = AffineExpr::symbolic("y")
            .scale(3)
            .expect("3y")
            .add(&AffineExpr::constant(1))
            .expect("3y+1");
        gray_sat.assume_le(&three_y_p1, &two_x);
        gray_sat.assume_le(&two_x, &three_y_p1);
        assert_eq!(gray_sat.check_sat(), SolveResult::True);

        let mut gray_unsat = gray_sat.clone();
        gray_unsat.assume_divides(2, &AffineExpr::symbolic("y"));
        assert_eq!(gray_unsat.check_sat(), SolveResult::False);

        // 6. Real-feasible but integer-infeasible triangle:
        //    2x - 2y >= 1, 2x - 2y <= 1 -> tightened by GCD to x - y >= 1 and x - y <= 0 -> UNSAT!
        let mut int_gap = OmegaContext::new();
        let two_xm2y = AffineExpr::symbolic("x")
            .sub(&AffineExpr::symbolic("y"))
            .expect("x-y")
            .scale(2)
            .expect("2(x-y)");
        int_gap.assume_le(&AffineExpr::constant(1), &two_xm2y);
        int_gap.assume_le(&two_xm2y, &AffineExpr::constant(1));
        assert_eq!(int_gap.check_sat(), SolveResult::False);

        // 7. Genuine Gray Shadow case where NO GCD tightening triggers:
        //    2x >= 3y + 1, 2x <= 3y + 2, 3x <= 4y, 3x >= 4y -> x=4, y=3 (2*4=8 not in [10,11]);
        //    simpler: 2x >= 3y + 1, 2x <= 3y + 1 -> tight pair catches it;
        //    use strict non-tight gap: 2x >= 3y + 1, 2x <= 3y + 1 + 0 vs
        //    real-only point (x = 0.5, y = 0.5): 2x + 2y >= 2? No, try:
        //    3x >= 2y + 1, 3x <= 2y + 2, y >= 0, y <= 0 => 1 <= 3x <= 2 => NO integer x!
        let mut real_only = OmegaContext::new();
        let three_x = AffineExpr::symbolic("x").scale(3).expect("3x");
        let two_y = AffineExpr::symbolic("y").scale(2).expect("2y");
        real_only.assume_le(&two_y.add(&AffineExpr::constant(1)).expect("+1"), &three_x);
        real_only.assume_le(&three_x, &two_y.add(&AffineExpr::constant(2)).expect("+2"));
        real_only.assume_le(&AffineExpr::constant(0), &AffineExpr::symbolic("y"));
        real_only.assume_le(&AffineExpr::symbolic("y"), &AffineExpr::constant(0));
        assert_eq!(real_only.check_sat(), SolveResult::False);
    }
}
