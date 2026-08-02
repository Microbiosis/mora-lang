//! v0.75.43: copy-and-patch JIT（零 LLVM 运行时依赖）
//!
//! 设计（CPython 3.13+/PEP 744 模式）：每个 MirInst 对应一个**预编译的
//! 机器码模板**（本文件内以字节序列手写），编译时把模板「复制」进可执行
//! 内存并「patch」占位槽（寄存器号 → 位移、常量 → 立即数、跳转 → 相对偏移）。
//! 生成的是直落原生代码，无逐指令 dispatch —— 这是 copy-and-patch 相对
//! 解释器的核心收益。
//!
//! ## v1 子集（诚实边界）
//! 可编译的指令集：
//! - `Const(reg, Int/Float/Bool/Nil)` — 立即数写入寄存器槽
//! - `BinaryOp(d, a, op, b)` 其中 a/b 线性分析证明为 **Float** — 原生
//!   SSE2 double 算术（Add/Sub/Mul/Div）+ 比较（Equal/NotEqual/Greater/
//!   Less/GreaterEqual/LessEqual）。Float 除零 = IEEE inf、NaN 比较 = false，
//!   与解释器（flow::eval_binary / values_equal）语义精确一致。
//!
//! 编译期拒绝（回落解释器）：Int×Int 算术（i64 精确语义需 round 对齐，
//!   v1 不冒险）、Mod（无 SSE2 fmod）、Var/Define/调用/效果/控制流、
//!   Mixed 类型、其他架构。拒绝总是 Err —— 调用方（h_with_config）回落
//!   `run_mir`，语义正确性永远由解释器兜底，JIT 只是加速器。
//!
//! ## 寄存器表示
//! 生成代码操作 `regs: *mut JitValue` 数组（每个槽固定 16 字节，repr(C)
//! 布局稳定）。v1 无 env/宿主调用，`run_jit` 的 interp/env 参数保留签名
//! （未来步骤 slot-in）。
//!
//! ## 平台
//! - x86-64：完整支持（Windows x64 / System V 传参约定，cfg 区分 arg 寄存器）
//! - 其他架构：编译期拒绝 → 回落解释器

use crate::mir::host::MirHost;
use crate::mir::{MirFunction, MirInst, Reg};
use crate::value::{Environment, Value};

// ===================================================================
// JitValue — 布局稳定的标签联合（repr(C)，16 字节/槽）
// ===================================================================

/// 标签值（与生成代码中的立即数一致，勿改编号）。
const TAG_INT: u64 = 0;
const TAG_FLOAT: u64 = 1;
const TAG_BOOL: u64 = 2;
const TAG_NIL: u64 = 3;

/// JIT 寄存器槽：`{ tag: u64, payload: u64 }`，固定 16 字节。
/// payload 为位模式（i64 / f64 bits / bool 0|1）。
#[repr(C)]
#[derive(Clone, Copy)]
struct JitValue {
    tag: u64,
    payload: u64,
}

impl JitValue {
    fn nil() -> Self {
        JitValue {
            tag: TAG_NIL,
            payload: 0,
        }
    }
}

/// v1 可接受的 Const 值 → JitValue。其余 Value 变体编译期拒绝。
fn const_to_jit(v: &Value) -> Option<JitValue> {
    match v {
        Value::Int(i) => Some(JitValue {
            tag: TAG_INT,
            payload: *i as u64,
        }),
        Value::Float(f) => Some(JitValue {
            tag: TAG_FLOAT,
            payload: f.to_bits(),
        }),
        Value::Bool(b) => Some(JitValue {
            tag: TAG_BOOL,
            payload: *b as u64,
        }),
        Value::Nil => Some(JitValue {
            tag: TAG_NIL,
            payload: 0,
        }),
        _ => None,
    }
}

/// JitValue → Value（函数结果回写）。
fn jit_to_value(v: JitValue) -> Value {
    match v.tag {
        TAG_INT => Value::Int(v.payload as i64),
        TAG_FLOAT => Value::Float(f64::from_bits(v.payload)),
        TAG_BOOL => Value::Bool(v.payload != 0),
        _ => Value::Nil,
    }
}

// ===================================================================
// ExecMem — 可执行内存（零外部依赖）
// ===================================================================

