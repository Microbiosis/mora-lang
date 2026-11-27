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

mod normalization;
pub(crate) use normalization::RowNorm;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::common::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::common::{
    BinaryOp, DType, Device, ScalarValue, bf16_bits_to_f64, f16_bits_to_f64, f64_to_bf16_bits,
    f64_to_f16_bits, f64_to_fp8_e4m3_bits, f64_to_fp8_e5m2_bits, fp8_e4m3_bits_to_f64,
    fp8_e5m2_bits_to_f64, pack_i4_nibble, unpack_i4_nibble,
};
use crate::error::{MoraError, MoraResult};
use crate::runtime::CoreRuntime;
use crate::runtime::autograd::{AutogradContext, TapeNodeId, TapeOp};
use crate::runtime::compute_dag::DeviceVramBuffer;
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

    /// Allocate a zero-initialized 64-byte aligned buffer backed by at least `cap_bytes` bytes (for slab pooling).
    pub(crate) fn with_capacity_bytes(cap_bytes: usize, len: usize, dtype: DType) -> Self {
        let byte_len = dtype.storage_bytes(len);
        let num_blocks = cap_bytes.max(byte_len).div_ceil(64).max(1);
        Self {
            blocks: vec![AlignedCacheLine([0u8; 64]); num_blocks],
            byte_len,
            dtype,
            len,
        }
    }

    /// Lightweight zero-allocation sentinel left inside a [`TensorStorage`] after `MemoryPool.release`.
    pub(crate) fn empty_detached(dtype: DType) -> Self {
        Self {
            blocks: Vec::new(),
            byte_len: 0,
            dtype,
            len: 0,
        }
    }

    /// Repurpose this physical 64-byte aligned buffer in place for `len` zeroed elements of `dtype`,
    /// avoiding OS heap reallocation whenever existing block capacity suffices.
    pub(crate) fn repurpose_zeroed(&mut self, len: usize, dtype: DType) {
        let byte_len = dtype.storage_bytes(len);
        let needed_blocks = byte_len.div_ceil(64).max(1);
        if self.blocks.len() < needed_blocks {
            self.blocks
                .resize(needed_blocks, AlignedCacheLine([0u8; 64]));
        }
        for blk in self.blocks.iter_mut().take(needed_blocks) {
            blk.0.fill(0);
        }
        self.byte_len = byte_len;
        self.dtype = dtype;
        self.len = len;
    }

    /// Repurpose this physical 64-byte aligned buffer in place filled with `val`.
    pub(crate) fn repurpose_with_value(&mut self, len: usize, val: f64, dtype: DType) {
        self.repurpose_zeroed(len, dtype);
        if val != 0.0 {
            for i in 0..len {
                self.set_f64(i, val);
            }
        }
    }

    #[inline]
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.blocks.len() * 64
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

    /// Construct a compact 64-byte aligned buffer directly from raw little-endian hardware bytes.
    pub(crate) fn from_raw_bytes(raw: &[u8], len: usize, dtype: DType) -> MoraResult<Self> {
        let expected = dtype.storage_bytes(len);
        if raw.len() != expected {
            return Err(MoraError::Runtime(format!(
                "Tensor::from_bytes: byte length mismatch for dtype '{}' with {} elements: expected {} bytes, got {}",
                dtype.short_name(),
                len,
                expected,
                raw.len()
            )));
        }
        let mut buf = Self::zeroed(len, dtype);
        let mut offset = 0usize;
        for blk in &mut buf.blocks {
            if offset >= raw.len() {
                break;
            }
            let take = (raw.len() - offset).min(64);
            blk.0[..take].copy_from_slice(&raw[offset..offset + take]);
            offset += take;
        }
        Ok(buf)
    }

    /// Copy the exact `byte_len` little-endian hardware-packed bytes out of this buffer.
    pub(crate) fn copy_bytes_out(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.byte_len);
        let mut remaining = self.byte_len;
        for blk in &self.blocks {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(64);
            out.extend_from_slice(&blk.0[..take]);
            remaining -= take;
        }
        out
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
        if idx >= self.len {
            return 0.0;
        }
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
        if idx >= self.len {
            return scalar_value_for_dtype(0.0, self.dtype);
        }
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
///
/// On hardware GPU devices (`CUDA` / `Vulkan` / `Metal`), holds a real resident
/// [`DeviceVramBuffer`] with lazy D2H synchronization (`host_stale = true` for GPU kernel
/// outputs until host memory is actually read) so chained GPU DAGs run with zero intermediate
/// H2D/D2H round-trips.
#[derive(Debug)]
pub struct TensorStorage {
    data: RwLock<AlignedByteBuffer>,
    vram: RwLock<Option<Arc<dyn DeviceVramBuffer>>>,
    host_stale: AtomicBool,
    numel_for_sync: usize,
    dtype_for_sync: DType,
}

impl TensorStorage {
    pub fn new(buf: AlignedByteBuffer) -> Arc<Self> {
        let numel_for_sync = buf.len();
        let dtype_for_sync = buf.dtype();
        Arc::new(Self {
            data: RwLock::new(buf),
            vram: RwLock::new(None),
            host_stale: AtomicBool::new(false),
            numel_for_sync,
            dtype_for_sync,
        })
    }

    /// Construct storage for `device`, immediately allocating & uploading a physical GPU
    /// [`DeviceVramBuffer`] when `device` resolves to a live hardware GPU backend.
    pub(crate) fn for_device(buf: AlignedByteBuffer, device: &Device) -> MoraResult<Arc<Self>> {
        let numel_for_sync = buf.len();
        let dtype_for_sync = buf.dtype();
        let mut vram_opt: Option<Arc<dyn DeviceVramBuffer>> = None;
        if !matches!(device, Device::Cpu) && buf.byte_len() > 0 {
            let backend = crate::runtime::compute_dag::backend_for_device(device);
            if backend.supports_vram_residency()
                && let Some(vbuf) = backend.alloc_vram_buffer(buf.byte_len())?
            {
                let raw = buf.copy_bytes_out();
                vbuf.upload_bytes(&raw)?;
                vram_opt = Some(vbuf);
            }
        }
        Ok(Arc::new(Self {
            data: RwLock::new(buf),
            vram: RwLock::new(vram_opt),
            host_stale: AtomicBool::new(false),
            numel_for_sync,
            dtype_for_sync,
        }))
    }

    /// Construct VRAM-resident storage directly from a GPU kernel output buffer with lazy D2H sync.
    pub(crate) fn from_vram(
        vram_buf: Arc<dyn DeviceVramBuffer>,
        numel: usize,
        dtype: DType,
    ) -> Arc<Self> {
        Arc::new(Self {
            data: RwLock::new(AlignedByteBuffer::empty_detached(dtype)),
            vram: RwLock::new(Some(vram_buf)),
            host_stale: AtomicBool::new(true),
            numel_for_sync: numel,
            dtype_for_sync: dtype,
        })
    }

    /// Ensure host `AlignedByteBuffer` holds up-to-date bytes (performing at most one lazy D2H
    /// copy if this storage was produced directly in GPU VRAM).
    #[inline]
    pub(crate) fn ensure_host_synced(&self) {
        if !self.host_stale.load(Ordering::Acquire) {
            return;
        }
        let mut data_guard = self.data.write();
        if !self.host_stale.load(Ordering::Acquire) {
            return;
        }
        let vram_arc = self.vram.read().clone();
        if let Some(vbuf) = vram_arc {
            let mut raw = vec![0u8; vbuf.byte_len()];
            if vbuf.download_bytes(&mut raw).is_ok()
                && let Ok(aligned) = AlignedByteBuffer::from_raw_bytes(
                    &raw,
                    self.numel_for_sync,
                    self.dtype_for_sync,
                )
            {
                *data_guard = aligned;
            }
        }
        self.host_stale.store(false, Ordering::Release);
    }

