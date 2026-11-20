//! Wave 4 P2-1: Deterministic Binary Bytecode Codec (`MORABC02`) for [`ControlProgram`] SSA IR.
//!
//! Serializes and deserializes verified, optimized [`ControlProgram`] graphs and multi-module
//! [`CompiledPackage`] artifacts with:
//! - Compact unsigned LEB128 varints for registers, block/fn/symbol IDs, and lengths
//! - 64-bit FNV-1a integrity checksum over the entire payload
//! - Strict bounds & recursion-depth guards against malformed/corrupt bytecode inputs
//! - Full SSA CFG + dominance verification (`verify_program`) enforced on every load
//! - Zero source text embedded — standalone binaries & `.morac` artifacts execute directly on
//!   the stackful VM with zero lexing, parsing, HM typechecking, or SSA optimization overhead.

use std::collections::BTreeMap;

use crate::common::{BinaryOp, ScalarValue};
use crate::error::{MoraError, MoraResult};
use crate::mir::control::ir::{
    BasicBlock, BlockId, BuiltinFnId, CalleeTarget, CaptureSource, CompiledPattern, ControlConst,
    ControlFunction, ControlImplMethod, ControlInst, ControlProgram, ControlQqSegment,
    ControlSelectArm, ControlSliceAxis, ControlSpawnTarget, ControlStructField, ControlTerminator,
    ControlTraitMethod, FnId, FunctionKind, GlobalKind, GlobalMeta, GlobalSlot, LocalSlot,
    ParallelSegment, PatternTarget, SymId, SymbolTable, UnaryOp, UpvalueDesc, UpvalueSlot, VReg,
    verify_program,
};
use crate::value::BuiltinKind;

/// File & payload magic header for Mora AOT SSA bytecode (`MORABC02`).
pub(crate) const BYTECODE_MAGIC: &[u8; 8] = b"MORABC02";
/// Binary wire format version.
pub(crate) const BYTECODE_VERSION: u16 = 2;

const MAX_COLLECTION_LEN: usize = 1_000_000;
const MAX_STR_LEN: usize = 64 * 1024 * 1024;
const MAX_PATTERN_DEPTH: usize = 128;

/// Complete AOT-compiled package: optimized entry [`ControlProgram`] + precompiled transitive modules.
#[derive(Debug, Clone)]
pub struct CompiledPackage {
    pub opt_level: u8,
    pub entry: ControlProgram,
    pub modules: BTreeMap<String, ControlProgram>,
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn corrupt(msg: impl Into<String>) -> MoraError {
    MoraError::Serialization(format!("corrupt MORABC02 bytecode: {}", msg.into()))
}

struct ByteWriter {
    buf: Vec<u8>,
}

impl ByteWriter {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
        }
    }

    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    #[inline]
    fn write_bool(&mut self, v: bool) {
        self.write_u8(u8::from(v));
    }

    #[inline]
    fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn write_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    #[inline]
    fn write_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    #[inline]
    fn write_var_u32(&mut self, mut v: u32) {
        while v >= 0x80 {
            self.buf.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }

    fn write_len(&mut self, len: usize) -> MoraResult<()> {
        let v = u32::try_from(len)
            .map_err(|_| MoraError::Serialization("bytecode collection exceeds u32".to_string()))?;
        self.write_var_u32(v);
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> MoraResult<()> {
        let bytes = s.as_bytes();
        self.write_len(bytes.len())?;
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn write_opt_str(&mut self, s: Option<&str>) -> MoraResult<()> {
        match s {
            None => self.write_u8(0),
            Some(v) => {
                self.write_u8(1);
                self.write_str(v)?;
            }
        }
        Ok(())
    }

    fn write_sym(&mut self, id: SymId) {
        self.write_var_u32(id.0);
    }

    fn write_block(&mut self, id: BlockId) {
        self.write_var_u32(id.0);
    }

    fn write_fn(&mut self, id: FnId) {
        self.write_var_u32(id.0);
    }

    fn write_reg(&mut self, r: VReg) {
        self.write_var_u32(r.0);
    }

    fn write_local(&mut self, s: LocalSlot) {
        self.write_var_u32(u32::from(s.0));
    }

    fn write_upvalue(&mut self, s: UpvalueSlot) {
        self.write_var_u32(u32::from(s.0));
    }

    fn write_global(&mut self, s: GlobalSlot) {
        self.write_var_u32(u32::from(s.0));
    }

    fn write_regs(&mut self, regs: &[VReg]) -> MoraResult<()> {
        self.write_len(regs.len())?;
        for &r in regs {
            self.write_reg(r);
        }
        Ok(())
    }

    fn write_named_regs(&mut self, named: &[(SymId, VReg)]) -> MoraResult<()> {
        self.write_len(named.len())?;
        for &(sym, r) in named {
            self.write_sym(sym);
            self.write_reg(r);
        }
        Ok(())
    }

    fn write_captures(&mut self, caps: &[CaptureSource]) -> MoraResult<()> {
        self.write_len(caps.len())?;
        for cap in caps {
            match *cap {
                CaptureSource::ParentLocal(s) => {
                    self.write_u8(0);
                    self.write_local(s);
                }
                CaptureSource::ParentUpvalue(s) => {
                    self.write_u8(1);
                    self.write_upvalue(s);
                }
            }
        }
        Ok(())
    }
}

struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_bytes(&mut self, n: usize) -> MoraResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| corrupt("offset overflow"))?;
        if end > self.data.len() {
            return Err(corrupt(format!(
                "unexpected EOF at byte {} (needed {} more bytes)",
                self.pos, n
            )));
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    #[inline]
    fn read_u8(&mut self) -> MoraResult<u8> {
        Ok(self.read_bytes(1)?[0])
    }

    #[inline]
    fn read_bool(&mut self) -> MoraResult<bool> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(corrupt(format!("invalid bool byte {other}"))),
        }
    }

    #[inline]
    fn read_u16(&mut self) -> MoraResult<u16> {
        let b = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    #[inline]
    fn read_u32(&mut self) -> MoraResult<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    #[inline]
    fn read_u64(&mut self) -> MoraResult<u64> {
        let b = self.read_bytes(8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    #[inline]
    fn read_i64(&mut self) -> MoraResult<i64> {
        let b = self.read_bytes(8)?;
        Ok(i64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    #[inline]
    fn read_f32(&mut self) -> MoraResult<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    #[inline]
    fn read_f64(&mut self) -> MoraResult<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    fn read_var_u32(&mut self) -> MoraResult<u32> {
        let mut out: u32 = 0;
        let mut shift = 0u32;
        for _ in 0..5 {
            let b = self.read_u8()?;
            out |= u32::from(b & 0x7f)
                .checked_shl(shift)
                .ok_or_else(|| corrupt("varint overflow"))?;
            if (b & 0x80) == 0 {
                return Ok(out);
            }
            shift += 7;
        }
        Err(corrupt("varint exceeded 5 bytes"))
    }

    fn read_len(&mut self, max: usize) -> MoraResult<usize> {
        let len = self.read_var_u32()? as usize;
        if len > max {
            return Err(corrupt(format!("length {len} exceeds bound {max}")));
        }
        Ok(len)
    }

    fn read_str(&mut self) -> MoraResult<String> {
        let len = self.read_len(MAX_STR_LEN)?;
        let bytes = self.read_bytes(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|e| corrupt(format!("invalid UTF-8: {e}")))
    }

    fn read_opt_str(&mut self) -> MoraResult<Option<String>> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.read_str()?)),
            other => Err(corrupt(format!("invalid option tag {other}"))),
        }
    }

    fn read_sym(&mut self) -> MoraResult<SymId> {
        Ok(SymId(self.read_var_u32()?))
    }

    fn read_block(&mut self) -> MoraResult<BlockId> {
        Ok(BlockId(self.read_var_u32()?))
    }

    fn read_fn(&mut self) -> MoraResult<FnId> {
        Ok(FnId(self.read_var_u32()?))
    }

    fn read_reg(&mut self) -> MoraResult<VReg> {
        Ok(VReg(self.read_var_u32()?))
    }

    fn read_local(&mut self) -> MoraResult<LocalSlot> {
        let v = self.read_var_u32()?;
        let slot = u16::try_from(v).map_err(|_| corrupt("LocalSlot overflow"))?;
        Ok(LocalSlot(slot))
    }

    fn read_upvalue(&mut self) -> MoraResult<UpvalueSlot> {
        let v = self.read_var_u32()?;
        let slot = u16::try_from(v).map_err(|_| corrupt("UpvalueSlot overflow"))?;
        Ok(UpvalueSlot(slot))
    }

    fn read_global(&mut self) -> MoraResult<GlobalSlot> {
        let v = self.read_var_u32()?;
        let slot = u16::try_from(v).map_err(|_| corrupt("GlobalSlot overflow"))?;
        Ok(GlobalSlot(slot))
    }

    fn read_regs(&mut self) -> MoraResult<Vec<VReg>> {
        let n = self.read_len(MAX_COLLECTION_LEN)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.read_reg()?);
        }
        Ok(out)
    }

    fn read_named_regs(&mut self) -> MoraResult<Vec<(SymId, VReg)>> {
        let n = self.read_len(MAX_COLLECTION_LEN)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push((self.read_sym()?, self.read_reg()?));
        }
        Ok(out)
    }

    fn read_captures(&mut self) -> MoraResult<Vec<CaptureSource>> {
        let n = self.read_len(MAX_COLLECTION_LEN)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let cap = match self.read_u8()? {
                0 => CaptureSource::ParentLocal(self.read_local()?),
                1 => CaptureSource::ParentUpvalue(self.read_upvalue()?),
                other => return Err(corrupt(format!("invalid CaptureSource tag {other}"))),
            };
            out.push(cap);
        }
        Ok(out)
    }
}

