#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    Let, Task, If, Then, End, Return, True, False, Nil, For, In, Import, Export,
    Parallel, Match, WithKeyword, Save, Load, Fn, Into, As, Do,
    Read, Write, Append, ReadBytes, WriteBytes,
    Stream, Tool, Break, Continue,
    // v0.06.7: 移除 v0.04 云服务原生关键字 Serve/Http/Mcp/Repl/Stdio/On
    // 云服务走显式 API: Router::new() / McpServer::new()
    Route, Observe, Span, Tags, Record,
    Trace, Metrics, Otel,
    // 注意: HTTP 方法 (GET/POST/PUT/DELETE/PATCH) 不作关键字
    // —— 保持 Identifier,显式 API Router.route() 按字符串匹配
    Identifier(String),
    String(String),
    PromptString(String),  // v0.04.0: p"..."
    Number(f64),
    Plus, Minus, Star, Slash, Percent,
    Assign, Equal, NotEqual,
    Greater, Less, GreaterEqual, LessEqual,
    Pipe, Arrow,
    // v0.05: := 显式 Any 标注（let x := expr = Any，跳过严格 typeck）
    Walrus,
    // v0.06.2: ? 操作符（expr? 传播 Result 错误）
    Question,
    // v0.07.1: :: 操作符（Namespace qualification like Router::new）
    ColonColon,
    LParen, RParen, LBracket, RBracket, LBrace, RBrace, Dot, Comma, Colon,
    Newline,
    EOF,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub token_type: TokenType,
    pub line: usize,
    pub column: usize,
}

