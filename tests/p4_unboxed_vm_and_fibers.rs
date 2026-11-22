//! P4-2 Integration & Stress Verification Suite:
//! - P4-2A: 16-byte hybrid unboxed `RegSlot` register file (`size_of::<RegSlot>() == 16`),
//!   zero-allocation primitive ALU/comparison/branch/local fast paths, and exact numeric tower parity.
//! - P4-2B: Unified `CapturedFrames` cooperative M:N frame suspension (`VmYield::Preempt` +
//!   `VmYield::AwaitFuture`), step-budget preemption parity with multi-shot algebraic effects,
//!   and 100+ concurrent fibers (CPU loops, `sleep`, unbuffered `channel`, `select`, `join`,
//!   TCP I/O, and cancellation) multiplexed cleanly onto just 2 OS worker threads.

use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::scheduler::TaskScheduler;
use mora::runtime::vm::{
    CapturedFrames, RegSlot, VmEngine, VmOutcome, VmYield, execute_program, init_globals,
    materialize_frame,
};
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn eval_with_runtime(src: &str, rt: &mut CoreRuntime) -> Result<Value, String> {
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
    let (val, _) =
        execute_program(Arc::new(prog), rt).map_err(|e| format!("runtime error: {e}"))?;
    Ok(val)
}

#[test]
fn p4_2a_regslot_16_byte_layout_and_unboxed_numeric_semantics() {
    assert_eq!(
        std::mem::size_of::<RegSlot>(),
        16,
        "RegSlot must remain strictly 16 bytes for L1-cache-dense VM frames"
    );
    assert!(
        std::mem::size_of::<RegSlot>() < std::mem::size_of::<Value>(),
        "RegSlot (16B) must be strictly smaller than full Value ({}B)",
        std::mem::size_of::<Value>()
    );

    // Round-trip primitive unboxed slots and heap-boxed slots.
    for sample in [
        Value::Nil,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(-42),
        Value::Int(i64::MAX),
        Value::Float(3.5),
        Value::String("boxed-str".to_string()),
    ] {
        let slot = RegSlot::from_value(sample.clone());
        assert_eq!(slot.to_value(), sample);
    }

    // Intensive numeric loop exercising unboxed Int/Float/Bool ALU, comparisons, locals, branches,
    // closures capturing locals, and O(1) tail recursion.
    let src = r#"
task sum_tail(n: int, acc: int): int
  if n <= 0 then
    return acc
  end
  return sum_tail(n - 1, acc + n)
end

let total = 0
let i = 1
while i <= 20000
  if (i % 2) == 0 then
    total = total + i
  else
    total = total - 1
  end
  i = i + 1
end

let fsum = 0.0
let j = 1
while j <= 1000
  fsum = fsum + (1.5 * 2.0) - 0.5
  j = j + 1
end

let tsum = sum_tail(5000, 0)
let captured = 10
let bump = fn(x: int)
  captured = captured + x
  captured
end
let c1 = bump(5)
let c2 = bump(7)

assert_eq(total, 100000000)
assert_eq(tsum, 12502500)
assert_eq(c1, 15)
assert_eq(c2, 22)
assert(fsum > 2499.9 and fsum < 2500.1)
[total, tsum, c2]
"#;
    let mut rt = CoreRuntime::default();
    let val = eval_with_runtime(src, &mut rt).expect("unboxed hot loop must pass");
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::Int(100_000_000),
                Value::Int(12_502_500),
                Value::Int(22),
            ]
            .into()
        )
    );

    // Exact integer overflow check must still trigger (no silent wrapping on unboxed fast path).
    let overflow_src = r#"
let x = 9223372036854775807
let y = x + 1
y
"#;
    let mut rt2 = CoreRuntime::default();
    let err = eval_with_runtime(overflow_src, &mut rt2)
        .expect_err("i64::MAX + 1 must still raise overflow error");
    assert!(
        err.to_lowercase().contains("overflow"),
        "expected overflow diagnostic, got: {err}"
    );
}