fn encode_builtin_fn_id(id: BuiltinFnId) -> u8 {
    match id {
        BuiltinFnId::Apply => 0,
        BuiltinFnId::Atom => 1,
        BuiltinFnId::Bool => 2,
        BuiltinFnId::Car => 3,
        BuiltinFnId::Cdr => 4,
        BuiltinFnId::Channel => 5,
        BuiltinFnId::Compose => 6,
        BuiltinFnId::Cons => 7,
        BuiltinFnId::Context => 8,
        BuiltinFnId::Curry => 9,
        BuiltinFnId::Deref => 10,
        BuiltinFnId::Eval => 11,
        BuiltinFnId::Float => 12,
        BuiltinFnId::Gensym => 13,
        BuiltinFnId::Grad => 14,
        BuiltinFnId::Int => 15,
        BuiltinFnId::Into => 16,
        BuiltinFnId::IsInstance => 17,
        BuiltinFnId::Jvp => 18,
        BuiltinFnId::KvAllocator => 19,
        BuiltinFnId::Len => 20,
        BuiltinFnId::Macroexpand => 21,
        BuiltinFnId::MemoryPool => 22,
        BuiltinFnId::MergeWith => 23,
        BuiltinFnId::MethodsOf => 24,
        BuiltinFnId::Partial => 25,
        BuiltinFnId::Print => 26,
        BuiltinFnId::Quote => 27,
        BuiltinFnId::Range => 28,
        BuiltinFnId::Read => 29,
        BuiltinFnId::Sleep => 30,
        BuiltinFnId::Str => 31,
        BuiltinFnId::Swap => 32,
        BuiltinFnId::Tail => 33,
        BuiltinFnId::TcpConnect => 34,
        BuiltinFnId::TcpListen => 35,
        BuiltinFnId::Tensor => 36,
        BuiltinFnId::TraceDag => 37,
        BuiltinFnId::TypeOf => 38,
        BuiltinFnId::Uncurry => 39,
        BuiltinFnId::ValueAndGrad => 40,
    }
}

fn decode_builtin_fn_id(tag: u8) -> MoraResult<BuiltinFnId> {
    Ok(match tag {
        0 => BuiltinFnId::Apply,
        1 => BuiltinFnId::Atom,
        2 => BuiltinFnId::Bool,
        3 => BuiltinFnId::Car,
        4 => BuiltinFnId::Cdr,
        5 => BuiltinFnId::Channel,
        6 => BuiltinFnId::Compose,
        7 => BuiltinFnId::Cons,
        8 => BuiltinFnId::Context,
        9 => BuiltinFnId::Curry,
        10 => BuiltinFnId::Deref,
        11 => BuiltinFnId::Eval,
        12 => BuiltinFnId::Float,
        13 => BuiltinFnId::Gensym,
        14 => BuiltinFnId::Grad,
        15 => BuiltinFnId::Int,
        16 => BuiltinFnId::Into,
        17 => BuiltinFnId::IsInstance,
        18 => BuiltinFnId::Jvp,
        19 => BuiltinFnId::KvAllocator,
        20 => BuiltinFnId::Len,
        21 => BuiltinFnId::Macroexpand,
        22 => BuiltinFnId::MemoryPool,
        23 => BuiltinFnId::MergeWith,
        24 => BuiltinFnId::MethodsOf,
        25 => BuiltinFnId::Partial,
        26 => BuiltinFnId::Print,
        27 => BuiltinFnId::Quote,
        28 => BuiltinFnId::Range,
        29 => BuiltinFnId::Read,
        30 => BuiltinFnId::Sleep,
        31 => BuiltinFnId::Str,
        32 => BuiltinFnId::Swap,
        33 => BuiltinFnId::Tail,
        34 => BuiltinFnId::TcpConnect,
        35 => BuiltinFnId::TcpListen,
        36 => BuiltinFnId::Tensor,
        37 => BuiltinFnId::TraceDag,
        38 => BuiltinFnId::TypeOf,
        39 => BuiltinFnId::Uncurry,
        40 => BuiltinFnId::ValueAndGrad,
        other => return Err(corrupt(format!("invalid BuiltinFnId tag {other}"))),
    })
}

fn write_builtin_kind(w: &mut ByteWriter, kind: BuiltinKind) -> MoraResult<()> {
    match kind {
        BuiltinKind::Print => w.write_u8(0),
        BuiltinKind::Range => w.write_u8(1),
        BuiltinKind::Len => w.write_u8(2),
        BuiltinKind::Json => w.write_u8(3),
        BuiltinKind::File => w.write_u8(4),
        BuiltinKind::Exec => w.write_u8(5),
        BuiltinKind::Eval => w.write_u8(6),
        BuiltinKind::Math => w.write_u8(7),
        BuiltinKind::Stats => w.write_u8(8),
        BuiltinKind::Linalg => w.write_u8(9),
        BuiltinKind::Random => w.write_u8(10),
        BuiltinKind::Tail => w.write_u8(11),
        BuiltinKind::FreeFn(name) => {
            let id = BuiltinFnId::from_name(name).ok_or_else(|| {
                MoraError::Serialization(format!("unknown FreeFn builtin '{name}'"))
            })?;
            w.write_u8(12);
            w.write_u8(encode_builtin_fn_id(id));
        }
    }
    Ok(())
}

fn read_builtin_kind(r: &mut ByteReader<'_>) -> MoraResult<BuiltinKind> {
    Ok(match r.read_u8()? {
        0 => BuiltinKind::Print,
        1 => BuiltinKind::Range,
        2 => BuiltinKind::Len,
        3 => BuiltinKind::Json,
        4 => BuiltinKind::File,
        5 => BuiltinKind::Exec,
        6 => BuiltinKind::Eval,
        7 => BuiltinKind::Math,
        8 => BuiltinKind::Stats,
        9 => BuiltinKind::Linalg,
        10 => BuiltinKind::Random,
        11 => BuiltinKind::Tail,
        12 => {
            let id = decode_builtin_fn_id(r.read_u8()?)?;
            BuiltinKind::FreeFn(id.name())
        }
        other => return Err(corrupt(format!("invalid BuiltinKind tag {other}"))),
    })
}

fn write_scalar(w: &mut ByteWriter, s: ScalarValue) {
    match s {
        ScalarValue::F32(v) => {
            w.write_u8(0);
            w.write_f32(v);
        }
        ScalarValue::F16(b) => {
            w.write_u8(1);
            w.write_u16(b);
        }
        ScalarValue::BF16(b) => {
            w.write_u8(2);
            w.write_u16(b);
        }
        ScalarValue::FP8E4M3(b) => {
            w.write_u8(3);
            w.write_u8(b);
        }
        ScalarValue::FP8E5M2(b) => {
            w.write_u8(4);
            w.write_u8(b);
        }
        ScalarValue::I32(v) => {
            w.write_u8(5);
            w.write_u32(v as u32);
        }
        ScalarValue::I16(v) => {
            w.write_u8(6);
            w.write_u16(v as u16);
        }
        ScalarValue::I8(v) => {
            w.write_u8(7);
            w.write_u8(v as u8);
        }
        ScalarValue::I4(v) => {
            w.write_u8(8);
            w.write_u8(v as u8);
        }
        ScalarValue::U64(v) => {
            w.write_u8(9);
            w.write_u64(v);
        }
        ScalarValue::U32(v) => {
            w.write_u8(10);
            w.write_u32(v);
        }
        ScalarValue::U16(v) => {
            w.write_u8(11);
            w.write_u16(v);
        }
        ScalarValue::U8(v) => {
            w.write_u8(12);
            w.write_u8(v);
        }
    }
}

fn read_scalar(r: &mut ByteReader<'_>) -> MoraResult<ScalarValue> {
    Ok(match r.read_u8()? {
        0 => ScalarValue::F32(r.read_f32()?),
        1 => ScalarValue::F16(r.read_u16()?),
        2 => ScalarValue::BF16(r.read_u16()?),
        3 => ScalarValue::FP8E4M3(r.read_u8()?),
        4 => ScalarValue::FP8E5M2(r.read_u8()?),
        5 => ScalarValue::I32(r.read_u32()? as i32),
        6 => ScalarValue::I16(r.read_u16()? as i16),
        7 => ScalarValue::I8(r.read_u8()? as i8),
        8 => {
            let v = r.read_u8()? as i8;
            if !(-8..=7).contains(&v) {
                return Err(corrupt(format!("ScalarValue::I4 out of range: {v}")));
            }
            ScalarValue::I4(v)
        }
        9 => ScalarValue::U64(r.read_u64()?),
        10 => ScalarValue::U32(r.read_u32()?),
        11 => ScalarValue::U16(r.read_u16()?),
        12 => ScalarValue::U8(r.read_u8()?),
        other => return Err(corrupt(format!("invalid ScalarValue tag {other}"))),
    })
}

fn write_binary_op(w: &mut ByteWriter, op: BinaryOp) {
    let tag = match op {
        BinaryOp::Add => 0,
        BinaryOp::Sub => 1,
        BinaryOp::Mul => 2,
        BinaryOp::Div => 3,
        BinaryOp::Mod => 4,
        BinaryOp::Pow => 5,
        BinaryOp::MatMul => 6,
        BinaryOp::Equal => 7,
        BinaryOp::NotEqual => 8,
        BinaryOp::Greater => 9,
        BinaryOp::Less => 10,
        BinaryOp::GreaterEqual => 11,
        BinaryOp::LessEqual => 12,
    };
    w.write_u8(tag);
}

fn read_binary_op(r: &mut ByteReader<'_>) -> MoraResult<BinaryOp> {
    Ok(match r.read_u8()? {
        0 => BinaryOp::Add,
        1 => BinaryOp::Sub,
        2 => BinaryOp::Mul,
        3 => BinaryOp::Div,
        4 => BinaryOp::Mod,
        5 => BinaryOp::Pow,
        6 => BinaryOp::MatMul,
        7 => BinaryOp::Equal,
        8 => BinaryOp::NotEqual,
        9 => BinaryOp::Greater,
        10 => BinaryOp::Less,
        11 => BinaryOp::GreaterEqual,
        12 => BinaryOp::LessEqual,
        other => return Err(corrupt(format!("invalid BinaryOp tag {other}"))),
    })
}

