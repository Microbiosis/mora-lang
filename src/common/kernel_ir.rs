//! Typed GPU kernel intermediate representation (`KernelProgram`) shared across
//! `parser_v3`, `typeck`, `mir::control` (`MORABC02` codec), `value`, and `runtime::gpu`.
//!
//! Zero external dependencies; compiles directly to CUDA PTX ISA 7.0, Vulkan SPIR-V 1.0,
//! Metal MSL, and parallel host CPU execution.

use crate::common::DType;

/// Parameter kind in a source-level `kernel name(params...) ... end` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelParamType {
    F32,
    F64,
    I32,
    BufF32,
    BufF64,
}

impl KernelParamType {
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::I32 => "i32",
            Self::BufF32 => "buf<f32>",
            Self::BufF64 => "buf<f64>",
        }
    }

    pub fn is_buffer(self) -> bool {
        matches!(self, Self::BufF32 | Self::BufF64)
    }

    pub fn buffer_dtype(self) -> Option<DType> {
        match self {
            Self::BufF32 => Some(DType::F32),
            Self::BufF64 => Some(DType::F64),
            _ => None,
        }
    }

    pub fn scalar_type(self) -> Option<KernelScalarType> {
        match self {
            Self::F32 => Some(KernelScalarType::F32),
            Self::F64 => Some(KernelScalarType::F64),
            Self::I32 => Some(KernelScalarType::I32),
            Self::BufF32 | Self::BufF64 => None,
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::F32 => 1,
            Self::F64 => 2,
            Self::I32 => 3,
            Self::BufF32 => 4,
            Self::BufF64 => 5,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            1 => Ok(Self::F32),
            2 => Ok(Self::F64),
            3 => Ok(Self::I32),
            4 => Ok(Self::BufF32),
            5 => Ok(Self::BufF64),
            other => Err(format!("invalid KernelParamType tag {other}")),
        }
    }
}

/// Scalar expression value type inside a typed GPU kernel body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelScalarType {
    I32,
    F32,
    F64,
    Bool,
}

impl KernelScalarType {
    pub fn canonical_name(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    fn tag(self) -> u8 {
        match self {
            Self::I32 => 1,
            Self::F32 => 2,
            Self::F64 => 3,
            Self::Bool => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            1 => Ok(Self::I32),
            2 => Ok(Self::F32),
            3 => Ok(Self::F64),
            4 => Ok(Self::Bool),
            other => Err(format!("invalid KernelScalarType tag {other}")),
        }
    }
}

/// One typed parameter of a `kernel` declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KernelParam {
    pub name: String,
    pub ty: KernelParamType,
}

/// Unary operations and scalar math/cast intrinsics inside a GPU kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelUnaryOp {
    Neg,
    Not,
    Exp,
    Sqrt,
    Tanh,
    Abs,
    ToF32,
    ToF64,
    ToI32,
}

impl KernelUnaryOp {
    fn tag(self) -> u8 {
        match self {
            Self::Neg => 1,
            Self::Not => 2,
            Self::Exp => 3,
            Self::Sqrt => 4,
            Self::Tanh => 5,
            Self::Abs => 6,
            Self::ToF32 => 7,
            Self::ToF64 => 8,
            Self::ToI32 => 9,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            1 => Ok(Self::Neg),
            2 => Ok(Self::Not),
            3 => Ok(Self::Exp),
            4 => Ok(Self::Sqrt),
            5 => Ok(Self::Tanh),
            6 => Ok(Self::Abs),
            7 => Ok(Self::ToF32),
            8 => Ok(Self::ToF64),
            9 => Ok(Self::ToI32),
            other => Err(format!("invalid KernelUnaryOp tag {other}")),
        }
    }
}

