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
use std::fmt;
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::compute_dag::{DeviceBackend, DeviceVramBuffer, backend_for_device};
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

/// G3: Host-RAM swapped sequence state downloaded from physical VRAM/pool pages.
#[derive(Debug, Clone)]
struct SwappedSeqState {
    host_pages: Vec<(Vec<u8>, Vec<u8>)>,
    num_tokens: usize,
    priority: i64,
}

/// P5-2A: Content-addressed RadixAttention prefix trie node mapping a full `page_tokens`-length
/// token block along a causal prefix path to an immutable cached physical KV page ID.
#[derive(Debug, Clone)]
struct PrefixNode {
    parent: Option<usize>,
    block_tokens: Vec<i64>,
    page_id: Option<usize>,
    children: HashMap<Vec<i64>, usize>,
    lru_tick: u64,
}

impl PrefixNode {
    fn root() -> Self {
        Self {
            parent: None,
            block_tokens: Vec::new(),
            page_id: None,
            children: HashMap::new(),
            lru_tick: 0,
        }
    }
}

#[derive(Debug)]
struct KvPoolInner {
    free_pages: Vec<usize>,
    refcounts: Vec<usize>,
    /// Number of RadixAttention prefix trie nodes currently pinning each physical page.
    /// `active_seq_refs(pid) == refcounts[pid] - radix_refcounts[pid]`.
    radix_refcounts: Vec<usize>,
    radix_nodes: Vec<Option<PrefixNode>>,
    radix_cached_pages: usize,
    k_pages: Vec<AlignedByteBuffer>,
    v_pages: Vec<AlignedByteBuffer>,
    /// Physical GPU VRAM buffers backing active allocated KV pages (1:1 with `refcounts[pid] > 0`
    /// when `KVPageAllocator.vram_backed == true`; dropped immediately when reclaimed to `free_pages`).
    k_vram_pages: Vec<Option<Arc<dyn DeviceVramBuffer>>>,
    v_vram_pages: Vec<Option<Arc<dyn DeviceVramBuffer>>>,
    seqs: HashMap<u64, SeqKvState>,
    swapped_seqs: HashMap<u64, SwappedSeqState>,
    tick: u64,
    max_bytes: Option<usize>,
    watermarks: Vec<WatermarkHook>,
    evictions: u64,
    preemptions: u64,
    watermark_triggers: u64,
    swap_outs: u64,
    swap_ins: u64,
    swap_out_bytes: u64,
    swap_in_bytes: u64,
    prefix_cache_hits: u64,
    prefix_evictions: u64,
}

/// Paged KV-cache memory allocator with Copy-on-Write prefix sharing, hard byte quotas,
/// priority-aware preemptive eviction, rising-edge watermark callbacks, and real physical GPU VRAM
/// page backing when instantiated on a live hardware GPU device.
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
    vram_backed: bool,
    backend: Arc<dyn DeviceBackend>,
    inner: Mutex<KvPoolInner>,
}