fn write_const(w: &mut ByteWriter, c: &ControlConst) -> MoraResult<()> {
    match c {
        ControlConst::Nil => w.write_u8(0),
        ControlConst::Bool(b) => {
            w.write_u8(1);
            w.write_bool(*b);
        }
        ControlConst::Int(i) => {
            w.write_u8(2);
            w.write_i64(*i);
        }
        ControlConst::Float(f) => {
            w.write_u8(3);
            w.write_f64(*f);
        }
        ControlConst::Scalar(s) => {
            w.write_u8(4);
            write_scalar(w, *s);
        }
        ControlConst::String(s) => {
            w.write_u8(5);
            w.write_str(s)?;
        }
        ControlConst::Char(ch) => {
            w.write_u8(6);
            w.write_u32(u32::from(*ch));
        }
        ControlConst::BuiltinFn(id) => {
            w.write_u8(7);
            w.write_u8(encode_builtin_fn_id(*id));
        }
        ControlConst::BuiltinModule(kind) => {
            w.write_u8(8);
            write_builtin_kind(w, *kind)?;
        }
        ControlConst::MissingArg => w.write_u8(9),
    }
    Ok(())
}

fn read_const(r: &mut ByteReader<'_>) -> MoraResult<ControlConst> {
    Ok(match r.read_u8()? {
        0 => ControlConst::Nil,
        1 => ControlConst::Bool(r.read_bool()?),
        2 => ControlConst::Int(r.read_i64()?),
        3 => ControlConst::Float(r.read_f64()?),
        4 => ControlConst::Scalar(read_scalar(r)?),
        5 => ControlConst::String(r.read_str()?),
        6 => {
            let code = r.read_u32()?;
            let ch = char::from_u32(code)
                .ok_or_else(|| corrupt(format!("invalid char scalar {code}")))?;
            ControlConst::Char(ch)
        }
        7 => ControlConst::BuiltinFn(decode_builtin_fn_id(r.read_u8()?)?),
        8 => ControlConst::BuiltinModule(read_builtin_kind(r)?),
        9 => ControlConst::MissingArg,
        other => return Err(corrupt(format!("invalid ControlConst tag {other}"))),
    })
}

fn write_callee(w: &mut ByteWriter, target: &CalleeTarget) {
    match *target {
        CalleeTarget::Builtin(id) => {
            w.write_u8(0);
            w.write_u8(encode_builtin_fn_id(id));
        }
        CalleeTarget::Fn(id) => {
            w.write_u8(1);
            w.write_fn(id);
        }
        CalleeTarget::Value(reg) => {
            w.write_u8(2);
            w.write_reg(reg);
        }
        CalleeTarget::DynGlobal(sym) => {
            w.write_u8(3);
            w.write_sym(sym);
        }
    }
}

fn read_callee(r: &mut ByteReader<'_>) -> MoraResult<CalleeTarget> {
    Ok(match r.read_u8()? {
        0 => CalleeTarget::Builtin(decode_builtin_fn_id(r.read_u8()?)?),
        1 => CalleeTarget::Fn(r.read_fn()?),
        2 => CalleeTarget::Value(r.read_reg()?),
        3 => CalleeTarget::DynGlobal(r.read_sym()?),
        other => return Err(corrupt(format!("invalid CalleeTarget tag {other}"))),
    })
}

fn write_pattern(w: &mut ByteWriter, pat: &CompiledPattern, depth: usize) -> MoraResult<()> {
    if depth > MAX_PATTERN_DEPTH {
        return Err(corrupt("CompiledPattern exceeded maximum nesting depth"));
    }
    match pat {
        CompiledPattern::Wildcard => w.write_u8(0),
        CompiledPattern::Bind(tgt) => {
            w.write_u8(1);
            match *tgt {
                PatternTarget::Local(s) => {
                    w.write_u8(0);
                    w.write_local(s);
                }
                PatternTarget::Global(s) => {
                    w.write_u8(1);
                    w.write_global(s);
                }
                PatternTarget::Dyn(s) => {
                    w.write_u8(2);
                    w.write_sym(s);
                }
                PatternTarget::Discard => w.write_u8(3),
            }
        }
        CompiledPattern::Nil => w.write_u8(2),
        CompiledPattern::Bool(b) => {
            w.write_u8(3);
            w.write_bool(*b);
        }
        CompiledPattern::Int(i) => {
            w.write_u8(4);
            w.write_i64(*i);
        }
        CompiledPattern::Float(f) => {
            w.write_u8(5);
            w.write_f64(*f);
        }
        CompiledPattern::Scalar(s) => {
            w.write_u8(6);
            write_scalar(w, *s);
        }
        CompiledPattern::Char(ch) => {
            w.write_u8(7);
            w.write_u32(u32::from(*ch));
        }
        CompiledPattern::String(s) => {
            w.write_u8(8);
            w.write_str(s)?;
        }
        CompiledPattern::Tuple(elems) => {
            w.write_u8(9);
            w.write_len(elems.len())?;
            for e in elems {
                write_pattern(w, e, depth + 1)?;
            }
        }
        CompiledPattern::ListVec { elements, rest } => {
            w.write_u8(10);
            w.write_len(elements.len())?;
            for e in elements {
                write_pattern(w, e, depth + 1)?;
            }
            match rest {
                None => w.write_u8(0),
                Some(rpat) => {
                    w.write_u8(1);
                    write_pattern(w, rpat, depth + 1)?;
                }
            }
        }
        CompiledPattern::ListCons { head, tail } => {
            w.write_u8(11);
            write_pattern(w, head, depth + 1)?;
            write_pattern(w, tail, depth + 1)?;
        }
        CompiledPattern::Dict { required, rest } => {
            w.write_u8(12);
            w.write_len(required.len())?;
            for (k, vpat) in required {
                w.write_str(k)?;
                write_pattern(w, vpat, depth + 1)?;
            }
            w.write_bool(*rest);
        }
        CompiledPattern::TypeAscription { ty_name, inner } => {
            w.write_u8(13);
            w.write_str(ty_name)?;
            write_pattern(w, inner, depth + 1)?;
        }
        CompiledPattern::Variant {
            enum_name,
            variant,
            fields,
        } => {
            w.write_u8(14);
            w.write_opt_str(enum_name.as_deref())?;
            w.write_str(variant)?;
            w.write_len(fields.len())?;
            for f in fields {
                write_pattern(w, f, depth + 1)?;
            }
        }
        CompiledPattern::Struct { name, fields, rest } => {
            w.write_u8(15);
            w.write_str(name)?;
            w.write_len(fields.len())?;
            for (fname, fpat) in fields {
                w.write_str(fname)?;
                write_pattern(w, fpat, depth + 1)?;
            }
            w.write_bool(*rest);
        }
    }
    Ok(())
}

fn read_pattern(r: &mut ByteReader<'_>, depth: usize) -> MoraResult<CompiledPattern> {
    if depth > MAX_PATTERN_DEPTH {
        return Err(corrupt("CompiledPattern exceeded maximum nesting depth"));
    }
    Ok(match r.read_u8()? {
        0 => CompiledPattern::Wildcard,
        1 => {
            let tgt = match r.read_u8()? {
                0 => PatternTarget::Local(r.read_local()?),
                1 => PatternTarget::Global(r.read_global()?),
                2 => PatternTarget::Dyn(r.read_sym()?),
                3 => PatternTarget::Discard,
                other => return Err(corrupt(format!("invalid PatternTarget tag {other}"))),
            };
            CompiledPattern::Bind(tgt)
        }
        2 => CompiledPattern::Nil,
        3 => CompiledPattern::Bool(r.read_bool()?),
        4 => CompiledPattern::Int(r.read_i64()?),
        5 => CompiledPattern::Float(r.read_f64()?),
        6 => CompiledPattern::Scalar(read_scalar(r)?),
        7 => {
            let code = r.read_u32()?;
            let ch = char::from_u32(code)
                .ok_or_else(|| corrupt(format!("invalid pattern char {code}")))?;
            CompiledPattern::Char(ch)
        }
        8 => CompiledPattern::String(r.read_str()?),
        9 => {
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut elems = Vec::with_capacity(n);
            for _ in 0..n {
                elems.push(read_pattern(r, depth + 1)?);
            }
            CompiledPattern::Tuple(elems)
        }
        10 => {
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut elements = Vec::with_capacity(n);
            for _ in 0..n {
                elements.push(read_pattern(r, depth + 1)?);
            }
            let rest = match r.read_u8()? {
                0 => None,
                1 => Some(Box::new(read_pattern(r, depth + 1)?)),
                other => return Err(corrupt(format!("invalid rest option tag {other}"))),
            };
            CompiledPattern::ListVec { elements, rest }
        }
        11 => CompiledPattern::ListCons {
            head: Box::new(read_pattern(r, depth + 1)?),
            tail: Box::new(read_pattern(r, depth + 1)?),
        },
        12 => {
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut required = Vec::with_capacity(n);
            for _ in 0..n {
                let k = r.read_str()?;
                let p = read_pattern(r, depth + 1)?;
                required.push((k, p));
            }
            let rest = r.read_bool()?;
            CompiledPattern::Dict { required, rest }
        }
        13 => CompiledPattern::TypeAscription {
            ty_name: r.read_str()?,
            inner: Box::new(read_pattern(r, depth + 1)?),
        },
        14 => {
            let enum_name = r.read_opt_str()?;
            let variant = r.read_str()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                fields.push(read_pattern(r, depth + 1)?);
            }
            CompiledPattern::Variant {
                enum_name,
                variant,
                fields,
            }
        }
        15 => {
            let name = r.read_str()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                let fname = r.read_str()?;
                let fpat = read_pattern(r, depth + 1)?;
                fields.push((fname, fpat));
            }
            let rest = r.read_bool()?;
            CompiledPattern::Struct { name, fields, rest }
        }
        other => return Err(corrupt(format!("invalid CompiledPattern tag {other}"))),
    })
}