    #[inline]
    pub(crate) fn read_host(&self) -> RwLockReadGuard<'_, AlignedByteBuffer> {
        self.ensure_host_synced();
        self.data.read()
    }

    #[inline]
    pub(crate) fn write_host(&self) -> RwLockWriteGuard<'_, AlignedByteBuffer> {
        self.ensure_host_synced();
        self.data.write()
    }

    /// Push host mutations (`fill_`, `add_`, `mul_`) back into resident VRAM when present.
    pub(crate) fn sync_host_to_vram(&self) {
        let vram_arc = self.vram.read().clone();
        if let Some(vbuf) = vram_arc {
            let raw = self.data.read().copy_bytes_out();
            if raw.len() == vbuf.byte_len() {
                let _ = vbuf.upload_bytes(&raw);
            }
        }
    }

    /// Mark host shadow buffer stale after an in-place GPU kernel wrote directly to VRAM.
    #[inline]
    pub(crate) fn mark_host_stale(&self) {
        if self.vram.read().is_some() {
            self.host_stale.store(true, Ordering::Release);
        }
    }

    /// Return `true` when this storage currently holds an active physical GPU [`DeviceVramBuffer`].
    #[inline]
    pub(crate) fn is_vram_resident(&self) -> bool {
        self.vram.read().is_some()
    }

    /// Clone the underlying resident [`DeviceVramBuffer`] if present.
    #[inline]
    pub(crate) fn vram_buffer(&self) -> Option<Arc<dyn DeviceVramBuffer>> {
        self.vram.read().clone()
    }

    /// Atomically detach and return the underlying 64-byte aligned buffer, leaving an empty
    /// detached placeholder so any stale handle reads cleanly return `0.0` without UB.
    pub(crate) fn detach_buffer(&self, dtype: DType) -> AlignedByteBuffer {
        self.ensure_host_synced();
        let mut guard = self.data.write();
        *self.vram.write() = None;
        self.host_stale.store(false, Ordering::Release);
        std::mem::replace(&mut *guard, AlignedByteBuffer::empty_detached(dtype))
    }

    /// Physical packed storage byte length.
    pub fn byte_len(&self) -> usize {
        if self.host_stale.load(Ordering::Acquire)
            && let Some(vbuf) = self.vram.read().as_ref()
        {
            return vbuf.byte_len();
        }
        self.data.read().byte_len()
    }

    /// 64-byte aligned base byte pointer for SIMD / GPU DMA kernels.
    pub fn aligned_byte_ptr(&self) -> *const u8 {
        self.ensure_host_synced();
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
    /// Stage 3C / Wave 4 P1: Active autograd tape nodes `(tape_id, node_idx)` tracking this tensor
    /// across arbitrary nested differentiation levels (`grad(grad(f))`, `jvp(grad(f))`, `trace_dag(grad(f))`).
    pub grad_nodes: Vec<TapeNodeId>,
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
        let storage =
            TensorStorage::for_device(AlignedByteBuffer::from_f64_slice(&data, dtype), &device)?;
        Ok(Arc::new(Self {
            storage,
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_nodes: Vec::new(),
        }))
    }

    /// Construct a contiguous compact tensor directly from a pooled 64-byte aligned buffer.
    pub(crate) fn from_aligned_buffer(
        buf: AlignedByteBuffer,
        shape: Vec<usize>,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        let expected = checked_numel(&shape)?;
        if buf.len() != expected {
            return Err(MoraError::Runtime(format!(
                "Tensor buffer length {} does not match shape {:?} ({} elements)",
                buf.len(),
                shape,
                expected
            )));
        }
        let dtype = buf.dtype();
        let strides = contiguous_strides(&shape);
        let storage = TensorStorage::for_device(buf, &device)?;
        Ok(Arc::new(Self {
            storage,
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_nodes: Vec::new(),
        }))
    }

    /// Construct a contiguous VRAM-resident tensor directly from a GPU kernel output buffer
    /// (zero immediate D2H copy; host memory is synced lazily on first host read).
    pub(crate) fn from_vram_output(
        vram_buf: Arc<dyn DeviceVramBuffer>,
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        let numel = checked_numel(&shape)?;
        let expected_bytes = dtype.storage_bytes(numel);
        if vram_buf.byte_len() != expected_bytes {
            return Err(MoraError::Runtime(format!(
                "Tensor::from_vram_output: VRAM buffer size {} does not match shape {:?} dtype {} ({expected_bytes} bytes)",
                vram_buf.byte_len(),
                shape,
                dtype.short_name()
            )));
        }
        let strides = contiguous_strides(&shape);
        let storage = TensorStorage::from_vram(vram_buf, numel, dtype);
        Ok(Arc::new(Self {
            storage,
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_nodes: Vec::new(),
        }))
    }

    /// Return `true` when this tensor's storage is physically resident in GPU VRAM.
    #[inline]
    pub(crate) fn is_vram_resident(&self) -> bool {
        self.storage.is_vram_resident()
    }

    /// Return the underlying physical GPU [`DeviceVramBuffer`] when this tensor is a full,
    /// contiguous, zero-offset view of that buffer (safe for direct GPU kernel dispatch).
    #[inline]
    pub(crate) fn vram_buffer_if_contiguous(&self) -> Option<Arc<dyn DeviceVramBuffer>> {
        if !self.is_contiguous() || self.offset != 0 {
            return None;
        }
        let buf = self.storage.vram_buffer()?;
        if buf.device() == &self.device && buf.byte_len() == self.nbytes() {
            Some(buf)
        } else {
            None
        }
    }

    /// Mark this tensor's host copy stale after an in-place GPU kernel mutated its VRAM buffer.
    #[inline]
    pub(crate) fn mark_host_stale(&self) {
        self.storage.mark_host_stale();
    }

    /// Overwrite a contiguous zero-offset `f32` tensor's storage from CPU kernel execution output.
    pub(crate) fn overwrite_contiguous_f32(&self, vals: &[f32]) -> MoraResult<()> {
        if self.dtype != DType::F32 || !self.is_contiguous() || self.offset != 0 {
            return Err(MoraError::Runtime(
                "overwrite_contiguous_f32 requires a contiguous zero-offset f32 Tensor".to_string(),
            ));
        }
        let mut raw = Vec::with_capacity(vals.len() * 4);
        for &v in vals {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        let next = AlignedByteBuffer::from_raw_bytes(&raw, vals.len(), DType::F32)?;
        {
            let mut guard = self.storage.write_host();
            *guard = next;
        }
        self.storage.sync_host_to_vram();
        Ok(())
    }

    /// Overwrite a contiguous zero-offset `f64` tensor's storage from CPU kernel execution output.
    pub(crate) fn overwrite_contiguous_f64(&self, vals: &[f64]) -> MoraResult<()> {
        if self.dtype != DType::F64 || !self.is_contiguous() || self.offset != 0 {
            return Err(MoraError::Runtime(
                "overwrite_contiguous_f64 requires a contiguous zero-offset f64 Tensor".to_string(),
            ));
        }
        let mut raw = Vec::with_capacity(vals.len() * 8);
        for &v in vals {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        let next = AlignedByteBuffer::from_raw_bytes(&raw, vals.len(), DType::F64)?;
        {
            let mut guard = self.storage.write_host();
            *guard = next;
        }
        self.storage.sync_host_to_vram();
        Ok(())
    }

    /// P5-1B: Copy the exact hardware-packed contiguous little-endian bytes of a zero-offset tensor.
    pub(crate) fn copy_contiguous_raw_bytes(&self) -> MoraResult<Vec<u8>> {
        if !self.is_contiguous() || self.offset != 0 {
            return Err(MoraError::Runtime(
                "copy_contiguous_raw_bytes requires a contiguous zero-offset Tensor".to_string(),
            ));
        }
        let guard = self.storage.read_host();
        Ok(guard.copy_bytes_out())
    }

    /// P5-1B: Overwrite a contiguous zero-offset tensor's storage from raw little-endian bytes
    /// (supporting `f16`/`bf16`/`u8`/`i32`/`f32`/`f64` buffers from CPU kernel execution).
    pub(crate) fn overwrite_contiguous_raw(
        &self,
        raw: &[u8],
        expected_dtype: DType,
    ) -> MoraResult<()> {
        if self.dtype != expected_dtype || !self.is_contiguous() || self.offset != 0 {
            return Err(MoraError::Runtime(format!(
                "overwrite_contiguous_raw requires a contiguous zero-offset {} Tensor",
                expected_dtype.canonical_type_name()
            )));
        }
        let next = AlignedByteBuffer::from_raw_bytes(raw, self.numel(), expected_dtype)?;
        {
            let mut guard = self.storage.write_host();
            *guard = next;
        }
        self.storage.sync_host_to_vram();
        Ok(())
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
        let storage =
            TensorStorage::for_device(AlignedByteBuffer::from_values(values, dtype), &device)?;
        Ok(Arc::new(Self {
            storage,
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_nodes: Vec::new(),
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
        let storage = TensorStorage::for_device(AlignedByteBuffer::zeroed(numel, dtype), &device)?;
        Ok(Arc::new(Self {
            storage,
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_nodes: Vec::new(),
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
        let storage = TensorStorage::for_device(
            AlignedByteBuffer::with_value(numel, fill_val, dtype),
            &device,
        )?;
        Ok(Arc::new(Self {
            storage,
            shape,
            strides,
            offset: 0,
            dtype,
            device,
            requires_grad,
            is_view: false,
            grad_nodes: Vec::new(),
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
            data.push(core.session.random_state.next_gauss());
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
            data.push(core.session.random_state.next_f64());
        }
        Self::from_raw(data, shape, dtype, device, requires_grad)
    }

    /// Construct a 1D `Tensor<u8, [N], cpu>` directly from raw bytes with zero per-byte boxing.
    pub(crate) fn from_u8_vec(bytes: &[u8]) -> MoraResult<Arc<Self>> {
        let len = bytes.len();
        let buf = AlignedByteBuffer::from_raw_bytes(bytes, len, DType::U8)?;
        Self::from_aligned_buffer(buf, vec![len], Device::Cpu, false)
    }

    /// Materialize the exact little-endian hardware-packed bytes of this tensor (handling slices/views).
    pub(crate) fn to_raw_byte_vec(self: &Arc<Self>) -> MoraResult<Vec<u8>> {
        let numel = self.numel();
        let guard = self.storage.read_host();
        if self.is_contiguous() {
            if self.offset == 0 && numel == guard.len() {
                return Ok(guard.copy_bytes_out());
            }
            if self.dtype != DType::I4 {
                let elem_bytes = self.dtype.storage_bytes(1);
                let byte_start = self.offset * elem_bytes;
                let byte_len = numel * elem_bytes;
                let mut out = Vec::with_capacity(byte_len);
                let mut pos = byte_start;
                let end = byte_start + byte_len;
                while pos < end {
                    let blk_idx = pos / 64;
                    let blk_off = pos % 64;
                    let take = (64 - blk_off).min(end - pos);
                    out.extend_from_slice(&guard.blocks[blk_idx].0[blk_off..blk_off + take]);
                    pos += take;
                }
                return Ok(out);
            }
        }
        drop(guard);
        let contig = self.contiguous()?;
        let guard = contig.storage.read_host();
        Ok(guard.copy_bytes_out())
    }

    /// Return this tensor's raw little-endian hardware bytes as a 1D `Tensor<u8, [nbytes], cpu>`.
    pub(crate) fn to_bytes(self: &Arc<Self>) -> MoraResult<Arc<Self>> {
        let raw = self.to_raw_byte_vec()?;
        Self::from_u8_vec(&raw)
    }

    /// Reconstruct a `Tensor<dtype, shape, device>` losslessly from a `Tensor<u8>` byte buffer.
    pub(crate) fn from_bytes(
        buf: &Arc<Self>,
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
        requires_grad: bool,
    ) -> MoraResult<Arc<Self>> {
        if buf.dtype != DType::U8 {
            return Err(MoraError::Runtime(format!(
                "Tensor::from_bytes: buffer must have dtype 'u8', got '{}'",
                buf.dtype.short_name()
            )));
        }
        let numel = checked_numel(&shape)?;
        let raw = buf.to_raw_byte_vec()?;
        let aligned = AlignedByteBuffer::from_raw_bytes(&raw, numel, dtype)?;
        Self::from_aligned_buffer(aligned, shape, device, requires_grad)
    }

    /// Encode a UTF-8 string into a 1D `Tensor<u8, [N], cpu>` byte tensor.
    pub(crate) fn from_utf8(text: &str) -> MoraResult<Arc<Self>> {
        Self::from_u8_vec(text.as_bytes())
    }

    /// Decode a `Tensor<u8>` byte tensor into a validated UTF-8 string.
    pub(crate) fn to_utf8(self: &Arc<Self>) -> MoraResult<String> {
        if self.dtype != DType::U8 {
            return Err(MoraError::Runtime(format!(
                "Tensor.to_utf8: expected dtype 'u8', got '{}'",
                self.dtype.short_name()
            )));
        }
        let raw = self.to_raw_byte_vec()?;
        String::from_utf8(raw)
            .map_err(|e| MoraError::Runtime(format!("Tensor.to_utf8: invalid UTF-8 bytes: {e}")))
    }

    /// Concatenate a non-empty sequence of tensors along `axis` (supports negative axes).
    pub(crate) fn cat(tensors: &[Arc<Self>], axis: isize) -> MoraResult<Arc<Self>> {
        if tensors.is_empty() {
            return Err(MoraError::Runtime(
                "Tensor::cat: expects a non-empty list of tensors".to_string(),
            ));
        }
        let first = &tensors[0];
        let rank = first.shape.len();
        if rank == 0 {
            return Err(MoraError::Runtime(
                "Tensor::cat: cannot concatenate 0D scalar tensors".to_string(),
            ));
        }
        let resolved = if axis < 0 {
            axis + (rank as isize)
        } else {
            axis
        };
        if resolved < 0 || (resolved as usize) >= rank {
            return Err(MoraError::Runtime(format!(
                "Tensor::cat: axis {axis} out of bounds for {rank}D tensor"
            )));
        }
        let ax = resolved as usize;
        let dtype = first.dtype;
        let device = first.device.clone();

        let mut out_shape = first.shape.clone();
        let mut concat_dim_total = first.shape[ax];
        for (idx, t) in tensors.iter().enumerate().skip(1) {
            if t.dtype != dtype {
                return Err(MoraError::Runtime(format!(
                    "Tensor::cat: dtype mismatch at index {idx}: expected '{}', got '{}'",
                    dtype.short_name(),
                    t.dtype.short_name()
                )));
            }
            if t.device != device {
                return Err(MoraError::Runtime(format!(
                    "Tensor::cat: device mismatch at index {idx}: expected '{device}', got '{}'",
                    t.device
                )));
            }
            if t.shape.len() != rank {
                return Err(MoraError::Runtime(format!(
                    "Tensor::cat: rank mismatch at index {idx}: expected {rank}D, got {}D",
                    t.shape.len()
                )));
            }
            for d in 0..rank {
                if d != ax && t.shape[d] != first.shape[d] {
                    return Err(MoraError::Runtime(format!(
                        "Tensor::cat: shape mismatch at index {idx} dim {d}: expected {}, got {}",
                        first.shape[d], t.shape[d]
                    )));
                }
            }
            concat_dim_total = concat_dim_total.checked_add(t.shape[ax]).ok_or_else(|| {
                MoraError::Runtime("Tensor::cat: concatenated dimension overflow".to_string())
            })?;
        }
        out_shape[ax] = concat_dim_total;
        let total_numel = checked_numel(&out_shape)?;

        // Fast direct-byte memcpy path for axis == 0 on byte-addressable dtypes (critical for binary I/O).
        if ax == 0 && dtype != DType::I4 {
            let total_bytes = dtype.storage_bytes(total_numel);
            let mut raw = Vec::with_capacity(total_bytes);
            for t in tensors {
                raw.extend_from_slice(&t.to_raw_byte_vec()?);
            }
            let buf = AlignedByteBuffer::from_raw_bytes(&raw, total_numel, dtype)?;
            return Self::from_aligned_buffer(buf, out_shape, device, false);
        }

        // General ND axis concatenation preserving exact hardware values.
        let outer: usize = out_shape[..ax].iter().copied().product::<usize>().max(1);
        let inner: usize = out_shape[ax + 1..]
            .iter()
            .copied()
            .product::<usize>()
            .max(1);
        let mut vals = Vec::with_capacity(total_numel);
        let flats: Vec<Vec<Value>> = tensors.iter().map(|t| t.to_flat_values()).collect();
        for o in 0..outer {
            for (ti, t) in tensors.iter().enumerate() {
                let chunk = t.shape[ax] * inner;
                let base = o * chunk;
                vals.extend_from_slice(&flats[ti][base..base + chunk]);
            }
        }
        Self::from_values(&vals, out_shape, dtype, device, false)
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

    /// Physical buffer identity (GPU VRAM handle when VRAM-resident without triggering D2H sync,
    /// or 64-byte aligned host pointer otherwise) for deterministic zero-copy & FBIP verification.
    pub fn storage_id(&self) -> i64 {
        if let Some(vbuf) = self.storage.vram_buffer() {
            return vbuf.raw_handle() as i64;
        }
        self.storage.aligned_byte_ptr() as usize as i64
    }

    /// Read the element at multi-dimensional `coords` as `f64`.
    pub fn get_at(&self, coords: &[usize]) -> f64 {
        let mut idx = self.offset as isize;
        for (&c, &s) in coords.iter().zip(self.strides.iter()) {
            idx += (c as isize) * s;
        }
        let guard = self.storage.read_host();
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
        let guard = self.storage.read_host();
        guard.get_f64(phys)
    }

    /// Read the element at logical flat row-major index `linear_idx` as an exact Mora [`Value`].
    pub fn get_linear_value(&self, linear_idx: usize) -> Value {
        let phys = self.physical_index(linear_idx);
        let guard = self.storage.read_host();
        guard.get_value(phys)
    }

    /// Materialize the logical elements of this tensor view as `f64` in contiguous row-major order.
    pub fn to_flat_vec(&self) -> Vec<f64> {
        let n = self.numel();
        let guard = self.storage.read_host();
        if self.is_contiguous() {
            return (0..n).map(|i| guard.get_f64(self.offset + i)).collect();
        }
        drop(guard);
        (0..n).map(|i| self.get_linear(i)).collect()
    }

    /// Materialize the logical elements of this tensor view as exact Mora [`Value`]s.
    pub fn to_flat_values(&self) -> Vec<Value> {
        let n = self.numel();
        let guard = self.storage.read_host();
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
            grad_nodes: Vec::new(),
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
                grad_nodes: Vec::new(),
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

    /// Transfer tensor to `device` (allocating & uploading physical GPU VRAM when targeting a live GPU).
    pub fn to_device(self: &Arc<Self>, device: Device) -> MoraResult<Arc<Self>> {
        if self.device == device && !self.is_view && Arc::strong_count(&self.storage) == 1 {
            return Ok(Arc::clone(self));
        }
        let raw = self.to_raw_byte_vec()?;
        let aligned = AlignedByteBuffer::from_raw_bytes(&raw, self.numel(), self.dtype)?;
        Self::from_aligned_buffer(aligned, self.shape.clone(), device, self.requires_grad)
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
            cloned.grad_nodes.clear();
        }
        Arc::new(cloned)
    }

    /// Look up this tensor's local node index on `tape_id`, if tracked by that tape.
    #[inline]
    pub(crate) fn node_on_tape(&self, tape_id: u64) -> Option<usize> {
        self.grad_nodes
            .iter()
            .rev()
            .find_map(|&(tid, idx)| if tid == tape_id { Some(idx) } else { None })
    }

    /// Return a copy of this tensor attached to all active tape nodes in `nodes`.
    pub(crate) fn with_grad_nodes(self: &Arc<Self>, nodes: Vec<TapeNodeId>) -> Arc<Self> {
        let mut cloned = (**self).clone();
        if !nodes.is_empty() {
            cloned.requires_grad = true;
        }
        cloned.grad_nodes = nodes;
        Arc::new(cloned)
    }

    /// Return a copy of this tensor preserving existing outer tape memberships and pushing `node`
    /// for a newly entered inner tape (`grad(grad(f))`, `jvp(grad(f))`, `trace_dag(grad(f))`).
    pub(crate) fn with_pushed_grad_node(self: &Arc<Self>, node: TapeNodeId) -> Arc<Self> {
        let mut cloned = (**self).clone();
        cloned.grad_nodes.retain(|&(tid, _)| tid != node.0);
        cloned.grad_nodes.push(node);
        cloned.requires_grad = true;
        Arc::new(cloned)
    }

    /// Sum reduction along `axis` with optional multi-tape autograd tracking.
    pub(crate) fn sum_axis_tracked(
        self: &Arc<Self>,
        axis: usize,
        keepdim: bool,
        autograd: Option<&mut AutogradContext>,
    ) -> MoraResult<Arc<Self>> {
        if axis >= self.shape.len() {
            return Err(MoraError::Runtime(format!(
                "Tensor.sum_axis: axis {axis} out of bounds for rank {}",
                self.shape.len()
            )));
        }
        let flat = self.to_flat_vec();
        let out_shape = crate::runtime::autograd::sum_axis_output_shape(&self.shape, axis, keepdim);
        let out_vals = crate::runtime::autograd::sum_axis_flat(&flat, &self.shape, axis, keepdim);
        let out = Self::from_raw(out_vals, out_shape, self.dtype, self.device.clone(), false)?;
        if let Some(ag) = autograd {
            let orig = self.shape.clone();
            return Ok(ag.record_unary(self, out, |src| TapeOp::SumAxis {
                a: src,
                axis,
                keepdim,
                orig_shape: orig.clone(),
            }));
        }
        Ok(out)
    }

    /// Flat gather with optional multi-tape autograd tracking.
    pub(crate) fn gather_tracked(
        self: &Arc<Self>,
        out_shape: Vec<usize>,
        src_flat_indices: Vec<usize>,
        autograd: Option<&mut AutogradContext>,
    ) -> MoraResult<Arc<Self>> {
        let flat = self.to_flat_vec();
        let gathered: Vec<f64> = src_flat_indices
            .iter()
            .map(|&si| flat.get(si).copied().unwrap_or(0.0))
            .collect();
        let out = Self::from_raw(gathered, out_shape, self.dtype, self.device.clone(), false)?;
        if let Some(ag) = autograd {
            let src_numel = self.numel();
            return Ok(ag.record_unary(self, out, |src| TapeOp::Gather {
                a: src,
                src_numel,
                src_flat_indices: src_flat_indices.clone(),
            }));
        }
        Ok(out)
    }

    /// Flat scatter-add (adjoint of `gather_tracked`) with optional multi-tape autograd tracking.
    pub(crate) fn scatter_add_tracked(
        self: &Arc<Self>,
        out_shape: Vec<usize>,
        dst_flat_indices: Vec<usize>,
        autograd: Option<&mut AutogradContext>,
    ) -> MoraResult<Arc<Self>> {
        let out_numel = checked_numel(&out_shape)?;
        let flat = self.to_flat_vec();
        let scattered =
            crate::runtime::autograd::scatter_add_flat(&flat, out_numel, &dst_flat_indices);
        let out = Self::from_raw(
            scattered,
            out_shape.clone(),
            self.dtype,
            self.device.clone(),
            false,
        )?;
        if let Some(ag) = autograd {
            return Ok(ag.record_unary(self, out, |src| TapeOp::ScatterAdd {
                a: src,
                out_shape: out_shape.clone(),
                dst_flat_indices: dst_flat_indices.clone(),
            }));
        }
        Ok(out)
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
        let mut guard = self.storage.write_host();
        for i in 0..n {
            guard.set_value(self.offset + i, val);
        }
        drop(guard);
        self.storage.sync_host_to_vram();
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
        let mut guard = self.storage.write_host();
        for (i, v) in flat.iter().enumerate() {
            guard.set_value(self.offset + i, v);
        }
        drop(guard);
        self.storage.sync_host_to_vram();
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
        let mut guard = self.storage.write_host();
        for (i, v) in flat.iter().enumerate() {
            guard.set_value(self.offset + i, v);
        }
        drop(guard);
        self.storage.sync_host_to_vram();
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

    /// Numerically stable log-softmax along the last axis.
    pub fn log_softmax(&self) -> MoraResult<Arc<Self>> {
        if !self.dtype.is_float() {
            return Err(MoraError::Runtime(format!(
                "Tensor.log_softmax requires a floating-point dtype, got {}",
                self.dtype
            )));
        }
        let last_dim = *self.shape.last().unwrap_or(&1);
        let mut flat = self.to_flat_vec();
        for row in flat.chunks_mut(last_dim) {
            let max_v = row.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let sum_exp: f64 = row.iter().map(|&v| (v - max_v).exp()).sum();
            let log_sum_exp = max_v + sum_exp.ln();
            for v in row.iter_mut() {
                *v = quantize_for_dtype(*v - log_sum_exp, self.dtype);
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

    /// Contiguous N-D slice `[starts[d]..ends[d]]` with zero-overhead metadata.
    pub(crate) fn slice_contiguous(
        &self,
        starts: &[usize],
        ends: &[usize],
    ) -> MoraResult<Arc<Self>> {
        let rank = self.shape.len();
        if starts.len() != rank || ends.len() != rank {
            return Err(MoraError::Runtime(format!(
                "Tensor::slice_contiguous: rank mismatch ({rank} vs starts={} ends={})",
                starts.len(),
                ends.len()
            )));
        }
        let mut out_shape = Vec::with_capacity(rank);
        for d in 0..rank {
            if starts[d] >= ends[d] || ends[d] > self.shape[d] {
                return Err(MoraError::Runtime(format!(
                    "Tensor::slice_contiguous: invalid range {}..{} for dim {} of size {}",
                    starts[d], ends[d], d, self.shape[d]
                )));
            }
            out_shape.push(ends[d] - starts[d]);
        }
        let src = self.to_flat_vec();
        let data = crate::runtime::autograd::slice_flat(&src, &self.shape, starts, ends);
        Self::from_raw(
            data,
            out_shape,
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Zero-pad each axis `d` by `(pads[d].0, pads[d].1)`.
    pub(crate) fn pad_zeros(&self, pads: &[(usize, usize)]) -> MoraResult<Arc<Self>> {
        let rank = self.shape.len();
        if pads.len() != rank {
            return Err(MoraError::Runtime(format!(
                "Tensor::pad_zeros: rank mismatch ({rank} vs pads={})",
                pads.len()
            )));
        }
        let out_shape: Vec<usize> = self
            .shape
            .iter()
            .zip(pads.iter())
            .map(|(&s, &(lo, hi))| s + lo + hi)
            .collect();
        let src = self.to_flat_vec();
        let data = crate::runtime::autograd::pad_flat(&src, &self.shape, pads, &out_shape);
        Self::from_raw(
            data,
            out_shape,
            self.dtype,
            self.device.clone(),
            self.requires_grad,
        )
    }

    /// Root Mean Square Layer Normalization (`RMSNorm`) along the last axis.
    pub fn rms_norm(&self, eps: f64) -> MoraResult<Arc<Self>> {
        RowNorm::Rms.allocated(self, eps)
    }

    /// 沿最后一维执行 LayerNorm。
    pub fn layer_norm(&self, eps: f64) -> MoraResult<Arc<Self>> {
        RowNorm::Layer.allocated(self, eps)
    }

    /// Index along axis 0 (`t[idx]`): supports negative indices and optional multi-tape autograd tracking.
    pub fn index_axis0_tracked(
        self: &Arc<Self>,
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
        let is_tracked = autograd.as_ref().is_some_and(|ag| ag.is_tracked(self));

        if self.shape.len() == 1 {
            if let Some(ag) = autograd
                && is_tracked
            {
                let val = self.storage.read_host().get_f64(base as usize);
                let out =
                    Tensor::from_raw(vec![val], Vec::new(), self.dtype, self.device.clone(), true)?;
                let src_numel = self.numel();
                let tracked = ag.record_unary(self, out, |src_node| TapeOp::Gather {
                    a: src_node,
                    src_numel,
                    src_flat_indices: vec![idx_u],
                });
                return Ok(Value::Tensor(tracked));
            }
            let guard = self.storage.read_host();
            Ok(guard.get_value(base as usize))
        } else {
            let new_shape = self.shape[1..].to_vec();
            let view = Arc::new(Self {
                storage: Arc::clone(&self.storage),
                shape: new_shape.clone(),
                strides: self.strides[1..].to_vec(),
                offset: base as usize,
                dtype: self.dtype,
                device: self.device.clone(),
                requires_grad: self.requires_grad,
                is_view: true,
                grad_nodes: Vec::new(),
            });
            if let Some(ag) = autograd
                && is_tracked
            {
                let row_size: usize = new_shape.iter().copied().product::<usize>().max(1);
                let start_flat = idx_u * row_size;
                let indices: Vec<usize> = (start_flat..start_flat + row_size).collect();
                let src_numel = self.numel();
                let tracked = ag.record_unary(self, view, |src_node| TapeOp::Gather {
                    a: src_node,
                    src_numel,
                    src_flat_indices: indices.clone(),
                });
                return Ok(Value::Tensor(tracked));
            }
            Ok(Value::Tensor(view))
        }
    }

    /// Stage 3C: Zero-copy multi-dimensional strided slicing `t[0..4, :, -1]`, `t[1:5:2, ..., None]`.
    pub fn slice_nd(
        self: &Arc<Self>,
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

        let is_tracked = autograd.as_ref().is_some_and(|ag| ag.is_tracked(self));

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
            if let Some(ag) = autograd
                && is_tracked
            {
                let indices = build_gather_indices();
                let val = self.storage.read_host().get_f64(new_offset as usize);
                let out =
                    Tensor::from_raw(vec![val], Vec::new(), self.dtype, self.device.clone(), true)?;
                let src_numel = self.numel();
                let tracked = ag.record_unary(self, out, |src_node| TapeOp::Gather {
                    a: src_node,
                    src_numel,
                    src_flat_indices: indices.clone(),
                });
                return Ok(Value::Tensor(tracked));
            }
            let guard = self.storage.read_host();
            return Ok(guard.get_value(new_offset as usize));
        }

        let contiguous_slice_bounds = if is_tracked && new_shape.len() == self.shape.len() {
            let mut starts = Vec::with_capacity(self.shape.len());
            let mut ends = Vec::with_capacity(self.shape.len());
            let mut all_unit = true;
            for (d, am) in axis_maps.iter().enumerate() {
                match *am {
                    AxisMap::Linear {
                        out_axis,
                        start,
                        step: 1,
                    } if out_axis == d && start >= 0 => {
                        let s = start as usize;
                        starts.push(s);
                        ends.push(s + new_shape[d]);
                    }
                    _ => {
                        all_unit = false;
                        break;
                    }
                }
            }
            if all_unit { Some((starts, ends)) } else { None }
        } else {
            None
        };

        let indices_opt = if is_tracked && contiguous_slice_bounds.is_none() {
            Some(build_gather_indices())
        } else {
            None
        };
        let view = Arc::new(Self {
            storage: Arc::clone(&self.storage),
            shape: new_shape,
            strides: new_strides,
            offset: new_offset as usize,
            dtype: self.dtype,
            device: self.device.clone(),
            requires_grad: self.requires_grad,
            is_view: true,
            grad_nodes: Vec::new(),
        });
        if let Some(ag) = autograd {
            if let Some((starts, ends)) = contiguous_slice_bounds {
                let orig_shape = self.shape.clone();
                let tracked = ag.record_unary(self, view, |src_node| TapeOp::Slice {
                    a: src_node,
                    starts: starts.clone(),
                    ends: ends.clone(),
                    orig_shape: orig_shape.clone(),
                });
                return Ok(Value::Tensor(tracked));
            }
            if let Some(indices) = indices_opt {
                let src_numel = self.numel();
                let tracked = ag.record_unary(self, view, |src_node| TapeOp::Gather {
                    a: src_node,
                    src_numel,
                    src_flat_indices: indices.clone(),
                });
                return Ok(Value::Tensor(tracked));
            }
        }

        Ok(Value::Tensor(view))
    }

    /// Index along axis 0 (`t[idx]`): returns an exact scalar `Value` for 1D tensors or a
    /// zero-copy strided `(rank - 1)`-dimensional view `Value::Tensor` for ND (`rank >= 2`).
    pub fn index_axis0(self: &Arc<Self>, idx: usize) -> MoraResult<Value> {
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
        let mut guard = self.storage.write_host();
        for (i, &rhs_v) in rhs_flat.iter().enumerate().take(numel) {
            let cur = guard.get_f64(self.offset + i);
            guard.set_f64(self.offset + i, cur + rhs_v);
        }
        drop(guard);
        self.storage.sync_host_to_vram();
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
        let mut guard = self.storage.write_host();
        for (i, &rhs_v) in rhs_flat.iter().enumerate().take(numel) {
            let cur = guard.get_f64(self.offset + i);
            guard.set_f64(self.offset + i, cur * rhs_v);
        }
        drop(guard);
        self.storage.sync_host_to_vram();
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
        Some(Value::String(s)) => Device::parse(s),
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
        "from_bytes" => &["bytes", "shape", "dtype", "device", "requires_grad"],
        "from_utf8" => &["text"],
        "cat" => &["tensors", "axis"],
        "devices" => &[],
        "device_info" => &["device"],
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
        "devices" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "Tensor::devices() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            let devs: Vec<Value> = crate::runtime::gpu::list_hardware_devices()
                .into_iter()
                .map(|info| Value::String(info.device.to_string()))
                .collect();
            Ok(Value::List(devs.into()))
        }
        "device_info" => {
            if args.len() > 1 {
                return Err(MoraError::Runtime(format!(
                    "Tensor::device_info(device?) expects 0..=1 arguments, got {}",
                    args.len()
                )));
            }
            let dev = parse_optional_device(args.first(), "Tensor::device_info")?;
            let map: std::collections::HashMap<String, Value> =
                crate::runtime::gpu::hardware_device_info_map(&dev)
                    .into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect();
            Ok(Value::Dict(map.into()))
        }
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
        "from_bytes" => {
            if args.len() < 3 {
                return Err(MoraError::Runtime(
                    "Tensor::from_bytes(bytes, shape, dtype, device?, requires_grad?) requires at least 3 arguments".to_string(),
                ));
            }
            let Value::Tensor(buf) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "Tensor::from_bytes: first argument must be a u8 Tensor, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let shape = parse_shape_arg(&args[1], "Tensor::from_bytes")?;
            let dtype =
                parse_optional_dtype(args.get(2), "Tensor::from_bytes")?.ok_or_else(|| {
                    MoraError::Runtime("Tensor::from_bytes requires a dtype string".to_string())
                })?;
            let device = parse_optional_device(args.get(3), "Tensor::from_bytes")?;
            let requires_grad = parse_optional_bool(args.get(4), "Tensor::from_bytes")?;
            Ok(Value::Tensor(Tensor::from_bytes(
                buf,
                shape,
                dtype,
                device,
                requires_grad,
            )?))
        }
        "from_utf8" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(format!(
                    "Tensor::from_utf8(text) expects 1 argument, got {}",
                    args.len()
                )));
            }
            let Value::String(text) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "Tensor::from_utf8: expected string, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            Ok(Value::Tensor(Tensor::from_utf8(text)?))
        }
        "cat" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "Tensor::cat(tensors, axis?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::List(list) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "Tensor::cat: first argument must be a list of tensors, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let mut tensors = Vec::with_capacity(list.len());
            for (idx, item) in list.iter().enumerate() {
                let Value::Tensor(t) = item else {
                    return Err(MoraError::Runtime(format!(
                        "Tensor::cat: element {idx} must be a Tensor, got {}",
                        crate::flow::type_name(item)
                    )));
                };
                tensors.push(Arc::clone(t));
            }
            let axis = match args.get(1) {
                None | Some(Value::Nil) => 0isize,
                Some(v) => {
                    let raw = parse_num_arg(v, "Tensor::cat")?;
                    if !raw.is_finite() || raw.fract() != 0.0 {
                        return Err(MoraError::Runtime(format!(
                            "Tensor::cat: axis must be an integer, got {raw}"
                        )));
                    }
                    raw as isize
                }
            };
            let out = Tensor::cat(&tensors, axis)?;
            let rank = tensors[0].shape.len() as isize;
            let resolved_ax = if axis < 0 { axis + rank } else { axis } as usize;
            if core.session.autograd.is_recording()
                && tensors.iter().any(|t| !t.grad_nodes.is_empty())
            {
                let input_shapes: Vec<Vec<usize>> =
                    tensors.iter().map(|t| t.shape.clone()).collect();
                let tracked = core.session.autograd.record_nary(&tensors, out, |mapped| {
                    let const_tensors: Vec<Option<Arc<Tensor>>> = mapped
                        .iter()
                        .enumerate()
                        .map(|(i, m)| {
                            if m.is_none() {
                                Some(Arc::clone(&tensors[i]))
                            } else {
                                None
                            }
                        })
                        .collect();
                    TapeOp::Cat {
                        inputs: mapped,
                        const_tensors,
                        input_shapes: input_shapes.clone(),
                        axis: resolved_ax,
                    }
                });
                return Ok(Value::Tensor(tracked));
            }
            Ok(Value::Tensor(out))
        }
        _ => Err(MoraError::Runtime(format!(
            "Tensor has no associated function 'Tensor::{assoc}'"
        ))),
    }
}

/// 复用必须保留物理布局与结果类型。嵌套 no_grad 暂停记录，但外层磁带仍可读取旧值。
/// 没有上下文的宿主调用无法判断已有节点的磁带寿命，保留这些节点。
fn can_reuse_tensor_in_place(
    t: &Arc<Tensor>,
    autograd: Option<&AutogradContext>,
    dtype: DType,
    shape: &[usize],
    device: &Device,
) -> bool {
    t.dtype == dtype
        && t.shape == shape
        && &t.device == device
        && !t.is_view
        && Arc::strong_count(t) == 1
        && Arc::strong_count(&t.storage) == 1
        && t.storage
            .vram
            .read()
            .as_ref()
            .is_none_or(|buffer| Arc::strong_count(buffer) == 1)
        && t.is_contiguous()
        && t.storage.byte_len() == t.nbytes()
        && autograd.map_or_else(|| t.grad_nodes.is_empty(), |ag| !ag.has_live_tape_node(t))
}

#[inline]
fn prepare_reused_tensor_meta(t: &mut Arc<Tensor>, requires_grad: bool) {
    if (!t.grad_nodes.is_empty() || t.requires_grad != requires_grad)
        && let Some(inner) = Arc::get_mut(t)
    {
        inner.grad_nodes.clear();
        inner.requires_grad = requires_grad;
    }
}

/// Shared read-only inspection / reduction dispatch on `&Tensor` (0 arguments) used by BOTH
/// [`call_tensor_method_fbip`] and borrowed `RegSlot` method dispatch (`method.rs`).
pub(crate) fn eval_tensor_readonly(
    t: &Arc<Tensor>,
    method: &str,
    arity: usize,
    is_tracked: bool,
) -> Option<MoraResult<Value>> {
    if arity != 0
        && matches!(
            method,
            "storage_id" | "is_vram_resident" | "to_bytes" | "to_utf8"
        )
    {
        return Some(Err(MoraError::Runtime(format!(
            "Tensor.{method}() expects 0 arguments, got {arity}"
        ))));
    }
    match method {
        "shape" => Some(Ok(Value::List(
            t.shape
                .iter()
                .map(|&d| Value::Int(d as i64))
                .collect::<Vec<_>>()
                .into(),
        ))),
        "ndim" | "rank" => Some(Ok(Value::Int(t.rank() as i64))),
        "numel" => Some(Ok(Value::Int(t.numel() as i64))),
        "nbytes" | "byte_len" => Some(Ok(Value::Int(t.nbytes() as i64))),
        "storage_id" => Some(Ok(Value::Int(t.storage_id()))),
        "len" => Some(Ok(Value::Int(t.shape.first().copied().unwrap_or(0) as i64))),
        "dtype" => Some(Ok(Value::String(t.dtype.short_name().to_string()))),
        "device" => Some(Ok(Value::String(t.device.to_string()))),
        "is_contiguous" => Some(Ok(Value::Bool(t.is_contiguous()))),
        "is_vram_resident" => Some(Ok(Value::Bool(t.is_vram_resident()))),
        "requires_grad" => Some(Ok(Value::Bool(t.requires_grad))),
        "to_bytes" => Some(t.to_bytes().map(Value::Tensor)),
        "to_utf8" => Some(t.to_utf8().map(Value::String)),
        "to_list" => Some(Ok(t.to_nested_list())),
        "item" => {
            if t.numel() != 1 {
                Some(Err(MoraError::Runtime(format!(
                    "Tensor.item() requires a 1-element tensor, got {} elements (shape {:?})",
                    t.numel(),
                    t.shape
                ))))
            } else {
                Some(Ok(t.get_linear_value(0)))
            }
        }
        "sum" if !is_tracked => {
            let s: f64 = t.to_flat_vec().iter().sum();
            Some(Ok(scalar_value_for_dtype(s, t.dtype)))
        }
        "mean" if !is_tracked => {
            let flat = t.to_flat_vec();
            if flat.is_empty() {
                Some(Err(MoraError::Runtime(
                    "Tensor.mean() cannot be called on an empty tensor".to_string(),
                )))
            } else {
                let m = flat.iter().sum::<f64>() / (flat.len() as f64);
                let dt = if t.dtype.is_float() {
                    t.dtype
                } else {
                    DType::F64
                };
                Some(Ok(scalar_value_for_dtype(m, dt)))
            }
        }
        "max" => {
            let flat = t.to_flat_vec();
            match flat.first() {
                None => Some(Err(MoraError::Runtime(
                    "Tensor.max() cannot be called on an empty tensor".to_string(),
                ))),
                Some(&first) => {
                    let m = flat.iter().copied().fold(first, f64::max);
                    Some(Ok(scalar_value_for_dtype(m, t.dtype)))
                }
            }
        }
        "min" => {
            let flat = t.to_flat_vec();
            match flat.first() {
                None => Some(Err(MoraError::Runtime(
                    "Tensor.min() cannot be called on an empty tensor".to_string(),
                ))),
                Some(&first) => {
                    let m = flat.iter().copied().fold(first, f64::min);
                    Some(Ok(scalar_value_for_dtype(m, t.dtype)))
                }
            }
        }
        _ => None,
    }
}
/// Sentinel error returned when a tensor method name is not a built-in `Tensor` method, returning
/// ownership of `Arc<Tensor>` intact (`strong_count` preserved) for user `impl` lookup.
pub(crate) enum TensorMethodDispatch {
    Handled(MoraResult<Value>),
    UnknownMethod(Arc<Tensor>, MoraError),
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
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    match call_tensor_method_fbip(Arc::clone(t), method, args, autograd, None) {
        TensorMethodDispatch::Handled(res) => res,
        TensorMethodDispatch::UnknownMethod(_, err) => Err(err),
    }
}

/// P6-1 Perceus/FBIP: Dispatch `t.<method>(args...)` consuming `t: Arc<Tensor>` by ownership so
/// uniquely-owned contiguous tensors reuse their physical 64B host or GPU VRAM buffer in place.
pub(crate) fn call_tensor_method_fbip(
    mut t: Arc<Tensor>,
    method: &str,
    args: &[Value],
    mut autograd: Option<&mut AutogradContext>,
    fbip_stats: Option<&crate::runtime::fbip::FbipCounters>,
) -> TensorMethodDispatch {
    let is_tracked = autograd.as_ref().is_some_and(|ag| ag.is_tracked(&t));
    if let Some(result) = eval_tensor_readonly(&t, method, args.len(), is_tracked) {
        return TensorMethodDispatch::Handled(result);
    }
    if !matches!(
        method,
        "contiguous"
            | "with_grad"
            | "reshape"
            | "transpose"
            | "to_dtype"
            | "to_device"
            | "to"
            | "fill_"
            | "add_"
            | "mul_"
            | "sum"
            | "mean"
            | "abs"
            | "sqrt"
            | "exp"
            | "ln"
            | "log"
            | "sin"
            | "cos"
            | "relu"
            | "gelu"
            | "silu"
            | "tanh"
            | "sigmoid"
            | "softmax"
            | "log_softmax"
            | "rms_norm"
            | "layer_norm"
    ) {
        return TensorMethodDispatch::UnknownMethod(
            t,
            MoraError::Runtime(format!("Tensor has no method: {method}")),
        );
    }
    let res: MoraResult<Value> = (|| match method {
        "contiguous" => {
            let out = t.contiguous()?;
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                return Ok(Value::Tensor(
                    ag.record_unary(&t, out, |src| TapeOp::Identity { a: src }),
                ));
            }
            Ok(Value::Tensor(out))
        }
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
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                let orig = t.shape.clone();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |src| {
                    TapeOp::Reshape {
                        a: src,
                        orig_shape: orig.clone(),
                    }
                })));
            }
            Ok(Value::Tensor(out))
        }
        "transpose" => {
            let rank = t.shape.len();
            let norm_axis = |v: &Value| -> MoraResult<usize> {
                let raw = parse_num_arg(v, "Tensor.transpose")?;
                if !raw.is_finite() || raw.fract() != 0.0 {
                    return Err(MoraError::Runtime(format!(
                        "Tensor.transpose: axis must be an integer, got {raw}"
                    )));
                }
                let idx = raw as isize;
                let resolved = if idx < 0 { idx + (rank as isize) } else { idx };
                if resolved < 0 || (resolved as usize) >= rank {
                    return Err(MoraError::Runtime(format!(
                        "Tensor.transpose axis {idx} out of bounds for {rank}D tensor"
                    )));
                }
                Ok(resolved as usize)
            };
            let d0 = match args.first() {
                None | Some(Value::Nil) => None,
                Some(v) => Some(norm_axis(v)?),
            };
            let d1 = match args.get(1) {
                None | Some(Value::Nil) => None,
                Some(v) => Some(norm_axis(v)?),
            };
            let eff_d0 = d0.unwrap_or_else(|| rank.saturating_sub(2));
            let eff_d1 = d1.unwrap_or_else(|| rank.saturating_sub(1));
            let out = t.transpose(d0, d1)?;
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                let orig = t.shape.clone();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |src| {
                    TapeOp::Transpose {
                        a: src,
                        dim0: eff_d0,
                        dim1: eff_d1,
                        orig_shape: orig.clone(),
                    }
                })));
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
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                return Ok(Value::Tensor(
                    ag.record_unary(&t, out, |src| TapeOp::Identity { a: src }),
                ));
            }
            Ok(Value::Tensor(out))
        }
        "to_device" => {
            let Some(Value::String(s)) = args.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.to_device(device) requires a device string argument".to_string(),
                ));
            };
            let dev = Device::parse(s)?;
            let out = t.to_device(dev)?;
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                return Ok(Value::Tensor(
                    ag.record_unary(&t, out, |src| TapeOp::Identity { a: src }),
                ));
            }
            Ok(Value::Tensor(out))
        }
        "to" => {
            let Some(Value::String(first)) = args.first() else {
                return Err(MoraError::Runtime(
                    "Tensor.to(dtype_or_device, second?) requires a string argument".to_string(),
                ));
            };
            let mut cur = Arc::clone(&t);
            if let Some(dt) = DType::from_name(first) {
                cur = cur.to_dtype(dt)?;
            } else {
                let dev = Device::parse(first)?;
                cur = cur.to_device(dev)?;
            }
            if let Some(Value::String(second)) = args.get(1) {
                if let Some(dt) = DType::from_name(second) {
                    cur = cur.to_dtype(dt)?;
                } else {
                    let dev = Device::parse(second)?;
                    cur = cur.to_device(dev)?;
                }
            }
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                return Ok(Value::Tensor(
                    ag.record_unary(&t, cur, |src| TapeOp::Identity { a: src }),
                ));
            }
            Ok(Value::Tensor(cur))
        }
        "fill_" => {
            let v = args.first().ok_or_else(|| {
                MoraError::Runtime("Tensor.fill_(value) requires 1 scalar argument".to_string())
            })?;
            t.fill_inplace(v)?;
            Ok(Value::Tensor(t))
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
            Ok(Value::Tensor(t))
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
            Ok(Value::Tensor(t))
        }
        "sum" => {
            let flat = t.to_flat_vec();
            let s: f64 = flat.iter().sum();
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                let out = Tensor::from_raw(vec![s], Vec::new(), t.dtype, t.device.clone(), true)?;
                let a_shape = t.shape.clone();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |src| TapeOp::Sum {
                    a: src,
                    a_shape: a_shape.clone(),
                })));
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
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                let out = Tensor::from_raw(vec![m], Vec::new(), dt, t.device.clone(), true)?;
                let a_shape = t.shape.clone();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |src| {
                    TapeOp::Mean {
                        a: src,
                        a_shape: a_shape.clone(),
                    }
                })));
            }
            Ok(scalar_value_for_dtype(m, dt))
        }
        "abs" | "sqrt" | "exp" | "ln" | "log" | "sin" | "cos" | "relu" | "gelu" | "silu"
        | "tanh" | "sigmoid" => eval_tensor_math_unary_fbip(t, method, autograd, fbip_stats),
        "softmax" => {
            if can_reuse_tensor_in_place(&t, autograd.as_deref(), t.dtype, &t.shape, &t.device)
                && t.dtype.is_float()
                && !t.is_vram_resident()
            {
                let last_dim = *t.shape.last().unwrap_or(&1);
                let dt = t.dtype;
                let mut guard = t.storage.write_host();
                let n = guard.len();
                let mut row_buf = vec![0.0; last_dim];
                let mut base = 0usize;
                while base < n {
                    let take = (n - base).min(last_dim);
                    let mut max_v = f64::NEG_INFINITY;
                    for (i, slot) in row_buf.iter_mut().enumerate().take(take) {
                        let v = guard.get_f64(base + i);
                        *slot = v;
                        if v > max_v {
                            max_v = v;
                        }
                    }
                    let mut sum = 0.0;
                    for v in row_buf.iter_mut().take(take) {
                        *v = (*v - max_v).exp();
                        sum += *v;
                    }
                    for (i, &v) in row_buf.iter().take(take).enumerate() {
                        let out = if sum > 0.0 { v / sum } else { v };
                        guard.set_f64(base + i, quantize_for_dtype(out, dt));
                    }
                    base += take;
                }
                drop(guard);
                let keep_grad = t.requires_grad;
                prepare_reused_tensor_meta(&mut t, keep_grad);
                if let Some(stats) = fbip_stats {
                    stats.on_tensor_host_reuse();
                }
                return Ok(Value::Tensor(t));
            }
            let out = t.softmax()?;
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                let last_dim = t.shape.last().copied().unwrap_or(1);
                let out_vals = out.to_flat_vec();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |src| {
                    TapeOp::Softmax {
                        a: src,
                        out_vals: out_vals.clone(),
                        last_dim,
                    }
                })));
            }
            Ok(Value::Tensor(out))
        }
        "log_softmax" => {
            let out = t.log_softmax()?;
            if let Some(ag) = autograd.as_deref_mut()
                && is_tracked
            {
                let last_dim = t.shape.last().copied().unwrap_or(1);
                let out_vals = out.to_flat_vec();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |src| {
                    TapeOp::LogSoftmax {
                        a: src,
                        out_vals: out_vals.clone(),
                        last_dim,
                    }
                })));
            }
            Ok(Value::Tensor(out))
        }
        "rms_norm" | "layer_norm" => {
            let norm = if method == "rms_norm" {
                RowNorm::Rms
            } else {
                RowNorm::Layer
            };
            let eps = match args.first() {
                None | Some(Value::Nil) => 1e-5,
                Some(v) => parse_num_arg(v, &format!("Tensor.{method}"))?,
            };
            if can_reuse_tensor_in_place(&t, autograd.as_deref(), t.dtype, &t.shape, &t.device) {
                norm.reuse(&t, eps, fbip_stats)?;
                let keep_grad = t.requires_grad;
                prepare_reused_tensor_meta(&mut t, keep_grad);
                return Ok(Value::Tensor(t));
            }
            let out = norm.allocated(&t, eps)?;
            if let Some(ag) = autograd
                && is_tracked
            {
                let last_dim = t.shape.last().copied().unwrap_or(1);
                let a_vals = t.to_flat_vec();
                return Ok(Value::Tensor(ag.record_unary(&t, out, |a| match norm {
                    RowNorm::Rms => TapeOp::RmsNorm {
                        a,
                        a_vals: a_vals.clone(),
                        last_dim,
                        eps,
                    },
                    RowNorm::Layer => TapeOp::LayerNorm {
                        a,
                        a_vals: a_vals.clone(),
                        last_dim,
                        eps,
                    },
                })));
            }
            Ok(Value::Tensor(out))
        }
        _ => unreachable!("checked recognized Tensor method set above"),
    })();

    TensorMethodDispatch::Handled(res)
}