pub struct Lexer {
    source: Vec<char>,
    current: usize,
    line: usize,
    column: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            current: 0,
            line: 1,
            column: 1,
        }
    }

    pub fn scan_tokens(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        while !self.is_at_end() {
            if let Some(token) = self.next_token() {
                tokens.push(token);
            }
        }
        tokens.push(Token {
            token_type: TokenType::EOF,
            line: self.line,
            column: self.column,
        });
        tokens
    }

    fn is_at_end(&self) -> bool {
        self.current >= self.source.len()
    }

    fn advance(&mut self) -> char {
        let c = self.source[self.current];
        self.current += 1;
        self.column += 1;
        c
    }

    fn peek(&self) -> char {
        if self.is_at_end() { '\0' } else { self.source[self.current] }
    }

    fn peek_next(&self) -> char {
        if self.current + 1 >= self.source.len() { '\0' } else { self.source[self.current + 1] }
    }

    fn match_char(&mut self, expected: char) -> bool {
        if self.is_at_end() || self.source[self.current] != expected {
            return false;
        }
        self.current += 1;
        self.column += 1;
        true
    }

    fn skip_whitespace(&mut self) {
        while !self.is_at_end() {
            match self.peek() {
                ' ' | '\r' | '\t' => { self.advance(); }
                _ => break,
            }
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();
        if self.is_at_end() { return None; }

        // 记录 token 起始位置
        let start_line = self.line;
        let start_col = self.column;

        let c = self.advance();
        match c {
            '+' => Some(Token { token_type: TokenType::Plus, line: start_line, column: start_col }),
            '-' => {
                if self.match_char('-') {
                    while self.peek() != '\n' && !self.is_at_end() { self.advance(); }
                    self.next_token()
                } else if self.match_char('>') {
                    Some(Token { token_type: TokenType::Arrow, line: start_line, column: start_col })
                } else {
                    Some(Token { token_type: TokenType::Minus, line: start_line, column: start_col })
                }
            }
            '*' => Some(Token { token_type: TokenType::Star, line: start_line, column: start_col }),
            '/' => Some(Token { token_type: TokenType::Slash, line: start_line, column: start_col }),
            '%' => Some(Token { token_type: TokenType::Percent, line: start_line, column: start_col }),
            '(' => Some(Token { token_type: TokenType::LParen, line: start_line, column: start_col }),
            ')' => Some(Token { token_type: TokenType::RParen, line: start_line, column: start_col }),
            '[' => Some(Token { token_type: TokenType::LBracket, line: start_line, column: start_col }),
            ']' => Some(Token { token_type: TokenType::RBracket, line: start_line, column: start_col }),
            '{' => Some(Token { token_type: TokenType::LBrace, line: start_line, column: start_col }),
            '}' => Some(Token { token_type: TokenType::RBrace, line: start_line, column: start_col }),
            '.' => Some(Token { token_type: TokenType::Dot, line: start_line, column: start_col }),
            ',' => Some(Token { token_type: TokenType::Comma, line: start_line, column: start_col }),
            ':' => {
                if self.match_char('=') {
                    Some(Token { token_type: TokenType::Walrus, line: start_line, column: start_col })
                } else if self.match_char(':') {
                    Some(Token { token_type: TokenType::ColonColon, line: start_line, column: start_col })
                } else {
                    Some(Token { token_type: TokenType::Colon, line: start_line, column: start_col })
                }
            }
            '|' => {
                if self.match_char('>') { Some(Token { token_type: TokenType::Pipe, line: start_line, column: start_col }) }
                else { panic!("Unexpected '|' at line {}; did you mean '|>'?", self.line) }
            }
            '>' => {
                if self.match_char('=') { Some(Token { token_type: TokenType::GreaterEqual, line: start_line, column: start_col }) }
                else { Some(Token { token_type: TokenType::Greater, line: start_line, column: start_col }) }
            }
            '<' => {
                if self.match_char('=') { Some(Token { token_type: TokenType::LessEqual, line: start_line, column: start_col }) }
                else { Some(Token { token_type: TokenType::Less, line: start_line, column: start_col }) }
            }
            '=' => {
                if self.match_char('=') { Some(Token { token_type: TokenType::Equal, line: start_line, column: start_col }) }
                else { Some(Token { token_type: TokenType::Assign, line: start_line, column: start_col }) }
            }
            '!' => {
                if self.match_char('=') { Some(Token { token_type: TokenType::NotEqual, line: start_line, column: start_col }) }
                else { panic!("Unexpected '!' at line {}", self.line) }
            }
            // v0.06.2: ? 操作符
            '?' => Some(Token { token_type: TokenType::Question, line: start_line, column: start_col }),
            '"' => Some(self.string_from(start_line, start_col)),
            '\n' => { self.line += 1; self.column = 1; Some(Token { token_type: TokenType::Newline, line: start_line, column: start_col }) }
            _ => {
                if c.is_ascii_digit() { Some(self.number_from(start_line, start_col)) }
                else if c.is_ascii_alphabetic() || c == '_' {
                    // v0.04.0: 检测 p"..." 前缀
                    if c == 'p' && self.peek() == '"' {
                        self.advance(); // 消费 "
                        return Some(self.prompt_string_from(start_line, start_col));
                    }
                    Some(self.identifier_from(start_line, start_col))
                }
                else { panic!("Unexpected character '{}' at line {}", c, self.line) }
            }
        }
    }

    fn string_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let mut value = String::new();
        while self.peek() != '"' && !self.is_at_end() {
            if self.peek() == '\n' { self.line += 1; self.column = 0; }
            if self.peek() == '\\' {
                self.advance(); // consume backslash
                if self.is_at_end() { break; }
                match self.advance() {
                    '"' => { value.push('"'); }
                    '\\' => { value.push('\\'); }
                    'n' => { value.push('\n'); }
                    't' => { value.push('\t'); }
                    'r' => { value.push('\r'); }
                    '0' => { value.push('\0'); }
                    other => {
                        value.push('\\');
                        value.push(other);
                    }
                }
            } else {
                value.push(self.advance());
            }
        }
        if self.is_at_end() { panic!("Unterminated string at line {}", self.line) }
        self.advance(); // closing "
        Token { token_type: TokenType::String(value), line: start_line, column: start_col }
    }

    /// v0.04.0: 解析 p"..." prompt 字符串
    /// 复用 string_from 的转义规则，但 token 类型是 PromptString
    fn prompt_string_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let mut value = String::new();
        while self.peek() != '"' && !self.is_at_end() {
            if self.peek() == '\n' { self.line += 1; self.column = 0; }
            if self.peek() == '\\' {
                self.advance();
                if self.is_at_end() { break; }
                match self.advance() {
                    '"' => { value.push('"'); }
                    '\\' => { value.push('\\'); }
                    'n' => { value.push('\n'); }
                    't' => { value.push('\t'); }
                    'r' => { value.push('\r'); }
                    '0' => { value.push('\0'); }
                    other => {
                        value.push('\\');
                        value.push(other);
                    }
                }
            } else {
                value.push(self.advance());
            }
        }
        if self.is_at_end() { panic!("Unterminated prompt string at line {}", self.line) }
        self.advance(); // closing "
        Token { token_type: TokenType::PromptString(value), line: start_line, column: start_col }
    }

    fn number_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let start = self.current - 1;
        while self.peek().is_ascii_digit() { self.advance(); }
        if self.peek() == '.' && self.peek_next().is_ascii_digit() {
            self.advance();
            while self.peek().is_ascii_digit() { self.advance(); }
        }
        let value: String = self.source[start..self.current].iter().collect();
        let num: f64 = value.parse().unwrap();
        Token { token_type: TokenType::Number(num), line: start_line, column: start_col }
    }

    fn identifier_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let start = self.current - 1;
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' { self.advance(); }
        let value: String = self.source[start..self.current].iter().collect();
        let token_type = match value.as_str() {
            "let" => TokenType::Let,
            "task" => TokenType::Task,
            "if" => TokenType::If,
            "then" => TokenType::Then,
            "end" => TokenType::End,
            "return" => TokenType::Return,
            "true" => TokenType::True,
            "false" => TokenType::False,
            "nil" => TokenType::Nil,
            "for" => TokenType::For,
            "in" => TokenType::In,
            "try" => TokenType::Identifier("try".to_string()),
            "catch" => TokenType::Identifier("catch".to_string()),
            "import" => TokenType::Import,
            "export" => TokenType::Export,
            "parallel" => TokenType::Parallel,
            "match" => TokenType::Match,
            "with" => TokenType::WithKeyword,
            "save" => TokenType::Save,
            "load" => TokenType::Load,
            "fn" => TokenType::Fn,
            "into" => TokenType::Into,
            "read" => TokenType::Read,
            "write" => TokenType::Write,
            "append" => TokenType::Append,
            "read_bytes" => TokenType::ReadBytes,
            "write_bytes" => TokenType::WriteBytes,
            "as" => TokenType::As,
            "do" => TokenType::Do,
            "on" => TokenType::Identifier("on".to_string()),
            "stream" => TokenType::Stream,
            "tool" => TokenType::Tool,
            "break" => TokenType::Break,
            "continue" => TokenType::Continue,
            // v0.06.7: serve/as/mcp/repl/stdio/http/on 不再是关键字——移除
            "route" => TokenType::Route,
            "observe" => TokenType::Observe,
            "span" => TokenType::Span,
            "tags" => TokenType::Tags,
            "record" => TokenType::Record,
            "repl" => TokenType::Identifier("repl".to_string()),
            "stdio" => TokenType::Identifier("stdio".to_string()),
            "mcp" => TokenType::Identifier("mcp".to_string()),
            "http" => TokenType::Identifier("http".to_string()),
            "trace" => TokenType::Trace,
            "metrics" => TokenType::Metrics,
            "otel" => TokenType::Otel,
            _ => TokenType::Identifier(value),
        };
        Token { token_type, line: start_line, column: start_col }
    }
}