fn write_opt_global(w: &mut ByteWriter, dst: Option<GlobalSlot>) {
    match dst {
        None => w.write_u8(0),
        Some(g) => {
            w.write_u8(1);
            w.write_global(g);
        }
    }
}

fn read_opt_global(r: &mut ByteReader<'_>) -> MoraResult<Option<GlobalSlot>> {
    match r.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(r.read_global()?)),
        other => Err(corrupt(format!("invalid opt GlobalSlot tag {other}"))),
    }
}

fn write_opt_fn(w: &mut ByteWriter, id: Option<FnId>) {
    match id {
        None => w.write_u8(0),
        Some(f) => {
            w.write_u8(1);
            w.write_fn(f);
        }
    }
}

fn read_opt_fn(r: &mut ByteReader<'_>) -> MoraResult<Option<FnId>> {
    match r.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(r.read_fn()?)),
        other => Err(corrupt(format!("invalid opt FnId tag {other}"))),
    }
}

fn write_opt_reg(w: &mut ByteWriter, reg: Option<VReg>) {
    match reg {
        None => w.write_u8(0),
        Some(v) => {
            w.write_u8(1);
            w.write_reg(v);
        }
    }
}

fn read_opt_reg(r: &mut ByteReader<'_>) -> MoraResult<Option<VReg>> {
    match r.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(r.read_reg()?)),
        other => Err(corrupt(format!("invalid opt VReg tag {other}"))),
    }
}

fn write_inst(w: &mut ByteWriter, inst: &ControlInst) -> MoraResult<()> {
    match inst {
        ControlInst::Const(d, c) => {
            w.write_u8(0);
            w.write_reg(*d);
            write_const(w, c)?;
        }
        ControlInst::Copy(d, s) => {
            w.write_u8(1);
            w.write_reg(*d);
            w.write_reg(*s);
        }
        ControlInst::Unary(d, op, s) => {
            w.write_u8(2);
            w.write_reg(*d);
            w.write_u8(match op {
                UnaryOp::Truthy => 0,
                UnaryOp::Not => 1,
                UnaryOp::Neg => 2,
            });
            w.write_reg(*s);
        }
        ControlInst::Binary(d, l, op, r) => {
            w.write_u8(3);
            w.write_reg(*d);
            w.write_reg(*l);
            write_binary_op(w, *op);
            w.write_reg(*r);
        }
        ControlInst::ListLit(d, items) => {
            w.write_u8(4);
            w.write_reg(*d);
            w.write_regs(items)?;
        }
        ControlInst::DictLit(d, pairs) => {
            w.write_u8(5);
            w.write_reg(*d);
            w.write_named_regs(pairs)?;
        }
        ControlInst::Quasiquote(d, segs) => {
            w.write_u8(6);
            w.write_reg(*d);
            w.write_len(segs.len())?;
            for seg in segs {
                match seg {
                    ControlQqSegment::Quote(s) => {
                        w.write_u8(0);
                        w.write_str(s)?;
                    }
                    ControlQqSegment::Unquote(r) => {
                        w.write_u8(1);
                        w.write_reg(*r);
                    }
                    ControlQqSegment::UnquoteSplice(r) => {
                        w.write_u8(2);
                        w.write_reg(*r);
                    }
                }
            }
        }
        ControlInst::MakeClosure {
            dst,
            fn_id,
            captures,
        } => {
            w.write_u8(7);
            w.write_reg(*dst);
            w.write_fn(*fn_id);
            w.write_captures(captures)?;
        }
        ControlInst::MakeDynTrait {
            dst,
            src,
            trait_name,
            generics,
        } => {
            w.write_u8(8);
            w.write_reg(*dst);
            w.write_reg(*src);
            w.write_sym(*trait_name);
            w.write_len(generics.len())?;
            for g in generics {
                w.write_str(g)?;
            }
        }
        ControlInst::LoadLocal(d, s) => {
            w.write_u8(9);
            w.write_reg(*d);
            w.write_local(*s);
        }
        ControlInst::StoreLocal(s, v) => {
            w.write_u8(10);
            w.write_local(*s);
            w.write_reg(*v);
        }
        ControlInst::IsMissingArg(d, s) => {
            w.write_u8(11);
            w.write_reg(*d);
            w.write_local(*s);
        }
        ControlInst::LoadUpvalue(d, s) => {
            w.write_u8(12);
            w.write_reg(*d);
            w.write_upvalue(*s);
        }
        ControlInst::StoreUpvalue(s, v) => {
            w.write_u8(13);
            w.write_upvalue(*s);
            w.write_reg(*v);
        }
        ControlInst::LoadGlobal(d, s) => {
            w.write_u8(14);
            w.write_reg(*d);
            w.write_global(*s);
        }
        ControlInst::StoreGlobal(s, v) => {
            w.write_u8(15);
            w.write_global(*s);
            w.write_reg(*v);
        }
        ControlInst::LoadDyn(d, sym) => {
            w.write_u8(16);
            w.write_reg(*d);
            w.write_sym(*sym);
        }
        ControlInst::DefineDyn(sym, v, exported) => {
            w.write_u8(17);
            w.write_sym(*sym);
            w.write_reg(*v);
            w.write_bool(*exported);
        }
        ControlInst::AssignDyn(sym, v) => {
            w.write_u8(18);
            w.write_sym(*sym);
            w.write_reg(*v);
        }
        ControlInst::ExportGlobal(s) => {
            w.write_u8(19);
            w.write_global(*s);
        }
        ControlInst::ExportDyn(sym) => {
            w.write_u8(20);
            w.write_sym(*sym);
        }
        ControlInst::Index(d, o, i) => {
            w.write_u8(21);
            w.write_reg(*d);
            w.write_reg(*o);
            w.write_reg(*i);
        }
        ControlInst::IndexAssign(d, o, i, v) => {
            w.write_u8(22);
            w.write_reg(*d);
            w.write_reg(*o);
            w.write_reg(*i);
            w.write_reg(*v);
        }
        ControlInst::TensorSlice { dst, target, axes } => {
            w.write_u8(23);
            w.write_reg(*dst);
            w.write_reg(*target);
            w.write_len(axes.len())?;
            for ax in axes {
                match ax {
                    ControlSliceAxis::Index(r) => {
                        w.write_u8(0);
                        w.write_reg(*r);
                    }
                    ControlSliceAxis::Range { start, stop, step } => {
                        w.write_u8(1);
                        write_opt_reg(w, *start);
                        write_opt_reg(w, *stop);
                        write_opt_reg(w, *step);
                    }
                    ControlSliceAxis::Ellipsis => w.write_u8(2),
                    ControlSliceAxis::NewAxis => w.write_u8(3),
                }
            }
        }
        ControlInst::Call {
            dst,
            target,
            args,
            named_args,
        } => {
            w.write_u8(24);
            w.write_reg(*dst);
            write_callee(w, target);
            w.write_regs(args)?;
            w.write_named_regs(named_args)?;
        }
        ControlInst::ModuleCall {
            dst,
            module,
            method,
            args,
            named_args,
        } => {
            w.write_u8(25);
            w.write_reg(*dst);
            write_builtin_kind(w, *module)?;
            w.write_sym(*method);
            w.write_regs(args)?;
            w.write_named_regs(named_args)?;
        }
        ControlInst::MethodCall {
            dst,
            receiver,
            method,
            args,
            named_args,
        } => {
            w.write_u8(26);
            w.write_reg(*dst);
            w.write_reg(*receiver);
            w.write_sym(*method);
            w.write_regs(args)?;
            w.write_named_regs(named_args)?;
        }
        ControlInst::Pipe { dst, lhs, rhs } => {
            w.write_u8(27);
            w.write_reg(*dst);
            w.write_reg(*lhs);
            w.write_reg(*rhs);
        }
        ControlInst::MatchPattern {
            dst_ok,
            val,
            pattern,
        } => {
            w.write_u8(28);
            w.write_reg(*dst_ok);
            w.write_reg(*val);
            write_pattern(w, pattern, 0)?;
        }
        ControlInst::Handle {
            dst,
            effect,
            body_fn,
            body_captures,
            handler_fn,
            handler_captures,
            auto_resume,
        } => {
            w.write_u8(29);
            w.write_reg(*dst);
            w.write_sym(*effect);
            w.write_fn(*body_fn);
            w.write_captures(body_captures)?;
            w.write_fn(*handler_fn);
            w.write_captures(handler_captures)?;
            w.write_bool(*auto_resume);
        }
        ControlInst::Transaction {
            dst,
            body_fn,
            body_captures,
            comp_fn,
            comp_captures,
        } => {
            w.write_u8(30);
            w.write_reg(*dst);
            w.write_fn(*body_fn);
            w.write_captures(body_captures)?;
            w.write_fn(*comp_fn);
            w.write_captures(comp_captures)?;
        }
        ControlInst::Parallel { dst, segments } => {
            w.write_u8(31);
            w.write_reg(*dst);
            w.write_len(segments.len())?;
            for seg in segments {
                match seg {
                    ParallelSegment::WorkerBatch(workers) => {
                        w.write_u8(0);
                        w.write_len(workers.len())?;
                        for (sym, fid, caps) in workers {
                            w.write_sym(*sym);
                            w.write_fn(*fid);
                            w.write_captures(caps)?;
                        }
                    }
                    ParallelSegment::Sequential(fid, caps) => {
                        w.write_u8(1);
                        w.write_fn(*fid);
                        w.write_captures(caps)?;
                    }
                }
            }
        }
        ControlInst::Spawn { dst, target } => {
            w.write_u8(32);
            w.write_reg(*dst);
            match target {
                ControlSpawnTarget::Call {
                    target,
                    args,
                    named_args,
                } => {
                    w.write_u8(0);
                    write_callee(w, target);
                    w.write_regs(args)?;
                    w.write_named_regs(named_args)?;
                }
                ControlSpawnTarget::MethodCall {
                    receiver,
                    method,
                    args,
                    named_args,
                } => {
                    w.write_u8(1);
                    w.write_reg(*receiver);
                    w.write_sym(*method);
                    w.write_regs(args)?;
                    w.write_named_regs(named_args)?;
                }
                ControlSpawnTarget::Callable(r) => {
                    w.write_u8(2);
                    w.write_reg(*r);
                }
            }
        }
        ControlInst::Select { dst, arms } => {
            w.write_u8(33);
            w.write_reg(*dst);
            w.write_len(arms.len())?;
            for arm in arms {
                match arm {
                    ControlSelectArm::Recv {
                        channel,
                        has_binding,
                        body_fn,
                        captures,
                    } => {
                        w.write_u8(0);
                        w.write_reg(*channel);
                        w.write_bool(*has_binding);
                        w.write_fn(*body_fn);
                        w.write_captures(captures)?;
                    }
                    ControlSelectArm::Send {
                        channel,
                        value,
                        body_fn,
                        captures,
                    } => {
                        w.write_u8(1);
                        w.write_reg(*channel);
                        w.write_reg(*value);
                        w.write_fn(*body_fn);
                        w.write_captures(captures)?;
                    }
                    ControlSelectArm::Timeout {
                        ms,
                        body_fn,
                        captures,
                    } => {
                        w.write_u8(2);
                        w.write_reg(*ms);
                        w.write_fn(*body_fn);
                        w.write_captures(captures)?;
                    }
                    ControlSelectArm::Default { body_fn, captures } => {
                        w.write_u8(3);
                        w.write_fn(*body_fn);
                        w.write_captures(captures)?;
                    }
                }
            }
        }
        ControlInst::NoGrad {
            dst,
            body_fn,
            body_captures,
        } => {
            w.write_u8(34);
            w.write_reg(*dst);
            w.write_fn(*body_fn);
            w.write_captures(body_captures)?;
        }
        ControlInst::Import { path } => {
            w.write_u8(35);
            w.write_str(path)?;
        }
        ControlInst::EvalAssert {
            name,
            given,
            expects,
            tolerance,
            replay_path,
        } => {
            w.write_u8(36);
            w.write_sym(*name);
            w.write_reg(*given);
            w.write_regs(expects)?;
            match tolerance {
                None => w.write_u8(0),
                Some(t) => {
                    w.write_u8(1);
                    w.write_f64(*t);
                }
            }
            w.write_opt_str(replay_path.as_deref())?;
        }
        ControlInst::Assert(d, cond) => {
            w.write_u8(37);
            w.write_reg(*d);
            w.write_reg(*cond);
        }
        ControlInst::Commit => w.write_u8(38),
        ControlInst::Rollback => w.write_u8(39),
        ControlInst::RegisterTypeAlias { name, target, dst } => {
            w.write_u8(40);
            w.write_sym(*name);
            w.write_str(target)?;
            write_opt_global(w, *dst);
        }
        ControlInst::RegisterEnum {
            name,
            type_params,
            variants,
            dst,
        } => {
            w.write_u8(41);
            w.write_sym(*name);
            w.write_len(type_params.len())?;
            for &tp in type_params {
                w.write_sym(tp);
            }
            w.write_len(variants.len())?;
            for (vname, payloads) in variants {
                w.write_sym(*vname);
                w.write_len(payloads.len())?;
                for p in payloads {
                    w.write_str(p)?;
                }
            }
            write_opt_global(w, *dst);
        }
        ControlInst::RegisterStruct {
            name,
            type_params,
            fields,
            dst,
        } => {
            w.write_u8(42);
            w.write_sym(*name);
            w.write_len(type_params.len())?;
            for &tp in type_params {
                w.write_sym(tp);
            }
            w.write_len(fields.len())?;
            for f in fields {
                w.write_sym(f.name);
                w.write_str(&f.type_name)?;
                write_opt_fn(w, f.default_fn);
                w.write_len(f.tags.len())?;
                for (tk, tv) in &f.tags {
                    w.write_str(tk)?;
                    w.write_str(tv)?;
                }
            }
            write_opt_global(w, *dst);
        }
        ControlInst::RegisterTrait {
            name,
            type_params,
            parents,
            methods,
            dst,
        } => {
            w.write_u8(43);
            w.write_sym(*name);
            w.write_len(type_params.len())?;
            for &tp in type_params {
                w.write_sym(tp);
            }
            w.write_len(parents.len())?;
            for &p in parents {
                w.write_sym(p);
            }
            w.write_len(methods.len())?;
            for m in methods {
                w.write_sym(m.name);
                w.write_len(m.params.len())?;
                for (psym, pty) in &m.params {
                    w.write_sym(*psym);
                    w.write_opt_str(pty.as_deref())?;
                }
                w.write_opt_str(m.return_type.as_deref())?;
                w.write_bool(m.has_self);
                write_opt_fn(w, m.default_fn);
            }
            write_opt_global(w, *dst);
        }
        ControlInst::RegisterImpl {
            trait_name,
            trait_generics,
            target_type,
            target_generics,
            methods,
        } => {
            w.write_u8(44);
            match trait_name {
                None => w.write_u8(0),
                Some(sym) => {
                    w.write_u8(1);
                    w.write_sym(*sym);
                }
            }
            w.write_len(trait_generics.len())?;
            for g in trait_generics {
                w.write_str(g)?;
            }
            w.write_sym(*target_type);
            w.write_len(target_generics.len())?;
            for g in target_generics {
                w.write_str(g)?;
            }
            w.write_len(methods.len())?;
            for m in methods {
                w.write_sym(m.name);
                w.write_fn(m.fn_id);
                w.write_bool(m.has_self);
            }
        }
        ControlInst::RegisterMacro {
            name,
            params,
            body_fn,
            dst,
        } => {
            w.write_u8(45);
            w.write_sym(*name);
            w.write_len(params.len())?;
            for &p in params {
                w.write_sym(p);
            }
            w.write_fn(*body_fn);
            write_opt_global(w, *dst);
        }
        ControlInst::RegisterTask {
            name,
            params,
            fn_id,
            dst,
        } => {
            w.write_u8(46);
            w.write_sym(*name);
            w.write_len(params.len())?;
            for &p in params {
                w.write_sym(p);
            }
            w.write_fn(*fn_id);
            write_opt_global(w, *dst);
        }
        ControlInst::KeepAlive(r) => {
            w.write_u8(47);
            w.write_reg(*r);
        }
    }
    Ok(())
}