fn unary_op_to_fused_step(op: &str) -> Option<crate::runtime::compute_dag::FusedEwStep> {
    use crate::runtime::compute_dag::FusedEwStep;
    match op {
        "neg" => Some(FusedEwStep::Neg(0)),
        "abs" => Some(FusedEwStep::Abs(0)),
        "sqrt" => Some(FusedEwStep::Sqrt(0)),
        "exp" => Some(FusedEwStep::Exp(0)),
        "ln" | "log" => Some(FusedEwStep::Ln(0)),
        "sin" => Some(FusedEwStep::Sin(0)),
        "cos" => Some(FusedEwStep::Cos(0)),
        "relu" => Some(FusedEwStep::Relu(0)),
        "gelu" => Some(FusedEwStep::Gelu(0)),
        "silu" => Some(FusedEwStep::Silu(0)),
        "tanh" => Some(FusedEwStep::Tanh(0)),
        "sigmoid" => Some(FusedEwStep::Sigmoid(0)),
        _ => None,
    }
}

fn binary_op_to_fused_step(
    op: &BinaryOp,
    lhs_idx: usize,
    rhs_idx: usize,
) -> Option<crate::runtime::compute_dag::FusedEwStep> {
    use crate::runtime::compute_dag::FusedEwStep;
    match op {
        BinaryOp::Add => Some(FusedEwStep::Add(lhs_idx, rhs_idx)),
        BinaryOp::Sub => Some(FusedEwStep::Sub(lhs_idx, rhs_idx)),
        BinaryOp::Mul => Some(FusedEwStep::Mul(lhs_idx, rhs_idx)),
        BinaryOp::Div => Some(FusedEwStep::Div(lhs_idx, rhs_idx)),
        BinaryOp::Pow => Some(FusedEwStep::Pow(lhs_idx, rhs_idx)),
        _ => None,
    }
}

