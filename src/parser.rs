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
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { statements.push(stmt); }
        }
        statements
    }

    fn declaration(&mut self) -> Option<Stmt> {
        let exported = self.match_token(&[TokenType::Export]);
        if self.match_token(&[TokenType::Let]) {
            Some(self.let_declaration(exported))
        } else if self.match_token(&[TokenType::Task]) {
            Some(self.task_declaration(exported))
        } else if exported {
            panic!("Expected 'let' or 'task' after 'export' at line {}", self.peek().map(|t| t.line).unwrap_or(0))
        } else {
            self.statement()
        }
    }

    fn let_declaration(&mut self, exported: bool) -> Stmt {
        // 'let' 关键字位置（"previous" 是刚消耗的 let token）
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected variable name after 'let'");
        let type_hint = if self.match_token(&[TokenType::Colon]) {
            Some(self.consume_identifier("Expected type name after ':'"))
        } else {
            None
        };
        self.consume(&TokenType::Assign, "Expected '=' after variable name/type");
        let init = self.expression();
        Stmt::Let { name, type_hint, init, exported, span }
    }

    fn task_declaration(&mut self, exported: bool) -> Stmt {
        let span = self.span_of_previous_keyword();
        let name = self.consume_identifier("Expected task name");
        self.consume(&TokenType::LParen, "Expected '(' after task name");
        let params = if self.check(&TokenType::RParen) { vec![] } else { self.parameters() };
        self.consume(&TokenType::RParen, "Expected ')' after parameters");
        // v11: 可选返回类型 hint —— `): T` 或 `) : T`
        let return_type = if self.check(&TokenType::Colon) {
            self.advance();
            Some(self.consume_identifier("Expected return type after ':'"))
        } else {
            None
        };
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { body.push(stmt); }
        }
        self.consume(&TokenType::End, "Expected 'end' after task body");
        Stmt::TaskDef { name, params, return_type, body, exported, span }
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

    fn statement(&mut self) -> Option<Stmt> {
        if self.match_token(&[TokenType::If]) { Some(self.if_statement()) }
        else if self.match_token(&[TokenType::For]) { Some(self.for_statement()) }
        else if self.match_token(&[TokenType::Try]) { Some(self.try_statement()) }
        else if self.match_token(&[TokenType::Import]) { Some(self.import_statement()) }
        else if self.match_token(&[TokenType::Parallel]) { Some(self.parallel_statement()) }
        else if self.match_token(&[TokenType::Save]) { Some(self.save_statement()) }
        else if self.match_token(&[TokenType::Load]) { Some(self.load_statement()) }
        else if self.match_token(&[TokenType::ReadBytes]) { Some(self.read_bytes_statement()) }
        else if self.match_token(&[TokenType::WriteBytes]) { Some(self.write_bytes_statement()) }
        else if self.match_token(&[TokenType::Read]) { Some(self.read_statement()) }
        else if self.match_token(&[TokenType::Append]) { Some(self.append_statement()) }
        else if self.match_token(&[TokenType::Write]) { Some(self.write_statement()) }
        else if self.match_token(&[TokenType::Return]) { Some(self.return_statement()) }
        else if self.check_index_assignment() { Some(self.index_assignment()) }
        else if self.check_assignment() { Some(self.assignment_statement()) }
        else { self.expression_statement() }
    }

    fn import_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = match self.peek() {
            Some(Token { token_type: TokenType::String(s), .. }) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => panic!("Expected string path after 'import' at line {}", self.peek().map(|t| t.line).unwrap_or(0)),
        };
        Stmt::Import { path, span }
    }

    fn parallel_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut stmts = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { stmts.push(stmt); }
        }
        self.consume(&TokenType::End, "Expected 'end' after parallel block");
        Stmt::Parallel { stmts, span }
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
        Stmt::WriteFile { path, content, span }
    }

    fn append_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let path = self.expression();
        self.consume(&TokenType::Comma, "Expected ',' after path in append");
        let content = self.expression();
        Stmt::AppendFile { path, content, span }
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
        Stmt::WriteBytesFile { path, content, span }
    }

    fn if_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let condition = self.expression();
        self.consume(&TokenType::Then, "Expected 'then' after if condition");
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut then_branch = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { then_branch.push(stmt); }
        }
        self.consume(&TokenType::End, "Expected 'end' after if body");
        Stmt::If { condition, then_branch, span }
    }

    fn for_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let var = self.consume_identifier("Expected loop variable after 'for'");
        // v11: 可选 `for x: T in ...`
        let var_type = if self.match_token(&[TokenType::Colon]) {
            Some(self.consume_identifier("Expected type after ':' in for variable"))
        } else { None };
        self.consume(&TokenType::In, "Expected 'in' after loop variable");
        let iterable = self.expression();
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { body.push(stmt); }
        }
        self.consume(&TokenType::End, "Expected 'end' after for body");
        Stmt::For { var, var_type, iterable, body, span }
    }

    fn try_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut try_block = Vec::new();
        while !self.check(&TokenType::Catch) && !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { try_block.push(stmt); }
        }
        self.consume(&TokenType::Catch, "Expected 'catch' after try block");
        let catch_var = self.consume_identifier("Expected variable name after 'catch'");
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut catch_block = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { catch_block.push(stmt); }
        }
        self.consume(&TokenType::End, "Expected 'end' after catch block");
        Stmt::Try { try_block, catch_var, catch_block, span }
    }

    fn return_statement(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let value = if self.check(&TokenType::Newline) || self.check(&TokenType::End) || self.is_at_end() {
            None
        } else {
            Some(self.expression())
        };
        Stmt::Return { value, span }
    }

    fn check_index_assignment(&mut self) -> bool {
        let save = self.current;
        let result = if let Some(Token { token_type: TokenType::Identifier(_), .. }) = self.peek() {
            self.advance();
            let r = self.match_token(&[TokenType::LBracket]);
            r
        } else { false };
        self.current = save;
        result
    }

    fn index_assignment(&mut self) -> Stmt {
        let span = self.span_of_previous_keyword();
        let object = self.expression();
        self.consume(&TokenType::LBracket, "Expected '['");
        let index = self.expression();
        self.consume(&TokenType::RBracket, "Expected ']'");
        self.consume(&TokenType::Assign, "Expected '=' after index");
        let value = self.expression();
        Stmt::IndexAssign { object, index, value, span }
    }

    fn check_assignment(&self) -> bool {
        if let Some(Token { token_type: TokenType::Identifier(_), .. }) = self.peek() {
            if let Some(Token { token_type: TokenType::Assign, .. }) = self.peek_next() {
                return true;
            }
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
            while self.check(&TokenType::Newline) { self.advance(); }
            self.match_token(&[TokenType::Pipe])
        } {
            while self.check(&TokenType::Newline) { self.advance(); }
            let right = self.equality();
            expr = Expr::Pipe { left: Box::new(expr), right: Box::new(right), span: Span::default() };
        }
        expr
    }

    fn equality(&mut self) -> Expr {
        let mut expr = self.comparison();
        while self.match_token(&[TokenType::Equal, TokenType::NotEqual]) {
            let op = self.previous_op();
            let right = self.comparison();
            expr = Expr::Binary { left: Box::new(expr), op, right: Box::new(right), span: Span::default() };
        }
        expr
    }

    fn comparison(&mut self) -> Expr {
        let mut expr = self.term();
        while self.match_token(&[TokenType::Greater, TokenType::GreaterEqual, TokenType::Less, TokenType::LessEqual]) {
            let op = self.previous_op();
            let right = self.term();
            expr = Expr::Binary { left: Box::new(expr), op, right: Box::new(right), span: Span::default() };
        }
        expr
    }

    fn term(&mut self) -> Expr {
        let mut expr = self.factor();
        while self.match_token(&[TokenType::Plus, TokenType::Minus]) {
            let op = self.previous_op();
            let right = self.factor();
            expr = Expr::Binary { left: Box::new(expr), op, right: Box::new(right), span: Span::default() };
        }
        expr
    }

    fn factor(&mut self) -> Expr {
        let mut expr = self.unary();
        while self.match_token(&[TokenType::Star, TokenType::Slash, TokenType::Percent]) {
            let op = self.previous_op();
            let right = self.unary();
            expr = Expr::Binary { left: Box::new(expr), op, right: Box::new(right), span: Span::default() };
        }
        expr
    }

    fn unary(&mut self) -> Expr {
        if self.match_token(&[TokenType::Minus]) {
            let op = self.previous_op();
            let right = self.unary();
            Expr::Binary { left: Box::new(Expr::Literal(Literal::Number(0.0, Span::default()))), op, right: Box::new(right), span: Span::default() }
        } else if self.match_token(&[TokenType::Match]) {
            self.match_expression()
        } else {
            self.call()
        }
    }

    fn match_expression(&mut self) -> Expr {
        let expr = self.expression();
        self.consume(&TokenType::With, "Expected 'with' after match expression");
        let mut arms = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(pattern) = self.pattern() {
                self.consume(&TokenType::Arrow, "Expected '->' after pattern");
                let arm_expr = self.expression();
                arms.push((pattern, Box::new(arm_expr)));
                while self.check(&TokenType::Newline) { self.advance(); }
            } else {
                break;
            }
        }
        self.consume(&TokenType::End, "Expected 'end' after match arms");
        Expr::Match { expr: Box::new(expr), arms, span: Span::default() }
    }

    fn pattern(&mut self) -> Option<Pattern> {
        if let Some(Token { token_type: TokenType::Identifier(name), .. }) = self.peek() {
            if name == "end" {
                return None;
            }
            if name == "_" {
                self.advance();
                return Some(Pattern::Wildcard);
            }
        }

        if self.match_token(&[TokenType::True]) { Some(Pattern::Literal(Literal::Bool(true, Span::default()))) }
        else if self.match_token(&[TokenType::False]) { Some(Pattern::Literal(Literal::Bool(false, Span::default()))) }
        else if self.match_token(&[TokenType::Nil]) { Some(Pattern::Literal(Literal::Nil(Span::default()))) }
        else if let Some(Token { token_type: TokenType::Number(n), .. }) = self.peek().cloned() {
            self.advance();
            Some(Pattern::Literal(Literal::Number(n, Span::default())))
        }
        else if let Some(Token { token_type: TokenType::String(s), .. }) = self.peek().cloned() {
            self.advance();
            Some(Pattern::Literal(Literal::String(s, Span::default())))
        }
        else if self.match_token(&[TokenType::LBracket]) {
            let mut items = Vec::new();
            if !self.check(&TokenType::RBracket) {
                if let Some(p) = self.pattern() { items.push(p); }
                while self.match_token(&[TokenType::Comma]) {
                    if let Some(p) = self.pattern() { items.push(p); }
                }
            }
            self.consume(&TokenType::RBracket, "Expected ']' after list pattern");
            Some(Pattern::List(items))
        }
        else if self.match_token(&[TokenType::LBrace]) {
            let mut entries = Vec::new();
            if !self.check(&TokenType::RBrace) {
                entries.push(self.dict_pattern_entry());
                while self.match_token(&[TokenType::Comma]) { entries.push(self.dict_pattern_entry()); }
            }
            self.consume(&TokenType::RBrace, "Expected '}' after dict pattern");
            Some(Pattern::Dict(entries))
        }
        else if let Some(Token { token_type: TokenType::Identifier(name), .. }) = self.peek().cloned() {
            self.advance();
            Some(Pattern::Variable(name))
        }
        else {
            None
        }
    }

    fn dict_pattern_entry(&mut self) -> (String, Pattern) {
        let key = match self.peek() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
                let name = name.clone();
                self.advance();
                name
            }
            Some(Token { token_type: TokenType::String(s), .. }) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => panic!("Expected identifier or string as dict pattern key at line {}", self.peek().map(|t| t.line).unwrap_or(0)),
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
                let args = if self.check(&TokenType::RParen) { vec![] } else { self.arguments() };
                self.consume(&TokenType::RParen, "Expected ')' after arguments");
                if let Expr::Variable(name, _) = expr {
                    expr = Expr::Call { callee: name, args, span };
                } else {
                    panic!("Can only call functions by name in Mora v1");
                }
            } else if self.match_token(&[TokenType::LBracket]) {
                let span = self.span_of_previous_keyword();
                let index = self.expression();
                self.consume(&TokenType::RBracket, "Expected ']' after index");
                expr = Expr::Index { object: Box::new(expr), index: Box::new(index), span };
            } else if self.match_token(&[TokenType::Dot]) {
                let span = self.span_of_previous_keyword();
                let method = self.consume_method_name("Expected method name after '.'");
                let args = if self.match_token(&[TokenType::LParen]) {
                    let a = if self.check(&TokenType::RParen) { vec![] } else { self.arguments() };
                    self.consume(&TokenType::RParen, "Expected ')' after arguments");
                    a
                } else { vec![] };
                expr = Expr::MethodCall { object: Box::new(expr), method, args, span };
            } else {
                break;
            }
        }
        expr
    }

    fn primary(&mut self) -> Expr {
        if self.match_token(&[TokenType::True]) {
            let span = self.span_of_previous_keyword();
            Expr::Literal(Literal::Bool(true, span))
        }
        else if self.match_token(&[TokenType::False]) {
            let span = self.span_of_previous_keyword();
            Expr::Literal(Literal::Bool(false, span))
        }
        else if self.match_token(&[TokenType::Nil]) {
            let span = self.span_of_previous_keyword();
            Expr::Literal(Literal::Nil(span))
        }
        else if self.match_token(&[TokenType::Fn]) { self.closure_expression() }
        else if let Some(Token { token_type: TokenType::Number(n), line, column, .. }) = self.peek().cloned() {
            self.advance();
            Expr::Literal(Literal::Number(n, Span::new(line, column)))
        }
        else if let Some(Token { token_type: TokenType::String(s), line, column, .. }) = self.peek().cloned() {
            self.advance();
            // 只在 { 后跟标识符字符时才触发格式字符串解析
            // 避免误触发 JSON 字符串如 "{"name":"hello"}"
            if has_format_interpolation(&s) {
                self.parse_format_string(&s)
            } else {
                Expr::Literal(Literal::String(s, Span::new(line, column)))
            }
        }
        else if self.match_token(&[TokenType::LBracket]) {
            let span = self.span_of_previous_keyword();
            let mut items = Vec::new();
            if !self.check(&TokenType::RBracket) {
                items.push(Box::new(self.expression()));
                while self.match_token(&[TokenType::Comma]) { items.push(Box::new(self.expression())); }
            }
            self.consume(&TokenType::RBracket, "Expected ']' after list");
            Expr::Literal(Literal::List(items, span))
        }
        else if self.match_token(&[TokenType::LBrace]) {
            let span = self.span_of_previous_keyword();
            let mut entries = Vec::new();
            while self.check(&TokenType::Newline) { self.advance(); }
            if !self.check(&TokenType::RBrace) {
                let (k, v) = self.dict_entry();
                entries.push((k, Box::new(v)));
                while self.match_token(&[TokenType::Comma]) {
                    while self.check(&TokenType::Newline) { self.advance(); }
                    if self.check(&TokenType::RBrace) { break; }
                    let (k, v) = self.dict_entry();
                    entries.push((k, Box::new(v)));
                }
            }
            self.consume(&TokenType::RBrace, "Expected '}' after dict");
            Expr::Literal(Literal::Dict(entries, span))
        }
        else if let Some(Token { token_type: TokenType::Identifier(name), line, column, .. }) = self.peek().cloned() {
            self.advance();
            Expr::Variable(name, Span::new(line, column))
        }
        else if self.match_token(&[TokenType::LParen]) {
            let span = self.span_of_previous_keyword();
            let expr = self.expression();
            self.consume(&TokenType::RParen, "Expected ')' after expression");
            Expr::Grouping(Box::new(expr), span)
        }
        else {
            panic!("Unexpected token: {:?} at line {}", self.peek(), self.peek().map(|t| t.line).unwrap_or(0))
        }
    }

    fn closure_expression(&mut self) -> Expr {
        let span = self.span_of_previous_keyword();
        self.consume(&TokenType::LParen, "Expected '(' after 'fn'");
        let params = if self.check(&TokenType::RParen) { vec![] } else { self.parameters() };
        self.consume(&TokenType::RParen, "Expected ')' after parameters");
        // v11: 可选返回类型 hint —— `): T`
        let return_type = if self.check(&TokenType::Colon) {
            self.advance();
            Some(self.consume_identifier("Expected return type after ':' in closure"))
        } else { None };
        while self.check(&TokenType::Newline) { self.advance(); }
        let mut body = Vec::new();
        while !self.check(&TokenType::End) && !self.is_at_end() {
            if self.check(&TokenType::Newline) { self.advance(); continue; }
            if let Some(stmt) = self.declaration() { body.push(stmt); }
        }
        self.consume(&TokenType::End, "Expected 'end' after closure body");
        Expr::Closure { params, return_type, body, span }
    }

    fn dict_entry(&mut self) -> (String, Expr) {
        let key = match self.peek() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
                let name = name.clone();
                self.advance();
                name
            }
            Some(Token { token_type: TokenType::String(s), .. }) => {
                let s = s.clone();
                self.advance();
                s
            }
            _ => panic!("Expected identifier or string as dict key at line {}", self.peek().map(|t| t.line).unwrap_or(0)),
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
                    parts.push(Expr::Literal(Literal::String(current.clone(), Span::default())));
                    current.clear();
                }
                    let mut expr_str = String::new();
                    let mut depth = 1;
                    while let Some(c) = chars.next() {
                        if c == '{' { depth += 1; }
                        else if c == '}' { depth -= 1; if depth == 0 { break; } }
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

    fn arguments(&mut self) -> Vec<Box<Expr>> {
        let mut args = vec![Box::new(self.expression())];
        while self.match_token(&[TokenType::Comma]) { args.push(Box::new(self.expression())); }
        args
    }

    fn match_token(&mut self, types: &[TokenType]) -> bool {
        for t in types { if self.check(t) { self.advance(); return true; } }
        false
    }

    fn check(&self, token_type: &TokenType) -> bool {
        if let Some(token) = self.peek() {
            std::mem::discriminant(&token.token_type) == std::mem::discriminant(token_type)
        } else { false }
    }

    fn advance(&mut self) -> Option<&Token> {
        if !self.is_at_end() { self.current += 1; }
        self.previous()
    }

    fn is_at_end(&self) -> bool { self.check(&TokenType::EOF) }

    fn peek(&self) -> Option<&Token> { self.tokens.get(self.current) }
    fn peek_next(&self) -> Option<&Token> { self.tokens.get(self.current + 1) }
    fn previous(&self) -> Option<&Token> { self.tokens.get(self.current - 1) }

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
        if self.check(token_type) { return self.advance().unwrap(); }
        panic!("{} at line {}", message, self.peek().map(|t| t.line).unwrap_or(0))
    }

    fn consume_identifier(&mut self, message: &str) -> String {
        if let Some(Token { token_type: TokenType::Identifier(name), .. }) = self.peek() {
            let name = name.clone();
            self.advance();
            return name;
        }
        panic!("{} at line {}", message, self.peek().map(|t| t.line).unwrap_or(0))
    }

    /// v11: 接受普通 Identifier **或** 复合关键字(read_bytes/write_bytes)作为方法名。
    /// 用 lexer 关键字化后,这些方法名不再是 TokenType::Identifier,旧 consume_identifier 会 panic。
    /// 这里把它们还原为字面字符串,语义不变(运行时再分发)。
    fn consume_method_name(&mut self, message: &str) -> String {
        match self.peek() {
            Some(Token { token_type: TokenType::Identifier(name), .. }) => {
                let n = name.clone();
                self.advance();
                n
            }
            Some(Token { token_type: TokenType::ReadBytes, .. }) => {
                self.advance();
                "read_bytes".to_string()
            }
            Some(Token { token_type: TokenType::WriteBytes, .. }) => {
                self.advance();
                "write_bytes".to_string()
            }
            Some(Token { token_type: TokenType::Read, .. }) => {
                self.advance();
                "read".to_string()
            }
            Some(Token { token_type: TokenType::Write, .. }) => {
                self.advance();
                "write".to_string()
            }
            Some(Token { token_type: TokenType::Append, .. }) => {
                self.advance();
                "append".to_string()
            }
            _ => panic!("{} at line {}", message, self.peek().map(|t| t.line).unwrap_or(0)),
        }
    }
}

/// 检查字符串是否包含格式插值（{var} 或 {expr}）。
/// 只在 { 后紧跟字母/下划线时才视为插值，避免误触发 JSON 字符串。
fn has_format_interpolation(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if i + 1 < chars.len() {
                let next = chars[i + 1];
                if next == '{' {
                    i += 2; // skip {{ (literal brace)
                    continue;
                }
                if next.is_ascii_alphabetic() || next == '_' {
                    return true; // {var...} — format interpolation
                }
            }
        }
        i += 1;
    }
    false
}