fn read_inst(r: &mut ByteReader<'_>) -> MoraResult<ControlInst> {
    Ok(match r.read_u8()? {
        0 => ControlInst::Const(r.read_reg()?, read_const(r)?),
        1 => ControlInst::Copy(r.read_reg()?, r.read_reg()?),
        2 => {
            let d = r.read_reg()?;
            let op = match r.read_u8()? {
                0 => UnaryOp::Truthy,
                1 => UnaryOp::Not,
                2 => UnaryOp::Neg,
                other => return Err(corrupt(format!("invalid UnaryOp tag {other}"))),
            };
            ControlInst::Unary(d, op, r.read_reg()?)
        }
        3 => ControlInst::Binary(
            r.read_reg()?,
            r.read_reg()?,
            read_binary_op(r)?,
            r.read_reg()?,
        ),
        4 => ControlInst::ListLit(r.read_reg()?, r.read_regs()?),
        5 => ControlInst::DictLit(r.read_reg()?, r.read_named_regs()?),
        6 => {
            let d = r.read_reg()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut segs = Vec::with_capacity(n);
            for _ in 0..n {
                segs.push(match r.read_u8()? {
                    0 => ControlQqSegment::Quote(r.read_str()?),
                    1 => ControlQqSegment::Unquote(r.read_reg()?),
                    2 => ControlQqSegment::UnquoteSplice(r.read_reg()?),
                    other => return Err(corrupt(format!("invalid ControlQqSegment tag {other}"))),
                });
            }
            ControlInst::Quasiquote(d, segs)
        }
        7 => ControlInst::MakeClosure {
            dst: r.read_reg()?,
            fn_id: r.read_fn()?,
            captures: r.read_captures()?,
        },
        8 => {
            let dst = r.read_reg()?;
            let src = r.read_reg()?;
            let trait_name = r.read_sym()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut generics = Vec::with_capacity(n);
            for _ in 0..n {
                generics.push(r.read_str()?);
            }
            ControlInst::MakeDynTrait {
                dst,
                src,
                trait_name,
                generics,
            }
        }
        9 => ControlInst::LoadLocal(r.read_reg()?, r.read_local()?),
        10 => ControlInst::StoreLocal(r.read_local()?, r.read_reg()?),
        11 => ControlInst::IsMissingArg(r.read_reg()?, r.read_local()?),
        12 => ControlInst::LoadUpvalue(r.read_reg()?, r.read_upvalue()?),
        13 => ControlInst::StoreUpvalue(r.read_upvalue()?, r.read_reg()?),
        14 => ControlInst::LoadGlobal(r.read_reg()?, r.read_global()?),
        15 => ControlInst::StoreGlobal(r.read_global()?, r.read_reg()?),
        16 => ControlInst::LoadDyn(r.read_reg()?, r.read_sym()?),
        17 => ControlInst::DefineDyn(r.read_sym()?, r.read_reg()?, r.read_bool()?),
        18 => ControlInst::AssignDyn(r.read_sym()?, r.read_reg()?),
        19 => ControlInst::ExportGlobal(r.read_global()?),
        20 => ControlInst::ExportDyn(r.read_sym()?),
        21 => ControlInst::Index(r.read_reg()?, r.read_reg()?, r.read_reg()?),
        22 => ControlInst::IndexAssign(r.read_reg()?, r.read_reg()?, r.read_reg()?, r.read_reg()?),
        23 => {
            let dst = r.read_reg()?;
            let target = r.read_reg()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut axes = Vec::with_capacity(n);
            for _ in 0..n {
                axes.push(match r.read_u8()? {
                    0 => ControlSliceAxis::Index(r.read_reg()?),
                    1 => ControlSliceAxis::Range {
                        start: read_opt_reg(r)?,
                        stop: read_opt_reg(r)?,
                        step: read_opt_reg(r)?,
                    },
                    2 => ControlSliceAxis::Ellipsis,
                    3 => ControlSliceAxis::NewAxis,
                    other => return Err(corrupt(format!("invalid ControlSliceAxis tag {other}"))),
                });
            }
            ControlInst::TensorSlice { dst, target, axes }
        }
        24 => ControlInst::Call {
            dst: r.read_reg()?,
            target: read_callee(r)?,
            args: r.read_regs()?,
            named_args: r.read_named_regs()?,
        },
        25 => ControlInst::ModuleCall {
            dst: r.read_reg()?,
            module: read_builtin_kind(r)?,
            method: r.read_sym()?,
            args: r.read_regs()?,
            named_args: r.read_named_regs()?,
        },
        26 => ControlInst::MethodCall {
            dst: r.read_reg()?,
            receiver: r.read_reg()?,
            method: r.read_sym()?,
            args: r.read_regs()?,
            named_args: r.read_named_regs()?,
        },
        27 => ControlInst::Pipe {
            dst: r.read_reg()?,
            lhs: r.read_reg()?,
            rhs: r.read_reg()?,
        },
        28 => ControlInst::MatchPattern {
            dst_ok: r.read_reg()?,
            val: r.read_reg()?,
            pattern: read_pattern(r, 0)?,
        },
        29 => ControlInst::Handle {
            dst: r.read_reg()?,
            effect: r.read_sym()?,
            body_fn: r.read_fn()?,
            body_captures: r.read_captures()?,
            handler_fn: r.read_fn()?,
            handler_captures: r.read_captures()?,
            auto_resume: r.read_bool()?,
        },
        30 => ControlInst::Transaction {
            dst: r.read_reg()?,
            body_fn: r.read_fn()?,
            body_captures: r.read_captures()?,
            comp_fn: r.read_fn()?,
            comp_captures: r.read_captures()?,
        },
        31 => {
            let dst = r.read_reg()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut segments = Vec::with_capacity(n);
            for _ in 0..n {
                segments.push(match r.read_u8()? {
                    0 => {
                        let wn = r.read_len(MAX_COLLECTION_LEN)?;
                        let mut workers = Vec::with_capacity(wn);
                        for _ in 0..wn {
                            workers.push((r.read_sym()?, r.read_fn()?, r.read_captures()?));
                        }
                        ParallelSegment::WorkerBatch(workers)
                    }
                    1 => ParallelSegment::Sequential(r.read_fn()?, r.read_captures()?),
                    other => return Err(corrupt(format!("invalid ParallelSegment tag {other}"))),
                });
            }
            ControlInst::Parallel { dst, segments }
        }
        32 => {
            let dst = r.read_reg()?;
            let target = match r.read_u8()? {
                0 => ControlSpawnTarget::Call {
                    target: read_callee(r)?,
                    args: r.read_regs()?,
                    named_args: r.read_named_regs()?,
                },
                1 => ControlSpawnTarget::MethodCall {
                    receiver: r.read_reg()?,
                    method: r.read_sym()?,
                    args: r.read_regs()?,
                    named_args: r.read_named_regs()?,
                },
                2 => ControlSpawnTarget::Callable(r.read_reg()?),
                other => return Err(corrupt(format!("invalid ControlSpawnTarget tag {other}"))),
            };
            ControlInst::Spawn { dst, target }
        }
        33 => {
            let dst = r.read_reg()?;
            let n = r.read_len(MAX_COLLECTION_LEN)?;
            let mut arms = Vec::with_capacity(n);
            for _ in 0..n {
                arms.push(match r.read_u8()? {
                    0 => ControlSelectArm::Recv {
                        channel: r.read_reg()?,
                        has_binding: r.read_bool()?,
                        body_fn: r.read_fn()?,
                        captures: r.read_captures()?,
                    },
                    1 => ControlSelectArm::Send {
                        channel: r.read_reg()?,
                        value: r.read_reg()?,
                        body_fn: r.read_fn()?,
                        captures: r.read_captures()?,
                    },
                    2 => ControlSelectArm::Timeout {
                        ms: r.read_reg()?,
                        body_fn: r.read_fn()?,
                        captures: r.read_captures()?,
                    },
                    3 => ControlSelectArm::Default {
                        body_fn: r.read_fn()?,
                        captures: r.read_captures()?,
                    },
                    other => return Err(corrupt(format!("invalid ControlSelectArm tag {other}"))),
                });
            }
            ControlInst::Select { dst, arms }
        }
        34 => ControlInst::NoGrad {
            dst: r.read_reg()?,
            body_fn: r.read_fn()?,
            body_captures: r.read_captures()?,
        },
        35 => ControlInst::Import {
            path: r.read_str()?,
        },
        36 => {
            let name = r.read_sym()?;
            let given = r.read_reg()?;
            let expects = r.read_regs()?;
            let tolerance = match r.read_u8()? {
                0 => None,
                1 => Some(r.read_f64()?),
                other => return Err(corrupt(format!("invalid tolerance tag {other}"))),
            };
            let replay_path = r.read_opt_str()?;
            ControlInst::EvalAssert {
                name,
                given,
                expects,
                tolerance,
                replay_path,
            }
        }
        37 => ControlInst::Assert(r.read_reg()?, r.read_reg()?),
        38 => ControlInst::Commit,
        39 => ControlInst::Rollback,
        40 => ControlInst::RegisterTypeAlias {
            name: r.read_sym()?,
            target: r.read_str()?,
            dst: read_opt_global(r)?,
        },
        41 => {
            let name = r.read_sym()?;
            let n_tp = r.read_len(MAX_COLLECTION_LEN)?;
            let mut type_params = Vec::with_capacity(n_tp);
            for _ in 0..n_tp {
                type_params.push(r.read_sym()?);
            }
            let n_var = r.read_len(MAX_COLLECTION_LEN)?;
            let mut variants = Vec::with_capacity(n_var);
            for _ in 0..n_var {
                let vname = r.read_sym()?;
                let n_p = r.read_len(MAX_COLLECTION_LEN)?;
                let mut payloads = Vec::with_capacity(n_p);
                for _ in 0..n_p {
                    payloads.push(r.read_str()?);
                }
                variants.push((vname, payloads));
            }
            ControlInst::RegisterEnum {
                name,
                type_params,
                variants,
                dst: read_opt_global(r)?,
            }
        }
        42 => {
            let name = r.read_sym()?;
            let n_tp = r.read_len(MAX_COLLECTION_LEN)?;
            let mut type_params = Vec::with_capacity(n_tp);
            for _ in 0..n_tp {
                type_params.push(r.read_sym()?);
            }
            let n_f = r.read_len(MAX_COLLECTION_LEN)?;
            let mut fields = Vec::with_capacity(n_f);
            for _ in 0..n_f {
                let fname = r.read_sym()?;
                let type_name = r.read_str()?;
                let default_fn = read_opt_fn(r)?;
                let n_t = r.read_len(MAX_COLLECTION_LEN)?;
                let mut tags = Vec::with_capacity(n_t);
                for _ in 0..n_t {
                    tags.push((r.read_str()?, r.read_str()?));
                }
                fields.push(ControlStructField {
                    name: fname,
                    type_name,
                    default_fn,
                    tags,
                });
            }
            ControlInst::RegisterStruct {
                name,
                type_params,
                fields,
                dst: read_opt_global(r)?,
            }
        }
        43 => {
            let name = r.read_sym()?;
            let n_tp = r.read_len(MAX_COLLECTION_LEN)?;
            let mut type_params = Vec::with_capacity(n_tp);
            for _ in 0..n_tp {
                type_params.push(r.read_sym()?);
            }
            let n_par = r.read_len(MAX_COLLECTION_LEN)?;
            let mut parents = Vec::with_capacity(n_par);
            for _ in 0..n_par {
                parents.push(r.read_sym()?);
            }
            let n_m = r.read_len(MAX_COLLECTION_LEN)?;
            let mut methods = Vec::with_capacity(n_m);
            for _ in 0..n_m {
                let mname = r.read_sym()?;
                let n_p = r.read_len(MAX_COLLECTION_LEN)?;
                let mut params = Vec::with_capacity(n_p);
                for _ in 0..n_p {
                    params.push((r.read_sym()?, r.read_opt_str()?));
                }
                let return_type = r.read_opt_str()?;
                let has_self = r.read_bool()?;
                let default_fn = read_opt_fn(r)?;
                methods.push(ControlTraitMethod {
                    name: mname,
                    params,
                    return_type,
                    has_self,
                    default_fn,
                });
            }
            ControlInst::RegisterTrait {
                name,
                type_params,
                parents,
                methods,
                dst: read_opt_global(r)?,
            }
        }
        44 => {
            let trait_name = match r.read_u8()? {
                0 => None,
                1 => Some(r.read_sym()?),
                other => return Err(corrupt(format!("invalid opt trait_name tag {other}"))),
            };
            let n_tg = r.read_len(MAX_COLLECTION_LEN)?;
            let mut trait_generics = Vec::with_capacity(n_tg);
            for _ in 0..n_tg {
                trait_generics.push(r.read_str()?);
            }
            let target_type = r.read_sym()?;
            let n_gg = r.read_len(MAX_COLLECTION_LEN)?;
            let mut target_generics = Vec::with_capacity(n_gg);
            for _ in 0..n_gg {
                target_generics.push(r.read_str()?);
            }
            let n_m = r.read_len(MAX_COLLECTION_LEN)?;
            let mut methods = Vec::with_capacity(n_m);
            for _ in 0..n_m {
                methods.push(ControlImplMethod {
                    name: r.read_sym()?,
                    fn_id: r.read_fn()?,
                    has_self: r.read_bool()?,
                });
            }
            ControlInst::RegisterImpl {
                trait_name,
                trait_generics,
                target_type,
                target_generics,
                methods,
            }
        }
        45 => {
            let name = r.read_sym()?;
            let n_p = r.read_len(MAX_COLLECTION_LEN)?;
            let mut params = Vec::with_capacity(n_p);
            for _ in 0..n_p {
                params.push(r.read_sym()?);
            }
            ControlInst::RegisterMacro {
                name,
                params,
                body_fn: r.read_fn()?,
                dst: read_opt_global(r)?,
            }
        }
        46 => {
            let name = r.read_sym()?;
            let n_p = r.read_len(MAX_COLLECTION_LEN)?;
            let mut params = Vec::with_capacity(n_p);
            for _ in 0..n_p {
                params.push(r.read_sym()?);
            }
            ControlInst::RegisterTask {
                name,
                params,
                fn_id: r.read_fn()?,
                dst: read_opt_global(r)?,
            }
        }
        47 => ControlInst::KeepAlive(r.read_reg()?),
        other => return Err(corrupt(format!("invalid ControlInst opcode {other}"))),
    })
}

