# 容错 + 再平衡（v0.74）

## 探索发现的 4 个关键问题

1. **🔴 并行 hang bug**（必须修）：worker 里 agent 出错 → `panic!` → 线程死 → 不出结果 → `run_batch` 的 `recv()` 永久阻塞。**整个 BSP 挂死而非报错。**
2. **无故障回滚**：agent 出错只 `?` 向上传播，无"回滚到最近 checkpoint 重跑"。
3. **无超时**：`MirInterruptWhen::Timeout(u64)` 是死变体，从不构造/匹配。
4. **无 checkpoint 保留**：`max_checkpoints` 从不被读取，MemorySaver 无限增长。

另有：`WorkerPool` 每超步重建（无法积累指标）；`restore_checkpoint` 不恢复 `vertex_state`；`Interpreter::rewind` 的 ID 格式与引擎写出的 UUID 不匹配（死代码）。

## 方案（单进程务实版：粗粒度步级容错 + 动态再平衡）

### Step 1: 修复并行 hang（`worker_pool.rs` + 引擎）
- worker 闭包改为返回 `Result<AgentExecOutcome, String>`（出错不再 panic）
- `run_batch` 增加 `run_batch_with_timeout(jobs, Option<Duration>)`：用 `recv_timeout` 检测挂死；超时返回 `BatchResult { outcomes, timed_out: bool }`
- 引擎并行路径收 `Err` 而非 panic → 走故障处理

### Step 2: 步级故障容错（`mir_pregel_engine.rs`）
- `with_fault_tolerance(max_retries: usize)` builder（默认关，保持现有行为）
- 每步开始自动存 checkpoint（step-start 快照）
- 整步（EXEC+UPDATE+ADVANCE）包进重试循环：
  ```
  for attempt in 0..=max_retries:
      checkpoint 已存（step-start）
      run step
      if 成功 → break
      else → restore_checkpoint(step-start) → 重跑该步
  ```
- 重试耗尽 → 返回明确错误（含 agent 名 + 尝试次数）
- `restore_checkpoint` 补恢复 `vertex_state`（清空，重跑会重建）

### Step 3: 再平衡 + 指标（`worker_pool.rs` + 引擎）
- **缓存 WorkerPool**：引擎持有 `Option<WorkerPool>`，超步间复用（当前每步新建）
- 共享队列本就是动态 work-stealing（LIFO pop）——已是分区平衡
- 新增引擎 `stats()`：`steps`、`agents_run`、`total_ms`、`retries`、`timeouts`
- 超步内按 agent 定义序 push（LIFO pop → 先定义的先跑，确定性）

### Step 4: 超时（引擎 + pool）
- `with_step_timeout(Duration)` builder（默认 None）
- 每步 EXEC 用 `run_batch_with_timeout`；超时 → 视为故障 → 走 Step 2 重试
- 超时 worker 线程泄漏（Rust 无线程取消）——文档记录，池重建丢弃

### Step 5: checkpoint 保留（引擎）
- auto-save 时若 `max_checkpoints` 设了 → `saver.list` + 删最旧超限的
- 修复 `Interpreter::rewind` 的 UUID ID 解析 bug（改为按 thread_id+step 语义或直接用 saver）

## 改动范围

| 文件 | 改动 | 行数 |
|------|------|------|
| `worker_pool.rs` | +`run_batch_with_timeout` + `BatchResult` | +40 |
| `mir_pregel_engine.rs` | FT 重试循环 + 池缓存 + `stats()` + 超时 + 保留 + restore 修复 | +120/-20 |
| `interpreter/mod.rs` | `rewind` ID 修复 | +5 |
| 测试 | FT 回滚测试 + 超时测试 + 保留测试 | +40 |

净增 ~185 行。交付：并行 hang 修复（关键）+ 步级容错 + 再平衡指标 + 超时 + 保留。

## 取舍（文档记录）
- 单进程 = 无真实机器分布；"容错"实现为 checkpoint 回滚重试（粗粒度步级），与 Pregel 的"从 checkpoint 重启"语义一致
- 超时后 worker 线程泄漏（无协作取消点）——用池重建隔离
- 默认全关（`fault_tolerance`/`timeout`），现有行为与测试不受影响