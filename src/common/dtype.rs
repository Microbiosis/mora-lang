//! Hardware numeric `DType`, exact bit-level quantization codecs, `ScalarValue`, and `Device`
//! (Wave 3 — Stage 3B).
//!
//! Architectural guarantees:
//! 1. Exact IEEE 754 `binary16` (`f16`), `bfloat16` (`bf16`), OCP `FP8 E4M3FN` (`fp8`/`fp8_e4m3`),
//!    OCP `FP8 E5M2` (`fp8_e5m2`), and 4-bit two's-complement (`i4`) bit-level codecs with zero
//!    double-rounding from `f64`.
//! 2. First-class runtime [`ScalarValue`] carrying exact hardware bit representations for all
//!    low-bitwidth / unsigned scalar types (`f32`, `f16`, `bf16`, `fp8_e4m3`, `fp8_e5m2`, `i32`,
//!    `i16`, `i8`, `i4`, `u64`, `u32`, `u16`, `u8`) while default `i64` / `f64` stay `Value::Int`
//!    and `Value::Float`.
//! 3. Strict same-`DType` runtime scalar arithmetic with exact hardware re-quantization for floats
//!    and exact range-checked overflow detection for integers.

use std::fmt;

use super::BinaryOp;
use crate::error::{MoraError, MoraResult};

/// Hardware numeric precision and quantization data type (Wave 3 — Stage 3B).
///
/// Unified first-class `DType` shared by both scalar types (`Type::Scalar(DType)` /
/// `Type::Int` / `Type::Float` / `Type::Bool`) and tensors (`Tensor<DType, Shape, Device>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DType {
    /// 64-bit IEEE 754 float (canonical type name `float`, alias `f64`)
    F64,
    /// 32-bit IEEE 754 float (`f32`)
    F32,
    /// 16-bit IEEE 754 half-precision float (`f16`, 1s + 5e + 10m)
    F16,
    /// 16-bit Brain Floating Point (`bf16`, 1s + 8e + 7m)
    BF16,
    /// 8-bit OCP FP8 E4M3FN (`fp8` / `fp8_e4m3`, 1s + 4e + 3m, max finite 448.0)
    FP8E4M3,
    /// 8-bit OCP FP8 E5M2 (`fp8_e5m2`, 1s + 5e + 2m, max finite 57344.0)
    FP8E5M2,
    /// 64-bit signed integer (canonical type name `int`, alias `i64`)
    I64,
    /// 32-bit signed integer (`i32`)
    I32,
    /// 16-bit signed integer (`i16`)
    I16,
    /// 8-bit signed integer (`i8`)
    I8,
    /// 4-bit signed quantized integer (`i4`, range `-8..=7`, nibble-packed 2 elems/byte)
    I4,
    /// 64-bit unsigned integer (`u64`)
    U64,
    /// 32-bit unsigned integer (`u32`)
    U32,
    /// 16-bit unsigned integer (`u16`)
    U16,
    /// 8-bit unsigned integer (`u8`)
    U8,
    /// 1-byte boolean mask dtype (`bool`)
    Bool,
}

