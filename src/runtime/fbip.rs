//! P6-1: Perceus / Functional-But-In-Place (FBIP) runtime telemetry counters.
//!
//! Scoped per [`crate::runtime::CoreRuntime`] (and shared across child fibers spawned from it via
//! `Arc::clone` in `ExecSession::fork_for_fiber`), avoiding any process-global mutable state so
//! concurrent tests and independent runtime instances never interfere.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::value::Value;

#[cfg(debug_assertions)]
use std::cell::Cell;

#[cfg(debug_assertions)]
thread_local! {
    /// Debug-build-only trace of outer `RegSlot::Heap(Arc<Value>)` refcount bumps on the calling
    /// thread. Tests use it to prove a borrowed call passes the caller's slot by address instead
    /// of cloning the outer `Arc`; release builds compile this instrumentation out entirely.
    static REGSLOT_HEAP_CLONES: Cell<u64> = const { Cell::new(0) };
}

/// Debug-build-only: count one outer `RegSlot::Heap(Arc<Value>)` clone on this thread.
#[inline]
pub(crate) fn note_regslot_heap_clone() {
    #[cfg(debug_assertions)]
    REGSLOT_HEAP_CLONES.with(|count| count.set(count.get() + 1));
}

/// Debug-build-only: zero the calling thread's outer-`Arc` clone trace (called by
/// `fbip_reset_stats`, so the count reports clones since that reset).
#[inline]
pub(crate) fn reset_thread_heap_clone_count() {
    #[cfg(debug_assertions)]
    REGSLOT_HEAP_CLONES.with(|count| count.set(0));
}

/// Outer-`Arc` clones observed on the calling thread since the last reset (always 0 in release).
#[inline]
pub(crate) fn thread_heap_clone_count() -> u64 {
    #[cfg(debug_assertions)]
    {
        REGSLOT_HEAP_CLONES.with(Cell::get)
    }
    #[cfg(not(debug_assertions))]
    {
        0
    }
}

/// Lock-free atomic telemetry counters tracking Perceus / FBIP in-place reset/reuse hits and
/// multi-shot continuation copy fallbacks within a [`crate::runtime::CoreRuntime`] session.
#[derive(Debug, Default)]
pub struct FbipCounters {
    pub dict_inplace_reuses: AtomicU64,
    pub list_inplace_reuses: AtomicU64,
    pub tensor_host_reuses: AtomicU64,
    pub tensor_vram_reuses: AtomicU64,
    pub borrowed_call_elisions: AtomicU64,
    pub continuation_copy_fallbacks: AtomicU64,
}

impl FbipCounters {
    #[inline]
    pub fn on_dict_reuse(&self) {
        self.dict_inplace_reuses.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn on_list_reuse(&self) {
        self.list_inplace_reuses.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn on_tensor_host_reuse(&self) {
        self.tensor_host_reuses.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn on_tensor_vram_reuse(&self) {
        self.tensor_vram_reuses.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn on_borrowed_call(&self) {
        self.borrowed_call_elisions.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn on_continuation_copy_fallback(&self) {
        self.continuation_copy_fallbacks
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Reset all FBIP counters in this runtime session to zero.
    pub fn reset(&self) {
        self.dict_inplace_reuses.store(0, Ordering::Relaxed);
        self.list_inplace_reuses.store(0, Ordering::Relaxed);
        self.tensor_host_reuses.store(0, Ordering::Relaxed);
        self.tensor_vram_reuses.store(0, Ordering::Relaxed);
        self.borrowed_call_elisions.store(0, Ordering::Relaxed);
        self.continuation_copy_fallbacks.store(0, Ordering::Relaxed);
    }

    /// Snapshot all FBIP counters into a Mora `dict<string, int>`.
    pub fn to_dict_value(&self) -> Value {
        let mut map = HashMap::with_capacity(8);
        let d = self.dict_inplace_reuses.load(Ordering::Relaxed) as i64;
        let l = self.list_inplace_reuses.load(Ordering::Relaxed) as i64;
        let th = self.tensor_host_reuses.load(Ordering::Relaxed) as i64;
        let tv = self.tensor_vram_reuses.load(Ordering::Relaxed) as i64;
        let bc = self.borrowed_call_elisions.load(Ordering::Relaxed) as i64;
        let cf = self.continuation_copy_fallbacks.load(Ordering::Relaxed) as i64;
        map.insert("dict_inplace_reuses".to_string(), Value::Int(d));
        map.insert("list_inplace_reuses".to_string(), Value::Int(l));
        map.insert("tensor_host_reuses".to_string(), Value::Int(th));
        map.insert("tensor_vram_reuses".to_string(), Value::Int(tv));
        map.insert(
            "total_inplace_reuses".to_string(),
            Value::Int(d + l + th + tv),
        );
        map.insert("borrowed_call_elisions".to_string(), Value::Int(bc));
        map.insert("continuation_copy_fallbacks".to_string(), Value::Int(cf));
        map.insert(
            "regslot_heap_clones".to_string(),
            Value::Int(thread_heap_clone_count() as i64),
        );
        Value::Dict(map.into())
    }
}
