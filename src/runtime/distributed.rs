//! P4-4: Distributed Tensor Collectives (`ProcessGroup`) over In-Process M:N Fibers
//! and Live `AsyncIoReactor` TCP Ring Sockets.
//!
//! Architecture (`AGENTS.md §0`):
//! - Zero external Cargo dependencies: built directly on [`crate::runtime::scheduler::AsyncNotify`]
//!   and [`crate::runtime::reactor::{MoraTcpListener, MoraTcpStream}`].
//! - 100% cooperative async futures (`all_reduce`, `all_gather`, `broadcast`, `reduce_scatter`,
//!   `barrier`), driven via `CallAction::AwaitFuture` on the M:N VM scheduler so multi-rank
//!   collectives NEVER block OS worker threads even when `world_size > worker_threads`.
//! - Preserves tensor `DType` and GPU VRAM residency (`Device::Cuda` / `Device::Vulkan` /
//!   `Device::Metal`) across collective outputs, and records nanosecond spans into
//!   [`crate::runtime::profiler`] when tracing is active.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::common::sync::Mutex;
use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::profiler::TraceScope;
use crate::runtime::reactor::{MoraTcpListener, MoraTcpStream};
use crate::runtime::scheduler::{AsyncNotify, TaskContext, sleep};
use crate::value::tensor::{Tensor, quantize_for_dtype};

/// Reduction operator supported by `all_reduce` and `reduce_scatter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReduceOp {
    Sum,
    Mean,
    Max,
    Min,
}

impl ReduceOp {
    pub(crate) fn parse(s: &str) -> MoraResult<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sum" | "add" => Ok(Self::Sum),
            "mean" | "avg" => Ok(Self::Mean),
            "max" => Ok(Self::Max),
            "min" => Ok(Self::Min),
            other => Err(MoraError::Runtime(format!(
                "ProcessGroup: unsupported reduce op '{other}' (expected \"sum\", \"mean\", \"max\", or \"min\")"
            ))),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Mean => "mean",
            Self::Max => "max",
            Self::Min => "min",
        }
    }
}

#[derive(Debug, Default)]
struct CollectiveStats {
    all_reduce_calls: AtomicU64,
    all_gather_calls: AtomicU64,
    broadcast_calls: AtomicU64,
    reduce_scatter_calls: AtomicU64,
    barrier_calls: AtomicU64,
    send_calls: AtomicU64,
    recv_calls: AtomicU64,
    split_calls: AtomicU64,
    bytes_transferred: AtomicU64,
    wire_bytes_sent: AtomicU64,
    wire_bytes_recv: AtomicU64,
}

// ── 1. In-Process Shared Multi-Rank Collective Coordinator (Async, Zero Thread Blocking) ──

#[derive(Debug, Clone)]
struct TensorPayload {
    data: Vec<f64>,
    shape: Vec<usize>,
    dtype: DType,
}

impl TensorPayload {
    fn from_tensor(t: &Tensor) -> Self {
        Self {
            data: t.to_flat_vec(),
            shape: t.shape.clone(),
            dtype: t.dtype,
        }
    }

    fn byte_len(&self) -> u64 {
        self.dtype.storage_bytes(self.data.len()) as u64
    }

    fn into_tensor_on_device(self, device: &Device) -> MoraResult<Arc<Tensor>> {
        let host = Tensor::from_raw(self.data, self.shape, self.dtype, Device::Cpu, false)?;
        if *device == Device::Cpu {
            Ok(host)
        } else {
            host.to_device(device.clone())
        }
    }
}

#[derive(Debug)]
struct GenBucket {
    slots: Vec<Option<TensorPayload>>,
    arrived: usize,
    readers: usize,
    failed: Option<String>,
}

type P2pMailboxMap = HashMap<(usize, usize), VecDeque<Arc<Tensor>>>;

#[derive(Debug)]
struct InProcCollectiveState {
    world_size: usize,
    /// Per-generation staging table keyed by collective sequence number.
    gens: Mutex<HashMap<u64, GenBucket>>,
    notify: AsyncNotify,
    /// P5-3: VRAM-preserving cooperative P2P FIFO mailboxes keyed by `(src_rank, dst_rank)`.
    p2p_queues: Mutex<P2pMailboxMap>,
    p2p_notify: AsyncNotify,
    /// P5-3: Deterministic shared sub-group registry keyed by `(split_seq, color)`.
    split_groups: Mutex<HashMap<(u64, i64), Arc<InProcCollectiveState>>>,
}

impl InProcCollectiveState {
    fn new(world_size: usize) -> Arc<Self> {
        Arc::new(Self {
            world_size,
            gens: Mutex::new(HashMap::new()),
            notify: AsyncNotify::new(),
            p2p_queues: Mutex::new(HashMap::new()),
            p2p_notify: AsyncNotify::new(),
            split_groups: Mutex::new(HashMap::new()),
        })
    }

    fn p2p_send(&self, src_rank: usize, dst_rank: usize, tensor: Arc<Tensor>) {
        let mut qs = self.p2p_queues.lock();
        qs.entry((src_rank, dst_rank))
            .or_default()
            .push_back(tensor);
        drop(qs);
        self.p2p_notify.notify_waiters();
    }

    async fn p2p_recv(
        &self,
        src_rank: usize,
        dst_rank: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        loop {
            if let Some(c) = ctx {
                c.status()?;
            }
            let notified = self.p2p_notify.notified();
            {
                let mut qs = self.p2p_queues.lock();
                if let Some(q) = qs.get_mut(&(src_rank, dst_rank))
                    && let Some(t) = q.pop_front()
                {
                    return Ok(t);
                }
            }
            match ctx {
                Some(c) => match crate::runtime::scheduler::race2(c.cancelled(), notified).await {
                    crate::runtime::scheduler::Select2::First(err) => return Err(err),
                    crate::runtime::scheduler::Select2::Second(()) => {}
                },
                None => notified.await,
            }
        }
    }

    fn get_or_create_split_group(
        &self,
        seq: u64,
        color: i64,
        new_world_size: usize,
    ) -> Arc<InProcCollectiveState> {
        let mut map = self.split_groups.lock();
        Arc::clone(
            map.entry((seq, color))
                .or_insert_with(|| InProcCollectiveState::new(new_world_size)),
        )
    }