impl DType {
    /// Parse a `DType` from a type annotation or tensor constructor dtype string.
    ///
    /// Note: single-letter literal suffixes (`f`, `i`, `u`) are intentionally excluded here so
    /// generic type parameters like `task foo<T, U, F, I>(...)` are never hijacked as dtypes;
    /// use [`Self::from_literal_suffix`] in the lexer.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "float" | "f64" => Some(Self::F64),
            "f32" => Some(Self::F32),
            "f16" => Some(Self::F16),
            "bf16" => Some(Self::BF16),
            "fp8" | "fp8_e4m3" | "fp8e4m3" => Some(Self::FP8E4M3),
            "fp8_e5m2" | "fp8e5m2" => Some(Self::FP8E5M2),
            "int" | "i64" => Some(Self::I64),
            "i32" => Some(Self::I32),
            "i16" => Some(Self::I16),
            "i8" => Some(Self::I8),
            "i4" => Some(Self::I4),
            "u64" => Some(Self::U64),
            "u32" => Some(Self::U32),
            "u16" => Some(Self::U16),
            "u8" => Some(Self::U8),
            "bool" => Some(Self::Bool),
            _ => None,
        }
    }

    /// Parse a numeric literal suffix in the lexer (includes shorthand `f`, `i`, `u`).
    pub fn from_literal_suffix(suf: &str) -> Option<Self> {
        match suf.trim().to_ascii_lowercase().as_str() {
            "f" => Some(Self::F64),
            "i" => Some(Self::I64),
            "u" => Some(Self::U64),
            other => Self::from_name(other),
        }
    }

    /// Canonical Mora type display name (`I64 -> "int"`, `F64 -> "float"`, `Bool -> "bool"`,
    /// `FP8E4M3 -> "fp8"`, `FP8E5M2 -> "fp8_e5m2"`, others keep hardware width names).
    pub fn canonical_type_name(&self) -> &'static str {
        match self {
            Self::F64 => "float",
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::BF16 => "bf16",
            Self::FP8E4M3 => "fp8",
            Self::FP8E5M2 => "fp8_e5m2",
            Self::I64 => "int",
            Self::I32 => "i32",
            Self::I16 => "i16",
            Self::I8 => "i8",
            Self::I4 => "i4",
            Self::U64 => "u64",
            Self::U32 => "u32",
            Self::U16 => "u16",
            Self::U8 => "u8",
            Self::Bool => "bool",
        }
    }

    /// Hardware bit-width short identifier (`f64`, `f32`, `f16`, `bf16`, `fp8`, `fp8_e5m2`, `i4`..`i64`, `u8`..`u64`, `bool`).
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::F64 => "f64",
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::BF16 => "bf16",
            Self::FP8E4M3 => "fp8",
            Self::FP8E5M2 => "fp8_e5m2",
            Self::I64 => "i64",
            Self::I32 => "i32",
            Self::I16 => "i16",
            Self::I8 => "i8",
            Self::I4 => "i4",
            Self::U64 => "u64",
            Self::U32 => "u32",
            Self::U16 => "u16",
            Self::U8 => "u8",
            Self::Bool => "bool",
        }
    }

    pub fn is_float(&self) -> bool {
        matches!(
            self,
            Self::F64 | Self::F32 | Self::F16 | Self::BF16 | Self::FP8E4M3 | Self::FP8E5M2
        )
    }

    pub fn is_int(&self) -> bool {
        self.is_signed_int() || self.is_unsigned_int()
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Self::Bool)
    }

    pub fn is_signed_int(&self) -> bool {
        matches!(
            self,
            Self::I64 | Self::I32 | Self::I16 | Self::I8 | Self::I4
        )
    }

    pub fn is_unsigned_int(&self) -> bool {
        matches!(self, Self::U64 | Self::U32 | Self::U16 | Self::U8)
    }

    /// Physical bit width per element in packed tensor storage.
    pub fn bit_width(&self) -> u8 {
        match self {
            Self::F64 | Self::I64 | Self::U64 => 64,
            Self::F32 | Self::I32 | Self::U32 => 32,
            Self::F16 | Self::BF16 | Self::I16 | Self::U16 => 16,
            Self::FP8E4M3 | Self::FP8E5M2 | Self::I8 | Self::U8 | Self::Bool => 8,
            Self::I4 => 4,
        }
    }

    /// Packed storage byte count required for `numel` elements.
    pub fn storage_bytes(&self, numel: usize) -> usize {
        match self.bit_width() {
            4 => numel.div_ceil(2),
            bits => numel * ((bits as usize) / 8),
        }
    }

    /// Inclusive `(min, max)` bounds representable in `i64` for integer `DType`s.
    pub fn int_bounds(&self) -> Option<(i64, i64)> {
        match self {
            Self::I4 => Some((-8, 7)),
            Self::I8 => Some((i8::MIN as i64, i8::MAX as i64)),
            Self::I16 => Some((i16::MIN as i64, i16::MAX as i64)),
            Self::I32 => Some((i32::MIN as i64, i32::MAX as i64)),
            Self::I64 => Some((i64::MIN, i64::MAX)),
            Self::U8 => Some((0, u8::MAX as i64)),
            Self::U16 => Some((0, u16::MAX as i64)),
            Self::U32 => Some((0, u32::MAX as i64)),
            Self::U64 => Some((0, i64::MAX)),
            _ => None,
        }
    }

    /// Check whether signed integer `v` falls within this integer `DType`'s exact range.
    pub fn check_int_range(&self, v: i64) -> MoraResult<()> {
        match self.int_bounds() {
            Some((lo, hi)) if v >= lo && v <= hi => Ok(()),
            Some((lo, hi)) => Err(MoraError::Runtime(format!(
                "Integer literal {v} out of range for dtype '{}' (valid range: {lo}..={hi})",
                self.short_name()
            ))),
            None => Err(MoraError::Runtime(format!(
                "DType '{}' does not support integer range check",
                self.short_name()
            ))),
        }
    }

    /// Check whether non-negative lexical magnitude `n` is legal for this integer `DType`
    /// (permits `|MIN|` such as `128i8` or `8i4` so unary `-128i8` / `-8i4` folds cleanly).
    pub fn check_lex_int_magnitude(&self, n: i64) -> bool {
        if n < 0 {
            return false;
        }
        match self {
            Self::I4 => n <= 8,
            Self::I8 => n <= 128,
            Self::I16 => n <= 32768,
            Self::I32 => n <= 2_147_483_648,
            Self::I64 => true,
            Self::U8 => n <= u8::MAX as i64,
            Self::U16 => n <= u16::MAX as i64,
            Self::U32 => n <= u32::MAX as i64,
            Self::U64 => true,
            _ => false,
        }
    }

    /// Saturate/clamp `v` into this integer `DType`'s exact bounds.
    pub fn quantize_i64(&self, v: i64) -> i64 {
        match self.int_bounds() {
            Some((lo, hi)) => v.clamp(lo, hi),
            None if matches!(self, Self::Bool) => i64::from(v != 0),
            None => v,
        }
    }

    /// Quantize `v` through the exact bit-level hardware format codec for `self` and decode back to `f64`.
    pub fn quantize_f64(&self, v: f64) -> f64 {
        match self {
            Self::F64 => v,
            Self::F32 => (v as f32) as f64,
            Self::F16 => f16_bits_to_f64(f64_to_f16_bits(v)),
            Self::BF16 => bf16_bits_to_f64(f64_to_bf16_bits(v)),
            Self::FP8E4M3 => fp8_e4m3_bits_to_f64(f64_to_fp8_e4m3_bits(v)),
            Self::FP8E5M2 => fp8_e5m2_bits_to_f64(f64_to_fp8_e5m2_bits(v)),
            Self::Bool => {
                if v != 0.0 && !v.is_nan() {
                    1.0
                } else {
                    0.0
                }
            }
            _ => {
                if !v.is_finite() {
                    0.0
                } else {
                    self.quantize_i64(v as i64) as f64
                }
            }
        }
    }
}

impl fmt::Display for DType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.canonical_type_name())
    }
}

// ============================================================================
// Exact Bit-Level Hardware Codecs (F16, BF16, FP8 E4M3FN, FP8 E5M2, I4)
// ============================================================================