/// Binary arithmetic, comparison, logical, and min/max operations inside a GPU kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Min,
    Max,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl KernelBinOp {
    fn tag(self) -> u8 {
        match self {
            Self::Add => 1,
            Self::Sub => 2,
            Self::Mul => 3,
            Self::Div => 4,
            Self::Rem => 5,
            Self::Min => 6,
            Self::Max => 7,
            Self::Lt => 8,
            Self::Le => 9,
            Self::Gt => 10,
            Self::Ge => 11,
            Self::Eq => 12,
            Self::Ne => 13,
            Self::And => 14,
            Self::Or => 15,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, String> {
        match tag {
            1 => Ok(Self::Add),
            2 => Ok(Self::Sub),
            3 => Ok(Self::Mul),
            4 => Ok(Self::Div),
            5 => Ok(Self::Rem),
            6 => Ok(Self::Min),
            7 => Ok(Self::Max),
            8 => Ok(Self::Lt),
            9 => Ok(Self::Le),
            10 => Ok(Self::Gt),
            11 => Ok(Self::Ge),
            12 => Ok(Self::Eq),
            13 => Ok(Self::Ne),
            14 => Ok(Self::And),
            15 => Ok(Self::Or),
            other => Err(format!("invalid KernelBinOp tag {other}")),
        }
    }
}

/// Typed scalar expression inside a GPU kernel body.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelExpr {
    ConstI32(i32),
    ConstF32(f32),
    ConstF64(f64),
    ConstBool(bool),
    Param(usize, KernelScalarType),
    Var(String, KernelScalarType),
    GlobalId,
    BlockIdx,
    ThreadIdx,
    BlockDim,
    GridDim,
    Load {
        buf_param: usize,
        index: Box<KernelExpr>,
        elem_ty: KernelScalarType,
    },
    Unary {
        op: KernelUnaryOp,
        arg: Box<KernelExpr>,
        ty: KernelScalarType,
    },
    Binary {
        op: KernelBinOp,
        lhs: Box<KernelExpr>,
        rhs: Box<KernelExpr>,
        ty: KernelScalarType,
    },
}

impl KernelExpr {
    pub fn ty(&self) -> KernelScalarType {
        match self {
            Self::ConstI32(_)
            | Self::GlobalId
            | Self::BlockIdx
            | Self::ThreadIdx
            | Self::BlockDim
            | Self::GridDim => KernelScalarType::I32,
            Self::ConstF32(_) => KernelScalarType::F32,
            Self::ConstF64(_) => KernelScalarType::F64,
            Self::ConstBool(_) => KernelScalarType::Bool,
            Self::Param(_, ty) | Self::Var(_, ty) => *ty,
            Self::Load { elem_ty, .. } => *elem_ty,
            Self::Unary { ty, .. } | Self::Binary { ty, .. } => *ty,
        }
    }
}

/// Structured statement inside a typed GPU kernel body.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelStmt {
    Let {
        name: String,
        ty: KernelScalarType,
        expr: KernelExpr,
    },
    Assign {
        name: String,
        expr: KernelExpr,
    },
    Store {
        buf_param: usize,
        index: KernelExpr,
        value: KernelExpr,
    },
    If {
        cond: KernelExpr,
        then_body: Vec<KernelStmt>,
        else_body: Vec<KernelStmt>,
    },
    While {
        cond: KernelExpr,
        body: Vec<KernelStmt>,
    },
}

/// Complete typed GPU kernel IR ready for PTX / SPIR-V / MSL / CPU code generation.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelProgram {
    pub name: String,
    pub params: Vec<KernelParam>,
    pub body: Vec<KernelStmt>,
}

impl KernelProgram {
    /// Deterministic 64-bit FNV-1a content hash for GPU pipeline/module cache indexing.
    pub fn content_hash(&self) -> u64 {
        let bytes = self.encode_bytes();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Returns true if any parameter, local variable, or literal in this kernel uses `f64`.
    pub fn uses_f64(&self) -> bool {
        if self
            .params
            .iter()
            .any(|p| matches!(p.ty, KernelParamType::F64 | KernelParamType::BufF64))
        {
            return true;
        }
        fn expr_uses_f64(e: &KernelExpr) -> bool {
            if e.ty() == KernelScalarType::F64 {
                return true;
            }
            match e {
                KernelExpr::Load { index, .. } => expr_uses_f64(index),
                KernelExpr::Unary { arg, .. } => expr_uses_f64(arg),
                KernelExpr::Binary { lhs, rhs, .. } => expr_uses_f64(lhs) || expr_uses_f64(rhs),
                _ => false,
            }
        }
        fn stmts_use_f64(stmts: &[KernelStmt]) -> bool {
            stmts.iter().any(|s| match s {
                KernelStmt::Let { ty, expr, .. } => {
                    *ty == KernelScalarType::F64 || expr_uses_f64(expr)
                }
                KernelStmt::Assign { expr, .. } => expr_uses_f64(expr),
                KernelStmt::Store { index, value, .. } => {
                    expr_uses_f64(index) || expr_uses_f64(value)
                }
                KernelStmt::If {
                    cond,
                    then_body,
                    else_body,
                } => expr_uses_f64(cond) || stmts_use_f64(then_body) || stmts_use_f64(else_body),
                KernelStmt::While { cond, body } => expr_uses_f64(cond) || stmts_use_f64(body),
            })
        }
        stmts_use_f64(&self.body)
    }

    /// Encode this `KernelProgram` into a self-contained binary byte buffer (`MORABC02` codec).
    pub fn encode_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        write_str(&mut buf, &self.name);
        write_u32(&mut buf, self.params.len() as u32);
        for p in &self.params {
            write_str(&mut buf, &p.name);
            buf.push(p.ty.tag());
        }
        encode_stmts(&mut buf, &self.body);
        buf
    }

