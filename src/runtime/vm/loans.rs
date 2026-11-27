//! 借用指向暂停帧内的未捕获本地槽。栈位置在一次调用期间固定；
//! 连续执行与协作式挂起保留这些位置，截取续延之前将被截取的借用转为快照。
use super::continuation::{CallFrame, LocalCell, RegSlot, ScriptFrame};
use crate::mir::control::{LocalSlot, VReg};
use crate::runtime::fbip::FbipCounters;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlotAddress {
    depth: usize,
    local: LocalSlot,
}

impl SlotAddress {
    fn resolve<'a>(self, parents: &'a [CallFrame], current: &'a ScriptFrame) -> &'a RegSlot {
        let owner = if self.depth == parents.len() {
            current
        } else if let CallFrame::Script(owner) = &parents[self.depth] {
            owner
        } else {
            unreachable!("借用地址必须指向脚本调用帧")
        };
        match &owner.locals[self.local.0 as usize] {
            LocalCell::Val(slot) => slot,
            LocalCell::Shared(_) => unreachable!("被闭包捕获的槽不能作为借用源"),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct FrameLoans {
    parameters: Vec<(LocalSlot, SlotAddress)>,
    registers: Vec<(VReg, LocalSlot)>,
}

/// 先从寄存器取出所有实参，再同时借用本地槽。这样参数求值和绑定无需重叠的可变引用。
pub(crate) enum PendingArg {
    Owned(RegSlot),
    Borrowed(SlotAddress),
}

impl PendingArg {
    pub(crate) fn bind<'a>(self, parents: &'a [CallFrame], frame: &'a ScriptFrame) -> CallArg<'a> {
        match self {
            Self::Owned(slot) => CallArg::Owned(slot),
            Self::Borrowed(source) => CallArg::Borrowed(source, source.resolve(parents, frame)),
        }
    }
}

pub(crate) enum CallArg<'a> {
    Owned(RegSlot),
    Borrowed(SlotAddress, &'a RegSlot),
}

impl std::ops::Deref for CallArg<'_> {
    type Target = RegSlot;
    fn deref(&self) -> &RegSlot {
        match self {
            Self::Owned(slot) => slot,
            Self::Borrowed(_, slot) => slot,
        }
    }
}

impl CallArg<'_> {
    pub(crate) fn from_value(value: crate::value::Value) -> Self {
        Self::Owned(RegSlot::from_value(value))
    }

    pub(crate) fn take_value(self) -> crate::value::Value {
        match self {
            Self::Owned(slot) => slot.take_value(),
            Self::Borrowed(_, slot) => slot.to_value(),
        }
    }

    pub(crate) fn bind_parameter(
        self,
        local: LocalSlot,
        may_borrow: bool,
        loans: &mut FrameLoans,
    ) -> RegSlot {
        match self {
            Self::Owned(slot) => slot,
            Self::Borrowed(source, _) if may_borrow => {
                loans.parameters.push((local, source));
                RegSlot::Nil
            }
            Self::Borrowed(_, slot) => slot.clone(),
        }
    }
}

impl ScriptFrame {
    fn local_address(&self, local: LocalSlot, depth: usize) -> SlotAddress {
        self.loans
            .parameters
            .iter()
            .find(|(p, _)| *p == local)
            .map_or(SlotAddress { depth, local }, |(_, source)| *source)
    }

    pub(crate) fn borrow_local(&mut self, dst: VReg, local: LocalSlot, parents: &[CallFrame]) {
        if let Some(i) = self.loans.registers.iter().position(|(r, _)| *r == dst) {
            self.loans.registers.swap_remove(i);
        }
        let source = self.local_address(local, parents.len());
        match source.resolve(parents, self) {
            RegSlot::Heap(_) => {
                self.clear_reg(dst);
                self.loans.registers.push((dst, local));
            }
            scalar => self.set_slot(dst, scalar.clone()),
        }
    }

    pub(crate) fn prepare_arg(
        &mut self,
        reg: VReg,
        kill: bool,
        can_move: bool,
        depth: usize,
        stats: &FbipCounters,
    ) -> PendingArg {
        if let Some(i) = self.loans.registers.iter().position(|(r, _)| *r == reg) {
            let local = self.loans.registers[i].1;
            if kill {
                self.loans.registers.swap_remove(i);
            }
            stats.on_borrowed_call();
            PendingArg::Borrowed(self.local_address(local, depth))
        } else {
            PendingArg::Owned(self.read_or_take_slot(reg, kill, can_move, stats))
        }
    }

    /// 按值使用借入参数时仅物化被读的参数。引用始终指向未暂停写入的祖先 owner。
    pub(crate) fn materialize_local(&mut self, local: LocalSlot, parents: &[CallFrame]) {
        if let Some(i) = self.loans.parameters.iter().position(|(p, _)| *p == local) {
            let (_, source) = self.loans.parameters.swap_remove(i);
            let slot = source.resolve(parents, self).clone();
            self.locals[local.0 as usize].set_slot(slot);
        }
    }

    pub(crate) fn has_borrowed_parameters(&self) -> bool {
        !self.loans.parameters.is_empty()
    }

    /// Resolve `local` for reflective `eval` snapshots, following an active parameter loan
    /// rather than reading the `Nil` placeholder stored in `self.locals`.
    pub(crate) fn snapshot_local_value(
        &self,
        local: LocalSlot,
        parents: &[CallFrame],
    ) -> crate::value::Value {
        if let Some((_, source)) = self.loans.parameters.iter().find(|(p, _)| *p == local) {
            source.resolve(parents, self).to_value()
        } else {
            self.locals[local.0 as usize].get()
        }
    }

    /// 续延可能离开创建它的调用栈，先把外部引用解析为可独立复制的值。
    pub(crate) fn snapshot_loans(&mut self, parents: &[CallFrame]) {
        while let Some(&(p, _)) = self.loans.parameters.last() {
            self.materialize_local(p, parents);
        }
        for (reg, local) in std::mem::take(&mut self.loans.registers) {
            self.set_slot(reg, self.locals[local.0 as usize].get_slot());
        }
        // A captured continuation can outlive the frames the loans pointed into; after snapshotting
        // the frame must be self-contained so replay never resolves a stale slot address.
        debug_assert!(
            self.loans.parameters.is_empty(),
            "continuation snapshot left {} unresolved parameter loans",
            self.loans.parameters.len()
        );
        debug_assert!(
            self.loans.registers.is_empty(),
            "continuation snapshot left unresolved register loans"
        );
    }

    /// 尾调用不保留当前帧。把即将失效的 owner 直接移交给下一个帧；
    /// 一个 owner 若被多次传入，只有前面的别名需要一次共享所有权。
    pub(crate) fn inherit_tail_owners(&mut self, caller: &mut ScriptFrame, depth: usize) {
        let mut i = 0;
        while i < self.loans.parameters.len() {
            let (param, source) = self.loans.parameters[i];
            if source.depth != depth {
                i += 1;
                continue;
            }
            self.loans.parameters.swap_remove(i);
            let owner = &mut caller.locals[source.local.0 as usize];
            let slot = if self.loans.parameters.iter().any(|(_, s)| *s == source) {
                owner.get_slot()
            } else {
                owner.take_slot()
            };
            self.locals[param.0 as usize].set_slot(slot);
        }
    }
}
