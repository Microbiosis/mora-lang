//! Wave 4 P1: General 64-Byte Aligned Size-Classed Tensor [`MemoryPool`].
//!
//! Architecture (`AGENTS.md §0`):
//! - Power-of-two 64-byte cache-line aligned slab buckets recycling physical [`AlignedByteBuffer`]s
//!   with zero OS heap churn on reuse (`pool_hits` / `pool_misses`).
//! - Hard byte quota enforcement (`max_bytes` / `set_quota`): automatically trims idle cached slabs
//!   under memory pressure before failing closed with an explicit quota-exceeded error.
//! - Rising-edge watermark callbacks (`on_watermark(threshold, cb)` / `clear_watermarks`) executed
//!   outside the pool lock on the stackful VM frame stack (`CombinatorFrame::WatermarkCallbacks`).
//! - Thread-safe via `parking_lot::Mutex` across `spawn`ed concurrent tasks.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::kv_cache::TriggeredWatermark;
use crate::value::Value;
use crate::value::tensor::{AlignedByteBuffer, Tensor, checked_numel};

#[derive(Debug, Clone)]
struct PoolWatermark {
    threshold: f64,
    callback: Value,
    triggered: bool,
}

#[derive(Debug)]
struct MemoryPoolInner {
    max_bytes: usize,
    active_bytes: usize,
    cached_bytes: usize,
    peak_active_bytes: usize,
    /// Map from live `Arc::as_ptr(&tensor.storage) as usize` -> allocated slab bucket size in bytes.
    live_allocs: HashMap<usize, usize>,
    /// Size-classed free lists: `bucket_bytes -> Vec<AlignedByteBuffer>` (all 64-byte aligned).
    free_buckets: BTreeMap<usize, Vec<AlignedByteBuffer>>,
    pool_hits: u64,
    pool_misses: u64,
    trim_evictions: u64,
    watermarks: Vec<PoolWatermark>,
    watermark_triggers: u64,
}

/// General 64-byte aligned size-classed caching tensor [`MemoryPool`].
#[derive(Debug)]
pub struct MemoryPool {
    pub device: Device,
    inner: Mutex<MemoryPoolInner>,
}

/// Round requested byte size up to a 64-byte cache-line aligned power-of-two slab bucket.
#[inline]
fn bucket_size_bytes(raw_bytes: usize) -> usize {
    let min_aligned = raw_bytes.max(64).div_ceil(64) * 64;
    min_aligned
        .checked_next_power_of_two()
        .unwrap_or(min_aligned)
}

impl MemoryPool {
    /// Construct a new general tensor [`MemoryPool`] bounded by `max_bytes` on `device`.
    pub(crate) fn new(max_bytes: usize, device: Device) -> MoraResult<Arc<Self>> {
        if max_bytes < 64 {
            return Err(MoraError::Runtime(format!(
                "memory_pool: max_bytes must be >= 64 (one 64-byte cache line), got {max_bytes}"
            )));
        }
        Ok(Arc::new(Self {
            device,
            inner: Mutex::new(MemoryPoolInner {
                max_bytes,
                active_bytes: 0,
                cached_bytes: 0,
                peak_active_bytes: 0,
                live_allocs: HashMap::new(),
                free_buckets: BTreeMap::new(),
                pool_hits: 0,
                pool_misses: 0,
                trim_evictions: 0,
                watermarks: Vec::new(),
                watermark_triggers: 0,
            }),
        }))
    }

    fn collect_triggered_watermarks(&self, inner: &mut MemoryPoolInner) -> Vec<TriggeredWatermark> {
        let util = (inner.active_bytes as f64) / (inner.max_bytes as f64);
        let mut fired = Vec::new();
        for wm in &mut inner.watermarks {
            if util >= wm.threshold {
                if !wm.triggered {
                    wm.triggered = true;
                    inner.watermark_triggers += 1;
                    let mut event = HashMap::new();
                    event.insert("utilization".to_string(), Value::Float(util));
                    event.insert("threshold".to_string(), Value::Float(wm.threshold));
                    event.insert(
                        "active_bytes".to_string(),
                        Value::Int(inner.active_bytes as i64),
                    );
                    event.insert(
                        "cached_bytes".to_string(),
                        Value::Int(inner.cached_bytes as i64),
                    );
                    event.insert("max_bytes".to_string(), Value::Int(inner.max_bytes as i64));
                    event.insert(
                        "live_tensors".to_string(),
                        Value::Int(inner.live_allocs.len() as i64),
                    );
                    fired.push((wm.callback.clone(), event));
                }
            } else {
                wm.triggered = false;
            }
        }
        fired
    }

