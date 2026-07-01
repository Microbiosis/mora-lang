#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    Let,
    Task,
    If,
    Then,
    End,
    Return,
    True,
    False,
    Nil,
    For,
    In,
    Import,
    Export,
    Parallel,
    Match,
    WithKeyword,
    Save,
    Load,
    Fn,
    Into,
    As,
    Do,
    Read,
    Write,
    Append,
    ReadBytes,
    WriteBytes,
    Stream,
    Tool,
    Break,
    Continue,
    // v0.06.7: 移除 v0.04 云服务原生关键字 Serve/Http/Mcp/Repl/Stdio/On
    // 云服务走显式 API: Router::new() / McpServer::new()
    Route,
    Observe,
    Span,
    Tags,
    Record,
    Trace,
    Metrics,
    Otel,
    // v0.19: Worker 并发关键字
    Worker,
    Send,    // ->
    Receive, // <-
    // v0.19: 事务关键字
    Transaction,
    Commit,
    Rollback,
    Compensation,
    // v0.20: 宏关键字
    Macro,
    // v0.25: Multi-Agent 协调关键字
    Orchestrate,
    Edges,
    Loop,
    MaxRounds,
    ExitWhen,
    Rounds,
    // v0.25: Eval + Skill 关键字
    Eval,
    Skill,
    Expect,
    Tolerance,
    // v0.26: prompt section 块 — 用于声明一段 system prompt 分段
    // 注意：与 p"..." 模板字符串(prompt_string)互不干扰,后者必须在 'p"' 双字符触发
    Prompt,
    // v0.27: Document 块（与 prompt 块语义类似）
    Document,
    // 注意: HTTP 方法 (GET/POST/PUT/DELETE/PATCH) 不作关键字
    // —— 保持 Identifier,显式 API Router.route() 按字符串匹配
    Identifier(String),
    String(String),
    /// v0.x: 单字符字面量（`'a'`）
    Char(char),
    PromptString(String), // v0.04.0: p"..."
    Number(f64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
    Pipe,
    Arrow,
    // v0.06.2: ? 操作符（expr? 传播 Result 错误）
    Question,
    // v0.07.1: :: 操作符（Namespace qualification like Router::new）
    ColonColon,
    // v0.08: trait / impl / dyn / Self
    Trait,
    Impl,
    Dyn,
    Self_,
    // v0.09: where 子句关键字（trait/impl 末尾的约束）
    Where,
    // v0.23: 类型系统增强
    Type,   // type 关键字
    Enum,   // enum 关键字
    Struct, // struct 关键字
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Dot,
    DotDotDot, // v0.16: '...' 用于列表 rest 模式
    Comma,
    Colon,
    Amp,              // v0.21: '&' 借用
    AmpMut,           // v0.21: '&mut' 可变借用
    Lifetime(String), // v0.21: 'a 生命周期标注
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
        if self.is_at_end() {
            '\0'
        } else {
            self.source[self.current]
        }
    }

    fn peek_next(&self) -> char {
        if self.current + 1 >= self.source.len() {
            '\0'
        } else {
            self.source[self.current + 1]
        }
    }

    /// v0.27: 跳过空格/制表/换行,判断下一个非空白字符是否是 `"`。
    /// 用于:把 `document "x" do ... end` 与 `document.parse(...)` 区分开。
    fn peek_non_newline_is_string(&self) -> bool {
        let mut i = self.current;
        while i < self.source.len() {
            let c = self.source[i];
            if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
                i += 1;
            } else {
                break;
            }
        }
        i < self.source.len() && self.source[i] == '"'
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
                ' ' | '\r' | '\t' => {
                    self.advance();
                }
                _ => break,
            }
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        self.skip_whitespace();
        if self.is_at_end() {
            return None;
        }

        // 记录 token 起始位置
        let start_line = self.line;
        let start_col = self.column;

        let c = self.advance();
        match c {
            '+' => Some(Token {
                token_type: TokenType::Plus,
                line: start_line,
                column: start_col,
            }),
            '-' => {
                if self.match_char('-') {
                    while self.peek() != '\n' && !self.is_at_end() {
                        self.advance();
                    }
                    self.next_token()
                } else if self.match_char('>') {
                    Some(Token {
                        token_type: TokenType::Arrow,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(Token {
                        token_type: TokenType::Minus,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            '*' => Some(Token {
                token_type: TokenType::Star,
                line: start_line,
                column: start_col,
            }),
            '/' => Some(Token {
                token_type: TokenType::Slash,
                line: start_line,
                column: start_col,
            }),
            '%' => Some(Token {
                token_type: TokenType::Percent,
                line: start_line,
                column: start_col,
            }),
            '(' => Some(Token {
                token_type: TokenType::LParen,
                line: start_line,
                column: start_col,
            }),
            ')' => Some(Token {
                token_type: TokenType::RParen,
                line: start_line,
                column: start_col,
            }),
            '[' => Some(Token {
                token_type: TokenType::LBracket,
                line: start_line,
                column: start_col,
            }),
            ']' => Some(Token {
                token_type: TokenType::RBracket,
                line: start_line,
                column: start_col,
            }),
            '{' => Some(Token {
                token_type: TokenType::LBrace,
                line: start_line,
                column: start_col,
            }),
            '}' => Some(Token {
                token_type: TokenType::RBrace,
                line: start_line,
                column: start_col,
            }),
            '.' => {
                if self.match_char('.') && self.match_char('.') {
                    // '...' 三个点 → DotDotDot
                    Some(Token {
                        token_type: TokenType::DotDotDot,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(Token {
                        token_type: TokenType::Dot,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            ',' => Some(Token {
                token_type: TokenType::Comma,
                line: start_line,
                column: start_col,
            }),
            ':' => {
                if self.match_char(':') {
                    Some(Token {
                        token_type: TokenType::ColonColon,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(Token {
                        token_type: TokenType::Colon,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            '|' => {
                if self.match_char('>') {
                    Some(Token {
                        token_type: TokenType::Pipe,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    panic!("Unexpected '|' at line {}; did you mean '|>'?", self.line)
                }
            }
            '>' => {
                if self.match_char('=') {
                    Some(Token {
                        token_type: TokenType::GreaterEqual,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(Token {
                        token_type: TokenType::Greater,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            '<' => {
                if self.match_char('=') {
                    Some(Token {
                        token_type: TokenType::LessEqual,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(Token {
                        token_type: TokenType::Less,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            '=' => {
                if self.match_char('=') {
                    Some(Token {
                        token_type: TokenType::Equal,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(Token {
                        token_type: TokenType::Assign,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            '!' => {
                if self.match_char('=') {
                    Some(Token {
                        token_type: TokenType::NotEqual,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    panic!("Unexpected '!' at line {}", self.line)
                }
            }
            // v0.06.2: ? 操作符
            '?' => Some(Token {
                token_type: TokenType::Question,
                line: start_line,
                column: start_col,
            }),
            // v0.21: & 借用操作符
            '&' => {
                if self.match_char('m') && self.peek() == 'u' {
                    // '&mut' 可变借用
                    self.advance(); // consume 'u'
                    self.advance(); // consume 't'
                    Some(Token {
                        token_type: TokenType::AmpMut,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    // '&' 不可变借用
                    Some(Token {
                        token_type: TokenType::Amp,
                        line: start_line,
                        column: start_col,
                    })
                }
            }
            '"' => Some(self.string_from(start_line, start_col)),
            '\'' => {
                // v0.21: 检查是字符还是生命周期
                // 字符: 'x' (单个字符后跟 ')
                // 生命周期: 'a (后跟 >, ), ,, 空格, 换行, 或非字母字符)
                if self.peek().is_ascii_alphabetic() {
                    // 检查是否是字符 'x' 模式
                    let next = self.peek_next();
                    if next == '\'' {
                        // 字符 'x'
                        Some(self.char_from(start_line, start_col))
                    } else if next == '>'
                        || next == ')'
                        || next == ','
                        || next == ' '
                        || next == '\n'
                        || next == '\0'
                        || !next.is_ascii_alphanumeric()
                    {
                        // 生命周期 'a
                        let mut lifetime = String::new();
                        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                            lifetime.push(self.advance());
                        }
                        Some(Token {
                            token_type: TokenType::Lifetime(lifetime),
                            line: start_line,
                            column: start_col,
                        })
                    } else {
                        // 字符 'x'
                        Some(self.char_from(start_line, start_col))
                    }
                } else {
                    // 字符
                    Some(self.char_from(start_line, start_col))
                }
            }
            '\n' => {
                self.line += 1;
                self.column = 1;
                Some(Token {
                    token_type: TokenType::Newline,
                    line: start_line,
                    column: start_col,
                })
            }
            _ => {
                if c.is_ascii_digit() {
                    Some(self.number_from(start_line, start_col))
                } else if c.is_ascii_alphabetic() || c == '_' {
                    // v0.04.0: 检测 p"..." 前缀
                    if c == 'p' && self.peek() == '"' {
                        self.advance(); // 消费 "
                        return Some(self.prompt_string_from(start_line, start_col));
                    }
                    Some(self.identifier_from(start_line, start_col))
                } else {
                    panic!("Unexpected character '{}' at line {}", c, self.line)
                }
            }
        }
    }

    fn string_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let mut value = String::new();
        while self.peek() != '"' && !self.is_at_end() {
            if self.peek() == '\n' {
                self.line += 1;
                self.column = 0;
            }
            if self.peek() == '\\' {
                self.advance(); // consume backslash
                if self.is_at_end() {
                    break;
                }
                match self.advance() {
                    '"' => {
                        value.push('"');
                    }
                    '\\' => {
                        value.push('\\');
                    }
                    'n' => {
                        value.push('\n');
                    }
                    't' => {
                        value.push('\t');
                    }
                    'r' => {
                        value.push('\r');
                    }
                    '0' => {
                        value.push('\0');
                    }
                    other => {
                        value.push('\\');
                        value.push(other);
                    }
                }
            } else {
                value.push(self.advance());
            }
        }
        if self.is_at_end() {
            panic!("Unterminated string at line {}", self.line)
        }
        self.advance(); // closing "
        Token {
            token_type: TokenType::String(value),
            line: start_line,
            column: start_col,
        }
    }

    /// v0.x: 解析单字符字面量 `'a'`
    /// 不支持转义（除 `\'` `\\` 外），仅单字符；多字符报错
    fn char_from(&mut self, start_line: usize, start_col: usize) -> Token {
        // 已经消耗了起始单引号 '，现在读一个字符 + 一个闭合 '
        if self.is_at_end() {
            panic!("Unterminated char literal at line {}", self.line)
        }
        let ch = if self.peek() == '\\' {
            self.advance(); // consume backslash
            if self.is_at_end() {
                panic!("Unterminated char literal at line {}", self.line)
            }
            match self.advance() {
                '\'' => '\'',
                '\\' => '\\',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '0' => '\0',
                other => panic!(
                    "Unsupported char escape '\\{}' at line {}",
                    other, self.line
                ),
            }
        } else {
            self.advance()
        };
        // 期望闭合 '
        if self.is_at_end() || self.peek() != '\'' {
            panic!(
                "Char literal must contain exactly one character at line {}",
                self.line
            );
        }
        self.advance(); // consume closing '
        Token {
            token_type: TokenType::Char(ch),
            line: start_line,
            column: start_col,
        }
    }

    /// v0.04.0: 解析 p"..." prompt 字符串
    /// 复用 string_from 的转义规则，但 token 类型是 PromptString
    fn prompt_string_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let mut value = String::new();
        while self.peek() != '"' && !self.is_at_end() {
            if self.peek() == '\n' {
                self.line += 1;
                self.column = 0;
            }
            if self.peek() == '\\' {
                self.advance();
                if self.is_at_end() {
                    break;
                }
                match self.advance() {
                    '"' => {
                        value.push('"');
                    }
                    '\\' => {
                        value.push('\\');
                    }
                    'n' => {
                        value.push('\n');
                    }
                    't' => {
                        value.push('\t');
                    }
                    'r' => {
                        value.push('\r');
                    }
                    '0' => {
                        value.push('\0');
                    }
                    other => {
                        value.push('\\');
                        value.push(other);
                    }
                }
            } else {
                value.push(self.advance());
            }
        }
        if self.is_at_end() {
            panic!("Unterminated prompt string at line {}", self.line)
        }
        self.advance(); // closing "
        Token {
            token_type: TokenType::PromptString(value),
            line: start_line,
            column: start_col,
        }
    }

    fn number_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let start = self.current - 1;
        while self.peek().is_ascii_digit() {
            self.advance();
        }
        if self.peek() == '.' && self.peek_next().is_ascii_digit() {
            self.advance();
            while self.peek().is_ascii_digit() {
                self.advance();
            }
        }
        let value: String = self.source[start..self.current].iter().collect();
        let num: f64 = value.parse().unwrap();
        Token {
            token_type: TokenType::Number(num),
            line: start_line,
            column: start_col,
        }
    }

    fn identifier_from(&mut self, start_line: usize, start_col: usize) -> Token {
        let start = self.current - 1;
        while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
            self.advance();
        }
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
            // v0.19: Worker 并发
            "worker" => TokenType::Worker,
            "transaction" => TokenType::Transaction,
            "commit" => TokenType::Commit,
            "rollback" => TokenType::Rollback,
            "compensation" => TokenType::Compensation,
            "macro" => TokenType::Macro,
            // v0.08: trait 系统
            "trait" => TokenType::Trait,
            "impl" => TokenType::Impl,
            "where" => TokenType::Where,
            "dyn" => TokenType::Dyn,
            "Self" => TokenType::Self_,
            // v0.23: 类型系统增强
            "type" => TokenType::Type,
            "enum" => TokenType::Enum,
            "struct" => TokenType::Struct,
            // v0.25: Multi-Agent 协调
            "orchestrate" => TokenType::Orchestrate,
            "edges" => TokenType::Edges,
            "loop" => TokenType::Loop,
            "max_rounds" => TokenType::MaxRounds,
            "exit_when" => TokenType::ExitWhen,
            "rounds" => TokenType::Rounds,
            // v0.25: Eval + Skill (description/version/requires/verify/given/replay 是上下文关键字)
            "eval" => TokenType::Eval,
            "skill" => TokenType::Skill,
            "expect" => TokenType::Expect,
            "tolerance" => TokenType::Tolerance,
            // v0.26: prompt 块语句（与 p"..." 模板字符串互不干扰）
            "prompt" => TokenType::Prompt,
            // v0.27: document 块语句（与 prompt "x" do end 同款）
            // 但允许 `document.parse(...)` 形式:仅当下一个 token 是字符串字面量
            // (块语句起始)时识别为 Document 关键字,否则退化为 Identifier,
            // 使其可作为表达式上下文中的模块名。
            "document" => {
                if self.peek_non_newline_is_string() {
                    TokenType::Document
                } else {
                    TokenType::Identifier(value)
                }
            }
            _ => TokenType::Identifier(value),
        };
        Token {
            token_type,
            line: start_line,
            column: start_col,
        }
    }
}