fn write_terminator(w: &mut ByteWriter, term: &ControlTerminator) -> MoraResult<()> {
    match term {
        ControlTerminator::Jump { target, args } => {
            w.write_u8(0);
            w.write_block(*target);
            w.write_regs(args)?;
        }
        ControlTerminator::Branch {
            cond,
            then_bb,
            then_args,
            else_bb,
            else_args,
        } => {
            w.write_u8(1);
            w.write_reg(*cond);
            w.write_block(*then_bb);
            w.write_regs(then_args)?;
            w.write_block(*else_bb);
            w.write_regs(else_args)?;
        }
        ControlTerminator::Return(reg) => {
            w.write_u8(2);
            write_opt_reg(w, *reg);
        }
        ControlTerminator::Perform {
            effect,
            args,
            resume_bb,
        } => {
            w.write_u8(3);
            w.write_sym(*effect);
            w.write_regs(args)?;
            w.write_block(*resume_bb);
        }
        ControlTerminator::TailCall { target, args } => {
            w.write_u8(4);
            write_callee(w, target);
            w.write_regs(args)?;
        }
        ControlTerminator::Halt(reg) => {
            w.write_u8(5);
            write_opt_reg(w, *reg);
        }
        ControlTerminator::Unreachable => w.write_u8(6),
    }
    Ok(())
}

