//! Wave 4 P1: Dynamic VRAM / KV-Cache Resource Orchestration (`KVPageAllocator`).
//!
//! Architecture (`AGENTS.md §0`):
//! - PagedAttention-style virtual-to-physical page table management backed by 64-byte aligned
//!   hardware-packed [`AlignedByteBuffer`] K/V page pools honoring exact `DType` widths.
//! - Copy-on-Write prefix sharing (`fork_seq` + CoW page split on `write_kv`).
//! - Hard byte quota enforcement (`max_bytes` / `set_quota` / `quota_bytes`) checked atomically
//!   before physical page commit so failed allocations never leak pages.
//! - Sequence priority & preemptive eviction (`alloc_seq(id, priority?)`, `set_priority`,
//!   `seq_priority`, `evict_preempt(min_priority?)`, `reclaim_for(pages_needed, requester_priority)`),
//!   plus pure LRU (`evict_lru`).
//! - Rising-edge watermark callbacks (`on_watermark(threshold, cb)` / `clear_watermarks`):
//!   callbacks are returned as `(callee, event_dict)` AFTER releasing the pool lock and executed
//!   iteratively on the stackful VM frame stack (`CombinatorFrame::WatermarkCallbacks`) so
//!   callbacks can freely call `evict_preempt` / `free_seq` / `channel.send` with zero deadlock.

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::value::Value;
use crate::value::tensor::{AlignedByteBuffer, Tensor};

/// Triggered watermark callback invocation `(mora_callable, event_payload_dict)`.
pub(crate) type TriggeredWatermark = (Value, HashMap<String, Value>);

#[derive(Debug, Clone)]
struct WatermarkHook {
    threshold: f64,
    callback: Value,
    triggered: bool,
}

#[derive(Debug, Clone)]
struct SeqKvState {
    page_table: Vec<usize>,
    num_tokens: usize,
    priority: i64,
    last_access_tick: u64,
}

#[derive(Debug)]
struct KvPoolInner {
    free_pages: Vec<usize>,
    refcounts: Vec<usize>,
    k_pages: Vec<AlignedByteBuffer>,
    v_pages: Vec<AlignedByteBuffer>,
    seqs: HashMap<u64, SeqKvState>,
    tick: u64,
    max_bytes: Option<usize>,
    watermarks: Vec<WatermarkHook>,
    evictions: u64,
    preemptions: u64,
    watermark_triggers: u64,
}

/// Paged KV-cache memory allocator with Copy-on-Write prefix sharing, hard byte quotas,
/// priority-aware preemptive eviction, and rising-edge watermark callbacks.
#[derive(Debug)]
pub struct KVPageAllocator {
    pub total_pages: usize,
    pub page_tokens: usize,
    pub num_heads: usize,
    pub head_dim: usize,
    pub dtype: DType,
    pub device: Device,
    elems_per_page: usize,
    elems_per_token: usize,
    bytes_per_page: usize,
    inner: Mutex<KvPoolInner>,
}

