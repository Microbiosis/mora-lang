//! v0.110.0: `ParserV3` 语法辅助子系统（模式 / 类型注解解析）。
//!
//! 这些方法直接产出 `MirWitness`（witness-native），零 AST 中间层：
//! - `parse_pattern` / `try_parse_literal_pattern` — 模式匹配
//! - `parse_type_annotation` / `parse_single_type_annotation` — 类型注解（含 union / 泛型）

use super::*;

impl ParserV3 {
    // ════════════════════════════════════════════════════════════════
    // Pattern 解析（witness-native）
    // ════════════════════════════════════════════════════════════════

    pub(super) fn parse_pattern(&mut self) -> Option<crate::mir::witness::WitnessPattern> {
        if let Some(tok) = self.peek()
            && let TokenType::Identifier(ref name) = tok.token_type
            && name == "_"
        {
            self.advance();
            return Some(crate::mir::witness::WitnessPattern::Wildcard);
        }

        if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
            self.advance();
            let name_opt = if let Some(tok) = self.peek() {
                if let TokenType::Identifier(ref name) = tok.token_type {
                    Some(name.clone())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(name) = name_opt {
                self.advance();
                return Some(crate::mir::witness::WitnessPattern::Variable(name));
            }
            return Some(crate::mir::witness::WitnessPattern::Wildcard);
        }

        if let Some(tok) = self.peek()
            && let TokenType::Identifier(ref name) = tok.token_type
        {
            if let Some(next) = self.tokens.get(self.current + 1)
                && next.token_type == TokenType::Colon
            {
                let name = name.clone();
                self.advance();
                self.advance();
                if let Some(inner) = self.parse_pattern() {
                    return Some(crate::mir::witness::WitnessPattern::TypeAscription {
                        name,
                        pattern: Box::new(inner),
                    });
                }
                return None;
            }
            // Stage 3C: Enum variant pattern `Enum::Variant` or `Enum::Variant(p1, p2)`
            if let Some(next) = self.tokens.get(self.current + 1)
                && next.token_type == TokenType::ColonColon
            {
                let enum_name = name.clone();
                self.advance(); // enum_name
                self.advance(); // '::'
                let variant = self.consume_identifier("Expected variant name after '::'")?;
                let mut fields = Vec::new();
                if self.match_token_exact(TokenType::LParen) {
                    while !self.check(&TokenType::RParen) && !self.is_at_end() {
                        fields.push(self.parse_pattern()?);
                        if !self.match_token_exact(TokenType::Comma) {
                            break;
                        }
                    }
                    self.consume(TokenType::RParen, "Expected ')' after variant pattern")?;
                }
                return Some(crate::mir::witness::WitnessPattern::Variant {
                    enum_name: Some(enum_name),
                    variant,
                    fields,
                });
            }
            // Stage 3C: Nominal struct pattern `Point { x: a, y, .. }`
            if let Some(next) = self.tokens.get(self.current + 1)
                && next.token_type == TokenType::LBrace
                && (self.declared_type_names.contains(name.as_str())
                    || name.chars().next().is_some_and(|c| c.is_uppercase()))
            {
                let struct_name = name.clone();
                self.advance(); // struct_name
                self.advance(); // '{'
                let mut fields = Vec::new();
                let mut rest = false;
                loop {
                    if self.match_token_exact(TokenType::RBrace) {
                        break;
                    }
                    if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
                        self.advance();
                        rest = true;
                        self.consume(TokenType::RBrace, "Expected '}' after struct rest pattern")?;
                        break;
                    }
                    let key = self.consume_identifier("Expected struct field name")?;
                    let pat = if self.match_token_exact(TokenType::Colon) {
                        self.parse_pattern()?
                    } else {
                        crate::mir::witness::WitnessPattern::Variable(key.clone())
                    };
                    fields.push((key, pat));
                    if !self.match_token_exact(TokenType::Comma) {
                        self.consume(TokenType::RBrace, "Expected '}' after struct pattern")?;
                        break;
                    }
                }
                return Some(crate::mir::witness::WitnessPattern::Struct {
                    name: struct_name,
                    fields,
                    rest,
                });
            }
            // Stage 3C: Unqualified payload variant pattern `Some(x)` (uppercase identifier + '(')
            if let Some(next) = self.tokens.get(self.current + 1)
                && next.token_type == TokenType::LParen
                && name.chars().next().is_some_and(|c| c.is_uppercase())
            {
                let variant = name.clone();
                self.advance(); // variant
                self.advance(); // '('
                let mut fields = Vec::new();
                while !self.check(&TokenType::RParen) && !self.is_at_end() {
                    fields.push(self.parse_pattern()?);
                    if !self.match_token_exact(TokenType::Comma) {
                        break;
                    }
                }
                self.consume(TokenType::RParen, "Expected ')' after variant pattern")?;
                return Some(crate::mir::witness::WitnessPattern::Variant {
                    enum_name: None,
                    variant,
                    fields,
                });
            }
            let name = self.consume_identifier("Expected pattern")?;
            return Some(crate::mir::witness::WitnessPattern::Variable(name));
        }

        if let Some(literal) = self.try_parse_literal_pattern() {
            return Some(crate::mir::witness::WitnessPattern::Literal(literal));
        }

        if self.match_token_exact(TokenType::LBracket) {
            let mut elements = Vec::new();
            let mut rest = None;
            loop {
                if self.match_token_exact(TokenType::RBracket) {
                    break;
                }
                if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
                    self.advance();
                    let name_opt = if let Some(tok) = self.peek() {
                        if let TokenType::Identifier(ref name) = tok.token_type {
                            Some(name.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(name) = name_opt {
                        self.advance();
                        rest = Some(Box::new(crate::mir::witness::WitnessPattern::Variable(
                            name,
                        )));
                    } else {
                        rest = Some(Box::new(crate::mir::witness::WitnessPattern::Wildcard));
                    }
                    self.consume(TokenType::RBracket, "Expected ']' after rest pattern")?;
                    break;
                }
                // v0.104.4: `?` 取代 `if let … else return None`
                //（clippy 1.98 的 question_mark；本函数返回 `Option`，
                // Option 的 `?` 在 `None` 时提前返回 `None`）。
                let elem = self.parse_pattern()?;
                elements.push(elem);
                if !self.match_token_exact(TokenType::Comma) {
                    self.consume(TokenType::RBracket, "Expected ']' after list pattern")?;
                    break;
                }
            }
            return Some(crate::mir::witness::WitnessPattern::ListVec { elements, rest });
        }

        if self.match_token_exact(TokenType::LBrace) {
            let mut required = Vec::new();
            let mut rest = false;
            loop {
                if self.match_token_exact(TokenType::RBrace) {
                    break;
                }
                let key = self.consume_identifier("Expected dict key")?;
                self.consume(TokenType::Colon, "Expected ':' after dict key")?;
                // v0.104.4: `?` 取代 `if let … else return None`
                //（clippy 1.98 的 question_mark）。
                let value_pat = self.parse_pattern()?;
                required.push((key, value_pat));
                if !self.match_token_exact(TokenType::Comma) {
                    self.consume(TokenType::RBrace, "Expected '}' after dict pattern")?;
                    break;
                }
                if self.check(&TokenType::DotDot) || self.check(&TokenType::DotDotDot) {
                    self.advance();
                    rest = true;
                    self.consume(TokenType::RBrace, "Expected '}' after dict rest")?;
                    break;
                }
            }
            return Some(crate::mir::witness::WitnessPattern::Dict { required, rest });
        }

        None
    }

    fn try_parse_literal_pattern(&mut self) -> Option<crate::common::Literal> {
        let token = self.peek()?.token_type.clone();

        match token {
            TokenType::True => {
                self.advance();
                Some(crate::common::Literal::Bool(true, self.span_of_current()))
            }
            TokenType::False => {
                self.advance();
                Some(crate::common::Literal::Bool(false, self.span_of_current()))
            }
            TokenType::Int(val) => {
                self.advance();
                Some(crate::common::Literal::Int(val, self.span_of_current()))
            }
            TokenType::TypedInt(val, dt) => {
                let span = self.span_of_current();
                if let Err(msg) = dt.check_int_range(val) {
                    eprintln!(
                        "Parse error: {} at line {}, column {}",
                        msg, span.line, span.column
                    );
                    return None;
                }
                self.advance();
                Some(crate::common::Literal::TypedInt(val, dt, span))
            }
            TokenType::Float(val) => {
                self.advance();
                Some(crate::common::Literal::Float(val, self.span_of_current()))
            }
            TokenType::TypedFloat(val, dt) => {
                let span = self.span_of_current();
                self.advance();
                Some(crate::common::Literal::TypedFloat(val, dt, span))
            }
            TokenType::String(s) => {
                self.advance();
                Some(crate::common::Literal::String(s, self.span_of_current()))
            }
            TokenType::Nil => {
                self.advance();
                Some(crate::common::Literal::Nil(self.span_of_current()))
            }
            _ => None,
        }
    }

    // ════════════════════════════════════════════════════════════════
    // 类型注解解析（含 union / 泛型）
    // ════════════════════════════════════════════════════════════════

    /// v0.104.6：解析泛型声明子句 `f<T, U>`（紧跟 `task` / `fn` 名字、形参之前）。
    /// 无 `<` ⇒ 非泛型：返回空 `Vec`。
    /// 有 `<` ⇒ 解析逗号分隔的类型参数名，存入 [`ParserV3::generic_params`]，
    /// 供形参/返回标注把裸名认作 `Type::TypeVar(TypeVarId::Rigid(name))`，
    /// 并返回完整参数名列表。重复名称报错。
    pub(super) fn parse_generic_clause(&mut self) -> Option<Vec<String>> {
        self.parse_generic_clause_with_consts()
            .map(|(names, _)| names)
    }

    /// P4-1A: Parse generic parameter clause `<T, const IN: int, const OUT: usize>`.
    /// Returns `(all_param_names_in_order, const_param_names)`.
    pub(super) fn parse_generic_clause_with_consts(
        &mut self,
    ) -> Option<(Vec<String>, Vec<String>)> {
        if !self.match_token(&[TokenType::Less]) {
            return Some((Vec::new(), Vec::new()));
        }
        let mut names: Vec<String> = Vec::new();
        let mut consts: Vec<String> = Vec::new();
        loop {
            let is_const = matches!(
                self.peek().map(|t| &t.token_type),
                Some(TokenType::Identifier(s)) if s == "const"
            ) && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.token_type),
                Some(TokenType::Identifier(_))
            );
            if is_const {
                self.advance(); // consume 'const'
                let cname = self.consume_identifier("Expected const generic parameter name")?;
                self.consume(
                    TokenType::Colon,
                    "Expected ':' and integer type (int/usize) after const generic parameter name",
                )?;
                let ty_name = self.consume_identifier("Expected const generic type (int/usize)")?;
                let lower = ty_name.to_lowercase();
                if !matches!(lower.as_str(), "int" | "usize" | "i64" | "i32") {
                    let msg = format!(
                        "Parse error: const generic parameter '{}' must have integer type (int/usize), got '{}' at line {}",
                        cname,
                        ty_name,
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                if names.contains(&cname) {
                    let msg = format!(
                        "Parse error: duplicate generic parameter '{}' at line {}",
                        cname,
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                names.push(cname.clone());
                consts.push(cname);
            } else {
                let n = self.consume_identifier("Expected type parameter name")?;
                if names.contains(&n) {
                    let msg = format!(
                        "Parse error: duplicate generic type parameter '{}' at line {}",
                        n,
                        self.current_line()
                    );
                    eprintln!("{msg}");
                    self.diag = Some(msg);
                    return None;
                }
                names.push(n);
            }
            if self.match_token(&[TokenType::Comma]) {
                continue;
            }
            break;
        }
        if !self.match_token(&[TokenType::Greater]) {
            eprintln!(
                "Parse error: expected '>' after type parameters at line {}",
                self.current_line()
            );
            return None;
        }
        self.generic_params = names.clone();
        Some((names, consts))
    }

    pub(super) fn parse_type_annotation(&mut self) -> Option<crate::typeck::Type> {
        use crate::typeck::Type;
        // v0.85: 先解析一个基础类型，再收集 `|` 分隔的 union 成员
        let ty = self.parse_single_type_annotation()?;

        // v0.85: 收集 union 成员 (string | number | bool)
        let mut members: Vec<Type> = vec![ty];
        while self.match_token_exact(TokenType::Or) {
            // 跳过 `|` 后的可选空格/换行
            let next = self.peek();
            if next.is_none() {
                eprintln!(
                    "Parse error: expected type after '|' at line {}",
                    self.current_line()
                );
                return None;
            }
            let member = self.parse_single_type_annotation()?;
            members.push(member);
        }

        if members.len() == 1 {
            Some(
                members
                    .into_iter()
                    .next()
                    .expect("members.len() == 1 verified"),
            )
        } else {
            Some(Type::Union(members))
        }
    }

    /// 解析单个类型注解（不含 `|` 联合），内部由 `parse_type_annotation` 调用。
    fn parse_single_type_annotation(&mut self) -> Option<crate::typeck::Type> {
        use crate::typeck::Type;
        let tok = self.peek().cloned()?;
        match &tok.token_type {
            TokenType::Dyn => {
                self.advance();
                let name = self.consume_identifier("Expected trait name after 'dyn'")?;
                let generics = if self.match_token(&[TokenType::Less]) {
                    let mut g: Vec<Type> = Vec::new();
                    loop {
                        g.push(self.parse_type_annotation()?);
                        if !self.match_token(&[TokenType::Comma]) {
                            break;
                        }
                    }
                    if !self.match_token(&[TokenType::Greater]) {
                        eprintln!(
                            "Parse error: expected '>' in dyn trait generics at line {}",
                            self.current_line()
                        );
                        return None;
                    }
                    g
                } else {
                    Vec::new()
                };
                Some(Type::TraitObject {
                    trait_name: name,
                    generics,
                })
            }
            // v0.104.6 D64：`nil` 是**关键字 token**（lexer.rs:13 `TokenType::Nil`），
            // 走不到下面的 `Identifier` 分支 —— 白名单里的 `"nil" => Type::Nil`
            // 因此是一条**不可达的死 arm**，`let v: nil = nil` 直接报
            // "expected type annotation"。spec §3.1 :115 把 `nil` 列为正式类型，
            // 且 typeck 三处（`subtype_of` mod.rs:762、`compatible_with` mod.rs:532、
            // `unify` unify.rs:307）都已成对支持 `Nil`，补上这一条即可跑通。
            TokenType::Nil => {
                self.advance();
                Some(Type::Nil)
            }
            TokenType::Self_ => {
                self.advance();
                Some(Type::Trait {
                    name: "Self".to_string(),
                    generics: Vec::new(),
                })
            }
            // P4-1A: Compile-time integer const-generic argument in type annotations (`Linear<4, 3>`).
            TokenType::Int(n) => {
                let val = *n;
                self.advance();
                Some(Type::ConstInt(val))
            }
            TokenType::TypedInt(n, _) => {
                let val = *n;
                self.advance();
                Some(Type::ConstInt(val))
            }
            TokenType::Identifier(name) => {
                let orig_name = name.clone();
                let lower = name.to_lowercase();
                if matches!(
                    self.tokens.get(self.current + 1).map(|t| &t.token_type),
                    Some(TokenType::Less)
                ) {
                    if lower == "tensor" {
                        return self.parse_tensor_type_annotation();
                    }
                    self.advance();
                    self.advance();
                    let mut args: Vec<Type> = Vec::new();
                    loop {
                        args.push(self.parse_type_annotation()?);
                        if self.match_token(&[TokenType::Comma]) {
                            continue;
                        }
                        break;
                    }
                    if !self.match_token(&[TokenType::Greater]) {
                        eprintln!(
                            "Parse error: expected '>' after generic type arguments at line {}",
                            self.current_line()
                        );
                        return None;
                    }
                    return match lower.as_str() {
                        "list" => Some(Type::List(Box::new(
                            args.into_iter().next().unwrap_or(Type::Unknown),
                        ))),
                        "channel" => Some(Type::Channel(Box::new(
                            args.into_iter().next().unwrap_or(Type::Unknown),
                        ))),
                        "task_handle" | "taskhandle" => Some(Type::TaskHandle(Box::new(
                            args.into_iter().next().unwrap_or(Type::Unknown),
                        ))),
                        "dict" => {
                            let mut it = args.into_iter();
                            let k = it.next().unwrap_or(Type::Unknown);
                            let v = it.next().unwrap_or(Type::Unknown);
                            Some(Type::Dict(Box::new(k), Box::new(v)))
                        }
                        other => {
                            if self.declared_type_names.contains(orig_name.as_str()) {
                                return Some(Type::Concrete {
                                    name: orig_name,
                                    generics: args,
                                    traits: Vec::new(),
                                });
                            }
                            eprintln!(
                                "Parse error: unsupported generic type annotation '{}' at line {}",
                                other,
                                self.current_line()
                            );
                            None
                        }
                    };
                }
                if let Some(stored) = self
                    .generic_params
                    .iter()
                    .find(|p| p.to_lowercase() == lower)
                    .cloned()
                {
                    self.advance();
                    return Some(Type::TypeVar(crate::typeck::TypeVarId::Rigid(stored)));
                }
                if let Some(dt) = crate::common::DType::from_name(&lower) {
                    self.advance();
                    return Some(Type::from_dtype(dt));
                }
                let ty = match lower.as_str() {
                    "int" => Type::Int,
                    "number" => Type::Union(vec![Type::Int, Type::Float]),
                    "float" => Type::Float,
                    "string" => Type::String,
                    "char" => Type::Char,
                    "bool" => Type::Bool,
                    "nil" => Type::Nil,
                    "context" => Type::Context,
                    "tcp_listener" | "tcplistener" => Type::TcpListener,
                    "tcp_stream" | "tcpstream" => Type::TcpStream,
                    "tls_listener" | "tlslistener" => Type::TlsListener,
                    "tls_stream" | "tlsstream" => Type::TlsStream,
                    "compute_dag" | "computedag" => Type::ComputeDag,
                    "kv_allocator" | "kvallocator" => Type::KVAllocator,
                    "memory_pool" | "memorypool" => Type::MemoryPool,
                    "process_group" | "processgroup" => Type::ProcessGroup,
                    "trace_profiler" | "traceprofiler" => Type::TraceProfiler,
                    "gpu_kernel" | "gpukernel" => Type::GpuKernel(None),
                    "channel" => Type::Channel(Box::new(Type::Any)),
                    "task_handle" | "taskhandle" => Type::TaskHandle(Box::new(Type::Any)),
                    "tensor" => Type::any_tensor(),
                    "any" => Type::Any,
                    "unknown" => Type::Unknown,
                    other => {
                        if let Some(stored) = self
                            .generic_params
                            .iter()
                            .find(|p| p.to_lowercase() == other)
                            .cloned()
                        {
                            self.advance();
                            return Some(Type::TypeVar(crate::typeck::TypeVarId::Rigid(stored)));
                        }
                        if self.declared_type_names.contains(name.as_str()) {
                            self.advance();
                            return Some(Type::Trait {
                                name: name.clone(),
                                generics: Vec::new(),
                            });
                        }
                        eprintln!(
                            "Parse error: unsupported type annotation '{}' at line {}",
                            other,
                            self.current_line()
                        );
                        return None;
                    }
                };
                self.advance();
                Some(ty)
            }
            _ => {
                eprintln!(
                    "Parse error: expected type annotation at line {}",
                    self.current_line()
                );
                None
            }
        }
    }

    /// Parse `Tensor<DType, Shape?, Device?>` / `tensor<DType, Shape?, Device?>`.
    fn parse_tensor_type_annotation(&mut self) -> Option<crate::typeck::Type> {
        use crate::typeck::{DeviceType, ShapeType, Type};
        self.advance(); // consume 'Tensor' / 'tensor'
        self.advance(); // consume '<'
        let dtype = self.parse_type_annotation()?;
        let (shape, device) = if self.match_token(&[TokenType::Comma]) {
            let shape = self.parse_tensor_shape_annotation()?;
            let device = if self.match_token(&[TokenType::Comma]) {
                self.parse_tensor_device_annotation()?
            } else {
                DeviceType::Concrete(crate::common::Device::Cpu)
            };
            (shape, device)
        } else {
            (
                ShapeType::Unranked(None),
                DeviceType::Concrete(crate::common::Device::Cpu),
            )
        };
        if !self.match_token(&[TokenType::Greater]) {
            eprintln!(
                "Parse error: expected '>' after Tensor type arguments at line {}",
                self.current_line()
            );
            return None;
        }
        Some(Type::Tensor {
            dtype: Box::new(dtype),
            shape,
            device,
        })
    }

    fn parse_tensor_shape_annotation(&mut self) -> Option<crate::typeck::ShapeType> {
        use crate::typeck::{DimExpr, ShapeType, TypeVarId};
        if self.match_token_exact(TokenType::LBracket) {
            let mut dims = Vec::new();
            while !self.check(&TokenType::RBracket) && !self.is_at_end() {
                let tok = self.peek()?.token_type.clone();
                match tok {
                    TokenType::Int(n) if n >= 0 => {
                        self.advance();
                        dims.push(DimExpr::Known(n as usize));
                    }
                    TokenType::TypedInt(n, _) if n >= 0 => {
                        self.advance();
                        dims.push(DimExpr::Known(n as usize));
                    }
                    TokenType::Identifier(sym) => {
                        self.advance();
                        dims.push(DimExpr::Symbolic(sym));
                    }
                    _ => {
                        eprintln!(
                            "Parse error: expected dimension size or symbolic name in tensor shape at line {}",
                            self.current_line()
                        );
                        return None;
                    }
                }
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(TokenType::RBracket, "Expected ']' in tensor shape")?;
            Some(ShapeType::Ranked(dims))
        } else if let Some(TokenType::Identifier(name)) = self.peek().map(|t| t.token_type.clone())
        {
            self.advance();
            if name == "_" {
                Some(ShapeType::Unranked(None))
            } else {
                let stored = self
                    .generic_params
                    .iter()
                    .find(|p| p.eq_ignore_ascii_case(&name))
                    .cloned()
                    .unwrap_or(name);
                Some(ShapeType::Unranked(Some(TypeVarId::Rigid(stored))))
            }
        } else {
            eprintln!(
                "Parse error: expected '[dim, ...]' or shape parameter in Tensor<...> at line {}",
                self.current_line()
            );
            None
        }
    }

    fn parse_tensor_device_annotation(&mut self) -> Option<crate::typeck::DeviceType> {
        use crate::common::Device;
        use crate::typeck::{DeviceType, TypeVarId};
        let tok = self.peek()?.token_type.clone();
        match tok {
            TokenType::Identifier(name) => {
                self.advance();
                let lower = name.to_lowercase();
                match lower.as_str() {
                    "_" | "any" => Some(DeviceType::Any),
                    "cpu" => Some(DeviceType::Concrete(Device::Cpu)),
                    "cuda" => {
                        let idx = if self.match_token_exact(TokenType::Colon) {
                            match self.peek()?.token_type {
                                TokenType::Int(n) if (0..=u16::MAX as i64).contains(&n) => {
                                    self.advance();
                                    n as u16
                                }
                                _ => {
                                    eprintln!(
                                        "Parse error: expected non-negative device index after 'cuda:' at line {}",
                                        self.current_line()
                                    );
                                    return None;
                                }
                            }
                        } else {
                            0
                        };
                        Some(DeviceType::Concrete(Device::Cuda(idx)))
                    }
                    "metal" => {
                        let idx = if self.match_token_exact(TokenType::Colon) {
                            match self.peek()?.token_type {
                                TokenType::Int(n) if (0..=u16::MAX as i64).contains(&n) => {
                                    self.advance();
                                    n as u16
                                }
                                _ => {
                                    eprintln!(
                                        "Parse error: expected non-negative device index after 'metal:' at line {}",
                                        self.current_line()
                                    );
                                    return None;
                                }
                            }
                        } else {
                            0
                        };
                        Some(DeviceType::Concrete(Device::Metal(idx)))
                    }
                    "vulkan" => {
                        let idx = if self.match_token_exact(TokenType::Colon) {
                            match self.peek()?.token_type {
                                TokenType::Int(n) if (0..=u16::MAX as i64).contains(&n) => {
                                    self.advance();
                                    n as u16
                                }
                                _ => {
                                    eprintln!(
                                        "Parse error: expected non-negative device index after 'vulkan:' at line {}",
                                        self.current_line()
                                    );
                                    return None;
                                }
                            }
                        } else {
                            0
                        };
                        Some(DeviceType::Concrete(Device::Vulkan(idx)))
                    }
                    _ => {
                        let stored = self
                            .generic_params
                            .iter()
                            .find(|p| p.eq_ignore_ascii_case(&name))
                            .cloned()
                            .unwrap_or(name);
                        Some(DeviceType::Var(TypeVarId::Rigid(stored)))
                    }
                }
            }
            TokenType::String(s) => {
                self.advance();
                let dev = match Device::parse(&s) {
                    Ok(d) => d,
                    Err(msg) => {
                        eprintln!("Parse error: {} at line {}", msg, self.current_line());
                        return None;
                    }
                };
                Some(DeviceType::Concrete(dev))
            }
            _ => {
                eprintln!(
                    "Parse error: expected device ('cpu', 'cuda:0', 'metal:0', or type parameter) in Tensor<...> at line {}",
                    self.current_line()
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lexer::Lexer;
    use crate::parser_v3::ParserV3;
    use crate::typeck::Type;

    fn parser(src: &str) -> ParserV3 {
        let tokens = Lexer::new(src).scan_tokens();
        ParserV3::new(tokens, src)
    }

    use crate::mir::witness::WitnessPattern as Pat;

    /// `parse_pattern` 的四种基本形态。
    ///
    /// 模式是 `match` arm 与 `let` 解构的共同基础，而 `emit_match_arm_w`
    /// 首个动作就是调它 —— 它返回 `None` 时 arm 被判为畸形。
    #[test]
    fn parse_pattern_covers_wildcard_variable_and_literals() {
        assert!(matches!(parser("_").parse_pattern(), Some(Pat::Wildcard)));
        assert!(matches!(
            parser("x").parse_pattern(),
            Some(Pat::Variable(ref n)) if n == "x"
        ));
        assert!(matches!(
            parser("42").parse_pattern(),
            Some(Pat::Literal(_))
        ));
        assert!(matches!(
            parser("\"s\"").parse_pattern(),
            Some(Pat::Literal(_))
        ));
    }

    /// rest 模式 `..name` 绑定新变量，裸 `..` 不绑定。
    ///
    /// 两者的区别直接决定 list 模式是「收集剩余元素」还是「丢弃剩余元素」。
    #[test]
    fn rest_pattern_binds_or_discards() {
        assert!(
            matches!(parser("..rest").parse_pattern(), Some(Pat::Variable(ref n)) if n == "rest"),
            "`..name` 应绑定为具名变量"
        );
        assert!(
            matches!(parser("..").parse_pattern(), Some(Pat::Wildcard)),
            "裸 `..` 不绑定，应退化为通配"
        );
    }

    /// 列表模式的元素数与 rest 绑定。
    ///
    /// `[a, b]` 无 rest、`[a, ..rest]` 捕获剩余元素 —— 这个区别直接决定
    /// 匹配时是否收集尾部，配错会静默丢元素。
    #[test]
    fn list_patterns_carry_elements_and_optional_rest() {
        match parser("[a, b]").parse_pattern() {
            Some(Pat::ListVec {
                ref elements,
                ref rest,
            }) => {
                assert_eq!(elements.len(), 2);
                assert!(rest.is_none(), "`[a, b]` 不应有 rest 绑定");
            }
            other => panic!("应为 ListVec，实得 {other:?}"),
        }
        match parser("[a, ..rest]").parse_pattern() {
            Some(Pat::ListVec {
                ref elements,
                ref rest,
            }) => {
                assert_eq!(elements.len(), 1);
                assert!(rest.is_some(), "`[a, ..rest]` 必须捕获 rest");
            }
            other => panic!("应为 ListVec，实得 {other:?}"),
        }
    }

    /// 字典模式：条目是 `key: pattern`，`{..}` 表示忽略其余字段。
    ///
    /// `rest` 标志区分「必须恰好匹配这些键」与「允许多余键」，配错会让
    /// 多余字段从静默通过变成误拒（或反之）。
    #[test]
    fn dict_patterns_distinguish_strict_from_rest() {
        match parser("{a: 1, b: x}").parse_pattern() {
            Some(Pat::Dict { ref required, rest }) => {
                assert_eq!(required.len(), 2, "两个必填键");
                assert!(!rest, "无 `..` ⇒ rest 为 false");
            }
            other => panic!("应为 Dict，实得 {other:?}"),
        }
        match parser("{a: 1, ..}").parse_pattern() {
            Some(Pat::Dict { ref required, rest }) => {
                assert_eq!(required.len(), 1);
                assert!(rest, "`{{a: 1, ..}}` 必须置 rest");
            }
            other => panic!("应为 Dict，实得 {other:?}"),
        }
    }

    /// 单一类型注解**不得**被包成 `Union(vec![T])` —— 单成员 union 会
    /// 让 `subtype_of` / `unify` 多走一条匹配分支，且打印诊断时呈现为
    /// 只有一个成员的花括号。
    #[test]
    fn single_type_annotation_is_not_wrapped_in_a_one_member_union() {
        let ty = parser("int").parse_type_annotation().expect("int 应可解析");
        assert_eq!(ty, Type::Int);
        assert!(
            !matches!(ty, Type::Union(_)),
            "单类型不得包装成 Union：{ty:?}"
        );
    }

    /// `|` 连接的多个类型必须合成真正的 `Union`。
    #[test]
    fn pipe_joined_annotations_form_a_union() {
        let ty = parser("string | number")
            .parse_type_annotation()
            .expect("联合类型应可解析");
        match &ty {
            Type::Union(members) => assert_eq!(members.len(), 2, "两个成员：{ty:?}"),
            other => panic!("应为 Union，实得 {other:?}"),
        }
    }

    /// D64：`nil` 是**关键字 token**，走不到 `Identifier` 白名单分支。
    ///
    /// 修前白名单里的 `"nil" => Type::Nil` 是**不可达的死 arm**，
    /// `let v: nil = nil` 直接报 "expected type annotation"，而 spec §3.1
    /// 明确把 `nil` 列为正式类型，typeck 三处也都已成对支持 `Nil`。
    #[test]
    fn d64_nil_keyword_parses_as_a_type() {
        assert_eq!(
            parser("nil").parse_type_annotation(),
            Some(Type::Nil),
            "`nil` 关键字必须解析为 Type::Nil"
        );
        assert!(
            ParserV3::compile("let v: nil = nil\nprint(v)\n").is_ok(),
            "spec §3.1 把 `nil` 列为正式类型，端到端必须能编译"
        );
    }

    /// `dyn Trait<...>` 泛型实参必须成对解析，缺 `>` 报错。
    #[test]
    fn dyn_trait_objects_parse_with_and_without_generics() {
        let ty = parser("dyn Show")
            .parse_type_annotation()
            .expect("dyn Show");
        assert!(matches!(ty, Type::TraitObject { .. }));

        let ty = parser("dyn Show<number>").parse_type_annotation();
        assert!(
            matches!(ty, Some(Type::TraitObject { ref generics, .. }) if generics.len() == 1),
            "泛型实参应被收进 TraitObject：{ty:?}"
        );

        assert!(
            parser("dyn Show<number").parse_type_annotation().is_none(),
            "缺 `>` 必须报错，不得当成合法注解放行"
        );
    }
}
