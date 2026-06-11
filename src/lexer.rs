#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    Let, Task, If, Then, End, Return, True, False, Nil, For, In, Try, Catch, Import, Export,
    Parallel, Match, With, Save, Load, Fn, Into,
    Read, Write, Append, ReadBytes, WriteBytes,
    Identifier(String),
    String(String),
    Number(f64),
    Plus, Minus, Star, Slash, Percent,
    Assign, Equal, NotEqual,
    Greater, Less, GreaterEqual, LessEqual,
    Pipe, Arrow,
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
            ':' => Some(Token { token_type: TokenType::Colon, line: start_line, column: start_col }),
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
            '"' => Some(self.string_from(start_line, start_col)),
            '\n' => { self.line += 1; self.column = 1; Some(Token { token_type: TokenType::Newline, line: start_line, column: start_col }) }
            _ => {
                if c.is_ascii_digit() { Some(self.number_from(start_line, start_col)) }
                else if c.is_ascii_alphabetic() || c == '_' { Some(self.identifier_from(start_line, start_col)) }
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
            "try" => TokenType::Try,
            "catch" => TokenType::Catch,
            "import" => TokenType::Import,
            "export" => TokenType::Export,
            "parallel" => TokenType::Parallel,
            "match" => TokenType::Match,
            "with" => TokenType::With,
            "save" => TokenType::Save,
            "load" => TokenType::Load,
            "fn" => TokenType::Fn,
            "into" => TokenType::Into,
            "read" => TokenType::Read,
            "write" => TokenType::Write,
            "append" => TokenType::Append,
            "read_bytes" => TokenType::ReadBytes,
            "write_bytes" => TokenType::WriteBytes,
            _ => TokenType::Identifier(value),
        };
        Token { token_type, line: start_line, column: start_col }
    }
}