fn read_terminator(r: &mut ByteReader<'_>) -> MoraResult<ControlTerminator> {
    Ok(match r.read_u8()? {
        0 => ControlTerminator::Jump {
            target: r.read_block()?,
            args: r.read_regs()?,
        },
        1 => ControlTerminator::Branch {
            cond: r.read_reg()?,
            then_bb: r.read_block()?,
            then_args: r.read_regs()?,
            else_bb: r.read_block()?,
            else_args: r.read_regs()?,
        },
        2 => ControlTerminator::Return(read_opt_reg(r)?),
        3 => ControlTerminator::Perform {
            effect: r.read_sym()?,
            args: r.read_regs()?,
            resume_bb: r.read_block()?,
        },
        4 => ControlTerminator::TailCall {
            target: read_callee(r)?,
            args: r.read_regs()?,
        },
        5 => ControlTerminator::Halt(read_opt_reg(r)?),
        6 => ControlTerminator::Unreachable,
        other => return Err(corrupt(format!("invalid ControlTerminator tag {other}"))),
    })
}

fn write_function_kind(w: &mut ByteWriter, kind: FunctionKind) {
    let tag = match kind {
        FunctionKind::Main => 0,
        FunctionKind::Task => 1,
        FunctionKind::Closure => 2,
        FunctionKind::Macro => 3,
        FunctionKind::HandleBody => 4,
        FunctionKind::HandleHandler => 5,
        FunctionKind::TransactionBody => 6,
        FunctionKind::TransactionComp => 7,
        FunctionKind::Worker => 8,
        FunctionKind::ParallelSeq => 9,
        FunctionKind::SelectArm => 10,
        FunctionKind::NoGradBody => 11,
        FunctionKind::TraitDefault => 12,
        FunctionKind::ImplMethod => 13,
        FunctionKind::DefaultArg => 14,
    };
    w.write_u8(tag);
}

fn read_function_kind(r: &mut ByteReader<'_>) -> MoraResult<FunctionKind> {
    Ok(match r.read_u8()? {
        0 => FunctionKind::Main,
        1 => FunctionKind::Task,
        2 => FunctionKind::Closure,
        3 => FunctionKind::Macro,
        4 => FunctionKind::HandleBody,
        5 => FunctionKind::HandleHandler,
        6 => FunctionKind::TransactionBody,
        7 => FunctionKind::TransactionComp,
        8 => FunctionKind::Worker,
        9 => FunctionKind::ParallelSeq,
        10 => FunctionKind::SelectArm,
        11 => FunctionKind::NoGradBody,
        12 => FunctionKind::TraitDefault,
        13 => FunctionKind::ImplMethod,
        14 => FunctionKind::DefaultArg,
        other => return Err(corrupt(format!("invalid FunctionKind tag {other}"))),
    })
}

fn write_program(w: &mut ByteWriter, prog: &ControlProgram) -> MoraResult<()> {
    // 1. Symbol table
    let syms = prog.symbols.as_slice();
    w.write_len(syms.len())?;
    for s in syms {
        w.write_str(s)?;
    }

    // 2. Entry function ID
    w.write_fn(prog.entry);

    // 3. Global metadata
    w.write_len(prog.globals.len())?;
    for g in &prog.globals {
        w.write_sym(g.sym);
        w.write_str(&g.name)?;
        w.write_bool(g.exported);
        match g.kind {
            GlobalKind::Var => w.write_u8(0),
            GlobalKind::Task(fid) => {
                w.write_u8(1);
                w.write_fn(fid);
            }
            GlobalKind::Macro(fid) => {
                w.write_u8(2);
                w.write_fn(fid);
            }
            GlobalKind::Enum => w.write_u8(3),
            GlobalKind::Struct => w.write_u8(4),
            GlobalKind::Trait => w.write_u8(5),
            GlobalKind::TypeAlias => w.write_u8(6),
        }
    }

    // 4. Functions
    w.write_len(prog.functions.len())?;
    for f in &prog.functions {
        w.write_fn(f.id);
        match f.name {
            None => w.write_u8(0),
            Some(sym) => {
                w.write_u8(1);
                w.write_sym(sym);
            }
        }
        write_function_kind(w, f.kind);
        w.write_len(f.params.len())?;
        for &p in &f.params {
            w.write_sym(p);
        }
        w.write_len(f.param_has_default.len())?;
        for &hd in &f.param_has_default {
            w.write_bool(hd);
        }
        w.write_len(f.upvalues.len())?;
        for uv in &f.upvalues {
            w.write_sym(uv.sym);
            match uv.source {
                CaptureSource::ParentLocal(s) => {
                    w.write_u8(0);
                    w.write_local(s);
                }
                CaptureSource::ParentUpvalue(s) => {
                    w.write_u8(1);
                    w.write_upvalue(s);
                }
            }
            w.write_bool(uv.shared);
        }
        w.write_len(f.shared_locals.len())?;
        for &sh in &f.shared_locals {
            w.write_bool(sh);
        }
        w.write_len(f.local_names.len())?;
        for &ln in &f.local_names {
            w.write_sym(ln);
        }
        w.write_u16(f.n_locals);
        w.write_var_u32(f.n_regs);
        w.write_block(f.entry);
        match f.handler_k_slot {
            None => w.write_u8(0),
            Some(s) => {
                w.write_u8(1);
                w.write_local(s);
            }
        }
        w.write_len(f.handler_arg_slots.len())?;
        for slot in &f.handler_arg_slots {
            match slot {
                None => w.write_u8(0),
                Some(s) => {
                    w.write_u8(1);
                    w.write_local(*s);
                }
            }
        }
        w.write_len(f.blocks.len())?;
        for bb in &f.blocks {
            w.write_block(bb.id);
            w.write_regs(&bb.params)?;
            w.write_len(bb.insts.len())?;
            for inst in &bb.insts {
                write_inst(w, inst)?;
            }
            write_terminator(w, &bb.terminator)?;
        }
    }
    Ok(())
}

