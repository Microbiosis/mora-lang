//! Wave 4 Step 2 verification: Dynamic VRAM / KV-Cache Resource Orchestration (`KVPageAllocator`)
//! - Physical page allocation, multi-sequence page tables, hardware-packed DType byte accounting
//! - Zero-copy prefix sharing (`fork_seq`) + Copy-on-Write split on `write_kv`
//! - OOM detection, `free_seq` page reclamation, `evict_lru`, and concurrent `spawn`ed workers
//! - Static HM types (`kv_allocator`) and LSP completion parity

use std::collections::HashMap;
use std::sync::Arc;

use mora::lsp::json::Value as JsonVal;
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
fn w4_kv_allocator_static_types_and_lsp_completion() {
    assert_typecheck_fails("let k: kv_allocator = 123\n");
    assert_typecheck_fails("let k = kv_allocator(\"bad\", 2, 1, 2)\n");
    assert_typecheck_fails("let k = kv_allocator(2, 2, 1, 2)\nk.alloc_seq(\"bad\")\n");
    assert_typecheck_fails("let k = kv_allocator(2, 2, 1, 2)\nlet s: string = k.utilization()\n");

    let mut docs = HashMap::new();
    let uri = "file:///w4_kv.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "let kv = kv_allocator(4, 2, 1, 2)\n".to_string(),
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
    for expected in ["jvp", "trace_dag", "compute_dag", "kv_allocator"] {
        assert!(
            labels.contains(&expected),
            "LSP completion missing Wave 4 entry {expected:?}"
        );
    }
}
