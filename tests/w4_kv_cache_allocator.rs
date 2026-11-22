//! Wave 4 Step 2 verification: Dynamic VRAM / KV-Cache Resource Orchestration (`KVPageAllocator`)
//! - Physical page allocation, multi-sequence page tables, hardware-packed DType byte accounting
//! - Zero-copy prefix sharing (`fork_seq`) + Copy-on-Write split on `write_kv`
//! - OOM detection, `free_seq` page reclamation, `evict_lru`, and concurrent `spawn`ed workers
//! - Static HM types (`kv_allocator`) and LSP completion parity

use std::collections::HashMap;
use std::sync::Arc;

use mora::flow::json::JsonValue as JsonVal;
use mora::lsp::providers::completion_v3;
use mora::lsp::server::DocumentState;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors for:\n{src}\ngot: {errs:?}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error for:\n{src}\nerr: {e}"));
    val
}

fn assert_typecheck_fails(src: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|_e| panic!("compile failed:\n{src}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "expected static type error for:\n{src}\nbut typeck succeeded"
    );
}

#[test]
fn w4_kv_allocator_paging_cow_fork_and_lru_eviction() {
    let src = r#"
-- 4 physical pages, 2 tokens/page, 2 heads, head_dim=2, f16 (2 bytes/elem => 16 bytes/page * 2 = 32 bytes/page)
let kv: kv_allocator = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f16")
kv.alloc_seq(1)
let p1 = kv.append_tokens(1, 3) -- needs 2 pages

let k0 = tensor([[1.0, 2.0], [3.0, 4.0]], dtype="f16")
let v0 = tensor([[0.5, 1.5], [2.5, 3.5]], dtype="f16")
let k1 = tensor([[5.0, 6.0], [7.0, 8.0]], dtype="f16")
let v1 = tensor([[4.5, 5.5], [6.5, 7.5]], dtype="f16")
kv.write_kv(1, 0, k0, v0)
kv.write_kv(1, 1, k1, v1)

-- Fork seq 1 -> seq 2: shares same 2 physical pages (zero extra pages allocated!)
let p2 = kv.fork_seq(1, 2)
let stats_after_fork = kv.stats()

-- CoW write on seq 2 token 0: splits page 0 into a new physical page without mutating seq 1!
let k0_new = tensor([[9.0, 9.0], [9.0, 9.0]], dtype="f16")
let v0_new = tensor([[1.0, 1.0], [1.0, 1.0]], dtype="f16")
kv.write_kv(2, 0, k0_new, v0_new)

let kv1 = kv.read_kv(1)
let kv2 = kv.read_kv(2)

-- Evict LRU (seq 1 was accessed before seq 2; wait: read_kv(1) then read_kv(2) => seq 1 is older!)
let evicted = kv.evict_lru()
let free_after_evict = kv.free_pages()
let freed_seq2 = kv.free_seq(2)

[str(p1.len()), str(p1 == p2), str(stats_after_fork.get("cow_shared_pages")), str(stats_after_fork.get("used_pages")), str(kv.bytes_used()), str(kv1[0][0].to_list()), str(kv2[0][0].to_list()), str(evicted), str(free_after_evict), str(freed_seq2), str(kv.free_pages())]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("2".to_string()));
    assert_eq!(v[1], Value::String("true".to_string())); // fork shares identical physical pages
    assert_eq!(v[2], Value::String("2".to_string())); // 2 cow_shared_pages
    assert_eq!(v[3], Value::String("2".to_string())); // only 2 physical pages used
    // After CoW split (3 pages used) -> evict_lru(1) frees seq 1's 1 unshared page -> 2 pages used; free_seq(2) frees 2 pages
    assert_eq!(v[5], Value::String("[[1.0, 2.0], [3.0, 4.0]]".to_string())); // seq 1 untouched!
    assert_eq!(v[6], Value::String("[[9.0, 9.0], [9.0, 9.0]]".to_string())); // seq 2 got new page!
    assert_eq!(v[7], Value::String("1".to_string())); // seq 1 evicted
    assert_eq!(v[8], Value::String("2".to_string())); // 2 free pages after evicting seq 1
    assert_eq!(v[9], Value::String("2".to_string())); // free_seq(2) reclaimed 2 pages
    assert_eq!(v[10], Value::String("4".to_string())); // all 4 pages free
}