    /// Decode a `KernelProgram` from bytes produced by [`Self::encode_bytes`].
    pub fn decode_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut cursor = 0usize;
        let name = read_str(bytes, &mut cursor)?;
        let n_params = read_u32(bytes, &mut cursor)? as usize;
        let mut params = Vec::with_capacity(n_params);
        for _ in 0..n_params {
            let pname = read_str(bytes, &mut cursor)?;
            let pty = KernelParamType::from_tag(read_u8(bytes, &mut cursor)?)?;
            params.push(KernelParam {
                name: pname,
                ty: pty,
            });
        }
        let body = decode_stmts(bytes, &mut cursor)?;
        if cursor != bytes.len() {
            return Err(format!(
                "trailing {} bytes in KernelProgram binary payload",
                bytes.len() - cursor
            ));
        }
        Ok(Self { name, params, body })
    }
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_u32(buf, s.len() as u32);
    buf.extend_from_slice(s.as_bytes());
}

fn read_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, String> {
    let b = *bytes
        .get(*cursor)
        .ok_or_else(|| "unexpected EOF in KernelProgram".to_string())?;
    *cursor += 1;
    Ok(b)
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, String> {
    if *cursor + 4 > bytes.len() {
        return Err("unexpected EOF reading u32 in KernelProgram".to_string());
    }
    let arr: [u8; 4] = bytes[*cursor..*cursor + 4]
        .try_into()
        .expect("4-byte slice");
    *cursor += 4;
    Ok(u32::from_le_bytes(arr))
}

fn read_str(bytes: &[u8], cursor: &mut usize) -> Result<String, String> {
    let len = read_u32(bytes, cursor)? as usize;
    if *cursor + len > bytes.len() {
        return Err("unexpected EOF reading string in KernelProgram".to_string());
    }
    let s = std::str::from_utf8(&bytes[*cursor..*cursor + len])
        .map_err(|e| format!("invalid UTF-8 in KernelProgram: {e}"))?
        .to_string();
    *cursor += len;
    Ok(s)
}

fn encode_expr(buf: &mut Vec<u8>, expr: &KernelExpr) {
    match expr {
        KernelExpr::ConstI32(v) => {
            buf.push(1);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        KernelExpr::ConstF32(v) => {
            buf.push(2);
            buf.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        KernelExpr::ConstF64(v) => {
            buf.push(3);
            buf.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        KernelExpr::ConstBool(v) => {
            buf.push(4);
            buf.push(u8::from(*v));
        }
        KernelExpr::Param(idx, ty) => {
            buf.push(5);
            write_u32(buf, *idx as u32);
            buf.push(ty.tag());
        }
        KernelExpr::Var(name, ty) => {
            buf.push(6);
            write_str(buf, name);
            buf.push(ty.tag());
        }
        KernelExpr::GlobalId => buf.push(7),
        KernelExpr::BlockIdx => buf.push(8),
        KernelExpr::ThreadIdx => buf.push(9),
        KernelExpr::BlockDim => buf.push(10),
        KernelExpr::GridDim => buf.push(11),
        KernelExpr::Load {
            buf_param,
            index,
            elem_ty,
        } => {
            buf.push(12);
            write_u32(buf, *buf_param as u32);
            buf.push(elem_ty.tag());
            encode_expr(buf, index);
        }
        KernelExpr::Unary { op, arg, ty } => {
            buf.push(13);
            buf.push(op.tag());
            buf.push(ty.tag());
            encode_expr(buf, arg);
        }
        KernelExpr::Binary { op, lhs, rhs, ty } => {
            buf.push(14);
            buf.push(op.tag());
            buf.push(ty.tag());
            encode_expr(buf, lhs);
            encode_expr(buf, rhs);
        }
    }
}

fn decode_expr(bytes: &[u8], cursor: &mut usize) -> Result<KernelExpr, String> {
    let tag = read_u8(bytes, cursor)?;
    match tag {
        1 => {
            let bits = read_u32(bytes, cursor)?;
            Ok(KernelExpr::ConstI32(bits as i32))
        }
        2 => {
            let bits = read_u32(bytes, cursor)?;
            Ok(KernelExpr::ConstF32(f32::from_bits(bits)))
        }
        3 => {
            if *cursor + 8 > bytes.len() {
                return Err("unexpected EOF reading f64 in KernelProgram".to_string());
            }
            let arr: [u8; 8] = bytes[*cursor..*cursor + 8]
                .try_into()
                .expect("8-byte slice");
            *cursor += 8;
            Ok(KernelExpr::ConstF64(f64::from_bits(u64::from_le_bytes(
                arr,
            ))))
        }
        4 => Ok(KernelExpr::ConstBool(read_u8(bytes, cursor)? != 0)),
        5 => {
            let idx = read_u32(bytes, cursor)? as usize;
            let ty = KernelScalarType::from_tag(read_u8(bytes, cursor)?)?;
            Ok(KernelExpr::Param(idx, ty))
        }
        6 => {
            let name = read_str(bytes, cursor)?;
            let ty = KernelScalarType::from_tag(read_u8(bytes, cursor)?)?;
            Ok(KernelExpr::Var(name, ty))
        }
        7 => Ok(KernelExpr::GlobalId),
        8 => Ok(KernelExpr::BlockIdx),
        9 => Ok(KernelExpr::ThreadIdx),
        10 => Ok(KernelExpr::BlockDim),
        11 => Ok(KernelExpr::GridDim),
        12 => {
            let buf_param = read_u32(bytes, cursor)? as usize;
            let elem_ty = KernelScalarType::from_tag(read_u8(bytes, cursor)?)?;
            let index = Box::new(decode_expr(bytes, cursor)?);
            Ok(KernelExpr::Load {
                buf_param,
                index,
                elem_ty,
            })
        }
        13 => {
            let op = KernelUnaryOp::from_tag(read_u8(bytes, cursor)?)?;
            let ty = KernelScalarType::from_tag(read_u8(bytes, cursor)?)?;
            let arg = Box::new(decode_expr(bytes, cursor)?);
            Ok(KernelExpr::Unary { op, arg, ty })
        }
        14 => {
            let op = KernelBinOp::from_tag(read_u8(bytes, cursor)?)?;
            let ty = KernelScalarType::from_tag(read_u8(bytes, cursor)?)?;
            let lhs = Box::new(decode_expr(bytes, cursor)?);
            let rhs = Box::new(decode_expr(bytes, cursor)?);
            Ok(KernelExpr::Binary { op, lhs, rhs, ty })
        }
        other => Err(format!("invalid KernelExpr tag {other}")),
    }
}

fn encode_stmts(buf: &mut Vec<u8>, stmts: &[KernelStmt]) {
    write_u32(buf, stmts.len() as u32);
    for s in stmts {
        match s {
            KernelStmt::Let { name, ty, expr } => {
                buf.push(1);
                write_str(buf, name);
                buf.push(ty.tag());
                encode_expr(buf, expr);
            }
            KernelStmt::Assign { name, expr } => {
                buf.push(2);
                write_str(buf, name);
                encode_expr(buf, expr);
            }
            KernelStmt::Store {
                buf_param,
                index,
                value,
            } => {
                buf.push(3);
                write_u32(buf, *buf_param as u32);
                encode_expr(buf, index);
                encode_expr(buf, value);
            }
            KernelStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                buf.push(4);
                encode_expr(buf, cond);
                encode_stmts(buf, then_body);
                encode_stmts(buf, else_body);
            }
            KernelStmt::While { cond, body } => {
                buf.push(5);
                encode_expr(buf, cond);
                encode_stmts(buf, body);
            }
        }
    }
}

fn decode_stmts(bytes: &[u8], cursor: &mut usize) -> Result<Vec<KernelStmt>, String> {
    let n = read_u32(bytes, cursor)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = read_u8(bytes, cursor)?;
        let stmt = match tag {
            1 => {
                let name = read_str(bytes, cursor)?;
                let ty = KernelScalarType::from_tag(read_u8(bytes, cursor)?)?;
                let expr = decode_expr(bytes, cursor)?;
                KernelStmt::Let { name, ty, expr }
            }
            2 => {
                let name = read_str(bytes, cursor)?;
                let expr = decode_expr(bytes, cursor)?;
                KernelStmt::Assign { name, expr }
            }
            3 => {
                let buf_param = read_u32(bytes, cursor)? as usize;
                let index = decode_expr(bytes, cursor)?;
                let value = decode_expr(bytes, cursor)?;
                KernelStmt::Store {
                    buf_param,
                    index,
                    value,
                }
            }
            4 => {
                let cond = decode_expr(bytes, cursor)?;
                let then_body = decode_stmts(bytes, cursor)?;
                let else_body = decode_stmts(bytes, cursor)?;
                KernelStmt::If {
                    cond,
                    then_body,
                    else_body,
                }
            }
            5 => {
                let cond = decode_expr(bytes, cursor)?;
                let body = decode_stmts(bytes, cursor)?;
                KernelStmt::While { cond, body }
            }
            other => return Err(format!("invalid KernelStmt tag {other}")),
        };
        out.push(stmt);
    }
    Ok(out)
}