fn binary_scalar_op_to_fused_step(
    op: &BinaryOp,
    scalar: f64,
    reversed: bool,
) -> Option<crate::runtime::compute_dag::FusedEwStep> {
    use crate::runtime::compute_dag::FusedEwStep;
    match op {
        BinaryOp::Add => Some(FusedEwStep::AddScalar(0, scalar)),
        BinaryOp::Sub => Some(FusedEwStep::SubScalar(0, scalar, reversed)),
        BinaryOp::Mul => Some(FusedEwStep::MulScalar(0, scalar)),
        BinaryOp::Div => Some(FusedEwStep::DivScalar(0, scalar, reversed)),
        BinaryOp::Pow => Some(FusedEwStep::PowScalar(0, scalar, reversed)),
        _ => None,
    }
}

/// Update an exclusively owned allocation. Without a native kernel for the requested
/// precision, retain the device allocation and propagate errors from both transfers.
fn reuse_storage(
    t: &Tensor,
    stats: Option<&crate::runtime::fbip::FbipCounters>,
    native: impl FnOnce(&Arc<dyn DeviceVramBuffer>) -> MoraResult<bool>,
    host: impl FnOnce(&mut AlignedByteBuffer) -> MoraResult<()>,
) -> MoraResult<()> {
    if let Some(buffer) = t.storage.vram_buffer() {
        if native(&buffer)? {
            t.storage.mark_host_stale();
        } else {
            let mut shadow = t.storage.data.write();
            if t.storage.host_stale.load(Ordering::Acquire) {
                let mut bytes = vec![0u8; buffer.byte_len()];
                buffer.download_bytes(&mut bytes)?;
                *shadow = AlignedByteBuffer::from_raw_bytes(&bytes, t.numel(), t.dtype)?;
            }
            // A failed operation or transfer leaves VRAM authoritative.
            t.storage.host_stale.store(true, Ordering::Release);
            host(&mut shadow)?;
            buffer.upload_bytes(&shadow.copy_bytes_out())?;
            t.storage.host_stale.store(false, Ordering::Release);
        }
        if let Some(stats) = stats {
            stats.on_tensor_vram_reuse();
        }
    } else {
        host(&mut t.storage.write_host())?;
        if let Some(stats) = stats {
            stats.on_tensor_host_reuse();
        }
    }
    Ok(())
}

