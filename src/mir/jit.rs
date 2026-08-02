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
//! - `BinaryOp(d, a, op, b)` 其中 a/b 线性分析证明为**同型数字**：
//!   - **Float×Float** — SSE2 double 算术（addsd/subsd/mulsd/divsd）+
//!     比较（comisd/setcc）。Float 除零 = IEEE inf、NaN 比较 = false
//!     （NotEqual = true），与解释器精确一致。
//!   - **Int×Int** — 精确复刻解释器 `numeric_op` 的 **f64 round-trip**
//!     （`((a as f64) op (b as f64)).round() as i64`）：cvtsi2sd → 运算 →
//!     roundsd（half-away）→ cvtsd2si（越界饱和）。Mod 用
//!     `a - trunc(a/b)*b` 序列复刻 Rust 浮点余数。比较 = `a as f64 op
//!     b as f64`。
//!
//! 编译期拒绝（回落解释器）：Mixed 类型、Var/Define/调用/效果/控制流、
//!   其他架构。拒绝总是 Err —— 调用方（h_with_config）回落 `run_mir`，
//!   语义正确性永远由解释器兜底，JIT 只是加速器。
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

/// 可执行内存缓冲区（W^X 双阶段：alloc_rw 写入 → make_exec 切 RX）。
/// 生成代码拷入并经 make_exec 后，`as_fn_ptr` 转函数指针调用。
struct ExecMem {
    ptr: *mut u8,
    // 仅 unix munmap/mprotect 需要长度（windows VirtualFree 免长）。
    len: usize,
}