fn read_program(r: &mut ByteReader<'_>) -> MoraResult<ControlProgram> {
    // 1. Symbol table
    let n_syms = r.read_len(MAX_COLLECTION_LEN)?;
    let mut names = Vec::with_capacity(n_syms);
    for _ in 0..n_syms {
        names.push(r.read_str()?);
    }
    let symbols = SymbolTable::from_names(names);

    // 2. Entry function ID
    let entry = r.read_fn()?;

    // 3. Global metadata
    let n_globals = r.read_len(MAX_COLLECTION_LEN)?;
    let mut globals = Vec::with_capacity(n_globals);
    for _ in 0..n_globals {
        let sym = r.read_sym()?;
        let name = r.read_str()?;
        let exported = r.read_bool()?;
        let kind = match r.read_u8()? {
            0 => GlobalKind::Var,
            1 => GlobalKind::Task(r.read_fn()?),
            2 => GlobalKind::Macro(r.read_fn()?),
            3 => GlobalKind::Enum,
            4 => GlobalKind::Struct,
            5 => GlobalKind::Trait,
            6 => GlobalKind::TypeAlias,
            other => return Err(corrupt(format!("invalid GlobalKind tag {other}"))),
        };
        globals.push(GlobalMeta {
            sym,
            name,
            exported,
            kind,
        });
    }

    // 4. Functions
    let n_funcs = r.read_len(MAX_COLLECTION_LEN)?;
    let mut functions = Vec::with_capacity(n_funcs);
    for _ in 0..n_funcs {
        let id = r.read_fn()?;
        let name = match r.read_u8()? {
            0 => None,
            1 => Some(r.read_sym()?),
            other => return Err(corrupt(format!("invalid opt fn name tag {other}"))),
        };
        let kind = read_function_kind(r)?;
        let n_params = r.read_len(MAX_COLLECTION_LEN)?;
        let mut params = Vec::with_capacity(n_params);
        for _ in 0..n_params {
            params.push(r.read_sym()?);
        }
        let n_def = r.read_len(MAX_COLLECTION_LEN)?;
        let mut param_has_default = Vec::with_capacity(n_def);
        for _ in 0..n_def {
            param_has_default.push(r.read_bool()?);
        }
        let n_uv = r.read_len(MAX_COLLECTION_LEN)?;
        let mut upvalues = Vec::with_capacity(n_uv);
        for _ in 0..n_uv {
            let sym = r.read_sym()?;
            let source = match r.read_u8()? {
                0 => CaptureSource::ParentLocal(r.read_local()?),
                1 => CaptureSource::ParentUpvalue(r.read_upvalue()?),
                other => return Err(corrupt(format!("invalid UpvalueDesc source tag {other}"))),
            };
            let shared = r.read_bool()?;
            upvalues.push(UpvalueDesc {
                sym,
                source,
                shared,
            });
        }
        let n_sh = r.read_len(MAX_COLLECTION_LEN)?;
        let mut shared_locals = Vec::with_capacity(n_sh);
        for _ in 0..n_sh {
            shared_locals.push(r.read_bool()?);
        }
        let n_ln = r.read_len(MAX_COLLECTION_LEN)?;
        let mut local_names = Vec::with_capacity(n_ln);
        for _ in 0..n_ln {
            local_names.push(r.read_sym()?);
        }
        let n_locals = r.read_u16()?;
        let n_regs = r.read_var_u32()?;
        let fn_entry = r.read_block()?;
        let handler_k_slot = match r.read_u8()? {
            0 => None,
            1 => Some(r.read_local()?),
            other => return Err(corrupt(format!("invalid handler_k_slot tag {other}"))),
        };
        let n_ha = r.read_len(MAX_COLLECTION_LEN)?;
        let mut handler_arg_slots = Vec::with_capacity(n_ha);
        for _ in 0..n_ha {
            handler_arg_slots.push(match r.read_u8()? {
                0 => None,
                1 => Some(r.read_local()?),
                other => return Err(corrupt(format!("invalid handler_arg_slot tag {other}"))),
            });
        }
        let n_blocks = r.read_len(MAX_COLLECTION_LEN)?;
        let mut blocks = Vec::with_capacity(n_blocks);
        for _ in 0..n_blocks {
            let bb_id = r.read_block()?;
            let bb_params = r.read_regs()?;
            let n_insts = r.read_len(MAX_COLLECTION_LEN)?;
            let mut insts = Vec::with_capacity(n_insts);
            for _ in 0..n_insts {
                insts.push(read_inst(r)?);
            }
            let terminator = read_terminator(r)?;
            blocks.push(BasicBlock {
                id: bb_id,
                params: bb_params,
                insts,
                terminator,
            });
        }
        functions.push(ControlFunction {
            id,
            name,
            kind,
            params,
            param_has_default,
            upvalues,
            shared_locals,
            local_names,
            n_locals,
            n_regs,
            blocks,
            entry: fn_entry,
            handler_k_slot,
            handler_arg_slots,
        });
    }

    let prog = ControlProgram {
        symbols,
        functions,
        entry,
        globals,
    };
    verify_program(&prog)?;
    Ok(prog)
}

/// Serialize a verified [`CompiledPackage`] into deterministic `MORABC02` binary bytecode.
pub(crate) fn encode_package(pkg: &CompiledPackage) -> MoraResult<Vec<u8>> {
    verify_program(&pkg.entry)?;
    for mod_prog in pkg.modules.values() {
        verify_program(mod_prog)?;
    }

    let mut w = ByteWriter::new();
    w.buf.extend_from_slice(BYTECODE_MAGIC);
    w.write_u16(BYTECODE_VERSION);
    w.write_u8(pkg.opt_level);
    write_program(&mut w, &pkg.entry)?;

    w.write_len(pkg.modules.len())?;
    for (mod_path, mod_prog) in &pkg.modules {
        w.write_str(mod_path)?;
        write_program(&mut w, mod_prog)?;
    }

    let csum = fnv1a_64(&w.buf);
    w.write_u64(csum);
    Ok(w.buf)
}

/// Deserialize and structurally verify a `MORABC02` binary bytecode package.
pub(crate) fn decode_package(data: &[u8]) -> MoraResult<CompiledPackage> {
    // Minimum size: 8 magic + 2 version + 1 opt + 8 checksum = 19 bytes.
    if data.len() < 19 {
        return Err(corrupt("payload too short"));
    }
    if &data[..8] != BYTECODE_MAGIC {
        return Err(corrupt("invalid magic header (expected MORABC02)"));
    }
    let body_end = data.len() - 8;
    let expected_csum = u64::from_le_bytes(data[body_end..].try_into().expect("8-byte checksum"));
    let actual_csum = fnv1a_64(&data[..body_end]);
    if expected_csum != actual_csum {
        return Err(corrupt("FNV-1a checksum mismatch"));
    }

    let mut r = ByteReader::new(&data[8..body_end]);
    let version = r.read_u16()?;
    if version != BYTECODE_VERSION {
        return Err(corrupt(format!(
            "unsupported bytecode version {version} (expected {BYTECODE_VERSION})"
        )));
    }
    let opt_level = r.read_u8()?;
    let entry = read_program(&mut r)?;

    let n_mods = r.read_len(MAX_COLLECTION_LEN)?;
    let mut modules = BTreeMap::new();
    for _ in 0..n_mods {
        let mod_path = r.read_str()?;
        let mod_prog = read_program(&mut r)?;
        modules.insert(mod_path, mod_prog);
    }

    if r.pos != r.data.len() {
        return Err(corrupt(format!(
            "trailing {} unread bytes in bytecode payload",
            r.data.len() - r.pos
        )));
    }

    Ok(CompiledPackage {
        opt_level,
        entry,
        modules,
    })
}

/// Return true if `bytes` begins with the `MORABC02` AOT bytecode magic header.
pub(crate) fn is_bytecode_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= BYTECODE_MAGIC.len() && &bytes[..BYTECODE_MAGIC.len()] == BYTECODE_MAGIC
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mir::control::{OptLevel, optimize_program};
    use crate::parser_v3::ParserV3;
    use crate::runtime::CoreRuntime;
    use crate::runtime::vm::execute_program;
    use crate::value::Value;

    #[test]
    fn roundtrip_complex_control_program_preserves_semantics() {
        let src = r#"
struct Pair
  a: int
  b: int = 10
end

enum Choice
  Left(int)
  Right(string)
end

trait Scorable
  task score(self): int
end

impl Scorable for Pair
  task score(self): int
    return self.a * 2 + self.b
  end
end

effect Ask

task run_all(x: int = 3): int
  let p = Pair(a=x)
  let c = Choice::Left(p.score())
  let base = match c with
    Choice::Left(n) -> n
    Choice::Right(_) -> 0
  end
  let eff_val = handle Ask {
    let v = perform Ask(base)
    return v + 1
  } {
    resume(__arg0 + 5)
  }
  let ch = channel(2)
  ch.send(eff_val)
  let got = select
    case v = ch.recv() -> v
    default -> -1
  end
  let t = Tensor::full([2, 3], 2.0)
  let sl = t[0..1, 1..3]
  let n = no_grad
    return int(sl.sum())
  end
  return got + n
end

run_all(7)
"#;
        let (mut prog, _w) = ParserV3::compile(src).expect("compile");
        optimize_program(&mut prog, OptLevel::Aggressive);
        let pkg = CompiledPackage {
            opt_level: 2,
            entry: prog,
            modules: BTreeMap::new(),
        };
        let encoded = encode_package(&pkg).expect("encode MORABC02");
        assert!(is_bytecode_bytes(&encoded));
        // Decoded program must verify and execute identically without re-parsing!
        let decoded = decode_package(&encoded).expect("decode MORABC02");
        assert_eq!(decoded.opt_level, 2);

        let mut core = CoreRuntime::default();
        let (val, _) = execute_program(Arc::new(decoded.entry), &mut core).expect("execute");
        // Pair(7, 10).score() = 24 -> Ask(+5) = 29 -> +1 = 30 -> sl has 2 elems of 2.0 => sum 4 -> 34
        assert_eq!(val, Value::Int(34));
    }

    #[test]
    fn corrupt_bytecode_is_rejected_cleanly() {
        let (prog, _) = ParserV3::compile("let x = 1 + 2\nx\n").expect("compile");
        let pkg = CompiledPackage {
            opt_level: 1,
            entry: prog,
            modules: BTreeMap::new(),
        };
        let encoded = encode_package(&pkg).expect("encode");

        // 1. Bit flip caught by FNV-1a checksum
        let mut flipped = encoded.clone();
        flipped[12] ^= 0xff;
        assert!(
            decode_package(&flipped)
                .expect_err("bitflip must fail")
                .to_string()
                .contains("checksum mismatch")
        );

        // 2. Bad magic caught immediately
        let mut bad_magic = encoded.clone();
        bad_magic[0] = b'X';
        assert!(
            decode_package(&bad_magic)
                .expect_err("bad magic must fail")
                .to_string()
                .contains("invalid magic")
        );

        // 3. Truncated buffer rejected cleanly
        assert!(decode_package(&encoded[..10]).is_err());
    }
}
