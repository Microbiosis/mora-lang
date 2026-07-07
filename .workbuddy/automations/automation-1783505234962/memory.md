# 架构检测自动化执行记录

## 2026-08-03 架构与 Bug 检测（v0.75.53）
- **报告**: `ARCHITECTURE_BUG_DETECTION_REPORT_2026-08-03.md`
- **编译**: cargo build ✅ / fmt ✅ / clippy --all-targets --all-features -D warnings ✅
- **测试**: 集成 62 passed / MIR 86 passed / Interpreter 103 passed (4 ignored) / Lib 611 tests 后台运行
- **unwrap 精确值**: 生产仅 **2** 处（上期 29，大幅下降因精确过滤了 `#[cfg(test)]` 块）
- **panic!**: 生产 **3** 处（ssa.rs×2, cli/mod.rs×1）
- **expect**: 生产 **33** 处（trace_collector.rs 10, main.rs 5, builtins/sandbox.rs 4, ai_chat.rs 4）
- **P1**: trace_collector.rs 10 处 `.lock().expect()` — Mutex 中毒连锁 panic（上期 P3 升级为 P1）
- **P1**: cli/mod.rs panic — 编译失败直接 panic
- **P2**: parser_v3/mod.rs 3226 行（最大文件）/ pregel/mod.rs 2549 行（第二大）
- **P2**: builtins/mod.rs 3319 行含 14 个 `#[cfg(test)]` 块（82% 为测试代码，应迁出）
- **改善**: fmt 3 文件违规已修复 / clippy 全 feature 通过 / TODO 8→3 / builtins 从 5154 行拆分为 13 子文件
- **架构**: vm.rs 合并 interp+dag_interp（P4）/ CLI 拆分子模块（P9）/ dispatch 静态表（P5,6）

## 2026-08-02 架构与 Bug 检测（v0.75.34）
- **报告**: `ARCHITECTURE_BUG_DETECTION_REPORT_2026-08-02.md`
- **编译**: cargo build ✅ / clippy (默认) ✅ / clippy (--all-features) ⚠️ jit 需系统 LLVM
- **fmt**: ❌ 3 文件违规（dag.rs / handlers.rs / dag_search.rs）— v0.75.34 提交未 fmt
- **测试**: 核心 199/0 通过（MIR 83 + 集成 116）；全量 cargo test 含 OCR >45min 未跑完
- **unwrap**: 总 338（生产 29 / 测试 309）— 较 7-29 的 835 大幅下降
- **panic!**: 生产仅 3 处（main.rs + ssa.rs×2）
- **P1**: fmt 违规（违反 CI 红线）
- **P2**: dag_interp.rs:10-18 注释过时（声称循环走线性 run_mir，实际 run_mir→DAG 路径）；typeck/mod.rs ~360 行死代码（旧 TypeChecker 零调用）
- **P2**: worker_pool 超时线程泄漏（已文档记录）
- **P3**: trace_collector.rs 10 处 .lock().unwrap()（Mutex 中毒连锁 panic）
- **改善**: ai_infra 死代码已解决（v0.75.25 迁移）；unwrap 974→338（-65%）；TODO 29→8
- **架构**: Interpreter 7 facade 稳定；builtins/mod.rs 4888 LOC 仍是最大文件
- ****
  - **🔴 P0**commit a9770e5 mir_pregel_engine.rs  9  76+84
  - mir/expr.rs MirExpr E0072BoxCommand/Send/EvalTest
  - mir_pregel_engine.rs MirExpr structenumE0574 15
  - clippy/test 
  - fmt 116mir/expr.rs + mir_pregel_engine.rs
  - unwrap 464 / panic 146 / expect 225 = 835974 -139
  - ai_infra.rs 65 dead_code 8.3% 
  - TODO/FIXME 29 / 9mir_pregel_engine.rs 13
  - Interpreter 7 facadeADR-001
  -  0.0.53v0.55

## 2026-07-11  Bug 
- ****
- ****`ARCHITECTURE_BUG_DETECTION_REPORT_2026-07-11.md`
- ****
  - build/test/clippy/fmt clippy+fmt 
  -  755→863+108
  - unwrap 423→473+50 974unwrap+panic+expect
  - ai_infra.rs 65 dead_code 8.3% 
  - ai_chat.rs 865  Bug 
  - runtime ↔ interpreter  5 
  - runtime 34  pub 
  - checkpoint/sqlite.rs unwrap  104/