impl ExecMem {
    /// 阶段 1：RW 内存（写入生成代码，不可执行）。
    fn alloc_rw(len: usize) -> Option<ExecMem> {
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
            const PAGE_READWRITE: u32 = 0x04;
            let p = unsafe {
                VirtualAlloc(
                    std::ptr::null_mut(),
                    len,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
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
            const MAP_PRIVATE: i32 = 2;
            const MAP_ANONYMOUS: i32 = 0x20;
            let p = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    len,
                    PROT_READ | PROT_WRITE,
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

    /// 阶段 2：RW → RX（W^X 收口；写入完成后调用，之后不可再写）。
    fn make_exec(&mut self) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            unsafe extern "system" {
                fn VirtualProtect(
                    lp_address: *mut std::os::raw::c_void,
                    dw_size: usize,
                    fl_new_protect: u32,
                    lpfl_old_protect: *mut u32,
                ) -> i32;
            }
            const PAGE_EXECUTE_READ: u32 = 0x20;
            let mut old: u32 = 0;
            let rc = unsafe {
                VirtualProtect(
                    self.ptr as *mut std::os::raw::c_void,
                    self.len,
                    PAGE_EXECUTE_READ,
                    &mut old,
                )
            };
            if rc == 0 {
                return Err("VirtualProtect failed (W^X)".to_string());
            }
            Ok(())
        }
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
            }
            const PROT_READ: i32 = 1;
            const PROT_EXEC: i32 = 4;
            let rc = unsafe { mprotect(self.ptr, self.len, PROT_READ | PROT_EXEC) };
            if rc != 0 {
                return Err("mprotect failed (W^X)".to_string());
            }
            Ok(())
        }
        #[cfg(not(any(windows, unix)))]
        {
            Err("W^X not supported on this platform".to_string())
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

    /// `jcc rel32`（占位 0，收尾时 patch 到 bail 块）。cc 为 0F 8x 后缀：
    /// 0x85=jne / 0x83=jae / 0x8A=jp。
    fn jcc_bail(&mut self, cc: u8) {
        self.extend(&[0x0F, cc, 0, 0, 0, 0]);
        self.patches.push(self.buf.len() - 4);
    }

    /// 类型不匹配 → bail（jne）。
    fn jne_bail(&mut self) {
        self.jcc_bail(0x85);
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

    /// `cvtsi2sd xmm0, [rax+disp]` — Int payload → double。
    fn load_int_xmm0(&mut self, reg: Reg) {
        self.extend(&[0xF2, 0x0F, 0x2A, 0x80]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    /// `cvtsi2sd xmm1, [rax+disp]`。
    fn load_int_xmm1(&mut self, reg: Reg) {
        self.extend(&[0xF2, 0x0F, 0x2A, 0x88]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    /// `mov qword [rax+disp], r11` — 写 Int payload。
    fn store_payload_r11(&mut self, reg: Reg) {
        self.extend(&[0x4C, 0x89, 0x98]);
        self.extend(&Self::disp(reg, true).to_le_bytes());
    }

    fn int_saturate(&mut self) {
        // comisd xmm0, xmm0（判 nan：无序 → PF）
        self.extend(&[0x66, 0x0F, 0x2F, 0xC0]);
        self.extend(&[0x7A, 28]); // jp L_nan（next=0x88 → +28 = 0xa4）
        // movabs rdx, 0x43E0000000000000（2^63 的 double 位模式 = i64 上限）
        self.extend(&[0x48, 0xBA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE0, 0x43]);
        // movq xmm1, rdx（modrm=0xCA：mod=11 寄存器形式；0x0A 是 mod=00 的
        // (%rdx) 内存形式 —— objdump 证实解码为 movq (%rdx),%xmm1，运行时
        // 解引用 0x43E0... 非法地址 → AV）
        self.extend(&[0x66, 0x48, 0x0F, 0x6E, 0xCA]);
        // comisd xmm0, xmm1（modrm=0xC1：mod=11、reg=0→xmm0 第一操作数、
        // rm=1→xmm1 第二操作数 → Intel comisd xmm0,xmm1 即比较 xmm0 vs
        // xmm1。0xC8 是 reg=xmm1 → Intel comisd xmm1,xmm0 比较 2^63 vs
        // xmm0 → 恒 CF=0 → 恒跳 L_max → 全 MAX，objdump 已证实）
        self.extend(&[0x66, 0x0F, 0x2F, 0xC1]); // comisd xmm0, xmm1（xmm0>=2^63 → CF=0）
        self.extend(&[0x73, 12]); // jnc L_max（+12 → 40；xmm0>=2^63 → MAX）
        self.extend(&[0xF2, 0x4C, 0x0F, 0x2D, 0xD8]); // cvtsd2si r11, xmm0（-2^63<=x<2^63 无溢出）
        self.extend(&[0xEB, 15]); // jmp done（+15 → 50）
        // L_nan(35)：
        self.extend(&[0x4D, 0x31, 0xDB]); // xor r11, r11（nan → 0）
        self.extend(&[0xEB, 10]); // jmp done（+10 → 50）
        // L_max(40)：
        self.extend(&[0x49, 0xBB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]); // movabs r11, i64::MAX
        // done(50)：
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

/// Int 算术（复刻解释器**分裂语义**）：
/// - Add：eval_binary Int+Int **直接 i64 加法**（`a + b`，溢出 wrap ——
///   x86 add 天然 64 位 wrap；debug 解释器会 panic，release 与 JIT 一致）。
/// - Sub/Mul/Div：numeric_op **f64 round-trip**
///   （`((a as f64) op (b as f64)).round() as i64`）：cvtsi2sd → 运算 →
///   roundsd（half-away）→ cvtsd2si（范围检查饱和）。
///
/// 除零（Div）：a as f64 / 0 = ±inf → roundsd 保持 inf → cvtsd2si 饱和
/// i64::MIN/MAX，与解释器 `(inf).round() as i64` 一致。
fn emit_binop_int(code: &mut Code, dst: Reg, a: Reg, op: crate::common::BinaryOp, b: Reg) {
    code.load_regs();
    code.cmp_tag(a, TAG_INT);
    code.jne_bail();
    code.cmp_tag(b, TAG_INT);
    code.jne_bail();
    if matches!(op, crate::common::BinaryOp::Add) {
        // i64 直接加法：mov r11, [a+8]; add r11, [b+8]
        code.extend(&[0x4C, 0x8B, 0x98]);
        code.buf.extend(&Code::disp(a, true).to_le_bytes()); // mov r11, [a+8]
        code.extend(&[0x4C, 0x03, 0x98]);
        code.buf.extend(&Code::disp(b, true).to_le_bytes()); // add r11, [b+8]
        code.store_tag(dst, TAG_INT);
        code.store_payload_r11(dst);
        return;
    }
    code.load_int_xmm0(a);
    code.load_int_xmm1(b);
    let opcode = match op {
        crate::common::BinaryOp::Sub => 0x5C, // subsd
        crate::common::BinaryOp::Mul => 0x59, // mulsd
        crate::common::BinaryOp::Div => 0x5E, // divsd
        _ => unreachable!("non-arith op"),
    };
    code.extend(&[0xF2, 0x0F, opcode, 0xC1]); // xmm0 op xmm1（rm=xmm1）
    code.extend(&[0x66, 0x0F, 0x3A, 0x0B, 0xC0, 0x00]); // roundsd xmm0, xmm0, 0
    code.extend(&[0xF2, 0x4C, 0x0F, 0x2D, 0xD8]); // cvtsd2si r11, xmm0
    code.int_saturate();
    code.store_tag(dst, TAG_INT);
    code.store_payload_r11(dst);
}

/// Int Mod（精确复刻解释器 `(af % bf).round() as i64`，Rust 浮点 % =
/// 截断余数 `a - trunc(a/b)*b`）。SSE2：div → roundsd mode 3（trunc）→
/// mul → 重载 a 相减 → roundsd mode 0 → cvtsd2si。
fn emit_binop_int_mod(code: &mut Code, dst: Reg, a: Reg, b: Reg) {
    code.load_regs();
    code.cmp_tag(a, TAG_INT);
    code.jne_bail();
    code.cmp_tag(b, TAG_INT);
    code.jne_bail();
    code.load_int_xmm0(a); // xmm0 = a
    code.load_int_xmm1(b); // xmm1 = b
    code.extend(&[0xF2, 0x0F, 0x5E, 0xC1]); // divsd xmm0, xmm1 → a/b
    code.extend(&[0x66, 0x0F, 0x3A, 0x0B, 0xC0, 0x03]); // roundsd xmm0, xmm0, 3 → trunc(a/b)
    code.extend(&[0xF2, 0x0F, 0x59, 0xC1]); // mulsd xmm0, xmm1 → trunc(a/b)*b
    code.load_int_xmm1(a); // xmm1 = a（重读，覆盖 b）
    code.extend(&[0xF2, 0x0F, 0x5C, 0xC8]); // subsd xmm1, xmm0 → a - trunc(a/b)*b
    code.extend(&[0x66, 0x0F, 0x3A, 0x0B, 0xC9, 0x00]); // roundsd xmm1, xmm1, 0
    code.extend(&[0xF2, 0x4C, 0x0F, 0x2D, 0xD9]); // cvtsd2si r11, xmm1
    code.extend(&[0x66, 0x0F, 0x28, 0xC1]); // movaps xmm0, xmm1（饱和判别读 xmm0）
    code.int_saturate();
    code.store_tag(dst, TAG_INT);
    code.store_payload_r11(dst);
}

/// Int 比较（精确复刻解释器 numeric_cmp Int 分支 `a as f64 op b as f64`）。
/// 无 NaN 修正（Int→f64 恒有序）。
fn emit_binop_int_cmp(code: &mut Code, dst: Reg, a: Reg, op: crate::common::BinaryOp, b: Reg) {
    code.load_regs();
    code.cmp_tag(a, TAG_INT);
    code.jne_bail();
    code.cmp_tag(b, TAG_INT);
    code.jne_bail();
    code.load_int_xmm0(a);
    code.load_int_xmm1(b);
    code.extend(&[0x66, 0x0F, 0x2F, 0xC1]); // comisd xmm0, xmm1（寄存器形式）
    // 清 eax 需在 comisd 后 setcc 前？不行——setcc 前清 eax 会毁标志。
    // 直接 setcc al + movzx（高位残留用 movzx 清，见 Float cmp 同款）。
    let setcc = match op {
        crate::common::BinaryOp::Equal => 0x94,        // sete
        crate::common::BinaryOp::NotEqual => 0x95,     // setne
        crate::common::BinaryOp::Less => 0x92,         // setb
        crate::common::BinaryOp::LessEqual => 0x96,    // setbe
        crate::common::BinaryOp::Greater => 0x97,      // seta
        crate::common::BinaryOp::GreaterEqual => 0x93, // setae
        _ => unreachable!("non-compare op"),
    };
    code.setcc_al(setcc);
    code.extend(&[0x0F, 0xB6, 0xC0]); // movzx eax, al
    code.store_bool(dst);
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

/// 线性编译门：body 全部 ∈ {Const, 同型 BinaryOp} 才编译。
/// 返回 (可执行代码, 结果寄存器)。
fn try_compile(func: &MirFunction) -> Option<(ExecMem, Reg)> {
    if func.body.is_empty() || !cfg!(target_arch = "x86_64") {
        return None;
    }
    // 寄存器类型线性跟踪（Int / Float；比较结果 Bool 不再参与后续 BinaryOp）。
    #[derive(Clone, Copy, PartialEq)]
    enum Ty {
        Int,
        Float,
    }
    let mut types: Vec<Option<Ty>> = vec![None; func.n_regs];
    let mut code = Code::new();
    let mut last_dst: Option<Reg> = None;

    for inst in &func.body {
        match inst {
            MirInst::Const(dst, v) => {
                if !emit_const(&mut code, *dst, v) {
                    return None;
                }
                types[*dst] = match v {
                    Value::Int(_) => Some(Ty::Int),
                    Value::Float(_) => Some(Ty::Float),
                    _ => None,
                };
                last_dst = Some(*dst);
            }
            MirInst::BinaryOp(dst, a, op, b) => {
                if *dst >= func.n_regs || *a >= func.n_regs || *b >= func.n_regs {
                    return None;
                }
                // 同型（Int×Int / Float×Float）才编译；Mixed/未跟踪拒绝。
                let (is_int, is_float) = match (types[*a], types[*b]) {
                    (Some(Ty::Int), Some(Ty::Int)) => (true, false),
                    (Some(Ty::Float), Some(Ty::Float)) => (false, true),
                    _ => return None,
                };
                let arith = matches!(
                    op,
                    crate::common::BinaryOp::Add
                        | crate::common::BinaryOp::Sub
                        | crate::common::BinaryOp::Mul
                        | crate::common::BinaryOp::Div
                );
                let cmp = matches!(
                    op,
                    crate::common::BinaryOp::Equal
                        | crate::common::BinaryOp::NotEqual
                        | crate::common::BinaryOp::Greater
                        | crate::common::BinaryOp::Less
                        | crate::common::BinaryOp::GreaterEqual
                        | crate::common::BinaryOp::LessEqual
                );
                if is_float && cmp {
                    emit_binop_float_cmp(&mut code, *dst, *a, op.clone(), *b);
                    types[*dst] = None; // Bool
                } else if is_int && cmp {
                    emit_binop_int_cmp(&mut code, *dst, *a, op.clone(), *b);
                    types[*dst] = None; // Bool
                } else if is_float && arith {
                    emit_binop_float(&mut code, *dst, *a, op.clone(), *b);
                    types[*dst] = Some(Ty::Float);
                } else if is_int && arith {
                    emit_binop_int(&mut code, *dst, *a, op.clone(), *b);
                    types[*dst] = Some(Ty::Int);
                } else if is_int && matches!(op, crate::common::BinaryOp::Mod) {
                    emit_binop_int_mod(&mut code, *dst, *a, *b);
                    types[*dst] = Some(Ty::Int);
                } else {
                    return None; // Float Mod（无 fmod 模板）等
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
    // W^X：先 RW 写入，再切 RX（见 ExecMem::make_exec）。
    let mut mem = ExecMem::alloc_rw(bytes.len())?;
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mem.ptr, bytes.len());
    }
    mem.make_exec().ok()?;
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