/// Encode `f64` directly to IEEE 754 `binary16` (`u16`) with round-to-nearest-even (no double rounding).
pub fn f64_to_f16_bits(v: f64) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exp64 = ((bits >> 52) & 0x7ff) as i32;
    let frac64 = bits & 0x000f_ffff_ffff_ffff;

    if exp64 == 0x7ff {
        return if frac64 != 0 {
            sign | 0x7e00 // quiet NaN
        } else {
            sign | 0x7c00 // ±Infinity
        };
    }
    if exp64 == 0 && frac64 == 0 {
        return sign; // ±0.0
    }

    let unbiased = exp64 - 1023;
    if unbiased >= -14 {
        let mut mant = (frac64 >> 42) as u32;
        let round = (frac64 >> 41) & 1;
        let sticky = (frac64 & ((1u64 << 41) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        let mut exp16 = (unbiased + 15) as u32;
        if mant == 1024 {
            mant = 0;
            exp16 += 1;
        }
        if exp16 >= 31 {
            return sign | 0x7c00; // overflow to ±Inf
        }
        return sign | ((exp16 as u16) << 10) | (mant as u16);
    }

    if unbiased >= -25 {
        let sig = (1u64 << 52) | frac64;
        let shift = (28 - unbiased) as u32; // 43..=53
        let mut mant = if shift >= 64 {
            0
        } else {
            (sig >> shift) as u32
        };
        let round = (sig >> (shift - 1)) & 1;
        let sticky = (sig & ((1u64 << (shift - 1)) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        if mant == 1024 {
            return sign | 0x0400; // promoted to smallest normal (exp=1, mant=0)
        }
        return sign | (mant as u16);
    }

    sign // underflow to ±0.0
}

/// Decode IEEE 754 `binary16` (`u16`) to exact `f64`.
pub fn f16_bits_to_f64(bits: u16) -> f64 {
    let sign = if (bits & 0x8000) != 0 { -1.0 } else { 1.0 };
    let exp = ((bits >> 10) & 0x1f) as i32;
    let mant = (bits & 0x03ff) as u32;

    match exp {
        0 => {
            if mant == 0 {
                0.0 * sign
            } else {
                sign * (mant as f64) * 2.0_f64.powi(-24)
            }
        }
        31 => {
            if mant == 0 {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1.0 + (mant as f64) / 1024.0) * 2.0_f64.powi(exp - 15),
    }
}

/// Encode `f64` directly to `bfloat16` (`u16`) with round-to-nearest-even (no double rounding).
pub fn f64_to_bf16_bits(v: f64) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exp64 = ((bits >> 52) & 0x7ff) as i32;
    let frac64 = bits & 0x000f_ffff_ffff_ffff;

    if exp64 == 0x7ff {
        return if frac64 != 0 {
            sign | 0x7fc0 // quiet NaN
        } else {
            sign | 0x7f80 // ±Infinity
        };
    }
    if exp64 == 0 && frac64 == 0 {
        return sign;
    }

    let unbiased = exp64 - 1023;
    if unbiased >= -126 {
        let mut mant = (frac64 >> 45) as u32;
        let round = (frac64 >> 44) & 1;
        let sticky = (frac64 & ((1u64 << 44) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        let mut exp8 = (unbiased + 127) as u32;
        if mant == 128 {
            mant = 0;
            exp8 += 1;
        }
        if exp8 >= 255 {
            return sign | 0x7f80;
        }
        return sign | ((exp8 as u16) << 7) | (mant as u16);
    }

    if unbiased >= -134 {
        let sig = (1u64 << 52) | frac64;
        let shift = (-81 - unbiased) as u32; // 46..=53
        let mut mant = (sig >> shift) as u32;
        let round = (sig >> (shift - 1)) & 1;
        let sticky = (sig & ((1u64 << (shift - 1)) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        if mant == 128 {
            return sign | 0x0080; // smallest normal
        }
        return sign | (mant as u16);
    }

    sign
}

/// Decode `bfloat16` (`u16`) to exact `f64`.
#[inline]
pub fn bf16_bits_to_f64(bits: u16) -> f64 {
    f32::from_bits((bits as u32) << 16) as f64
}

/// Encode `f64` to OCP `FP8 E4M3FN` (`u8`, 1s + 4e bias 7 + 3m, max finite 448.0, finite saturation).
pub fn f64_to_fp8_e4m3_bits(v: f64) -> u8 {
    let bits = v.to_bits();
    let sign = ((bits >> 63) as u8) << 7;
    let exp64 = ((bits >> 52) & 0x7ff) as i32;
    let frac64 = bits & 0x000f_ffff_ffff_ffff;

    if exp64 == 0x7ff {
        return if frac64 != 0 {
            sign | 0x7f // NaN
        } else {
            sign | 0x7e // E4M3FN has no Inf; clamp ±Inf to ±448.0
        };
    }
    if exp64 == 0 && frac64 == 0 {
        return sign;
    }

    let unbiased = exp64 - 1023;
    if unbiased >= -6 {
        let mut mant = (frac64 >> 49) as u32;
        let round = (frac64 >> 48) & 1;
        let sticky = (frac64 & ((1u64 << 48) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        let mut exp4 = (unbiased + 7) as u32;
        if mant == 8 {
            mant = 0;
            exp4 += 1;
        }
        // Max finite is exp4 == 15, mant == 6 (448.0); exp4 == 15, mant == 7 is NaN.
        if exp4 > 15 || (exp4 == 15 && mant >= 7) {
            return sign | 0x7e;
        }
        return sign | ((exp4 as u8) << 3) | (mant as u8);
    }

    if unbiased >= -10 {
        let sig = (1u64 << 52) | frac64;
        let shift = (43 - unbiased) as u32; // 50..=53
        let mut mant = (sig >> shift) as u32;
        let round = (sig >> (shift - 1)) & 1;
        let sticky = (sig & ((1u64 << (shift - 1)) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        if mant == 8 {
            return sign | 0x08; // smallest normal (exp4=1, mant=0)
        }
        return sign | (mant as u8);
    }

    sign
}

/// Decode OCP `FP8 E4M3FN` (`u8`) to exact `f64`.
pub fn fp8_e4m3_bits_to_f64(bits: u8) -> f64 {
    let sign = if (bits & 0x80) != 0 { -1.0 } else { 1.0 };
    let exp = ((bits >> 3) & 0x0f) as i32;
    let mant = (bits & 0x07) as u32;

    if exp == 15 && mant == 7 {
        return f64::NAN;
    }
    if exp == 0 {
        if mant == 0 {
            0.0 * sign
        } else {
            sign * (mant as f64) * 2.0_f64.powi(-9)
        }
    } else {
        sign * (1.0 + (mant as f64) / 8.0) * 2.0_f64.powi(exp - 7)
    }
}

/// Encode `f64` to OCP `FP8 E5M2` (`u8`, 1s + 5e bias 15 + 2m, max finite 57344.0).
pub fn f64_to_fp8_e5m2_bits(v: f64) -> u8 {
    let bits = v.to_bits();
    let sign = ((bits >> 63) as u8) << 7;
    let exp64 = ((bits >> 52) & 0x7ff) as i32;
    let frac64 = bits & 0x000f_ffff_ffff_ffff;

    if exp64 == 0x7ff {
        return if frac64 != 0 {
            sign | 0x7e // NaN
        } else {
            sign | 0x7c // ±Inf
        };
    }
    if exp64 == 0 && frac64 == 0 {
        return sign;
    }

    let unbiased = exp64 - 1023;
    if unbiased >= -14 {
        let mut mant = (frac64 >> 50) as u32;
        let round = (frac64 >> 49) & 1;
        let sticky = (frac64 & ((1u64 << 49) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        let mut exp5 = (unbiased + 15) as u32;
        if mant == 4 {
            mant = 0;
            exp5 += 1;
        }
        if exp5 >= 31 {
            return sign | 0x7c; // ±Inf
        }
        return sign | ((exp5 as u8) << 2) | (mant as u8);
    }

    if unbiased >= -17 {
        let sig = (1u64 << 52) | frac64;
        let shift = (36 - unbiased) as u32; // 51..=53
        let mut mant = (sig >> shift) as u32;
        let round = (sig >> (shift - 1)) & 1;
        let sticky = (sig & ((1u64 << (shift - 1)) - 1)) != 0;
        if round == 1 && (sticky || (mant & 1) == 1) {
            mant += 1;
        }
        if mant == 4 {
            return sign | 0x04; // smallest normal (exp5=1, mant=0)
        }
        return sign | (mant as u8);
    }

    sign
}

/// Decode OCP `FP8 E5M2` (`u8`) to exact `f64`.
pub fn fp8_e5m2_bits_to_f64(bits: u8) -> f64 {
    let sign = if (bits & 0x80) != 0 { -1.0 } else { 1.0 };
    let exp = ((bits >> 2) & 0x1f) as i32;
    let mant = (bits & 0x03) as u32;

    match exp {
        0 => {
            if mant == 0 {
                0.0 * sign
            } else {
                sign * (mant as f64) * 2.0_f64.powi(-16)
            }
        }
        31 => {
            if mant == 0 {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1.0 + (mant as f64) / 4.0) * 2.0_f64.powi(exp - 15),
    }
}

/// Pack a signed 4-bit integer (`-8..=7`) into its low 4-bit two's-complement nibble.
#[inline]
pub fn pack_i4_nibble(v: i8) -> u8 {
    (v.clamp(-8, 7) as u8) & 0x0f
}

/// Unpack a 4-bit two's-complement nibble into a signed `i8` (`-8..=7`).
#[inline]
pub fn unpack_i4_nibble(nibble: u8) -> i8 {
    let n = nibble & 0x0f;
    if (n & 0x08) != 0 {
        (n | 0xf0) as i8
    } else {
        n as i8
    }
}

// ============================================================================
// First-Class Runtime Hardware Scalar Value (`Value::Scalar(ScalarValue)`)
// ============================================================================

/// Result of a same-`DType` binary operation on two [`ScalarValue`]s.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScalarBinaryOut {
    Scalar(ScalarValue),
    Bool(bool),
}

/// Non-default hardware-width scalar value carrying exact packed hardware bit representation.
///
/// Default 64-bit `i64` / `f64` stay `Value::Int` / `Value::Float`, while all explicit hardware
/// widths (`f32`, `f16`, `bf16`, `fp8`, `fp8_e5m2`, `i32`, `i16`, `i8`, `i4`, `u64`, `u32`,
/// `u16`, `u8`) preserve their true precision across VM arithmetic, control constants, and
/// tensor construction / indexing / reductions.
#[derive(Debug, Clone, Copy)]
pub enum ScalarValue {
    F32(f32),
    F16(u16),
    BF16(u16),
    FP8E4M3(u8),
    FP8E5M2(u8),
    I32(i32),
    I16(i16),
    I8(i8),
    /// Signed 4-bit value guaranteed in `-8..=7`.
    I4(i8),
    U64(u64),
    U32(u32),
    U16(u16),
    U8(u8),
}

impl ScalarValue {
    #[inline]
    pub fn dtype(&self) -> DType {
        match self {
            Self::F32(_) => DType::F32,
            Self::F16(_) => DType::F16,
            Self::BF16(_) => DType::BF16,
            Self::FP8E4M3(_) => DType::FP8E4M3,
            Self::FP8E5M2(_) => DType::FP8E5M2,
            Self::I32(_) => DType::I32,
            Self::I16(_) => DType::I16,
            Self::I8(_) => DType::I8,
            Self::I4(_) => DType::I4,
            Self::U64(_) => DType::U64,
            Self::U32(_) => DType::U32,
            Self::U16(_) => DType::U16,
            Self::U8(_) => DType::U8,
        }
    }

    /// Construct a [`ScalarValue`] from an `i64` in `dtype`, returning an error on range overflow.
    pub fn from_i64_checked(v: i64, dtype: DType) -> MoraResult<Self> {
        match dtype {
            DType::I32 => i32::try_from(v).map(Self::I32).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for i32"))
            }),
            DType::I16 => i16::try_from(v).map(Self::I16).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for i16"))
            }),
            DType::I8 => i8::try_from(v).map(Self::I8).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for i8"))
            }),
            DType::I4 => {
                if (-8..=7).contains(&v) {
                    Ok(Self::I4(v as i8))
                } else {
                    Err(MoraError::Runtime(format!(
                        "integer overflow: {v} out of range for i4 (-8..=7)"
                    )))
                }
            }
            DType::U64 => u64::try_from(v).map(Self::U64).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for u64"))
            }),
            DType::U32 => u32::try_from(v).map(Self::U32).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for u32"))
            }),
            DType::U16 => u16::try_from(v).map(Self::U16).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for u16"))
            }),
            DType::U8 => u8::try_from(v).map(Self::U8).map_err(|_| {
                MoraError::Runtime(format!("integer overflow: {v} out of range for u8"))
            }),
            _ => Err(MoraError::Runtime(format!(
                "cannot construct low-bitwidth integer ScalarValue for dtype '{dtype}'"
            ))),
        }
    }

    /// Construct a [`ScalarValue`] by quantizing/saturating `v` into `dtype`.
    pub fn from_f64_quantized(v: f64, dtype: DType) -> Option<Self> {
        Some(match dtype {
            DType::F32 => Self::F32(v as f32),
            DType::F16 => Self::F16(f64_to_f16_bits(v)),
            DType::BF16 => Self::BF16(f64_to_bf16_bits(v)),
            DType::FP8E4M3 => Self::FP8E4M3(f64_to_fp8_e4m3_bits(v)),
            DType::FP8E5M2 => Self::FP8E5M2(f64_to_fp8_e5m2_bits(v)),
            DType::I32 => Self::I32(dtype.quantize_i64(v as i64) as i32),
            DType::I16 => Self::I16(dtype.quantize_i64(v as i64) as i16),
            DType::I8 => Self::I8(dtype.quantize_i64(v as i64) as i8),
            DType::I4 => Self::I4(dtype.quantize_i64(v as i64) as i8),
            DType::U64 => {
                if v < 0.0 || !v.is_finite() {
                    Self::U64(0)
                } else {
                    Self::U64(v as u64)
                }
            }
            DType::U32 => Self::U32(dtype.quantize_i64(v as i64) as u32),
            DType::U16 => Self::U16(dtype.quantize_i64(v as i64) as u16),
            DType::U8 => Self::U8(dtype.quantize_i64(v as i64) as u8),
            DType::F64 | DType::I64 | DType::Bool => return None,
        })
    }

    /// Convert to `f64` for numeric display, tensor ingestion, and float kernels.
    #[inline]
    pub fn as_f64(&self) -> f64 {
        match *self {
            Self::F32(x) => x as f64,
            Self::F16(b) => f16_bits_to_f64(b),
            Self::BF16(b) => bf16_bits_to_f64(b),
            Self::FP8E4M3(b) => fp8_e4m3_bits_to_f64(b),
            Self::FP8E5M2(b) => fp8_e5m2_bits_to_f64(b),
            Self::I32(n) => n as f64,
            Self::I16(n) => n as f64,
            Self::I8(n) => n as f64,
            Self::I4(n) => n as f64,
            Self::U64(n) => n as f64,
            Self::U32(n) => n as f64,
            Self::U16(n) => n as f64,
            Self::U8(n) => n as f64,
        }
    }

    /// Convert integer scalars to exact `i64` (returns `None` for floats or `u64 > i64::MAX`).
    #[inline]
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Self::I32(n) => Some(n as i64),
            Self::I16(n) => Some(n as i64),
            Self::I8(n) => Some(n as i64),
            Self::I4(n) => Some(n as i64),
            Self::U64(n) => i64::try_from(n).ok(),
            Self::U32(n) => Some(n as i64),
            Self::U16(n) => Some(n as i64),
            Self::U8(n) => Some(n as i64),
            Self::F32(_) | Self::F16(_) | Self::BF16(_) | Self::FP8E4M3(_) | Self::FP8E5M2(_) => {
                None
            }
        }
    }

    /// Whether this scalar is truthy (non-zero and non-NaN).
    #[inline]
    pub fn is_truthy(&self) -> bool {
        match *self {
            Self::F32(x) => x != 0.0 && !x.is_nan(),
            Self::F16(b) => {
                let x = f16_bits_to_f64(b);
                x != 0.0 && !x.is_nan()
            }
            Self::BF16(b) => {
                let x = bf16_bits_to_f64(b);
                x != 0.0 && !x.is_nan()
            }
            Self::FP8E4M3(b) => {
                let x = fp8_e4m3_bits_to_f64(b);
                x != 0.0 && !x.is_nan()
            }
            Self::FP8E5M2(b) => {
                let x = fp8_e5m2_bits_to_f64(b);
                x != 0.0 && !x.is_nan()
            }
            Self::I32(n) => n != 0,
            Self::I16(n) => n != 0,
            Self::I8(n) => n != 0,
            Self::I4(n) => n != 0,
            Self::U64(n) => n != 0,
            Self::U32(n) => n != 0,
            Self::U16(n) => n != 0,
            Self::U8(n) => n != 0,
        }
    }

    /// Exact hardware bit key for SSA constant folding / CSE (preserves `-0.0` and distinct NaN bits).
    pub fn bit_key(&self) -> (DType, u64) {
        let bits = match *self {
            Self::F32(x) => x.to_bits() as u64,
            Self::F16(b) => b as u64,
            Self::BF16(b) => b as u64,
            Self::FP8E4M3(b) => b as u64,
            Self::FP8E5M2(b) => b as u64,
            Self::I32(n) => (n as u32) as u64,
            Self::I16(n) => (n as u16) as u64,
            Self::I8(n) => (n as u8) as u64,
            Self::I4(n) => pack_i4_nibble(n) as u64,
            Self::U64(n) => n,
            Self::U32(n) => n as u64,
            Self::U16(n) => n as u64,
            Self::U8(n) => n as u64,
        };
        (self.dtype(), bits)
    }

    /// Checked unary negation preserving exact hardware width.
    pub fn neg(&self) -> MoraResult<Self> {
        match *self {
            Self::F32(x) => Ok(Self::F32(-x)),
            Self::F16(b) => Ok(Self::F16(b ^ 0x8000)),
            Self::BF16(b) => Ok(Self::BF16(b ^ 0x8000)),
            Self::FP8E4M3(b) => Ok(Self::FP8E4M3(b ^ 0x80)),
            Self::FP8E5M2(b) => Ok(Self::FP8E5M2(b ^ 0x80)),
            Self::I32(n) => n.checked_neg().map(Self::I32).ok_or_else(|| {
                MoraError::Runtime("integer overflow in unary negation of i32::MIN".to_string())
            }),
            Self::I16(n) => n.checked_neg().map(Self::I16).ok_or_else(|| {
                MoraError::Runtime("integer overflow in unary negation of i16::MIN".to_string())
            }),
            Self::I8(n) => n.checked_neg().map(Self::I8).ok_or_else(|| {
                MoraError::Runtime("integer overflow in unary negation of i8::MIN".to_string())
            }),
            Self::I4(n) => {
                if n <= -8 {
                    Err(MoraError::Runtime(
                        "integer overflow in unary negation of -8i4".to_string(),
                    ))
                } else {
                    Ok(Self::I4(-n))
                }
            }
            Self::U64(_) | Self::U32(_) | Self::U16(_) | Self::U8(_) => {
                Err(MoraError::Runtime(format!(
                    "cannot negate unsigned integer scalar of dtype '{}'",
                    self.dtype()
                )))
            }
        }
    }

    /// Evaluate a binary operator on two scalars of the **same** hardware `DType`.
    pub fn eval_binary(op: &BinaryOp, lhs: Self, rhs: Self) -> MoraResult<ScalarBinaryOut> {
        if lhs.dtype() != rhs.dtype() {
            return Err(MoraError::Runtime(format!(
                "DType mismatch: cannot apply {:?} to '{}' and '{}' without explicit cast",
                op,
                lhs.dtype(),
                rhs.dtype()
            )));
        }
        let dt = lhs.dtype();

        if dt.is_float() {
            let a = lhs.as_f64();
            let b = rhs.as_f64();
            let out = match op {
                BinaryOp::Equal => return Ok(ScalarBinaryOut::Bool(a == b)),
                BinaryOp::NotEqual => return Ok(ScalarBinaryOut::Bool(a != b)),
                BinaryOp::Greater => return Ok(ScalarBinaryOut::Bool(a > b)),
                BinaryOp::Less => return Ok(ScalarBinaryOut::Bool(a < b)),
                BinaryOp::GreaterEqual => return Ok(ScalarBinaryOut::Bool(a >= b)),
                BinaryOp::LessEqual => return Ok(ScalarBinaryOut::Bool(a <= b)),
                BinaryOp::Add => {
                    if dt == DType::F32 {
                        return Ok(ScalarBinaryOut::Scalar(Self::F32((a as f32) + (b as f32))));
                    }
                    a + b
                }
                BinaryOp::Sub => {
                    if dt == DType::F32 {
                        return Ok(ScalarBinaryOut::Scalar(Self::F32((a as f32) - (b as f32))));
                    }
                    a - b
                }
                BinaryOp::Mul => {
                    if dt == DType::F32 {
                        return Ok(ScalarBinaryOut::Scalar(Self::F32((a as f32) * (b as f32))));
                    }
                    a * b
                }
                BinaryOp::Div => {
                    if dt == DType::F32 {
                        return Ok(ScalarBinaryOut::Scalar(Self::F32((a as f32) / (b as f32))));
                    }
                    a / b
                }
                BinaryOp::Mod => {
                    if b == 0.0 {
                        return Err(MoraError::Runtime(format!(
                            "modulo by zero in '{dt}' arithmetic"
                        )));
                    }
                    a % b
                }
                BinaryOp::Pow => a.powf(b),
                BinaryOp::MatMul => {
                    return Err(MoraError::Runtime(format!(
                        "operator '@' is not defined for scalar dtype '{dt}'"
                    )));
                }
                BinaryOp::BitAnd
                | BinaryOp::BitOr
                | BinaryOp::BitXor
                | BinaryOp::Shl
                | BinaryOp::Shr => {
                    return Err(MoraError::Runtime(format!(
                        "bitwise operator '{op:?}' is not defined for floating-point dtype '{dt}'"
                    )));
                }
            };
            let scalar = Self::from_f64_quantized(out, dt).ok_or_else(|| {
                MoraError::Runtime(format!("cannot quantize scalar result to '{dt}'"))
            })?;
            return Ok(ScalarBinaryOut::Scalar(scalar));
        }

        // Exact integer arithmetic with checked overflow for every signed/unsigned width
        if let (Self::U64(a), Self::U64(b)) = (lhs, rhs) {
            return eval_u64_binary(op, a, b);
        }

        let a = lhs.as_i64().ok_or_else(|| {
            MoraError::Runtime(format!("internal error: non-integer scalar '{dt}'"))
        })?;
        let b = rhs.as_i64().ok_or_else(|| {
            MoraError::Runtime(format!("internal error: non-integer scalar '{dt}'"))
        })?;

        match op {
            BinaryOp::Equal => return Ok(ScalarBinaryOut::Bool(a == b)),
            BinaryOp::NotEqual => return Ok(ScalarBinaryOut::Bool(a != b)),
            BinaryOp::Greater => return Ok(ScalarBinaryOut::Bool(a > b)),
            BinaryOp::Less => return Ok(ScalarBinaryOut::Bool(a < b)),
            BinaryOp::GreaterEqual => return Ok(ScalarBinaryOut::Bool(a >= b)),
            BinaryOp::LessEqual => return Ok(ScalarBinaryOut::Bool(a <= b)),
            BinaryOp::MatMul => {
                return Err(MoraError::Runtime(format!(
                    "operator '@' is not defined for scalar dtype '{dt}'"
                )));
            }
            _ => {}
        }

        let res_i128: i128 = match op {
            BinaryOp::Add => (a as i128) + (b as i128),
            BinaryOp::Sub => (a as i128) - (b as i128),
            BinaryOp::Mul => (a as i128) * (b as i128),
            BinaryOp::Div => {
                if b == 0 {
                    return Err(MoraError::Runtime(format!(
                        "division by zero in '{dt}' arithmetic"
                    )));
                }
                (a as i128) / (b as i128)
            }
            BinaryOp::Mod => {
                if b == 0 {
                    return Err(MoraError::Runtime(format!(
                        "modulo by zero in '{dt}' arithmetic"
                    )));
                }
                (a as i128) % (b as i128)
            }
            BinaryOp::Pow => {
                if b < 0 {
                    return Err(MoraError::Runtime(format!(
                        "negative exponent {b} is not valid for integer dtype '{dt}'"
                    )));
                }
                let exp = u32::try_from(b).map_err(|_| {
                    MoraError::Runtime(format!("exponent {b} overflows '{dt}' power"))
                })?;
                (a as i128).checked_pow(exp).ok_or_else(|| {
                    MoraError::Runtime(format!("integer overflow in '{dt}' power"))
                })?
            }
            BinaryOp::BitAnd => (a & b) as i128,
            BinaryOp::BitOr => (a | b) as i128,
            BinaryOp::BitXor => (a ^ b) as i128,
            BinaryOp::Shl => {
                let shift = (b as u32) & 63;
                a.wrapping_shl(shift) as i128
            }
            BinaryOp::Shr => {
                let shift = (b as u32) & 63;
                a.wrapping_shr(shift) as i128
            }
            _ => unreachable!(),
        };

        let res_i64 = i64::try_from(res_i128).map_err(|_| {
            MoraError::Runtime(format!(
                "integer overflow: result {res_i128} out of range for '{dt}'"
            ))
        })?;
        let scalar = Self::from_i64_checked(res_i64, dt)?;
        Ok(ScalarBinaryOut::Scalar(scalar))
    }
}

