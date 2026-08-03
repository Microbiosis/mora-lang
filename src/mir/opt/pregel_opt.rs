// v0.75.60: 自 opt.rs 按 pass 组拆出（D6 单文件惯例）。
// Pregel 专属优化：superstep_fusion + optimize_pregel。

use crate::mir::{MirFunction, MirInst};

pub fn superstep_fusion(func: &mut MirFunction) {
    let mut i = 0;
    while i + 1 < func.body.len() {
        if let (MirInst::Orchestrate { kind: kind1, .. }, MirInst::Orchestrate { kind: kind2, .. }) =
            (&func.body[i], &func.body[i + 1])
            && kind1 == kind2
        {
            func.body.remove(i + 1);
            continue; // re-check current position with new neighbor
        }
        i += 1;
    }
}

/// 运行所有 Pregel 优化 pass（当前仅 superstep_fusion）
pub fn optimize_pregel(func: &mut MirFunction) {
    superstep_fusion(func);
}