fn reuse_elementwise(
    t: &Tensor,
    step: Option<crate::runtime::compute_dag::FusedEwStep>,
    stats: Option<&crate::runtime::fbip::FbipCounters>,
    mut value: impl FnMut(f64) -> MoraResult<f64>,
) -> MoraResult<()> {
    reuse_storage(
        t,
        stats,
        |buffer| {
            let Some(step) = step else { return Ok(false) };
            let backend = crate::runtime::compute_dag::backend_for_device(&t.device);
            Ok(backend
                .run_fused_elementwise_vram(
                    &[buffer.as_ref()],
                    &[&t.shape],
                    &[step],
                    &t.shape,
                    t.numel(),
                    t.dtype,
                    Some(Arc::clone(buffer)),
                )?
                .is_some())
        },
        |data| {
            for i in 0..data.len() {
                data.set_f64(i, value(data.get_f64(i))?);
            }
            Ok(())
        },
    )
}

/// Evaluate unary mathematical / activation functions on a [`Tensor`] with optional multi-tape autograd tracking.
pub fn eval_tensor_math_unary_tracked(
    t: &Arc<Tensor>,
    op: &str,
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    eval_tensor_math_unary_fbip(Arc::clone(t), op, autograd, None)
}

/// P6-1 Perceus/FBIP: Evaluate unary mathematical / activation functions consuming `t: Arc<Tensor>`
/// by ownership so dead contiguous inputs reuse their 64B host or GPU VRAM buffer in place.
pub fn eval_tensor_math_unary_fbip(
    mut t: Arc<Tensor>,
    op: &str,
    autograd: Option<&mut AutogradContext>,
    fbip_stats: Option<&crate::runtime::fbip::FbipCounters>,
) -> MoraResult<Value> {
    let is_tracked = autograd.as_ref().is_some_and(|ag| ag.is_tracked(&t));
    let evaluate: fn(f64) -> f64 = match op {
        "abs" => f64::abs,
        "sqrt" => f64::sqrt,
        "exp" => f64::exp,
        "ln" | "log" => f64::ln,
        "sin" => f64::sin,
        "cos" => f64::cos,
        "relu" => |x| x.max(0.0),
        "gelu" => |x| {
            let c = (2.0 / std::f64::consts::PI).sqrt();
            0.5 * x * (1.0 + (c * (x + 0.044715 * x.powi(3))).tanh())
        },
        "silu" => |x| x / (1.0 + (-x).exp()),
        "tanh" => f64::tanh,
        "sigmoid" => |x| 1.0 / (1.0 + (-x).exp()),
        _ => {
            return Err(MoraError::Runtime(format!(
                "Tensor has no unary math operation '{op}'"
            )));
        }
    };
    if can_reuse_tensor_in_place(&t, autograd.as_deref(), t.dtype, &t.shape, &t.device) {
        reuse_elementwise(&t, unary_op_to_fused_step(op), fbip_stats, |x| {
            Ok(evaluate(x))
        })?;
        let keep_grad = t.requires_grad;
        prepare_reused_tensor_meta(&mut t, keep_grad);
        return Ok(Value::Tensor(t));
    }
    let out = t.map_unary(evaluate)?;
    if let Some(ag) = autograd
        && is_tracked
    {
        let payload = match op {
            "sqrt" | "exp" | "tanh" | "sigmoid" => out.to_flat_vec(),
            _ => t.to_flat_vec(),
        };
        let tracked = ag.record_unary(&t, out, |src| match op {
            "abs" => TapeOp::Abs {
                a: src,
                a_vals: payload.clone(),
            },
            "sqrt" => TapeOp::Sqrt {
                a: src,
                out_vals: payload.clone(),
            },
            "exp" => TapeOp::Exp {
                a: src,
                out_vals: payload.clone(),
            },
            "ln" | "log" => TapeOp::Ln {
                a: src,
                a_vals: payload.clone(),
            },
            "sin" => TapeOp::Sin {
                a: src,
                a_vals: payload.clone(),
            },
            "cos" => TapeOp::Cos {
                a: src,
                a_vals: payload.clone(),
            },
            "relu" => TapeOp::Relu {
                a: src,
                a_vals: payload.clone(),
            },
            "gelu" => TapeOp::Gelu {
                a: src,
                a_vals: payload.clone(),
            },
            "silu" => TapeOp::Silu {
                a: src,
                a_vals: payload.clone(),
            },
            "tanh" => TapeOp::Tanh {
                a: src,
                out_vals: payload.clone(),
            },
            "sigmoid" => TapeOp::Sigmoid {
                a: src,
                out_vals: payload.clone(),
            },
            _ => unreachable!(),
        });
        return Ok(Value::Tensor(tracked));
    }
    Ok(Value::Tensor(out))
}