    fn refresh_watermark_hysteresis(inner: &mut MemoryPoolInner) {
        let util = (inner.active_bytes as f64) / (inner.max_bytes as f64);
        for wm in &mut inner.watermarks {
            if util < wm.threshold {
                wm.triggered = false;
            }
        }
    }

    /// Evict cached idle slab buffers until `active_bytes + cached_bytes <= target_total_bytes`.
    fn trim_to_fit(inner: &mut MemoryPoolInner, target_total_bytes: usize) -> usize {
        let mut freed = 0usize;
        while inner.active_bytes + inner.cached_bytes > target_total_bytes {
            // Evict largest cached slab first to minimize number of evicted blocks.
            let Some((&bsize, _)) = inner
                .free_buckets
                .iter()
                .rev()
                .find(|(_, list)| !list.is_empty())
            else {
                break;
            };
            if let Some(list) = inner.free_buckets.get_mut(&bsize)
                && list.pop().is_some()
            {
                inner.cached_bytes = inner.cached_bytes.saturating_sub(bsize);
                inner.trim_evictions += 1;
                freed += bsize;
                if list.is_empty() {
                    inner.free_buckets.remove(&bsize);
                }
            }
        }
        freed
    }

    /// Allocate a 64-byte aligned tensor of `shape` and `dtype` filled with `fill_val` from this pool,
    /// reusing a cached physical slab buffer if available.
    pub(crate) fn alloc_tensor_with_watermarks(
        &self,
        shape: Vec<usize>,
        dtype: DType,
        fill_val: f64,
    ) -> MoraResult<(Arc<Tensor>, Vec<TriggeredWatermark>)> {
        let numel = checked_numel(&shape)?;
        let raw_bytes = dtype.storage_bytes(numel);
        let bucket = bucket_size_bytes(raw_bytes);

        let mut inner = self.inner.lock();
        if inner.active_bytes + bucket > inner.max_bytes {
            return Err(MoraError::Runtime(format!(
                "MemoryPool quota exceeded: allocation needs {} bytes (bucket {}B), active {}B / max {}B",
                raw_bytes, bucket, inner.active_bytes, inner.max_bytes
            )));
        }

        // Try exact bucket hit first, then smallest larger cached bucket within 2x size.
        let hit_key = if inner
            .free_buckets
            .get(&bucket)
            .is_some_and(|v| !v.is_empty())
        {
            Some(bucket)
        } else {
            inner
                .free_buckets
                .range(bucket..=bucket.saturating_mul(2))
                .find(|(_, v)| !v.is_empty())
                .map(|(&k, _)| k)
        };

        let (buf, allocated_bucket) = if let Some(k) = hit_key {
            let mut reused = inner
                .free_buckets
                .get_mut(&k)
                .and_then(|v| v.pop())
                .expect("non-empty bucket");
            if inner.free_buckets.get(&k).is_some_and(|v| v.is_empty()) {
                inner.free_buckets.remove(&k);
            }
            inner.cached_bytes = inner.cached_bytes.saturating_sub(k);
            inner.pool_hits += 1;
            if fill_val == 0.0 {
                reused.repurpose_zeroed(numel, dtype);
            } else {
                reused.repurpose_with_value(numel, fill_val, dtype);
            }
            (reused, k)
        } else {
            // Ensure active + cached + bucket <= max_bytes by trimming idle slabs first.
            let target = inner.max_bytes.saturating_sub(bucket);
            Self::trim_to_fit(&mut inner, target);
            if inner.active_bytes + bucket > inner.max_bytes {
                return Err(MoraError::Runtime(format!(
                    "MemoryPool quota exceeded: cannot fit {}B slab within {}B quota",
                    bucket, inner.max_bytes
                )));
            }
            let mut fresh = AlignedByteBuffer::with_capacity_bytes(bucket, numel, dtype);
            if fill_val != 0.0 {
                fresh.repurpose_with_value(numel, fill_val, dtype);
            }
            let cap = fresh.capacity_bytes();
            inner.pool_misses += 1;
            (fresh, cap)
        };

        let tensor = Tensor::from_aligned_buffer(buf, shape, self.device.clone(), false)?;
        let ptr_key = Arc::as_ptr(&tensor.storage) as usize;
        inner.live_allocs.insert(ptr_key, allocated_bucket);
        inner.active_bytes += allocated_bucket;
        if inner.active_bytes > inner.peak_active_bytes {
            inner.peak_active_bytes = inner.active_bytes;
        }
        let fired = self.collect_triggered_watermarks(&mut inner);
        Ok((tensor, fired))
    }