impl KVPageAllocator {
    /// Create a new paged KV allocator managing `total_pages` physical blocks with an optional
    /// hard byte quota `max_bytes`.
    pub fn new(
        total_pages: usize,
        page_tokens: usize,
        num_heads: usize,
        head_dim: usize,
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Self>> {
        Self::new_with_quota(
            total_pages,
            page_tokens,
            num_heads,
            head_dim,
            dtype,
            device,
            None,
        )
    }

    /// Create a new paged KV allocator with optional hard byte quota `max_bytes`.
    pub(crate) fn new_with_quota(
        total_pages: usize,
        page_tokens: usize,
        num_heads: usize,
        head_dim: usize,
        dtype: DType,
        device: Device,
        max_bytes: Option<usize>,
    ) -> MoraResult<Arc<Self>> {
        if total_pages == 0 || page_tokens == 0 || num_heads == 0 || head_dim == 0 {
            return Err(MoraError::Runtime(
                "kv_allocator: total_pages, page_tokens, num_heads, and head_dim must all be > 0"
                    .to_string(),
            ));
        }
        if let Some(q) = max_bytes
            && q == 0
        {
            return Err(MoraError::Runtime(
                "kv_allocator: max_bytes quota must be > 0".to_string(),
            ));
        }
        let elems_per_token = num_heads
            .checked_mul(head_dim)
            .ok_or_else(|| MoraError::Runtime("kv_allocator: token stride overflow".to_string()))?;
        let elems_per_page = page_tokens
            .checked_mul(elems_per_token)
            .ok_or_else(|| MoraError::Runtime("kv_allocator: page size overflow".to_string()))?;

        let free_pages: Vec<usize> = (0..total_pages).rev().collect();
        let refcounts = vec![0usize; total_pages];
        let mut k_pages = Vec::with_capacity(total_pages);
        let mut v_pages = Vec::with_capacity(total_pages);
        for _ in 0..total_pages {
            k_pages.push(AlignedByteBuffer::zeroed(elems_per_page, dtype));
            v_pages.push(AlignedByteBuffer::zeroed(elems_per_page, dtype));
        }
        let bytes_per_page = k_pages[0].byte_len() + v_pages[0].byte_len();

        Ok(Arc::new(Self {
            total_pages,
            page_tokens,
            num_heads,
            head_dim,
            dtype,
            device,
            elems_per_page,
            elems_per_token,
            bytes_per_page,
            inner: Mutex::new(KvPoolInner {
                free_pages,
                refcounts,
                k_pages,
                v_pages,
                seqs: HashMap::new(),
                tick: 0,
                max_bytes,
                watermarks: Vec::new(),
                evictions: 0,
                preemptions: 0,
                watermark_triggers: 0,
            }),
        }))
    }

    fn next_tick(inner: &mut KvPoolInner) -> u64 {
        inner.tick += 1;
        inner.tick
    }

    #[inline]
    fn used_pages_inner(&self, inner: &KvPoolInner) -> usize {
        self.total_pages - inner.free_pages.len()
    }

    #[inline]
    fn bytes_used_inner(&self, inner: &KvPoolInner) -> usize {
        self.used_pages_inner(inner) * self.bytes_per_page
    }

    #[inline]
    fn effective_quota_bytes_inner(&self, inner: &KvPoolInner) -> usize {
        let physical_cap = self.total_pages * self.bytes_per_page;
        match inner.max_bytes {
            Some(q) => q.min(physical_cap),
            None => physical_cap,
        }
    }

    fn ensure_can_allocate(
        &self,
        inner: &KvPoolInner,
        extra_pages: usize,
        seq_id: u64,
    ) -> MoraResult<()> {
        if extra_pages == 0 {
            return Ok(());
        }
        if extra_pages > inner.free_pages.len() {
            return Err(MoraError::Runtime(format!(
                "KVPageAllocator out of memory: sequence {seq_id} needs {extra_pages} page(s), only {} free",
                inner.free_pages.len()
            )));
        }
        if let Some(quota) = inner.max_bytes {
            let projected_bytes =
                (self.used_pages_inner(inner) + extra_pages) * self.bytes_per_page;
            if projected_bytes > quota {
                return Err(MoraError::Runtime(format!(
                    "KVPageAllocator quota exceeded: sequence {seq_id} needs {projected_bytes} bytes, exceeds hard quota {quota} bytes"
                )));
            }
        }
        Ok(())
    }

    fn alloc_physical_page(inner: &mut KvPoolInner) -> MoraResult<usize> {
        let pid = inner.free_pages.pop().ok_or_else(|| {
            MoraError::Runtime(format!(
                "KVPageAllocator out of memory: all {} physical pages are in use",
                inner.refcounts.len()
            ))
        })?;
        inner.refcounts[pid] = 1;
        Ok(pid)
    }

    fn release_physical_page(inner: &mut KvPoolInner, pid: usize) -> bool {
        if inner.refcounts[pid] == 0 {
            return false;
        }
        inner.refcounts[pid] -= 1;
        if inner.refcounts[pid] == 0 {
            inner.free_pages.push(pid);
            true
        } else {
            false
        }
    }

    /// Update watermark hysteresis and collect any rising-edge callbacks triggered by the current utilization.
    fn collect_triggered_watermarks(&self, inner: &mut KvPoolInner) -> Vec<TriggeredWatermark> {
        let used = self.used_pages_inner(inner);
        let free = inner.free_pages.len();
        let bytes_used = used * self.bytes_per_page;
        let quota_bytes = self.effective_quota_bytes_inner(inner);
        let util = if quota_bytes > 0 {
            (bytes_used as f64) / (quota_bytes as f64)
        } else {
            (used as f64) / (self.total_pages as f64)
        };

        let mut fired = Vec::new();
        for wm in &mut inner.watermarks {
            if util >= wm.threshold {
                if !wm.triggered {
                    wm.triggered = true;
                    inner.watermark_triggers += 1;
                    let mut event = HashMap::new();
                    event.insert("utilization".to_string(), Value::Float(util));
                    event.insert("threshold".to_string(), Value::Float(wm.threshold));
                    event.insert("used_pages".to_string(), Value::Int(used as i64));
                    event.insert("free_pages".to_string(), Value::Int(free as i64));
                    event.insert(
                        "total_pages".to_string(),
                        Value::Int(self.total_pages as i64),
                    );
                    event.insert("bytes_used".to_string(), Value::Int(bytes_used as i64));
                    event.insert("quota_bytes".to_string(), Value::Int(quota_bytes as i64));
                    fired.push((wm.callback.clone(), event));
                }
            } else {
                // Reset hysteresis once utilization drops back below threshold.
                wm.triggered = false;
            }
        }
        fired
    }

    /// Reset any triggered watermarks whose utilization has dropped below their threshold.
    fn refresh_watermark_hysteresis(&self, inner: &mut KvPoolInner) {
        let bytes_used = self.bytes_used_inner(inner);
        let quota_bytes = self.effective_quota_bytes_inner(inner);
        let util = if quota_bytes > 0 {
            (bytes_used as f64) / (quota_bytes as f64)
        } else {
            0.0
        };
        for wm in &mut inner.watermarks {
            if util < wm.threshold {
                wm.triggered = false;
            }
        }
    }

    /// Register a new sequence `seq_id` with default priority `0`.
    pub fn alloc_seq(&self, seq_id: u64) -> MoraResult<bool> {
        self.alloc_seq_with_priority(seq_id, 0)
    }

    /// Register a new sequence `seq_id` with explicit scheduling `priority` (higher = more protected).
    pub(crate) fn alloc_seq_with_priority(&self, seq_id: u64, priority: i64) -> MoraResult<bool> {
        let mut inner = self.inner.lock();
        if inner.seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator: sequence {seq_id} already allocated"
            )));
        }
        let tick = Self::next_tick(&mut inner);
        inner.seqs.insert(
            seq_id,
            SeqKvState {
                page_table: Vec::new(),
                num_tokens: 0,
                priority,
                last_access_tick: tick,
            },
        );
        Ok(true)
    }

    /// Update the scheduling priority of an active sequence `seq_id`.
    pub(crate) fn set_priority(&self, seq_id: u64, priority: i64) -> MoraResult<()> {
        let mut inner = self.inner.lock();
        let seq = inner.seqs.get_mut(&seq_id).ok_or_else(|| {
            MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
        })?;
        seq.priority = priority;
        Ok(())
    }

    /// Query the scheduling priority of an active sequence `seq_id`.
    pub(crate) fn seq_priority(&self, seq_id: u64) -> MoraResult<i64> {
        let inner = self.inner.lock();
        inner
            .seqs
            .get(&seq_id)
            .map(|s| s.priority)
            .ok_or_else(|| MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}")))
    }

    /// Configure or update the hard byte quota `max_bytes`.
    pub(crate) fn set_quota(&self, max_bytes: usize) -> MoraResult<()> {
        if max_bytes == 0 {
            return Err(MoraError::Runtime(
                "kv_allocator.set_quota: max_bytes must be > 0".to_string(),
            ));
        }
        let mut inner = self.inner.lock();
        inner.max_bytes = Some(max_bytes);
        self.refresh_watermark_hysteresis(&mut inner);
        Ok(())
    }

    /// Effective hard byte cap (`min(max_bytes, physical_capacity)` when configured).
    pub(crate) fn quota_bytes(&self) -> usize {
        let inner = self.inner.lock();
        self.effective_quota_bytes_inner(&inner)
    }

    /// Register a rising-edge watermark callback triggered whenever utilization crosses `threshold` (`0.0..=1.0`).
    pub(crate) fn register_watermark(&self, threshold: f64, callback: Value) -> MoraResult<()> {
        if !threshold.is_finite() || threshold <= 0.0 || threshold > 1.0 {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.on_watermark: threshold must be in (0.0, 1.0], got {threshold}"
            )));
        }
        let mut inner = self.inner.lock();
        let bytes_used = self.bytes_used_inner(&inner);
        let quota_bytes = self.effective_quota_bytes_inner(&inner);
        let util = if quota_bytes > 0 {
            (bytes_used as f64) / (quota_bytes as f64)
        } else {
            0.0
        };
        inner.watermarks.push(WatermarkHook {
            threshold,
            callback,
            triggered: util >= threshold,
        });
        Ok(())
    }

    /// Remove all registered watermark callbacks.
    pub(crate) fn clear_watermarks(&self) {
        self.inner.lock().watermarks.clear();
    }

    /// Reserve capacity for `count` additional tokens in sequence `seq_id`.
    pub fn append_tokens(&self, seq_id: u64, count: usize) -> MoraResult<Vec<usize>> {
        self.append_tokens_with_watermarks(seq_id, count)
            .map(|(pages, _)| pages)
    }

    /// Reserve capacity for `count` additional tokens, returning `(page_table, triggered_watermarks)`.
    pub(crate) fn append_tokens_with_watermarks(
        &self,
        seq_id: u64,
        count: usize,
    ) -> MoraResult<(Vec<usize>, Vec<TriggeredWatermark>)> {
        let mut inner = self.inner.lock();
        let tick = Self::next_tick(&mut inner);
        let cur_tokens = inner
            .seqs
            .get(&seq_id)
            .ok_or_else(|| MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}")))?
            .num_tokens;
        let cur_pages = inner.seqs[&seq_id].page_table.len();
        let new_tokens = cur_tokens
            .checked_add(count)
            .ok_or_else(|| MoraError::Runtime("kv_allocator: token count overflow".to_string()))?;
        let needed_pages = if new_tokens == 0 {
            0
        } else {
            new_tokens.div_ceil(self.page_tokens)
        };
        let extra = needed_pages.saturating_sub(cur_pages);
        self.ensure_can_allocate(&inner, extra, seq_id)?;

        let mut newly = Vec::with_capacity(extra);
        for _ in 0..extra {
            newly.push(Self::alloc_physical_page(&mut inner)?);
        }
        let page_table = {
            let seq = inner.seqs.get_mut(&seq_id).expect("checked");
            seq.page_table.extend(newly);
            seq.num_tokens = new_tokens;
            seq.last_access_tick = tick;
            seq.page_table.clone()
        };
        let fired = self.collect_triggered_watermarks(&mut inner);
        Ok((page_table, fired))
    }

    /// Fork `src_seq_id` into `dst_seq_id`, inheriting priority and sharing physical pages via CoW.
    pub fn fork_seq(&self, src_seq_id: u64, dst_seq_id: u64) -> MoraResult<Vec<usize>> {
        let mut inner = self.inner.lock();
        if inner.seqs.contains_key(&dst_seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator: destination sequence {dst_seq_id} already allocated"
            )));
        }
        let tick = Self::next_tick(&mut inner);
        let src = inner.seqs.get(&src_seq_id).cloned().ok_or_else(|| {
            MoraError::Runtime(format!(
                "kv_allocator: unknown source sequence {src_seq_id}"
            ))
        })?;
        for &pid in &src.page_table {
            inner.refcounts[pid] += 1;
        }
        let table = src.page_table.clone();
        inner.seqs.insert(
            dst_seq_id,
            SeqKvState {
                page_table: src.page_table,
                num_tokens: src.num_tokens,
                priority: src.priority,
                last_access_tick: tick,
            },
        );
        if let Some(src_mut) = inner.seqs.get_mut(&src_seq_id) {
            src_mut.last_access_tick = tick;
        }
        Ok(table)
    }

    /// Write `[num_heads, head_dim]` K/V tensors at `token_pos`, splitting shared CoW pages if needed.
    pub fn write_kv(
        &self,
        seq_id: u64,
        token_pos: usize,
        k: &Tensor,
        v: &Tensor,
    ) -> MoraResult<()> {
        self.write_kv_with_watermarks(seq_id, token_pos, k, v)
            .map(|_| ())
    }

    /// Write K/V tensors with CoW page splitting, returning any rising-edge watermark callbacks.
    pub(crate) fn write_kv_with_watermarks(
        &self,
        seq_id: u64,
        token_pos: usize,
        k: &Tensor,
        v: &Tensor,
    ) -> MoraResult<Vec<TriggeredWatermark>> {
        let expected_shape = [self.num_heads, self.head_dim];
        if k.shape != expected_shape || v.shape != expected_shape {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.write_kv: expected K/V shape {:?}, got {:?} and {:?}",
                expected_shape, k.shape, v.shape
            )));
        }
        if k.dtype != self.dtype || v.dtype != self.dtype {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.write_kv: expected dtype {}, got {} and {}",
                self.dtype.short_name(),
                k.dtype.short_name(),
                v.dtype.short_name()
            )));
        }
        if k.device != self.device || v.device != self.device {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.write_kv: expected device {}, got {} and {}",
                self.device, k.device, v.device
            )));
        }

        let k_vals = k.to_flat_vec();
        let v_vals = v.to_flat_vec();

        let mut inner = self.inner.lock();
        let tick = Self::next_tick(&mut inner);
        let logical_page = token_pos / self.page_tokens;
        let slot_in_page = token_pos % self.page_tokens;

        let (num_tokens, cur_pid) = {
            let seq = inner.seqs.get(&seq_id).ok_or_else(|| {
                MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
            })?;
            (
                seq.num_tokens,
                *seq.page_table.get(logical_page).ok_or_else(|| {
                    MoraError::Runtime(format!(
                        "kv_allocator.write_kv: token_pos {token_pos} out of bounds for sequence {seq_id} (tokens={})",
                        seq.num_tokens
                    ))
                })?,
            )
        };
        if token_pos >= num_tokens {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.write_kv: token_pos {token_pos} >= seq_tokens {num_tokens}"
            )));
        }

        let target_pid = if inner.refcounts[cur_pid] > 1 {
            self.ensure_can_allocate(&inner, 1, seq_id)?;
            let new_pid = Self::alloc_physical_page(&mut inner)?;
            let cloned_k = inner.k_pages[cur_pid].clone();
            let cloned_v = inner.v_pages[cur_pid].clone();
            inner.k_pages[new_pid] = cloned_k;
            inner.v_pages[new_pid] = cloned_v;
            let _ = Self::release_physical_page(&mut inner, cur_pid);
            let seq = inner.seqs.get_mut(&seq_id).expect("checked");
            seq.page_table[logical_page] = new_pid;
            new_pid
        } else {
            cur_pid
        };

        let elem_offset = slot_in_page * self.elems_per_token;
        for (i, (&kv, &vv)) in k_vals.iter().zip(v_vals.iter()).enumerate() {
            inner.k_pages[target_pid].set_f64(elem_offset + i, kv);
            inner.v_pages[target_pid].set_f64(elem_offset + i, vv);
        }
        if let Some(seq) = inner.seqs.get_mut(&seq_id) {
            seq.last_access_tick = tick;
        }
        let fired = self.collect_triggered_watermarks(&mut inner);
        Ok(fired)
    }

    /// Materialize `[K, V]` tensors of shape `[num_tokens, num_heads, head_dim]` for `seq_id`.
    pub fn read_kv(&self, seq_id: u64) -> MoraResult<(Arc<Tensor>, Arc<Tensor>)> {
        let mut inner = self.inner.lock();
        let tick = Self::next_tick(&mut inner);
        let seq = inner.seqs.get_mut(&seq_id).ok_or_else(|| {
            MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
        })?;
        seq.last_access_tick = tick;
        let num_tokens = seq.num_tokens;
        let page_table = seq.page_table.clone();

        let total_elems = num_tokens * self.elems_per_token;
        let mut k_out = Vec::with_capacity(total_elems);
        let mut v_out = Vec::with_capacity(total_elems);
        for t_idx in 0..num_tokens {
            let pid = page_table[t_idx / self.page_tokens];
            let offset = (t_idx % self.page_tokens) * self.elems_per_token;
            for i in 0..self.elems_per_token {
                k_out.push(inner.k_pages[pid].get_f64(offset + i));
                v_out.push(inner.v_pages[pid].get_f64(offset + i));
            }
        }
        let shape = vec![num_tokens, self.num_heads, self.head_dim];
        let kt = Tensor::from_raw(k_out, shape.clone(), self.dtype, self.device.clone(), false)?;
        let vt = Tensor::from_raw(v_out, shape, self.dtype, self.device.clone(), false)?;
        Ok((kt, vt))
    }

    /// Free sequence `seq_id`, returning the number of physical pages reclaimed to the free pool.
    pub fn free_seq(&self, seq_id: u64) -> MoraResult<usize> {
        let mut inner = self.inner.lock();
        let seq = inner.seqs.remove(&seq_id).ok_or_else(|| {
            MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
        })?;
        let mut freed = 0usize;
        for pid in seq.page_table {
            if Self::release_physical_page(&mut inner, pid) {
                freed += 1;
            }
        }
        self.refresh_watermark_hysteresis(&mut inner);
        Ok(freed)
    }

    /// Evict the least-recently-used sequence, returning its `seq_id` (or `-1` if no sequences active).
    pub fn evict_lru(&self) -> i64 {
        let mut inner = self.inner.lock();
        let Some((&lru_id, _)) = inner.seqs.iter().min_by_key(|(_, st)| st.last_access_tick) else {
            return -1;
        };
        let seq = inner.seqs.remove(&lru_id).expect("lru exists");
        for pid in seq.page_table {
            let _ = Self::release_physical_page(&mut inner, pid);
        }
        inner.evictions += 1;
        self.refresh_watermark_hysteresis(&mut inner);
        lru_id as i64
    }

    /// Priority-aware preemptive eviction: evicts the lowest-priority sequence (breaking ties by
    /// oldest `last_access_tick` = LRU), skipping sequences with `priority >= max_victim_priority`
    /// when `max_victim_priority` is provided. Returns evicted `seq_id` or `-1` if none eligible.
    pub(crate) fn evict_preempt(&self, max_victim_priority: Option<i64>) -> i64 {
        let mut inner = self.inner.lock();
        let victim = inner
            .seqs
            .iter()
            .filter(|(_, st)| match max_victim_priority {
                Some(limit) => st.priority < limit,
                None => true,
            })
            .min_by_key(|(_, st)| (st.priority, st.last_access_tick))
            .map(|(&id, _)| id);

        let Some(victim_id) = victim else {
            return -1;
        };
        let seq = inner.seqs.remove(&victim_id).expect("victim exists");
        for pid in seq.page_table {
            let _ = Self::release_physical_page(&mut inner, pid);
        }
        inner.evictions += 1;
        inner.preemptions += 1;
        self.refresh_watermark_hysteresis(&mut inner);
        victim_id as i64
    }

    /// Preempt lowest-priority sequences (strictly `< requester_priority`, LRU tie-break) until
    /// `pages_needed` physical pages can be allocated within both pool capacity and hard byte quota.
    /// Returns the list of preempted `seq_id`s.
    pub(crate) fn reclaim_for(&self, pages_needed: usize, requester_priority: i64) -> Vec<u64> {
        let mut inner = self.inner.lock();
        let mut evicted = Vec::new();
        while self.ensure_can_allocate(&inner, pages_needed, 0).is_err() {
            let victim = inner
                .seqs
                .iter()
                .filter(|(_, st)| st.priority < requester_priority)
                .min_by_key(|(_, st)| (st.priority, st.last_access_tick))
                .map(|(&id, _)| id);
            let Some(vid) = victim else {
                break;
            };
            let seq = inner.seqs.remove(&vid).expect("victim exists");
            for pid in seq.page_table {
                let _ = Self::release_physical_page(&mut inner, pid);
            }
            inner.evictions += 1;
            inner.preemptions += 1;
            evicted.push(vid);
        }
        self.refresh_watermark_hysteresis(&mut inner);
        evicted
    }

    pub fn seq_pages(&self, seq_id: u64) -> MoraResult<Vec<usize>> {
        let inner = self.inner.lock();
        inner
            .seqs
            .get(&seq_id)
            .map(|s| s.page_table.clone())
            .ok_or_else(|| MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}")))
    }

    pub fn seq_tokens(&self, seq_id: u64) -> MoraResult<usize> {
        let inner = self.inner.lock();
        inner
            .seqs
            .get(&seq_id)
            .map(|s| s.num_tokens)
            .ok_or_else(|| MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}")))
    }

    pub fn free_pages(&self) -> usize {
        self.inner.lock().free_pages.len()
    }

    pub fn used_pages(&self) -> usize {
        self.total_pages - self.free_pages()
    }

    pub fn utilization(&self) -> f64 {
        let inner = self.inner.lock();
        let bytes_used = self.bytes_used_inner(&inner);
        let quota = self.effective_quota_bytes_inner(&inner);
        if quota > 0 {
            (bytes_used as f64) / (quota as f64)
        } else {
            (self.used_pages_inner(&inner) as f64) / (self.total_pages as f64)
        }
    }

    pub fn bytes_used(&self) -> usize {
        let inner = self.inner.lock();
        self.bytes_used_inner(&inner)
    }

    pub fn stats_dict(&self) -> HashMap<String, Value> {
        let inner = self.inner.lock();
        let free = inner.free_pages.len();
        let used = self.total_pages - free;
        let cow_shared = inner.refcounts.iter().filter(|&&rc| rc > 1).count();
        let bytes = self.bytes_used_inner(&inner);
        let quota_bytes = self.effective_quota_bytes_inner(&inner);
        let mut m = HashMap::new();
        m.insert(
            "total_pages".to_string(),
            Value::Int(self.total_pages as i64),
        );
        m.insert("used_pages".to_string(), Value::Int(used as i64));
        m.insert("free_pages".to_string(), Value::Int(free as i64));
        m.insert(
            "active_seqs".to_string(),
            Value::Int(inner.seqs.len() as i64),
        );
        m.insert(
            "cow_shared_pages".to_string(),
            Value::Int(cow_shared as i64),
        );
        m.insert("bytes_used".to_string(), Value::Int(bytes as i64));
        m.insert("quota_bytes".to_string(), Value::Int(quota_bytes as i64));
        m.insert(
            "page_tokens".to_string(),
            Value::Int(self.page_tokens as i64),
        );
        m.insert(
            "elems_per_page".to_string(),
            Value::Int(self.elems_per_page as i64),
        );
        m.insert("evictions".to_string(), Value::Int(inner.evictions as i64));
        m.insert(
            "preemptions".to_string(),
            Value::Int(inner.preemptions as i64),
        );
        m.insert(
            "watermark_triggers".to_string(),
            Value::Int(inner.watermark_triggers as i64),
        );
        m
    }
}
