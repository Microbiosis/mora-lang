//! Wave 2 — Stage W2.4 regression suite:
//! - Domain-typed `MoraError` / `MoraResult<T>` (`kind_name`, `message`, `with_prefix`,
//!   `is_cancellation_or_timeout`, `From<std::io::Error>`)
//! - `TaskScheduler` structured concurrency (`TaskGroup` join, panic propagation as
//!   `MoraError::Task`, abort-on-drop cleanup)
//! - Bounded/unbounded channels (`channel`, `unbounded_channel`, backpressure, `close`,
//!   `recv_ctx` / `send_ctx` cancellation)
//! - Fair `select_channels` round-robin scheduling and `select2`
//! - Hierarchical `TaskContext` cancellation cascade & deadline expiration (`Timeout`)
//! - `AsyncIoReactor` (`sleep`, `timeout`, `AsyncTcpListener`, `serve_connections`,
//!   concurrent `request_response` loopback multiplexing)
//! - Cooperative `VmEngine` cancellation via `spawn_mora_task` & `Transaction` rollback
//!   preserving `MoraError::Cancelled`
//! - `parallel` block deterministic declaration-order merge + error propagation on
//!   `TaskScheduler::run_indexed_batch`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use mora::error::{MoraError, MoraResult};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::reactor::{AsyncIoReactor, AsyncTcpListener};
use mora::runtime::scheduler::{Select2, TaskContext, TaskScheduler};
use mora::runtime::vm::execute_program;
use mora::value::Value;

#[test]
fn mora_error_variants_kind_prefix_and_cancellation() {
    let cases = [
        (MoraError::Parse("bad syntax".into()), "parse", false),
        (MoraError::Typeck("mismatch".into()), "typeck", false),
        (MoraError::Runtime("div zero".into()), "runtime", false),
        (MoraError::Task("worker panicked".into()), "task", false),
        (MoraError::Channel("closed".into()), "channel", false),
        (MoraError::Cancelled("aborted".into()), "cancelled", true),
        (MoraError::Timeout("deadline".into()), "timeout", true),
        (MoraError::Io("eof".into()), "io", false),
        (
            MoraError::Serialization("bad json".into()),
            "serialization",
            false,
        ),
        (MoraError::Other("misc".into()), "other", false),
    ];

    for (err, expected_kind, expected_cancel_or_timeout) in cases {
        assert_eq!(err.kind_name(), expected_kind);
        assert_eq!(
            err.is_cancellation_or_timeout(),
            expected_cancel_or_timeout,
            "unexpected is_cancellation_or_timeout for {err:?}"
        );
        let orig_msg = err.message().to_string();
        let prefixed = err.with_prefix("stage: ");
        assert_eq!(prefixed.kind_name(), expected_kind);
        assert_eq!(prefixed.message(), format!("stage: {orig_msg}"));
        assert_eq!(
            prefixed.is_cancellation_or_timeout(),
            expected_cancel_or_timeout
        );
    }

    let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broke");
    let mapped = MoraError::from(io_err);
    assert_eq!(mapped.kind_name(), "io");
    assert!(mapped.message().contains("pipe broke"));
}

struct DropFlag {
    dropped: Arc<AtomicBool>,
}

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn task_group_structured_concurrency_panic_and_abort_on_drop() {
    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));

    // 1. Concurrent group fan-out & join_all
    let sum: i64 = sched.block_on(async {
        let mut group = sched.group::<i64>(None);
        for i in 1..=8 {
            group.spawn(move |_ctx| async move { Ok(i * i) });
        }
        let mut vals = group.join_all().await.expect("group join_all");
        vals.sort();
        assert_eq!(vals, vec![1, 4, 9, 16, 25, 36, 49, 64]);
        vals.into_iter().sum()
    });
    assert_eq!(sum, 204);

    // 2. Worker panic surfaces cleanly as MoraError::Task
    let panic_err = sched.block_on(async {
        let mut group = sched.group::<i32>(None);
        group.spawn(|_ctx| async move { Ok(1) });
        group.spawn(|_ctx| async move { panic!("intentional w2_4 test panic") });
        group.join_all().await.expect_err("panic must propagate")
    });
    assert_eq!(panic_err.kind_name(), "task");

    // 3. Dropping an unfinished TaskGroup aborts spawned tasks cleanly
    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_clone = Arc::clone(&aborted);
    let sched_clone = Arc::clone(&sched);
    sched.block_on(async move {
        {
            let mut group = sched_clone.group::<()>(None);
            group.spawn(move |_ctx| async move {
                let _guard = DropFlag {
                    dropped: aborted_clone,
                };
                std::future::pending::<()>().await;
                Ok(())
            });
            // Yield briefly so the spawned task starts polling and installs _guard
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        // Poll up to 500ms for tokio runtime to drop the aborted future stack
        for _ in 0..50 {
            if aborted.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            aborted.load(Ordering::SeqCst),
            "dropping TaskGroup must abort active tasks and run Drop"
        );
    });
}

