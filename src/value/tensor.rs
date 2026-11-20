//! Wave 3 — Stage 3B-1: First-class `Tensor<DType, Shape, Device>` runtime representation.
//!
//! Architectural guarantees:
//! 1. **64-byte cache-line aligned, compact hardware-packed storage** ([`AlignedByteBuffer`] inside
//!    [`TensorStorage`]): elements are physically stored at their true hardware bit width
//!    (`F64`/`I64`/`U64` = 8B, `F32`/`I32`/`U32` = 4B, `F16`/`BF16`/`I16`/`U16` = 2B,
//!    `FP8E4M3`/`FP8E5M2`/`I8`/`U8`/`Bool` = 1B, `I4` = 4-bit nibble-packed 2 elems/byte).
//! 2. **Zero-copy strided views**: `transpose`, contiguous `reshape`, and ND axis-0 indexing
//!    share the underlying `Arc<TensorStorage>` by adjusting `shape`, `strides`, and `offset`
//!    with `is_view = true`.
//! 3. **Hardware `DType` & `Device` fidelity**: every element written into or read from a tensor
//!    uses exact bit-level hardware codecs, preserving `Value::Scalar(ScalarValue)` / `Value::Bool` /
//!    `Value::Int` / `Value::Float` on ingestion, indexing, reductions, and 1D dot products;
//!    cross-device or cross-dtype operations without explicit `.to(...)` fail closed.
//! 4. **Autograd & Resource safety**: in-place mutations (`fill_`, `add_`, `mul_`) are rejected
//!    when `requires_grad` is active or when the storage is aliased by multiple views
//!    (`Arc::strong_count(&storage) > 1 || is_view`).

use std::sync::Arc;

use parking_lot::RwLock;

use crate::common::{
    BinaryOp, DType, Device, ScalarValue, bf16_bits_to_f64, f16_bits_to_f64, f64_to_bf16_bits,
    f64_to_f16_bits, f64_to_fp8_e4m3_bits, f64_to_fp8_e5m2_bits, fp8_e4m3_bits_to_f64,
    fp8_e5m2_bits_to_f64, pack_i4_nibble, unpack_i4_nibble,
};
use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::runtime::autograd::{AutogradContext, TapeNodeId, TapeOp};
use crate::value::Value;
use crate::value::list::List;

/// 64-byte aligned cache-line block (`[u8; 64]`) for SIMD / AVX-512 / DMA alignment.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy)]
struct AlignedCacheLine([u8; 64]);

/// Compact 64-byte aligned hardware-packed byte buffer storing elements at exact `DType` width.
#[derive(Debug, Clone)]
pub struct AlignedByteBuffer {
    blocks: Vec<AlignedCacheLine>,
    byte_len: usize,
    dtype: DType,
    len: usize,
}

impl AlignedByteBuffer {
    /// Allocate a zero-initialized compact buffer for `len` elements of `dtype`.
    pub fn zeroed(len: usize, dtype: DType) -> Self {
        let byte_len = dtype.storage_bytes(len);
        let num_blocks = byte_len.div_ceil(64).max(1);
        Self {
            blocks: vec![AlignedCacheLine([0u8; 64]); num_blocks],
            byte_len,
            dtype,
            len,
        }
    }

    /// Allocate a compact buffer filled with `val` encoded into `dtype`.
    pub fn with_value(len: usize, val: f64, dtype: DType) -> Self {
        let mut buf = Self::zeroed(len, dtype);
        for i in 0..len {
            buf.set_f64(i, val);
        }
        buf
    }

    /// Allocate a compact buffer initialized from `data` encoded into `dtype`.
    pub fn from_f64_slice(data: &[f64], dtype: DType) -> Self {
        let mut buf = Self::zeroed(data.len(), dtype);
        for (i, &v) in data.iter().enumerate() {
            buf.set_f64(i, v);
        }
        buf
    }

    /// Allocate a compact buffer initialized losslessly from `Value`s encoded into `dtype`.
    pub fn from_values(values: &[Value], dtype: DType) -> Self {
        let mut buf = Self::zeroed(values.len(), dtype);
        for (i, v) in values.iter().enumerate() {
            buf.set_value(i, v);
        }
        buf
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn dtype(&self) -> DType {
        self.dtype
    }

    /// Physical packed storage size in bytes (e.g. `i4` uses `ceil(len / 2)` bytes).
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Return the 64-byte aligned base byte pointer for SIMD / GPU DMA / native kernel dispatch.
    #[inline]
    pub fn aligned_byte_ptr(&self) -> *const u8 {
        self.blocks.as_ptr() as *const u8
    }

    #[inline]
    fn read_byte(&self, byte_idx: usize) -> u8 {
        debug_assert!(byte_idx < self.byte_len);
        self.blocks[byte_idx / 64].0[byte_idx % 64]
    }

    #[inline]
    fn write_byte(&mut self, byte_idx: usize, byte: u8) {
        debug_assert!(byte_idx < self.byte_len);
        self.blocks[byte_idx / 64].0[byte_idx % 64] = byte;
    }

    #[inline]
    fn read_bytes<const N: usize>(&self, start_byte: usize) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.read_byte(start_byte + i);
        }
        out
    }

    #[inline]
    fn write_bytes<const N: usize>(&mut self, start_byte: usize, bytes: [u8; N]) {
        for (i, b) in bytes.into_iter().enumerate() {
            self.write_byte(start_byte + i, b);
        }
    }

    /// Decode element `idx` as `f64` for arithmetic kernels.
    pub fn get_f64(&self, idx: usize) -> f64 {
        debug_assert!(idx < self.len);
        match self.dtype {
            DType::F64 => f64::from_le_bytes(self.read_bytes::<8>(idx * 8)),
            DType::F32 => f32::from_le_bytes(self.read_bytes::<4>(idx * 4)) as f64,
            DType::F16 => f16_bits_to_f64(u16::from_le_bytes(self.read_bytes::<2>(idx * 2))),
            DType::BF16 => bf16_bits_to_f64(u16::from_le_bytes(self.read_bytes::<2>(idx * 2))),
            DType::FP8E4M3 => fp8_e4m3_bits_to_f64(self.read_byte(idx)),
            DType::FP8E5M2 => fp8_e5m2_bits_to_f64(self.read_byte(idx)),
            DType::I64 => i64::from_le_bytes(self.read_bytes::<8>(idx * 8)) as f64,
            DType::I32 => i32::from_le_bytes(self.read_bytes::<4>(idx * 4)) as f64,
            DType::I16 => i16::from_le_bytes(self.read_bytes::<2>(idx * 2)) as f64,
            DType::I8 => (self.read_byte(idx) as i8) as f64,
            DType::I4 => {
                let b = self.read_byte(idx / 2);
                let nib = if idx.is_multiple_of(2) {
                    b & 0x0f
                } else {
                    b >> 4
                };
                unpack_i4_nibble(nib) as f64
            }
            DType::U64 => u64::from_le_bytes(self.read_bytes::<8>(idx * 8)) as f64,
            DType::U32 => u32::from_le_bytes(self.read_bytes::<4>(idx * 4)) as f64,
            DType::U16 => u16::from_le_bytes(self.read_bytes::<2>(idx * 2)) as f64,
            DType::U8 => self.read_byte(idx) as f64,
            DType::Bool => {
                if self.read_byte(idx) != 0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    /// Decode element `idx` into its exact Mora runtime [`Value`] (`Value::Int`, `Value::Float`,
    /// `Value::Bool`, or `Value::Scalar(ScalarValue)`).
    pub fn get_value(&self, idx: usize) -> Value {
        debug_assert!(idx < self.len);
        match self.dtype {
            DType::F64 => Value::Float(f64::from_le_bytes(self.read_bytes::<8>(idx * 8))),
            DType::F32 => Value::Scalar(ScalarValue::F32(f32::from_le_bytes(
                self.read_bytes::<4>(idx * 4),
            ))),
            DType::F16 => Value::Scalar(ScalarValue::F16(u16::from_le_bytes(
                self.read_bytes::<2>(idx * 2),
            ))),
            DType::BF16 => Value::Scalar(ScalarValue::BF16(u16::from_le_bytes(
                self.read_bytes::<2>(idx * 2),
            ))),
            DType::FP8E4M3 => Value::Scalar(ScalarValue::FP8E4M3(self.read_byte(idx))),
            DType::FP8E5M2 => Value::Scalar(ScalarValue::FP8E5M2(self.read_byte(idx))),
            DType::I64 => Value::Int(i64::from_le_bytes(self.read_bytes::<8>(idx * 8))),
            DType::I32 => Value::Scalar(ScalarValue::I32(i32::from_le_bytes(
                self.read_bytes::<4>(idx * 4),
            ))),
            DType::I16 => Value::Scalar(ScalarValue::I16(i16::from_le_bytes(
                self.read_bytes::<2>(idx * 2),
            ))),
            DType::I8 => Value::Scalar(ScalarValue::I8(self.read_byte(idx) as i8)),
            DType::I4 => {
                let b = self.read_byte(idx / 2);
                let nib = if idx.is_multiple_of(2) {
                    b & 0x0f
                } else {
                    b >> 4
                };
                Value::Scalar(ScalarValue::I4(unpack_i4_nibble(nib)))
            }
            DType::U64 => Value::Scalar(ScalarValue::U64(u64::from_le_bytes(
                self.read_bytes::<8>(idx * 8),
            ))),
            DType::U32 => Value::Scalar(ScalarValue::U32(u32::from_le_bytes(
                self.read_bytes::<4>(idx * 4),
            ))),
            DType::U16 => Value::Scalar(ScalarValue::U16(u16::from_le_bytes(
                self.read_bytes::<2>(idx * 2),
            ))),
            DType::U8 => Value::Scalar(ScalarValue::U8(self.read_byte(idx))),
            DType::Bool => Value::Bool(self.read_byte(idx) != 0),
        }
    }

    /// Encode `val` (`f64`) into element `idx` at exact hardware width.
    pub fn set_f64(&mut self, idx: usize, val: f64) {
        debug_assert!(idx < self.len);
        match self.dtype {
            DType::F64 => self.write_bytes(idx * 8, val.to_le_bytes()),
            DType::F32 => self.write_bytes(idx * 4, (val as f32).to_le_bytes()),
            DType::F16 => self.write_bytes(idx * 2, f64_to_f16_bits(val).to_le_bytes()),
            DType::BF16 => self.write_bytes(idx * 2, f64_to_bf16_bits(val).to_le_bytes()),
            DType::FP8E4M3 => self.write_byte(idx, f64_to_fp8_e4m3_bits(val)),
            DType::FP8E5M2 => self.write_byte(idx, f64_to_fp8_e5m2_bits(val)),
            DType::I64 => {
                let n = if val.is_finite() { val as i64 } else { 0 };
                self.write_bytes(idx * 8, n.to_le_bytes());
            }
            DType::I32 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as i32
                } else {
                    0
                };
                self.write_bytes(idx * 4, n.to_le_bytes());
            }
            DType::I16 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as i16
                } else {
                    0
                };
                self.write_bytes(idx * 2, n.to_le_bytes());
            }
            DType::I8 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as i8
                } else {
                    0
                };
                self.write_byte(idx, n as u8);
            }
            DType::I4 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as i8
                } else {
                    0
                };
                let nib = pack_i4_nibble(n);
                let byte_idx = idx / 2;
                let cur = self.read_byte(byte_idx);
                let next = if idx.is_multiple_of(2) {
                    (cur & 0xf0) | nib
                } else {
                    (cur & 0x0f) | (nib << 4)
                };
                self.write_byte(byte_idx, next);
            }
            DType::U64 => {
                let n = if val.is_finite() && val >= 0.0 {
                    val as u64
                } else {
                    0
                };
                self.write_bytes(idx * 8, n.to_le_bytes());
            }
            DType::U32 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as u32
                } else {
                    0
                };
                self.write_bytes(idx * 4, n.to_le_bytes());
            }
            DType::U16 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as u16
                } else {
                    0
                };
                self.write_bytes(idx * 2, n.to_le_bytes());
            }
            DType::U8 => {
                let n = if val.is_finite() {
                    self.dtype.quantize_i64(val as i64) as u8
                } else {
                    0
                };
                self.write_byte(idx, n);
            }
            DType::Bool => {
                let b = u8::from(val != 0.0 && !val.is_nan());
                self.write_byte(idx, b);
            }
        }
    }

    /// Encode a runtime [`Value`] losslessly into element `idx` at exact hardware width.
    pub fn set_value(&mut self, idx: usize, val: &Value) {
        match (self.dtype, val) {
            (DType::I64, Value::Int(n)) => self.write_bytes(idx * 8, n.to_le_bytes()),
            (DType::U64, Value::Scalar(ScalarValue::U64(n))) => {
                self.write_bytes(idx * 8, n.to_le_bytes())
            }
            (DType::U64, Value::Int(n)) => {
                self.write_bytes(idx * 8, (*n).max(0).cast_unsigned().to_le_bytes())
            }
            (DType::F16, Value::Scalar(ScalarValue::F16(bits))) => {
                self.write_bytes(idx * 2, bits.to_le_bytes())
            }
            (DType::BF16, Value::Scalar(ScalarValue::BF16(bits))) => {
                self.write_bytes(idx * 2, bits.to_le_bytes())
            }
            (DType::FP8E4M3, Value::Scalar(ScalarValue::FP8E4M3(bits))) => {
                self.write_byte(idx, *bits)
            }
            (DType::FP8E5M2, Value::Scalar(ScalarValue::FP8E5M2(bits))) => {
                self.write_byte(idx, *bits)
            }
            (DType::Bool, Value::Bool(b)) => self.write_byte(idx, u8::from(*b)),
            (_, Value::Int(n)) => self.set_f64(idx, *n as f64),
            (_, Value::Float(f)) => self.set_f64(idx, *f),
            (_, Value::Scalar(s)) => self.set_f64(idx, s.as_f64()),
            (_, Value::Bool(b)) => self.set_f64(idx, if *b { 1.0 } else { 0.0 }),
            _ => self.set_f64(idx, 0.0),
        }
    }
}