#[test]
fn p4_2b_vm_step_budget_preemption_and_captured_frames_parity() {
    // Program combining loops AND multi-shot algebraic effects (`perform`/`handle`),
    // driven under a tiny 16-instruction step budget to verify `CapturedFrames` parity
    // between cooperative preemption (`VmYield::Preempt`) and delimited continuations.
    let src = r#"
let out = handle Choice {
  let x = perform Choice()
  let s = 0
  let i = 0
  while i < 25
    s = s + x + i
    i = i + 1
  end
  s
} {
  resume(10) + resume(20)
}
out
"#;
    let (prog, witnesses) = ParserV3::compile(src).expect("compile");
    assert!(check_program_witnesses(&witnesses).is_empty());
    let prog = Arc::new(prog);
    let globals = init_globals(&prog);
    let entry = prog.entry;
    let entry_frame =
        materialize_frame(prog, entry, globals, Arc::new(Vec::new()), &[]).expect("entry frame");

    let mut rt = CoreRuntime::default();
    let mut frames: CapturedFrames =
        vec![mora::runtime::vm::CallFrame::Script(Box::new(entry_frame))];
    let mut preempt_count = 0usize;
    let final_val = loop {
        let mut vm = VmEngine::with_frames(&mut rt, frames);
        match vm.run_with_budget(16).expect("budgeted VM step") {
            VmOutcome::Completed(v) => break v,
            VmOutcome::Suspended {
                frames: captured,
                yield_reason: VmYield::Preempt,
            } => {
                preempt_count += 1;
                frames = captured;
            }
            VmOutcome::Suspended {
                yield_reason: VmYield::AwaitFuture(_),
                ..
            } => panic!("pure compute + effects script should not emit AwaitFuture"),
        }
    };
    // For x=10: sum_{i=0..24}(10+i) = 250 + 300 = 550
    // For x=20: sum_{i=0..24}(20+i) = 500 + 300 = 800
    // Multi-shot total = 1350
    assert_eq!(final_val, Value::Int(1350));
    assert!(
        preempt_count >= 10,
        "expected >= 10 cooperative preemptions under 16-step budget, got {preempt_count}"
    );
}