#[test]
fn bounded_and_unbounded_channels_backpressure_close_and_cancel() {
    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));

    let zero_err = sched
        .channel::<i32>(0)
        .err()
        .expect("zero capacity rejected");
    assert_eq!(zero_err.kind_name(), "channel");

    sched.block_on(async {
        let (tx, mut rx) = sched.channel::<i32>(1).expect("bounded(1)");
        tx.try_send(10).expect("first fits in buffer");
        let full_err = tx.try_send(20).expect_err("second hits backpressure");
        assert_eq!(full_err.kind_name(), "channel");

        let tx_clone = tx.clone();
        let ctx = TaskContext::root();
        let send_handle = sched.spawn(&ctx, move |_c| async move {
            tx_clone.send(20).await?;
            Ok(())
        });

        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(
            !send_handle.is_finished(),
            "sender must block until buffer drains"
        );

        assert_eq!(rx.recv().await.expect("recv first"), 10);
        send_handle.join().await.expect("blocked send completes");
        assert_eq!(rx.recv().await.expect("recv second"), 20);

        // Cancelled send_ctx / recv_ctx
        tx.try_send(30).expect("fill slot again");
        let send_cancel = TaskContext::root();
        send_cancel.cancel("stop send");
        assert_eq!(
            tx.send_ctx(40, &send_cancel)
                .await
                .expect_err("send_ctx cancelled")
                .kind_name(),
            "cancelled"
        );
        assert_eq!(rx.recv().await.expect("drain 30"), 30);

        let recv_cancel = TaskContext::root();
        recv_cancel.cancel("stop recv");
        assert_eq!(
            rx.recv_ctx(&recv_cancel)
                .await
                .expect_err("recv_ctx cancelled")
                .kind_name(),
            "cancelled"
        );

        rx.close();
        assert_eq!(
            tx.send(50)
                .await
                .expect_err("closed receiver rejects send")
                .kind_name(),
            "channel"
        );

        // Unbounded channel FIFO
        let (utx, mut urx) = sched.unbounded_channel::<&'static str>();
        utx.try_send("a").expect("u1");
        utx.try_send("b").expect("u2");
        assert_eq!(urx.recv().await.expect("ur1"), "a");
        assert_eq!(urx.recv().await.expect("ur2"), "b");
    });
}

#[test]
fn select_channels_fair_round_robin_and_select2() {
    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));

    sched.block_on(async {
        let (tx0, rx0) = sched.channel::<&'static str>(2).expect("ch0");
        let (tx1, rx1) = sched.channel::<&'static str>(2).expect("ch1");
        let (tx2, rx2) = sched.channel::<&'static str>(2).expect("ch2");
        tx0.send("c0").await.expect("s0");
        tx1.send("c1").await.expect("s1");
        tx2.send("c2").await.expect("s2");

        let mut rxs = [rx0, rx1, rx2];
        // start_hint rotates polling priority fairly
        let second = sched
            .select_channels(&mut rxs, 1, None)
            .await
            .expect("pick from ch1");
        assert_eq!(second, (1, "c1"));

        let third = sched
            .select_channels(&mut rxs, 2, None)
            .await
            .expect("pick from ch2");
        assert_eq!(third, (2, "c2"));

        let first = sched
            .select_channels(&mut rxs, 0, None)
            .await
            .expect("pick from ch0");
        assert_eq!(first, (0, "c0"));

        let s2 = sched
            .select2(async { "fast" }, async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                "slow"
            })
            .await;
        assert_eq!(s2, Select2::First("fast"));

        drop(tx0);
        drop(tx1);
        drop(tx2);
        let closed = sched
            .select_channels(&mut rxs, 0, None)
            .await
            .expect_err("all channels closed");
        assert_eq!(closed.kind_name(), "channel");
    });
}

