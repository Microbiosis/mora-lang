use crate::ast::*;
use crate::lexer::{Lexer, Token, TokenType};

pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, current: 0 }
    }

    pub fn parse(&mut self) -> Vec<Stmt> {
        let mut statements = Vec::new();
        while !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                statements.push(stmt);
            }
        }
        statements
    }

    fn declaration(&mut self) -> Option<Stmt> {
        let exported = self.match_token(&[TokenType::Export]);
        if self.match_token(&[TokenType::Let]) {
            Some(self.let_declaration(exported))
        } else if self.match_token(&[TokenType::Task]) {
            Some(self.task_declaration(exported))
        } else if self.match_token(&[TokenType::Trait]) {
            Some(self.parse_trait_def(exported))
        } else if self.match_token(&[TokenType::Impl]) {
            Some(self.parse_impl_def())
        } else if self.match_token(&[TokenType::Type]) {
            Some(self.parse_type_alias())
        } else if self.match_token(&[TokenType::Enum]) {
            Some(self.parse_enum_def())
        } else if self.match_token(&[TokenType::Struct]) {
            Some(self.parse_struct_def())
        } else if exported {
            panic!(
                "Expected 'let' or 'task' after 'export' at line {}",
                self.peek().map(|t| t.line).unwrap_or(0)
            )
        } else {
            self.statement()
        }
    }

    fn let_declaration(&mut self, exported: bool) -> Stmt {
        // 'let' 关键字位置（"previous" 是刚消耗的 let token）
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected variable name after 'let'");
        // v0.05: 互斥语法 —— `:` (类型 hint) 或 `:=` (显式 Any)
        // v0.13: Walrus 语法已删除, let 必须 `: T = expr` 或省略
        // v0.08: 支持 `let x: dyn Trait = expr`
        // v0.09: 支持 `let x: dyn Trait<T> = expr`（泛型 trait）
        let type_hint = if self.match_token(&[TokenType::Colon]) {
            // 检查是否是 dyn Trait
            let hint = if self.match_token(&[TokenType::Dyn]) {
                let tname = self.consume_identifier("Expected trait name after 'dyn'");
                // v0.09: 解析泛型 `<T, U>`（如果存在）
                let generics_suffix = if self.check(&TokenType::Less) {
                    self.parse_type_list_to_string()
                } else {
                    String::new()
                };
                format!("dyn:{}{}", tname, generics_suffix)
            } else {
                // v0.x: 支持泛型类型 hint：`list<int>` / `dict<string, number>`
                // parse_type_name_recursive 内部处理嵌套泛型
                self.parse_type_name_recursive()
            };
            Some(hint)
        } else {
            None
        };
        self.consume(&TokenType::Assign, "Expected '=' after variable name/type");
        let init = self.expression();
        Stmt::Let {
            name,
            type_hint,
            init,
            exported,
            span,
        }
    }

    fn task_declaration(&mut self, exported: bool) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected task name");

        // v0.21: 解析生命周期参数 <'a, 'b>
        let mut lifetime_params = Vec::new();
        if self.check(&TokenType::Less) {
            self.advance(); // consume '<'
            loop {
                if let Some(Token {
                    token_type: TokenType::Lifetime(lt),
                    ..
                }) = self.peek().cloned()
                {
                    self.advance();
                    lifetime_params.push(lt);
                    if self.match_token(&[TokenType::Comma]) {
                        continue;
                    }
                }
                if self.check(&TokenType::Greater) {
                    self.advance(); // consume '>'
                    break;
                }
                if self.is_at_end() {
                    break;
                }
            }
        }

        self.consume(&TokenType::LParen, "Expected '(' after task name");
        let params = if self.check(&TokenType::RParen) {
            vec![]
        } else {
            self.parameters()
        };
        self.consume(&TokenType::RParen, "Expected ')' after parameters");
        // v11: 可选返回类型 hint —— `): T` 或 `) : T`
        let return_type = if self.check(&TokenType::Colon) {
            self.advance();
            Some(self.consume_identifier("Expected return type after ':'"))
        } else {
            None
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after task body");
        Stmt::TaskDef {
            name,
            lifetime_params,
            params,
            return_type,
            body,
            exported,
            span,
        }
    }

    fn parameters(&mut self) -> Vec<(String, Option<String>)> {
        let mut params = vec![self.typed_parameter()];
        while self.match_token(&[TokenType::Comma]) {
            params.push(self.typed_parameter());
        }
        params
    }

    fn typed_parameter(&mut self) -> (String, Option<String>) {
        let name = self.consume_identifier("Expected parameter name");
        let type_hint = if self.match_token(&[TokenType::Colon]) {
            Some(self.consume_identifier("Expected type name after ':'"))
        } else {
            None
        };
        (name, type_hint)
    }

    /// 取"刚消耗的关键字 token"的行号和列号用于 span。
    fn span_of_previous_keyword(&self) -> Span {
        if let Some(tok) = self.previous() {
            Span::new(tok.line, tok.column)
        } else {
            Span::default()
        }
    }

    /// peek 当前 token 是否是名字为 `name` 的普通标识符
    fn peek_is_identifier(&self, name: &str) -> bool {
        matches!(self.peek(), Some(Token { token_type: TokenType::Identifier(n), .. }) if n == name)
    }

    /// 消耗当前 token, 若是指定名字的普通标识符则返回 true; 否则不消耗并返回 false
    fn match_identifier(&mut self, name: &str) -> bool {
        if self.peek_is_identifier(name) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn statement(&mut self) -> Option<Stmt> {
        if self.match_token(&[TokenType::If]) {
            Some(self.if_statement())
        } else if self.match_token(&[TokenType::For]) {
            Some(self.for_statement())
        } else if self.match_token(&[TokenType::Import]) {
            Some(self.import_statement())
        } else if self.match_token(&[TokenType::Parallel]) {
            Some(self.parallel_statement())
        } else if self.match_token(&[TokenType::Transaction]) {
            Some(self.transaction_statement())
        } else if self.match_token(&[TokenType::Commit]) {
            Some(Stmt::Commit {
                span: self.span_of_previous_keyword(),
            })
        } else if self.match_token(&[TokenType::Rollback]) {
            Some(Stmt::Rollback {
                span: self.span_of_previous_keyword(),
            })
        } else if self.match_token(&[TokenType::Macro]) {
            Some(self.macro_definition())
        } else if self.match_token(&[TokenType::Save]) {
            Some(self.save_statement())
        } else if self.match_token(&[TokenType::Load]) {
            Some(self.load_statement())
        } else if self.match_token(&[TokenType::ReadBytes]) {
            Some(self.read_bytes_statement())
        } else if self.match_token(&[TokenType::WriteBytes]) {
            Some(self.write_bytes_statement())
        } else if self.match_token(&[TokenType::Read]) {
            Some(self.read_statement())
        } else if self.match_token(&[TokenType::Append]) {
            Some(self.append_statement())
        } else if self.match_token(&[TokenType::Write]) {
            Some(self.write_statement())
        } else if self.match_token(&[TokenType::Return]) {
            Some(self.return_statement())
        }
        // v0.04.0: AI 原语
        else if self.match_token(&[TokenType::WithKeyword]) {
            Some(self.with_statement())
        } else if self.match_token(&[TokenType::Stream]) {
            Some(self.stream_statement())
        } else if self.match_token(&[TokenType::Tool]) {
            Some(self.tool_statement())
        } else if self.match_token(&[TokenType::Break]) {
            Some(self.break_statement())
        } else if self.match_token(&[TokenType::Continue]) {
            Some(self.continue_statement())
        }
        // v0.04: 云服务原生（serve as 语法糖已移除，走显式 Router/McpServer API）
        else if self.match_token(&[TokenType::Route]) {
            Some(self.route_statement())
        } else if self.match_token(&[TokenType::Observe]) {
            Some(self.observe_statement())
        } else if self.match_token(&[TokenType::Span]) {
            Some(self.span_statement())
        }
        // v0.04.0 终态补: 显式 token 计数（RFC §2.4）
        // 词法把 "record_tokens" 整体当普通标识符；match_identifier 先消耗它, 然后调 record_tokens_statement
        else if self.match_identifier("record_tokens") {
            Some(self.record_tokens_statement())
        } else if self.check_index_assignment() {
            Some(self.index_assignment())
        } else if self.check_assignment() {
            Some(self.assignment_statement())
        } else {
            self.expression_statement()
        }
    }

    fn import_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = match self.peek() {
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => panic!(
                "Expected string path after 'import' at line {}",
                self.peek().map(|t| t.line).unwrap_or(0)
            ),
        };
        Stmt::Import { path, span }
    }

    /// v0.23: type Name = TargetType
    fn parse_type_alias(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected type alias name");

        // 可选泛型参数 <T, U>
        let mut generics = Vec::new();
        if self.check(&TokenType::Less) {
            self.advance(); // consume '<'
            loop {
                let param = self.consume_identifier("Expected generic parameter");
                generics.push(param);
                if self.match_token(&[TokenType::Comma]) {
                    continue;
                }
                if self.check(&TokenType::Greater) {
                    self.advance(); // consume '>'
                    break;
                }
                if self.is_at_end() {
                    break;
                }
            }
        }

        self.consume(&TokenType::Assign, "Expected '=' after type alias name");
        let target = self.consume_identifier("Expected target type");
        Stmt::TypeAlias {
            name,
            generics,
            target,
            span,
        }
    }

    /// v0.23: enum Name { Variant1, Variant2(Type) }
    fn parse_enum_def(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected enum name");

        // 可选泛型参数
        let mut generics = Vec::new();
        if self.check(&TokenType::Less) {
            self.advance();
            loop {
                let param = self.consume_identifier("Expected generic parameter");
                generics.push(param);
                if self.match_token(&[TokenType::Comma]) {
                    continue;
                }
                if self.check(&TokenType::Greater) {
                    self.advance();
                    break;
                }
                if self.is_at_end() {
                    break;
                }
            }
        }

        self.consume(&TokenType::LBrace, "Expected '{' after enum name");
        while self.check(&TokenType::Newline) {
            self.advance();
        }

        let mut variants = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            let variant_name = self.consume_identifier("Expected variant name");
            let data = if self.check(&TokenType::LParen) {
                self.advance(); // consume '('
                let t = self.consume_identifier("Expected variant data type");
                self.consume(&TokenType::RParen, "Expected ')' after variant data");
                Some(t)
            } else {
                None
            };
            variants.push(crate::ast::EnumVariant {
                name: variant_name,
                data,
            });
            self.match_token(&[TokenType::Comma]);
        }
        self.consume(&TokenType::RBrace, "Expected '}' after enum variants");
        Stmt::EnumDef {
            name,
            generics,
            variants,
            span,
        }
    }

    /// v0.23: struct Name { field1: Type, field2: Type }
    fn parse_struct_def(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected struct name");

        // 可选泛型参数
        let mut generics = Vec::new();
        if self.check(&TokenType::Less) {
            self.advance();
            loop {
                let param = self.consume_identifier("Expected generic parameter");
                generics.push(param);
                if self.match_token(&[TokenType::Comma]) {
                    continue;
                }
                if self.check(&TokenType::Greater) {
                    self.advance();
                    break;
                }
                if self.is_at_end() {
                    break;
                }
            }
        }

        self.consume(&TokenType::LBrace, "Expected '{' after struct name");
        while self.check(&TokenType::Newline) {
            self.advance();
        }

        let mut fields = Vec::new();
        while !self.check(&TokenType::RBrace) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            let field_name = self.consume_identifier("Expected field name");
            self.consume(&TokenType::Colon, "Expected ':' after field name");
            let type_hint = self.consume_identifier("Expected field type");
            fields.push(crate::ast::StructField {
                name: field_name,
                type_hint,
            });
            self.match_token(&[TokenType::Comma]);
        }
        self.consume(&TokenType::RBrace, "Expected '}' after struct fields");
        Stmt::StructDef {
            name,
            generics,
            fields,
            span,
        }
    }

    fn parallel_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut stmts = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            // v0.19: 检查 worker 声明
            if self.check(&TokenType::Worker) {
                stmts.push(self.worker_statement());
            } else if let Some(stmt) = self.declaration() {
                stmts.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after parallel block");
        Stmt::Parallel { stmts, span }
    }

    /// v0.19: worker name ... end
    fn worker_statement(&mut self) -> Stmt {
        self.advance(); // 消耗 'worker' 关键字
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected worker name");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            // v0.19: 解析 send 语句 (-> value to target)
            if self.check_identifier("->") {
                self.advance(); // consume '->'
                let value = self.expression();
                self.consume_identifier("Expected 'to' after value");
                let target = self.consume_identifier("Expected target worker name");
                body.push(Stmt::Send {
                    value,
                    target,
                    span: Span::default(),
                });
            }
            // v0.19: 解析 receive 语句 (let x = <- source)
            else if self.check(&TokenType::Less)
                && self
                    .peek_next()
                    .map(|t| t.token_type == TokenType::Minus)
                    .unwrap_or(false)
            {
                self.advance(); // consume '<'
                self.advance(); // consume '-'
                let source = self.consume_identifier("Expected source worker name");
                // 创建一个临时变量名，稍后在 let 语句中使用
                body.push(Stmt::Receive {
                    var: "_recv".to_string(),
                    source,
                    span: Span::default(),
                });
            } else if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after worker block");
        Stmt::Worker { name, body, span }
    }

    /// v0.19: transaction ... compensation ... end
    fn transaction_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        let mut compensation = Vec::new();
        let mut in_compensation = false;

        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if self.check(&TokenType::Compensation) {
                self.advance(); // consume 'compensation'
                in_compensation = true;
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
                continue;
            }
            if let Some(stmt) = self.declaration() {
                if in_compensation {
                    compensation.push(stmt);
                } else {
                    body.push(stmt);
                }
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after transaction block");
        Stmt::Transaction {
            body,
            compensation,
            span,
        }
    }

    /// v0.20: macro name(params) ... end
    fn macro_definition(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected macro name");
        self.consume(&TokenType::LParen, "Expected '(' after macro name");
        let mut params = Vec::new();
        if !self.check(&TokenType::RParen) {
            params.push(self.consume_identifier("Expected parameter name"));
            while self.match_token(&[TokenType::Comma]) {
                params.push(self.consume_identifier("Expected parameter name"));
            }
        }
        self.consume(&TokenType::RParen, "Expected ')' after parameters");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after macro body");
        Stmt::MacroDef {
            name,
            params,
            body,
            span,
        }
    }

    /// 检查下一个 token 是否是指定的标识符
    fn check_identifier(&self, name: &str) -> bool {
        if let Some(Token {
            token_type: TokenType::Identifier(n),
            ..
        }) = self.peek()
        {
            n == name
        } else {
            false
        }
    }

    /// 获取下一个 token (不消耗)
    fn peek_next(&self) -> Option<&Token> {
        self.tokens.get(self.current + 1)
    }

    fn save_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' after path in save");
        let value = self.expression();
        Stmt::Save { path, value, span }
    }

    fn load_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' after path in load");
        let var = self.consume_identifier("Expected variable name after ',' in load");
        Stmt::Load { path, var, span }
    }

    fn read_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into' after path in read");
        let var = self.consume_identifier("Expected variable name after 'into' in read");
        Stmt::ReadFile { path, var, span }
    }

    fn write_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' after path in write");
        let content = self.expression();
        Stmt::WriteFile {
            path,
            content,
            span,
        }
    }

    fn append_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' after path in append");
        let content = self.expression();
        Stmt::AppendFile {
            path,
            content,
            span,
        }
    }

    fn read_bytes_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Into, "Expected 'into' after path in read_bytes");
        let var = self.consume_identifier("Expected variable name after 'into' in read_bytes");
        Stmt::ReadBytesFile { path, var, span }
    }

    fn write_bytes_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' after path in write_bytes");
        let content = self.expression();
        Stmt::WriteBytesFile {
            path,
            content,
            span,
        }
    }

    fn if_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let condition = self.expression();
        self.consume(&TokenType::Then, "Expected 'then' after if condition");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut then_branch = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                then_branch.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after if body");
        Stmt::If {
            condition,
            then_branch,
            span,
        }
    }

    fn for_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let var = self.consume_identifier("Expected loop variable after 'for'");
        // v11: 可选 `for x: T in ...`
        let var_type = if self.match_token(&[TokenType::Colon]) {
            Some(self.consume_identifier("Expected type after ':' in for variable"))
        } else {
            None
        };
        self.consume(&TokenType::In, "Expected 'in' after loop variable");
        let iterable = self.expression();
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after for body");
        Stmt::For {
            var,
            var_type,
            iterable,
            body,
            span,
        }
    }

    fn return_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let value =
            if self.check(&TokenType::Newline) || self.check(&TokenType::End) || self.is_at_end() {
                None
            } else {
                Some(self.expression())
            };
        Stmt::Return { value, span }
    }

    fn check_index_assignment(&mut self) -> bool {
        let save = self.current;
        let result = if let Some(Token {
            token_type: TokenType::Identifier(_),
            ..
        }) = self.peek()
        {
            self.advance();
            self.match_token(&[TokenType::LBracket])
        } else {
            false
        };
        self.current = save;
        result
    }

    /// 解析 `IDENT [ expr ] = expr` 形式的索引赋值。
    /// 对象只能是单标识符（不支持 `obj.field[0] = ...` — 那是普通 expression statement）
    fn index_assignment(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        // 对象：先取单标识符名 + 位置，包成 Expr::Variable
        let name_tok = self
            .peek()
            .cloned()
            .expect("check_index_assignment guarantees IDENT");
        let name = match name_tok.token_type {
            TokenType::Identifier(n) => n,
            _ => unreachable!("check_index_assignment guarantees IDENT"),
        };
        let name_span = Span::new(name_tok.line, name_tok.column);
        self.advance(); // 消耗 IDENT
        self.consume(&TokenType::LBracket, "Expected '[' after object");
        let index = self.expression();
        self.consume(&TokenType::RBracket, "Expected ']' after index");
        self.consume(&TokenType::Assign, "Expected '=' after index expression");
        let value = self.expression();
        let object = Expr::Variable(name, name_span);
        Stmt::IndexAssign {
            object,
            index,
            value,
            span,
        }
    }

    fn check_assignment(&self) -> bool {
        if let Some(Token {
            token_type: TokenType::Identifier(_),
            ..
        }) = self.peek()
            && let Some(Token {
                token_type: TokenType::Assign,
                ..
            }) = self.peek_next()
        {
            return true;
        }
        false
    }

    fn assignment_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected identifier");
        self.advance(); // consume '='
        let value = self.expression();
        Stmt::Assign { name, value, span }
    }

    fn expression_statement(&mut self) -> Option<Stmt> {
        let expr = self.expression();
        // 表达式 stmt 的 span 默认 0（不在关键字位置），typeck 可借用外层
        Some(Stmt::Expr(expr))
    }

    fn expression(&mut self) -> Expr {
        self.pipe()
    }

    fn pipe(&mut self) -> Expr {
        let mut expr = self.equality();
        // 跳过换行符后检查管道运算符（支持多行管道链）
        while {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            self.match_token(&[TokenType::Pipe])
        } {
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            let right = self.equality();
            expr = Expr::Pipe {
                left: Box::new(expr),
                right: Box::new(right),
                span: Span::default(),
            };
        }
        expr
    }

    fn equality(&mut self) -> Expr {
        let mut expr = self.comparison();
        while self.match_token(&[TokenType::Equal, TokenType::NotEqual]) {
            let op = self.previous_op();
            let right = self.comparison();
            let span = self.span_of_previous_keyword();
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn comparison(&mut self) -> Expr {
        let mut expr = self.term();
        while self.match_token(&[
            TokenType::Greater,
            TokenType::GreaterEqual,
            TokenType::Less,
            TokenType::LessEqual,
        ]) {
            let op = self.previous_op();
            let right = self.term();
            let span = self.span_of_previous_keyword();
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn term(&mut self) -> Expr {
        let mut expr = self.factor();
        while self.match_token(&[TokenType::Plus, TokenType::Minus]) {
            let op = self.previous_op();
            let right = self.factor();
            let span = self.span_of_previous_keyword();
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn factor(&mut self) -> Expr {
        let mut expr = self.unary();
        while self.match_token(&[TokenType::Star, TokenType::Slash, TokenType::Percent]) {
            let op = self.previous_op();
            let right = self.unary();
            let span = self.span_of_previous_keyword();
            expr = Expr::Binary {
                left: Box::new(expr),
                op,
                right: Box::new(right),
                span,
            };
        }
        expr
    }

    fn unary(&mut self) -> Expr {
        if self.match_token(&[TokenType::Minus]) {
            let op = self.previous_op();
            let right = self.unary();
            let span = self.span_of_previous_keyword();
            Expr::Binary {
                left: Box::new(Expr::Literal(Literal::Number(0.0, Span::default()))),
                op,
                right: Box::new(right),
                span,
            }
        } else if self.match_token(&[TokenType::Match]) {
            self.match_expression()
        } else {
            let mut expr = self.call();
            // v0.06.2: 后置 ? 操作符
            if self.match_token(&[TokenType::Question]) {
                let span = self.span_of_previous_keyword();
                expr = Expr::Question {
                    expr: Box::new(expr),
                    span,
                };
            }
            expr
        }
    }

    fn match_expression(&mut self) -> Expr {
        let expr = self.expression();
        self.consume(
            &TokenType::WithKeyword,
            "Expected 'with' after match expression",
        );
        let mut arms = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(pattern) = self.pattern() {
                // v0.16: 解析 when 守卫条件
                let is_when = if let Some(Token {
                    token_type: TokenType::Identifier(name),
                    ..
                }) = self.peek()
                {
                    name == "when"
                } else {
                    false
                };
                let pattern = if is_when {
                    self.advance(); // consume 'when'
                    let condition = self.expression();
                    Pattern::Guard {
                        pattern: Box::new(pattern),
                        condition: Box::new(condition),
                    }
                } else {
                    pattern
                };
                self.consume(&TokenType::Arrow, "Expected '->' after pattern");
                let arm_expr = self.expression();
                arms.push((pattern, Box::new(arm_expr)));
                while self.check(&TokenType::Newline) {
                    self.advance();
                }
            } else {
                break;
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after match arms");
        Expr::Match {
            expr: Box::new(expr),
            arms,
            span: Span::default(),
        }
    }

    fn pattern(&mut self) -> Option<Pattern> {
        if let Some(Token {
            token_type: TokenType::Identifier(name),
            ..
        }) = self.peek()
        {
            if name == "end" {
                return None;
            }
            if name == "_" {
                self.advance();
                return Some(Pattern::Wildcard);
            }
        }

        if self.match_token(&[TokenType::True]) {
            Some(Pattern::Literal(Literal::Bool(true, Span::default())))
        } else if self.match_token(&[TokenType::False]) {
            Some(Pattern::Literal(Literal::Bool(false, Span::default())))
        } else if self.match_token(&[TokenType::Nil]) {
            Some(Pattern::Literal(Literal::Nil(Span::default())))
        } else if let Some(Token {
            token_type: TokenType::Number(n),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Some(Pattern::Literal(Literal::Number(n, Span::default())))
        } else if let Some(Token {
            token_type: TokenType::String(s),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Some(Pattern::Literal(Literal::String(s, Span::default())))
        } else if self.match_token(&[TokenType::LBracket]) {
            let mut items = Vec::new();
            let mut rest = None;
            if !self.check(&TokenType::RBracket) {
                // 检查是否是 ...rest 模式
                if self.check(&TokenType::DotDotDot) {
                    self.advance(); // consume '...'
                    rest = Some(self.consume_identifier("Expected variable name after '...'"));
                } else {
                    if let Some(p) = self.pattern() {
                        items.push(p);
                    }
                    while self.match_token(&[TokenType::Comma]) {
                        // 检查是否是 ...rest 模式
                        if self.check(&TokenType::DotDotDot) {
                            self.advance(); // consume '...'
                            rest =
                                Some(self.consume_identifier("Expected variable name after '...'"));
                            break;
                        }
                        if let Some(p) = self.pattern() {
                            items.push(p);
                        }
                    }
                }
            }
            self.consume(&TokenType::RBracket, "Expected ']' after list pattern");
            Some(Pattern::List {
                prefix: items,
                rest,
            })
        } else if self.match_token(&[TokenType::LBrace]) {
            let mut entries = Vec::new();
            if !self.check(&TokenType::RBrace) {
                entries.push(self.dict_pattern_entry());
                while self.match_token(&[TokenType::Comma]) {
                    entries.push(self.dict_pattern_entry());
                }
            }
            self.consume(&TokenType::RBrace, "Expected '}' after dict pattern");
            Some(Pattern::Dict(entries))
        } else if let Some(Token {
            token_type: TokenType::Identifier(name),
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Some(Pattern::Variable(name))
        } else {
            None
        }
    }

    fn dict_pattern_entry(&mut self) -> (String, Pattern) {
        let key = match self.peek() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                let name = name.clone();
                self.advance();
                name
            }
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => panic!(
                "Expected identifier or string as dict pattern key at line {}",
                self.peek().map(|t| t.line).unwrap_or(0)
            ),
        };
        self.consume(&TokenType::Colon, "Expected ':' after dict pattern key");
        let pattern = self.pattern().expect("Expected pattern after ':'");
        (key, pattern)
    }

    fn call(&mut self) -> Expr {
        let mut expr = self.primary();
        loop {
            if self.match_token(&[TokenType::LParen]) {
                let span = self.span_of_previous_keyword();
                if let Expr::Variable(name, _) = &expr {
                    if name == "ai_model" {
                        // ai_model(...) 走专用解析, 支持 keyword arg
                        expr = self.parse_ai_model_call(span);
                    } else if self.check(&TokenType::RParen) {
                        // 空参, 直接生成 Call
                        self.advance();
                        expr = Expr::Call {
                            callee: name.clone(),
                            args: vec![],
                            span,
                        };
                    } else {
                        let args = self.arguments();
                        self.consume(&TokenType::RParen, "Expected ')' after arguments");
                        expr = Expr::Call {
                            callee: name.clone(),
                            args,
                            span,
                        };
                    }
                } else if let Expr::NamespaceRef {
                    namespace, name, ..
                } = &expr
                {
                    // Router::new() / McpServer::new() / Container<number>::new() etc.
                    // v0.09: namespace 可能含 generics（如 "Container<number>"）
                    let callee = format!("{}::{}", namespace, name);
                    if self.check(&TokenType::RParen) {
                        self.advance();
                        expr = Expr::Call {
                            callee,
                            args: vec![],
                            span,
                        };
                    } else {
                        let args = self.arguments();
                        self.consume(&TokenType::RParen, "Expected ')' after arguments");
                        expr = Expr::Call { callee, args, span };
                    }
                } else {
                    // other Expr followed by (): skip arguments and treat as direct call attempt
                    if self.check(&TokenType::RParen) {
                        self.advance();
                    } else {
                        let _args = self.arguments();
                        self.consume(&TokenType::RParen, "Expected ')' after arguments");
                    }
                    panic!("Can only call functions by name in Mora v1");
                }
            } else if self.match_token(&[TokenType::LBracket]) {
                let span = self.span_of_previous_keyword();
                let index = self.expression();
                self.consume(&TokenType::RBracket, "Expected ']' after index");
                expr = Expr::Index {
                    object: Box::new(expr),
                    index: Box::new(index),
                    span,
                };
            } else if self.match_token(&[TokenType::Dot]) {
                let _span = self.span_of_previous_keyword();
                let method = self.consume_method_name("Expected method name after '.'");
                let args = if self.match_token(&[TokenType::LParen]) {
                    let a = if self.check(&TokenType::RParen) {
                        vec![]
                    } else {
                        self.arguments()
                    };
                    self.consume(&TokenType::RParen, "Expected ')' after arguments");
                    a
                } else {
                    vec![]
                };
                let span = self.span_of_previous_keyword(); // span for MethodCall
                expr = Expr::MethodCall {
                    object: Box::new(expr),
                    method,
                    args,
                    span,
                };
            } else if self.match_token(&[TokenType::ColonColon]) {
                // v0.07.1: expr::method call - convert to NamespaceRef or handle inline
                let method = self.consume_method_name("Expected method name after '::'");
                let cspan = self.span_of_previous_keyword();
                match &expr {
                    Expr::Variable(ns, _) if ns == "Router" || ns == "McpServer" => {
                        let callee = format!("{}::{}", ns, method);
                        let args = if self.match_token(&[TokenType::LParen]) {
                            let a = if self.check(&TokenType::RParen) {
                                vec![]
                            } else {
                                self.arguments()
                            };
                            self.consume(&TokenType::RParen, "Expected ')' after arguments");
                            a
                        } else {
                            vec![]
                        };
                        expr = Expr::Call {
                            callee,
                            args,
                            span: cspan,
                        };
                    }
                    _ => panic!(
                        "Expected method name after '.' at line {}",
                        self.peek().map(|t| t.line).unwrap_or(0)
                    ),
                }
            } else {
                break;
            }
        }
        expr
    }

    fn primary(&mut self) -> Expr {
        // v0.21: 借用表达式
        if self.match_token(&[TokenType::AmpMut]) {
            let span = self.span_of_previous_keyword();
            let expr = self.expression();
            Expr::BorrowMut {
                expr: Box::new(expr),
                span,
            }
        } else if self.match_token(&[TokenType::Amp]) {
            let span = self.span_of_previous_keyword();
            let expr = self.expression();
            Expr::Borrow {
                expr: Box::new(expr),
                span,
            }
        } else if self.match_token(&[TokenType::True]) {
            let span = self.span_of_previous_keyword();
            Expr::Literal(Literal::Bool(true, span))
        } else if self.match_token(&[TokenType::False]) {
            let span = self.span_of_previous_keyword();
            Expr::Literal(Literal::Bool(false, span))
        } else if self.match_token(&[TokenType::Nil]) {
            let span = self.span_of_previous_keyword();
            Expr::Literal(Literal::Nil(span))
        } else if let Some(Token {
            token_type: TokenType::Char(c),
            line,
            column,
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Expr::Literal(Literal::Char(c, Span::new(line, column)))
        } else if self.match_token(&[TokenType::Fn]) {
            self.closure_expression()
        } else if let Some(Token {
            token_type: TokenType::Number(n),
            line,
            column,
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            Expr::Literal(Literal::Number(n, Span::new(line, column)))
        } else if let Some(Token {
            token_type: TokenType::String(s),
            line,
            column,
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            // 只在 { 后跟标识符字符时才触发格式字符串解析
            // 避免误触发 JSON 字符串如 "{"name":"hello"}"
            if has_format_interpolation(&s) {
                self.parse_format_string(&s)
            } else {
                Expr::Literal(Literal::String(s, Span::new(line, column)))
            }
        }
        // v0.04.0: p"..." prompt 表达式
        else if let Some(Token {
            token_type: TokenType::PromptString(s),
            line,
            column,
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            let span = Span::new(line, column);
            let inner = if has_format_interpolation(&s) {
                self.parse_format_string(&s)
            } else {
                Expr::Literal(Literal::String(s, span))
            };
            // 无论是否有插值，都包成 Prompt 节点，让解释器走 ai.chat
            let parts = match inner {
                Expr::Literal(Literal::String(s, _)) => {
                    vec![Expr::Literal(Literal::String(s, span))]
                }
                Expr::Binary {
                    left,
                    op: BinaryOp::Add,
                    right,
                    ..
                } => self.flatten_prompt_parts(*left, *right),
                other => vec![other],
            };
            Expr::Prompt { parts, span }
        } else if self.match_token(&[TokenType::LBracket]) {
            let span = self.span_of_previous_keyword();
            let mut items = Vec::new();
            if !self.check(&TokenType::RBracket) {
                items.push(Box::new(self.expression()));
                while self.match_token(&[TokenType::Comma]) {
                    items.push(Box::new(self.expression()));
                }
            }
            self.consume(&TokenType::RBracket, "Expected ']' after list");
            Expr::Literal(Literal::List(items, span))
        } else if self.match_token(&[TokenType::LBrace]) {
            let span = self.span_of_previous_keyword();
            let mut entries = Vec::new();
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if !self.check(&TokenType::RBrace) {
                let (k, v) = self.dict_entry();
                entries.push((k, Box::new(v)));
                while self.match_token(&[TokenType::Comma]) {
                    while self.check(&TokenType::Newline) {
                        self.advance();
                    }
                    if self.check(&TokenType::RBrace) {
                        break;
                    }
                    let (k, v) = self.dict_entry();
                    entries.push((k, Box::new(v)));
                }
            }
            self.consume(&TokenType::RBrace, "Expected '}' after dict");
            Expr::Literal(Literal::Dict(entries, span))
        } else if let Some(Token {
            token_type: TokenType::Identifier(name),
            line,
            column,
            ..
        }) = self.peek().cloned()
        {
            self.advance();
            // v0.09: 检查是否带泛型 `<T, U>` —— 需满足 <T(,T)*> 形式才识别（避免 a < b 比较运算误识别）
            let mut ns_or_name = name.clone();
            if self.check(&TokenType::Less) && self.peek_type_list_can_close() {
                // 解析泛型，把 "Foo<T,U>" 拼到 namespace
                let generics = self.parse_type_list();
                ns_or_name = format!("{}<{}>", name, generics.join(","));
            }
            // v0.07.1: IDENT::IDENT → NamespaceRef
            if self.match_token(&[TokenType::ColonColon]) {
                let method = self.consume_identifier("Expected name after '::'");
                Expr::NamespaceRef {
                    namespace: ns_or_name,
                    name: method,
                    span: Span::new(line, column),
                }
            } else {
                Expr::Variable(ns_or_name, Span::new(line, column))
            }
        } else if self.match_token(&[TokenType::LParen]) {
            let span = self.span_of_previous_keyword();
            let expr = self.expression();
            self.consume(&TokenType::RParen, "Expected ')' after expression");
            Expr::Grouping(Box::new(expr), span)
        } else {
            panic!(
                "Unexpected token: {:?} at line {}",
                self.peek(),
                self.peek().map(|t| t.line).unwrap_or(0)
            )
        }
    }

    fn closure_expression(&mut self) -> Expr {
        let span = self.span_of_previous_keyword();
        self.consume(&TokenType::LParen, "Expected '(' after 'fn'");
        let params = if self.check(&TokenType::RParen) {
            vec![]
        } else {
            self.parameters()
        };
        self.consume(&TokenType::RParen, "Expected ')' after parameters");
        // v11: 可选返回类型 hint —— `): T`
        let return_type = if self.check(&TokenType::Colon) {
            self.advance();
            Some(self.consume_identifier("Expected return type after ':' in closure"))
        } else {
            None
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after closure body");
        Expr::Closure {
            params,
            return_type,
            body,
            span,
        }
    }

    fn dict_entry(&mut self) -> (String, Expr) {
        let key = match self.peek() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                let name = name.clone();
                self.advance();
                name
            }
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => panic!(
                "Expected identifier or string as dict key at line {}",
                self.peek().map(|t| t.line).unwrap_or(0)
            ),
        };
        self.consume(&TokenType::Colon, "Expected ':' after dict key");
        let value = self.expression();
        (key, value)
    }

    fn parse_format_string(&mut self, s: &str) -> Expr {
        let mut parts: Vec<Expr> = Vec::new();
        let mut current = String::new();
        let mut chars = s.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '{' {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    current.push('{');
                } else {
                    if !current.is_empty() {
                        parts.push(Expr::Literal(Literal::String(
                            current.clone(),
                            Span::default(),
                        )));
                        current.clear();
                    }
                    let mut expr_str = String::new();
                    let mut depth = 1;
                    for c in chars.by_ref() {
                        if c == '{' {
                            depth += 1;
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        expr_str.push(c);
                    }
                    if depth != 0 {
                        panic!("Unmatched '{{' in format string");
                    }
                    let mut lexer = Lexer::new(&expr_str);
                    let tokens = lexer.scan_tokens();
                    let mut parser = Parser::new(tokens);
                    let expr = parser.expression();
                    parts.push(expr);
                }
            } else {
                current.push(ch);
            }
        }

        if !current.is_empty() {
            parts.push(Expr::Literal(Literal::String(current, Span::default())));
        }

        if parts.is_empty() {
            Expr::Literal(Literal::String(String::new(), Span::default()))
        } else {
            let mut result = parts.remove(0);
            for part in parts {
                result = Expr::Binary {
                    left: Box::new(result),
                    op: BinaryOp::Add,
                    right: Box::new(part),
                    span: Span::default(),
                };
            }
            result
        }
    }

    // Box<Expr> 是必要的：Expr 是递归 enum（Call.args: Vec<Expr>...），
    // 不包 Box 会导致无限大小。Vec<Expr> 已堆分配，再 Box 一次反而多一层间接。
    // 此处保留 Box<Expr> 以保持 ast.rs 公共类型签名一致；
    // AST 改动（移除 Box）属于独立的 v0.x 重构，超出本次 lint 清理范围。
    #[allow(clippy::vec_box)]
    fn arguments(&mut self) -> Vec<Box<Expr>> {
        let mut args = vec![Box::new(self.expression())];
        while self.match_token(&[TokenType::Comma]) {
            args.push(Box::new(self.expression()));
        }
        args
    }

    fn match_token(&mut self, types: &[TokenType]) -> bool {
        for t in types {
            if self.check(t) {
                self.advance();
                return true;
            }
        }
        false
    }

    fn check(&self, token_type: &TokenType) -> bool {
        if let Some(token) = self.peek() {
            std::mem::discriminant(&token.token_type) == std::mem::discriminant(token_type)
        } else {
            false
        }
    }

    fn advance(&mut self) -> Option<&Token> {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    fn is_at_end(&self) -> bool {
        self.check(&TokenType::EOF)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.current)
    }
    fn previous(&self) -> Option<&Token> {
        self.tokens.get(self.current - 1)
    }

    /// v0.09: 检查当前 `<` 后是否是 IDENT（用于消歧泛型 vs 比较）
    /// peek() = Less, peek_next() = Identifier(_) → 泛型开始
    fn peek_after_less_is_ident(&self) -> bool {
        let is_less = self
            .peek()
            .map(|t| matches!(t.token_type, TokenType::Less))
            .unwrap_or(false);
        let next_is_ident = self
            .peek_next()
            .map(|t| matches!(t.token_type, TokenType::Identifier(_)))
            .unwrap_or(false);
        is_less && next_is_ident
    }

    /// v0.09 修复: 在表达式上下文中, `IDENT < ...` 可能是比较运算 (a < b) 也可能是泛型 (Foo<T>)。
    /// 仅当 `<` 后能形成完整类型列表 `T(,T)* >` 时才识别为泛型,
    /// 否则留给后续比较运算符处理。**支持嵌套 `Foo<Bar<T>>`**
    /// peek() 假定是 `Less`
    fn peek_type_list_can_close(&self) -> bool {
        let tokens = &self.tokens;
        let start = self.current;
        if !matches!(
            tokens.get(start).map(|t| &t.token_type),
            Some(TokenType::Less)
        ) {
            return false;
        }
        // 跳过一段完整的类型名（含嵌套泛型）
        fn skip_type(tokens: &[crate::lexer::Token], mut i: usize) -> Option<usize> {
            // 必须以 IDENT 开头
            match tokens.get(i).map(|t| &t.token_type) {
                Some(TokenType::Identifier(_)) => {
                    i += 1;
                }
                _ => return None,
            }
            // 可选嵌套 <T, U>
            if matches!(tokens.get(i).map(|t| &t.token_type), Some(TokenType::Less)) {
                i += 1;
                // 至少一个类型
                i = skip_type(tokens, i)?;
                loop {
                    match tokens.get(i).map(|t| &t.token_type) {
                        Some(TokenType::Greater) => {
                            i += 1;
                            break;
                        }
                        Some(TokenType::Comma) => {
                            i += 1;
                        }
                        _ => return None,
                    }
                    i = skip_type(tokens, i)?;
                }
            }
            Some(i)
        }
        let mut i = start + 1;
        // 第一个类型
        i = match skip_type(tokens, i) {
            Some(v) => v,
            None => return false,
        };
        loop {
            match tokens.get(i).map(|t| &t.token_type) {
                Some(TokenType::Greater) => return true,
                Some(TokenType::Comma) => {
                    i += 1;
                }
                _ => return false,
            }
            i = match skip_type(tokens, i) {
                Some(v) => v,
                None => return false,
            };
        }
    }

    /// v0.09: 解析 trait/impl 的泛型参数 `<T>` 或 `<T: Bound>` 或 `<T, U, V>`
    /// 当前 token 假设是 `<` (Less)
    fn parse_generic_params(&mut self) -> Vec<crate::ast::GenericParam> {
        use crate::ast::GenericParam;
        let mut params = Vec::new();
        // 当前 token 是 `<`
        self.advance(); // 消耗 `<`
        loop {
            let pspan = self.span_of_previous_keyword();
            let pname = self.consume_identifier("Expected generic param name");
            let pbound = if self.match_token(&[TokenType::Colon]) {
                Some(self.consume_identifier("Expected bound trait name after ':'"))
            } else {
                None
            };
            params.push(GenericParam {
                name: pname,
                bound: pbound,
                span: pspan,
            });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(&TokenType::Greater, "Expected '>' after generic params");
        params
    }

    /// v0.09: 解析类型列表 `<T, U, V>` 用于 trait_generics / for_generics
    /// 当前 token 假设是 `<` (Less)
    /// v0.09 完整版: 解析类型列表，支持嵌套 `Foo<Bar<T>>`（v0.09 简化版只支持单层）
    /// v0.10 强化: 完全泛型类型系统（method-level generics + monomorphize）
    /// 当前实现: 把嵌套泛型展平成字符串，如 `Foo<Bar<number>>` → `["Foo<Bar<number>>"]`
    fn parse_type_list(&mut self) -> Vec<String> {
        let mut types = Vec::new();
        self.advance(); // 消耗 `<`
        loop {
            let tn = self.parse_type_name_recursive();
            types.push(tn);
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        self.consume(&TokenType::Greater, "Expected '>' after type list");
        types
    }

    /// v0.09 完整版: 解析类型名（支持嵌套泛型 Foo<Bar<T>>）
    ///   返回完整字符串，如 `Boxed<number>` 或 `number`
    fn parse_type_name_recursive(&mut self) -> String {
        let tn = self.consume_identifier("Expected type name");
        // 检查是否带泛型参数 <...>（任意 `<` 后是 IDENT 的情况视为泛型）
        if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            let generics = self.parse_type_list();
            format!("{}<{}>", tn, generics.join(","))
        } else {
            tn
        }
    }

    /// v0.09: 解析类型列表为字符串（用于 type hint 如 `dyn:Container<number>`）
    fn parse_type_list_to_string(&mut self) -> String {
        let types = self.parse_type_list();
        if types.is_empty() {
            String::new()
        } else {
            format!("<{}>", types.join(","))
        }
    }

    /// v0.09: 解析 where 子句 `where T: Bound, U: Bound2`
    /// 调用约定: 调用方已通过 `match_token(&[TokenType::Where])` 消耗 `where`,
    ///           进入时 peek 是子句第一个 token（`T` 之类 IDENT）
    fn parse_where_clause(&mut self) -> Vec<crate::ast::GenericParam> {
        use crate::ast::GenericParam;
        // 重要: 调用方已经消耗了 `where`，不要再次 advance
        let mut clauses = Vec::new();
        loop {
            let pspan = self.span_of_previous_keyword();
            let pname = self.consume_identifier("Expected where clause param name");
            let pbound = if self.match_token(&[TokenType::Colon]) {
                Some(self.consume_identifier("Expected bound trait name after ':'"))
            } else {
                None
            };
            clauses.push(GenericParam {
                name: pname,
                bound: pbound,
                span: pspan,
            });
            if !self.match_token(&[TokenType::Comma]) {
                break;
            }
        }
        clauses
    }

    fn previous_op(&self) -> BinaryOp {
        match self.previous().unwrap().token_type {
            TokenType::Plus => BinaryOp::Add,
            TokenType::Minus => BinaryOp::Sub,
            TokenType::Star => BinaryOp::Mul,
            TokenType::Slash => BinaryOp::Div,
            TokenType::Percent => BinaryOp::Mod,
            TokenType::Equal => BinaryOp::Equal,
            TokenType::NotEqual => BinaryOp::NotEqual,
            TokenType::Greater => BinaryOp::Greater,
            TokenType::Less => BinaryOp::Less,
            TokenType::GreaterEqual => BinaryOp::GreaterEqual,
            TokenType::LessEqual => BinaryOp::LessEqual,
            _ => panic!("Not a binary operator"),
        }
    }

    fn consume(&mut self, token_type: &TokenType, message: &str) -> &Token {
        if self.check(token_type) {
            return self.advance().unwrap();
        }
        panic!(
            "{} at line {}",
            message,
            self.peek().map(|t| t.line).unwrap_or(0)
        )
    }

    fn consume_identifier(&mut self, message: &str) -> String {
        if let Some(Token {
            token_type: TokenType::Identifier(name),
            ..
        }) = self.peek()
        {
            let name = name.clone();
            self.advance();
            return name;
        }
        panic!(
            "{} at line {}",
            message,
            self.peek().map(|t| t.line).unwrap_or(0)
        )
    }

    /// v11: 接受普通 Identifier **或** 复合关键字(read_bytes/write_bytes)作为方法名。
    /// 用 lexer 关键字化后,这些方法名不再是 TokenType::Identifier,旧 consume_identifier 会 panic。
    /// 这里把它们还原为字面字符串,语义不变(运行时再分发)。
    fn consume_method_name(&mut self, message: &str) -> String {
        // v0.07.1: 接受任何 token（包括 Identifier + keyword）作为方法名
        // consume_identifier 只接受 TokenType::Identifier，但像 "route"/"mcp" 被词法
        // 关键字化后不再是 Identifier——这里统一用 advance 拿到它然后返回字符串表示
        match self.peek() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                let n = name.clone();
                self.advance();
                n
            }
            Some(tok) => {
                // 取 token 的字符串表示（lexer 已经把关键字映射好了）
                let name = match &tok.token_type {
                    TokenType::Route => "route".to_string(),
                    TokenType::ReadBytes => "read_bytes".to_string(),
                    TokenType::WriteBytes => "write_bytes".to_string(),
                    TokenType::Read => "read".to_string(),
                    TokenType::Write => "write".to_string(),
                    TokenType::Append => "append".to_string(),
                    TokenType::Let => "let".to_string(),
                    TokenType::Task => "task".to_string(),
                    TokenType::If => "if".to_string(),
                    TokenType::For => "for".to_string(),
                    TokenType::In => "in".to_string(),
                    TokenType::Import => "import".to_string(),
                    TokenType::As => "as".to_string(),
                    TokenType::Do => "do".to_string(),
                    TokenType::WithKeyword => "with".to_string(),
                    TokenType::Save => "save".to_string(),
                    TokenType::Load => "load".to_string(),
                    TokenType::Fn => "fn".to_string(),
                    TokenType::Into => "into".to_string(),
                    TokenType::Stream => "stream".to_string(),
                    TokenType::Tool => "tool".to_string(),
                    TokenType::Break => "break".to_string(),
                    TokenType::Continue => "continue".to_string(),
                    TokenType::Observe => "observe".to_string(),
                    TokenType::Span => "span".to_string(),
                    TokenType::Tags => "tags".to_string(),
                    TokenType::Record => "record".to_string(),
                    TokenType::Trace => "trace".to_string(),
                    TokenType::Metrics => "metrics".to_string(),
                    TokenType::Otel => "otel".to_string(),
                    TokenType::Export => "export".to_string(),
                    TokenType::Parallel => "parallel".to_string(),
                    _ => {
                        // 检查是否是 Identifier 类 token (被词法降级为 Ident 的关键字)
                        panic!(
                            "{} at line {}: unexpected token {:?}",
                            message, tok.line, tok.token_type
                        )
                    }
                };
                self.advance();
                name
            }
            None => panic!("{} at end of input", message),
        }
    }

    // ===================================================================
    // v0.04.0: AI 原语语句解析
    // ===================================================================

    /// `with model = "...", budget = 1000 do ... end`
    fn with_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let mut bindings = Vec::new();
        // 第一个 binding 必需
        let key = self
            .consume_identifier("Expected binding key in 'with' (e.g. model, budget, temperature)");
        self.consume(&TokenType::Assign, "Expected '=' after 'with' binding key");
        let value = self.expression();
        bindings.push((key, value));
        // 后续 binding
        while self.match_token(&[TokenType::Comma]) {
            let key = self.consume_identifier("Expected binding key in 'with'");
            self.consume(&TokenType::Assign, "Expected '=' after 'with' binding key");
            let value = self.expression();
            bindings.push((key, value));
        }
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after with block");
        Stmt::With {
            bindings,
            body,
            span,
        }
    }

    /// `stream <expr> as <var> do ... end`
    fn stream_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let prompt = self.expression();
        self.consume(&TokenType::As, "Expected 'as' after stream expression (v0.04.0 syntax: stream <expr> as <var> do ... end)");
        let var = self.consume_identifier("Expected variable name after 'as'");
        self.consume(&TokenType::Do, "Expected 'do' after variable name");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after stream block");
        Stmt::StreamFor {
            prompt,
            var,
            body,
            span,
        }
    }

    /// `tool name(params): return_type do ... end`
    fn tool_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected tool name");
        let params = if self.match_token(&[TokenType::LParen]) {
            let mut p = Vec::new();
            if !self.check(&TokenType::RParen) {
                p.push(self.typed_parameter());
                while self.match_token(&[TokenType::Comma]) {
                    p.push(self.typed_parameter());
                }
            }
            self.consume(&TokenType::RParen, "Expected ')' after tool params");
            p
        } else {
            Vec::new()
        };
        let return_type = if self.match_token(&[TokenType::Colon]) {
            Some(self.consume_identifier("Expected return type after ':'"))
        } else {
            None
        };
        self.consume(&TokenType::Do, "Expected 'do' after tool signature");
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) {
                self.advance();
                continue;
            }
            if let Some(stmt) = self.declaration() {
                body.push(stmt);
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after tool body");
        let exported = false; // v0.04.1: export tool 跟进
        Stmt::ToolDef {
            name,
            params,
            return_type,
            body,
            exported,
            span,
        }
    }

    fn break_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        Stmt::Break { span }
    }

    fn continue_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        Stmt::Continue { span }
    }

    /// 把 parse_format_string 生成的 StringConcat 左结合链展平为 Vec<Expr>
    fn flatten_prompt_parts(&self, left: Expr, right: Expr) -> Vec<Expr> {
        let mut out = Vec::new();
        fn collect(e: Expr, out: &mut Vec<Expr>) {
            match e {
                Expr::Binary {
                    left,
                    op: BinaryOp::Add,
                    right,
                    ..
                } => {
                    collect(*left, out);
                    collect(*right, out);
                }
                other => out.push(other),
            }
        }
        collect(left, &mut out);
        collect(right, &mut out);
        out
    }

    // ===================================================================
    // v0.04: 云服务原生 statement 解析
    // ===================================================================

    /// `route <name>: <expr>`
    fn route_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected route name after 'route'");
        self.consume(&TokenType::Colon, "Expected ':' after route name");
        let target = self.expression();
        Stmt::Route { name, target, span }
    }

    /// `observe <config> do body end`
    /// config: trace / metrics / otel endpoint "<url>"
    fn observe_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let config = if self.match_token(&[TokenType::Trace]) {
            ObserveConfig::Trace
        } else if self.match_token(&[TokenType::Metrics]) {
            ObserveConfig::Metrics
        } else if self.match_token(&[TokenType::Otel]) {
            // otel endpoint "<url>"
            self.consume_identifier("Expected 'endpoint' after 'observe otel'");
            // 期望 string
            let endpoint = if let Some(Token {
                token_type: TokenType::String(s),
                ..
            }) = self.peek().cloned()
            {
                self.advance();
                Expr::Literal(Literal::String(s, Span::default()))
            } else {
                panic!("Expected string endpoint");
            };
            ObserveConfig::Otel { endpoint }
        } else {
            panic!("Expected trace / metrics / otel after 'observe'");
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let body = if self.match_token(&[TokenType::Do]) {
            let mut b = Vec::new();
            while !self.check(&TokenType::End) && !self.is_at_end() {
                if self.check(&TokenType::Newline) {
                    self.advance();
                    continue;
                }
                if let Some(stmt) = self.declaration() {
                    b.push(stmt);
                }
            }
            self.consume(&TokenType::End, "Expected 'end' after observe body");
            b
        } else {
            // 没有 do: body 为空, 但 end 还是要消费
            self.consume(&TokenType::End, "Expected 'end' after observe block");
            Vec::new()
        };
        Stmt::Observe { config, body, span }
    }

    /// `span "<name>" tags {..} do body end`
    fn span_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = match self.advance() {
            Some(Token {
                token_type: TokenType::String(s),
                ..
            }) => s.clone(),
            _ => panic!("Expected span name string"),
        };
        // 可选 tags
        let attributes = if self.match_token(&[TokenType::Tags]) {
            self.consume(&TokenType::LBrace, "Expected '{' after 'tags'");
            let mut attrs = Vec::new();
            // 解析 k: v, k: v, ...
            loop {
                let key = match self.advance() {
                    Some(Token {
                        token_type: TokenType::Identifier(n),
                        ..
                    }) => n.clone(),
                    Some(Token {
                        token_type: TokenType::String(s),
                        ..
                    }) => s.clone(),
                    _ => panic!("Expected tag key"),
                };
                self.consume(&TokenType::Colon, "Expected ':' after tag key");
                let val = self.expression();
                attrs.push((key, val));
                if !self.match_token(&[TokenType::Comma]) {
                    break;
                }
            }
            self.consume(&TokenType::RBrace, "Expected '}' after tags");
            attrs
        } else {
            Vec::new()
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let body = if self.match_token(&[TokenType::Do]) {
            let mut b = Vec::new();
            while !self.check(&TokenType::End) && !self.is_at_end() {
                if self.check(&TokenType::Newline) {
                    self.advance();
                    continue;
                }
                if let Some(stmt) = self.declaration() {
                    b.push(stmt);
                }
            }
            self.consume(&TokenType::End, "Expected 'end' after span body");
            b
        } else {
            Vec::new()
        };
        Stmt::Span {
            name,
            attributes,
            body,
            span,
        }
    }

    /// `record_tokens(<input>, <output>)` 顶层语句
    /// v0.04.0 终态补: 显式 token 计数（RFC §2.4 / §3.3）
    /// 词法把 `record_tokens` 当作普通 Identifier; statement() 在分派前用 match_identifier
    /// 消耗该标识符后调用本函数。
    fn record_tokens_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        self.consume(&TokenType::LParen, "Expected '(' after 'record_tokens'");
        let input = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' between record_tokens args");
        let output = self.expression();
        self.consume(&TokenType::RParen, "Expected ')' after record_tokens args");
        Stmt::RecordTokens {
            input,
            output,
            span,
        }
    }

    /// `ai_model(<model>, [temperature: T], [max_tokens: N], [system: "..."])`
    /// v0.04补: 路由元数据表达式（RFC §2.3）
    /// 在 LParen 已被消耗后调用; 结束时需消耗 RParen
    fn parse_ai_model_call(&mut self, span: Span) -> Expr {
        // 第一参数必为 model 名字符串
        if self.check(&TokenType::RParen) {
            panic!("ai_model: missing model name argument");
        }
        let model = self.expression();
        // 解析可选 keyword args: temperature: / max_tokens: / system:
        let mut temperature = None;
        let mut max_tokens = None;
        let mut system = None;
        while self.match_token(&[TokenType::Comma]) {
            // 期望 IDENT: expr
            let key = match self.advance() {
                Some(Token {
                    token_type: TokenType::Identifier(n),
                    ..
                }) => n.clone(),
                _ => panic!("ai_model: expected keyword name (temperature/max_tokens/system)"),
            };
            self.consume(&TokenType::Colon, "ai_model: expected ':' after keyword");
            let val = self.expression();
            match key.as_str() {
                "temperature" => temperature = Some(Box::new(val)),
                "max_tokens" => max_tokens = Some(Box::new(val)),
                "system" => system = Some(Box::new(val)),
                other => panic!(
                    "ai_model: unknown keyword '{}' (valid: temperature, max_tokens, system)",
                    other
                ),
            }
        }
        self.consume(&TokenType::RParen, "Expected ')' after ai_model args");
        Expr::AiModelCall {
            model: Box::new(model),
            temperature,
            max_tokens,
            system,
            span,
        }
    }

    // ===================================================================
    // v0.08: trait 系统解析
    // ===================================================================

    /// `trait Name ... method_signatures ... end`
    /// `trait Name [: Parent1, Parent2, ...] ... method_signatures ... end`
    fn parse_trait_def(&mut self, _exported: bool) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected trait name");
        // v0.09: 解析 trait 自身的泛型参数 `<T>` / `<T: Bound>` / `<T, U, V>`
        //   复用 lexer 的 Less/Greater token（与比较运算符同）
        //   消歧规则: `<` 后是 IDENT → 泛型开始
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_generic_params()
        } else {
            vec![]
        };
        // v0.08.4: 可选 `:` 后跟父 trait 列表（用 `,` 分隔）
        let parents = if self.match_token(&[TokenType::Colon]) {
            let mut ps = vec![self.consume_identifier("Expected parent trait name after ':'")];
            while self.match_token(&[TokenType::Comma]) {
                ps.push(self.consume_identifier("Expected parent trait name"));
            }
            ps
        } else {
            vec![]
        };
        // v0.09 完整版: trait 也支持 where 子句（`trait Foo<T> where T: Bar ...`）
        let trait_where = if self.match_token(&[TokenType::Where]) {
            self.parse_where_clause()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut methods = Vec::new();
        loop {
            // 跳过方法之间的 Newline
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            let mspan = self.span_of_previous_keyword();
            self.consume(&TokenType::Fn, "Expected 'fn' in trait method");
            let mname = self.consume_identifier("Expected method name in trait");
            self.consume(&TokenType::LParen, "Expected '(' after trait method name");
            let params = if self.check(&TokenType::RParen) {
                vec![]
            } else {
                self.parameters()
            };
            self.consume(&TokenType::RParen, "Expected ')' after trait method params");
            // v0.08.1: 支持 `-> RetType` 语法（也兼容 `: RetType` 旧风格）
            let return_type = if self.match_token(&[TokenType::Arrow]) {
                Some(self.consume_identifier("Expected return type after '->'"))
            } else if self.check(&TokenType::Colon) {
                self.advance();
                Some(self.consume_identifier("Expected return type after ':'"))
            } else {
                None
            };
            // v0.08.3: 默认实现 `= expr`（trait 内 fn 直接给实现，impl 可省略）
            // v0.08.5 任务 2: 增加 `do ... end` 块语法（多语句默认实现）
            let body = if self.match_token(&[TokenType::Assign]) {
                let expr = self.expression();
                vec![Stmt::Expr(expr)]
            } else if self.match_token(&[TokenType::Do]) {
                let mut body = Vec::new();
                while !self.check(&TokenType::End) && !self.is_at_end() {
                    if self.check(&TokenType::Newline) {
                        self.advance();
                        continue;
                    }
                    if let Some(stmt) = self.declaration() {
                        body.push(stmt);
                    }
                }
                self.consume(&TokenType::End, "Expected 'end' after trait method body");
                body
            } else {
                vec![]
            };
            // 跳到本行末尾
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            // v0.09: trait method 自己的 generics (暂留空，Step 2 加解析)
            methods.push(TraitMethod {
                name: mname,
                params,
                return_type,
                body,
                generics: vec![],
                span: mspan,
            });
        }
        self.consume(&TokenType::End, "Expected 'end' after trait body");
        Stmt::TraitDef {
            name,
            generics,
            parents,
            trait_where, // v0.09 完整版: trait where 子句
            methods,
            span,
        }
    }

    /// `impl<T> TraitName<...> for TypeName<...> where T: Bound ... method_bodies ... end`
    /// v0.09: 支持 impl generics + trait_generics + for_generics + where clause
    fn parse_impl_def(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();

        // v0.09: impl 自身的泛型参数 `impl<T, U>`
        let generics = if self.check(&TokenType::Less) && self.peek_after_less_is_ident() {
            self.parse_generic_params()
        } else {
            vec![]
        };

        let trait_name = self.consume_identifier("Expected trait name after 'impl'");

        // v0.09: trait 的泛型参数 `Foo<T>`（任意 IDENT 后都视为类型列表）
        let trait_generics = if self.check(&TokenType::Less) {
            self.parse_type_list()
        } else {
            vec![]
        };

        self.consume(&TokenType::For, "Expected 'for' after trait name in impl");
        let for_type = self.consume_identifier("Expected type name after 'for'");

        // v0.09: for_type 的泛型参数 `Bar<U>`
        let for_generics = if self.check(&TokenType::Less) {
            self.parse_type_list()
        } else {
            vec![]
        };

        // v0.09: where 子句 `where T: Bound, U: Bound2`
        let where_clause = if self.match_token(&[TokenType::Where]) {
            self.parse_where_clause()
        } else {
            vec![]
        };
        while self.check(&TokenType::Newline) {
            self.advance();
        }
        let mut methods = Vec::new();
        loop {
            // 跳过方法之间的 Newline
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            if self.check(&TokenType::End) || self.is_at_end() {
                break;
            }
            let mspan = self.span_of_previous_keyword();
            self.consume(&TokenType::Fn, "Expected 'fn' in impl method");
            let mname = self.consume_identifier("Expected method name in impl");
            self.consume(&TokenType::LParen, "Expected '(' after impl method name");
            let params = if self.check(&TokenType::RParen) {
                vec![]
            } else {
                self.parameters()
            };
            self.consume(&TokenType::RParen, "Expected ')' after impl method params");
            // v0.08.1: 支持 `-> RetType` 和 `: RetType` 两种
            let return_type = if self.match_token(&[TokenType::Arrow]) {
                Some(self.consume_identifier("Expected return type after '->'"))
            } else if self.check(&TokenType::Colon) {
                self.advance();
                Some(self.consume_identifier("Expected return type after ':'"))
            } else {
                None
            };
            // body: = expr 或 do ... end 块
            // v0.09 修复: `= do ... end` 也应走 do 块分支
            let body = if self.match_token(&[TokenType::Assign]) {
                if self.check(&TokenType::Do) {
                    self.advance();
                    let mut body = Vec::new();
                    while !self.check(&TokenType::End) && !self.is_at_end() {
                        if self.check(&TokenType::Newline) {
                            self.advance();
                            continue;
                        }
                        if let Some(stmt) = self.declaration() {
                            body.push(stmt);
                        }
                    }
                    self.consume(&TokenType::End, "Expected 'end' after impl method body");
                    body
                } else {
                    let expr = self.expression();
                    vec![Stmt::Expr(expr)]
                }
            } else if self.match_token(&[TokenType::Do]) {
                let mut body = Vec::new();
                while !self.check(&TokenType::End) && !self.is_at_end() {
                    if self.check(&TokenType::Newline) {
                        self.advance();
                        continue;
                    }
                    if let Some(stmt) = self.declaration() {
                        body.push(stmt);
                    }
                }
                self.consume(&TokenType::End, "Expected 'end' after impl method body");
                body
            } else {
                vec![]
            };
            while self.check(&TokenType::Newline) {
                self.advance();
            }
            methods.push(FnDef {
                name: mname,
                params,
                return_type,
                body,
                span: mspan,
            });
        }
        self.consume(&TokenType::End, "Expected 'end' after impl block");
        Stmt::ImplDef {
            generics,
            trait_generics,
            trait_name,
            for_type,
            for_generics,
            where_clause,
            methods,
            span,
        }
    }
}

/// 检查字符串是否包含格式插值（{var} 或 {expr}）。
/// 只在 { 后紧跟字母/下划线时才视为插值，避免误触发 JSON 字符串。
fn has_format_interpolation(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if next == '{' {
                i += 2; // skip {{ (literal brace)
                continue;
            }
            if next.is_ascii_alphabetic() || next == '_' {
                return true; // {var...} — format interpolation
            }
        }
        i += 1;
    }
    false
}