#[test]
fn w4_kv_allocator_concurrent_spawn_and_oom_protection() {
    let src = r#"
let kv = kv_allocator(6, 2, 1, 2, dtype="f64")
let h1 = spawn fn()
  kv.alloc_seq(10)
  kv.append_tokens(10, 4)
  return kv.seq_tokens(10)
end
let h2 = spawn fn()
  kv.alloc_seq(20)
  kv.append_tokens(20, 4)
  return kv.seq_tokens(20)
end
let t1 = h1.join()
let t2 = h2.join()
let util = kv.utilization()
[str(t1), str(t2), str(kv.used_pages()), str(util > 0.6)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("4".to_string()));
    assert_eq!(v[1], Value::String("4".to_string()));
    assert_eq!(v[2], Value::String("4".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));

    // OOM error check at runtime
    let oom_src = r#"
let kv = kv_allocator(2, 2, 1, 2)
kv.alloc_seq(1)
kv.append_tokens(1, 6)
"#;
    let (prog, w) = ParserV3::compile(oom_src).expect("compile");
    assert!(check_program_witnesses(&w).is_empty());
    let mut core = CoreRuntime::default();
    let err = execute_program(Arc::new(prog), &mut core).expect_err("OOM expected");
    assert!(
        err.to_string().contains("out of memory"),
        "expected OOM error, got: {err}"
    );
}

#[test]
fn w4_kv_allocator_hard_quota_priority_preemption_and_reclaim() {
    // 8 pages, 2 tokens/page, 2 heads, head_dim=2, f16 (2B/elem => 16B K + 16B V = 32B/page)
    // Hard quota max_bytes=96 => at most 3 physical pages even though 8 physical pages exist!
    let src = r#"
let kv = kv_allocator(total_pages=8, page_tokens=2, num_heads=2, head_dim=2, dtype="f16", max_bytes=96)
kv.alloc_seq(1, 1)
kv.alloc_seq(2, 1)
kv.alloc_seq(3, 10)
kv.append_tokens(1, 2)
kv.append_tokens(2, 2)
kv.append_tokens(3, 2)

-- seq 1 and seq 2 both have priority 1; seq 1 was accessed earlier (older LRU), seq 3 has priority 10.
let p3_before = kv.seq_priority(3)
kv.set_priority(2, 0)
let p2_after = kv.seq_priority(2)

-- evict_preempt(5) must pick lowest priority (seq 2, prio 0) even though seq 1 is older LRU!
let v1 = kv.evict_preempt(5)
-- evict_preempt(1) must return -1 since remaining seqs have priorities 1 and 10 (>= 1).
let v_none = kv.evict_preempt(1)
-- reclaim_for(2, 10) needs 2 pages within 96B quota (currently 2 used = 64B, so only 1 free under quota):
-- must preempt seq 1 (prio 1 < 10) so 2 pages fit under 96B quota, while protecting seq 3 (prio 10)!
let reclaimed = kv.reclaim_for(2, 10)
-- Raise quota dynamically to 192B (6 pages) and append 4 tokens (2 pages) to seq 3
kv.set_quota(192)
kv.append_tokens(3, 4)
let st = kv.stats()
[str(p3_before), str(p2_after), str(v1), str(v_none), str(reclaimed), str(kv.quota_bytes()), str(kv.used_pages()), str(st.get("preemptions"))]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("10".to_string()));
    assert_eq!(v[1], Value::String("0".to_string()));
    assert_eq!(v[2], Value::String("2".to_string())); // lowest priority evicted first
    assert_eq!(v[3], Value::String("-1".to_string())); // high-priority sequences protected
    assert_eq!(v[4], Value::String("[1]".to_string())); // reclaimed seq 1 for prio-10 requester
    assert_eq!(v[5], Value::String("192".to_string()));
    assert_eq!(v[6], Value::String("3".to_string())); // seq 3 has 1 + 2 = 3 pages
    assert_eq!(v[7], Value::String("2".to_string())); // 2 preemptions recorded

    // Verify atomic byte-quota rejection (exceeding max_bytes fails with zero page leak)
    let quota_err_src = r#"
let kv = kv_allocator(8, 2, 2, 2, "f16", "cpu", 64)
kv.alloc_seq(1)
kv.append_tokens(1, 2)
kv.append_tokens(1, 4)
"#;
    let (prog, w) = ParserV3::compile(quota_err_src).expect("compile");
    assert!(check_program_witnesses(&w).is_empty());
    let mut core = CoreRuntime::default();
    let err = execute_program(Arc::new(prog), &mut core).expect_err("quota error expected");
    assert!(
        err.to_string().contains("quota exceeded"),
        "expected quota exceeded error, got: {err}"
    );
}