#[test]
fn p4_2b_mn_fibers_120_concurrent_tasks_on_2_worker_threads() {
    // Strictly bound the M:N executor pool to ONLY 2 OS worker threads, then run 120+
    // concurrent Mora fibers combining tight CPU loops, `sleep`, unbuffered `channel`
    // rendezvous, `select`, and `handle.join()`. Under a 1:1 blocking model on 2 workers
    // this would immediately deadlock; under P4-2 cooperative frame suspension it finishes cleanly!
    let scheduler = Arc::new(TaskScheduler::with_worker_threads(2).expect("2-worker scheduler"));
    let mut rt = CoreRuntime::default().with_scheduler(Arc::clone(&scheduler));

    let src = r#"
task cpu_spin(iters: int, tag: int): int
  let s = 0
  let i = 0
  while i < iters
    s = s + (i % 7)
    i = i + 1
  end
  return s + tag
end

task producer(ch: channel<int>, id: int): int
  sleep(5)
  let work = cpu_spin(800, id)
  ch.send(work)
  return id
end

task consumer(ch: channel<int>, id: int): int
  let received = select
    case v = ch.recv() => v
    case timeout(5000) => -999999
  end
  return received
end

let pairs = 40
let prod_handles: list<TaskHandle<int>> = []
let cons_handles: list<TaskHandle<int>> = []
let cpu_handles: list<TaskHandle<int>> = []

let i = 0
while i < pairs
  let ch: channel<int> = channel()
  prod_handles = prod_handles.push(spawn producer(ch, i))
  cons_handles = cons_handles.push(spawn consumer(ch, i))
  cpu_handles = cpu_handles.push(spawn cpu_spin(1200, i))
  i = i + 1
end

let sum_cons = 0
let sum_cpu = 0
let k = 0
while k < pairs
  let _p = prod_handles[k].join()
  sum_cons = sum_cons + cons_handles[k].join()
  sum_cpu = sum_cpu + cpu_handles[k].join()
  k = k + 1
end

[sum_cons, sum_cpu]
"#;

    let val = eval_with_runtime(src, &mut rt).expect("120 M:N fibers on 2 threads must succeed");
    // cpu_spin(800, id): 800 = 114*7 + 2 -> 114*(0+1+2+3+4+5+6) + (0+1) = 114*21 + 1 = 2395
    // sum_{id=0..39}(2395 + id) = 40*2395 + 780 = 96580
    // cpu_spin(1200, id): 1200 = 171*7 + 3 -> 171*21 + (0+1+2) = 3594
    // sum_{id=0..39}(3594 + id) = 40*3594 + 780 = 144540
    assert_eq!(
        val,
        Value::List(vec![Value::Int(96_580), Value::Int(144_540)].into())
    );

    let (workers, preemptions, async_suspensions) = scheduler.fiber_stats();
    assert_eq!(workers, 2, "M:N pool must remain fixed at 2 OS threads");
    assert!(
        preemptions > 0,
        "tight CPU loops inside fibers must trigger cooperative step-budget preemptions"
    );
    assert!(
        async_suspensions >= 80,
        "expected >= 80 non-blocking async fiber suspensions across sleep/channel/select, got {async_suspensions}"
    );
}

#[test]
fn p4_2b_mn_fiber_tcp_echo_and_cancellation_on_2_worker_threads() {
    let scheduler = Arc::new(TaskScheduler::with_worker_threads(2).expect("2-worker scheduler"));
    let mut rt = CoreRuntime::default().with_scheduler(Arc::clone(&scheduler));

    let src = r#"
task handle_client(conn: TcpStream): int
  let line = conn.read_line()
  conn.write("echo:" + line)
  conn.close()
  return 1
end

task run_server(listener: TcpListener, count: int): int
  let handlers: list<TaskHandle<int>> = []
  let i = 0
  while i < count
    let c = listener.accept()
    handlers = handlers.push(spawn handle_client(c))
    i = i + 1
  end
  let ok = 0
  let j = 0
  while j < count
    ok = ok + handlers[j].join()
    j = j + 1
  end
  listener.close()
  return ok
end

task run_client(addr: string, id: int): bool
  let s = tcp_connect(addr)
  let msg = "hi-" + str(id) + "\n"
  s.write(msg)
  let resp = s.read_line()
  s.close()
  return resp == ("echo:" + msg)
end

let listener = tcp_listen("127.0.0.1:0")
let addr = listener.local_addr()
let n_clients = 8
let srv = spawn run_server(listener, n_clients)

let clients: list<TaskHandle<bool>> = []
let i = 0
while i < n_clients
  clients = clients.push(spawn run_client(addr, i))
  i = i + 1
end

let passed = 0
let k = 0
while k < n_clients
  if clients[k].join() then
    passed = passed + 1
  end
  k = k + 1
end
let handled = srv.join()

let sleeper = spawn sleep(5000)
sleep(10)
sleeper.abort()
assert(sleeper.is_done())

[passed, handled]
"#;

    let val = eval_with_runtime(src, &mut rt).expect("M:N TCP echo + abort on 2 workers must pass");
    assert_eq!(val, Value::List(vec![Value::Int(8), Value::Int(8)].into()));

    let (workers, _preemptions, async_suspensions) = scheduler.fiber_stats();
    assert_eq!(workers, 2);
    assert!(
        async_suspensions >= 16,
        "TCP accept/connect/read/write/sleep must suspend cooperatively, got {async_suspensions}"
    );
}
