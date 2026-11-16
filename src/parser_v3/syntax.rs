//! ParserV3 语法辅助子系统（自 parse.rs 迁出，P1.4 拆分）。
//!
//! 这些方法直接产出 `MirWitness`（witness-native），不再经 MirExpr 桥接：
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
            TokenType::Float(val) => {
                self.advance();
                Some(crate::common::Literal::Float(val, self.span_of_current()))
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
        if !self.match_token(&[TokenType::Less]) {
            return Some(Vec::new());
        }
        let mut names: Vec<String> = Vec::new();
        loop {
            let n = self.consume_identifier("Expected type parameter name")?;
            if names.contains(&n) {
                eprintln!(
                    "Parse error: duplicate generic type parameter '{n}' at line {}",
                    self.current_line()
                );
                return None;
            }
            names.push(n);
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
        Some(names)
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
            TokenType::Identifier(name) => {
                let lower = name.to_lowercase();
                if matches!(
                    self.tokens.get(self.current + 1).map(|t| &t.token_type),
                    Some(TokenType::Less)
                ) {
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
                            eprintln!(
                                "Parse error: unsupported generic type annotation '{}' at line {}",
                                other,
                                self.current_line()
                            );
                            None
                        }
                    };
                }
                let ty = match lower.as_str() {
                    "int" | "i4" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" => {
                        Type::Int
                    }
                    "number" => Type::Union(vec![Type::Int, Type::Float]),
                    "float" | "f16" | "bf16" | "fp8" | "f32" | "f64" => Type::Float,
                    "string" => Type::String,
                    "char" => Type::Char,
                    "bool" => Type::Bool,
                    "nil" => Type::Nil,
                    "context" => Type::Context,
                    "channel" => Type::Channel(Box::new(Type::Any)),
                    "task_handle" | "taskhandle" => Type::TaskHandle(Box::new(Type::Any)),
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