#[test]
fn w4_kv_allocator_watermark_callbacks_and_dynamic_preemption() {
    // 4 pages, 2 tokens/page => crossing 0.50 (2 pages) logs event; crossing 0.75 (3 pages)
    // dynamically preempts low-priority seq 1 INSIDE the watermark callback without deadlocking!
    let src = r#"
let kv = kv_allocator(4, 2, 1, 2, dtype="f64")
let log_count = atom(0)
let last_evicted = atom(-1)

kv.on_watermark(0.5, fn(ev)
  swap(log_count, fn(c) c + 1 end)
end)

kv.on_watermark(0.75, fn(ev)
  let victim = kv.evict_preempt(5)
  swap(last_evicted, fn(_) victim end)
end)

kv.alloc_seq(1, 1)
kv.alloc_seq(2, 10)
-- 1 page (0.25) -> no watermark
kv.append_tokens(1, 2)
let c1 = deref(log_count)
-- 2 pages (0.50) -> 0.5 watermark fires once
kv.append_tokens(2, 2)
let c2 = deref(log_count)
-- 3 pages (0.75) -> 0.5 stays latched (no refire), 0.75 fires and preempts seq 1 -> drops back to 2 pages (0.50)!
kv.append_tokens(2, 2)
let c3 = deref(log_count)
let v_after = deref(last_evicted)
let used_after_cb = kv.used_pages()

-- Free seq 2 -> drops to 0.0 (resets hysteresis), then re-allocate 2 pages -> 0.5 watermark MUST fire again!
kv.free_seq(2)
kv.alloc_seq(3, 10)
kv.append_tokens(3, 4)
let c4 = deref(log_count)

kv.clear_watermarks()
kv.free_seq(3)
kv.alloc_seq(4, 10)
kv.append_tokens(4, 4)
let c5 = deref(log_count)

let st = kv.stats()
[str(c1), str(c2), str(c3), str(v_after), str(used_after_cb), str(c4), str(c5), str(st.get("watermark_triggers"))]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("0".to_string()));
    assert_eq!(v[1], Value::String("1".to_string())); // crossed 0.5
    assert_eq!(v[2], Value::String("1".to_string())); // 0.5 latched, did not refire
    assert_eq!(v[3], Value::String("1".to_string())); // 0.75 callback preempted seq 1 without deadlock!
    assert_eq!(v[4], Value::String("2".to_string())); // 3 pages - 1 preempted = 2 pages
    assert_eq!(v[5], Value::String("2".to_string())); // hysteresis reset and refired on second crossing!
    assert_eq!(v[6], Value::String("2".to_string())); // clear_watermarks stopped further triggers
    assert_eq!(v[7], Value::String("3".to_string())); // 2x 0.5 triggers + 1x 0.75 trigger = 3
}