    async fn exchange(
        &self,
        seq: u64,
        rank: usize,
        payload: TensorPayload,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<TensorPayload>> {
        if self.world_size == 1 {
            return Ok(vec![payload]);
        }
        {
            let mut map = self.gens.lock();
            let bucket = map.entry(seq).or_insert_with(|| GenBucket {
                slots: vec![None; self.world_size],
                arrived: 0,
                readers: 0,
                failed: None,
            });
            if let Some(msg) = &bucket.failed {
                return Err(MoraError::Runtime(msg.clone()));
            }
            if bucket.slots[rank].is_some() {
                let msg = format!(
                    "ProcessGroup: duplicate arrival on rank {rank} for collective seq {seq} (each spawned fiber must hold a distinct rank handle)"
                );
                bucket.failed = Some(msg.clone());
                drop(map);
                self.notify.notify_waiters();
                return Err(MoraError::Runtime(msg));
            }
            bucket.slots[rank] = Some(payload);
            bucket.arrived += 1;
            if bucket.arrived == self.world_size {
                self.notify.notify_waiters();
            }
        }
        loop {
            if let Some(c) = ctx {
                c.status()?;
            }
            let notified = self.notify.notified();
            {
                let mut map = self.gens.lock();
                if let Some(bucket) = map.get_mut(&seq) {
                    if let Some(msg) = &bucket.failed {
                        return Err(MoraError::Runtime(msg.clone()));
                    }
                    if bucket.arrived == self.world_size {
                        let all: Vec<TensorPayload> = bucket
                            .slots
                            .iter()
                            .map(|s| s.clone().expect("all ranks arrived"))
                            .collect();
                        bucket.readers += 1;
                        if bucket.readers >= self.world_size {
                            map.remove(&seq);
                        }
                        return Ok(all);
                    }
                }
            }
            match ctx {
                Some(c) => match crate::runtime::scheduler::race2(c.cancelled(), notified).await {
                    crate::runtime::scheduler::Select2::First(err) => return Err(err),
                    crate::runtime::scheduler::Select2::Second(()) => {}
                },
                None => notified.await,
            }
        }
    }
}

// ── 2. Live TCP Ring Collective & Direct P2P Transport (`AsyncIoReactor` TCP Sockets) ──

const TCP_RING_MAGIC: u32 = 0x4D50_4752; // "MPGR"
const TCP_CHUNK_MAGIC: u32 = 0x4D50_4743; // "MPGC" (2-phase Scatter-Reduce / AllGather chunk)
const TCP_P2P_MAGIC: u32 = 0x4D50_4750; // "MPGP" (direct point-to-point tensor packet)

const CONN_KIND_RING: u8 = 0x01;
const CONN_KIND_P2P: u8 = 0x02;

#[derive(Debug)]
struct TcpRingState {
    /// Outgoing ring edge `rank -> (rank + 1) % world_size`.
    next_stream: Arc<MoraTcpStream>,
    /// Incoming ring edge `(rank + world_size - 1) % world_size -> rank`.
    prev_stream: Arc<MoraTcpStream>,
    /// Peer address table (`addrs[r]` is rank `r`'s bound listener address for direct P2P & `split`).
    addrs: Vec<String>,
    /// Listener kept alive for incoming direct P2P connections.
    listener: Arc<MoraTcpListener>,
    /// Async-safe lock + strict launch-order sequencer serializing collective frames on this
    /// rank handle without holding a non-`Send` `MutexGuard` across `.await`.
    /// Tuple `(busy, next_expected_seq)` ensures multiple pipelined `all_reduce_async` futures
    /// always transmit frames on the TCP stream in exact launch order.
    io_state: Mutex<(bool, u64)>,
    io_notify: AsyncNotify,
    /// Async-safe lock serializing `listener.accept()` for incoming P2P frames.
    p2p_accept_busy: Mutex<bool>,
    p2p_inbox: Mutex<HashMap<usize, VecDeque<TensorPayload>>>,
    p2p_notify: AsyncNotify,
}

impl TcpRingState {
    async fn acquire_io(&self, seq: u64) {
        loop {
            let notified = self.io_notify.notified();
            {
                let mut st = self.io_state.lock();
                if !st.0 && st.1 == seq {
                    st.0 = true;
                    return;
                }
            }
            notified.await;
        }
    }

    fn release_io(&self) {
        {
            let mut st = self.io_state.lock();
            st.0 = false;
            st.1 = st.1.wrapping_add(1);
        }
        self.io_notify.notify_waiters();
    }