/// 可执行内存缓冲区。生成代码拷入后经 `ptr()` 转函数指针调用。
struct ExecMem {
    ptr: *mut u8,
    // 仅 unix munmap 释放需要长度（windows VirtualFree 免长）。
    #[cfg_attr(not(unix), allow(dead_code))]
    len: usize,
}

impl ExecMem {
    fn alloc(len: usize) -> Option<ExecMem> {
        let len = len.max(1);
        #[cfg(target_os = "windows")]
        {
            use std::os::raw::c_void;
            unsafe extern "system" {
                fn VirtualAlloc(
                    lp_address: *mut c_void,
                    dw_size: usize,
                    fl_allocation_type: u32,
                    fl_protect: u32,
                ) -> *mut c_void;
            }
            const MEM_COMMIT: u32 = 0x1000;
            const MEM_RESERVE: u32 = 0x2000;
            const PAGE_EXECUTE_READWRITE: u32 = 0x40;
            let p = unsafe {
                VirtualAlloc(
                    std::ptr::null_mut(),
                    len,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                )
            };
            if p.is_null() {
                None
            } else {
                Some(ExecMem {
                    ptr: p as *mut u8,
                    len,
                })
            }
        }
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn mmap(
                    addr: *mut u8,
                    length: usize,
                    prot: i32,
                    flags: i32,
                    fd: i32,
                    offset: i64,
                ) -> *mut u8;
            }
            const PROT_READ: i32 = 1;
            const PROT_WRITE: i32 = 2;
            const PROT_EXEC: i32 = 4;
            const MAP_PRIVATE: i32 = 2;
            const MAP_ANONYMOUS: i32 = 0x20;
            let p = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    len,
                    PROT_READ | PROT_WRITE | PROT_EXEC,
                    MAP_PRIVATE | MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if p as usize == usize::MAX {
                None
            } else {
                Some(ExecMem { ptr: p, len })
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = len;
            None
        }
    }

    fn as_fn_ptr(&self) -> unsafe extern "C" fn(*mut JitState) -> u32 {
        unsafe { std::mem::transmute(self.ptr) }
    }
}

impl Drop for ExecMem {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            unsafe extern "system" {
                fn VirtualFree(
                    lp_address: *mut std::os::raw::c_void,
                    dw_size: usize,
                    dw_free_type: u32,
                ) -> i32;
            }
            const MEM_RELEASE: u32 = 0x8000;
            unsafe {
                VirtualFree(self.ptr as *mut std::os::raw::c_void, 0, MEM_RELEASE);
            }
        }
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn munmap(addr: *mut u8, length: usize) -> i32;
            }
            unsafe {
                munmap(self.ptr, self.len);
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = self.ptr;
            let _ = self.len;
        }
    }
}

// ===================================================================
// 运行时状态（生成代码通过 arg 寄存器接收）
// ===================================================================

/// 生成函数的入参状态。`bail != 0` 表示生成代码遇到动态失败，
/// 调用方回落解释器。offset 布局与生成代码中的位移常量一致。
#[repr(C)]
struct JitState {
    regs: *mut JitValue,
    n_regs: u32,
    bail: u32,
}

// ===================================================================
// x86-64 模板发射器
// ===================================================================

/// 字节流 + 跳转 patch 位置簿。
struct Code {
    buf: Vec<u8>,
    patches: Vec<usize>,
}