#[test]
fn w4_memory_pool_slab_reuse_quota_trim_watermarks_and_concurrency() {
    let src = r#"
let pool: memory_pool = memory_pool(max_bytes=512, device="cpu")
let wm_hits = atom(0)
pool.on_watermark(0.5, fn(ev)
  pool.trim()
  swap(wm_hits, fn(n) n + 1 end)
end)

-- [8] f64 = 64B -> bucket 64B; [4, 4] f64 = 128B -> bucket 128B
let t1 = pool.zeros([8], "f64")
let t2 = pool.full([4, 4], 3.5, "f64")
let active1 = pool.active_bytes()
let reclaimed1 = pool.release(t1)
let cached1 = pool.cached_bytes()

-- Re-allocate [8] f64 -> MUST hit cached 64B slab with zero OS realloc!
let t3 = pool.full([8], 2.25, "f64")
let v3_live = t3[0]
let st_after_hit = pool.stats()

-- Allocate another 128B tensor -> active = 128 + 64 + 128 = 320B >= 50% of 512B -> triggers watermark!
let t4 = pool.zeros([16], "f64")
let hits_after_wm = deref(wm_hits)

-- Release t2, t3, t4 and trim cached slabs back to OS
pool.release(t2)
pool.release(t3)
pool.release(t4)
-- Reading detached handle t3[0] after release must remain memory-safe (returns 0.0, zero UB)
let v3_detached = t3[0]
let trimmed = pool.trim()

-- Dynamic quota update + concurrent spawn allocation/release safety
pool.set_quota(1024)
let h1 = spawn fn()
  let a = pool.full([8], 7.0, "f64")
  let s = a.sum()
  pool.release(a)
  return s
end
let h2 = spawn fn()
  let b = pool.full([8], 5.0, "f64")
  let s = b.sum()
  pool.release(b)
  return s
end
let s1 = h1.join()
let s2 = h2.join()

[str(active1), str(reclaimed1), str(cached1), str(v3_live), str(st_after_hit.get("pool_hits")), str(st_after_hit.get("pool_misses")), str(hits_after_wm), str(trimmed), str(pool.active_bytes()), str(v3_detached), str(pool.max_bytes()), str(s1), str(s2)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("192".to_string())); // 64 + 128
    assert_eq!(v[1], Value::String("64".to_string()));
    assert_eq!(v[2], Value::String("64".to_string()));
    assert_eq!(v[3], Value::String("2.25".to_string()));
    assert_eq!(v[4], Value::String("1".to_string())); // physical slab hit!
    assert_eq!(v[5], Value::String("2".to_string())); // first two misses
    assert_eq!(v[6], Value::String("1".to_string())); // watermark fired at 320/512 >= 0.5
    assert_eq!(v[7], Value::String("320".to_string())); // 128 + 64 + 128 trimmed
    assert_eq!(v[8], Value::String("0".to_string())); // 0 active after concurrent releases
    assert_eq!(v[9], Value::String("0.0".to_string())); // detached buffer read is memory-safe
    assert_eq!(v[10], Value::String("1024".to_string())); // updated quota
    assert_eq!(v[11], Value::String("56.0".to_string())); // 8 * 7.0
    assert_eq!(v[12], Value::String("40.0".to_string())); // 8 * 5.0

    // Quota overflow must fail closed
    let oom_src = r#"
let p = memory_pool(128)
let a = p.zeros([16], "f64")
let b = p.zeros([8], "f64")
"#;
    let (prog, w) = ParserV3::compile(oom_src).expect("compile");
    assert!(check_program_witnesses(&w).is_empty());
    let mut core = CoreRuntime::default();
    let err = execute_program(Arc::new(prog), &mut core).expect_err("pool quota error expected");
    assert!(
        err.to_string().contains("MemoryPool quota exceeded"),
        "expected MemoryPool quota error, got: {err}"
    );

    // Double release & view release must both be rejected cleanly
    let dbl_src = r#"
let p = memory_pool(256)
let a = p.zeros([8], "f64")
p.release(a)
p.release(a)
"#;
    let (prog2, w2) = ParserV3::compile(dbl_src).expect("compile");
    assert!(check_program_witnesses(&w2).is_empty());
    let mut core2 = CoreRuntime::default();
    let err2 = execute_program(Arc::new(prog2), &mut core2).expect_err("double release error");
    assert!(
        err2.to_string().contains("already released"),
        "expected double-release error, got: {err2}"
    );

    let view_src = r#"
let p = memory_pool(256)
let a = p.zeros([2, 4], "f64")
let vt = a.transpose(0, 1)
p.release(vt)
"#;
    let (prog3, w3) = ParserV3::compile(view_src).expect("compile");
    assert!(check_program_witnesses(&w3).is_empty());
    let mut core3 = CoreRuntime::default();
    let err3 = execute_program(Arc::new(prog3), &mut core3).expect_err("view release error");
    assert!(
        err3.to_string().contains("strided tensor view"),
        "expected view-release error, got: {err3}"
    );
}

#[test]
fn w4_kv_allocator_static_types_and_lsp_completion() {
    assert_typecheck_fails("let k: kv_allocator = 123\n");
    assert_typecheck_fails("let k = kv_allocator(\"bad\", 2, 1, 2)\n");
    assert_typecheck_fails("let k = kv_allocator(2, 2, 1, 2)\nk.alloc_seq(\"bad\")\n");
    assert_typecheck_fails("let k = kv_allocator(2, 2, 1, 2)\nlet s: string = k.utilization()\n");
    assert_typecheck_fails("let k = kv_allocator(2, 2, 1, 2)\nk.set_priority(1, \"high\")\n");
    assert_typecheck_fails("let p: memory_pool = 42\n");
    assert_typecheck_fails("let p = memory_pool(\"bad\")\n");
    assert_typecheck_fails("let p = memory_pool(512)\nlet s: string = p.active_bytes()\n");
    assert_typecheck_fails("let p = memory_pool(512)\np.release(123)\n");

    let mut docs = HashMap::new();
    let uri = "file:///w4_kv.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "let kv = kv_allocator(4, 2, 1, 2)\nlet mp = memory_pool(1024)\n".to_string(),
            version: 1,
            diagnostics: Vec::new(),
        },
    );
    let comp = completion_v3(
        &docs,
        &JsonVal::Object(
            [(
                "textDocument".to_string(),
                JsonVal::Object(
                    [("uri".to_string(), JsonVal::String_(uri.to_string()))]
                        .into_iter()
                        .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        ),
    );
    let labels: Vec<&str> = comp
        .as_array()
        .expect("completion array")
        .iter()
        .filter_map(|item| item.get("label").and_then(|v| v.as_str()))
        .collect();
    for expected in [
        "jvp",
        "trace_dag",
        "compute_dag",
        "kv_allocator",
        "memory_pool",
    ] {
        assert!(
            labels.contains(&expected),
            "LSP completion missing Wave 4 entry {expected:?}"
        );
    }
}
