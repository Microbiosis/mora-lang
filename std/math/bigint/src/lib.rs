//! Mora standard library arbitrary-precision integer module (`std/math/bigint`).
//!
//! Demoted out of the core compiler and `Value` representation in v0.110.0 so the
//! language kernel stays focused on hardware tensor widths while preserving an
//! exact, panic-free arbitrary-precision integer library.

use std::fmt;
use std::ops::{Add, Mul, Neg, Sub};
use std::str::FromStr;

use num_bigint::Sign;
use num_traits::{Signed, ToPrimitive, Zero};

/// Errors produced by arbitrary-precision integer operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BigIntError {
    /// String could not be parsed as an integer in the requested radix.
    InvalidFormat(String),
    /// Radix outside the supported `2..=36` range.
    InvalidRadix(u32),
    /// Attempted division by zero.
    DivisionByZero,
    /// Attempted modulo by zero.
    ModuloByZero,
    /// Attempted modular exponentiation with a non-positive modulus.
    NonPositiveModulus,
    /// Attempted modular exponentiation with a negative exponent.
    NegativeExponent,
    /// Value does not fit into a signed 64-bit integer (`i64`).
    OutOfI64Range(String),
}

impl fmt::Display for BigIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat(s) => write!(f, "invalid bigint literal: {s}"),
            Self::InvalidRadix(r) => write!(f, "invalid radix {r}: expected 2..=36"),
            Self::DivisionByZero => write!(f, "division by zero"),
            Self::ModuloByZero => write!(f, "modulo by zero"),
            Self::NonPositiveModulus => {
                write!(f, "modular exponentiation requires a positive modulus")
            }
            Self::NegativeExponent => {
                write!(f, "modular exponentiation requires a non-negative exponent")
            }
            Self::OutOfI64Range(v) => write!(f, "bigint({v}) out of i64 range"),
        }
    }
}

impl std::error::Error for BigIntError {}

/// Arbitrary-precision signed integer (`std/math/bigint`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BigInt(num_bigint::BigInt);

impl BigInt {
    /// Returns `0`.
    #[must_use]
    pub fn zero() -> Self {
        Self(num_bigint::BigInt::zero())
    }

    /// Returns `1`.
    #[must_use]
    pub fn one() -> Self {
        Self(num_bigint::BigInt::from(1u8))
    }

