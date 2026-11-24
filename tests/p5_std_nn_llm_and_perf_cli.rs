//! Phase P5-4 Integration Tests:
//! - P5-4A: Pure-Mora `std/nn/layers`, `std/nn/optim`, `std/nn/tokenizer`, and `std/nn/serving`
//!   (Const-Generic `Linear<IN, OUT>`, RoPE + RMSNorm + GQA + SwiGLU `CausalLM`, PyTree
//!   `value_and_grad` over `list<Tensor>` & `dict<string, Tensor>`, `AdamW` / `SGD` /
//!   `clip_grad_norm` / `cosine_lr_schedule` / `sync_grads_async`, Safetensors & GGUF
//!   checkpoint roundtrips, lossless UTF-8 BPE tokenizer training + GGUF metadata roundtrip,
//!   and live OpenAI HTTP + SSE streaming + RadixAttention prefix reuse + MCP serving).
//! - P5-4B: `mora profile` (Chrome Trace JSON + category summary table) & `mora bench`
//!   (auto-discovery, `--filter`, warmup/iters percentile table).

use std::process::Command;
use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(errs.is_empty(), "unexpected type errors:\n{errs:?}");
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("VM execution failed:\n{e}"));
    val
}

#[test]
fn p5_4a_layers_optim_pytree_autograd_and_checkpoint_roundtrip() {
    let tmp = TempDir::new("p5_4a_ckpt");
    let st_path = tmp
        .path()
        .join("mini_causal_lm.safetensors")
        .to_string_lossy()
        .replace('\\', "/");
    let gguf_path = tmp
        .path()
        .join("mini_causal_lm.gguf")
        .to_string_lossy()
        .replace('\\', "/");

    // 1. Static Const-Generic shape check: `Linear<4, 3>` must REJECT input width 5 at compile time!
    let bad_shape_src = r#"
import "std/nn/layers"
let fc: Linear<4, 3> = Linear<4, 3>(weight = Tensor::ones([4, 3], "f32", "cpu"), bias = Tensor::zeros([3], "f32", "cpu"))
let bad_x: Tensor<f32, [2, 5], cpu> = Tensor::ones([2, 5], "f32", "cpu")
let _y = fc.forward(bad_x)
"#;
    let (_, bad_witnesses) = ParserV3::compile(bad_shape_src).expect("parse bad linear");
    let bad_errs = check_program_witnesses(&bad_witnesses);
    assert!(
        !bad_errs.is_empty(),
        "Linear<4, 3>.forward(Tensor<f32, [2, 5]>) must fail static shape check"
    );
    let joined = format!("{bad_errs:?}");
    assert!(
        joined.contains("Shape")
            || joined.contains("shape")
            || joined.contains("4")
            || joined.contains("5")
            || joined.contains("Unification"),
        "expected static shape mismatch diagnostic, got: {joined}"
    );

    // 2. Full PyTree autograd (`list<Tensor>` & `dict<string, Tensor>`), CausalLM training with
    //    AdamW + clip_grad_norm + cosine_lr_schedule + SGD + sync_grads_async, and
    //    Safetensors + GGUF save/load parity + KV-cached generation parity!
    let src = format!(
        r#"
import "std/nn/layers"
import "std/nn/optim"

-- Verify Const-Generic `Linear<4, 3>` static shape-checked forward pass
let fc: Linear<4, 3> = Linear<4, 3>(weight = Tensor::ones([4, 3], "f32", "cpu"), bias = Tensor::zeros([3], "f32", "cpu"))
let x_ok: Tensor<f32, [2, 4], cpu> = Tensor::ones([2, 4], "f32", "cpu")
let y_ok = fc.forward(x_ok)
assert_eq(y_ok.shape(), [2, 3])
assert_eq(fc.dims(), [4, 3])

-- Verify PyTree `grad` on `dict<string, Tensor>`
let dict_loss = fn(d: dict<string, any>)
  let a: any = d.get("a")
  let b: any = d.get("b")
  return (a * a + b * 3.0f32).sum()
end
let d_in: dict<string, any> = {{"a": Tensor::full([2], 4.0, "f32"), "b": Tensor::full([2], 2.0, "f32")}}
let d_grads = grad(dict_loss)(d_in)
let ga = d_grads.get("a").to_list()
let gb = d_grads.get("b").to_list()
assert(math.abs(float(ga[0]) - 8.0) < 0.001, "d/da(a^2) = 2a = 8")
assert(math.abs(float(gb[0]) - 3.0) < 0.001, "d/db(3b) = 3")

-- Build a mini Llama/Qwen CausalLM (RoPE + RMSNorm + GQA 4Q/2KV + SwiGLU)
let cfg = ModelConfig(vocab_size = 16, d_model = 16, n_layers = 1, n_heads = 4, n_kv_heads = 2, d_ff = 32, dtype = "f32", device = "cpu")
let model = causal_lm_init(cfg, 0.15)
let input_ids: list<int> = [1, 2, 3, 4]
let target_ids: list<int> = [2, 3, 4, 5]

-- Train for 12 steps with PyTree `value_and_grad(loss_fn)(params)` + AdamW + clip_grad_norm + cosine LR
let params = model.parameters()
let opt = adamw_new(params, 0.05, 0.9, 0.999, 0.00000001, 0.001)
let first_loss = 0.0
let last_loss = 0.0
let step = 0
while step < 12
  let loss_fn = fn(ps: list<any>)
    let m = model.with_parameters(ps)
    let logits = m.forward(input_ids)
    return cross_entropy_loss(logits, target_ids)
  end
  let vg = value_and_grad(loss_fn)(params)
  let loss_val = float(vg[0])
  let grads = clip_grad_norm(vg[1], 5.0)
  if step == 0 then
    assign first_loss = loss_val
  end
  assign last_loss = loss_val
  let lr = cosine_lr_schedule(step, 2, 12, 0.06, 0.01)
  let upd = opt.step(params, grads, lr)
  assign params = upd.params
  assign opt = upd.state
  assign step = step + 1
end

assign model = model.with_parameters(params)
assert(last_loss < first_loss * 0.65, "AdamW training must significantly reduce cross-entropy loss")
let trained_logits = model.forward(input_ids)
assert_eq(argmax_last_row(trained_logits), 5)

-- Exercise SGD (with momentum + weight_decay) on a small parameter pair
let sgd_p = [Tensor::full([2], 3.0, "f32")]
let sgd_g = [Tensor::full([2], 1.0, "f32")]
let sgd_opt = sgd_new(sgd_p, 0.1, 0.9, 0.01)
let sgd_res = sgd_opt.step(sgd_p, sgd_g)
let sgd_after = float(sgd_res.params[0].to_list()[0])
assert(sgd_after < 2.95 and sgd_after > 2.8, "SGD step must move parameter against gradient")

-- Exercise distributed `sync_grads_async` across 2 ranks (inproc ProcessGroup)
let pgs = process_group(2).ranks()
let h0 = spawn fn()
  let synced = sync_grads_async(pgs[0], [Tensor::full([4], 2.0, "f32"), Tensor::full([2], 10.0, "f32")], "mean")
  return [float(synced[0].to_list()[0]), float(synced[1].to_list()[0])]
end
let h1 = spawn fn()
  let synced = sync_grads_async(pgs[1], [Tensor::full([4], 6.0, "f32"), Tensor::full([2], 20.0, "f32")], "mean")
  return [float(synced[0].to_list()[0]), float(synced[1].to_list()[0])]
end
let r0 = h0.join()
let r1 = h1.join()
assert(math.abs(r0[0] - 4.0) < 0.001 and math.abs(r1[0] - 4.0) < 0.001)
assert(math.abs(r0[1] - 15.0) < 0.001 and math.abs(r1[1] - 15.0) < 0.001)

-- Save trained CausalLM to BOTH Safetensors and GGUF, reload, and verify exact logit parity!
import "std/nn/safetensors"
import "std/nn/gguf"
let st_res = model.save_safetensors("{st_path}")
let st_bytes = match st_res with
  SafetensorsResult::Ok(n) -> n
  SafetensorsResult::Err(_) -> 0
end
assert(st_bytes > 0)
let gguf_res = model.save_gguf("{gguf_path}")
let gguf_bytes = match gguf_res with
  GgufResult::Ok(n) -> n
  GgufResult::Err(_) -> 0
end
assert(gguf_bytes > 0)

let model_st = causal_lm_from_safetensors("{st_path}", cfg)
let model_gguf = causal_lm_from_gguf("{gguf_path}", cfg)
let logits_st = model_st.forward(input_ids)
let logits_gguf = model_gguf.forward(input_ids)
let diff_st = float((trained_logits - logits_st).abs().max())
let diff_gguf = float((trained_logits - logits_gguf).abs().max())
assert(diff_st < 0.00001, "Safetensors reloaded CausalLM logits must match original")
assert(diff_gguf < 0.00001, "GGUF reloaded CausalLM logits must match original")

-- Verify KV-cached `generate_greedy` matches uncached `generate_greedy` token-for-token!
let kv = kv_allocator(32, 4, cfg.n_kv_heads, cfg.d_model / cfg.n_heads, "f32", "cpu")
let gen_uncached = model.generate_greedy([1, 2, 3, 4], 4, -1, nil, 1)
let gen_cached = model.generate_greedy([1, 2, 3, 4], 4, -1, kv, 1)
assert_eq(gen_cached, gen_uncached)
assert(kv.match_prefix([1, 2, 3, 4]) == 4, "prompt must now be cached in RadixAttention trie")

[first_loss, last_loss, st_bytes, gguf_bytes]
"#
    );

    let out = compile_check_and_run(&src);
    let Value::List(items) = out else {
        panic!("expected list result, got {out:?}");
    };
    let v = items.to_vec();
    match (&v[0], &v[1]) {
        (Value::Float(first), Value::Float(last)) => {
            assert!(
                *last < *first * 0.65,
                "expected loss reduction: first={first}, last={last}"
            );
            println!("CausalLM training verified: loss {first:.4} -> {last:.4}");
        }
        other => panic!("unexpected loss tuple: {other:?}"),
    }
}