fn eval_u64_binary(op: &BinaryOp, a: u64, b: u64) -> MoraResult<ScalarBinaryOut> {
    match op {
        BinaryOp::Equal => Ok(ScalarBinaryOut::Bool(a == b)),
        BinaryOp::NotEqual => Ok(ScalarBinaryOut::Bool(a != b)),
        BinaryOp::Greater => Ok(ScalarBinaryOut::Bool(a > b)),
        BinaryOp::Less => Ok(ScalarBinaryOut::Bool(a < b)),
        BinaryOp::GreaterEqual => Ok(ScalarBinaryOut::Bool(a >= b)),
        BinaryOp::LessEqual => Ok(ScalarBinaryOut::Bool(a <= b)),
        BinaryOp::Add => a
            .checked_add(b)
            .map(|r| ScalarBinaryOut::Scalar(ScalarValue::U64(r)))
            .ok_or_else(|| MoraError::Runtime(format!("u64 overflow: {a} + {b}"))),
        BinaryOp::Sub => a
            .checked_sub(b)
            .map(|r| ScalarBinaryOut::Scalar(ScalarValue::U64(r)))
            .ok_or_else(|| MoraError::Runtime(format!("u64 underflow: {a} - {b}"))),
        BinaryOp::Mul => a
            .checked_mul(b)
            .map(|r| ScalarBinaryOut::Scalar(ScalarValue::U64(r)))
            .ok_or_else(|| MoraError::Runtime(format!("u64 overflow: {a} * {b}"))),
        BinaryOp::Div => a
            .checked_div(b)
            .map(|r| ScalarBinaryOut::Scalar(ScalarValue::U64(r)))
            .ok_or_else(|| MoraError::Runtime("division by zero in 'u64' arithmetic".to_string())),
        BinaryOp::Mod => a
            .checked_rem(b)
            .map(|r| ScalarBinaryOut::Scalar(ScalarValue::U64(r)))
            .ok_or_else(|| MoraError::Runtime("modulo by zero in 'u64' arithmetic".to_string())),
        BinaryOp::Pow => {
            let exp = u32::try_from(b)
                .map_err(|_| MoraError::Runtime(format!("exponent {b} overflows 'u64' power")))?;
            a.checked_pow(exp)
                .map(|r| ScalarBinaryOut::Scalar(ScalarValue::U64(r)))
                .ok_or_else(|| MoraError::Runtime(format!("u64 overflow: {a} ** {b}")))
        }
        BinaryOp::MatMul => Err(MoraError::Runtime(
            "operator '@' is not defined for scalar dtype 'u64'".to_string(),
        )),
        BinaryOp::BitAnd => Ok(ScalarBinaryOut::Scalar(ScalarValue::U64(a & b))),
        BinaryOp::BitOr => Ok(ScalarBinaryOut::Scalar(ScalarValue::U64(a | b))),
        BinaryOp::BitXor => Ok(ScalarBinaryOut::Scalar(ScalarValue::U64(a ^ b))),
        BinaryOp::Shl => Ok(ScalarBinaryOut::Scalar(ScalarValue::U64(
            a.wrapping_shl((b as u32) & 63),
        ))),
        BinaryOp::Shr => Ok(ScalarBinaryOut::Scalar(ScalarValue::U64(
            a.wrapping_shr((b as u32) & 63),
        ))),
    }
}

