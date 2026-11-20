//! Wave 4: Dynamic VRAM / KV-Cache Resource Orchestration (`KVPageAllocator`).
//!
//! Implements PagedAttention-style virtual-to-physical page table management:
//! - Hardware-packed 64-byte aligned [`AlignedByteBuffer`] K/V page pools honoring exact `DType`
//!   widths (`f16`, `bf16`, `fp8`, `f32`, `f64`, `i8`, `i4`, ...).
//! - Per-sequence page tables, dynamic token growth (`append_tokens`), Copy-on-Write prefix
//!   sharing (`fork_seq` + CoW split on `write_kv`), LRU eviction (`evict_lru`), and live
//!   utilization/memory telemetry (`stats`, `utilization`, `bytes_used`).
//! - Thread-safe via `parking_lot::Mutex` for concurrent `spawn`ed inference handlers.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::value::Value;
use crate::value::tensor::{AlignedByteBuffer, Tensor};

#[derive(Debug, Clone)]
struct SeqKvState {
    page_table: Vec<usize>,
    num_tokens: usize,
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
}

/// Paged KV-cache memory allocator with Copy-on-Write prefix sharing and LRU eviction.
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
    inner: Mutex<KvPoolInner>,
}

impl KVPageAllocator {
    /// Create a new paged KV allocator managing `total_pages` physical blocks.
    pub fn new(
        total_pages: usize,
        page_tokens: usize,
        num_heads: usize,
        head_dim: usize,
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Self>> {
        if total_pages == 0 || page_tokens == 0 || num_heads == 0 || head_dim == 0 {
            return Err(MoraError::Runtime(
                "kv_allocator: total_pages, page_tokens, num_heads, and head_dim must all be > 0"
                    .to_string(),
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

        Ok(Arc::new(Self {
            total_pages,
            page_tokens,
            num_heads,
            head_dim,
            dtype,
            device,
            elems_per_page,
            elems_per_token,
            inner: Mutex::new(KvPoolInner {
                free_pages,
                refcounts,
                k_pages,
                v_pages,
                seqs: HashMap::new(),
                tick: 0,
            }),
        }))
    }

    fn next_tick(inner: &mut KvPoolInner) -> u64 {
        inner.tick += 1;
        inner.tick
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

    /// Register a new sequence `seq_id` with an empty page table.
    pub fn alloc_seq(&self, seq_id: u64) -> MoraResult<bool> {
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
                last_access_tick: tick,
            },
        );
        Ok(true)
    }

    /// Reserve capacity for `count` additional tokens in sequence `seq_id`, allocating pages on demand.
    /// Rolls back newly allocated pages atomically if physical pages are exhausted mid-request.
    pub fn append_tokens(&self, seq_id: u64, count: usize) -> MoraResult<Vec<usize>> {
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
        if extra > inner.free_pages.len() {
            return Err(MoraError::Runtime(format!(
                "KVPageAllocator out of memory: sequence {seq_id} needs {extra} page(s), only {} free",
                inner.free_pages.len()
            )));
        }
        let mut newly = Vec::with_capacity(extra);
        for _ in 0..extra {
            newly.push(Self::alloc_physical_page(&mut inner)?);
        }
        let seq = inner.seqs.get_mut(&seq_id).expect("checked");
        seq.page_table.extend(newly);
        seq.num_tokens = new_tokens;
        seq.last_access_tick = tick;
        Ok(seq.page_table.clone())
    }

    /// Fork `src_seq_id` into `dst_seq_id`, sharing existing physical pages via Copy-on-Write refcounts.
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
                last_access_tick: tick,
            },
        );
        if let Some(src_mut) = inner.seqs.get_mut(&src_seq_id) {
            src_mut.last_access_tick = tick;
        }
        Ok(table)
    }

    /// Write `[num_heads, head_dim]` key and value tensors at logical `token_pos` of `seq_id`.
    /// Performs Copy-on-Write physical page splitting if the target page is shared (`refcount > 1`).
    pub fn write_kv(
        &self,
        seq_id: u64,
        token_pos: usize,
        k: &Tensor,
        v: &Tensor,
    ) -> MoraResult<()> {
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
            // Copy-on-Write split!
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
        Ok(())
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
        lru_id as i64
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
        (self.used_pages() as f64) / (self.total_pages as f64)
    }

    pub fn bytes_used(&self) -> usize {
        let inner = self.inner.lock();
        let mut bytes = 0usize;
        for (pid, &rc) in inner.refcounts.iter().enumerate() {
            if rc > 0 {
                bytes += inner.k_pages[pid].byte_len() + inner.v_pages[pid].byte_len();
            }
        }
        bytes
    }

    pub fn stats_dict(&self) -> HashMap<String, Value> {
        let inner = self.inner.lock();
        let free = inner.free_pages.len();
        let used = self.total_pages - free;
        let cow_shared = inner.refcounts.iter().filter(|&&rc| rc > 1).count();
        let mut bytes = 0usize;
        for (pid, &rc) in inner.refcounts.iter().enumerate() {
            if rc > 0 {
                bytes += inner.k_pages[pid].byte_len() + inner.v_pages[pid].byte_len();
            }
        }
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
        m.insert(
            "page_tokens".to_string(),
            Value::Int(self.page_tokens as i64),
        );
        m.insert(
            "elems_per_page".to_string(),
            Value::Int(self.elems_per_page as i64),
        );
        m
    }
}
