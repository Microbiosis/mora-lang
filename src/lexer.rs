#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    Let, Task, If, Then, End, Return, True, False, Nil, For, In, Try, Catch, Import, Export,
    Parallel, Match, With, Save, Load, Fn,
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
}

pub struct Lexer {
    source: Vec<char>,
    current: usize,
    line: usize,
}

impl Lexer {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            current: 0,
            line: 1,
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
        });
        tokens
    }

    fn is_at_end(&self) -> bool {
        self.current >= self.source.len()
    }

    fn advance(&mut self) -> char {
        let c = self.source[self.current];
        self.current += 1;
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
        true
    }

    fn make_token(&self, token_type: TokenType) -> Token {
        Token { token_type, line: self.line }
    }

    fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();
        if self.is_at_end() { return None; }

        let c = self.advance();
        match c {
            '+' => Some(self.make_token(TokenType::Plus)),
            '-' => {
                if self.match_char('-') {
                    while self.peek() != '\n' && !self.is_at_end() { self.advance(); }
                    self.next_token()
                } else if self.match_char('>') {
                    Some(self.make_token(TokenType::Arrow))
                } else {
                    Some(self.make_token(TokenType::Minus))
                }
            }
            '*' => Some(self.make_token(TokenType::Star)),
            '/' => Some(self.make_token(TokenType::Slash)),
            '%' => Some(self.make_token(TokenType::Percent)),
            '(' => Some(self.make_token(TokenType::LParen)),
            ')' => Some(self.make_token(TokenType::RParen)),
            '[' => Some(self.make_token(TokenType::LBracket)),
            ']' => Some(self.make_token(TokenType::RBracket)),
            '{' => Some(self.make_token(TokenType::LBrace)),
            '}' => Some(self.make_token(TokenType::RBrace)),
            '.' => Some(self.make_token(TokenType::Dot)),
            ',' => Some(self.make_token(TokenType::Comma)),
            ':' => Some(self.make_token(TokenType::Colon)),
            '|' => {
                if self.match_char('>') { Some(self.make_token(TokenType::Pipe)) }
                else { panic!("Unexpected '|' at line {}; did you mean '|>'?", self.line) }
            }
            '>' => {
                if self.match_char('=') { Some(self.make_token(TokenType::GreaterEqual)) }
                else { Some(self.make_token(TokenType::Greater)) }
            }
            '<' => {
                if self.match_char('=') { Some(self.make_token(TokenType::LessEqual)) }
                else { Some(self.make_token(TokenType::Less)) }
            }
            '=' => {
                if self.match_char('=') { Some(self.make_token(TokenType::Equal)) }
                else { Some(self.make_token(TokenType::Assign)) }
            }
            '!' => {
                if self.match_char('=') { Some(self.make_token(TokenType::NotEqual)) }
                else { panic!("Unexpected '!' at line {}", self.line) }
            }
            '"' => Some(self.string()),
            '\n' => { self.line += 1; Some(self.make_token(TokenType::Newline)) }
            _ => {
                if c.is_ascii_digit() { Some(self.number()) }
                else if c.is_ascii_alphabetic() || c == '_' { Some(self.identifier()) }
                else { panic!("Unexpected character '{}' at line {}", c, self.line) }
            }
        }
    }

    fn skip_whitespace(&mut self) {
        while !self.is_at_end() {
            match self.peek() {
                ' ' | '\r' | '\t' => { self.advance(); }
                _ => break,
            }
        }
    }

    fn string(&mut self) -> Token {
        let mut value = String::new();
        while self.peek() != '"' && !self.is_at_end() {
            if self.peek() == '\n' { self.line += 1; }
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
        self.make_token(TokenType::String(value))
    }

    fn number(&mut self) -> Token {
        let start = self.current - 1;
        while self.peek().is_ascii_digit() { self.advance(); }
        if self.peek() == '.' && self.peek_next().is_ascii_digit() {
            self.advance();
            while self.peek().is_ascii_digit() { self.advance(); }
        }
        let value: String = self.source[start..self.current].iter().collect();
        let num: f64 = value.parse().unwrap();
        self.make_token(TokenType::Number(num))
    }

    fn identifier(&mut self) -> Token {
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
            _ => TokenType::Identifier(value),
        };
        self.make_token(token_type)
    }
}