/// Shared compact contiguous storage backing one or more [`Tensor`] views.
#[derive(Debug)]
pub struct TensorStorage {
    data: RwLock<AlignedByteBuffer>,
}

impl TensorStorage {
    pub fn new(buf: AlignedByteBuffer) -> Arc<Self> {
        Arc::new(Self {
            data: RwLock::new(buf),
        })
    }

    /// Physical packed storage byte length.
    pub fn byte_len(&self) -> usize {
        self.data.read().byte_len()
    }

    /// 64-byte aligned base byte pointer for SIMD / GPU DMA kernels.
    pub fn aligned_byte_ptr(&self) -> *const u8 {
        self.data.read().aligned_byte_ptr()
    }
}

/// First-class strided N-dimensional tensor value (`Value::Tensor(Arc<Tensor>)`).
#[derive(Debug, Clone)]
pub struct Tensor {
    pub storage: Arc<TensorStorage>,
    pub shape: Vec<usize>,
    pub strides: Vec<isize>,
    pub offset: usize,
    pub dtype: DType,
    pub device: Device,
    pub requires_grad: bool,
    pub is_view: bool,
    /// Stage 3C: Optional active reverse-mode autograd tape node `(tape_id, node_idx)`.
    pub grad_node: Option<TapeNodeId>,
}

/// Wave 3 Stage 3C: Runtime axis specification for zero-copy multi-dimensional slicing `t[0..4, :, -1]`.
#[derive(Debug, Clone, Copy)]
pub enum RuntimeSliceAxis {
    Index(isize),
    Range {
        start: Option<isize>,
        stop: Option<isize>,
        step: Option<isize>,
    },
    Ellipsis,
    NewAxis,
}

/// Compute contiguous row-major (C-order) strides for `shape`.
pub fn contiguous_strides(shape: &[usize]) -> Vec<isize> {
    if shape.is_empty() {
        return Vec::new();
    }
    let mut strides = vec![1isize; shape.len()];
    for i in (0..shape.len() - 1).rev() {
        strides[i] = strides[i + 1] * (shape[i + 1] as isize);
    }
    strides
}

/// Checked total element count of `shape`.
pub fn checked_numel(shape: &[usize]) -> MoraResult<usize> {
    if shape.is_empty() {
        return Ok(1);
    }
    let mut total = 1usize;
    for &d in shape {
        if d == 0 {
            return Err(MoraError::Runtime(
                "Tensor shape dimensions must be positive (> 0)".to_string(),
            ));
        }
        total = total.checked_mul(d).ok_or_else(|| {
            MoraError::Runtime(format!("Tensor shape {shape:?} overflows element count"))
        })?;
    }
    Ok(total)
}

/// Quantize a raw `f64` value according to `dtype`.
#[inline]
pub fn quantize_for_dtype(val: f64, dtype: DType) -> f64 {
    dtype.quantize_f64(val)
}

/// Convert a quantized scalar `f64` element to its exact Mora [`Value`] representation for `dtype`.
#[inline]
pub fn scalar_value_for_dtype(val: f64, dtype: DType) -> Value {
    match dtype {
        DType::F64 => Value::Float(val),
        DType::I64 => Value::Int(if val.is_finite() { val as i64 } else { 0 }),
        DType::Bool => Value::Bool(val != 0.0 && !val.is_nan()),
        other => match ScalarValue::from_f64_quantized(val, other) {
            Some(s) => Value::Scalar(s),
            None => Value::Float(val),
        },
    }
}

/// Extract numeric `f64` and its `DType` from a scalar Mora `Value`.
fn scalar_val_dtype_and_f64(v: &Value) -> Option<(DType, f64)> {
    match v {
        Value::Int(n) => Some((DType::I64, *n as f64)),
        Value::Float(f) => Some((DType::F64, *f)),
        Value::Scalar(s) => Some((s.dtype(), s.as_f64())),
        Value::Bool(b) => Some((DType::Bool, if *b { 1.0 } else { 0.0 })),
        _ => None,
    }
}

impl Tensor {
    /// Construct a contiguous compact tensor from raw `f64` elements quantized to `dtype`.
    pub fn from_raw(
        data: Vec<f64>,
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        let expected = checked_numel(&shape)?;
        if data.len() != expected {
            return Err(MoraError::Runtime(format!(
                "Tensor data length {} does not match shape {:?} ({} elements)",
                data.len(),
                shape,
                expected
            )));
        }
        let strides = contiguous_strides(&shape);
        Ok(Arc::new(Self {
            storage: TensorStorage::new(AlignedByteBuffer::from_f64_slice(&data, dtype)),
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_node: None,
        }))
    }

    /// Construct a contiguous compact tensor losslessly from `Value`s.
    pub fn from_values(
        values: &[Value],
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        let expected = checked_numel(&shape)?;
        if values.len() != expected {
            return Err(MoraError::Runtime(format!(
                "Tensor data length {} does not match shape {:?} ({} elements)",
                values.len(),
                shape,
                expected
            )));
        }
        let strides = contiguous_strides(&shape);
        Ok(Arc::new(Self {
            storage: TensorStorage::new(AlignedByteBuffer::from_values(values, dtype)),
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_node: None,
        }))
    }

    pub fn zeros(
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        let numel = checked_numel(&shape)?;
        let strides = contiguous_strides(&shape);
        Ok(Arc::new(Self {
            storage: TensorStorage::new(AlignedByteBuffer::zeroed(numel, dtype)),
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_node: None,
        }))
    }

    pub fn ones(
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        Self::full(shape, 1.0, dtype, device, requires_grad)
    }

    pub fn full(
        shape: Vec<usize>,
        fill_val: f64,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        let numel = checked_numel(&shape)?;
        let strides = contiguous_strides(&shape);
        Ok(Arc::new(Self {
            storage: TensorStorage::new(AlignedByteBuffer::with_value(numel, fill_val, dtype)),
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_node: None,
        }))
    }

    pub fn eye(
        n: usize,
        m: usize,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        if n == 0 || m == 0 {
            return Err(MoraError::Runtime(
                "Tensor::eye: dimensions n and m must be > 0".to_string(),
            ));
        }
        let mut data = vec![0.0; n * m];
        for i in 0..n.min(m) {
            data[i * m + i] = 1.0;
        }
        Self::from_raw(data, vec![n, m], dtype, device, requires_grad)
    }

    pub fn arange(
        start: f64,
        end: f64,
        step: f64,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        if step == 0.0 || !step.is_finite() || !start.is_finite() || !end.is_finite() {
            return Err(MoraError::Runtime(
                "Tensor::arange: start, end, and non-zero step must be finite".to_string(),
            ));
        }
        let mut data = Vec::new();
        let mut cur = start;
        if step > 0.0 {
            while cur < end {
                data.push(cur);
                cur += step;
            }
        } else {
            while cur > end {
                data.push(cur);
                cur += step;
            }
        }
        if data.is_empty() {
            return Err(MoraError::Runtime(
                "Tensor::arange produced an empty range".to_string(),
            ));
        }
        let len = data.len();
        Self::from_raw(data, vec![len], dtype, device, requires_grad)
    }

    /// Standard normal `N(0, 1)` random tensor drawn deterministically from `CoreRuntime.random_state`.
    pub fn randn(
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
        core: &mut CoreRuntime,
    ) -> MoraResult<Arc<Self>> {
        if !dtype.is_float() {
            return Err(MoraError::Runtime(format!(
                "Tensor::randn requires a floating-point dtype, got '{dtype}'"
            )));
        }
        let numel = checked_numel(&shape)?;
        let mut data = Vec::with_capacity(numel);
        for _ in 0..numel {
            data.push(core.random_state.next_gauss());
        }
        Self::from_raw(data, shape, dtype, device, requires_grad)
    }

    /// Uniform `[0, 1)` random tensor drawn deterministically from `CoreRuntime.random_state`.
    pub fn rand(
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
        core: &mut CoreRuntime,
    ) -> MoraResult<Arc<Self>> {
        if !dtype.is_float() {
            return Err(MoraError::Runtime(format!(
                "Tensor::rand requires a floating-point dtype, got '{dtype}'"
            )));
        }
        let numel = checked_numel(&shape)?;
        let mut data = Vec::with_capacity(numel);
        for _ in 0..numel {
            data.push(core.random_state.next_f64());
        }
        Self::from_raw(data, shape, dtype, device, requires_grad)
    }

    /// Construct a `Tensor` from a nested [`Value::List`] (or clone/cast an existing `Value::Tensor`),
    /// faithfully inferring hardware `DType` from `Value::Scalar`, `Value::Bool`, `Value::Int`, `Value::Float`.
    pub fn from_value(
        val: &Value,
        explicit_dtype: Option<DType>,
        explicit_device: Option<Device>,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        if let Value::Tensor(t) = val {
            let dt = explicit_dtype.unwrap_or(t.dtype);
            let dev = explicit_device.unwrap_or_else(|| t.device.clone());
            let vals = t.to_flat_values();
            return Self::from_values(&vals, t.shape.clone(), dt, dev, requires_grad);
        }

        let mut inferred_dtype: Option<DType> = None;
        let mut shape = Vec::new();
        let mut flat = Vec::new();
        flatten_nested_value(val, 0, &mut shape, &mut flat, &mut inferred_dtype)?;
        if shape.is_empty() {
            shape.push(1);
        }
        let dt = explicit_dtype.unwrap_or(inferred_dtype.unwrap_or(DType::F64));
        let dev = explicit_device.unwrap_or(Device::Cpu);
        Self::from_values(&flat, shape, dt, dev, requires_grad)
    }

    #[inline]
    pub fn ndim(&self) -> usize {
        self.shape.len()
    }

    #[inline]
    pub fn rank(&self) -> usize {
        self.shape.len()
    }

    #[inline]
    pub fn numel(&self) -> usize {
        self.shape.iter().copied().product::<usize>().max(1)
    }

    /// Compact hardware storage footprint in bytes for this tensor's logical elements.
    #[inline]
    pub fn nbytes(&self) -> usize {
        self.dtype.storage_bytes(self.numel())
    }

    /// Return whether this tensor view is laid out contiguously in row-major order.
    pub fn is_contiguous(&self) -> bool {
        self.offset == 0 && self.strides == contiguous_strides(&self.shape)
    }

    /// Read the element at multi-dimensional `coords` as `f64`.
    pub fn get_at(&self, coords: &[usize]) -> f64 {
        let mut idx = self.offset as isize;
        for (&c, &s) in coords.iter().zip(self.strides.iter()) {
            idx += (c as isize) * s;
        }
        let guard = self.storage.data.read();
        guard.get_f64(idx as usize)
    }

    fn physical_index(&self, mut linear_idx: usize) -> usize {
        if self.is_contiguous() {
            return self.offset + linear_idx;
        }
        let mut phys = self.offset as isize;
        for i in (0..self.shape.len()).rev() {
            let d = self.shape[i];
            let coord = linear_idx % d;
            linear_idx /= d;
            phys += (coord as isize) * self.strides[i];
        }
        phys as usize
    }

    /// Read the element at logical flat row-major index `linear_idx` (`0..self.numel()`) as `f64`.
    pub fn get_linear(&self, linear_idx: usize) -> f64 {
        let phys = self.physical_index(linear_idx);
        let guard = self.storage.data.read();
        guard.get_f64(phys)
    }

    /// Read the element at logical flat row-major index `linear_idx` as an exact Mora [`Value`].
    pub fn get_linear_value(&self, linear_idx: usize) -> Value {
        let phys = self.physical_index(linear_idx);
        let guard = self.storage.data.read();
        guard.get_value(phys)
    }