    /// Release `tensor`'s physical 64-byte aligned buffer back into this pool's slab free list.
    /// Returns the reclaimed bucket size in bytes.
    pub(crate) fn release_tensor(&self, tensor: &Arc<Tensor>) -> MoraResult<usize> {
        if tensor.is_view {
            return Err(MoraError::Runtime(
                "memory_pool.release: cannot release a strided tensor view; release the owning tensor"
                    .to_string(),
            ));
        }
        let ptr_key = Arc::as_ptr(&tensor.storage) as usize;
        let mut inner = self.inner.lock();
        let Some(bucket) = inner.live_allocs.remove(&ptr_key) else {
            return Err(MoraError::Runtime(
                "memory_pool.release: tensor does not belong to this MemoryPool or was already released"
                    .to_string(),
            ));
        };
        let reclaimed_buf = tensor.storage.detach_buffer(tensor.dtype);
        inner.active_bytes = inner.active_bytes.saturating_sub(bucket);
        if inner.active_bytes + inner.cached_bytes + bucket <= inner.max_bytes {
            inner.cached_bytes += bucket;
            inner
                .free_buckets
                .entry(bucket)
                .or_default()
                .push(reclaimed_buf);
        } else {
            inner.trim_evictions += 1;
        }
        Self::refresh_watermark_hysteresis(&mut inner);
        Ok(bucket)
    }

    /// Drain all idle cached slab buffers back to the OS, returning total bytes freed.
    pub(crate) fn trim(&self) -> usize {
        let mut inner = self.inner.lock();
        let mut freed = 0usize;
        for (bsize, list) in std::mem::take(&mut inner.free_buckets) {
            let count = list.len();
            freed += bsize * count;
            inner.trim_evictions += count as u64;
        }
        inner.cached_bytes = 0;
        freed
    }

    /// Update the hard byte quota `max_bytes`, automatically trimming idle cached slabs if needed.
    pub(crate) fn set_quota(&self, max_bytes: usize) -> MoraResult<()> {
        if max_bytes < 64 {
            return Err(MoraError::Runtime(format!(
                "memory_pool.set_quota: max_bytes must be >= 64, got {max_bytes}"
            )));
        }
        let mut inner = self.inner.lock();
        inner.max_bytes = max_bytes;
        Self::trim_to_fit(&mut inner, max_bytes);
        Self::refresh_watermark_hysteresis(&mut inner);
        Ok(())
    }

    /// Register a rising-edge watermark callback triggered when `active_bytes / max_bytes >= threshold`.
    pub(crate) fn register_watermark(&self, threshold: f64, callback: Value) -> MoraResult<()> {
        if !threshold.is_finite() || threshold <= 0.0 || threshold > 1.0 {
            return Err(MoraError::Runtime(format!(
                "memory_pool.on_watermark: threshold must be in (0.0, 1.0], got {threshold}"
            )));
        }
        let mut inner = self.inner.lock();
        let util = (inner.active_bytes as f64) / (inner.max_bytes as f64);
        inner.watermarks.push(PoolWatermark {
            threshold,
            callback,
            triggered: util >= threshold,
        });
        Ok(())
    }

    pub(crate) fn clear_watermarks(&self) {
        self.inner.lock().watermarks.clear();
    }

    pub(crate) fn active_bytes(&self) -> usize {
        self.inner.lock().active_bytes
    }

    pub(crate) fn cached_bytes(&self) -> usize {
        self.inner.lock().cached_bytes
    }

    pub(crate) fn max_bytes(&self) -> usize {
        self.inner.lock().max_bytes
    }

    pub(crate) fn utilization(&self) -> f64 {
        let inner = self.inner.lock();
        (inner.active_bytes as f64) / (inner.max_bytes as f64)
    }

    pub(crate) fn stats_dict(&self) -> HashMap<String, Value> {
        let inner = self.inner.lock();
        let cached_blocks: usize = inner.free_buckets.values().map(|v| v.len()).sum();
        let mut m = HashMap::new();
        m.insert("max_bytes".to_string(), Value::Int(inner.max_bytes as i64));
        m.insert(
            "active_bytes".to_string(),
            Value::Int(inner.active_bytes as i64),
        );
        m.insert(
            "cached_bytes".to_string(),
            Value::Int(inner.cached_bytes as i64),
        );
        m.insert(
            "peak_active_bytes".to_string(),
            Value::Int(inner.peak_active_bytes as i64),
        );
        m.insert(
            "live_tensors".to_string(),
            Value::Int(inner.live_allocs.len() as i64),
        );
        m.insert(
            "cached_blocks".to_string(),
            Value::Int(cached_blocks as i64),
        );
        m.insert("pool_hits".to_string(), Value::Int(inner.pool_hits as i64));
        m.insert(
            "pool_misses".to_string(),
            Value::Int(inner.pool_misses as i64),
        );
        m.insert(
            "trim_evictions".to_string(),
            Value::Int(inner.trim_evictions as i64),
        );
        m.insert(
            "watermark_triggers".to_string(),
            Value::Int(inner.watermark_triggers as i64),
        );
        m
    }
}