impl Code {
    fn new() -> Code {
        Code {
            buf: Vec::new(),
            patches: Vec::new(),
        }
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// `jne rel32`（占位 0，收尾时 patch 到 bail 块）。
    fn jne_bail(&mut self) {
        self.extend(&[0x0F, 0x85, 0, 0, 0, 0]);
        self.patches.push(self.buf.len() - 4);
    }

    fn disp(reg: Reg, payload: bool) -> i32 {
        (reg as i32) * 16 + if payload { 8 } else { 0 }
    }

    /// `mov rax, [arg_reg]` — 加载 regs 指针（Windows rcx / SysV rdi）。
    fn load_regs(&mut self) {
        #[cfg(target_os = "windows")]
        self.extend(&[0x48, 0x8B, 0x01]); // mov rax, [rcx]
        #[cfg(all(unix, not(target_os = "windows")))]
        self.extend(&[0x48, 0x8B, 0x07]); // mov rax, [rdi]
    }

    /// `mov qword [rax+disp], imm32` — 写 tag（tag 恒 < 2^31）。
    fn store_tag(&mut self, reg: Reg, tag: u64) {
        self.extend(&[0x48, 0xC7, 0x80]);
        self.extend(&Self::disp(reg, false).to_le_bytes());
        self.extend(&(tag as i32).to_le_bytes());
    }

    /// `cmp qword [rax+disp], imm32` — 检查 tag == 目标值。
    fn cmp_tag(&mut self, reg: Reg, tag: u64) {
        self.extend(&[0x48, 0x81, 0xB8]);
        self.extend(&Self::disp(reg, false).to_le_bytes());
        self.extend(&(tag as i32).to_le_bytes());
    }

    /// `movsd xmm0, [rax+disp]` — 加载 Float payload。
    fn load_xmm0_mem(&mut self, reg: Reg) {
        self.extend(&[0xF2, 0x0F, 0x10, 0x80]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    /// `addsd/subsd/mulsd/divsd xmm0, [rax+disp]`。
    fn arith_xmm0_mem(&mut self, opcode: u8, reg: Reg) {
        self.extend(&[0xF2, 0x0F, opcode, 0x80]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    /// `movsd [rax+disp], xmm0` — 写 Float payload。
    fn store_xmm0_mem(&mut self, reg: Reg) {
        self.extend(&[0xF2, 0x0F, 0x11, 0x80]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    /// `comisd xmm0, [rax+disp]` — 比较（有序；NaN → ZF=PF=CF=1）。
    fn comisd_xmm0_mem(&mut self, reg: Reg) {
        self.extend(&[0x66, 0x0F, 0x2F, 0x80]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    /// `setcc al`（sete/setne/setb/setbe/seta/setae 等）。
    fn setcc_al(&mut self, setcc: u8) {
        self.extend(&[0x0F, setcc, 0xC0]);
    }

    /// 写 Bool 结果到 dst（rax 可能被 comisd 污染，从 arg 重载 regs 到 rdx）。
    fn store_bool(&mut self, dst: Reg) {
        #[cfg(target_os = "windows")]
        self.extend(&[0x48, 0x8B, 0x11]); // mov rdx, [rcx]
        #[cfg(all(unix, not(target_os = "windows")))]
        self.extend(&[0x48, 0x8B, 0x17]); // mov rdx, [rdi]
        self.extend(&[0x48, 0xC7, 0x82]); // mov qword [rdx+disp], TAG_BOOL
        self.extend(&Self::disp(dst, false).to_le_bytes());
        self.extend(&(TAG_BOOL as i32).to_le_bytes());
        self.extend(&[0x48, 0x89, 0x82]); // mov qword [rdx+disp], rax
        self.extend(&Self::disp(dst, true).to_le_bytes());
    }

    /// 收尾：追加 bail 块并把所有 jne 占位 patch 过去。
    fn finish_bail(mut self, bail: &[u8]) -> Vec<u8> {
        let bail_pos = self.buf.len() as i32;
        self.buf.extend_from_slice(bail);
        for &pos in &self.patches {
            let rel = bail_pos - (pos as i32 + 4);
            self.buf[pos..pos + 4].copy_from_slice(&rel.to_le_bytes());
        }
        self.buf
    }
}

// ===================================================================
// 线性编译
// ===================================================================

#[cfg(target_arch = "x86_64")]
fn emit_bail_block() -> Vec<u8> {
    let mut b = Vec::new();
    // mov dword [state+12], 1  → state->bail = 1
    #[cfg(target_os = "windows")]
    b.extend(&[0xC7, 0x41, 0x0C, 0x01, 0x00, 0x00, 0x00]); // [rcx+0x0C]
    #[cfg(all(unix, not(target_os = "windows")))]
    b.extend(&[0xC7, 0x47, 0x0C, 0x01, 0x00, 0x00, 0x00]); // [rdi+0x0C]
    b.extend(&[0x31, 0xC0, 0xC3]); // xor eax,eax; ret
    b
}

/// 编译一个 Const 指令（Int/Float/Bool/Nil 立即数）。
fn emit_const(code: &mut Code, dst: Reg, v: &Value) -> bool {
    let jv = match const_to_jit(v) {
        Some(jv) => jv,
        None => return false,
    };
    code.load_regs();
    // movabs rdx, IMM64
    code.extend(&[0x48, 0xBA]);
    code.buf.extend(&jv.payload.to_le_bytes());
    code.store_tag(dst, jv.tag);
    // mov qword [rax+disp], rdx
    code.extend(&[0x48, 0x89, 0x90]);
    code.buf.extend(&Code::disp(dst, true).to_le_bytes());
    true
}

/// 编译 Float BinaryOp（Add/Sub/Mul/Div）。类型不匹配 → bail。
fn emit_binop_float(code: &mut Code, dst: Reg, a: Reg, op: crate::common::BinaryOp, b: Reg) {
    code.load_regs();
    code.cmp_tag(a, TAG_FLOAT);
    code.jne_bail();
    code.cmp_tag(b, TAG_FLOAT);
    code.jne_bail();
    code.load_xmm0_mem(a);
    let opcode = match op {
        crate::common::BinaryOp::Add => 0x58, // addsd
        crate::common::BinaryOp::Sub => 0x5C, // subsd
        crate::common::BinaryOp::Mul => 0x59, // mulsd
        crate::common::BinaryOp::Div => 0x5E, // divsd（除零 → IEEE inf，同解释器）
        _ => unreachable!("non-arith op"),
    };
    code.arith_xmm0_mem(opcode, b);
    code.store_tag(dst, TAG_FLOAT);
    code.store_xmm0_mem(dst);
}

/// 编译 Float 比较（结果 Bool）。NaN → false（comisd 无序 + jp 修正）。
fn emit_binop_float_cmp(code: &mut Code, dst: Reg, a: Reg, op: crate::common::BinaryOp, b: Reg) {
    code.load_regs();
    code.cmp_tag(a, TAG_FLOAT);
    code.jne_bail();
    code.cmp_tag(b, TAG_FLOAT);
    code.jne_bail();
    code.load_xmm0_mem(a);
    code.comisd_xmm0_mem(b);
    // setcc 只写 al 低 8 位 → 事后 movzx 零扩展清高位（不能 comisd 前
    // xor eax —— 会毁 rax 寻址基址与 comisd 标志）。
    // NaN 语义（与解释器 Rust 语义一致）：
    //   Equal/Less/LessEqual/Greater/GreaterEqual → false；NotEqual → true。
    // comisd 无序时 ZF=CF=1；setb/setbe/sete 误置 1、setne 误置 0。
    // 可靠 NaN 检测 = setb & sete → and（有序相等时 ZF=1 但 CF=0 → 无歧义）。
    let (setcc, nan_fix) = match op {
        crate::common::BinaryOp::Equal => (0x94, Some(0)), // sete
        crate::common::BinaryOp::NotEqual => (0x95, Some(1)), // setne
        crate::common::BinaryOp::Less => (0x92, Some(0)),  // setb
        crate::common::BinaryOp::LessEqual => (0x96, Some(0)), // setbe
        crate::common::BinaryOp::Greater => (0x97, None),  // seta
        crate::common::BinaryOp::GreaterEqual => (0x93, None), // setae
        _ => unreachable!("non-compare op"),
    };
    code.setcc_al(setcc);
    if let Some(nan_val) = nan_fix {
        // 可靠 NaN 检测：comisd 无序时 ZF=CF=1；有序相等时 ZF=1 但 CF=0
        // （结果非负无借位）。故 NaN ⇔ ZF∧CF，无 PF 歧义。
        //   sete r8b      ; 41 0F 94 C0（ZF）
        //   setb r9b      ; 45 0F 92 C1（CF）
        //   and r8b, r9b  ; 45 20 C8（reg=r9b → REX.R=1、rm=r8b → REX.B=1
        //                   → 0x40+4+1=0x45；W 必须 0，0x4E 会变 64 位 and）
        //   test r8b,r8b  ; 45 84 C0（reg=r8b → REX.R=1）
        //   jz +2         ; 74 02（有序 → 跳过 mov）
        //   mov al, imm8  ; B0 imm（NaN → 目标值）
        // 用 r8b/r9b 而非 cl/dl：cl/dl 是 arg 指针（Windows rcx / SysV
        // rdi）的低字节，写入会污染 state 指针，后续 [rcx]/[rdi] 崩溃。
        code.extend(&[0x41, 0x0F, 0x94, 0xC0]); // sete r8b
        code.extend(&[0x45, 0x0F, 0x92, 0xC1]); // setb r9b
        code.extend(&[0x45, 0x20, 0xC8]); // and r8b, r9b
        code.extend(&[0x45, 0x84, 0xC0]); // test r8b, r8b
        code.extend(&[0x74, 0x02]); // jz +2
        code.extend(&[0xB0, nan_val]); // mov al, imm8
    }
    // movzx eax, al — 零扩展清高位（Bool payload 需 0/1，防 rax 残留指针位）
    code.extend(&[0x0F, 0xB6, 0xC0]);
    code.store_bool(dst);
}

/// 线性编译门：body 全部 ∈ {Const, Float BinaryOp} 才编译。
/// 返回 (可执行代码, 结果寄存器)。
fn try_compile(func: &MirFunction) -> Option<(ExecMem, Reg)> {
    if func.body.is_empty() || !cfg!(target_arch = "x86_64") {
        return None;
    }
    // 寄存器类型线性跟踪（v1：仅 Float 参与 BinaryOp）。
    let mut types: Vec<bool> = vec![false; func.n_regs]; // true = Float
    let mut code = Code::new();
    let mut last_dst: Option<Reg> = None;

    for inst in &func.body {
        match inst {
            MirInst::Const(dst, v) => {
                if !emit_const(&mut code, *dst, v) {
                    return None;
                }
                types[*dst] = matches!(v, Value::Float(_));
                last_dst = Some(*dst);
            }
            MirInst::BinaryOp(dst, a, op, b) => {
                if *dst >= func.n_regs || *a >= func.n_regs || *b >= func.n_regs {
                    return None;
                }
                // v1：仅 Float×Float（Int 算术的 i64 精确语义 / Mod 的 fmod
                // 超出 v1 模板集，编译期拒绝回落解释器）。
                if !types[*a] || !types[*b] {
                    return None;
                }
                match op {
                    crate::common::BinaryOp::Add
                    | crate::common::BinaryOp::Sub
                    | crate::common::BinaryOp::Mul
                    | crate::common::BinaryOp::Div => {
                        emit_binop_float(&mut code, *dst, *a, op.clone(), *b);
                        types[*dst] = true;
                    }
                    crate::common::BinaryOp::Equal
                    | crate::common::BinaryOp::NotEqual
                    | crate::common::BinaryOp::Greater
                    | crate::common::BinaryOp::Less
                    | crate::common::BinaryOp::GreaterEqual
                    | crate::common::BinaryOp::LessEqual => {
                        emit_binop_float_cmp(&mut code, *dst, *a, op.clone(), *b);
                        types[*dst] = false; // Bool
                    }
                    // Mod：无 SSE2 fmod → 编译期拒绝
                    crate::common::BinaryOp::Mod => return None,
                }
                last_dst = Some(*dst);
            }
            _ => return None, // 任何未覆盖指令 → 回落解释器
        }
    }

    let last_dst = last_dst?;
    // 正常出口：xor eax,eax; ret
    code.extend(&[0x31, 0xC0, 0xC3]);
    let bytes = code.finish_bail(&emit_bail_block());
    let mem = ExecMem::alloc(bytes.len())?;
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem.ptr, bytes.len());
    }
    Some((mem, last_dst))
}

// ===================================================================
// 入口
// ===================================================================

/// copy-and-patch JIT：对纯线性 Float 子集编译执行，其余回落解释器。
///
/// `Err` = 不可编译或执行中 bail（类型不匹配）— 调用方回落 `run_mir`
/// （语义正确性由解释器兜底）。
pub fn run_jit(
    func: &MirFunction,
    _interp: &mut dyn MirHost,
    _env: &mut Environment,
) -> Result<Value, String> {
    let Some((mem, last_dst)) = try_compile(func) else {
        return Err("jit: not compilable (v1 linear Float subset)".to_string());
    };
    let mut regs: Vec<JitValue> = vec![JitValue::nil(); func.n_regs];
    let mut state = JitState {
        regs: regs.as_mut_ptr(),
        n_regs: func.n_regs as u32,
        bail: 0,
    };
    let entry = mem.as_fn_ptr();
    // SAFETY: `regs`/`state` 在调用期间保持存活；生成代码只读写 JitValue
    // 数组与 state.bail，纯原生算术无回调（v1 无 env/宿主调用）。
    let rc = unsafe { entry(&mut state) };
    if rc == 0 && state.bail == 0 {
        Ok(jit_to_value(regs[last_dst]))
    } else {
        Err("jit: bail (type mismatch)".to_string())
    }
}