#[test]
fn hierarchical_task_context_cancellation_and_deadline() {
    let parent = TaskContext::root();
    let child_a = parent.child();
    let grandchild_a = child_a.child();
    let child_b = parent.child();

    assert!(parent.status().is_ok());
    assert!(grandchild_a.status().is_ok());

    // Sibling isolation: cancelling child_a cancels grandchild_a, not parent or child_b
    child_a.cancel("branch aborted");
    assert_eq!(
        child_a.status().expect_err("child_a cancelled").kind_name(),
        "cancelled"
    );
    assert_eq!(
        grandchild_a
            .status()
            .expect_err("grandchild_a cancelled")
            .kind_name(),
        "cancelled"
    );
    assert!(parent.status().is_ok());
    assert!(child_b.status().is_ok());

    // Parent cascade reaches child_b
    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));
    let cascade_err = sched.block_on(async {
        let cb = child_b.clone();
        let waiter = sched.spawn(&TaskContext::root(), move |_| async move {
            MoraResult::<()>::Err(cb.cancelled().await)
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        parent.cancel("root shutdown");
        waiter.join().await.expect_err("cascade err")
    });
    assert_eq!(cascade_err.kind_name(), "cancelled");
    assert!(cascade_err.message().contains("root shutdown"));

    // Deadline expiration surfaces MoraError::Timeout in status(), reactor.sleep(), and batch
    let timed = TaskContext::root().with_timeout(Duration::from_millis(20));
    assert!(timed.deadline().is_some());

    let reactor = AsyncIoReactor::new(Arc::clone(&sched));
    let sleep_err = sched.block_on(async {
        reactor
            .sleep(Duration::from_millis(250), Some(&timed))
            .await
            .expect_err("sleep must hit deadline")
    });
    assert_eq!(sleep_err.kind_name(), "timeout");
    assert!(sleep_err.is_cancellation_or_timeout());

    let batch_err = sched
        .run_indexed_batch(
            vec![Box::new(|_ctx| {
                std::thread::sleep(Duration::from_millis(25));
                Ok(1usize)
            })
                as Box<dyn FnOnce(TaskContext) -> MoraResult<usize> + Send>],
            Some(&timed),
        )
        .expect_err("batch must fail when deadline already expired");
    assert_eq!(batch_err.kind_name(), "timeout");
}

#[test]
fn async_io_reactor_tcp_multiplexing_sleep_and_timeout() {
    let sched = Arc::new(TaskScheduler::with_worker_threads(2).expect("scheduler"));
    let reactor = Arc::new(AsyncIoReactor::new(Arc::clone(&sched)));

    sched.block_on(async {
        reactor
            .sleep(Duration::from_millis(5), None)
            .await
            .expect("short sleep ok");

        let to_err = reactor
            .timeout(
                Duration::from_millis(15),
                std::future::pending::<MoraResult<()>>(),
            )
            .await
            .expect_err("pending hits reactor timeout");
        assert_eq!(to_err.kind_name(), "timeout");

        let listener = AsyncTcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("local addr").to_string();

        let server_ctx = TaskContext::root().with_timeout(Duration::from_secs(5));
        let handled = Arc::new(AtomicUsize::new(0));
        let handled_srv = Arc::clone(&handled);
        let reactor_srv = Arc::clone(&reactor);
        let server = sched.spawn(&server_ctx, move |ctx| async move {
            reactor_srv
                .serve_connections(listener, Some(4), &ctx, move |mut stream, _peer, c| {
                    let h = Arc::clone(&handled_srv);
                    async move {
                        let req = stream.read_to_limit(1024, Some(&c)).await?;
                        let mut reply = b"echo:".to_vec();
                        reply.extend_from_slice(&req);
                        stream.write_all(&reply, Some(&c)).await?;
                        stream.shutdown().await?;
                        h.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                })
                .await
        });

        let mut group = sched.group::<(usize, Vec<u8>)>(Some(server_ctx.clone()));
        for i in 0..4usize {
            let addr_c = addr.clone();
            let reactor_c = Arc::clone(&reactor);
            group.spawn(move |ctx| async move {
                let payload = format!("msg-{i}");
                let resp = reactor_c
                    .request_response(
                        &addr_c,
                        payload.as_bytes(),
                        1024,
                        Duration::from_secs(3),
                        Some(&ctx),
                    )
                    .await?;
                Ok((i, resp))
            });
        }

        let mut responses = group.join_all().await.expect("all 4 clients ok");
        responses.sort_by_key(|(i, _)| *i);
        for (i, bytes) in responses {
            assert_eq!(bytes, format!("echo:msg-{i}").into_bytes());
        }

        let total = server.join().await.expect("server ok");
        assert_eq!(total, 4);
        assert_eq!(handled.load(Ordering::SeqCst), 4);
    });
}

#[test]
fn spawn_mora_task_cooperatively_cancels_loop_and_preserves_cancelled_in_transaction() {
    // 1. Infinite loop in spawned Mora task cooperatively yields Cancelled when context is cancelled
    let loop_src = r#"
let spin = fn()
  let i = 0i
  while true
    i = i + 1i
  end
  i
end
spin
"#;
    let (prog, _w) = ParserV3::compile(loop_src).expect("compile spin");
    let mut base_core = CoreRuntime::default();
    let sched = Arc::clone(base_core.scheduler());
    let (closure, _globals) = execute_program(Arc::new(prog), &mut base_core).expect("eval fn");

    let task_ctx = TaskContext::root();
    let cancel_trigger = task_ctx.clone();
    let core_for_task = base_core.with_task_context(task_ctx.clone());
    let handle = sched.spawn_mora_task(closure, vec![], &core_for_task, &task_ctx);

    let err = sched.block_on(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel_trigger.cancel("user cancelled infinite loop");
        handle.join().await.expect_err("spin must abort on cancel")
    });
    assert_eq!(err.kind_name(), "cancelled");
    assert!(err.is_cancellation_or_timeout());

    // 2. Cancelled execution inside a Mora transaction preserves MoraError::Cancelled
    let tx_src = r#"
let tx_spin = fn()
  let x = 0i
  transaction
    x = 1i
    while true
      x = x + 1i
    end
  compensation
    x = 99i
  end
  x
end
tx_spin
"#;
    let (tx_prog, _w) = ParserV3::compile(tx_src).expect("compile tx_spin");
    let mut tx_core = CoreRuntime::default();
    let tx_sched = Arc::clone(tx_core.scheduler());
    let (tx_closure, _) = execute_program(Arc::new(tx_prog), &mut tx_core).expect("eval tx fn");

    let tx_ctx = TaskContext::root();
    let tx_cancel = tx_ctx.clone();
    let tx_core_for_task = tx_core.with_task_context(tx_ctx.clone());
    let tx_handle = tx_sched.spawn_mora_task(tx_closure, vec![], &tx_core_for_task, &tx_ctx);

    let tx_err = tx_sched.block_on(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx_cancel.cancel("cancel inside transaction");
        tx_handle
            .join()
            .await
            .expect_err("cancelled transaction must propagate Cancelled")
    });
    assert_eq!(tx_err.kind_name(), "cancelled");
    assert!(tx_err.message().contains("cancel inside transaction"));
}