    /// Materialize the logical elements of this tensor view as `f64` in contiguous row-major order.
    pub fn to_flat_vec(&self) -> Vec<f64> {
        let n = self.numel();
        let guard = self.storage.data.read();
        if self.is_contiguous() {
            return (0..n).map(|i| guard.get_f64(self.offset + i)).collect();
        }
        drop(guard);
        (0..n).map(|i| self.get_linear(i)).collect()
    }

    /// Materialize the logical elements of this tensor view as exact Mora [`Value`]s.
    pub fn to_flat_values(&self) -> Vec<Value> {
        let n = self.numel();
        let guard = self.storage.data.read();
        if self.is_contiguous() {
            return (0..n).map(|i| guard.get_value(self.offset + i)).collect();
        }
        drop(guard);
        (0..n).map(|i| self.get_linear_value(i)).collect()
    }

    /// Return a contiguous owning tensor (fresh storage if this tensor is a view or aliased).
    pub fn contiguous(self: &Arc<Self>) -> MoraResult<Arc<Self>> {
        if self.is_contiguous() && !self.is_view && Arc::strong_count(&self.storage) == 1 {
            return Ok(Arc::clone(self));
        }
        let vals = self.to_flat_values();
        Self::from_values(
            &vals,
            self.shape.clone(),
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Zero-copy strided transpose swapping `dim0` and `dim1` (defaults to the last two axes).
    pub fn transpose(
        self: &Arc<Self>,
        dim0: Option<usize>,
        dim1: Option<usize>,
    ) -> MoraResult<Arc<Self>> {
        let rank = self.shape.len();
        if rank < 2 {
            return Err(MoraError::Runtime(format!(
                "Tensor.transpose requires at least a 2D tensor, got {rank}D shape {:?}",
                self.shape
            )));
        }
        let d0 = dim0.unwrap_or(rank - 2);
        let d1 = dim1.unwrap_or(rank - 1);
        if d0 >= rank || d1 >= rank {
            return Err(MoraError::Runtime(format!(
                "Tensor.transpose axes ({d0}, {d1}) out of bounds for {rank}D tensor"
            )));
        }
        let mut new_shape = self.shape.clone();
        let mut new_strides = self.strides.clone();
        new_shape.swap(d0, d1);
        new_strides.swap(d0, d1);
        Ok(Arc::new(Self {
            storage: Arc::clone(&self.storage),
            shape: new_shape,
            strides: new_strides,
            offset: self.offset,
            dtype: self.dtype,
            device: self.device.clone(),
            requires_grad: self.requires_grad,
            is_view: true,
            grad_node: None,
        }))
    }

    /// Reshape tensor to `new_shape`, returning a zero-copy view when contiguous.
    pub fn reshape(self: &Arc<Self>, new_shape: Vec<usize>) -> MoraResult<Arc<Self>> {
        let new_numel = checked_numel(&new_shape)?;
        let old_numel = self.numel();
        if new_numel != old_numel {
            return Err(MoraError::Runtime(format!(
                "Tensor.reshape: cannot reshape tensor of shape {:?} ({old_numel} elements) into {:?} ({new_numel} elements)",
                self.shape, new_shape
            )));
        }
        if self.is_contiguous() {
            let new_strides = contiguous_strides(&new_shape);
            return Ok(Arc::new(Self {
                storage: Arc::clone(&self.storage),
                shape: new_shape,
                strides: new_strides,
                offset: self.offset,
                dtype: self.dtype,
                device: self.device.clone(),
                requires_grad: self.requires_grad,
                is_view: true,
                grad_node: None,
            }));
        }
        let vals = self.to_flat_values();
        Self::from_values(
            &vals,
            new_shape,
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Transfer tensor to `device`.
    pub fn to_device(self: &Arc<Self>, device: Device) -> MoraResult<Arc<Self>> {
        if self.device == device && !self.is_view && Arc::strong_count(&self.storage) == 1 {
            return Ok(Arc::clone(self));
        }
        let vals = self.to_flat_values();
        Self::from_values(
            &vals,
            self.shape.clone(),
            self.dtype,
            device,
            self.requires_grad,
        )
    }

    /// Cast/quantize tensor to `dtype`.
    pub fn to_dtype(self: &Arc<Self>, dtype: DType) -> MoraResult<Arc<Self>> {
        if self.dtype == dtype && !self.is_view && Arc::strong_count(&self.storage) == 1 {
            return Ok(Arc::clone(self));
        }
        Self::from_raw(
            self.to_flat_vec(),
            self.shape.clone(),
            dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Return a copy of this tensor with `requires_grad` set to `flag`.
    pub fn with_grad(self: &Arc<Self>, flag: bool) -> Arc<Self> {
        let mut cloned = (**self).clone();
        cloned.requires_grad = flag;
        if !flag {
            cloned.grad_node = None;
        }
        Arc::new(cloned)
    }

    /// Return a copy of this tensor attached to an active autograd tape node.
    pub fn with_grad_node(self: &Arc<Self>, node: Option<TapeNodeId>) -> Arc<Self> {
        let mut cloned = (**self).clone();
        cloned.grad_node = node;
        if node.is_some() {
            cloned.requires_grad = true;
        }
        Arc::new(cloned)
    }

    /// Resource / Autograd safety check before any in-place mutation (`fill_`, `add_`, `mul_`).
    pub fn ensure_inplace_safe(&self, op_name: &str) -> MoraResult<()> {
        if self.requires_grad {
            return Err(MoraError::Runtime(format!(
                "Resource error in Tensor.{op_name}: in-place mutation on a gradient-tracked tensor (requires_grad=true) is forbidden as it corrupts the autograd tape"
            )));
        }
        if self.is_view || Arc::strong_count(&self.storage) > 1 {
            return Err(MoraError::Runtime(format!(
                "Resource error in Tensor.{op_name}: in-place mutation on an aliased or strided tensor view is forbidden; call .contiguous() first"
            )));
        }
        Ok(())
    }

    /// In-place fill (`fill_`), guarded by [`Self::ensure_inplace_safe`].
    pub fn fill_inplace(self: &Arc<Self>, val: &Value) -> MoraResult<Arc<Self>> {
        self.ensure_inplace_safe("fill_")?;
        let Some((val_dt, _)) = scalar_val_dtype_and_f64(val) else {
            return Err(MoraError::Runtime(format!(
                "Tensor.fill_ expects a scalar matching DType '{}', got {}",
                self.dtype,
                crate::flow::type_name(val)
            )));
        };
        if val_dt != self.dtype {
            return Err(MoraError::Runtime(format!(
                "DType mismatch in Tensor.fill_: cannot fill Tensor<{}> with '{}'",
                self.dtype, val_dt
            )));
        }
        let n = self.numel();
        let mut guard = self.storage.data.write();
        for i in 0..n {
            guard.set_value(self.offset + i, val);
        }
        drop(guard);
        Ok(Arc::clone(self))
    }

    /// In-place elementwise addition (`add_`), guarded by [`Self::ensure_inplace_safe`].
    pub fn add_inplace(self: &Arc<Self>, other: &Value) -> MoraResult<Arc<Self>> {
        self.ensure_inplace_safe("add_")?;
        let computed = eval_tensor_binary(&BinaryOp::Add, &Value::Tensor(Arc::clone(self)), other)?;
        let Value::Tensor(res) = computed else {
            unreachable!()
        };
        if res.shape != self.shape {
            return Err(MoraError::Runtime(format!(
                "Tensor.add_: broadcast result shape {:?} does not match in-place target shape {:?}",
                res.shape, self.shape
            )));
        }
        let flat = res.to_flat_values();
        let mut guard = self.storage.data.write();
        for (i, v) in flat.iter().enumerate() {
            guard.set_value(self.offset + i, v);
        }
        drop(guard);
        Ok(Arc::clone(self))
    }

    /// In-place elementwise multiplication (`mul_`), guarded by [`Self::ensure_inplace_safe`].
    pub fn mul_inplace(self: &Arc<Self>, other: &Value) -> MoraResult<Arc<Self>> {
        self.ensure_inplace_safe("mul_")?;
        let computed = eval_tensor_binary(&BinaryOp::Mul, &Value::Tensor(Arc::clone(self)), other)?;
        let Value::Tensor(res) = computed else {
            unreachable!()
        };
        if res.shape != self.shape {
            return Err(MoraError::Runtime(format!(
                "Tensor.mul_: broadcast result shape {:?} does not match in-place target shape {:?}",
                res.shape, self.shape
            )));
        }
        let flat = res.to_flat_values();
        let mut guard = self.storage.data.write();
        for (i, v) in flat.iter().enumerate() {
            guard.set_value(self.offset + i, v);
        }
        drop(guard);
        Ok(Arc::clone(self))
    }

    /// Convert this tensor back into a nested [`Value::List`] preserving exact element types.
    pub fn to_nested_list(&self) -> Value {
        let flat = self.to_flat_values();
        build_nested_list(&flat, &self.shape)
    }

    /// Apply a unary scalar transformation elementwise, preserving `shape`, `dtype`, `device`, and `requires_grad`.
    pub fn map_unary<F>(&self, mut f: F) -> MoraResult<Arc<Self>>
    where
        F: FnMut(f64) -> f64,
    {
        let flat: Vec<f64> = self.to_flat_vec().into_iter().map(&mut f).collect();
        Self::from_raw(
            flat,
            self.shape.clone(),
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Numerically stable softmax along the last axis.
    pub fn softmax(&self) -> MoraResult<Arc<Self>> {
        if !self.dtype.is_float() {
            return Err(MoraError::Runtime(format!(
                "Tensor.softmax requires a floating-point dtype, got {}",
                self.dtype
            )));
        }
        let last_dim = *self.shape.last().unwrap_or(&1);
        let mut flat = self.to_flat_vec();
        for row in flat.chunks_mut(last_dim) {
            let max_v = row.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut sum = 0.0;
            for v in row.iter_mut() {
                *v = (*v - max_v).exp();
                sum += *v;
            }
            if sum > 0.0 {
                for v in row.iter_mut() {
                    *v = quantize_for_dtype(*v / sum, self.dtype);
                }
            }
        }
        Self::from_raw(
            flat,
            self.shape.clone(),
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Root Mean Square Layer Normalization (`RMSNorm`) along the last axis.
    pub fn rms_norm(&self, eps: f64) -> MoraResult<Arc<Self>> {
        if !self.dtype.is_float() {
            return Err(MoraError::Runtime(format!(
                "Tensor.rms_norm requires a floating-point dtype, got {}",
                self.dtype
            )));
        }
        let last_dim = *self.shape.last().unwrap_or(&1);
        let mut flat = self.to_flat_vec();
        for row in flat.chunks_mut(last_dim) {
            let mean_sq = row.iter().map(|v| v * v).sum::<f64>() / (last_dim as f64);
            let inv_rms = 1.0 / (mean_sq + eps).sqrt();
            for v in row.iter_mut() {
                *v = quantize_for_dtype(*v * inv_rms, self.dtype);
            }
        }
        Self::from_raw(
            flat,
            self.shape.clone(),
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Standard Layer Normalization (`LayerNorm`) along the last axis.
    pub fn layer_norm(&self, eps: f64) -> MoraResult<Arc<Self>> {
        if !self.dtype.is_float() {
            return Err(MoraError::Runtime(format!(
                "Tensor.layer_norm requires a floating-point dtype, got {}",
                self.dtype
            )));
        }
        let last_dim = *self.shape.last().unwrap_or(&1);
        let mut flat = self.to_flat_vec();
        for row in flat.chunks_mut(last_dim) {
            let mean = row.iter().sum::<f64>() / (last_dim as f64);
            let var = row
                .iter()
                .map(|v| {
                    let d = *v - mean;
                    d * d
                })
                .sum::<f64>()
                / (last_dim as f64);
            let inv_std = 1.0 / (var + eps).sqrt();
            for v in row.iter_mut() {
                *v = quantize_for_dtype((*v - mean) * inv_std, self.dtype);
            }
        }
        Self::from_raw(
            flat,
            self.shape.clone(),
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Index along axis 0 (`t[idx]`): supports negative indices and optional autograd tracking.
    pub fn index_axis0_tracked(
        &self,
        raw_idx: isize,
        autograd: Option<&mut AutogradContext>,
    ) -> MoraResult<Value> {
        if self.shape.is_empty() {
            return Err(MoraError::Runtime(
                "cannot index a 0-dimensional scalar tensor".to_string(),
            ));
        }
        let dim0 = self.shape[0] as isize;
        let idx = if raw_idx < 0 { raw_idx + dim0 } else { raw_idx };
        if idx < 0 || idx >= dim0 {
            return Err(MoraError::Runtime(format!(
                "tensor index {} out of bounds for axis 0 with size {}",
                raw_idx, dim0
            )));
        }
        let idx_u = idx as usize;
        let stride0 = self.strides[0];
        let base = (self.offset as isize) + (idx_u as isize) * stride0;
        let tracked_src = autograd
            .as_ref()
            .and_then(|ag| ag.active_node_index(self.grad_node));

        if self.shape.len() == 1 {
            if let (Some(ag), Some(src_node)) = (autograd, tracked_src) {
                let val = self.storage.data.read().get_f64(base as usize);
                let node = ag.record(
                    TapeOp::Gather {
                        a: src_node,
                        src_numel: self.numel(),
                        src_flat_indices: vec![idx_u],
                    },
                    Vec::new(),
                    self.dtype,
                    self.device.clone(),
                );
                let out =
                    Tensor::from_raw(vec![val], Vec::new(), self.dtype, self.device.clone(), true)?;
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            let guard = self.storage.data.read();
            Ok(guard.get_value(base as usize))
        } else {
            let new_shape = self.shape[1..].to_vec();
            let grad_node = if let (Some(ag), Some(src_node)) = (autograd, tracked_src) {
                let row_size: usize = new_shape.iter().copied().product::<usize>().max(1);
                let start_flat = idx_u * row_size;
                let indices: Vec<usize> = (start_flat..start_flat + row_size).collect();
                ag.record(
                    TapeOp::Gather {
                        a: src_node,
                        src_numel: self.numel(),
                        src_flat_indices: indices,
                    },
                    new_shape.clone(),
                    self.dtype,
                    self.device.clone(),
                )
            } else {
                None
            };
            Ok(Value::Tensor(Arc::new(Self {
                storage: Arc::clone(&self.storage),
                shape: new_shape,
                strides: self.strides[1..].to_vec(),
                offset: base as usize,
                dtype: self.dtype,
                device: self.device.clone(),
                requires_grad: grad_node.is_some() || self.requires_grad,
                is_view: true,
                grad_node,
            })))
        }
    }

    /// Stage 3C: Zero-copy multi-dimensional strided slicing `t[0..4, :, -1]`, `t[1:5:2, ..., None]`.
    pub fn slice_nd(
        &self,
        axes: &[RuntimeSliceAxis],
        autograd: Option<&mut AutogradContext>,
    ) -> MoraResult<Value> {
        // Count non-NewAxis / non-Ellipsis consumed input axes and validate at most one Ellipsis.
        let mut ellipsis_pos: Option<usize> = None;
        let mut consumed_axes = 0usize;
        for (i, ax) in axes.iter().enumerate() {
            match ax {
                RuntimeSliceAxis::Ellipsis => {
                    if ellipsis_pos.is_some() {
                        return Err(MoraError::Runtime(
                            "Tensor slice cannot contain more than one ellipsis (`...`)"
                                .to_string(),
                        ));
                    }
                    ellipsis_pos = Some(i);
                }
                RuntimeSliceAxis::NewAxis => {}
                RuntimeSliceAxis::Index(_) | RuntimeSliceAxis::Range { .. } => {
                    consumed_axes += 1;
                }
            }
        }
        let rank = self.shape.len();
        if consumed_axes > rank {
            return Err(MoraError::Runtime(format!(
                "Tensor slice has {} axis specifiers for a {}D tensor (shape {:?})",
                consumed_axes, rank, self.shape
            )));
        }

        // Expand Ellipsis and trailing implicit full slices.
        let mut expanded: Vec<RuntimeSliceAxis> = Vec::with_capacity(axes.len() + rank);
        let fill_count = rank - consumed_axes;
        for (i, &ax) in axes.iter().enumerate() {
            if Some(i) == ellipsis_pos {
                for _ in 0..fill_count {
                    expanded.push(RuntimeSliceAxis::Range {
                        start: None,
                        stop: None,
                        step: None,
                    });
                }
            } else {
                expanded.push(ax);
            }
        }
        if ellipsis_pos.is_none() {
            for _ in 0..fill_count {
                expanded.push(RuntimeSliceAxis::Range {
                    start: None,
                    stop: None,
                    step: None,
                });
            }
        }

        // Track per-input-axis coordinate mapping for exact autograd Gather indices.
        enum AxisMap {
            Fixed(usize),
            Linear {
                out_axis: usize,
                start: isize,
                step: isize,
            },
        }
        let mut axis_maps: Vec<AxisMap> = Vec::with_capacity(rank);
        let mut new_offset = self.offset as isize;
        let mut new_shape: Vec<usize> = Vec::new();
        let mut new_strides: Vec<isize> = Vec::new();
        let mut in_axis = 0usize;

        for ax in expanded {
            match ax {
                RuntimeSliceAxis::NewAxis => {
                    new_shape.push(1);
                    new_strides.push(0);
                }
                RuntimeSliceAxis::Index(raw_i) => {
                    let dim = self.shape[in_axis] as isize;
                    let idx = if raw_i < 0 { raw_i + dim } else { raw_i };
                    if idx < 0 || idx >= dim {
                        return Err(MoraError::Runtime(format!(
                            "Tensor slice index {} out of bounds for axis {} with size {}",
                            raw_i, in_axis, dim
                        )));
                    }
                    new_offset += idx * self.strides[in_axis];
                    axis_maps.push(AxisMap::Fixed(idx as usize));
                    in_axis += 1;
                }
                RuntimeSliceAxis::Range { start, stop, step } => {
                    let dim = self.shape[in_axis] as isize;
                    let step = step.unwrap_or(1);
                    if step == 0 {
                        return Err(MoraError::Runtime(
                            "Tensor slice step cannot be zero".to_string(),
                        ));
                    }
                    let (s, len) = if step > 0 {
                        let mut s = start.unwrap_or(0);
                        if s < 0 {
                            s += dim;
                        }
                        s = s.clamp(0, dim);
                        let mut e = stop.unwrap_or(dim);
                        if e < 0 {
                            e += dim;
                        }
                        e = e.clamp(0, dim);
                        let len = if e > s {
                            ((e - s - 1) / step + 1) as usize
                        } else {
                            0
                        };
                        (s, len)
                    } else {
                        let mut s = start.unwrap_or(dim - 1);
                        if s < 0 {
                            s += dim;
                        }
                        s = s.clamp(-1, dim - 1);
                        let e = match stop {
                            Some(mut v) => {
                                if v < 0 {
                                    v += dim;
                                }
                                v.clamp(-1, dim - 1)
                            }
                            None => -1,
                        };
                        let len = if s > e {
                            ((s - e - 1) / (-step) + 1) as usize
                        } else {
                            0
                        };
                        (s, len)
                    };
                    if len == 0 {
                        return Err(MoraError::Runtime(format!(
                            "Tensor slice produces empty dimension (size 0) on axis {}",
                            in_axis
                        )));
                    }
                    let out_axis = new_shape.len();
                    new_offset += s * self.strides[in_axis];
                    new_shape.push(len);
                    new_strides.push(self.strides[in_axis] * step);
                    axis_maps.push(AxisMap::Linear {
                        out_axis,
                        start: s,
                        step,
                    });
                    in_axis += 1;
                }
                RuntimeSliceAxis::Ellipsis => unreachable!("ellipsis already expanded"),
            }
        }

        let tracked_src = autograd
            .as_ref()
            .and_then(|ag| ag.active_node_index(self.grad_node));

        let build_gather_indices = || -> Vec<usize> {
            let out_numel: usize = new_shape.iter().copied().product::<usize>().max(1);
            let orig_c_strides = contiguous_strides(&self.shape);
            let mut out_coords = vec![0usize; new_shape.len()];
            let mut indices = Vec::with_capacity(out_numel);
            for mut lin in 0..out_numel {
                for a in (0..new_shape.len()).rev() {
                    out_coords[a] = lin % new_shape[a];
                    lin /= new_shape[a];
                }
                let mut src_flat = 0usize;
                for (in_a, am) in axis_maps.iter().enumerate() {
                    let src_c = match *am {
                        AxisMap::Fixed(c) => c,
                        AxisMap::Linear {
                            out_axis,
                            start,
                            step,
                        } => (start + (out_coords[out_axis] as isize) * step) as usize,
                    };
                    src_flat += src_c * (orig_c_strides[in_a] as usize);
                }
                indices.push(src_flat);
            }
            indices
        };

        if new_shape.is_empty() {
            if let (Some(ag), Some(src_node)) = (autograd, tracked_src) {
                let indices = build_gather_indices();
                let val = self.storage.data.read().get_f64(new_offset as usize);
                let node = ag.record(
                    TapeOp::Gather {
                        a: src_node,
                        src_numel: self.numel(),
                        src_flat_indices: indices,
                    },
                    Vec::new(),
                    self.dtype,
                    self.device.clone(),
                );
                let out =
                    Tensor::from_raw(vec![val], Vec::new(), self.dtype, self.device.clone(), true)?;
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            let guard = self.storage.data.read();
            return Ok(guard.get_value(new_offset as usize));
        }

        let grad_node = if let (Some(ag), Some(src_node)) = (autograd, tracked_src) {
            let indices = build_gather_indices();
            ag.record(
                TapeOp::Gather {
                    a: src_node,
                    src_numel: self.numel(),
                    src_flat_indices: indices,
                },
                new_shape.clone(),
                self.dtype,
                self.device.clone(),
            )
        } else {
            None
        };

        Ok(Value::Tensor(Arc::new(Self {
            storage: Arc::clone(&self.storage),
            shape: new_shape,
            strides: new_strides,
            offset: new_offset as usize,
            dtype: self.dtype,
            device: self.device.clone(),
            requires_grad: grad_node.is_some() || self.requires_grad,
            is_view: true,
            grad_node,
        })))
    }

    /// Index along axis 0 (`t[idx]`): returns an exact scalar `Value` for 1D tensors or a
    /// zero-copy strided `(rank - 1)`-dimensional view `Value::Tensor` for ND (`rank >= 2`).
    pub fn index_axis0(&self, idx: usize) -> MoraResult<Value> {
        self.index_axis0_tracked(idx as isize, None)
    }

    /// In-place elementwise tensor addition `self += other`.
    pub fn add_tensor_inplace(&self, other: &Self) -> MoraResult<()> {
        self.ensure_inplace_safe("add_")?;
        if self.device != other.device {
            return Err(MoraError::Runtime(format!(
                "Device mismatch in add_: cannot combine '{}' and '{}'",
                self.device, other.device
            )));
        }
        if self.dtype != other.dtype {
            return Err(MoraError::Runtime(format!(
                "DType mismatch in add_: cannot combine Tensor<{}> and Tensor<{}>",
                self.dtype, other.dtype
            )));
        }
        if self.shape != other.shape {
            return Err(MoraError::Runtime(format!(
                "Shape mismatch in add_: expected {:?}, got {:?}",
                self.shape, other.shape
            )));
        }
        let rhs_flat = other.to_flat_vec();
        let numel = self.numel();
        let mut guard = self.storage.data.write();
        for (i, &rhs_v) in rhs_flat.iter().enumerate().take(numel) {
            let cur = guard.get_f64(self.offset + i);
            guard.set_f64(self.offset + i, cur + rhs_v);
        }
        Ok(())
    }

    /// In-place elementwise tensor multiplication `self *= other`.
    pub fn mul_tensor_inplace(&self, other: &Self) -> MoraResult<()> {
        self.ensure_inplace_safe("mul_")?;
        if self.device != other.device {
            return Err(MoraError::Runtime(format!(
                "Device mismatch in mul_: cannot combine '{}' and '{}'",
                self.device, other.device
            )));
        }
        if self.dtype != other.dtype {
            return Err(MoraError::Runtime(format!(
                "DType mismatch in mul_: cannot combine Tensor<{}> and Tensor<{}>",
                self.dtype, other.dtype
            )));
        }
        if self.shape != other.shape {
            return Err(MoraError::Runtime(format!(
                "Shape mismatch in mul_: expected {:?}, got {:?}",
                self.shape, other.shape
            )));
        }
        let rhs_flat = other.to_flat_vec();
        let numel = self.numel();
        let mut guard = self.storage.data.write();
        for (i, &rhs_v) in rhs_flat.iter().enumerate().take(numel) {
            let cur = guard.get_f64(self.offset + i);
            guard.set_f64(self.offset + i, cur * rhs_v);
        }
        Ok(())
    }
}

fn parse_shape_arg(val: &Value, fn_name: &str) -> MoraResult<Vec<usize>> {
    let Value::List(items) = val else {
        return Err(MoraError::Runtime(format!(
            "{fn_name}: expected a shape list [d1, d2, ...], got {}",
            crate::flow::type_name(val)
        )));
    };
    let mut dims = Vec::with_capacity(items.len());
    for item in items.iter() {
        let d = match item {
            Value::Int(n) if *n >= 0 => *n as usize,
            Value::Float(f) if f.is_finite() && *f >= 0.0 => *f as usize,
            Value::Scalar(s) if s.as_f64().is_finite() && s.as_f64() >= 0.0 => s.as_f64() as usize,
            other => {
                return Err(MoraError::Runtime(format!(
                    "{fn_name}: shape dimensions must be non-negative integers, got {other}"
                )));
            }
        };
        dims.push(d);
    }
    Ok(dims)
}

fn parse_optional_dtype(val: Option<&Value>, fn_name: &str) -> MoraResult<Option<DType>> {
    match val {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::String(s)) => DType::from_name(s)
            .ok_or_else(|| {
                MoraError::Runtime(format!(
                    "{fn_name}: unknown dtype '{s}' (expected f64, f32, f16, bf16, fp8, fp8_e4m3, fp8_e5m2, i64, i32, i16, i8, i4, u64, u32, u16, u8, bool)"
                ))
            })
            .map(Some),
        Some(other) => Err(MoraError::Runtime(format!(
            "{fn_name}: dtype must be a string, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn parse_optional_device(val: Option<&Value>, fn_name: &str) -> MoraResult<Device> {
    match val {
        None | Some(Value::Nil) => Ok(Device::Cpu),
        Some(Value::String(s)) => Device::parse(s).map_err(MoraError::Runtime),
        Some(other) => Err(MoraError::Runtime(format!(
            "{fn_name}: device must be a string ('cpu', 'cuda:0', 'metal:0'), got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn parse_optional_bool(val: Option<&Value>, fn_name: &str) -> MoraResult<bool> {
    match val {
        None | Some(Value::Nil) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(MoraError::Runtime(format!(
            "{fn_name}: requires_grad must be a bool, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn parse_num_arg(val: &Value, fn_name: &str) -> MoraResult<f64> {
    match val {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Scalar(s) => Ok(s.as_f64()),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(MoraError::Runtime(format!(
            "{fn_name}: expected a number, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

/// Reorder positional + keyword arguments (`dtype`, `device`, `requires_grad`, etc.) onto
/// a canonical positional parameter table for tensor constructors.
fn reorder_tensor_kwargs(
    fn_name: &str,
    param_names: &[&str],
    args: &[Value],
    named: &[(String, Value)],
) -> MoraResult<Vec<Value>> {
    if named.is_empty() {
        return Ok(args.to_vec());
    }
    if args.len() > param_names.len() {
        return Err(MoraError::Runtime(format!(
            "{fn_name}: too many positional arguments ({}, max {})",
            args.len(),
            param_names.len()
        )));
    }
    let mut slots: Vec<Option<Value>> = vec![None; param_names.len()];
    for (i, v) in args.iter().enumerate() {
        slots[i] = Some(v.clone());
    }
    for (k, v) in named {
        let Some(pos) = param_names.iter().position(|&p| p == k.as_str()) else {
            return Err(MoraError::Runtime(format!(
                "{fn_name}: unknown keyword argument '{k}'"
            )));
        };
        if slots[pos].is_some() {
            return Err(MoraError::Runtime(format!(
                "{fn_name}: multiple values for argument '{k}'"
            )));
        }
        slots[pos] = Some(v.clone());
    }
    while matches!(slots.last(), Some(None)) {
        slots.pop();
    }
    Ok(slots.into_iter().map(|o| o.unwrap_or(Value::Nil)).collect())
}

/// Dispatch `tensor(data, dtype=..., device=..., requires_grad=...)` with optional keyword args.
pub fn call_builtin_tensor_named(args: &[Value], named: &[(String, Value)]) -> MoraResult<Value> {
    let reordered = reorder_tensor_kwargs(
        "tensor",
        &["data", "dtype", "device", "requires_grad"],
        args,
        named,
    )?;
    call_builtin_tensor(&reordered)
}

/// Dispatch `Tensor::<assoc>(args..., kw=val...)` with optional keyword args.
pub fn call_tensor_associated_named(
    assoc: &str,
    args: &[Value],
    named: &[(String, Value)],
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    if named.is_empty() {
        return call_tensor_associated(assoc, args, core);
    }
    let params: &[&str] = match assoc {
        "zeros" | "ones" | "randn" | "rand" => &["shape", "dtype", "device", "requires_grad"],
        "full" => &["shape", "value", "dtype", "device", "requires_grad"],
        "eye" => &["n", "m", "dtype", "device", "requires_grad"],
        "arange" => &["start", "end", "step", "dtype", "device", "requires_grad"],
        "from_list" => &["data", "dtype", "device", "requires_grad"],
        _ => {
            return Err(MoraError::Runtime(format!(
                "Tensor has no associated function 'Tensor::{assoc}'"
            )));
        }
    };
    let reordered = reorder_tensor_kwargs(&format!("Tensor::{assoc}"), params, args, named)?;
    match assoc {
        "eye" => {
            let n_val = reordered
                .first()
                .filter(|v| !matches!(v, Value::Nil))
                .ok_or_else(|| {
                    MoraError::Runtime(
                        "Tensor::eye(n, m?, dtype?, device?, requires_grad?) requires 'n'"
                            .to_string(),
                    )
                })?;
            let n = parse_num_arg(n_val, "Tensor::eye")? as usize;
            let m = match reordered.get(1) {
                None | Some(Value::Nil) => n,
                Some(v) => parse_num_arg(v, "Tensor::eye")? as usize,
            };
            let dtype =
                parse_optional_dtype(reordered.get(2), "Tensor::eye")?.unwrap_or(DType::F64);
            let device = parse_optional_device(reordered.get(3), "Tensor::eye")?;
            let requires_grad = parse_optional_bool(reordered.get(4), "Tensor::eye")?;
            Ok(Value::Tensor(Tensor::eye(
                n,
                m,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "arange" => {
            let s_val = reordered.first().filter(|v| !matches!(v, Value::Nil));
            let e_val = reordered.get(1).filter(|v| !matches!(v, Value::Nil));
            let step_val = reordered.get(2).filter(|v| !matches!(v, Value::Nil));
            let (start, end, all_ints) = match (s_val, e_val) {
                (Some(s), Some(e)) => (
                    parse_num_arg(s, "Tensor::arange")?,
                    parse_num_arg(e, "Tensor::arange")?,
                    matches!(s, Value::Int(_)) && matches!(e, Value::Int(_)),
                ),
                (Some(e), None) => (
                    0.0,
                    parse_num_arg(e, "Tensor::arange")?,
                    matches!(e, Value::Int(_)),
                ),
                (None, Some(e)) => (
                    0.0,
                    parse_num_arg(e, "Tensor::arange")?,
                    matches!(e, Value::Int(_)),
                ),
                (None, None) => {
                    return Err(MoraError::Runtime(
                        "Tensor::arange(start, end?, step?, dtype?, device?, requires_grad?) requires at least 'start' or 'end'"
                            .to_string(),
                    ));
                }
            };
            let (step, step_is_int) = match step_val {
                Some(v) => (
                    parse_num_arg(v, "Tensor::arange")?,
                    matches!(v, Value::Int(_)),
                ),
                None => (1.0, true),
            };
            let default_dt = if all_ints && step_is_int {
                DType::I64
            } else {
                DType::F64
            };
            let dtype =
                parse_optional_dtype(reordered.get(3), "Tensor::arange")?.unwrap_or(default_dt);
            let device = parse_optional_device(reordered.get(4), "Tensor::arange")?;
            let requires_grad = parse_optional_bool(reordered.get(5), "Tensor::arange")?;
            Ok(Value::Tensor(Tensor::arange(
                start,
                end,
                step,
                dtype,
                device,
                requires_grad,
            )?))
        }
        _ => call_tensor_associated(assoc, &reordered, core),
    }
}

/// Dispatch `tensor(data, dtype?, device?, requires_grad?)` free-function constructor.
pub fn call_builtin_tensor(args: &[Value]) -> MoraResult<Value> {
    let data = args.first().ok_or_else(|| {
        MoraError::Runtime(
            "tensor(data, dtype?, device?, requires_grad?) requires at least 1 argument"
                .to_string(),
        )
    })?;
    let dtype = parse_optional_dtype(args.get(1), "tensor")?;
    let device = match args.get(2) {
        None | Some(Value::Nil) => None,
        Some(_) => Some(parse_optional_device(args.get(2), "tensor")?),
    };
    let requires_grad = parse_optional_bool(args.get(3), "tensor")?;
    let t = Tensor::from_value(data, dtype, device, requires_grad)?;
    Ok(Value::Tensor(t))
}

/// Dispatch `Tensor::<assoc>(args...)` associated constructor calls.
pub fn call_tensor_associated(
    assoc: &str,
    args: &[Value],
    core: &mut CoreRuntime,
) -> MoraResult<Value> {
    match assoc {
        "zeros" => {
            let shape_val = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor::zeros(shape, dtype?, device?, requires_grad?) requires a shape list"
                        .to_string(),
                )
            })?;
            let shape = parse_shape_arg(shape_val, "Tensor::zeros")?;
            let dtype = parse_optional_dtype(args.get(1), "Tensor::zeros")?.unwrap_or(DType::F64);
            let device = parse_optional_device(args.get(2), "Tensor::zeros")?;
            let requires_grad = parse_optional_bool(args.get(3), "Tensor::zeros")?;
            Ok(Value::Tensor(Tensor::zeros(
                shape,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "ones" => {
            let shape_val = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor::ones(shape, dtype?, device?, requires_grad?) requires a shape list"
                        .to_string(),
                )
            })?;
            let shape = parse_shape_arg(shape_val, "Tensor::ones")?;
            let dtype = parse_optional_dtype(args.get(1), "Tensor::ones")?.unwrap_or(DType::F64);
            let device = parse_optional_device(args.get(2), "Tensor::ones")?;
            let requires_grad = parse_optional_bool(args.get(3), "Tensor::ones")?;
            Ok(Value::Tensor(Tensor::ones(
                shape,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "randn" => {
            let shape_val = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor::randn(shape, dtype?, device?, requires_grad?) requires a shape list"
                        .to_string(),
                )
            })?;
            let shape = parse_shape_arg(shape_val, "Tensor::randn")?;
            let dtype = parse_optional_dtype(args.get(1), "Tensor::randn")?.unwrap_or(DType::F64);
            let device = parse_optional_device(args.get(2), "Tensor::randn")?;
            let requires_grad = parse_optional_bool(args.get(3), "Tensor::randn")?;
            Ok(Value::Tensor(Tensor::randn(
                shape,
                dtype,
                device,
                requires_grad,
                core,
            )?))
        }
        "rand" => {
            let shape_val = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor::rand(shape, dtype?, device?, requires_grad?) requires a shape list"
                        .to_string(),
                )
            })?;
            let shape = parse_shape_arg(shape_val, "Tensor::rand")?;
            let dtype = parse_optional_dtype(args.get(1), "Tensor::rand")?.unwrap_or(DType::F64);
            let device = parse_optional_device(args.get(2), "Tensor::rand")?;
            let requires_grad = parse_optional_bool(args.get(3), "Tensor::rand")?;
            Ok(Value::Tensor(Tensor::rand(
                shape,
                dtype,
                device,
                requires_grad,
                core,
            )?))
        }
        "full" => {
            if args.len() < 2 {
                return Err(MoraError::Runtime(
                    "Tensor::full(shape, value, dtype?, device?, requires_grad?) requires at least 2 arguments".to_string(),
                ));
            }
            let shape = parse_shape_arg(&args[0], "Tensor::full")?;
            let fill = parse_num_arg(&args[1], "Tensor::full")?;
            let default_dt = match &args[1] {
                Value::Int(_) => DType::I64,
                Value::Scalar(s) => s.dtype(),
                Value::Bool(_) => DType::Bool,
                _ => DType::F64,
            };
            let dtype = parse_optional_dtype(args.get(2), "Tensor::full")?.unwrap_or(default_dt);
            let device = parse_optional_device(args.get(3), "Tensor::full")?;
            let requires_grad = parse_optional_bool(args.get(4), "Tensor::full")?;
            Ok(Value::Tensor(Tensor::full(
                shape,
                fill,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "eye" => {
            let n_val = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor::eye(n, m?, dtype?, device?, requires_grad?) requires at least 1 argument"
                        .to_string(),
                )
            })?;
            let n = parse_num_arg(n_val, "Tensor::eye")? as usize;
            let (m, opt_start) = match args.get(1) {
                Some(Value::Int(m)) if *m >= 0 => (*m as usize, 2),
                Some(Value::Float(m)) if m.is_finite() && *m >= 0.0 => (*m as usize, 2),
                Some(Value::Scalar(s)) if s.as_f64().is_finite() && s.as_f64() >= 0.0 => {
                    (s.as_f64() as usize, 2)
                }
                _ => (n, 1),
            };
            let dtype =
                parse_optional_dtype(args.get(opt_start), "Tensor::eye")?.unwrap_or(DType::F64);
            let device = parse_optional_device(args.get(opt_start + 1), "Tensor::eye")?;
            let requires_grad = parse_optional_bool(args.get(opt_start + 2), "Tensor::eye")?;
            Ok(Value::Tensor(Tensor::eye(
                n,
                m,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "arange" => {
            if args.is_empty() {
                return Err(MoraError::Runtime(
                    "Tensor::arange(start, end?, step?, dtype?, device?, requires_grad?) requires at least 1 argument".to_string(),
                ));
            }
            let mut num_args = Vec::new();
            let mut idx = 0;
            while idx < args.len()
                && matches!(
                    args[idx],
                    Value::Int(_) | Value::Float(_) | Value::Scalar(_)
                )
                && num_args.len() < 3
            {
                num_args.push(parse_num_arg(&args[idx], "Tensor::arange")?);
                idx += 1;
            }
            let (start, end, step) = match num_args.as_slice() {
                [end] => (0.0, *end, 1.0),
                [start, end] => (*start, *end, 1.0),
                [start, end, step] => (*start, *end, *step),
                _ => {
                    return Err(MoraError::Runtime(
                        "Tensor::arange expects numeric start/end arguments".to_string(),
                    ));
                }
            };
            let all_ints = args[..idx].iter().all(|v| matches!(v, Value::Int(_)));
            let default_dt = if all_ints { DType::I64 } else { DType::F64 };
            let dtype =
                parse_optional_dtype(args.get(idx), "Tensor::arange")?.unwrap_or(default_dt);
            let device = parse_optional_device(args.get(idx + 1), "Tensor::arange")?;
            let requires_grad = parse_optional_bool(args.get(idx + 2), "Tensor::arange")?;
            Ok(Value::Tensor(Tensor::arange(
                start,
                end,
                step,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "from_list" => call_builtin_tensor(args),
        _ => Err(MoraError::Runtime(format!(
            "Tensor has no associated function 'Tensor::{assoc}'"
        ))),
    }
}

/// Dispatch `t.<method>(args...)` instance method calls on [`Value::Tensor`].
pub fn call_tensor_method(t: &Arc<Tensor>, method: &str, args: &[Value]) -> MoraResult<Value> {
    call_tensor_method_tracked(t, method, args, None)
}

/// Dispatch `t.<method>(args...)` on [`Value::Tensor`] with optional reverse-mode autograd tracking.
pub fn call_tensor_method_tracked(
    t: &Arc<Tensor>,
    method: &str,
    args: &[Value],
    mut autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    let tracked_src = autograd
        .as_ref()
        .and_then(|ag| ag.active_node_index(t.grad_node));
    match method {
        "shape" => Ok(Value::List(
            t.shape
                .iter()
                .map(|&d| Value::Int(d as i64))
                .collect::<Vec<_>>()
                .into(),
        )),
        "ndim" | "rank" => Ok(Value::Int(t.rank() as i64)),
        "numel" => Ok(Value::Int(t.numel() as i64)),
        "nbytes" | "byte_len" => Ok(Value::Int(t.nbytes() as i64)),
        "len" => Ok(Value::Int(t.shape.first().copied().unwrap_or(0) as i64)),
        "dtype" => Ok(Value::String(t.dtype.short_name().to_string())),
        "device" => Ok(Value::String(t.device.to_string())),
        "is_contiguous" => Ok(Value::Bool(t.is_contiguous())),
        "contiguous" => {
            let out = t.contiguous()?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Identity { a: src },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "requires_grad" => Ok(Value::Bool(t.requires_grad)),
        "with_grad" => {
            let enabled = match args.first() {
                None => true,
                Some(Value::Bool(b)) => *b,
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "Tensor.with_grad(enabled?) expects bool, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            Ok(Value::Tensor(t.with_grad(enabled)))
        }
        "reshape" => {
            if args.is_empty() {
                return Err(MoraError::Runtime(
                    "Tensor.reshape(shape) requires a target shape list or dimension arguments"
                        .to_string(),
                ));
            }
            let new_shape = if let Value::List(_) = &args[0] {
                parse_shape_arg(&args[0], "Tensor.reshape")?
            } else {
                let mut dims = Vec::with_capacity(args.len());
                for a in args {
                    let n = parse_num_arg(a, "Tensor.reshape")?;
                    if n < 0.0 {
                        return Err(MoraError::Runtime(
                            "Tensor.reshape: dimensions must be non-negative".to_string(),
                        ));
                    }
                    dims.push(n as usize);
                }
                dims
            };
            let out = t.reshape(new_shape)?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Reshape {
                        a: src,
                        orig_shape: t.shape.clone(),
                    },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "transpose" => {
            let d0 = match args.first() {
                None | Some(Value::Nil) => None,
                Some(v) => Some(parse_num_arg(v, "Tensor.transpose")? as usize),
            };
            let d1 = match args.get(1) {
                None | Some(Value::Nil) => None,
                Some(v) => Some(parse_num_arg(v, "Tensor.transpose")? as usize),
            };
            let rank = t.shape.len();
            let eff_d0 = d0.unwrap_or_else(|| rank.saturating_sub(2));
            let eff_d1 = d1.unwrap_or_else(|| rank.saturating_sub(1));
            let out = t.transpose(d0, d1)?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Transpose {
                        a: src,
                        dim0: eff_d0,
                        dim1: eff_d1,
                        orig_shape: t.shape.clone(),
                    },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "to_dtype" => {
            let Some(Value::String(s)) = args.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.to_dtype(dtype) requires a dtype string argument".to_string(),
                ));
            };
            let dt = DType::from_name(s).ok_or_else(|| {
                MoraError::Runtime(format!("Tensor.to_dtype: unknown dtype '{s}'"))
            })?;
            let out = t.to_dtype(dt)?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Identity { a: src },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "to_device" => {
            let Some(Value::String(s)) = args.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.to_device(device) requires a device string argument".to_string(),
                ));
            };
            let dev = Device::parse(s).map_err(MoraError::Runtime)?;
            let out = t.to_device(dev)?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Identity { a: src },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "to" => {
            let Some(Value::String(first)) = args.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.to(dtype_or_device, second?) requires a string argument".to_string(),
                ));
            };
            let mut cur = Arc::clone(t);
            if let Some(dt) = DType::from_name(first) {
                cur = cur.to_dtype(dt)?;
            } else {
                let dev = Device::parse(first).map_err(MoraError::Runtime)?;
                cur = cur.to_device(dev)?;
            }
            if let Some(Value::String(second)) = args.get(1) {
                if let Some(dt) = DType::from_name(second) {
                    cur = cur.to_dtype(dt)?;
                } else {
                    let dev = Device::parse(second).map_err(MoraError::Runtime)?;
                    cur = cur.to_device(dev)?;
                }
            }
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Identity { a: src },
                    cur.shape.clone(),
                    cur.dtype,
                    cur.device.clone(),
                );
                return Ok(Value::Tensor(cur.with_grad_node(node)));
            }
            Ok(Value::Tensor(cur))
        }
        "fill_" => {
            let v = args.first().ok_or_else(|| {
                MoraError::Runtime("Tensor.fill_(value) requires 1 scalar argument".to_string())
            })?;
            t.fill_inplace(v)?;
            Ok(Value::Tensor(Arc::clone(t)))
        }
        "add_" => {
            let v = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor.add_(rhs) requires 1 scalar or tensor argument".to_string(),
                )
            })?;
            match v {
                Value::Tensor(other) => t.add_tensor_inplace(other)?,
                _ => {
                    t.add_inplace(v)?;
                }
            }
            Ok(Value::Tensor(Arc::clone(t)))
        }
        "mul_" => {
            let v = args.first().ok_or_else(|| {
                MoraError::Runtime(
                    "Tensor.mul_(rhs) requires 1 scalar or tensor argument".to_string(),
                )
            })?;
            match v {
                Value::Tensor(other) => t.mul_tensor_inplace(other)?,
                _ => {
                    t.mul_inplace(v)?;
                }
            }
            Ok(Value::Tensor(Arc::clone(t)))
        }
        "sum" => {
            let flat = t.to_flat_vec();
            let s: f64 = flat.iter().sum();
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Sum {
                        a: src,
                        a_shape: t.shape.clone(),
                    },
                    Vec::new(),
                    t.dtype,
                    t.device.clone(),
                );
                let out = Tensor::from_raw(vec![s], Vec::new(), t.dtype, t.device.clone(), true)?;
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(scalar_value_for_dtype(s, t.dtype))
        }
        "mean" => {
            let flat = t.to_flat_vec();
            if flat.is_empty() {
                return Err(MoraError::Runtime(
                    "Tensor.mean() cannot be called on an empty tensor".to_string(),
                ));
            }
            let m = flat.iter().sum::<f64>() / (flat.len() as f64);
            let dt = if t.dtype.is_float() {
                t.dtype
            } else {
                DType::F64
            };
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let node = ag.record(
                    TapeOp::Mean {
                        a: src,
                        a_shape: t.shape.clone(),
                    },
                    Vec::new(),
                    dt,
                    t.device.clone(),
                );
                let out = Tensor::from_raw(vec![m], Vec::new(), dt, t.device.clone(), true)?;
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(scalar_value_for_dtype(m, dt))
        }
        "max" => {
            let flat = t.to_flat_vec();
            let Some(&first) = flat.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.max() cannot be called on an empty tensor".to_string(),
                ));
            };
            let m = flat.iter().copied().fold(first, f64::max);
            Ok(scalar_value_for_dtype(m, t.dtype))
        }
        "min" => {
            let flat = t.to_flat_vec();
            let Some(&first) = flat.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.min() cannot be called on an empty tensor".to_string(),
                ));
            };
            let m = flat.iter().copied().fold(first, f64::min);
            Ok(scalar_value_for_dtype(m, t.dtype))
        }
        "item" => {
            if t.numel() != 1 {
                return Err(MoraError::Runtime(format!(
                    "Tensor.item() requires a 1-element tensor, got {} elements (shape {:?})",
                    t.numel(),
                    t.shape
                )));
            }
            Ok(t.get_linear_value(0))
        }
        "abs" | "sqrt" | "exp" | "ln" | "relu" | "gelu" | "silu" | "tanh" | "sigmoid" => {
            eval_tensor_math_unary_tracked(t, method, autograd)
        }
        "softmax" => {
            let out = t.softmax()?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let last_dim = t.shape.last().copied().unwrap_or(1);
                let node = ag.record(
                    TapeOp::Softmax {
                        a: src,
                        out_vals: out.to_flat_vec(),
                        last_dim,
                    },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "rms_norm" => {
            let eps = match args.first() {
                None | Some(Value::Nil) => 1e-5,
                Some(v) => parse_num_arg(v, "Tensor.rms_norm")?,
            };
            let out = t.rms_norm(eps)?;
            if let (Some(ag), Some(src)) = (autograd.as_deref_mut(), tracked_src) {
                let last_dim = t.shape.last().copied().unwrap_or(1);
                let node = ag.record(
                    TapeOp::RmsNorm {
                        a: src,
                        a_vals: t.to_flat_vec(),
                        last_dim,
                        eps,
                    },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "layer_norm" => {
            let eps = match args.first() {
                None | Some(Value::Nil) => 1e-5,
                Some(v) => parse_num_arg(v, "Tensor.layer_norm")?,
            };
            let out = t.layer_norm(eps)?;
            if let (Some(ag), Some(src)) = (autograd, tracked_src) {
                let last_dim = t.shape.last().copied().unwrap_or(1);
                let node = ag.record(
                    TapeOp::LayerNorm {
                        a: src,
                        a_vals: t.to_flat_vec(),
                        last_dim,
                        eps,
                    },
                    out.shape.clone(),
                    out.dtype,
                    out.device.clone(),
                );
                return Ok(Value::Tensor(out.with_grad_node(node)));
            }
            Ok(Value::Tensor(out))
        }
        "to_list" => Ok(t.to_nested_list()),
        _ => Err(MoraError::Runtime(format!(
            "Tensor has no method: {method}"
        ))),
    }
}

/// Evaluate unary mathematical / activation functions on a [`Tensor`] with optional autograd tracking.
pub fn eval_tensor_math_unary_tracked(
    t: &Arc<Tensor>,
    op: &str,
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    let tracked_src = autograd
        .as_ref()
        .and_then(|ag| ag.active_node_index(t.grad_node));
    let out = match op {
        "abs" => t.map_unary(|x| x.abs())?,
        "sqrt" => t.map_unary(|x| x.sqrt())?,
        "exp" => t.map_unary(|x| x.exp())?,
        "ln" | "log" => t.map_unary(|x| x.ln())?,
        "sin" => t.map_unary(|x| x.sin())?,
        "cos" => t.map_unary(|x| x.cos())?,
        "relu" => t.map_unary(|x| x.max(0.0))?,
        "gelu" => t.map_unary(|x| {
            let c = (2.0 / std::f64::consts::PI).sqrt();
            0.5 * x * (1.0 + (c * (x + 0.044715 * x.powi(3))).tanh())
        })?,
        "silu" => t.map_unary(|x| x / (1.0 + (-x).exp()))?,
        "tanh" => t.map_unary(|x| x.tanh())?,
        "sigmoid" => t.map_unary(|x| 1.0 / (1.0 + (-x).exp()))?,
        other => {
            return Err(MoraError::Runtime(format!(
                "Tensor has no unary math operation '{other}'"
            )));
        }
    };
    if let (Some(ag), Some(src)) = (autograd, tracked_src) {
        let tape_op = match op {
            "abs" => TapeOp::Abs {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "sqrt" => TapeOp::Sqrt {
                a: src,
                out_vals: out.to_flat_vec(),
            },
            "exp" => TapeOp::Exp {
                a: src,
                out_vals: out.to_flat_vec(),
            },
            "ln" | "log" => TapeOp::Ln {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "sin" => TapeOp::Sin {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "cos" => TapeOp::Cos {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "relu" => TapeOp::Relu {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "gelu" => TapeOp::Gelu {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "silu" => TapeOp::Silu {
                a: src,
                a_vals: t.to_flat_vec(),
            },
            "tanh" => TapeOp::Tanh {
                a: src,
                out_vals: out.to_flat_vec(),
            },
            "sigmoid" => TapeOp::Sigmoid {
                a: src,
                out_vals: out.to_flat_vec(),
            },
            _ => unreachable!(),
        };
        let node = ag.record(tape_op, out.shape.clone(), out.dtype, out.device.clone());
        return Ok(Value::Tensor(out.with_grad_node(node)));
    }
    Ok(Value::Tensor(out))
}

/// Evaluate unary `-tensor` with optional autograd tracking.
pub fn eval_tensor_neg_tracked(
    t: &Arc<Tensor>,
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    if t.dtype.is_unsigned_int() || t.dtype.is_bool() {
        return Err(MoraError::Runtime(format!(
            "cannot negate Tensor<{}> (unsigned or bool dtype)",
            t.dtype
        )));
    }
    let out = t.map_unary(|x| -x)?;
    let tracked_src = autograd
        .as_ref()
        .and_then(|ag| ag.active_node_index(t.grad_node));
    if let (Some(ag), Some(src)) = (autograd, tracked_src) {
        let node = ag.record(
            TapeOp::Neg { a: src },
            out.shape.clone(),
            out.dtype,
            out.device.clone(),
        );
        return Ok(Value::Tensor(out.with_grad_node(node)));
    }
    Ok(Value::Tensor(out))
}

impl PartialEq for Tensor {
    fn eq(&self, other: &Self) -> bool {
        self.dtype == other.dtype
            && self.device == other.device
            && self.shape == other.shape
            && self.to_flat_values() == other.to_flat_values()
    }
}

impl std::fmt::Display for Tensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list_val = self.to_nested_list();
        write!(
            f,
            "Tensor({list_val}, dtype={}, shape={:?}, device={})",
            self.dtype, self.shape, self.device
        )
    }
}

/// Compute NumPy/PyTorch broadcast shape for two runtime shapes `a` and `b`.
pub fn broadcast_runtime_shapes(a: &[usize], b: &[usize]) -> MoraResult<Vec<usize>> {
    let max_rank = a.len().max(b.len());
    let mut out = vec![1usize; max_rank];
    for i in 0..max_rank {
        let da = if i < max_rank - a.len() {
            1
        } else {
            a[i - (max_rank - a.len())]
        };
        let db = if i < max_rank - b.len() {
            1
        } else {
            b[i - (max_rank - b.len())]
        };
        if da == db {
            out[i] = da;
        } else if da == 1 {
            out[i] = db;
        } else if db == 1 {
            out[i] = da;
        } else {
            return Err(MoraError::Runtime(format!(
                "Tensor shape mismatch: cannot broadcast shapes {:?} and {:?}",
                a, b
            )));
        }
    }
    Ok(out)
}

/// Map a linear index in `broadcast_shape` to a physical element of `t`.
fn read_broadcast_elem(t: &Tensor, broadcast_shape: &[usize], mut linear_idx: usize) -> f64 {
    let rank_diff = broadcast_shape.len() - t.shape.len();
    let mut phys = t.offset as isize;
    for i in (0..broadcast_shape.len()).rev() {
        let b_dim = broadcast_shape[i];
        let coord = linear_idx % b_dim;
        linear_idx /= b_dim;
        if i >= rank_diff {
            let t_axis = i - rank_diff;
            let t_coord = if t.shape[t_axis] == 1 { 0 } else { coord };
            phys += (t_coord as isize) * t.strides[t_axis];
        }
    }
    let guard = t.storage.data.read();
    guard.get_f64(phys as usize)
}

fn apply_scalar_op(op: &BinaryOp, a: f64, b: f64, dtype: DType) -> MoraResult<f64> {
    if dtype.is_bool() {
        return Err(MoraError::Runtime(format!(
            "Arithmetic operator {:?} is not supported on Tensor<bool>",
            op
        )));
    }
    let raw = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => {
            if dtype.is_int() && b == 0.0 {
                return Err(MoraError::Runtime(
                    "Tensor integer division by zero".to_string(),
                ));
            }
            if dtype.is_int() {
                ((a as i64) / (b as i64)) as f64
            } else {
                a / b
            }
        }
        BinaryOp::Mod => {
            if b == 0.0 {
                return Err(MoraError::Runtime("Tensor modulo by zero".to_string()));
            }
            if dtype.is_int() {
                ((a as i64) % (b as i64)) as f64
            } else {
                a % b
            }
        }
        BinaryOp::Pow => a.powf(b),
        _ => {
            return Err(MoraError::Runtime(format!(
                "Unsupported elementwise tensor binary operator: {op:?}"
            )));
        }
    };
    Ok(quantize_for_dtype(raw, dtype))
}

/// Evaluate binary operator `op` when at least one operand is [`Value::Tensor`].
pub fn eval_tensor_binary(op: &BinaryOp, left: &Value, right: &Value) -> MoraResult<Value> {
    eval_tensor_binary_tracked(op, left, right, None)
}

/// Evaluate binary operator `op` when at least one operand is [`Value::Tensor`],
/// recording reverse-mode autograd nodes when an active tape is recording.
pub fn eval_tensor_binary_tracked(
    op: &BinaryOp,
    left: &Value,
    right: &Value,
    mut autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    if matches!(op, BinaryOp::MatMul) {
        return eval_tensor_matmul_tracked(left, right, autograd);
    }

    match (left, right) {
        (Value::Tensor(a), Value::Tensor(b)) => {
            if a.device != b.device {
                return Err(MoraError::Runtime(format!(
                    "Device mismatch: cannot combine tensors on '{}' and '{}' without explicit .to(device)",
                    a.device, b.device
                )));
            }
            if a.dtype != b.dtype {
                return Err(MoraError::Runtime(format!(
                    "DType mismatch: cannot combine Tensor<{}> and Tensor<{}> without explicit .to(dtype)",
                    a.dtype, b.dtype
                )));
            }
            let out_shape = broadcast_runtime_shapes(&a.shape, &b.shape)?;
            let numel = checked_numel(&out_shape)?;
            let mut out = Vec::with_capacity(numel);
            for idx in 0..numel {
                let va = read_broadcast_elem(a, &out_shape, idx);
                let vb = read_broadcast_elem(b, &out_shape, idx);
                out.push(apply_scalar_op(op, va, vb, a.dtype)?);
            }
            let na = autograd
                .as_ref()
                .and_then(|ag| ag.active_node_index(a.grad_node));
            let nb = autograd
                .as_ref()
                .and_then(|ag| ag.active_node_index(b.grad_node));
            let grad_node = if (na.is_some() || nb.is_some())
                && let Some(ag) = autograd.as_deref_mut()
            {
                let tape_op = match op {
                    BinaryOp::Add => Some(TapeOp::Add {
                        a: na,
                        b: nb,
                        a_const: if na.is_none() {
                            Some(a.to_flat_vec())
                        } else {
                            None
                        },
                        b_const: if nb.is_none() {
                            Some(b.to_flat_vec())
                        } else {
                            None
                        },
                        a_shape: a.shape.clone(),
                        b_shape: b.shape.clone(),
                    }),
                    BinaryOp::Sub => Some(TapeOp::Sub {
                        a: na,
                        b: nb,
                        a_const: if na.is_none() {
                            Some(a.to_flat_vec())
                        } else {
                            None
                        },
                        b_const: if nb.is_none() {
                            Some(b.to_flat_vec())
                        } else {
                            None
                        },
                        a_shape: a.shape.clone(),
                        b_shape: b.shape.clone(),
                    }),
                    BinaryOp::Mul => Some(TapeOp::Mul {
                        a: na,
                        b: nb,
                        a_vals: a.to_flat_vec(),
                        b_vals: b.to_flat_vec(),
                        a_shape: a.shape.clone(),
                        b_shape: b.shape.clone(),
                    }),
                    BinaryOp::Div => Some(TapeOp::Div {
                        a: na,
                        b: nb,
                        a_vals: a.to_flat_vec(),
                        b_vals: b.to_flat_vec(),
                        a_shape: a.shape.clone(),
                        b_shape: b.shape.clone(),
                    }),
                    BinaryOp::Pow => Some(TapeOp::Pow {
                        a: na,
                        b: nb,
                        a_vals: a.to_flat_vec(),
                        b_vals: b.to_flat_vec(),
                        out_vals: out.clone(),
                        a_shape: a.shape.clone(),
                        b_shape: b.shape.clone(),
                    }),
                    _ => None,
                };
                tape_op.and_then(|op| ag.record(op, out_shape.clone(), a.dtype, a.device.clone()))
            } else {
                None
            };
            let t = Tensor::from_raw(
                out,
                out_shape,
                a.dtype,
                a.device.clone(),
                grad_node.is_some() || a.requires_grad || b.requires_grad,
            )?;
            Ok(Value::Tensor(if grad_node.is_some() {
                t.with_grad_node(grad_node)
            } else {
                t
            }))
        }
        (Value::Tensor(t), scalar) => {
            let Some((s_dt, s_val)) = scalar_val_dtype_and_f64(scalar) else {
                return Err(MoraError::Runtime(format!(
                    "Unsupported tensor binary operands: {} {:?} {}",
                    crate::flow::type_name(left),
                    op,
                    crate::flow::type_name(right)
                )));
            };
            if t.dtype != s_dt {
                return Err(MoraError::Runtime(format!(
                    "DType mismatch: cannot combine Tensor<{}> with scalar '{}'",
                    t.dtype, s_dt
                )));
            }
            let flat = t.to_flat_vec();
            let mut out = Vec::with_capacity(flat.len());
            for &v in &flat {
                out.push(apply_scalar_op(op, v, s_val, t.dtype)?);
            }
            let na = autograd
                .as_ref()
                .and_then(|ag| ag.active_node_index(t.grad_node));
            let grad_node = if let (Some(ag), Some(ia)) = (autograd.as_deref_mut(), na) {
                let tape_op = match op {
                    BinaryOp::Add => Some(TapeOp::AddScalar {
                        a: ia,
                        scalar: s_val,
                    }),
                    BinaryOp::Sub => Some(TapeOp::SubScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: false,
                    }),
                    BinaryOp::Mul => Some(TapeOp::MulScalar {
                        a: ia,
                        scalar: s_val,
                    }),
                    BinaryOp::Div => Some(TapeOp::DivScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: false,
                        a_vals: flat,
                    }),
                    BinaryOp::Pow => Some(TapeOp::PowScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: false,
                        a_vals: flat,
                        out_vals: out.clone(),
                    }),
                    _ => None,
                };
                tape_op.and_then(|op| ag.record(op, t.shape.clone(), t.dtype, t.device.clone()))
            } else {
                None
            };
            let res = Tensor::from_raw(
                out,
                t.shape.clone(),
                t.dtype,
                t.device.clone(),
                grad_node.is_some() || t.requires_grad,
            )?;
            Ok(Value::Tensor(if grad_node.is_some() {
                res.with_grad_node(grad_node)
            } else {
                res
            }))
        }
        (scalar, Value::Tensor(t)) => {
            let Some((s_dt, s_val)) = scalar_val_dtype_and_f64(scalar) else {
                return Err(MoraError::Runtime(format!(
                    "Unsupported tensor binary operands: {} {:?} {}",
                    crate::flow::type_name(left),
                    op,
                    crate::flow::type_name(right)
                )));
            };
            if t.dtype != s_dt {
                return Err(MoraError::Runtime(format!(
                    "DType mismatch: cannot combine scalar '{}' with Tensor<{}>",
                    s_dt, t.dtype
                )));
            }
            let flat = t.to_flat_vec();
            let mut out = Vec::with_capacity(flat.len());
            for &v in &flat {
                out.push(apply_scalar_op(op, s_val, v, t.dtype)?);
            }
            let na = autograd
                .as_ref()
                .and_then(|ag| ag.active_node_index(t.grad_node));
            let grad_node = if let (Some(ag), Some(ia)) = (autograd, na) {
                let tape_op = match op {
                    BinaryOp::Add => Some(TapeOp::AddScalar {
                        a: ia,
                        scalar: s_val,
                    }),
                    BinaryOp::Sub => Some(TapeOp::SubScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: true,
                    }),
                    BinaryOp::Mul => Some(TapeOp::MulScalar {
                        a: ia,
                        scalar: s_val,
                    }),
                    BinaryOp::Div => Some(TapeOp::DivScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: true,
                        a_vals: flat,
                    }),
                    BinaryOp::Pow => Some(TapeOp::PowScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: true,
                        a_vals: flat,
                        out_vals: out.clone(),
                    }),
                    _ => None,
                };
                tape_op.and_then(|op| ag.record(op, t.shape.clone(), t.dtype, t.device.clone()))
            } else {
                None
            };
            let res = Tensor::from_raw(
                out,
                t.shape.clone(),
                t.dtype,
                t.device.clone(),
                grad_node.is_some() || t.requires_grad,
            )?;
            Ok(Value::Tensor(if grad_node.is_some() {
                res.with_grad_node(grad_node)
            } else {
                res
            }))
        }
        _ => Err(MoraError::Runtime(format!(
            "Unsupported tensor binary operands: {} {:?} {}",
            crate::flow::type_name(left),
            op,
            crate::flow::type_name(right)
        ))),
    }
}

/// Evaluate matrix/tensor contraction (`@`) for 1D, 2D, and batched ND [`Tensor`]s.
pub fn eval_tensor_matmul(left: &Value, right: &Value) -> MoraResult<Value> {
    eval_tensor_matmul_tracked(left, right, None)
}

/// Evaluate matrix/tensor contraction (`@`) with optional reverse-mode autograd tape recording.
pub fn eval_tensor_matmul_tracked(
    left: &Value,
    right: &Value,
    mut autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    let (Value::Tensor(a), Value::Tensor(b)) = (left, right) else {
        return Err(MoraError::Runtime(format!(
            "Tensor matmul (@) requires two tensors, got {} @ {}",
            crate::flow::type_name(left),
            crate::flow::type_name(right)
        )));
    };

    if a.device != b.device {
        return Err(MoraError::Runtime(format!(
            "Device mismatch in '@': left tensor is on '{}' but right tensor is on '{}'",
            a.device, b.device
        )));
    }
    if a.dtype != b.dtype {
        return Err(MoraError::Runtime(format!(
            "DType mismatch in '@': cannot multiply Tensor<{}> and Tensor<{}>",
            a.dtype, b.dtype
        )));
    }
    if a.dtype.is_bool() {
        return Err(MoraError::Runtime(
            "Tensor matmul (@) is not defined for Tensor<bool>".to_string(),
        ));
    }

    let dt = a.dtype;
    let dev = a.device.clone();
    let na = autograd
        .as_ref()
        .and_then(|ag| ag.active_node_index(a.grad_node));
    let nb = autograd
        .as_ref()
        .and_then(|ag| ag.active_node_index(b.grad_node));
    let tracked = na.is_some() || nb.is_some();
    let req_grad = tracked || a.requires_grad || b.requires_grad;

    let attach = |out_data: Vec<f64>,
                  out_shape: Vec<usize>,
                  fa: Vec<f64>,
                  fb: Vec<f64>,
                  ag_opt: Option<&mut AutogradContext>|
     -> MoraResult<Value> {
        let grad_node = if tracked && let Some(ag) = ag_opt {
            ag.record(
                TapeOp::MatMul {
                    a: na,
                    b: nb,
                    a_vals: fa,
                    b_vals: fb,
                    a_shape: a.shape.clone(),
                    b_shape: b.shape.clone(),
                },
                out_shape.clone(),
                dt,
                dev.clone(),
            )
        } else {
            None
        };
        let t = Tensor::from_raw(out_data, out_shape, dt, dev.clone(), req_grad)?;
        Ok(Value::Tensor(if grad_node.is_some() {
            t.with_grad_node(grad_node)
        } else {
            t
        }))
    };

    match (a.shape.as_slice(), b.shape.as_slice()) {
        // 1D @ 1D -> scalar dot product (0-D tracked Tensor when recording autograd)
        ([k1], [k2]) => {
            if k1 != k2 {
                return Err(MoraError::Runtime(format!(
                    "Tensor matmul (@) shape mismatch: [{k1}] @ [{k2}]"
                )));
            }
            let fa = a.to_flat_vec();
            let fb = b.to_flat_vec();
            let sum: f64 = fa.iter().zip(fb.iter()).map(|(x, y)| x * y).sum();
            if tracked && autograd.is_some() {
                return attach(vec![sum], Vec::new(), fa, fb, autograd.as_deref_mut());
            }
            Ok(scalar_value_for_dtype(sum, dt))
        }
        // 2D @ 1D -> [M, K] @ [K] -> [M]
        ([m, k1], [k2]) => {
            if k1 != k2 {
                return Err(MoraError::Runtime(format!(
                    "Tensor matmul (@) shape mismatch: [{m}, {k1}] @ [{k2}]"
                )));
            }
            let fa = a.to_flat_vec();
            let fb = b.to_flat_vec();
            let mut out = vec![0.0; *m];
            for i in 0..*m {
                let mut acc = 0.0;
                for p in 0..*k1 {
                    acc += fa[i * *k1 + p] * fb[p];
                }
                out[i] = quantize_for_dtype(acc, dt);
            }
            attach(out, vec![*m], fa, fb, autograd.as_deref_mut())
        }
        // 1D @ 2D -> [K] @ [K, N] -> [N]
        ([k1], [k2, n]) => {
            if k1 != k2 {
                return Err(MoraError::Runtime(format!(
                    "Tensor matmul (@) shape mismatch: [{k1}] @ [{k2}, {n}]"
                )));
            }
            let fa = a.to_flat_vec();
            let fb = b.to_flat_vec();
            let mut out = vec![0.0; *n];
            for j in 0..*n {
                let mut acc = 0.0;
                for p in 0..*k1 {
                    acc += fa[p] * fb[p * *n + j];
                }
                out[j] = quantize_for_dtype(acc, dt);
            }
            attach(out, vec![*n], fa, fb, autograd.as_deref_mut())
        }
        // 2D @ 2D -> [M, K] @ [K, N] -> [M, N]
        ([m, k1], [k2, n]) => {
            if k1 != k2 {
                return Err(MoraError::Runtime(format!(
                    "Tensor matmul (@) shape mismatch: [{m}, {k1}] @ [{k2}, {n}]"
                )));
            }
            let fa = a.to_flat_vec();
            let fb = b.to_flat_vec();
            let mut out = vec![0.0; *m * *n];
            for i in 0..*m {
                for j in 0..*n {
                    let mut acc = 0.0;
                    for p in 0..*k1 {
                        acc += fa[i * *k1 + p] * fb[p * *n + j];
                    }
                    out[i * *n + j] = quantize_for_dtype(acc, dt);
                }
            }
            attach(out, vec![*m, *n], fa, fb, autograd.as_deref_mut())
        }
        // Batched ND @ ND (rank >= 2 on both sides, at least one >= 3)
        (sa, sb) if sa.len() >= 2 && sb.len() >= 2 => {
            let m = sa[sa.len() - 2];
            let k1 = sa[sa.len() - 1];
            let k2 = sb[sb.len() - 2];
            let n = sb[sb.len() - 1];
            if k1 != k2 {
                return Err(MoraError::Runtime(format!(
                    "Tensor batched matmul (@) contraction dimension mismatch: {:?} @ {:?}",
                    sa, sb
                )));
            }
            let batch_a = &sa[..sa.len() - 2];
            let batch_b = &sb[..sb.len() - 2];
            let batch_shape = broadcast_runtime_shapes(batch_a, batch_b)?;
            let batch_count: usize = batch_shape.iter().copied().product::<usize>().max(1);

            let fa = a.to_flat_vec();
            let fb = b.to_flat_vec();
            let mat_a_stride = m * k1;
            let mat_b_stride = k2 * n;
            let mat_out_stride = m * n;

            let map_batch = |mut b_lin: usize, target_batch: &[usize]| -> usize {
                if target_batch.is_empty() {
                    return 0;
                }
                let rdiff = batch_shape.len() - target_batch.len();
                let mut idx = 0usize;
                let mut stride = 1usize;
                for i in (0..batch_shape.len()).rev() {
                    let dim = batch_shape[i];
                    let coord = b_lin % dim;
                    b_lin /= dim;
                    if i >= rdiff {
                        let ta = i - rdiff;
                        let td = target_batch[ta];
                        let tc = if td == 1 { 0 } else { coord };
                        idx += tc * stride;
                        stride *= td;
                    }
                }
                idx
            };

            let mut out = vec![0.0; batch_count * mat_out_stride];
            for b_idx in 0..batch_count {
                let a_base = map_batch(b_idx, batch_a) * mat_a_stride;
                let b_base = map_batch(b_idx, batch_b) * mat_b_stride;
                let out_base = b_idx * mat_out_stride;
                for i in 0..m {
                    for j in 0..n {
                        let mut acc = 0.0;
                        for p in 0..k1 {
                            acc += fa[a_base + i * k1 + p] * fb[b_base + p * n + j];
                        }
                        out[out_base + i * n + j] = quantize_for_dtype(acc, dt);
                    }
                }
            }
            let mut full_shape = batch_shape;
            full_shape.push(m);
            full_shape.push(n);
            attach(out, full_shape, fa, fb, autograd)
        }
        (sa, sb) => Err(MoraError::Runtime(format!(
            "Tensor matmul (@) unsupported shapes: {:?} @ {:?}",
            sa, sb
        ))),
    }
}

fn flatten_nested_value(
    val: &Value,
    depth: usize,
    shape: &mut Vec<usize>,
    flat: &mut Vec<Value>,
    inferred_dtype: &mut Option<DType>,
) -> MoraResult<()> {
    match val {
        Value::Int(_) | Value::Float(_) | Value::Scalar(_) | Value::Bool(_) => {
            let elem_dt = match val {
                Value::Int(_) => DType::I64,
                Value::Float(_) => DType::F64,
                Value::Scalar(s) => s.dtype(),
                Value::Bool(_) => DType::Bool,
                _ => unreachable!(),
            };
            match inferred_dtype {
                None => *inferred_dtype = Some(elem_dt),
                Some(prev) if *prev == elem_dt => {}
                Some(prev) => {
                    if matches!(
                        (*prev, elem_dt),
                        (DType::I64, DType::F64) | (DType::F64, DType::I64)
                    ) {
                        *inferred_dtype = Some(DType::F64);
                    } else {
                        return Err(MoraError::Runtime(format!(
                            "Tensor literal elements have conflicting DTypes '{}' and '{}'",
                            prev, elem_dt
                        )));
                    }
                }
            }
            flat.push(val.clone());
            Ok(())
        }
        Value::List(items) => {
            if items.is_empty() {
                return Err(MoraError::Runtime(
                    "Tensor cannot be constructed from an empty list".to_string(),
                ));
            }
            if shape.len() <= depth {
                shape.push(items.len());
            } else if shape[depth] != items.len() {
                return Err(MoraError::Runtime(format!(
                    "Tensor requires a rectangular nested list (expected dimension {} at depth {}, got {})",
                    shape[depth],
                    depth,
                    items.len()
                )));
            }
            let first_is_list = matches!(items.first(), Some(Value::List(_)));
            for item in items.iter() {
                if matches!(item, Value::List(_)) != first_is_list {
                    return Err(MoraError::Runtime(
                        "Tensor requires uniform nesting depth across all elements".to_string(),
                    ));
                }
                flatten_nested_value(item, depth + 1, shape, flat, inferred_dtype)?;
            }
            Ok(())
        }
        other => Err(MoraError::Runtime(format!(
            "Tensor elements must be numeric or bool, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn build_nested_list(flat: &[Value], shape: &[usize]) -> Value {
    if shape.is_empty() {
        return flat.first().cloned().unwrap_or(Value::Nil);
    }
    if shape.len() == 1 {
        return Value::List(List::from_vec(flat.to_vec()));
    }
    let chunk_size: usize = shape[1..].iter().copied().product();
    let sub_lists: Vec<Value> = flat
        .chunks(chunk_size)
        .map(|chunk| build_nested_list(chunk, &shape[1..]))
        .collect();
    Value::List(List::from_vec(sub_lists))
}