#[test]
fn p5_4a_bpe_tokenizer_train_utf8_and_gguf_roundtrip() {
    let tmp = TempDir::new("p5_4a_bpe");
    let gguf_path = tmp
        .path()
        .join("bpe_tok.gguf")
        .to_string_lossy()
        .replace('\\', "/");

    let src = format!(
        r#"
import "std/nn/tokenizer"
import "std/nn/gguf"

let corpus = ["mora language compiles tensors and graphs fast. ", "mora language runs radix attention and bpe tokenization. ", "mora language serves openai streaming and mcp tools. ", "mora language trains transformer models end to end. "]
let tok = train_bpe(corpus, 24)
assert(tok.merges.len() > 0, "train_bpe must learn merges")
assert(tok.vocab_size() == 259 + tok.merges.len())

let sample = "mora language trains tensors fast!"
let ids = tok.encode(sample, false, false)
-- Learned merges on repeated "mora language " must strictly compress below raw UTF-8 byte count!
assert(ids.len() < sample.len(), "BPE merges must compress repeated corpus text")
assert_eq(tok.decode(ids, true), sample)

-- Lossless UTF-8 roundtrip on ASCII + punctuation + CJK + emoji!
let utf8_text = "Hello, Mora! 你好世界 — AI-native 语言 🚀\nLine 2."
let utf8_ids = tok.encode(utf8_text, true, true)
assert_eq(utf8_ids[0], tok.bos_id)
assert_eq(utf8_ids[utf8_ids.len() - 1], tok.eos_id)
assert_eq(tok.decode(utf8_ids, true), utf8_text)

-- Export BPE vocab & merges into GGUF metadata, save, reload, and verify exact encode/decode parity!
let meta = tok.to_gguf_metadata()
let dummy_tensors: dict<string, any> = {{"dummy": Tensor::zeros([2], "f32")}}
let save_res = gguf_save("{gguf_path}", dummy_tensors, meta)
let saved_ok = match save_res with
  GgufResult::Ok(n) -> n > 0
  GgufResult::Err(_) -> false
end
assert(saved_ok, "gguf_save with BPE metadata must succeed")

let load_res = gguf_load("{gguf_path}", "cpu")
let empty_meta: dict<string, any> = {{}}
let loaded_meta = match load_res with
  GgufResult::Ok(f) -> f.header.metadata
  GgufResult::Err(_) -> empty_meta
end
let tok2 = from_gguf_metadata(loaded_meta)
assert_eq(tok2.vocab_size(), tok.vocab_size())
assert_eq(tok2.merges.len(), tok.merges.len())
assert_eq(tok2.encode(sample, false, false), ids)
assert_eq(tok2.encode(utf8_text, true, true), utf8_ids)
assert_eq(tok2.decode(tok2.encode(utf8_text, true, true), true), utf8_text)

[tok.merges.len(), ids.len(), sample.len()]
"#
    );

    let out = compile_check_and_run(&src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    println!(
        "BPE verified: merges={:?}, compressed_tokens={:?} < raw_bytes={:?}",
        v[0], v[1], v[2]
    );
}

#[test]
fn p5_4a_serving_http_sse_radix_attention_and_mcp() {
    let src = r#"
import "std/nn/layers"
import "std/nn/tokenizer"
import "std/nn/serving"
import "std/net/http"
import "std/mcp"
import "std/trace"

trace_clear()
trace_start()

let tok = byte_level_base_tokenizer()
let cfg = ModelConfig(vocab_size = tok.vocab_size(), d_model = 16, n_layers = 1, n_heads = 2, n_kv_heads = 2, d_ff = 32, dtype = "f32", device = "cpu")
let model = causal_lm_init(cfg, 0.08)
let kv = kv_allocator(64, 4, cfg.n_kv_heads, cfg.d_model / cfg.n_heads, "f32", "cpu")
let engine = create_serving_engine(model, tok, kv, "mora-mini-1L")
let r = build_http_router(engine)

let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
let srv_handle = spawn r.serve_listener(listener, 5)

-- 1. GET /health
let health_resp = get(addr, "/health")
assert_eq(health_resp.status, 200)
let health_json = json.parse(health_resp.body)
assert_eq(str(health_json.get("status")), "ok")
assert_eq(str(health_json.get("model")), "mora-mini-1L")

-- 2. GET /v1/models
let models_resp = get(addr, "/v1/models")
assert_eq(models_resp.status, 200)
let models_json = json.parse(models_resp.body)
assert_eq(str(models_json.get("object")), "list")
assert_eq(str(models_json.get("data")[0].get("id")), "mora-mini-1L")

-- 3. POST /v1/chat/completions (non-streaming JSON) — warms RadixAttention with shared system prompt!
let req1_body = "{\"model\":\"mora-mini-1L\",\"messages\":[{\"role\":\"system\",\"content\":\"You are Mora AI assistant.\"},{\"role\":\"user\",\"content\":\"Hi\"}],\"max_tokens\":4,\"stream\":false}"
let comp_resp = post(addr, "/v1/chat/completions", req1_body)
assert_eq(comp_resp.status, 200)
let comp_json = json.parse(comp_resp.body)
assert_eq(str(comp_json.get("object")), "chat.completion")
assert_eq(str(comp_json.get("choices")[0].get("message").get("role")), "assistant")
assert(int(str(comp_json.get("usage").get("completion_tokens"))) > 0)

-- 4. POST /v1/chat/completions (real-time SSE streaming) sharing the EXACT same system prompt prefix!
let req2_body = "{\"model\":\"mora-mini-1L\",\"messages\":[{\"role\":\"system\",\"content\":\"You are Mora AI assistant.\"},{\"role\":\"user\",\"content\":\"Go\"}],\"max_tokens\":4,\"stream\":true}"
let sse = open_sse_post(addr, "/v1/chat/completions", req2_body)
let sse_chunks = 0
let saw_done = false
while true
  let ev = sse_read_event(sse)
  if ev.data == "" then
    break
  end
  if ev.data == "[DONE]" then
    assign saw_done = true
    break
  end
  let obj = json.parse(ev.data)
  assert_eq(str(obj.get("object")), "chat.completion.chunk")
  assign sse_chunks = sse_chunks + 1
end
sse.close()
assert(saw_done, "SSE stream must terminate with [DONE]")
assert(sse_chunks >= 2, "SSE stream must emit token chunks plus stop chunk")

-- 5. GET /metrics — must observe 2 served requests & RadixAttention prefix cache hits!
let met_resp = get(addr, "/metrics")
assert_eq(met_resp.status, 200)
let met_json = json.parse(met_resp.body)
assert_eq(int(str(met_json.get("requests_served"))), 2)
assert(int(str(met_json.get("prefix_cache_hits"))) >= 1, "Turn 2 must hit RadixAttention prefix cache")
assert(int(str(met_json.get("prefix_hit_tokens"))) >= 4, "Shared system prompt prefix must match >= 4 tokens")

let served_count = srv_handle.join()
assert_eq(served_count, 5)
listener.close()

-- 6. MCP server tools (`generate_completion` & `serving_stats`)
let mcp_srv = build_mcp_server(engine)
let list_resp_str = mcp_srv.handle_jsonrpc("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}")
let list_resp = json.parse(list_resp_str)
let tools = list_resp.get("result").get("tools")
assert_eq(tools.len(), 2)

let call_resp_str = mcp_srv.handle_jsonrpc("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"generate_completion\",\"arguments\":{\"prompt\":\"system: You are Mora AI assistant.\\nuser: MCP\",\"max_tokens\":3}}}")
let call_resp = json.parse(call_resp_str)
assert(call_resp.get("result") != nil, "MCP generate_completion must return result")

let stats_resp_str = mcp_srv.handle_jsonrpc("{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"serving_stats\",\"arguments\":{}}}")
let stats_resp = json.parse(stats_resp_str)
assert(stats_resp.get("result") != nil, "MCP serving_stats must return result")

-- 7. Verify `std/trace` captured HTTP `service` spans via `trace_http_middleware()`
let tr = trace_summary()
trace_stop()
assert(int(str(tr.get("event_count"))) >= 5, "trace_http_middleware must record HTTP request spans")
let svc_cnt = tr.get("categories").get("service")
assert(svc_cnt != nil and int(str(svc_cnt)) >= 5)

engine.shutdown()
[int(str(met_json.get("prefix_cache_hits"))), int(str(met_json.get("prefix_hit_tokens"))), sse_chunks]
"#;

    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    println!(
        "Serving HTTP+SSE+RadixAttention+MCP verified: prefix_hits={:?}, hit_tokens={:?}, sse_chunks={:?}",
        v[0], v[1], v[2]
    );
}

#[test]
fn p5_4b_cli_profile_and_bench_subcommands() {
    let bin = env!("CARGO_BIN_EXE_mora");
    let tmp = TempDir::new("p5_4b_cli");

    // 1. Test `mora profile <script.mora> --out <trace.json> --summary`
    let prof_script = tmp.path().join("workload.mora");
    let trace_out = tmp.path().join("trace.json");
    std::fs::write(
        &prof_script,
        r#"
import "std/trace"

let f = fn(x: Tensor<f32>, w: Tensor<f32>) (x @ w).relu() end
let x = Tensor::ones([4, 8], "f32")
let w = Tensor::ones([8, 4], "f32")
let dag = trace_dag(f, x, w).fuse()
let _y = dag.run(x, w)

let kv = kv_allocator(8, 4, 2, 4, "f32", "cpu")
kv.alloc_seq(1)
kv.append_tokens(1, 2)
kv.free_seq(1)

let _v = trace_span("service", "custom_work", fn() 42 end)
"#,
    )
    .expect("write profile workload");

    let prof_out = Command::new(bin)
        .args([
            "profile",
            prof_script.to_str().expect("utf8 path"),
            "--out",
            trace_out.to_str().expect("utf8 path"),
            "--summary",
        ])
        .output()
        .expect("run mora profile");
    assert!(
        prof_out.status.success(),
        "mora profile failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&prof_out.stdout),
        String::from_utf8_lossy(&prof_out.stderr)
    );
    let prof_stdout = String::from_utf8_lossy(&prof_out.stdout);
    assert!(
        prof_stdout.contains("CATEGORY") && prof_stdout.contains("service"),
        "expected profile summary table, got:\n{prof_stdout}"
    );
    assert!(
        trace_out.exists(),
        "mora profile --out must write Chrome Trace JSON file"
    );
    let trace_json = std::fs::read_to_string(&trace_out).expect("read trace.json");
    assert!(
        trace_json.contains("\"traceEvents\"") && trace_json.contains("\"custom_work\""),
        "trace.json must contain Chrome Trace JSON traceEvents, got:\n{trace_json}"
    );

    // 2. Test `mora bench <dir> --filter matmul --iters 5 --warmup 1`
    let bench_dir = tmp.path().join("benches");
    std::fs::create_dir_all(&bench_dir).expect("create bench dir");
    std::fs::write(
        bench_dir.join("tensor_bench.mora"),
        r#"
let A = Tensor::ones([8, 8], "f32")
let B = Tensor::ones([8, 8], "f32")

task bench_matmul_8x8()
  let _c = A @ B
  return nil
end

task bench_add_8x8()
  let _c = A + B
  return nil
end
"#,
    )
    .expect("write tensor_bench.mora");

    let bench_out = Command::new(bin)
        .args([
            "bench",
            bench_dir.to_str().expect("utf8 path"),
            "--filter",
            "matmul",
            "--iters",
            "5",
            "--warmup",
            "1",
        ])
        .output()
        .expect("run mora bench");
    assert!(
        bench_out.status.success(),
        "mora bench failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&bench_out.stdout),
        String::from_utf8_lossy(&bench_out.stderr)
    );
    let bench_stdout = String::from_utf8_lossy(&bench_out.stdout);
    assert!(
        bench_stdout.contains("BENCHMARK")
            && bench_stdout.contains("MIN(us)")
            && bench_stdout.contains("P50(us)")
            && bench_stdout.contains("P95(us)")
            && bench_stdout.contains("OPS/SEC"),
        "expected bench percentile header, got:\n{bench_stdout}"
    );
    assert!(
        bench_stdout.contains("bench_matmul_8x8"),
        "expected filtered benchmark bench_matmul_8x8 to run, got:\n{bench_stdout}"
    );
    assert!(
        !bench_stdout.contains("bench_add_8x8"),
        "bench_add_8x8 should have been filtered out by --filter matmul, got:\n{bench_stdout}"
    );
    println!("mora profile:\n{prof_stdout}\nmora bench:\n{bench_stdout}");
}