impl fmt::Debug for KVPageAllocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KVPageAllocator")
            .field("total_pages", &self.total_pages)
            .field("page_tokens", &self.page_tokens)
            .field("num_heads", &self.num_heads)
            .field("head_dim", &self.head_dim)
            .field("dtype", &self.dtype)
            .field("device", &self.device)
            .field("vram_backed", &self.vram_backed)
            .finish()
    }
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
        let backend = backend_for_device(&device);
        let vram_backed = backend.supports_vram_residency();

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
            vram_backed,
            backend,
            inner: Mutex::new(KvPoolInner {
                free_pages,
                refcounts,
                radix_refcounts: vec![0usize; total_pages],
                radix_nodes: vec![Some(PrefixNode::root())],
                radix_cached_pages: 0,
                k_pages,
                v_pages,
                k_vram_pages: vec![None; total_pages],
                v_vram_pages: vec![None; total_pages],
                seqs: HashMap::new(),
                swapped_seqs: HashMap::new(),
                tick: 0,
                max_bytes,
                watermarks: Vec::new(),
                evictions: 0,
                preemptions: 0,
                watermark_triggers: 0,
                swap_outs: 0,
                swap_ins: 0,
                swap_out_bytes: 0,
                swap_in_bytes: 0,
                prefix_cache_hits: 0,
                prefix_evictions: 0,
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

    /// P5-2A: Evict the single least-recently-used RadixAttention leaf node whose physical page
    /// has zero active sequence owners (`refcounts[pid] == radix_refcounts[pid]`).
    fn evict_one_lru_radix_leaf(inner: &mut KvPoolInner) -> bool {
        let victim = inner
            .radix_nodes
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(idx, slot)| {
                let node = slot.as_ref()?;
                if !node.children.is_empty() {
                    return None;
                }
                let pid = node.page_id?;
                if inner.refcounts[pid] == inner.radix_refcounts[pid] {
                    Some((idx, node.lru_tick))
                } else {
                    None
                }
            })
            .min_by_key(|&(_, tick)| tick)
            .map(|(idx, _)| idx);

        let Some(vidx) = victim else {
            return false;
        };
        let node = inner.radix_nodes[vidx].take().expect("radix victim exists");
        if let Some(parent_idx) = node.parent
            && let Some(parent) = inner.radix_nodes[parent_idx].as_mut()
        {
            parent.children.remove(&node.block_tokens);
        }
        if let Some(pid) = node.page_id {
            inner.radix_refcounts[pid] = inner.radix_refcounts[pid].saturating_sub(1);
            let _ = Self::release_physical_page(inner, pid);
        }
        inner.radix_cached_pages = inner.radix_cached_pages.saturating_sub(1);
        inner.prefix_evictions += 1;
        true
    }

    /// Ensure `extra_pages` can be allocated within capacity and hard `max_bytes` quota,
    /// automatically reclaiming LRU inactive RadixAttention cached prefix pages before failing.
    fn ensure_can_allocate_or_reclaim_radix(
        &self,
        inner: &mut KvPoolInner,
        extra_pages: usize,
        seq_id: u64,
    ) -> MoraResult<()> {
        while self
            .ensure_can_allocate(inner, extra_pages, seq_id)
            .is_err()
        {
            if !Self::evict_one_lru_radix_leaf(inner) {
                return self.ensure_can_allocate(inner, extra_pages, seq_id);
            }
        }
        Ok(())
    }

    fn alloc_physical_page(&self, inner: &mut KvPoolInner) -> MoraResult<usize> {
        let pid = inner.free_pages.pop().ok_or_else(|| {
            MoraError::Runtime(format!(
                "KVPageAllocator out of memory: all {} physical pages are in use",
                inner.refcounts.len()
            ))
        })?;
        if self.vram_backed {
            let k_bytes = inner.k_pages[pid].byte_len();
            let v_bytes = inner.v_pages[pid].byte_len();
            let k_vram = match self.backend.alloc_vram_buffer(k_bytes) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    inner.free_pages.push(pid);
                    return Err(MoraError::Runtime(
                        "KVPageAllocator: GPU backend did not allocate physical VRAM K page"
                            .to_string(),
                    ));
                }
                Err(e) => {
                    inner.free_pages.push(pid);
                    return Err(e);
                }
            };
            let v_vram = match self.backend.alloc_vram_buffer(v_bytes) {
                Ok(Some(b)) => b,
                Ok(None) => {
                    inner.free_pages.push(pid);
                    return Err(MoraError::Runtime(
                        "KVPageAllocator: GPU backend did not allocate physical VRAM V page"
                            .to_string(),
                    ));
                }
                Err(e) => {
                    inner.free_pages.push(pid);
                    return Err(e);
                }
            };
            inner.k_pages[pid].repurpose_zeroed(self.elems_per_page, self.dtype);
            inner.v_pages[pid].repurpose_zeroed(self.elems_per_page, self.dtype);
            let zero_k = inner.k_pages[pid].copy_bytes_out();
            let zero_v = inner.v_pages[pid].copy_bytes_out();
            if let Err(e) = k_vram
                .upload_bytes(&zero_k)
                .and_then(|()| v_vram.upload_bytes(&zero_v))
            {
                inner.free_pages.push(pid);
                return Err(e);
            }
            inner.k_vram_pages[pid] = Some(k_vram);
            inner.v_vram_pages[pid] = Some(v_vram);
        }
        inner.refcounts[pid] = 1;
        Ok(pid)
    }

    fn release_physical_page(inner: &mut KvPoolInner, pid: usize) -> bool {
        if inner.refcounts[pid] == 0 {
            return false;
        }
        inner.refcounts[pid] -= 1;
        if inner.refcounts[pid] == 0 {
            inner.k_vram_pages[pid] = None;
            inner.v_vram_pages[pid] = None;
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
        if inner.seqs.contains_key(&seq_id) || inner.swapped_seqs.contains_key(&seq_id) {
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

    /// Update the scheduling priority of an active or swapped sequence `seq_id`.
    pub(crate) fn set_priority(&self, seq_id: u64, priority: i64) -> MoraResult<()> {
        let mut inner = self.inner.lock();
        if let Some(seq) = inner.seqs.get_mut(&seq_id) {
            seq.priority = priority;
            return Ok(());
        }
        if let Some(swapped) = inner.swapped_seqs.get_mut(&seq_id) {
            swapped.priority = priority;
            return Ok(());
        }
        Err(MoraError::Runtime(format!(
            "kv_allocator: unknown sequence {seq_id}"
        )))
    }

    /// Query the scheduling priority of an active or swapped sequence `seq_id`.
    pub(crate) fn seq_priority(&self, seq_id: u64) -> MoraResult<i64> {
        let inner = self.inner.lock();
        if let Some(s) = inner.seqs.get(&seq_id) {
            return Ok(s.priority);
        }
        if let Some(s) = inner.swapped_seqs.get(&seq_id) {
            return Ok(s.priority);
        }
        Err(MoraError::Runtime(format!(
            "kv_allocator: unknown sequence {seq_id}"
        )))
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
        let _trace = crate::runtime::profiler::TraceScope::enter(
            "kv_cache",
            "kv_append_tokens",
            vec![
                ("seq_id".to_string(), seq_id.to_string()),
                ("tokens".to_string(), count.to_string()),
            ],
        );
        let mut inner = self.inner.lock();
        if inner.swapped_seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.append_tokens: sequence {seq_id} is swapped out to host RAM; call swap_in({seq_id}) first"
            )));
        }
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
        self.ensure_can_allocate_or_reclaim_radix(&mut inner, extra, seq_id)?;

        let mut newly = Vec::with_capacity(extra);
        for _ in 0..extra {
            match self.alloc_physical_page(&mut inner) {
                Ok(pid) => newly.push(pid),
                Err(e) => {
                    for pid in newly {
                        let _ = Self::release_physical_page(&mut inner, pid);
                    }
                    return Err(e);
                }
            }
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
        if inner.seqs.contains_key(&dst_seq_id) || inner.swapped_seqs.contains_key(&dst_seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator: destination sequence {dst_seq_id} already allocated"
            )));
        }
        if inner.swapped_seqs.contains_key(&src_seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.fork_seq: source sequence {src_seq_id} is swapped out to host RAM; call swap_in({src_seq_id}) first"
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
        if inner.swapped_seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.write_kv: sequence {seq_id} is swapped out to host RAM; call swap_in({seq_id}) first"
            )));
        }
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
            self.ensure_can_allocate_or_reclaim_radix(&mut inner, 1, seq_id)?;
            let new_pid = self.alloc_physical_page(&mut inner)?;
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
        if self.vram_backed {
            if let Some(k_vram) = inner.k_vram_pages[target_pid].clone() {
                let raw = inner.k_pages[target_pid].copy_bytes_out();
                k_vram.upload_bytes(&raw)?;
            }
            if let Some(v_vram) = inner.v_vram_pages[target_pid].clone() {
                let raw = inner.v_pages[target_pid].copy_bytes_out();
                v_vram.upload_bytes(&raw)?;
            }
        }
        if let Some(seq) = inner.seqs.get_mut(&seq_id) {
            seq.last_access_tick = tick;
        }
        let fired = self.collect_triggered_watermarks(&mut inner);
        Ok(fired)
    }

    /// Materialize `[K, V]` tensors of shape `[num_tokens, num_heads, head_dim]` for `seq_id`.
    /// When physically backed by GPU VRAM, reads directly from the sequence's resident VRAM pages.
    pub fn read_kv(&self, seq_id: u64) -> MoraResult<(Arc<Tensor>, Arc<Tensor>)> {
        let mut inner = self.inner.lock();
        if inner.swapped_seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.read_kv: sequence {seq_id} is swapped out to host RAM; call swap_in({seq_id}) first"
            )));
        }
        let tick = Self::next_tick(&mut inner);
        let seq = inner.seqs.get_mut(&seq_id).ok_or_else(|| {
            MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
        })?;
        seq.last_access_tick = tick;
        let num_tokens = seq.num_tokens;
        let page_table = seq.page_table.clone();

        let mut downloaded_pages: HashMap<usize, (AlignedByteBuffer, AlignedByteBuffer)> =
            HashMap::new();
        if self.vram_backed {
            for &pid in &page_table {
                if let std::collections::hash_map::Entry::Vacant(e) = downloaded_pages.entry(pid)
                    && let (Some(k_vram), Some(v_vram)) = (
                        inner.k_vram_pages[pid].as_ref(),
                        inner.v_vram_pages[pid].as_ref(),
                    )
                {
                    let mut raw_k = vec![0u8; k_vram.byte_len()];
                    let mut raw_v = vec![0u8; v_vram.byte_len()];
                    k_vram.download_bytes(&mut raw_k)?;
                    v_vram.download_bytes(&mut raw_v)?;
                    let kb =
                        AlignedByteBuffer::from_raw_bytes(&raw_k, self.elems_per_page, self.dtype)?;
                    let vb =
                        AlignedByteBuffer::from_raw_bytes(&raw_v, self.elems_per_page, self.dtype)?;
                    e.insert((kb, vb));
                }
            }
        }

        let total_elems = num_tokens * self.elems_per_token;
        let mut k_out = Vec::with_capacity(total_elems);
        let mut v_out = Vec::with_capacity(total_elems);
        for t_idx in 0..num_tokens {
            let pid = page_table[t_idx / self.page_tokens];
            let offset = (t_idx % self.page_tokens) * self.elems_per_token;
            let (k_src, v_src) = if let Some((kb, vb)) = downloaded_pages.get(&pid) {
                (kb, vb)
            } else {
                (&inner.k_pages[pid], &inner.v_pages[pid])
            };
            for i in 0..self.elems_per_token {
                k_out.push(k_src.get_f64(offset + i));
                v_out.push(v_src.get_f64(offset + i));
            }
        }
        let shape = vec![num_tokens, self.num_heads, self.head_dim];
        let kt = Tensor::from_raw(k_out, shape.clone(), self.dtype, self.device.clone(), false)?;
        let vt = Tensor::from_raw(v_out, shape, self.dtype, self.device.clone(), false)?;
        Ok((kt, vt))
    }

    /// Free sequence `seq_id` (resident or swapped out), returning the number of physical pages
    /// reclaimed to the free pool.
    pub fn free_seq(&self, seq_id: u64) -> MoraResult<usize> {
        let mut inner = self.inner.lock();
        if inner.swapped_seqs.remove(&seq_id).is_some() {
            return Ok(0);
        }
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

    /// G3: Offload an active sequence's K/V pages from physical VRAM/pool into host RAM,
    /// freeing its physical pages and GPU VRAM buffers back to the pool and updating watermark
    /// hysteresis. Returns the number of physical pages reclaimed.
    pub(crate) fn swap_out(&self, seq_id: u64) -> MoraResult<usize> {
        let _trace = crate::runtime::profiler::TraceScope::enter(
            "kv_cache",
            "kv_swap_out",
            vec![("seq_id".to_string(), seq_id.to_string())],
        );
        let mut inner = self.inner.lock();
        if inner.swapped_seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.swap_out: sequence {seq_id} is already swapped out"
            )));
        }
        let seq = inner.seqs.get(&seq_id).cloned().ok_or_else(|| {
            MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
        })?;

        let mut host_pages = Vec::with_capacity(seq.page_table.len());
        let mut transferred_bytes: usize = 0;
        for &pid in &seq.page_table {
            let (raw_k, raw_v) = if self.vram_backed
                && let (Some(k_vram), Some(v_vram)) = (
                    inner.k_vram_pages[pid].as_ref(),
                    inner.v_vram_pages[pid].as_ref(),
                ) {
                let mut rk = vec![0u8; k_vram.byte_len()];
                let mut rv = vec![0u8; v_vram.byte_len()];
                k_vram.download_bytes(&mut rk)?;
                v_vram.download_bytes(&mut rv)?;
                (rk, rv)
            } else {
                (
                    inner.k_pages[pid].copy_bytes_out(),
                    inner.v_pages[pid].copy_bytes_out(),
                )
            };
            transferred_bytes += raw_k.len() + raw_v.len();
            host_pages.push((raw_k, raw_v));
        }

        inner.seqs.remove(&seq_id);
        let mut freed = 0usize;
        for pid in seq.page_table {
            if Self::release_physical_page(&mut inner, pid) {
                freed += 1;
            }
        }
        inner.swapped_seqs.insert(
            seq_id,
            SwappedSeqState {
                host_pages,
                num_tokens: seq.num_tokens,
                priority: seq.priority,
            },
        );
        inner.swap_outs += 1;
        inner.swap_out_bytes += transferred_bytes as u64;
        self.refresh_watermark_hysteresis(&mut inner);
        Ok(freed)
    }

    /// G3: Restore a swapped-out sequence from host RAM back into freshly allocated physical pages
    /// (and physical GPU VRAM when `vram_backed`), enforcing both pool capacity and hard `max_bytes`
    /// quota atomically before committing. Returns `(new_page_table, triggered_watermarks)`.
    pub(crate) fn swap_in_with_watermarks(
        &self,
        seq_id: u64,
    ) -> MoraResult<(Vec<usize>, Vec<TriggeredWatermark>)> {
        let _trace = crate::runtime::profiler::TraceScope::enter(
            "kv_cache",
            "kv_swap_in",
            vec![("seq_id".to_string(), seq_id.to_string())],
        );
        let mut inner = self.inner.lock();
        if inner.seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.swap_in: sequence {seq_id} is already resident"
            )));
        }
        let swapped = inner.swapped_seqs.get(&seq_id).cloned().ok_or_else(|| {
            MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
        })?;
        let pages_needed = swapped.host_pages.len();
        self.ensure_can_allocate_or_reclaim_radix(&mut inner, pages_needed, seq_id)?;

        let mut newly = Vec::with_capacity(pages_needed);
        for _ in 0..pages_needed {
            match self.alloc_physical_page(&mut inner) {
                Ok(pid) => newly.push(pid),
                Err(e) => {
                    for pid in newly {
                        let _ = Self::release_physical_page(&mut inner, pid);
                    }
                    return Err(e);
                }
            }
        }

        let mut transferred_bytes: usize = 0;
        for (&pid, (raw_k, raw_v)) in newly.iter().zip(swapped.host_pages.iter()) {
            let kb = match AlignedByteBuffer::from_raw_bytes(raw_k, self.elems_per_page, self.dtype)
            {
                Ok(b) => b,
                Err(e) => {
                    for p in newly {
                        let _ = Self::release_physical_page(&mut inner, p);
                    }
                    return Err(e);
                }
            };
            let vb = match AlignedByteBuffer::from_raw_bytes(raw_v, self.elems_per_page, self.dtype)
            {
                Ok(b) => b,
                Err(e) => {
                    for p in newly {
                        let _ = Self::release_physical_page(&mut inner, p);
                    }
                    return Err(e);
                }
            };
            inner.k_pages[pid] = kb;
            inner.v_pages[pid] = vb;
            if self.vram_backed {
                let up_res = inner.k_vram_pages[pid]
                    .as_ref()
                    .map(|b| b.upload_bytes(raw_k))
                    .transpose()
                    .and_then(|_| {
                        inner.v_vram_pages[pid]
                            .as_ref()
                            .map(|b| b.upload_bytes(raw_v))
                            .transpose()
                    });
                if let Err(e) = up_res {
                    for p in newly {
                        let _ = Self::release_physical_page(&mut inner, p);
                    }
                    return Err(e);
                }
            }
            transferred_bytes += raw_k.len() + raw_v.len();
        }

        inner.swapped_seqs.remove(&seq_id);
        let tick = Self::next_tick(&mut inner);
        let page_table = newly.clone();
        inner.seqs.insert(
            seq_id,
            SeqKvState {
                page_table: newly,
                num_tokens: swapped.num_tokens,
                priority: swapped.priority,
                last_access_tick: tick,
            },
        );
        inner.swap_ins += 1;
        inner.swap_in_bytes += transferred_bytes as u64;
        let fired = self.collect_triggered_watermarks(&mut inner);
        Ok((page_table, fired))
    }

    /// P5-2A: Register all complete `page_tokens`-aligned prefix blocks of active sequence `seq_id`
    /// into the content-addressed RadixAttention prefix trie, returning the count of blocks newly cached.
    ///
    /// Increments physical `refcounts` so subsequent `write_kv` on `seq_id` triggers Copy-on-Write
    /// without mutating the cached immutable prefix pages, and `free_seq(seq_id)` leaves the warm
    /// physical pages (and GPU VRAM buffers) pinned in the radix cache for future zero-copy reuse.
    pub(crate) fn cache_prefix(&self, seq_id: u64, token_ids: &[i64]) -> MoraResult<usize> {
        let _trace = crate::runtime::profiler::TraceScope::enter(
            "kv_cache",
            "kv_cache_prefix",
            vec![
                ("seq_id".to_string(), seq_id.to_string()),
                ("tokens".to_string(), token_ids.len().to_string()),
            ],
        );
        let mut inner = self.inner.lock();
        if inner.swapped_seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.cache_prefix: sequence {seq_id} is swapped out to host RAM; call swap_in({seq_id}) first"
            )));
        }
        let (seq_tokens, seq_pages) = {
            let seq = inner.seqs.get(&seq_id).ok_or_else(|| {
                MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
            })?;
            (seq.num_tokens, seq.page_table.clone())
        };
        let usable = token_ids.len().min(seq_tokens);
        let full_blocks = usable / self.page_tokens;
        let tick = Self::next_tick(&mut inner);

        let mut cur = 0usize;
        let mut newly_cached = 0usize;
        for (blk, &pid) in seq_pages.iter().take(full_blocks).enumerate() {
            let start = blk * self.page_tokens;
            let key = token_ids[start..start + self.page_tokens].to_vec();
            let existing = inner.radix_nodes[cur]
                .as_ref()
                .and_then(|n| n.children.get(&key).copied());
            if let Some(child_idx) = existing {
                if let Some(child) = inner.radix_nodes[child_idx].as_mut() {
                    child.lru_tick = tick;
                }
                cur = child_idx;
            } else {
                inner.refcounts[pid] += 1;
                inner.radix_refcounts[pid] += 1;
                let node = PrefixNode {
                    parent: Some(cur),
                    block_tokens: key.clone(),
                    page_id: Some(pid),
                    children: HashMap::new(),
                    lru_tick: tick,
                };
                let new_idx = if let Some(slot_idx) = inner
                    .radix_nodes
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find_map(|(i, s)| s.is_none().then_some(i))
                {
                    inner.radix_nodes[slot_idx] = Some(node);
                    slot_idx
                } else {
                    let idx = inner.radix_nodes.len();
                    inner.radix_nodes.push(Some(node));
                    idx
                };
                if let Some(parent) = inner.radix_nodes[cur].as_mut() {
                    parent.children.insert(key, new_idx);
                }
                inner.radix_cached_pages += 1;
                newly_cached += 1;
                cur = new_idx;
            }
        }
        if let Some(seq) = inner.seqs.get_mut(&seq_id) {
            seq.last_access_tick = tick;
        }
        Ok(newly_cached)
    }

    /// P5-2A: Return the longest cached block-aligned prefix length (in tokens) matching `token_ids`.
    pub(crate) fn match_prefix(&self, token_ids: &[i64]) -> usize {
        let inner = self.inner.lock();
        let full_blocks = token_ids.len() / self.page_tokens;
        let mut cur = 0usize;
        let mut matched_blocks = 0usize;
        for blk in 0..full_blocks {
            let start = blk * self.page_tokens;
            let key = &token_ids[start..start + self.page_tokens];
            let next = inner.radix_nodes[cur]
                .as_ref()
                .and_then(|n| n.children.get(key).copied());
            match next {
                Some(child_idx) => {
                    cur = child_idx;
                    matched_blocks += 1;
                }
                None => break,
            }
        }
        matched_blocks * self.page_tokens
    }

    /// P5-2A: Zero-copy bind the longest cached prefix matching `token_ids` onto fresh empty
    /// sequence `seq_id` (incrementing `refcounts`, preserving CoW safety on subsequent writes).
    /// Returns the number of tokens attached.
    pub(crate) fn attach_prefix(&self, seq_id: u64, token_ids: &[i64]) -> MoraResult<usize> {
        let _trace = crate::runtime::profiler::TraceScope::enter(
            "kv_cache",
            "kv_attach_prefix",
            vec![
                ("seq_id".to_string(), seq_id.to_string()),
                ("tokens".to_string(), token_ids.len().to_string()),
            ],
        );
        let mut inner = self.inner.lock();
        if inner.swapped_seqs.contains_key(&seq_id) {
            return Err(MoraError::Runtime(format!(
                "kv_allocator.attach_prefix: sequence {seq_id} is swapped out to host RAM; call swap_in({seq_id}) first"
            )));
        }
        {
            let seq = inner.seqs.get(&seq_id).ok_or_else(|| {
                MoraError::Runtime(format!("kv_allocator: unknown sequence {seq_id}"))
            })?;
            if seq.num_tokens > 0 || !seq.page_table.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "kv_allocator.attach_prefix: sequence {seq_id} already has {} tokens (attach_prefix requires a fresh empty sequence)",
                    seq.num_tokens
                )));
            }
        }

        let full_blocks = token_ids.len() / self.page_tokens;
        let mut cur = 0usize;
        let mut matched: Vec<(usize, usize)> = Vec::new();
        for blk in 0..full_blocks {
            let start = blk * self.page_tokens;
            let key = &token_ids[start..start + self.page_tokens];
            let next = inner.radix_nodes[cur]
                .as_ref()
                .and_then(|n| n.children.get(key).copied());
            let Some(child_idx) = next else {
                break;
            };
            let Some(pid) = inner.radix_nodes[child_idx]
                .as_ref()
                .and_then(|n| n.page_id)
            else {
                break;
            };
            matched.push((child_idx, pid));
            cur = child_idx;
        }
        if matched.is_empty() {
            return Ok(0);
        }

        let tick = Self::next_tick(&mut inner);
        let mut attached_pages = Vec::with_capacity(matched.len());
        for (node_idx, pid) in matched {
            if let Some(node) = inner.radix_nodes[node_idx].as_mut() {
                node.lru_tick = tick;
            }
            inner.refcounts[pid] += 1;
            attached_pages.push(pid);
        }
        let matched_tokens = attached_pages.len() * self.page_tokens;
        if let Some(seq) = inner.seqs.get_mut(&seq_id) {
            seq.page_table = attached_pages;
            seq.num_tokens = matched_tokens;
            seq.last_access_tick = tick;
        }
        inner.prefix_cache_hits += 1;
        Ok(matched_tokens)
    }

    /// Return true if `seq_id` is currently swapped out to host RAM.
    pub(crate) fn is_swapped(&self, seq_id: u64) -> bool {
        self.inner.lock().swapped_seqs.contains_key(&seq_id)
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
            if Self::evict_one_lru_radix_leaf(&mut inner) {
                continue;
            }
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
        if let Some(s) = inner.seqs.get(&seq_id) {
            return Ok(s.num_tokens);
        }
        if let Some(s) = inner.swapped_seqs.get(&seq_id) {
            return Ok(s.num_tokens);
        }
        Err(MoraError::Runtime(format!(
            "kv_allocator: unknown sequence {seq_id}"
        )))
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
        let vram_bytes_allocated: usize = inner
            .k_vram_pages
            .iter()
            .zip(inner.v_vram_pages.iter())
            .map(|(k, v)| {
                k.as_ref().map_or(0, |b| b.byte_len()) + v.as_ref().map_or(0, |b| b.byte_len())
            })
            .sum();
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
            "vram_backed".to_string(),
            Value::Int(i64::from(self.vram_backed)),
        );
        m.insert(
            "vram_bytes_allocated".to_string(),
            Value::Int(vram_bytes_allocated as i64),
        );
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
        let swapped_pages: usize = inner
            .swapped_seqs
            .values()
            .map(|s| s.host_pages.len())
            .sum();
        m.insert(
            "watermark_triggers".to_string(),
            Value::Int(inner.watermark_triggers as i64),
        );
        m.insert(
            "swapped_seqs".to_string(),
            Value::Int(inner.swapped_seqs.len() as i64),
        );
        m.insert(
            "swapped_pages".to_string(),
            Value::Int(swapped_pages as i64),
        );
        m.insert("swap_outs".to_string(), Value::Int(inner.swap_outs as i64));
        m.insert("swap_ins".to_string(), Value::Int(inner.swap_ins as i64));
        m.insert(
            "swap_out_bytes".to_string(),
            Value::Int(inner.swap_out_bytes as i64),
        );
        m.insert(
            "swap_in_bytes".to_string(),
            Value::Int(inner.swap_in_bytes as i64),
        );
        m.insert(
            "prefix_cache_hits".to_string(),
            Value::Int(inner.prefix_cache_hits as i64),
        );
        m.insert(
            "prefix_cached_pages".to_string(),
            Value::Int(inner.radix_cached_pages as i64),
        );
        m.insert(
            "prefix_evictions".to_string(),
            Value::Int(inner.prefix_evictions as i64),
        );
        m
    }
}