/// Evaluate unary `-tensor` with optional multi-tape autograd tracking.
pub fn eval_tensor_neg_tracked(
    t: &Arc<Tensor>,
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    eval_tensor_neg_fbip(Arc::clone(t), autograd, None)
}

/// P6-1 Perceus/FBIP: Evaluate unary `-tensor` consuming `t: Arc<Tensor>` by ownership.
pub fn eval_tensor_neg_fbip(
    mut t: Arc<Tensor>,
    autograd: Option<&mut AutogradContext>,
    fbip_stats: Option<&crate::runtime::fbip::FbipCounters>,
) -> MoraResult<Value> {
    if t.dtype.is_unsigned_int() || t.dtype.is_bool() {
        return Err(MoraError::Runtime(format!(
            "cannot negate Tensor<{}> (unsigned or bool dtype)",
            t.dtype
        )));
    }
    let is_tracked = autograd.as_ref().is_some_and(|ag| ag.is_tracked(&t));
    if can_reuse_tensor_in_place(&t, autograd.as_deref(), t.dtype, &t.shape, &t.device) {
        reuse_elementwise(&t, unary_op_to_fused_step("neg"), fbip_stats, |x| Ok(-x))?;
        let keep_grad = t.requires_grad;
        prepare_reused_tensor_meta(&mut t, keep_grad);
        return Ok(Value::Tensor(t));
    }
    let out = t.map_unary(|x| -x)?;
    if let Some(ag) = autograd
        && is_tracked
    {
        return Ok(Value::Tensor(
            ag.record_unary(&t, out, |src| TapeOp::Neg { a: src }),
        ));
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
    let guard = t.storage.read_host();
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
    autograd: Option<&mut AutogradContext>,
) -> MoraResult<Value> {
    eval_tensor_binary_fbip(op, left.clone(), right.clone(), autograd, None)
}

/// P6-1 Perceus/FBIP: Evaluate binary operator `op` consuming `left` and `right` by ownership so
/// uniquely-owned contiguous tensors reuse their physical 64B host or GPU VRAM buffer in place.
pub fn eval_tensor_binary_fbip(
    op: &BinaryOp,
    left: Value,
    right: Value,
    mut autograd: Option<&mut AutogradContext>,
    fbip_stats: Option<&crate::runtime::fbip::FbipCounters>,
) -> MoraResult<Value> {
    if matches!(op, BinaryOp::MatMul) {
        return eval_tensor_matmul_tracked(&left, &right, autograd);
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
            if a.dtype.is_bool() {
                return Err(MoraError::Runtime(format!(
                    "Arithmetic operator {:?} is not supported on Tensor<bool>",
                    op
                )));
            }
            let out_shape = broadcast_runtime_shapes(&a.shape, &b.shape)?;
            let numel = checked_numel(&out_shape)?;
            let tracked_any = autograd
                .as_ref()
                .is_some_and(|ag| ag.is_tracked(&a) || ag.is_tracked(&b));
            let req_grad = tracked_any || a.requires_grad || b.requires_grad;

            if !tracked_any {
                let reuse_a = can_reuse_tensor_in_place(
                    &a,
                    autograd.as_deref(),
                    a.dtype,
                    &out_shape,
                    &a.device,
                );
                let reuse_b = !reuse_a
                    && can_reuse_tensor_in_place(
                        &b,
                        autograd.as_deref(),
                        a.dtype,
                        &out_shape,
                        &a.device,
                    );

                let gpu_binary = |dst: Option<Arc<dyn DeviceVramBuffer>>| {
                    let (Some(va), Some(vb), Some(step)) = (
                        a.vram_buffer_if_contiguous(),
                        b.vram_buffer_if_contiguous(),
                        binary_op_to_fused_step(op, 0, 1),
                    ) else {
                        return Ok(None);
                    };
                    crate::runtime::compute_dag::backend_for_device(&a.device)
                        .run_fused_elementwise_vram(
                            &[va.as_ref(), vb.as_ref()],
                            &[&a.shape, &b.shape],
                            &[step],
                            &out_shape,
                            numel,
                            a.dtype,
                            dst,
                        )
                };
                if reuse_a || reuse_b {
                    let (dst, other) = if reuse_a { (&a, &b) } else { (&b, &a) };
                    reuse_storage(
                        dst,
                        fbip_stats,
                        |buffer| Ok(gpu_binary(Some(Arc::clone(buffer)))?.is_some()),
                        |data| {
                            for idx in 0..numel {
                                let old = data.get_f64(idx);
                                let value = read_broadcast_elem(other, &out_shape, idx);
                                let (lhs, rhs) = if reuse_a { (old, value) } else { (value, old) };
                                data.set_f64(idx, apply_scalar_op(op, lhs, rhs, dst.dtype)?);
                            }
                            Ok(())
                        },
                    )?;
                    let mut result = if reuse_a { a } else { b };
                    prepare_reused_tensor_meta(&mut result, req_grad);
                    return Ok(Value::Tensor(result));
                }
                if let Some(output) = gpu_binary(None)? {
                    return Ok(Value::Tensor(Tensor::from_vram_output(
                        output,
                        out_shape,
                        a.dtype,
                        a.device.clone(),
                        req_grad,
                    )?));
                }
            }

            let mut out = Vec::with_capacity(numel);
            for idx in 0..numel {
                let va = read_broadcast_elem(&a, &out_shape, idx);
                let vb = read_broadcast_elem(&b, &out_shape, idx);
                out.push(apply_scalar_op(op, va, vb, a.dtype)?);
            }
            let t = Tensor::from_raw(out.clone(), out_shape, a.dtype, a.device.clone(), req_grad)?;
            if tracked_any
                && let Some(ag) = autograd.as_deref_mut()
                && matches!(
                    op,
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow
                )
            {
                let a_vals = a.to_flat_vec();
                let b_vals = b.to_flat_vec();
                let a_shape = a.shape.clone();
                let b_shape = b.shape.clone();
                let tracked = ag.record_binary(&a, &b, t, |na, nb| match op {
                    BinaryOp::Add => TapeOp::Add {
                        a: na,
                        b: nb,
                        a_const: if na.is_none() {
                            Some(a_vals.clone())
                        } else {
                            None
                        },
                        b_const: if nb.is_none() {
                            Some(b_vals.clone())
                        } else {
                            None
                        },
                        a_shape: a_shape.clone(),
                        b_shape: b_shape.clone(),
                    },
                    BinaryOp::Sub => TapeOp::Sub {
                        a: na,
                        b: nb,
                        a_const: if na.is_none() {
                            Some(a_vals.clone())
                        } else {
                            None
                        },
                        b_const: if nb.is_none() {
                            Some(b_vals.clone())
                        } else {
                            None
                        },
                        a_shape: a_shape.clone(),
                        b_shape: b_shape.clone(),
                    },
                    BinaryOp::Mul => TapeOp::Mul {
                        a: na,
                        b: nb,
                        a_vals: a_vals.clone(),
                        b_vals: b_vals.clone(),
                        a_shape: a_shape.clone(),
                        b_shape: b_shape.clone(),
                    },
                    BinaryOp::Div => TapeOp::Div {
                        a: na,
                        b: nb,
                        a_vals: a_vals.clone(),
                        b_vals: b_vals.clone(),
                        a_shape: a_shape.clone(),
                        b_shape: b_shape.clone(),
                    },
                    BinaryOp::Pow => TapeOp::Pow {
                        a: na,
                        b: nb,
                        a_vals: a_vals.clone(),
                        b_vals: b_vals.clone(),
                        out_vals: out.clone(),
                        a_shape: a_shape.clone(),
                        b_shape: b_shape.clone(),
                    },
                    _ => unreachable!(),
                });
                return Ok(Value::Tensor(tracked));
            }
            Ok(Value::Tensor(t))
        }
        (Value::Tensor(mut t), scalar) => {
            let Some((s_dt, s_val)) = scalar_val_dtype_and_f64(&scalar) else {
                return Err(MoraError::Runtime(format!(
                    "Unsupported tensor binary operands: tensor {:?} {}",
                    op,
                    crate::flow::type_name(&scalar)
                )));
            };
            if t.dtype != s_dt {
                return Err(MoraError::Runtime(format!(
                    "DType mismatch: cannot combine Tensor<{}> with scalar '{}'",
                    t.dtype, s_dt
                )));
            }
            if t.dtype.is_bool() {
                return Err(MoraError::Runtime(format!(
                    "Arithmetic operator {:?} is not supported on Tensor<bool>",
                    op
                )));
            }
            let tracked_t = autograd.as_ref().is_some_and(|ag| ag.is_tracked(&t));
            if can_reuse_tensor_in_place(&t, autograd.as_deref(), t.dtype, &t.shape, &t.device) {
                reuse_elementwise(
                    &t,
                    binary_scalar_op_to_fused_step(op, s_val, false),
                    fbip_stats,
                    |v| apply_scalar_op(op, v, s_val, t.dtype),
                )?;
                let keep_grad = t.requires_grad;
                prepare_reused_tensor_meta(&mut t, keep_grad);
                return Ok(Value::Tensor(t));
            }
            let flat = t.to_flat_vec();
            let mut out = Vec::with_capacity(flat.len());
            for &v in &flat {
                out.push(apply_scalar_op(op, v, s_val, t.dtype)?);
            }
            let res = Tensor::from_raw(
                out.clone(),
                t.shape.clone(),
                t.dtype,
                t.device.clone(),
                tracked_t || t.requires_grad,
            )?;
            if tracked_t
                && let Some(ag) = autograd.as_deref_mut()
                && matches!(
                    op,
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow
                )
            {
                let tracked = ag.record_unary(&t, res, |ia| match op {
                    BinaryOp::Add => TapeOp::AddScalar {
                        a: ia,
                        scalar: s_val,
                    },
                    BinaryOp::Sub => TapeOp::SubScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: false,
                    },
                    BinaryOp::Mul => TapeOp::MulScalar {
                        a: ia,
                        scalar: s_val,
                    },
                    BinaryOp::Div => TapeOp::DivScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: false,
                        a_vals: flat.clone(),
                    },
                    BinaryOp::Pow => TapeOp::PowScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: false,
                        a_vals: flat.clone(),
                        out_vals: out.clone(),
                    },
                    _ => unreachable!(),
                });
                return Ok(Value::Tensor(tracked));
            }
            Ok(Value::Tensor(res))
        }
        (scalar, Value::Tensor(mut t)) => {
            let Some((s_dt, s_val)) = scalar_val_dtype_and_f64(&scalar) else {
                return Err(MoraError::Runtime(format!(
                    "Unsupported tensor binary operands: {} {:?} tensor",
                    crate::flow::type_name(&scalar),
                    op
                )));
            };
            if t.dtype != s_dt {
                return Err(MoraError::Runtime(format!(
                    "DType mismatch: cannot combine scalar '{}' with Tensor<{}>",
                    s_dt, t.dtype
                )));
            }
            if t.dtype.is_bool() {
                return Err(MoraError::Runtime(format!(
                    "Arithmetic operator {:?} is not supported on Tensor<bool>",
                    op
                )));
            }
            let tracked_t = autograd.as_ref().is_some_and(|ag| ag.is_tracked(&t));
            if can_reuse_tensor_in_place(&t, autograd.as_deref(), t.dtype, &t.shape, &t.device) {
                reuse_elementwise(
                    &t,
                    binary_scalar_op_to_fused_step(op, s_val, true),
                    fbip_stats,
                    |v| apply_scalar_op(op, s_val, v, t.dtype),
                )?;
                let keep_grad = t.requires_grad;
                prepare_reused_tensor_meta(&mut t, keep_grad);
                return Ok(Value::Tensor(t));
            }
            let flat = t.to_flat_vec();
            let mut out = Vec::with_capacity(flat.len());
            for &v in &flat {
                out.push(apply_scalar_op(op, s_val, v, t.dtype)?);
            }
            let res = Tensor::from_raw(
                out.clone(),
                t.shape.clone(),
                t.dtype,
                t.device.clone(),
                tracked_t || t.requires_grad,
            )?;
            if tracked_t
                && let Some(ag) = autograd
                && matches!(
                    op,
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow
                )
            {
                let tracked = ag.record_unary(&t, res, |ia| match op {
                    BinaryOp::Add => TapeOp::AddScalar {
                        a: ia,
                        scalar: s_val,
                    },
                    BinaryOp::Sub => TapeOp::SubScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: true,
                    },
                    BinaryOp::Mul => TapeOp::MulScalar {
                        a: ia,
                        scalar: s_val,
                    },
                    BinaryOp::Div => TapeOp::DivScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: true,
                        a_vals: flat.clone(),
                    },
                    BinaryOp::Pow => TapeOp::PowScalar {
                        a: ia,
                        scalar: s_val,
                        reversed: true,
                        a_vals: flat.clone(),
                        out_vals: out.clone(),
                    },
                    _ => unreachable!(),
                });
                return Ok(Value::Tensor(tracked));
            }
            Ok(Value::Tensor(res))
        }
        (l, r) => Err(MoraError::Runtime(format!(
            "Unsupported tensor binary operands: {} {:?} {}",
            crate::flow::type_name(&l),
            op,
            crate::flow::type_name(&r)
        ))),
    }
}