    /// Constructs a `BigInt` losslessly from `i64`.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        Self(num_bigint::BigInt::from(value))
    }

    /// Constructs a `BigInt` losslessly from `u64`.
    #[must_use]
    pub fn from_u64(value: u64) -> Self {
        Self(num_bigint::BigInt::from(value))
    }

    /// Constructs a `BigInt` losslessly from `i128`.
    #[must_use]
    pub fn from_i128(value: i128) -> Self {
        Self(num_bigint::BigInt::from(value))
    }

    /// Parses a base-10 integer string (supports leading `+`/`-` and `_` separators).
    pub fn parse(input: &str) -> Result<Self, BigIntError> {
        Self::from_str_radix(input, 10)
    }

    /// Parses an integer string in `radix` (`2..=36`), ignoring ASCII underscores.
    pub fn from_str_radix(input: &str, radix: u32) -> Result<Self, BigIntError> {
        if !(2..=36).contains(&radix) {
            return Err(BigIntError::InvalidRadix(radix));
        }
        let trimmed = input.trim();
        let cleaned: String = trimmed.chars().filter(|&c| c != '_').collect();
        if cleaned.is_empty() || cleaned == "+" || cleaned == "-" {
            return Err(BigIntError::InvalidFormat(input.to_string()));
        }
        num_bigint::BigInt::parse_bytes(cleaned.as_bytes(), radix)
            .map(Self)
            .ok_or_else(|| BigIntError::InvalidFormat(input.to_string()))
    }

    /// Formats this integer in the given `radix` (`2..=36`).
    pub fn to_radix_string(&self, radix: u32) -> Result<String, BigIntError> {
        if !(2..=36).contains(&radix) {
            return Err(BigIntError::InvalidRadix(radix));
        }
        Ok(self.0.to_str_radix(radix))
    }

    /// Exact addition.
    #[must_use]
    pub fn checked_add(&self, rhs: &Self) -> Self {
        Self(&self.0 + &rhs.0)
    }

    /// Exact subtraction.
    #[must_use]
    pub fn checked_sub(&self, rhs: &Self) -> Self {
        Self(&self.0 - &rhs.0)
    }

    /// Exact multiplication.
    #[must_use]
    pub fn checked_mul(&self, rhs: &Self) -> Self {
        Self(&self.0 * &rhs.0)
    }

    /// Exact truncating division, returning `BigIntError::DivisionByZero` when `rhs == 0`.
    pub fn checked_div(&self, rhs: &Self) -> Result<Self, BigIntError> {
        if rhs.is_zero() {
            return Err(BigIntError::DivisionByZero);
        }
        Ok(Self(&self.0 / &rhs.0))
    }

    /// Exact remainder, returning `BigIntError::ModuloByZero` when `rhs == 0`.
    pub fn checked_rem(&self, rhs: &Self) -> Result<Self, BigIntError> {
        if rhs.is_zero() {
            return Err(BigIntError::ModuloByZero);
        }
        Ok(Self(&self.0 % &rhs.0))
    }

    /// Computes quotient and remainder together, rejecting a zero divisor.
    pub fn div_rem(&self, rhs: &Self) -> Result<(Self, Self), BigIntError> {
        if rhs.is_zero() {
            return Err(BigIntError::DivisionByZero);
        }
        let q = &self.0 / &rhs.0;
        let r = &self.0 % &rhs.0;
        Ok((Self(q), Self(r)))
    }

    /// Raises `self` to `exp`.
    #[must_use]
    pub fn pow(&self, exp: u32) -> Self {
        Self(self.0.pow(exp))
    }

    /// Negates `self`.
    #[must_use]
    pub fn neg(&self) -> Self {
        Self(-&self.0)
    }

    /// Absolute value.
    #[must_use]
    pub fn abs(&self) -> Self {
        Self(self.0.abs())
    }

    /// Signum as `-1`, `0`, or `1`.
    #[must_use]
    pub fn signum(&self) -> i8 {
        match self.0.sign() {
            Sign::Minus => -1,
            Sign::NoSign => 0,
            Sign::Plus => 1,
        }
    }

    /// Returns `true` if the value is `0`.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Returns `true` if strictly negative.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.0.is_negative()
    }

    /// Returns `true` if strictly positive.
    #[must_use]
    pub fn is_positive(&self) -> bool {
        self.0.is_positive()
    }

    /// Non-negative greatest common divisor via Euclidean algorithm.
    #[must_use]
    pub fn gcd(&self, other: &Self) -> Self {
        let mut x = self.0.abs();
        let mut y = other.0.abs();
        while !y.is_zero() {
            let r = &x % &y;
            x = y;
            y = r;
        }
        Self(x)
    }

    /// Non-negative least common multiple (returns `0` if either operand is `0`).
    #[must_use]
    pub fn lcm(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let g = self.gcd(other);
        let q = self.0.abs() / &g.0;
        Self(q * other.0.abs())
    }

    /// Modular exponentiation `(self ^ exp) mod modulus`.
    ///
    /// Rejects non-positive `modulus` and negative `exp` instead of panicking.
    pub fn mod_pow(&self, exp: &Self, modulus: &Self) -> Result<Self, BigIntError> {
        if !modulus.is_positive() {
            return Err(BigIntError::NonPositiveModulus);
        }
        if exp.is_negative() {
            return Err(BigIntError::NegativeExponent);
        }
        Ok(Self(self.0.modpow(&exp.0, &modulus.0)))
    }

    /// Converts to `i64` when representable without overflow.
    pub fn to_i64_checked(&self) -> Result<i64, BigIntError> {
        self.0
            .to_i64()
            .ok_or_else(|| BigIntError::OutOfI64Range(self.to_string()))
    }

    /// Lossy conversion to `f64`, mapping out-of-range magnitudes to signed `±inf`.
    #[must_use]
    pub fn to_f64_lossy(&self) -> f64 {
        self.0.to_f64().unwrap_or_else(|| {
            if self.is_negative() {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        })
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for BigInt {
    type Err = BigIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl From<i64> for BigInt {
    fn from(value: i64) -> Self {
        Self::from_i64(value)
    }
}

impl From<u64> for BigInt {
    fn from(value: u64) -> Self {
        Self::from_u64(value)
    }
}

impl From<i128> for BigInt {
    fn from(value: i128) -> Self {
        Self::from_i128(value)
    }
}

impl Add for BigInt {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Add for &BigInt {
    type Output = BigInt;

    fn add(self, rhs: Self) -> Self::Output {
        self.checked_add(rhs)
    }
}

impl Sub for BigInt {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Sub for &BigInt {
    type Output = BigInt;

    fn sub(self, rhs: Self) -> Self::Output {
        self.checked_sub(rhs)
    }
}

impl Mul for BigInt {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        Self(self.0 * rhs.0)
    }
}

impl Mul for &BigInt {
    type Output = BigInt;

    fn mul(self, rhs: Self) -> Self::Output {
        self.checked_mul(rhs)
    }
}

impl Neg for BigInt {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self(-self.0)
    }
}

impl Neg for &BigInt {
    type Output = BigInt;

    fn neg(self) -> Self::Output {
        BigInt(-&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn arith_and_cmp_are_exact_beyond_f64() {
        let a = BigInt::from_i64(10).pow(30);
        let one = BigInt::one();
        let sum = a.checked_add(&one);
        assert_eq!(sum.checked_sub(&a), one);
        assert!(sum > a);
    }

    #[test]
    fn div_and_mod_by_zero_return_error() {
        let a = BigInt::from_i64(7);
        let z = BigInt::zero();
        assert_eq!(a.checked_div(&z), Err(BigIntError::DivisionByZero));
        assert_eq!(a.checked_rem(&z), Err(BigIntError::ModuloByZero));
        assert_eq!(a.div_rem(&z), Err(BigIntError::DivisionByZero));
    }

    #[test]
    fn i64_checked_boundaries() {
        let max = BigInt::from_i64(i64::MAX);
        let min = BigInt::from_i64(i64::MIN);
        assert_eq!(max.to_i64_checked(), Ok(i64::MAX));
        assert_eq!(min.to_i64_checked(), Ok(i64::MIN));
        assert!(matches!(
            max.checked_add(&BigInt::one()).to_i64_checked(),
            Err(BigIntError::OutOfI64Range(_))
        ));
        assert!(matches!(
            min.checked_sub(&BigInt::one()).to_i64_checked(),
            Err(BigIntError::OutOfI64Range(_))
        ));
    }

    #[test]
    fn radix_parse_and_format() {
        let n = BigInt::from_str_radix("dead_beef_cafe_babe_1234", 16)
            .expect("valid hex literal should parse");
        assert_eq!(
            n.to_radix_string(16).expect("valid radix"),
            "deadbeefcafebabe1234"
        );
        assert_eq!(
            BigInt::from_str_radix("10", 1),
            Err(BigIntError::InvalidRadix(1))
        );
        assert_eq!(
            BigInt::from_str_radix("10", 37),
            Err(BigIntError::InvalidRadix(37))
        );
        assert!(matches!(
            BigInt::parse("   -  "),
            Err(BigIntError::InvalidFormat(_))
        ));
    }

    #[test]
    fn gcd_lcm_and_mod_pow_safety() {
        let a = BigInt::from_i64(-48);
        let b = BigInt::from_i64(18);
        assert_eq!(a.gcd(&b), BigInt::from_i64(6));
        assert_eq!(a.lcm(&b), BigInt::from_i64(144));
        assert_eq!(a.lcm(&BigInt::zero()), BigInt::zero());

        assert_eq!(
            BigInt::from_i64(2).mod_pow(&BigInt::from_i64(10), &BigInt::from_i64(1000)),
            Ok(BigInt::from_i64(24))
        );
        assert_eq!(
            BigInt::from_i64(2).mod_pow(&BigInt::from_i64(1), &BigInt::zero()),
            Err(BigIntError::NonPositiveModulus)
        );
        assert_eq!(
            BigInt::from_i64(2).mod_pow(&BigInt::from_i64(-1), &BigInt::from_i64(5)),
            Err(BigIntError::NegativeExponent)
        );
    }

    #[test]
    fn overflow_f64_preserves_sign() {
        let huge = BigInt::from_i64(2).pow(2000);
        let neg_huge = -huge.clone();
        assert_eq!(huge.to_f64_lossy(), f64::INFINITY);
        assert_eq!(neg_huge.to_f64_lossy(), f64::NEG_INFINITY);
    }

    proptest! {
        #[test]
        fn prop_div_rem_identity(a in any::<i128>(), b in any::<i128>().prop_filter("non-zero", |x| *x != 0)) {
            let ba = BigInt::from_i128(a);
            let bb = BigInt::from_i128(b);
            let (q, r) = ba.div_rem(&bb).expect("divisor is non-zero");
            prop_assert_eq!(q.checked_mul(&bb).checked_add(&r), ba);
        }

        #[test]
        fn prop_gcd_divides_both(a in any::<i64>(), b in any::<i64>()) {
            let ba = BigInt::from_i64(a);
            let bb = BigInt::from_i64(b);
            let g = ba.gcd(&bb);
            if !g.is_zero() {
                prop_assert_eq!(ba.checked_rem(&g).expect("non-zero gcd"), BigInt::zero());
                prop_assert_eq!(bb.checked_rem(&g).expect("non-zero gcd"), BigInt::zero());
            }
        }
    }
}
