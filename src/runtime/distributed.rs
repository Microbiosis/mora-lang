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

use std::collections::HashMap;
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
    bytes_transferred: AtomicU64,
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

#[derive(Debug)]
struct InProcCollectiveState {
    world_size: usize,
    /// Per-generation staging table keyed by collective sequence number.
    gens: Mutex<HashMap<u64, GenBucket>>,
    notify: AsyncNotify,
}

impl InProcCollectiveState {
    fn new(world_size: usize) -> Arc<Self> {
        Arc::new(Self {
            world_size,
            gens: Mutex::new(HashMap::new()),
            notify: AsyncNotify::new(),
        })
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

// ── 2. Live TCP Ring Collective Transport (`AsyncIoReactor` TCP Sockets) ──

const TCP_RING_MAGIC: u32 = 0x4D50_4752; // "MPGR"

#[derive(Debug)]
struct TcpRingState {
    /// Outgoing ring edge `rank -> (rank + 1) % world_size`.
    next_stream: Arc<MoraTcpStream>,
    /// Incoming ring edge `(rank + world_size - 1) % world_size -> rank`.
    prev_stream: Arc<MoraTcpStream>,
    /// Keep listener alive until shutdown.
    _listener: Arc<MoraTcpListener>,
    /// Async-safe lock serializing collective frames on this rank handle without holding
    /// a non-`Send` `MutexGuard` across `.await`.
    io_busy: Mutex<bool>,
    io_notify: AsyncNotify,
}

impl TcpRingState {
    async fn acquire_io(&self) {
        loop {
            let notified = self.io_notify.notified();
            {
                let mut busy = self.io_busy.lock();
                if !*busy {
                    *busy = true;
                    return;
                }
            }
            notified.await;
        }
    }

    fn release_io(&self) {
        *self.io_busy.lock() = false;
        self.io_notify.notify_one();
    }
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
        if world_size == 1 {
            // Single-rank loopback dummy connection not needed; self-transport short-circuits!
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

        // Concurrently accept predecessor while retrying connection to successor until all peers bind!
        let accept_fut = async move { accept_listener.accept(ctx_a.as_ref()).await };
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
                    Ok(stream) => return Ok(stream),
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
        let prev_stream = prev_res?;
        let next_stream = next_res?;

        Ok(Arc::new(Self {
            rank,
            world_size,
            transport: Transport::TcpRing(Arc::new(TcpRingState {
                next_stream,
                prev_stream,
                _listener: listener,
                io_busy: Mutex::new(false),
                io_notify: AsyncNotify::new(),
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
            "bytes_transferred".to_string(),
            self.stats.bytes_transferred.load(Ordering::Relaxed) as i64,
        );
        m
    }

    async fn exchange_payloads(
        &self,
        local: TensorPayload,
        ctx: Option<&TaskContext>,
    ) -> MoraResult<Vec<TensorPayload>> {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
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
                ring.acquire_io().await;
                let res = ring
                    .ring_exchange_all(self.rank, self.world_size, seq, local, ctx)
                    .await;
                ring.release_io();
                let (all, bytes) = res?;
                self.stats
                    .bytes_transferred
                    .fetch_add(bytes, Ordering::Relaxed);
                Ok(all)
            }
        }
    }

    /// Collective `all_reduce(tensor, op)`: reduces elementwise across all ranks and returns
    /// identical result tensor preserving `DType` and GPU/CPU `Device`.
    pub(crate) async fn all_reduce(
        &self,
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
        let all = self.exchange_payloads(local, ctx).await?;
        let reduced = reduce_payloads(&all, &shape, dtype, op)?;
        self.stats.all_reduce_calls.fetch_add(1, Ordering::Relaxed);
        reduced.into_tensor_on_device(&dev)
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