#[test]
fn parallel_workers_run_on_task_scheduler_with_deterministic_merge_and_error_propagation() {
    // 1. Multiple parallel workers share Scheduler + produce deterministic CRDT merge
    let merge_src = r#"
merge_with("sum", "add")
let sum = 0i
parallel
  worker w1 do
    sum = 10i
  end
  worker w2 do
    sum = 20i
  end
  worker w3 do
    sum = 30i
  end
end
sum
"#;
    let (prog, _w) = ParserV3::compile(merge_src).expect("compile parallel merge");
    let arc = Arc::new(prog);
    for _ in 0..3 {
        let mut core = CoreRuntime::default();
        let (val, _) = execute_program(Arc::clone(&arc), &mut core).expect("parallel merge");
        assert_eq!(val, Value::Int(60));
    }

    // 2. Pre-cancelled context on CoreRuntime stops parallel worker batch deterministically
    let cancelled_ctx = TaskContext::root();
    cancelled_ctx.cancel("stop parallel batch");
    let mut cancelled_core = CoreRuntime::default().with_task_context(cancelled_ctx);
    let err = execute_program(arc, &mut cancelled_core)
        .expect_err("cancelled context must abort parallel execution");
    assert_eq!(err.kind_name(), "cancelled");
    assert!(err.is_cancellation_or_timeout());

    // 3. Error raised by one parallel worker propagates cleanly as MoraError::Runtime
    let err_src = r#"
let x = 0i
parallel
  worker w1 do
    x = 1i
  end
  worker w2 do
    x = 1i / 0i
  end
end
x
"#;
    let (err_prog, _w) = ParserV3::compile(err_src).expect("compile parallel error");
    let mut core = CoreRuntime::default();
    let worker_err = execute_program(Arc::new(err_prog), &mut core)
        .expect_err("division by zero in parallel worker must propagate");
    assert_eq!(worker_err.kind_name(), "runtime");
}