impl PartialEq for ScalarValue {
    fn eq(&self, other: &Self) -> bool {
        if self.dtype() != other.dtype() {
            return false;
        }
        match (*self, *other) {
            (Self::F32(a), Self::F32(b)) => a == b,
            (Self::F16(a), Self::F16(b)) => f16_bits_to_f64(a) == f16_bits_to_f64(b),
            (Self::BF16(a), Self::BF16(b)) => bf16_bits_to_f64(a) == bf16_bits_to_f64(b),
            (Self::FP8E4M3(a), Self::FP8E4M3(b)) => {
                fp8_e4m3_bits_to_f64(a) == fp8_e4m3_bits_to_f64(b)
            }
            (Self::FP8E5M2(a), Self::FP8E5M2(b)) => {
                fp8_e5m2_bits_to_f64(a) == fp8_e5m2_bits_to_f64(b)
            }
            (Self::I32(a), Self::I32(b)) => a == b,
            (Self::I16(a), Self::I16(b)) => a == b,
            (Self::I8(a), Self::I8(b)) => a == b,
            (Self::I4(a), Self::I4(b)) => a == b,
            (Self::U64(a), Self::U64(b)) => a == b,
            (Self::U32(a), Self::U32(b)) => a == b,
            (Self::U16(a), Self::U16(b)) => a == b,
            (Self::U8(a), Self::U8(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for ScalarValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.dtype().is_float() {
            let v = self.as_f64();
            if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
                write!(f, "{v:.1}")
            } else {
                write!(f, "{v}")
            }
        } else {
            match *self {
                Self::I32(n) => write!(f, "{n}"),
                Self::I16(n) => write!(f, "{n}"),
                Self::I8(n) => write!(f, "{n}"),
                Self::I4(n) => write!(f, "{n}"),
                Self::U64(n) => write!(f, "{n}"),
                Self::U32(n) => write!(f, "{n}"),
                Self::U16(n) => write!(f, "{n}"),
                Self::U8(n) => write!(f, "{n}"),
                _ => unreachable!(),
            }
        }
    }
}

// ============================================================================
// Tensor Hardware Compute Device (`Device`)
// ============================================================================

/// Hardware compute device placement for tensors (Wave 3 — Stage 3B).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Device {
    #[default]
    Cpu,
    Cuda(u16),
    Metal(u16),
    Vulkan(u16),
}

impl Device {
    /// Parse a device descriptor (`"cpu"`, `"cuda"`, `"cuda:0"`, `"metal"`, `"metal:0"`, `"vulkan"`, `"vulkan:0"`).
    pub fn parse(raw: &str) -> MoraResult<Self> {
        let s = raw.trim().to_ascii_lowercase();
        if s == "cpu" {
            return Ok(Self::Cpu);
        }
        if s == "cuda" {
            return Ok(Self::Cuda(0));
        }
        if let Some(idx_str) = s.strip_prefix("cuda:") {
            let idx = idx_str
                .trim()
                .parse::<u16>()
                .map_err(|_| MoraError::Runtime(format!("Invalid CUDA device index in '{raw}'")))?;
            return Ok(Self::Cuda(idx));
        }
        if s == "metal" {
            return Ok(Self::Metal(0));
        }
        if let Some(idx_str) = s.strip_prefix("metal:") {
            let idx = idx_str.trim().parse::<u16>().map_err(|_| {
                MoraError::Runtime(format!("Invalid Metal device index in '{raw}'"))
            })?;
            return Ok(Self::Metal(idx));
        }
        if s == "vulkan" {
            return Ok(Self::Vulkan(0));
        }
        if let Some(idx_str) = s.strip_prefix("vulkan:") {
            let idx = idx_str.trim().parse::<u16>().map_err(|_| {
                MoraError::Runtime(format!("Invalid Vulkan device index in '{raw}'"))
            })?;
            return Ok(Self::Vulkan(idx));
        }
        Err(MoraError::Runtime(format!(
            "Unknown device '{raw}' (expected 'cpu', 'cuda:N', 'metal:N', or 'vulkan:N')"
        )))
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cpu => write!(f, "cpu"),
            Self::Cuda(id) => write!(f, "cuda:{id}"),
            Self::Metal(id) => write!(f, "metal:{id}"),
            Self::Vulkan(id) => write!(f, "vulkan:{id}"),
        }
    }
}
