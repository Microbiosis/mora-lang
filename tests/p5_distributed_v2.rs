//! P5-3 Integration & Live Network Verification Suite:
//! - Bandwidth-optimal 2-phase TCP Ring AllReduce (Scatter-Reduce + AllGather) proving `2*(W-1)/W*S` wire traffic
//! - Orthogonal 2D sub-communicators (`pg.split(color, key)`) for TP x DP meshes on both InProc & live TCP
//! - Point-to-point Pipeline Parallelism (`pg.send` / `pg.recv`) 3-stage 1F1B forward + backward on InProc & TCP
//! - Overlapped compute & communication via `pg.all_reduce_async(tensor, op?) -> TaskHandle<Tensor>`.

use std::net::TcpListener;
use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn check_source(src: &str) -> Result<(), String> {
    let witnesses = ParserV3::parse_witnesses(src).map_err(|e| format!("parse error: {e}"))?;
    let errors = check_program_witnesses(&witnesses);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors
            .iter()
            .map(|e| format!("line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn eval_source(src: &str) -> Result<Value, String> {
    check_source(src)?;
    let (prog, _witnesses) = ParserV3::compile(src).map_err(|e| format!("compile error: {e}"))?;
    let mut rt = CoreRuntime::default();
    let (val, _globals) =
        execute_program(Arc::new(prog), &mut rt).map_err(|e| format!("runtime error: {e}"))?;
    Ok(val)
}

fn reserve_free_addrs(n: usize) -> Vec<String> {
    let listeners: Vec<TcpListener> = (0..n)
        .map(|_| TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port"))
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().expect("local_addr").to_string())
        .collect()
}

#[test]
fn p5_3_tcp_ring_2phase_all_reduce_bandwidth_optimality_4_ranks() {
    let addrs = reserve_free_addrs(4);
    let addrs_mora = format!(
        "[{}]",
        addrs
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // 4-rank TCP ring over 1024-element f32 tensor (S = 4096 bytes):
    // 2-phase Scatter-Reduce + AllGather sends exactly 2 * (3/4) * 4096 + 6*21 = 6270 bytes/rank
    // vs naive (W-1)*S = 12288 bytes/rank (or 24576 bytes in f64).
    // Also verifies `mean`, `max`, `min`, and uneven non-divisible length 1027 f64!
    let src = format!(
        r#"
let addrs = {addrs_mora}
let handles = []
let r = 0
while r < 4
  let my_r = r
  let h = spawn fn()
    let pg = tcp_process_group(my_r, 4, addrs)
    let t_f32 = Tensor::full([1024], (my_r + 1) * 1.0, "f32")
    let sum_f32 = pg.all_reduce(t_f32, "sum")
    let st_after_one = pg.stats()
    let wire_sent_one = st_after_one["wire_bytes_sent"]
    let wire_recv_one = st_after_one["wire_bytes_recv"]

    let mean_f32 = pg.all_reduce(t_f32, "mean")
    let max_f32 = pg.all_reduce(t_f32, "max")
    let min_f32 = pg.all_reduce(t_f32, "min")

    -- Uneven length 1027 (not divisible by 4!) in f64
    let t_odd = Tensor::full([1027], (my_r + 1) * 2.0, "f64")
    let odd_sum = pg.all_reduce(t_odd, "sum")

    assert_eq(sum_f32.to_list()[0], 10.0f32)
    assert_eq(sum_f32.to_list()[1023], 10.0f32)
    assert_eq(mean_f32.to_list()[512], 2.5f32)
    assert_eq(max_f32.to_list()[777], 4.0f32)
    assert_eq(min_f32.to_list()[333], 1.0f32)
    assert_eq(odd_sum.to_list()[0], 20.0)
    assert_eq(odd_sum.to_list()[1026], 20.0)
    return [wire_sent_one, wire_recv_one]
  end
  handles = handles.push(h)
  r = r + 1
end

let out = []
for h in handles
  out = out.push(h.join())
end
out
"#
    );

    let val = eval_source(&src).expect("4-rank 2-phase TCP ring all_reduce");
    let Value::List(per_rank) = val else {
        panic!("expected per-rank wire stats list, got {val}");
    };
    assert_eq!(per_rank.len(), 4);
    for (rank, entry) in per_rank.iter().enumerate() {
        let Value::List(pair) = entry else {
            panic!("expected [sent, recv]");
        };
        let Value::Int(sent) = pair[0] else {
            panic!("expected int sent");
        };
        let Value::Int(recv) = pair[1] else {
            panic!("expected int recv");
        };
        // Exact theoretical 2-phase wire traffic: 6 hops * (21B header + 256*4B chunk) = 6270 bytes!
        assert_eq!(
            sent, 6270,
            "rank {rank}: expected exact 2-phase bandwidth-optimal wire_bytes_sent=6270 (1.5*S + 6*21), got {sent}"
        );
        assert_eq!(recv, 6270, "rank {rank}: wire_bytes_recv mismatch");
    }
}

#[test]
fn p5_3_orthogonal_2d_split_tp_x_dp_mesh_inproc_and_tcp() {
    // 1. InProc 2x2 TP x DP mesh:
    //    World ranks: 0, 1, 2, 3
    //    TP groups (color = r / 2): {0, 1} and {2, 3}
    //    DP groups (color = r % 2): {0, 2} and {1, 3}
    let inproc_src = r#"
let pg = process_group(4)
let ranks = pg.ranks()
let handles = []
let r = 0
while r < 4
  let my_r = r
  let my_pg = ranks[r]
  let h = spawn fn()
    let tp_pg = my_pg.split(my_r / 2, my_r % 2)
    let dp_pg = my_pg.split(my_r % 2, my_r / 2)
    assert_eq(tp_pg.world_size(), 2)
    assert_eq(dp_pg.world_size(), 2)
    assert_eq(tp_pg.rank(), my_r % 2)
    assert_eq(dp_pg.rank(), my_r / 2)

    -- Initial local value = (my_r + 1): [1, 2, 3, 4]
    let x = Tensor::full([4], (my_r + 1) * 1.0, "f64")
    -- TP sum: {0,1} -> 1+2=3, {2,3} -> 3+4=7
    let after_tp = tp_pg.all_reduce(x, "sum")
    -- DP mean: {0,2} -> (3+7)/2=5, {1,3} -> (3+7)/2=5
    let after_dp = dp_pg.all_reduce(after_tp, "mean")
    assert_eq(after_tp.to_list()[0], if my_r < 2 then 3.0 else 7.0 end)
    return after_dp.to_list()[0]
  end
  handles = handles.push(h)
  r = r + 1
end

let vals = []
for h in handles
  vals = vals.push(h.join())
end
vals
"#;
    let res = eval_source(inproc_src).expect("InProc 2x2 TP x DP split mesh");
    assert_eq!(
        res,
        Value::List(
            vec![
                Value::Float(5.0),
                Value::Float(5.0),
                Value::Float(5.0),
                Value::Float(5.0),
            ]
            .into()
        )
    );

    // 2. Live TCP 2x2 TP x DP mesh (each sub-group spins up its own independent ephemeral TCP sub-ring!).
    let addrs = reserve_free_addrs(4);
    let addrs_mora = format!(
        "[{}]",
        addrs
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let tcp_src = format!(
        r#"
let addrs = {addrs_mora}
let handles = []
let r = 0
while r < 4
  let my_r = r
  let h = spawn fn()
    let world = tcp_process_group(my_r, 4, addrs)
    let tp = world.split(my_r / 2, my_r % 2)
    let dp = world.split(my_r % 2, my_r / 2)
    let x = Tensor::full([16], (my_r + 1) * 1.0, "f32")
    let s_tp = tp.all_reduce(x, "sum")
    let s_dp = dp.all_reduce(s_tp, "mean")
    assert_eq(tp.backend(), "tcp-ring")
    assert_eq(dp.backend(), "tcp-ring")
    assert_eq(s_dp.to_list()[0], 5.0f32)
    return s_dp.to_list()[15]
  end
  handles = handles.push(h)
  r = r + 1
end
let vals = []
for h in handles
  vals = vals.push(h.join())
end
vals
"#
    );
    let tcp_res = eval_source(&tcp_src).expect("Live TCP 2x2 TP x DP split mesh");
    let Value::List(items) = tcp_res else {
        panic!("expected 4 f32 results, got {tcp_res}");
    };
    assert_eq!(items.len(), 4);
    for item in items {
        let v = match item {
            Value::Scalar(s) => s.as_f64(),
            Value::Float(f) => f,
            other => panic!("expected f32, got {other}"),
        };
        assert!((v - 5.0).abs() < 1e-5);
    }
}

#[test]
fn p5_3_p2p_3stage_pipeline_1f1b_inproc_and_tcp() {
    // 3-stage Pipeline Parallelism:
    // Forward: Stage 0 (x + 1) -> Stage 1 (x * 2) -> Stage 2 (x + 10)
    // Backward: Stage 2 grad (g = 1.0) -> Stage 1 (g * 2) -> Stage 0 (g + 0.5)
    // Run 3 microbatches cleanly on BOTH InProc and live TCP sockets!
    let inproc_pipe = r#"
let pg = process_group(3)
let ranks = pg.ranks()
let r0 = ranks[0]
let r1 = ranks[1]
let r2 = ranks[2]

let h0 = spawn fn()
  let grads = []
  let mb = 0
  while mb < 3
    let act0 = Tensor::full([2, 4], (mb + 1) * 1.0, "f32") + 1.0f32
    r0.send(act0, 1)
    let g = r0.recv(1)
    grads = grads.push((g + 0.5f32).to_list()[0][0])
    mb = mb + 1
  end
  return grads
end

let h1 = spawn fn()
  let mb = 0
  while mb < 3
    let inp = r1.recv(0)
    let act1 = inp * 2.0f32
    r1.send(act1, 2)
    let g_up = r1.recv(2)
    r1.send(g_up * 2.0f32, 0)
    mb = mb + 1
  end
  return 0
end

let h2 = spawn fn()
  let outs = []
  let mb = 0
  while mb < 3
    let inp = r2.recv(1)
    let out = inp + 10.0f32
    outs = outs.push(out.to_list()[0][0])
    r2.send(Tensor::full([2, 4], (mb + 1) * 1.0, "f32"), 1)
    mb = mb + 1
  end
  return outs
end

let g0 = h0.join()
let _ = h1.join()
let f2 = h2.join()
-- Forward outputs for mb=0,1,2: ((1+1)*2+10)=14, ((2+1)*2+10)=16, ((3+1)*2+10)=18
assert_eq(f2[0], 14.0f32)
assert_eq(f2[1], 16.0f32)
assert_eq(f2[2], 18.0f32)
-- Backward grads arriving at stage 0: 1*2+0.5=2.5, 2*2+0.5=4.5, 3*2+0.5=6.5
assert_eq(g0[0], 2.5f32)
assert_eq(g0[1], 4.5f32)
assert_eq(g0[2], 6.5f32)
f2
"#;
    eval_source(inproc_pipe).expect("InProc 3-stage pipeline P2P");

    let addrs = reserve_free_addrs(3);
    let addrs_mora = format!(
        "[{}]",
        addrs
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let tcp_pipe = format!(
        r#"
let addrs = {addrs_mora}
let h0 = spawn fn()
  let pg = tcp_process_group(0, 3, addrs)
  let act0 = Tensor::full([8], 3.0, "f32")
  pg.send(act0, 1)
  -- Direct skip-stage recv from rank 2 proving arbitrary (src, dst) P2P routing!
  let back = pg.recv(2)
  let st = pg.stats()
  assert_eq(st["send_calls"], 1)
  assert_eq(st["recv_calls"], 1)
  return back.to_list()[0]
end

let h1 = spawn fn()
  let pg = tcp_process_group(1, 3, addrs)
  let x = pg.recv(0)
  pg.send(x * 4.0f32, 2)
  return 0
end

let h2 = spawn fn()
  let pg = tcp_process_group(2, 3, addrs)
  let x = pg.recv(1)
  pg.send(x + 5.0f32, 0)
  return 0
end

let final_v = h0.join()
let _ = h1.join()
let _ = h2.join()
assert_eq(final_v, 17.0f32)
final_v
"#
    );
    eval_source(&tcp_pipe).expect("Live TCP 3-stage pipeline P2P");
}

#[test]
fn p5_3_overlapped_all_reduce_async_with_foreground_compute() {
    let src = r#"
let pg = process_group(4)
let ranks = pg.ranks()
let handles = []
let r = 0
while r < 4
  let my_r = r
  let my_pg = ranks[r]
  let h = spawn fn()
    let g_layer2 = Tensor::full([4, 4], (my_r + 1) * 2.0, "f32")
    -- Launch non-blocking collective returning TaskHandle<Tensor>!
    let comm_handle = my_pg.all_reduce_async(g_layer2, "mean")

    -- Overlap foreground compute while Layer 2 gradients reduce in the background
    let a = Tensor::full([8, 8], 1.5, "f32")
    let b = Tensor::full([8, 8], 0.5, "f32")
    let g_layer1 = a @ b

    -- Join overlapped collective handle
    let synced_layer2 = comm_handle.join()
    -- Mean of [2, 4, 6, 8] = 5.0; (8 * 1.5 * 0.5) = 6.0
    assert_eq(synced_layer2.to_list()[0][0], 5.0f32)
    assert_eq(g_layer1.to_list()[0][0], 6.0f32)
    return synced_layer2.to_list()[0][0]
  end
  handles = handles.push(h)
  r = r + 1
end

let results = []
for h in handles
  results = results.push(h.join())
end
assert_eq(len(results), 4)
results[0]
"#;
    let val = eval_source(src).expect("Overlapped all_reduce_async + foreground compute");
    let got = match val {
        Value::Scalar(s) => s.as_f64(),
        Value::Float(f) => f,
        other => panic!("expected f32 5.0, got {other}"),
    };
    assert!((got - 5.0).abs() < 1e-5);
}
