//! P4-4 Integration & Hardware Verification Suite:
//! - P4-4A: First-class `ProcessGroup` distributed collectives (`all_reduce` sum/mean/max/min,
//!   `all_gather`, `broadcast`, `reduce_scatter`, `barrier`) running cooperatively on M:N VM
//!   fibers across BOTH in-process shared groups (with GPU VRAM residency preservation on live
//!   CUDA/Vulkan hardware when available) AND live `AsyncIoReactor` TCP ring sockets (`MPGR`
//!   framing), with `world_size > worker_threads` proving zero worker-thread blocking.
//! - P4-4B: Unified nanosecond `TraceProfiler` + pure-Mora `std/trace` spanning `ComputeDag.run_profiled`,
//!   GPU `kernel` launches, `KVPageAllocator`, distributed `ProcessGroup` collectives, and live
//!   `std/net/http` `Router` service middleware, exporting valid Chrome Trace Event JSON
//!   (`chrome://tracing` / Perfetto) and structured summaries.

use std::net::TcpListener;
use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::scheduler::TaskScheduler;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn eval_typechecked_with_workers(src: &str, workers: usize) -> Result<Value, String> {
    let witnesses = ParserV3::parse_witnesses(src).map_err(|e| format!("parse error: {e}"))?;
    let errors = check_program_witnesses(&witnesses);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|e| format!("line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let (prog, _) = ParserV3::compile(src).map_err(|e| format!("compile error: {e}"))?;
    let sched = Arc::new(TaskScheduler::with_worker_threads(workers).expect("create scheduler"));
    let mut rt = CoreRuntime::default().with_scheduler(sched);
    let (val, _) =
        execute_program(Arc::new(prog), &mut rt).map_err(|e| format!("runtime error: {e}"))?;
    Ok(val)
}

#[test]
fn p4_4a_inproc_process_group_collectives_and_vram_preservation() {
    // world_size = 4 multiplexed onto just 2 OS worker threads: proves cooperative async
    // collectives NEVER block OS worker threads.
    let src = r#"
task run_inproc_rank(my_pg: process_group, my_r: int): dict<string, any>
  let base = float(my_r + 1)
  let t = Tensor::from_list([base, base * 10.0], "f32")
  let s_sum = my_pg.all_reduce(t, "sum")
  let s_mean = my_pg.all_reduce(t, "mean")
  let s_max = my_pg.all_reduce(t, "max")
  let s_min = my_pg.all_reduce(t, "min")
  let gathered = my_pg.all_gather(t)
  let bcast = my_pg.broadcast(t, 2)
  let shard_in = Tensor::from_list([base, base + 1.0, base + 2.0, base + 3.0], "f32")
  let scattered = my_pg.reduce_scatter(shard_in, "sum")
  my_pg.barrier()
  let out: dict<string, any> = {}
  assign out = out.set("rank", my_pg.rank()).set("sum", s_sum.to_list()).set("mean", s_mean.to_list()).set("max", s_max.to_list()).set("min", s_min.to_list()).set("gathered", gathered.to_list()).set("gathered_shape", gathered.shape()).set("bcast", bcast.to_list()).set("scattered", scattered.to_dtype("f64").to_list()).set("scattered_shape", scattered.shape()).set("dtype", s_sum.dtype())
  return out
end

let pg = process_group(4)
assert_eq(pg.world_size(), 4)
assert_eq(pg.backend(), "inproc")
let ranks = pg.ranks()
assert_eq(ranks.len(), 4)

let handles: list<any> = []
let r = 0
while r < 4
  let h = spawn run_inproc_rank(ranks[r], r)
  assign handles = handles.push(h)
  assign r = r + 1
end

let results: list<any> = []
let i = 0
while i < 4
  assign results = results.push(handles[i].join())
  assign i = i + 1
end

-- Check every rank saw identical reduced/gathered/broadcast values and unique reduce_scatter slices.
let j = 0
while j < 4
  let res = results[j]
  assert_eq(res["rank"], j)
  assert_eq(res["dtype"], "f32")
  assert_eq(res["sum"], [10.0f32, 100.0f32])
  assert_eq(res["mean"], [2.5f32, 25.0f32])
  assert_eq(res["max"], [4.0f32, 40.0f32])
  assert_eq(res["min"], [1.0f32, 10.0f32])
  assert_eq(res["gathered_shape"], [8])
  assert_eq(res["gathered"], [1.0f32, 10.0f32, 2.0f32, 20.0f32, 3.0f32, 30.0f32, 4.0f32, 40.0f32])
  assert_eq(res["bcast"], [3.0f32, 30.0f32])
  assert_eq(res["scattered_shape"], [1])
  assert_eq(res["scattered"], [float(10 + 4 * j)])
  assign j = j + 1
end

let st = pg.stats()
assert_eq(st["all_reduce_calls"], 16)
assert_eq(st["all_gather_calls"], 4)
assert_eq(st["broadcast_calls"], 4)
assert_eq(st["reduce_scatter_calls"], 4)
assert_eq(st["barrier_calls"], 4)
assert(st["bytes_transferred"] > 0)

-- Live GPU VRAM residency preservation check if CUDA/Vulkan/Metal device is present.
let devs = Tensor::devices()
let gpu_dev = "cpu"
let di = 0
while di < devs.len()
  if devs[di] != "cpu" and gpu_dev == "cpu" then
    assign gpu_dev = devs[di]
  end
  assign di = di + 1
end

let gpu_vram_preserved = false
if gpu_dev != "cpu" then
  let pg2 = process_group(2)
  let r2 = pg2.ranks()
  let h0 = spawn fn()
    let t0 = Tensor::from_list([1.0, 2.0, 3.0, 4.0], "f32", gpu_dev)
    let out = r2[0].all_reduce(t0, "sum")
    assert(out.is_vram_resident())
    assert_eq(out.device(), gpu_dev)
    assert_eq(out.dtype(), "f32")
    return out.to_list()
  end
  let h1 = spawn fn()
    let t1 = Tensor::from_list([4.0, 5.0, 6.0, 7.0], "f32", gpu_dev)
    let out = r2[1].all_reduce(t1, "sum")
    assert(out.is_vram_resident())
    assert_eq(out.device(), gpu_dev)
    return out.to_list()
  end
  let v0 = h0.join()
  let v1 = h1.join()
  assert_eq(v0, [5.0f32, 7.0f32, 9.0f32, 11.0f32])
  assert_eq(v1, [5.0f32, 7.0f32, 9.0f32, 11.0f32])
  assign gpu_vram_preserved = true
end

let final_out: dict<string, any> = {}
assign final_out = final_out.set("ok", true).set("gpu_dev", gpu_dev).set("gpu_vram_preserved", gpu_vram_preserved).set("bytes_transferred", st["bytes_transferred"])
final_out
"#;

    let val = eval_typechecked_with_workers(src, 2)
        .unwrap_or_else(|e| panic!("inproc ProcessGroup test failed:\n{e}"));
    let Value::Dict(map) = val else {
        panic!("expected dict result, got {val:?}");
    };
    assert_eq!(map.get("ok"), Some(&Value::Bool(true)));
    if let Some(Value::String(dev)) = map.get("gpu_dev")
        && dev != "cpu"
    {
        assert_eq!(
            map.get("gpu_vram_preserved"),
            Some(&Value::Bool(true)),
            "GPU device {dev} detected but VRAM residency was not preserved across all_reduce"
        );
    }
}

#[test]
fn p4_4a_live_tcp_ring_process_group_collectives() {
    // Reserve 3 free loopback ephemeral ports for the 3-node TCP ring.
    let l0 = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port 0");
    let l1 = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port 1");
    let l2 = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port 2");
    let a0 = l0.local_addr().expect("addr 0").to_string();
    let a1 = l1.local_addr().expect("addr 1").to_string();
    let a2 = l2.local_addr().expect("addr 2").to_string();
    drop((l0, l1, l2));

    // world_size = 3 live TCP ring sockets multiplexed onto just 2 OS worker threads!
    let src = format!(
        r#"
task run_tcp_rank(my_r: int, addrs: list<string>): dict<string, any>
  let pg = tcp_process_group(my_r, 3, addrs)
  assert_eq(pg.rank(), my_r)
  assert_eq(pg.world_size(), 3)
  assert_eq(pg.backend(), "tcp-ring")

  let base = float(my_r + 1)
  let t = Tensor::from_list([base, base * 10.0], "f64")
  let s_sum = pg.all_reduce(t, "sum")
  let s_mean = pg.all_reduce(t, "mean")
  let gathered = pg.all_gather(t)
  let bcast = pg.broadcast(t, 1)
  let shard_in = Tensor::from_list([base, base + 1.0, base + 2.0], "f64")
  let scattered = pg.reduce_scatter(shard_in, "sum")
  pg.barrier()
  let st = pg.stats()
  let out: dict<string, any> = {{}}
  assign out = out.set("rank", my_r).set("sum", s_sum.to_list()).set("mean", s_mean.to_list()).set("gathered", gathered.to_list()).set("bcast", bcast.to_list()).set("scattered", scattered.to_list()).set("bytes", st["bytes_transferred"])
  return out
end

let addrs = ["{a0}", "{a1}", "{a2}"]

let handles: list<any> = []
let r = 0
while r < 3
  let h = spawn run_tcp_rank(r, addrs)
  assign handles = handles.push(h)
  assign r = r + 1
end

let i = 0
let total_bytes = 0
while i < 3
  let res = handles[i].join()
  assert_eq(res["rank"], i)
  assert_eq(res["sum"], [6.0, 60.0])
  assert_eq(res["mean"], [2.0, 20.0])
  assert_eq(res["gathered"], [1.0, 10.0, 2.0, 20.0, 3.0, 30.0])
  assert_eq(res["bcast"], [2.0, 20.0])
  assert_eq(res["scattered"], [float(6 + 3 * i)])
  assert(res["bytes"] > 0)
  assign total_bytes = total_bytes + res["bytes"]
  assign i = i + 1
end

total_bytes
"#
    );

    let val = eval_typechecked_with_workers(&src, 2)
        .unwrap_or_else(|e| panic!("live TCP ring ProcessGroup test failed:\n{e}"));
    match val {
        Value::Int(bytes) => assert!(bytes > 0, "expected >0 bytes transferred across TCP ring"),
        other => panic!("expected total bytes Int, got {other:?}"),
    }
}

#[test]
fn p4_4b_trace_profiler_std_trace_compute_gpu_kv_distributed_and_service() {
    let src = r#"
import "std/trace"
import "std/net/http"

kernel trace_scale_k(x: buf<f32>, out: buf<f32>, n: i32)
  let i = global_id()
  if i < n then
    out[i] = x[i] * 2.0 + 1.0
  end
end

trace_start()
assert(trace_is_active())

-- 1. Compute category: ComputeDag.run_profiled()
let x = Tensor::from_list([1.0, 2.0, 3.0, 4.0], "f64")
let dag = trace_dag(fn(v) (v * 3.0 + 1.0).relu() end, x).fuse()
let prof = dag.run_profiled(x)
let dag_out = prof["output"]
assert_eq(dag_out.to_list(), [4.0, 7.0, 10.0, 13.0])
assert(prof["duration_ns"] >= 0)
assert(len(prof["events"]) >= 2)
let dag_chrome: string = prof["chrome_json"]
let dag_parsed = json.parse(dag_chrome)
assert(len(dag_parsed["traceEvents"]) >= 2)

-- 2. GPU category: custom kernel launch (hardware GPU if available, else CPU fallback)
let kx = Tensor::from_list([1.0, 2.0, 3.0, 4.0], "f32")
let kout = Tensor::zeros([4], "f32")
trace_scale_k.launch(1, 4, kx, kout, 4)
assert_eq(kout.to_list(), [3.0f32, 5.0f32, 7.0f32, 9.0f32])

-- 3. KV-cache category: paged KV allocator token append
let kv = kv_allocator(8, 4, 2, 4)
assert(kv.alloc_seq(1))
let pages = kv.append_tokens(1, 6)
assert_eq(pages.len(), 2)

-- 4. Distributed category: multi-rank ProcessGroup all_reduce (default op = "sum")
let pg = process_group(2)
let ranks = pg.ranks()
let h0 = spawn fn() ranks[0].all_reduce(Tensor::from_list([2.0, 4.0], "f32")) end
let h1 = spawn fn() ranks[1].all_reduce(Tensor::from_list([8.0, 6.0], "f32")) end
let ar0 = h0.join()
let ar1 = h1.join()
assert_eq(ar0.to_list(), [10.0f32, 10.0f32])
assert_eq(ar1.to_list(), [10.0f32, 10.0f32])

-- 5. Service category: live std/net/http Router with trace_http_middleware + trace_span
let app = router().use_middleware(trace_http_middleware()).get("/health", fn(_req: HttpRequest, _s: tcp_stream | tls_stream) response(200, "ok") end)
let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
let srv_task = spawn fn()
  return app.serve_listener(listener, 1)
end
let resp = get(addr, "/health")
assert_eq(resp.status, 200)
assert_eq(resp.body, "ok")
let served = srv_task.join()
assert_eq(served, 1)
listener.close()

let span_res = trace_span("service", "POST /v1/completions", fn() 42 end)
assert_eq(span_res, 42)
let t0 = trace_now_ns()
trace_service_span("GET", "/metrics", 200, t0, 2500)

trace_stop()
assert(!trace_is_active())

let summary = trace_summary()
let cats = summary["categories"]
assert(summary["event_count"] >= 7)
assert(cats["compute"] >= 1)
assert(cats["gpu"] >= 1)
assert(cats["kv_cache"] >= 1)
assert(cats["distributed"] >= 2)
assert(cats["service"] >= 3)

let chrome_json = trace_chrome_json()
let parsed = json.parse(chrome_json)
assert_eq(parsed["displayTimeUnit"], "ms")
assert_eq(len(parsed["traceEvents"]), summary["event_count"])

-- Clean up global profiler buffer before returning
trace_clear()
assert_eq(trace_summary()["event_count"], 0)

let final_out: dict<string, any> = {}
assign final_out = final_out.set("event_count", summary["event_count"]).set("chrome_json", chrome_json).set("compute", cats["compute"]).set("gpu", cats["gpu"]).set("kv_cache", cats["kv_cache"]).set("distributed", cats["distributed"]).set("service", cats["service"])
final_out
"#;

    let val = eval_typechecked_with_workers(src, 2)
        .unwrap_or_else(|e| panic!("std/trace unified profiler test failed:\n{e}"));
    let Value::Dict(map) = val else {
        panic!("expected dict result, got {val:?}");
    };
    let Some(Value::String(chrome)) = map.get("chrome_json") else {
        panic!("expected chrome_json string");
    };
    assert!(chrome.contains("\"traceEvents\""));
    assert!(chrome.contains("\"ph\":\"X\""));
    assert!(chrome.contains("\"cat\":\"compute\""));
    assert!(chrome.contains("\"cat\":\"gpu\""));
    assert!(chrome.contains("\"cat\":\"kv_cache\""));
    assert!(chrome.contains("\"cat\":\"distributed\""));
    assert!(chrome.contains("\"cat\":\"service\""));
    assert!(
        chrome.contains("\"name\":\"GET /health\""),
        "live HTTP middleware span should appear in Chrome Trace JSON: {chrome}"
    );
}

#[test]
fn p4_4_static_typeck_rejects_misused_process_group_and_profiler_apis() {
    let cases = [
        ("let pg: int = process_group(2)\n", "process_group return"),
        (
            "let pg = process_group(2)\nlet bad: int = pg.all_reduce(tensor([1.0]), \"sum\")\n",
            "all_reduce return",
        ),
        (
            "let pg = process_group(2)\nlet bad = pg.broadcast(tensor([1.0]), \"root\")\n",
            "broadcast root type",
        ),
        ("let p: string = profiler()\n", "profiler return"),
        (
            "let p = profiler()\nlet bad: int = p.to_chrome_json()\n",
            "to_chrome_json return",
        ),
    ];
    for (src, label) in cases {
        let witnesses =
            ParserV3::parse_witnesses(src).unwrap_or_else(|e| panic!("{label} parse: {e}"));
        let errs = check_program_witnesses(&witnesses);
        assert!(
            !errs.is_empty(),
            "expected HM typechecker to reject invalid {label} program:\n{src}"
        );
    }
}

#[test]
fn p4_4a_duplicate_rank_fails_fast_instead_of_deadlocking() {
    // Calling a collective concurrently with duplicate rank-0 handles on the same sequence must
    // poison the collective bucket, wake waiting peers, and return a clear runtime error rather
    // than silently hanging the collective barrier.
    let src = r#"
let pg = process_group(2)
let r_a = pg.ranks()
let r_b = pg.ranks()
let h0 = spawn fn() r_a[0].all_reduce(Tensor::from_list([1.0, 2.0], "f32"), "sum") end
let h1 = spawn fn() r_b[0].all_reduce(Tensor::from_list([3.0, 4.0], "f32"), "sum") end
let _a = h0.join()
let _b = h1.join()
"#;
    let err = eval_typechecked_with_workers(src, 2)
        .expect_err("duplicate rank arrival on same seq must fail fast");
    assert!(
        err.contains("duplicate arrival on rank 0"),
        "expected descriptive duplicate rank error, got: {err}"
    );
}