    /// P5-3: Bandwidth-optimal 2-phase Ring AllReduce (Scatter-Reduce + AllGather) on TCP sockets.
    /// Each rank sends & receives exactly `2 * (W - 1) / W * S` payload bytes instead of `(W - 1) * S`.
    #[allow(clippy::too_many_arguments)]
    async fn ring_all_reduce_2phase(
        &self,
        rank: usize,
        world_size: usize,
        seq: u64,
        mut buf: Vec<f64>,
        dtype: DType,
        op: ReduceOp,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<(Vec<f64>, u64, u64)> {
        let n = buf.len();
        let chunk_range = |c: usize| -> (usize, usize) {
            let base = n / world_size;
            let rem = n % world_size;
            let start = c * base + c.min(rem);
            let len = base + usize::from(c < rem);
            (start, start + len)
        };

        let mut total_sent: u64 = 0;
        let mut total_recv: u64 = 0;

        // Phase 1: Scatter-Reduce (`world_size - 1` hops of `1/W` chunks)
        for step in 0..(world_size - 1) {
            let send_chunk = (rank + world_size - step) % world_size;
            let recv_chunk = (rank + world_size - step - 1) % world_size;
            let (s0, s1) = chunk_range(send_chunk);
            let encoded = encode_chunk_frame(send_chunk, seq, dtype, &buf[s0..s1]);
            total_sent = total_sent.saturating_add(encoded.len() as u64);

            let next = Arc::clone(&self.next_stream);
            let prev = Arc::clone(&self.prev_stream);
            let ctx_s = ctx.cloned();
            let ctx_r = ctx.cloned();
            let send_fut = async move { write_all_stream(&next, &encoded, ctx_s.as_ref()).await };
            let recv_fut = async move { recv_chunk_frame(&prev, seq, ctx_r.as_ref()).await };
            let (s_res, r_res) = join2(send_fut, recv_fut).await;
            s_res?;
            let (got_chunk, recv_vals, recv_bytes) = r_res?;
            total_recv = total_recv.saturating_add(recv_bytes);
            if got_chunk != recv_chunk {
                return Err(MoraError::Io(format!(
                    "tcp_process_group 2-phase scatter-reduce: expected chunk {recv_chunk}, got {got_chunk}"
                )));
            }
            let (r0, r1) = chunk_range(recv_chunk);
            if recv_vals.len() != r1 - r0 {
                return Err(MoraError::Io(format!(
                    "tcp_process_group 2-phase scatter-reduce: chunk length mismatch ({} vs {})",
                    recv_vals.len(),
                    r1 - r0
                )));
            }
            let dst = &mut buf[r0..r1];
            match op {
                ReduceOp::Sum | ReduceOp::Mean => {
                    for (d, v) in dst.iter_mut().zip(recv_vals) {
                        *d += v;
                    }
                }
                ReduceOp::Max => {
                    for (d, v) in dst.iter_mut().zip(recv_vals) {
                        if v > *d || v.is_nan() {
                            *d = v;
                        }
                    }
                }
                ReduceOp::Min => {
                    for (d, v) in dst.iter_mut().zip(recv_vals) {
                        if v < *d || v.is_nan() {
                            *d = v;
                        }
                    }
                }
            }
        }

        // Normalize completed chunk `(rank + 1) % world_size` once before AllGather for bit-exact Mean.
        if op == ReduceOp::Mean {
            let owned = (rank + 1) % world_size;
            let (o0, o1) = chunk_range(owned);
            let denom = world_size as f64;
            for d in &mut buf[o0..o1] {
                *d /= denom;
            }
        }

        // Phase 2: AllGather (`world_size - 1` hops of completed `1/W` chunks)
        let gather_seq = seq ^ 0x8000_0000_0000_0000;
        for step in 0..(world_size - 1) {
            let send_chunk = (rank + 1 + world_size - step) % world_size;
            let recv_chunk = (rank + world_size - step) % world_size;
            let (s0, s1) = chunk_range(send_chunk);
            let encoded = encode_chunk_frame(send_chunk, gather_seq, dtype, &buf[s0..s1]);
            total_sent = total_sent.saturating_add(encoded.len() as u64);

            let next = Arc::clone(&self.next_stream);
            let prev = Arc::clone(&self.prev_stream);
            let ctx_s = ctx.cloned();
            let ctx_r = ctx.cloned();
            let send_fut = async move { write_all_stream(&next, &encoded, ctx_s.as_ref()).await };
            let recv_fut = async move { recv_chunk_frame(&prev, gather_seq, ctx_r.as_ref()).await };
            let (s_res, r_res) = join2(send_fut, recv_fut).await;
            s_res?;
            let (got_chunk, recv_vals, recv_bytes) = r_res?;
            total_recv = total_recv.saturating_add(recv_bytes);
            if got_chunk != recv_chunk {
                return Err(MoraError::Io(format!(
                    "tcp_process_group 2-phase allgather: expected chunk {recv_chunk}, got {got_chunk}"
                )));
            }
            let (r0, r1) = chunk_range(recv_chunk);
            if recv_vals.len() != r1 - r0 {
                return Err(MoraError::Io(format!(
                    "tcp_process_group 2-phase allgather: chunk length mismatch ({} vs {})",
                    recv_vals.len(),
                    r1 - r0
                )));
            }
            buf[r0..r1].copy_from_slice(&recv_vals);
        }

        if dtype != DType::F64 {
            for d in &mut buf {
                *d = quantize_for_dtype(*d, dtype);
            }
        }
        Ok((buf, total_sent, total_recv))
    }

    async fn p2p_send(
        &self,
        src_rank: usize,
        dst_rank: usize,
        payload: &TensorPayload,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<u64> {
        let addr = self.addrs.get(dst_rank).ok_or_else(|| {
            MoraError::Runtime(format!(
                "tcp_process_group.send: dst_rank {dst_rank} out of bounds"
            ))
        })?;
        let mut stream_opt = None;
        for attempt in 0..100usize {
            if let Some(c) = ctx {
                c.status()?;
            }
            match MoraTcpStream::connect(addr, Some(Duration::from_secs(2)), ctx).await {
                Ok(s) => {
                    stream_opt = Some(s);
                    break;
                }
                Err(e) if attempt + 1 >= 100 => {
                    return Err(MoraError::Io(format!(
                        "tcp_process_group.send(rank={src_rank}->{dst_rank}): failed to connect to {addr}: {e}"
                    )));
                }
                Err(_) => sleep(Duration::from_millis(5)).await,
            }
        }
        let stream = stream_opt.expect("connected");
        let frame = encode_p2p_frame(src_rank, payload);
        let mut wire = Vec::with_capacity(1 + frame.len());
        wire.push(CONN_KIND_P2P);
        wire.extend_from_slice(&frame);
        write_all_stream(&stream, &wire, ctx).await?;
        Ok(wire.len() as u64)
    }

    async fn p2p_recv(
        &self,
        src_rank: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<(TensorPayload, u64)> {
        loop {
            if let Some(c) = ctx {
                c.status()?;
            }
            // Fast path: check already-buffered incoming P2P frames first.
            {
                let mut inbox = self.p2p_inbox.lock();
                if let Some(q) = inbox.get_mut(&src_rank)
                    && let Some(payload) = q.pop_front()
                {
                    let bytes = payload.byte_len();
                    return Ok((payload, bytes));
                }
            }
            // Try to become the cooperative acceptor on `self.listener`; if another fiber is
            // already accepting, wait on `p2p_notify`.
            let became_acceptor = {
                let mut busy = self.p2p_accept_busy.lock();
                if !*busy {
                    *busy = true;
                    true
                } else {
                    false
                }
            };
            if became_acceptor {
                let accept_res = self.listener.accept(ctx).await;
                let frame_res = match accept_res {
                    Ok(stream) => {
                        let kind = read_exact_stream(&stream, 1, ctx).await;
                        match kind {
                            Ok(k) if k[0] == CONN_KIND_P2P => recv_p2p_frame(&stream, ctx).await,
                            Ok(k) => Err(MoraError::Io(format!(
                                "tcp_process_group.recv: unexpected connection kind {:#x}",
                                k[0]
                            ))),
                            Err(e) => Err(e),
                        }
                    }
                    Err(e) => Err(e),
                };
                *self.p2p_accept_busy.lock() = false;
                match frame_res {
                    Ok((from_rank, payload, wire_bytes)) => {
                        if from_rank == src_rank {
                            self.p2p_notify.notify_waiters();
                            return Ok((payload, wire_bytes));
                        }
                        self.p2p_inbox
                            .lock()
                            .entry(from_rank)
                            .or_default()
                            .push_back(payload);
                        self.p2p_notify.notify_waiters();
                    }
                    Err(e) => {
                        self.p2p_notify.notify_waiters();
                        return Err(e);
                    }
                }
            } else {
                let notified = self.p2p_notify.notified();
                // Re-check inbox before sleeping so we never miss a wakeup.
                {
                    let mut inbox = self.p2p_inbox.lock();
                    if let Some(q) = inbox.get_mut(&src_rank)
                        && let Some(payload) = q.pop_front()
                    {
                        let bytes = payload.byte_len();
                        return Ok((payload, bytes));
                    }
                }
                match ctx {
                    Some(c) => {
                        match crate::runtime::scheduler::race2(c.cancelled(), notified).await {
                            crate::runtime::scheduler::Select2::First(err) => return Err(err),
                            crate::runtime::scheduler::Select2::Second(()) => {}
                        }
                    }
                    None => notified.await,
                }
            }
        }
    }
}

fn encode_chunk_frame(chunk_idx: usize, seq: u64, dtype: DType, vals: &[f64]) -> Vec<u8> {
    let elem_bytes = if dtype == DType::F32 { 4 } else { 8 };
    let mut buf = Vec::with_capacity(21 + vals.len() * elem_bytes);
    buf.extend_from_slice(&TCP_CHUNK_MAGIC.to_le_bytes());
    buf.extend_from_slice(&(chunk_idx as u32).to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.push(dtype_to_u8(dtype));
    buf.extend_from_slice(&(vals.len() as u32).to_le_bytes());
    if dtype == DType::F32 {
        for &v in vals {
            buf.extend_from_slice(&(v as f32).to_le_bytes());
        }
    } else {
        for &v in vals {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    buf
}

async fn recv_chunk_frame(
    stream: &MoraTcpStream,
    expected_seq: u64,
    ctx: Option<&TaskContext>,
) -> MoraResult<(usize, Vec<f64>, u64)> {
    let hdr = read_exact_stream(stream, 21, ctx).await?;
    let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    if magic != TCP_CHUNK_MAGIC {
        return Err(MoraError::Io(format!(
            "tcp_process_group: bad chunk frame magic {magic:#x}"
        )));
    }
    let chunk_idx = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    let seq = u64::from_le_bytes([
        hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
    ]);
    if seq != expected_seq {
        return Err(MoraError::Io(format!(
            "tcp_process_group: chunk seq mismatch (expected {expected_seq}, got {seq})"
        )));
    }
    let dtype = u8_to_dtype(hdr[16])?;
    let count = u32::from_le_bytes([hdr[17], hdr[18], hdr[19], hdr[20]]) as usize;
    let elem_bytes = if dtype == DType::F32 { 4 } else { 8 };
    let body = read_exact_stream(stream, count * elem_bytes, ctx).await?;
    let vals: Vec<f64> = if dtype == DType::F32 {
        body.as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(|c| f64::from(f32::from_le_bytes(c)))
            .collect()
    } else {
        body.as_chunks::<8>()
            .0
            .iter()
            .copied()
            .map(f64::from_le_bytes)
            .collect()
    };
    Ok((chunk_idx, vals, (21 + body.len()) as u64))
}

fn encode_p2p_frame(src_rank: usize, payload: &TensorPayload) -> Vec<u8> {
    let elem_bytes = if payload.dtype == DType::F32 { 4 } else { 8 };
    let header_len = 4 + 4 + 1 + 4 + payload.shape.len() * 8 + 8;
    let mut buf = Vec::with_capacity(header_len + payload.data.len() * elem_bytes);
    buf.extend_from_slice(&TCP_P2P_MAGIC.to_le_bytes());
    buf.extend_from_slice(&(src_rank as u32).to_le_bytes());
    buf.push(dtype_to_u8(payload.dtype));
    buf.extend_from_slice(&(payload.shape.len() as u32).to_le_bytes());
    for &d in &payload.shape {
        buf.extend_from_slice(&(d as u64).to_le_bytes());
    }
    buf.extend_from_slice(&(payload.data.len() as u64).to_le_bytes());
    if payload.dtype == DType::F32 {
        for &v in &payload.data {
            buf.extend_from_slice(&(v as f32).to_le_bytes());
        }
    } else {
        for &v in &payload.data {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    buf
}

async fn recv_p2p_frame(
    stream: &MoraTcpStream,
    ctx: Option<&TaskContext>,
) -> MoraResult<(usize, TensorPayload, u64)> {
    let hdr = read_exact_stream(stream, 13, ctx).await?;
    let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    if magic != TCP_P2P_MAGIC {
        return Err(MoraError::Io(format!(
            "tcp_process_group: bad p2p frame magic {magic:#x}"
        )));
    }
    let src_rank = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    let dtype = u8_to_dtype(hdr[8])?;
    let ndim = u32::from_le_bytes([hdr[9], hdr[10], hdr[11], hdr[12]]) as usize;
    let shape_bytes = read_exact_stream(stream, ndim * 8 + 8, ctx).await?;
    let mut shape = Vec::with_capacity(ndim);
    for c in shape_bytes[..ndim * 8].as_chunks::<8>().0 {
        shape.push(u64::from_le_bytes(*c) as usize);
    }
    let numel = u64::from_le_bytes(
        shape_bytes[ndim * 8..ndim * 8 + 8]
            .try_into()
            .expect("8 bytes"),
    ) as usize;
    let elem_bytes = if dtype == DType::F32 { 4 } else { 8 };
    let data_bytes = read_exact_stream(stream, numel * elem_bytes, ctx).await?;
    let data: Vec<f64> = if dtype == DType::F32 {
        data_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(|c| f64::from(f32::from_le_bytes(c)))
            .collect()
    } else {
        data_bytes
            .as_chunks::<8>()
            .0
            .iter()
            .copied()
            .map(f64::from_le_bytes)
            .collect()
    };
    let wire_bytes = (1 + 13 + shape_bytes.len() + data_bytes.len()) as u64;
    Ok((src_rank, TensorPayload { data, shape, dtype }, wire_bytes))
}

fn dtype_to_u8(dt: DType) -> u8 {
    match dt {
        DType::F64 => 0,
        DType::F32 => 1,
        DType::F16 => 2,
        DType::BF16 => 3,
        DType::FP8E4M3 => 4,
        DType::FP8E5M2 => 5,
        DType::I64 => 6,
        DType::I32 => 7,
        DType::I16 => 8,
        DType::I8 => 9,
        DType::I4 => 10,
        DType::U64 => 11,
        DType::U32 => 12,
        DType::U16 => 13,
        DType::U8 => 14,
        DType::Bool => 15,
    }
}

fn u8_to_dtype(tag: u8) -> MoraResult<DType> {
    match tag {
        0 => Ok(DType::F64),
        1 => Ok(DType::F32),
        2 => Ok(DType::F16),
        3 => Ok(DType::BF16),
        4 => Ok(DType::FP8E4M3),
        5 => Ok(DType::FP8E5M2),
        6 => Ok(DType::I64),
        7 => Ok(DType::I32),
        8 => Ok(DType::I16),
        9 => Ok(DType::I8),
        10 => Ok(DType::I4),
        11 => Ok(DType::U64),
        12 => Ok(DType::U32),
        13 => Ok(DType::U16),
        14 => Ok(DType::U8),
        15 => Ok(DType::Bool),
        other => Err(MoraError::Io(format!(
            "tcp_process_group: invalid DType tag {other}"
        ))),
    }
}

fn encode_ring_frame(src_rank: usize, seq: u64, payload: &TensorPayload) -> Vec<u8> {
    let header_len = 4 + 4 + 8 + 1 + 4 + payload.shape.len() * 8 + 8;
    let body_len = payload.data.len() * 8;
    let mut buf = Vec::with_capacity(header_len + body_len);
    buf.extend_from_slice(&TCP_RING_MAGIC.to_le_bytes());
    buf.extend_from_slice(&(src_rank as u32).to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.push(dtype_to_u8(payload.dtype));
    buf.extend_from_slice(&(payload.shape.len() as u32).to_le_bytes());
    for &d in &payload.shape {
        buf.extend_from_slice(&(d as u64).to_le_bytes());
    }
    buf.extend_from_slice(&(payload.data.len() as u64).to_le_bytes());
    for &v in &payload.data {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

async fn write_all_stream(
    stream: &MoraTcpStream,
    data: &[u8],
    ctx: Option<&TaskContext>,
) -> MoraResult<()> {
    let mut off = 0usize;
    while off < data.len() {
        let n = stream.write_bytes(&data[off..], ctx).await?;
        if n == 0 {
            return Err(MoraError::Io(
                "tcp_process_group: unexpected EOF while writing ring frame".to_string(),
            ));
        }
        off += n;
    }
    Ok(())
}

async fn read_exact_stream(
    stream: &MoraTcpStream,
    len: usize,
    ctx: Option<&TaskContext>,
) -> MoraResult<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let need = (len - out.len()).min(65536);
        let chunk = stream.read_bytes(need, ctx).await?;
        if chunk.is_empty() {
            return Err(MoraError::Io(
                "tcp_process_group: unexpected EOF while reading ring frame".to_string(),
            ));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn recv_ring_frame(
    stream: &MoraTcpStream,
    expected_seq: u64,
    ctx: Option<&TaskContext>,
) -> MoraResult<(usize, TensorPayload)> {
    let hdr = read_exact_stream(stream, 21, ctx).await?;
    let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    if magic != TCP_RING_MAGIC {
        return Err(MoraError::Io(format!(
            "tcp_process_group: bad frame magic {magic:#x}"
        )));
    }
    let src_rank = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    let seq = u64::from_le_bytes([
        hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
    ]);
    if seq != expected_seq {
        return Err(MoraError::Io(format!(
            "tcp_process_group: collective sequence mismatch (expected {expected_seq}, got {seq})"
        )));
    }
    let dtype = u8_to_dtype(hdr[16])?;
    let ndim = u32::from_le_bytes([hdr[17], hdr[18], hdr[19], hdr[20]]) as usize;
    let shape_bytes = read_exact_stream(stream, ndim * 8 + 8, ctx).await?;
    let mut shape = Vec::with_capacity(ndim);
    for i in 0..ndim {
        let base = i * 8;
        let d = u64::from_le_bytes([
            shape_bytes[base],
            shape_bytes[base + 1],
            shape_bytes[base + 2],
            shape_bytes[base + 3],
            shape_bytes[base + 4],
            shape_bytes[base + 5],
            shape_bytes[base + 6],
            shape_bytes[base + 7],
        ]) as usize;
        shape.push(d);
    }
    let n_base = ndim * 8;
    let numel = u64::from_le_bytes([
        shape_bytes[n_base],
        shape_bytes[n_base + 1],
        shape_bytes[n_base + 2],
        shape_bytes[n_base + 3],
        shape_bytes[n_base + 4],
        shape_bytes[n_base + 5],
        shape_bytes[n_base + 6],
        shape_bytes[n_base + 7],
    ]) as usize;
    let data_bytes = read_exact_stream(stream, numel * 8, ctx).await?;
    let mut data = Vec::with_capacity(numel);
    for i in 0..numel {
        let base = i * 8;
        data.push(f64::from_le_bytes([
            data_bytes[base],
            data_bytes[base + 1],
            data_bytes[base + 2],
            data_bytes[base + 3],
            data_bytes[base + 4],
            data_bytes[base + 5],
            data_bytes[base + 6],
            data_bytes[base + 7],
        ]));
    }
    Ok((src_rank, TensorPayload { data, shape, dtype }))
}

impl TcpRingState {
    /// Circulate every rank's tensor around the TCP ring in `world_size - 1` hops, returning
    /// all `world_size` payloads indexed by rank `0..world_size`.
    async fn ring_exchange_all(
        &self,
        rank: usize,
        world_size: usize,
        seq: u64,
        local: TensorPayload,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<(Vec<TensorPayload>, u64)> {
        if world_size == 1 {
            return Ok((vec![local], 0));
        }
        let mut slots: Vec<Option<TensorPayload>> = vec![None; world_size];
        slots[rank] = Some(local.clone());
        let mut to_send_rank = rank;
        let mut to_send_payload = local;
        let mut total_bytes: u64 = 0;

        for _hop in 0..(world_size - 1) {
            let encoded = encode_ring_frame(to_send_rank, seq, &to_send_payload);
            total_bytes = total_bytes.saturating_add(encoded.len() as u64);
            // Send to successor and receive from predecessor concurrently to prevent TCP buffer
            // deadlock on large tensors!
            let next = Arc::clone(&self.next_stream);
            let prev = Arc::clone(&self.prev_stream);
            let ctx_s = ctx.cloned();
            let ctx_r = ctx.cloned();
            let send_fut = async move { write_all_stream(&next, &encoded, ctx_s.as_ref()).await };
            let recv_fut = async move { recv_ring_frame(&prev, seq, ctx_r.as_ref()).await };
            let (s_res, r_res) = join2(send_fut, recv_fut).await;
            s_res?;
            let (recv_rank, recv_payload) = r_res?;
            if recv_rank >= world_size {
                return Err(MoraError::Io(format!(
                    "tcp_process_group: received out-of-range rank {recv_rank}"
                )));
            }
            slots[recv_rank] = Some(recv_payload.clone());
            to_send_rank = recv_rank;
            to_send_payload = recv_payload;
        }

        let all = slots
            .into_iter()
            .map(|opt| {
                opt.ok_or_else(|| {
                    MoraError::Io("tcp_process_group: incomplete ring exchange".to_string())
                })
            })
            .collect::<MoraResult<Vec<_>>>()?;
        Ok((all, total_bytes))
    }
}

/// Minimal zero-dependency 2-future concurrent join combinator.
async fn join2<A, B, FA, FB>(fa: FA, fb: FB) -> (A, B)
where
    FA: std::future::Future<Output = A>,
    FB: std::future::Future<Output = B>,
{
    let mut fa = std::pin::pin!(fa);
    let mut fb = std::pin::pin!(fb);
    let mut out_a: Option<A> = None;
    let mut out_b: Option<B> = None;
    std::future::poll_fn(|cx| {
        if out_a.is_none()
            && let std::task::Poll::Ready(a) = fa.as_mut().poll(cx)
        {
            out_a = Some(a);
        }
        if out_b.is_none()
            && let std::task::Poll::Ready(b) = fb.as_mut().poll(cx)
        {
            out_b = Some(b);
        }
        if out_a.is_some() && out_b.is_some() {
            std::task::Poll::Ready((
                out_a.take().expect("a ready"),
                out_b.take().expect("b ready"),
            ))
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

// ── 3. Unified First-Class `ProcessGroup` Handle ──

#[derive(Debug)]
enum Transport {
    InProc(Arc<InProcCollectiveState>),
    TcpRing(Arc<TcpRingState>),
}

/// First-class distributed tensor collective communicator (`Value::ProcessGroup`).
#[derive(Debug)]
pub struct ProcessGroup {
    rank: usize,
    world_size: usize,
    transport: Transport,
    next_seq: AtomicU64,
    stats: Arc<CollectiveStats>,
}

impl ProcessGroup {
    /// Construct an in-process collective communicator (returning rank 0; use `.ranks()` to get all ranks).
    pub(crate) fn new_inproc(world_size: usize) -> MoraResult<Arc<Self>> {
        if world_size == 0 {
            return Err(MoraError::Runtime(
                "process_group(world_size): world_size must be >= 1".to_string(),
            ));
        }
        Ok(Arc::new(Self {
            rank: 0,
            world_size,
            transport: Transport::InProc(InProcCollectiveState::new(world_size)),
            next_seq: AtomicU64::new(1),
            stats: Arc::new(CollectiveStats::default()),
        }))
    }

    /// Return all `world_size` rank handles sharing this in-process collective group.
    pub(crate) fn inproc_ranks(&self) -> MoraResult<Vec<Arc<Self>>> {
        let Transport::InProc(state) = &self.transport else {
            return Err(MoraError::Runtime(
                "ProcessGroup.ranks() is only valid on in-process groups; TCP ranks are created per-node via tcp_process_group(rank, world_size, addrs)".to_string(),
            ));
        };
        Ok((0..self.world_size)
            .map(|r| {
                Arc::new(Self {
                    rank: r,
                    world_size: self.world_size,
                    transport: Transport::InProc(Arc::clone(state)),
                    next_seq: AtomicU64::new(self.next_seq.load(Ordering::Relaxed)),
                    stats: Arc::clone(&self.stats),
                })
            })
            .collect())
    }

    /// Establish a live `AsyncIoReactor` TCP ring collective group for `rank` in `0..world_size`.
    pub(crate) async fn connect_tcp_ring(
        rank: usize,
        world_size: usize,
        addrs: Vec<String>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Self>> {
        if world_size == 0 {
            return Err(MoraError::Runtime(
                "tcp_process_group: world_size must be >= 1".to_string(),
            ));
        }
        if rank >= world_size {
            return Err(MoraError::Runtime(format!(
                "tcp_process_group: rank {rank} out of bounds for world_size {world_size}"
            )));
        }
        if addrs.len() != world_size {
            return Err(MoraError::Runtime(format!(
                "tcp_process_group: expected {world_size} peer addresses, got {}",
                addrs.len()
            )));
        }
        let listener = MoraTcpListener::bind(&addrs[rank]).await?;
        Self::connect_tcp_ring_with_listener(rank, world_size, addrs, listener, ctx).await
    }

    async fn connect_tcp_ring_with_listener(
        rank: usize,
        world_size: usize,
        addrs: Vec<String>,
        listener: Arc<MoraTcpListener>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Self>> {
        if world_size == 1 {
            return Ok(Arc::new(Self {
                rank: 0,
                world_size: 1,
                transport: Transport::InProc(InProcCollectiveState::new(1)),
                next_seq: AtomicU64::new(1),
                stats: Arc::new(CollectiveStats::default()),
            }));
        }

        let next_addr = addrs[(rank + 1) % world_size].clone();
        let accept_listener = Arc::clone(&listener);
        let ctx_a = ctx.cloned();
        let ctx_c = ctx.cloned();

        // Concurrently accept ring predecessor (stashing any early incoming P2P frames cleanly!)
        // while connecting to ring successor with the `CONN_KIND_RING` 1-byte connection tag.
        let accept_fut = async move {
            let mut early_p2p: HashMap<usize, VecDeque<TensorPayload>> = HashMap::new();
            loop {
                let stream = accept_listener.accept(ctx_a.as_ref()).await?;
                let kind = read_exact_stream(&stream, 1, ctx_a.as_ref()).await?;
                if kind[0] == CONN_KIND_RING {
                    return Ok::<_, MoraError>((stream, early_p2p));
                } else if kind[0] == CONN_KIND_P2P {
                    let (src_r, payload, _) = recv_p2p_frame(&stream, ctx_a.as_ref()).await?;
                    early_p2p.entry(src_r).or_default().push_back(payload);
                } else {
                    return Err(MoraError::Io(format!(
                        "tcp_process_group(rank={rank}): unexpected handshake byte {:#x}",
                        kind[0]
                    )));
                }
            }
        };
        let connect_fut = async move {
            let mut attempts = 0usize;
            loop {
                if let Some(c) = ctx_c.as_ref() {
                    c.status()?;
                }
                match MoraTcpStream::connect(
                    &next_addr,
                    Some(Duration::from_secs(2)),
                    ctx_c.as_ref(),
                )
                .await
                {
                    Ok(stream) => {
                        write_all_stream(&stream, &[CONN_KIND_RING], ctx_c.as_ref()).await?;
                        return Ok(stream);
                    }
                    Err(e) => {
                        attempts += 1;
                        if attempts >= 200 {
                            return Err(MoraError::Io(format!(
                                "tcp_process_group(rank={rank}): failed to connect to successor {next_addr}: {e}"
                            )));
                        }
                        sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        };

        let (prev_res, next_res) = join2(accept_fut, connect_fut).await;
        let (prev_stream, early_p2p) = prev_res?;
        let next_stream = next_res?;

        Ok(Arc::new(Self {
            rank,
            world_size,
            transport: Transport::TcpRing(Arc::new(TcpRingState {
                next_stream,
                prev_stream,
                addrs,
                listener,
                io_state: Mutex::new((false, 1)),
                io_notify: AsyncNotify::new(),
                p2p_accept_busy: Mutex::new(false),
                p2p_inbox: Mutex::new(early_p2p),
                p2p_notify: AsyncNotify::new(),
            })),
            next_seq: AtomicU64::new(1),
            stats: Arc::new(CollectiveStats::default()),
        }))
    }

    pub(crate) fn rank(&self) -> usize {
        self.rank
    }

    pub(crate) fn world_size(&self) -> usize {
        self.world_size
    }

    pub(crate) fn backend_name(&self) -> &'static str {
        match &self.transport {
            Transport::InProc(_) => "inproc",
            Transport::TcpRing(_) => "tcp-ring",
        }
    }

    pub(crate) fn stats_map(&self) -> HashMap<String, i64> {
        let mut m = HashMap::new();
        m.insert("rank".to_string(), self.rank as i64);
        m.insert("world_size".to_string(), self.world_size as i64);
        m.insert(
            "all_reduce_calls".to_string(),
            self.stats.all_reduce_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "all_gather_calls".to_string(),
            self.stats.all_gather_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "broadcast_calls".to_string(),
            self.stats.broadcast_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "reduce_scatter_calls".to_string(),
            self.stats.reduce_scatter_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "barrier_calls".to_string(),
            self.stats.barrier_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "send_calls".to_string(),
            self.stats.send_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "recv_calls".to_string(),
            self.stats.recv_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "split_calls".to_string(),
            self.stats.split_calls.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "bytes_transferred".to_string(),
            self.stats.bytes_transferred.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "wire_bytes_sent".to_string(),
            self.stats.wire_bytes_sent.load(Ordering::Relaxed) as i64,
        );
        m.insert(
            "wire_bytes_recv".to_string(),
            self.stats.wire_bytes_recv.load(Ordering::Relaxed) as i64,
        );
        m
    }

    /// Synchronously reserve the next monotonic collective sequence ID in caller stream order.
    /// Used by `all_reduce_async` so pipelined non-blocking collectives always match across ranks
    /// even when spawned concurrently on multi-threaded scheduler workers.
    pub(crate) fn allocate_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::AcqRel)
    }

    async fn exchange_payloads_with_seq(
        &self,
        seq: u64,
        local: TensorPayload,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<TensorPayload>> {
        match &self.transport {
            Transport::InProc(st) => {
                let bytes = local.byte_len();
                let all = st.exchange(seq, self.rank, local, ctx).await?;
                self.stats
                    .bytes_transferred
                    .fetch_add(bytes, Ordering::Relaxed);
                Ok(all)
            }
            Transport::TcpRing(ring) => {
                ring.acquire_io(seq).await;
                let res = ring
                    .ring_exchange_all(self.rank, self.world_size, seq, local, ctx)
                    .await;
                ring.release_io();
                let (all, bytes) = res?;
                self.stats
                    .bytes_transferred
                    .fetch_add(bytes, Ordering::Relaxed);
                self.stats
                    .wire_bytes_sent
                    .fetch_add(bytes, Ordering::Relaxed);
                self.stats
                    .wire_bytes_recv
                    .fetch_add(bytes, Ordering::Relaxed);
                Ok(all)
            }
        }
    }

    async fn exchange_payloads(
        &self,
        local: TensorPayload,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<TensorPayload>> {
        let seq = self.allocate_seq();
        self.exchange_payloads_with_seq(seq, local, ctx).await
    }

    /// Collective `all_reduce(tensor, op)`: reduces elementwise across all ranks and returns
    /// identical result tensor preserving `DType` and GPU/CPU `Device`.
    ///
    /// On `TcpRing` with `numel >= world_size >= 2`, executes true **2-phase bandwidth-optimal
    /// Scatter-Reduce + AllGather** (`2 * (W - 1) / W * S` wire bytes per rank).
    pub(crate) async fn all_reduce(
        &self,
        tensor: Arc<Tensor>,
        op: ReduceOp,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        let seq = self.allocate_seq();
        self.all_reduce_with_seq(seq, tensor, op, ctx).await
    }

    /// Launch-ordered `all_reduce` taking a pre-allocated collective sequence ID `seq`.
    pub(crate) async fn all_reduce_with_seq(
        &self,
        seq: u64,
        tensor: Arc<Tensor>,
        op: ReduceOp,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        let _trace = TraceScope::enter(
            "distributed",
            format!("all_reduce({})", op.name()),
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("world_size".to_string(), self.world_size.to_string()),
                ("backend".to_string(), self.backend_name().to_string()),
            ],
        );
        let dev = tensor.device.clone();
        let dtype = tensor.dtype;
        let shape = tensor.shape.clone();
        let local = TensorPayload::from_tensor(&tensor);

        if let Transport::TcpRing(ring) = &self.transport
            && self.world_size >= 2
            && local.data.len() >= self.world_size
        {
            ring.acquire_io(seq).await;
            let res = ring
                .ring_all_reduce_2phase(self.rank, self.world_size, seq, local.data, dtype, op, ctx)
                .await;
            ring.release_io();
            let (reduced_data, sent_bytes, recv_bytes) = res?;
            self.stats
                .bytes_transferred
                .fetch_add(sent_bytes, Ordering::Relaxed);
            self.stats
                .wire_bytes_sent
                .fetch_add(sent_bytes, Ordering::Relaxed);
            self.stats
                .wire_bytes_recv
                .fetch_add(recv_bytes, Ordering::Relaxed);
            self.stats.all_reduce_calls.fetch_add(1, Ordering::Relaxed);
            return TensorPayload {
                data: reduced_data,
                shape,
                dtype,
            }
            .into_tensor_on_device(&dev);
        }

        let all = self.exchange_payloads_with_seq(seq, local, ctx).await?;
        let reduced = reduce_payloads(&all, &shape, dtype, op)?;
        self.stats.all_reduce_calls.fetch_add(1, Ordering::Relaxed);
        reduced.into_tensor_on_device(&dev)
    }

    /// P5-3: Point-to-point cooperative non-blocking `send(tensor, dst_rank)` for Pipeline Parallelism.
    pub(crate) async fn send(
        &self,
        tensor: Arc<Tensor>,
        dst_rank: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<()> {
        if dst_rank >= self.world_size || dst_rank == self.rank {
            return Err(MoraError::Runtime(format!(
                "ProcessGroup.send: invalid dst_rank {dst_rank} from rank {} (world_size={})",
                self.rank, self.world_size
            )));
        }
        let _trace = TraceScope::enter(
            "distributed",
            format!("p2p_send({}->{dst_rank})", self.rank),
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("dst_rank".to_string(), dst_rank.to_string()),
                ("backend".to_string(), self.backend_name().to_string()),
            ],
        );
        match &self.transport {
            Transport::InProc(st) => {
                let bytes = tensor.nbytes() as u64;
                st.p2p_send(self.rank, dst_rank, tensor);
                self.stats
                    .bytes_transferred
                    .fetch_add(bytes, Ordering::Relaxed);
            }
            Transport::TcpRing(ring) => {
                let payload = TensorPayload::from_tensor(&tensor);
                let sent = ring.p2p_send(self.rank, dst_rank, &payload, ctx).await?;
                self.stats
                    .bytes_transferred
                    .fetch_add(sent, Ordering::Relaxed);
                self.stats
                    .wire_bytes_sent
                    .fetch_add(sent, Ordering::Relaxed);
            }
        }
        self.stats.send_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// P5-3: Point-to-point cooperative `recv(src_rank)` honoring `TaskContext` cancellation.
    pub(crate) async fn recv(
        &self,
        src_rank: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        if src_rank >= self.world_size || src_rank == self.rank {
            return Err(MoraError::Runtime(format!(
                "ProcessGroup.recv: invalid src_rank {src_rank} on rank {} (world_size={})",
                self.rank, self.world_size
            )));
        }
        let _trace = TraceScope::enter(
            "distributed",
            format!("p2p_recv({src_rank}->{})", self.rank),
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("src_rank".to_string(), src_rank.to_string()),
                ("backend".to_string(), self.backend_name().to_string()),
            ],
        );
        let out = match &self.transport {
            Transport::InProc(st) => st.p2p_recv(src_rank, self.rank, ctx).await?,
            Transport::TcpRing(ring) => {
                let (payload, recv_bytes) = ring.p2p_recv(src_rank, ctx).await?;
                self.stats
                    .wire_bytes_recv
                    .fetch_add(recv_bytes, Ordering::Relaxed);
                payload.into_tensor_on_device(&Device::Cpu)?
            }
        };
        self.stats.recv_calls.fetch_add(1, Ordering::Relaxed);
        Ok(out)
    }

    /// P5-3: Collective `split(color, key) -> ProcessGroup` partitioning the world into orthogonal
    /// sub-communicators (e.g. 2D Tensor-Parallel × Data-Parallel meshes) with deterministic
    /// rank ordering by `(key, old_rank)`.
    pub(crate) async fn split(
        &self,
        color: i64,
        key: i64,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Self>> {
        let _trace = TraceScope::enter(
            "distributed",
            format!("split(color={color})"),
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("world_size".to_string(), self.world_size.to_string()),
                ("color".to_string(), color.to_string()),
            ],
        );
        let split_seq = self.next_seq.load(Ordering::Acquire);
        let meta = TensorPayload {
            data: vec![f64::from_bits(color as u64), f64::from_bits(key as u64)],
            shape: vec![2],
            dtype: DType::F64,
        };
        let all_meta = self.exchange_payloads(meta, ctx).await?;
        let mut members: Vec<(i64, usize)> = Vec::new();
        for (r, p) in all_meta.iter().enumerate() {
            let p_color = p.data[0].to_bits() as i64;
            let p_key = p.data[1].to_bits() as i64;
            if p_color == color {
                members.push((p_key, r));
            }
        }
        members.sort_by_key(|&(k, old_r)| (k, old_r));
        let new_world_size = members.len();
        let new_rank = members
            .iter()
            .position(|&(_, old_r)| old_r == self.rank)
            .expect("self rank always in its own color group");

        self.stats.split_calls.fetch_add(1, Ordering::Relaxed);

        match &self.transport {
            Transport::InProc(st) => {
                let sub_state = st.get_or_create_split_group(split_seq, color, new_world_size);
                Ok(Arc::new(Self {
                    rank: new_rank,
                    world_size: new_world_size,
                    transport: Transport::InProc(sub_state),
                    next_seq: AtomicU64::new(1),
                    stats: Arc::new(CollectiveStats::default()),
                }))
            }
            Transport::TcpRing(ring) => {
                // Bind an ephemeral port on the same host IP as our parent listener, exchange
                // assigned ports across the parent world, and connect an independent TCP sub-ring!
                let host = ring
                    .addrs
                    .get(self.rank)
                    .and_then(|a| a.rsplit_once(':').map(|(h, _)| h))
                    .unwrap_or("127.0.0.1");
                let sub_listener = MoraTcpListener::bind(&format!("{host}:0")).await?;
                let local_port: u16 = sub_listener
                    .local_addr()
                    .rsplit_once(':')
                    .and_then(|(_, p)| p.parse().ok())
                    .ok_or_else(|| {
                        MoraError::Io(format!(
                            "tcp_process_group.split: cannot parse local port from {}",
                            sub_listener.local_addr()
                        ))
                    })?;
                let port_payload = TensorPayload {
                    data: vec![f64::from(local_port)],
                    shape: vec![1],
                    dtype: DType::F64,
                };
                let all_ports = self.exchange_payloads(port_payload, ctx).await?;
                let mut sub_addrs = Vec::with_capacity(new_world_size);
                for &(_, old_r) in &members {
                    let h = ring
                        .addrs
                        .get(old_r)
                        .and_then(|a| a.rsplit_once(':').map(|(h, _)| h))
                        .unwrap_or("127.0.0.1");
                    let p = all_ports[old_r].data[0] as u16;
                    sub_addrs.push(format!("{h}:{p}"));
                }
                Self::connect_tcp_ring_with_listener(
                    new_rank,
                    new_world_size,
                    sub_addrs,
                    sub_listener,
                    ctx,
                )
                .await
            }
        }
    }

    /// Collective `all_gather(tensor)`: gathers rank tensors in rank order `0..world_size`
    /// and concatenates along axis 0 (for 0D scalars, produces `[world_size]`).
    pub(crate) async fn all_gather(
        &self,
        tensor: Arc<Tensor>,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        let _trace = TraceScope::enter(
            "distributed",
            "all_gather",
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("world_size".to_string(), self.world_size.to_string()),
                ("backend".to_string(), self.backend_name().to_string()),
            ],
        );
        let dev = tensor.device.clone();
        let dtype = tensor.dtype;
        let base_shape = tensor.shape.clone();
        let local = TensorPayload::from_tensor(&tensor);
        let all = self.exchange_payloads(local, ctx).await?;
        for (r, p) in all.iter().enumerate() {
            if p.shape != base_shape || p.dtype != dtype {
                return Err(MoraError::Runtime(format!(
                    "all_gather: rank {r} tensor shape/dtype {:?}/{:?} mismatches rank {} {:?}/{:?}",
                    p.shape, p.dtype, self.rank, base_shape, dtype
                )));
            }
        }
        let mut out_shape = if base_shape.is_empty() {
            vec![self.world_size]
        } else {
            let mut s = base_shape.clone();
            s[0] *= self.world_size;
            s
        };
        if out_shape.is_empty() {
            out_shape.push(self.world_size);
        }
        let total_elems: usize = all.iter().map(|p| p.data.len()).sum();
        let mut concat = Vec::with_capacity(total_elems);
        for p in all {
            concat.extend_from_slice(&p.data);
        }
        self.stats.all_gather_calls.fetch_add(1, Ordering::Relaxed);
        TensorPayload {
            data: concat,
            shape: out_shape,
            dtype,
        }
        .into_tensor_on_device(&dev)
    }

    /// Collective `broadcast(tensor, root)`: replicates `root`'s tensor across every rank.
    pub(crate) async fn broadcast(
        &self,
        tensor: Arc<Tensor>,
        root: usize,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        if root >= self.world_size {
            return Err(MoraError::Runtime(format!(
                "broadcast: root rank {root} out of bounds for world_size {}",
                self.world_size
            )));
        }
        let _trace = TraceScope::enter(
            "distributed",
            format!("broadcast(root={root})"),
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("world_size".to_string(), self.world_size.to_string()),
                ("backend".to_string(), self.backend_name().to_string()),
            ],
        );
        let dev = tensor.device.clone();
        let local = TensorPayload::from_tensor(&tensor);
        let all = self.exchange_payloads(local, ctx).await?;
        self.stats.broadcast_calls.fetch_add(1, Ordering::Relaxed);
        all[root].clone().into_tensor_on_device(&dev)
    }

    /// Collective `reduce_scatter(tensor, op)`: reduces across all ranks and scatters axis-0
    /// equal slice `rank` (`shape[0] % world_size == 0`) back to each rank.
    pub(crate) async fn reduce_scatter(
        &self,
        tensor: Arc<Tensor>,
        op: ReduceOp,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Arc<Tensor>> {
        let _trace = TraceScope::enter(
            "distributed",
            format!("reduce_scatter({})", op.name()),
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("world_size".to_string(), self.world_size.to_string()),
                ("backend".to_string(), self.backend_name().to_string()),
            ],
        );
        let dev = tensor.device.clone();
        let dtype = tensor.dtype;
        let shape = tensor.shape.clone();
        if shape.is_empty() || !shape[0].is_multiple_of(self.world_size) {
            return Err(MoraError::Runtime(format!(
                "reduce_scatter: axis 0 of shape {:?} must be divisible by world_size {}",
                shape, self.world_size
            )));
        }
        let local = TensorPayload::from_tensor(&tensor);
        let all = self.exchange_payloads(local, ctx).await?;
        let reduced = reduce_payloads(&all, &shape, dtype, op)?;
        let chunk_d0 = shape[0] / self.world_size;
        let inner_stride: usize = shape[1..].iter().product::<usize>().max(1);
        let chunk_elems = chunk_d0 * inner_stride;
        let start = self.rank * chunk_elems;
        let end = start + chunk_elems;
        let mut out_shape = shape;
        out_shape[0] = chunk_d0;
        self.stats
            .reduce_scatter_calls
            .fetch_add(1, Ordering::Relaxed);
        TensorPayload {
            data: reduced.data[start..end].to_vec(),
            shape: out_shape,
            dtype,
        }
        .into_tensor_on_device(&dev)
    }

    /// Collective `barrier()`: synchronizes all ranks before proceeding.
    pub(crate) async fn barrier(&self, ctx: Option<&TaskContext>) -> MoraResult<()> {
        let _trace = TraceScope::enter(
            "distributed",
            "barrier",
            vec![
                ("rank".to_string(), self.rank.to_string()),
                ("world_size".to_string(), self.world_size.to_string()),
            ],
        );
        let token = TensorPayload {
            data: vec![self.rank as f64],
            shape: vec![1],
            dtype: DType::F64,
        };
        let _ = self.exchange_payloads(token, ctx).await?;
        self.stats.barrier_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn reduce_payloads(
    all: &[TensorPayload],
    shape: &[usize],
    dtype: DType,
    op: ReduceOp,
) -> MoraResult<TensorPayload> {
    let n = all[0].data.len();
    for (r, p) in all.iter().enumerate() {
        if p.shape != shape || p.dtype != dtype || p.data.len() != n {
            return Err(MoraError::Runtime(format!(
                "ProcessGroup reduce: rank {r} tensor {:?}/{:?} mismatches expected {:?}/{:?}",
                p.shape, p.dtype, shape, dtype
            )));
        }
    }
    let mut acc = all[0].data.clone();
    for p in &all[1..] {
        match op {
            ReduceOp::Sum | ReduceOp::Mean => {
                for (d, &v) in acc.iter_mut().zip(p.data.iter()) {
                    *d += v;
                }
            }
            ReduceOp::Max => {
                for (d, &v) in acc.iter_mut().zip(p.data.iter()) {
                    if v > *d || v.is_nan() {
                        *d = v;
                    }
                }
            }
            ReduceOp::Min => {
                for (d, &v) in acc.iter_mut().zip(p.data.iter()) {
                    if v < *d || v.is_nan() {
                        *d = v;
                    }
                }
            }
        }
    }
    if op == ReduceOp::Mean {
        let denom = all.len() as f64;
        for d in &mut acc {
            *d /= denom;
        }
    }
    if dtype != DType::F64 {
        for d in &mut acc {
            *d = quantize_for_dtype(*d, dtype);
        }
    }
    Ok(TensorPayload {
        data: acc,
        shape: shape.to_vec(),
        dtype,
    })
}