/// Evaluate matrix/tensor contraction (`@`) for 1D, 2D, and batched ND [`Tensor`]s.
pub fn eval_tensor_matmul(left: &Value, right: &Value) -> MoraResult<Value> {
    eval_tensor_matmul_tracked(left, right, None)
}

/// Evaluate matrix/tensor contraction (`@`) with optional multi-tape autograd tape recording.
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
    let tracked = autograd
        .as_ref()
        .is_some_and(|ag| ag.is_tracked(a) || ag.is_tracked(b));
    let req_grad = tracked || a.requires_grad || b.requires_grad;

    let attach = |out_data: Vec<f64>,
                  out_shape: Vec<usize>,
                  fa: Vec<f64>,
                  fb: Vec<f64>,
                  ag_opt: Option<&mut AutogradContext>|
     -> MoraResult<Value> {
        let t = Tensor::from_raw(out_data, out_shape, dt, dev.clone(), req_grad)?;
        if tracked && let Some(ag) = ag_opt {
            let a_shape = a.shape.clone();
            let b_shape = b.shape.clone();
            let tracked_t = ag.record_binary(a, b, t, |na, nb| TapeOp::MatMul {
                a: na,
                b: nb,
                a_vals: fa.clone(),
                b_vals: fb.clone(),
                a_shape: a_shape.clone(),
                b_shape: b_shape.clone(),
            });
            return Ok(Value::Tensor(tracked_t));
        }
        Ok(Value::Tensor(t))
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
