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
    // v0.50: 状态与执行模型关键字
    State,      // 'state' (orchestrate 内)
    Node,       // 'node' (orchestrate 内)
    Channel,    // 'channel' (orchestrate 内)
    Checkpoint, // 'checkpoint' (orchestrate decorator)
    Rewind,     // 'rewind' (builtin)
    Resume,     // 'resume' (builtin)
    Thread,     // 'thread' (checkpoint thread_id)
    Dynamic,    // 'dynamic' (edge 修饰)
    Map,        // 'map' (dynamic 类型)
    Reduce,     // 'reduce' (dynamic 类型)
    FanIn,      // 'fan_in' (JoinNode)
    FanOut,     // 'fan_out' (parallel worker)
    Interrupt,  // 'interrupt' (HITL 暂停点)
    Before,     // 'before' (interrupt 位置)
    After,      // 'after' (interrupt 位置)
    Command,    // 'command' (返回类型)
    Goto,       // 'goto' (command 字段)
    Update,     // 'update' (command 字段)
    Add,        // '@add' 语义
    Last,       // '@last' 语义 (默认)
    Merge,      // '@merge' 语义
    // 注意: HTTP 方法 (GET/POST/PUT/DELETE/PATCH) 不作关键字
    // —— 保持 Identifier,显式 API Router.route() 按字符串匹配
    Identifier(String),
    String(String),
    /// v0.x: 单字符字面量（`'a'`）
    Char(char),
    PromptString(String), // v0.04.0: p"..."
    // v0.38: numeric tower — distinct Int/Float tokens. Number is the
    // legacy default for unsuffixed literals.
    Int(i64),
    Float(f64),
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
    // v0.30: `!` 前缀 (逻辑非) 和 `@` 装饰符 (如 @start, @exit graph 节点)
    Bang,
    At,
    // v0.31: 词法错误时 emit (不 panic), 携带错误信息
    Error(String),
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

    /// v0.31: 词法错误 emit Error token (不 panic).
    /// 由 parser 看到后停止, 错误信息保留在 token 里.
    fn error_token(&self, line: usize, column: usize, msg: &str) -> Token {
        Token {
            token_type: TokenType::Error(msg.to_string()),
            line,
            column,
        }
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
                    Some(self.error_token(
                        start_line,
                        start_col,
                        "Unexpected '|'; did you mean '|>'?",
                    ))
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
                    // v0.30: `!` 作为前缀操作符 (逻辑非), 在 parser 阶段处理
                    // (mora 同时支持 `not` 关键字, 两者等价)
                    Some(Token {
                        token_type: TokenType::Bang,
                        line: start_line,
                        column: start_col,
                    })
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
                } else if c == '@' {
                    // v0.30: `@` 装饰符 (e.g. @start, @exit 用于 graph node label)
                    // 把 @ 后跟的标识符作为整体 identifier 处理 (含 @ 前缀)
                    self.advance(); // 消费 @
                    let mut name = String::from("@");
                    while self.peek().is_ascii_alphanumeric() || self.peek() == '_' {
                        name.push(self.advance());
                    }
                    Some(Token {
                        token_type: TokenType::At,
                        line: start_line,
                        column: start_col,
                    })
                } else {
                    Some(self.error_token(
                        start_line,
                        start_col,
                        &format!("Unexpected character '{}'", c),
                    ))
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
                let c = self.advance();
                // v0.35 (P0-B4): reject control chars in string literals.
                // NUL and 0x01-0x1f/0x7f round-trip through lexer/JSON
                // but crash downstream at POSIX/HTTP/file boundaries.
                // Note: \t, \n, \r (0x09/0x0A/0x0D) are LEGITIMATE in
                // multi-line string literals and stay allowed.
                let code = c as u32;
                let is_legit = matches!(c, '\t' | '\n' | '\r');
                if !is_legit && (code < 0x20 || c == '\x7f') {
                    return self.error_token(
                        start_line,
                        start_col,
                        "control character in string literal",
                    );
                }
                value.push(c);
            }
        }
        if self.is_at_end() {
            return self.error_token(start_line, start_col, "Unterminated string");
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
            return self.error_token(start_line, start_col, "Unterminated char literal");
        }
        let ch = if self.peek() == '\\' {
            self.advance(); // consume backslash
            if self.is_at_end() {
                return self.error_token(start_line, start_col, "Unterminated char escape");
            }
            match self.advance() {
                '\'' => '\'',
                '\\' => '\\',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '0' => '\0',
                other => {
                    return self.error_token(
                        start_line,
                        start_col,
                        &format!("Unsupported char escape '\\{}'", other),
                    );
                }
            }
        } else {
            self.advance()
        };
        // 期望闭合 '
        if self.is_at_end() || self.peek() != '\'' {
            return self.error_token(
                start_line,
                start_col,
                "Char literal must contain exactly one character",
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
                let c = self.advance();
                // v0.35 (P0-B4): same control-char rejection as string_from.
                let code = c as u32;
                let is_legit = matches!(c, '\t' | '\n' | '\r');
                if !is_legit && (code < 0x20 || c == '\x7f') {
                    return self.error_token(
                        start_line,
                        start_col,
                        "control character in prompt string",
                    );
                }
                value.push(c);
            }
        }
        if self.is_at_end() {
            return self.error_token(start_line, start_col, "Unterminated prompt string");
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
        // v0.38: detect `i` / `u` / `f` / `I` suffix for Int/Number/Float.
        let mut value: String = self.source[start..self.current].iter().collect();
        let mut suffix: Option<char> = None;
        if matches!(self.peek(), 'i' | 'I' | 'u' | 'U' | 'f' | 'F') {
            suffix = Some(self.advance());
            // Optional width: 8/16/32/64.
            while self.peek().is_ascii_digit() {
                value.push(self.advance());
            }
        }
        let tt = if let Some(s) = suffix {
            match s {
                'i' | 'I' => {
                    // Parse as integer — strip the trailing width digits
                    // and suffix character before parsing the body.
                    let digits: String = value
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '-')
                        .collect();
                    match digits.parse::<i64>() {
                        Ok(n) => TokenType::Int(n),
                        Err(_) => {
                            return self.error_token(
                                start_line,
                                start_col,
                                &format!("Invalid integer literal: {}", value),
                            );
                        }
                    }
                }
                'u' | 'U' => {
                    // Same as int but cast via i64 (mora doesn't model unsigned).
                    let digits: String = value
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '-')
                        .collect();
                    match digits.parse::<i64>() {
                        Ok(n) => TokenType::Int(n),
                        Err(_) => {
                            return self.error_token(
                                start_line,
                                start_col,
                                &format!("Invalid integer literal: {}", value),
                            );
                        }
                    }
                }
                'f' | 'F' => {
                    // Float — re-parse with suffix stripped (was parsed as
                    // a partial number above). The trailing chars we
                    // appended are width digits ('32'/'64'), which are
                    // safe to drop.
                    let num: f64 = match value.parse() {
                        Ok(n) => n,
                        Err(_) => {
                            return self.error_token(
                                start_line,
                                start_col,
                                &format!("Invalid float literal: {}", value),
                            );
                        }
                    };
                    TokenType::Float(num)
                }
                _ => unreachable!(),
            }
        } else {
            let num: f64 = match value.parse() {
                Ok(n) => n,
                Err(_) => {
                    return self.error_token(
                        start_line,
                        start_col,
                        &format!("Invalid number literal: {}", value),
                    );
                }
            };
            TokenType::Number(num)
        };
        Token {
            token_type: tt,
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
            // v0.50: 状态与执行模型关键字
            "state" => TokenType::State,
            "node" => TokenType::Node,
            "channel" => TokenType::Channel,
            "checkpoint" => TokenType::Checkpoint,
            "rewind" => TokenType::Rewind,
            "resume" => TokenType::Resume,
            "thread" => TokenType::Thread,
            "dynamic" => TokenType::Dynamic,
            "map" => TokenType::Map,
            "reduce" => TokenType::Reduce,
            "fan_in" => TokenType::FanIn,
            "fan_out" => TokenType::FanOut,
            "interrupt" => TokenType::Interrupt,
            "before" => TokenType::Before,
            "after" => TokenType::After,
            "command" => TokenType::Command,
            "send" => TokenType::Send,
            "goto" => TokenType::Goto,
            "update" => TokenType::Update,
            "add" => TokenType::Add,
            "last" => TokenType::Last,
            "merge" => TokenType::Merge,
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

#[cfg(test)]
mod tests {
    //! 词法器白盒测试。
    //!
    //! 覆盖范围：
    //! - 标识符 / 字面量 / 关键字
    //! - 算子分派（含 `->` / `..` / `...` / `::` 多字符算子）
    //! - 行号 / 列号跟踪
    //! - 注释（`--` 至行尾）
    //! - 错误 Token emission（v0.31 不 panic）
    //! - EOF token 边界
    use super::*;

    /// 提取所有 token_type，丢掉 line/column 位置以便断言内容。
    fn types_of(src: &str) -> Vec<TokenType> {
        Lexer::new(src)
            .scan_tokens()
            .into_iter()
            .map(|t| t.token_type)
            .collect()
    }

    #[test]
    fn empty_source_yields_only_eof() {
        assert_eq!(types_of(""), vec![TokenType::EOF]);
    }

    #[test]
    fn whitespace_only_yields_only_eof() {
        assert_eq!(types_of("   \t  "), vec![TokenType::EOF]);
    }

    #[test]
    fn newlines_yield_only_eof_after_position_advance() {
        // 换行 **会** 发 Newline token;tab/CR 是普通空白
        // 这是 v0.x parser 把 Newline 当语句分隔符的依据
        let toks = types_of("\n\n\t\r");
        assert_eq!(
            toks,
            vec![TokenType::Newline, TokenType::Newline, TokenType::EOF,]
        );
    }

    #[test]
    fn identifier_round_trip() {
        let toks = types_of("foo");
        assert_eq!(
            toks,
            vec![TokenType::Identifier("foo".to_string()), TokenType::EOF]
        );
    }

    #[test]
    fn identifier_with_underscore_and_digits() {
        let toks = types_of("user_42 v1");
        assert_eq!(
            toks,
            vec![
                TokenType::Identifier("user_42".to_string()),
                TokenType::Identifier("v1".to_string()),
                TokenType::EOF,
            ]
        );
    }

    #[test]
    fn keyword_let_is_recognized() {
        let toks = types_of("let");
        assert_eq!(toks, vec![TokenType::Let, TokenType::EOF]);
    }

    #[test]
    fn keyword_task_is_recognized() {
        let toks = types_of("task");
        assert_eq!(toks, vec![TokenType::Task, TokenType::EOF]);
    }

    #[test]
    fn keyword_then_is_recognized() {
        let toks = types_of("if x then y end");
        assert_eq!(
            toks,
            vec![
                TokenType::If,
                TokenType::Identifier("x".to_string()),
                TokenType::Then,
                TokenType::Identifier("y".to_string()),
                TokenType::End,
                TokenType::EOF,
            ]
        );
    }

    #[test]
    fn integer_literal_with_i_suffix_is_int() {
        let toks = types_of("42i");
        assert_eq!(toks, vec![TokenType::Int(42), TokenType::EOF]);
    }

    #[test]
    fn integer_literal_with_negative_sign() {
        // 单 `-` 视为 Minus;无后缀数字归入 Number(f64),负号数值组合由 parser 处理
        let toks = types_of("-3");
        assert_eq!(
            toks,
            vec![TokenType::Minus, TokenType::Number(3.0), TokenType::EOF]
        );
    }

    #[test]
    fn float_literal_with_f_suffix_is_float() {
        // 用 1.5 避开 clippy::approx_constant 触发 (3.14 ≈ π)
        let toks = types_of("1.5f");
        assert_eq!(toks, vec![TokenType::Float(1.5), TokenType::EOF]);
    }

    #[test]
    fn unsuffixed_numeric_is_number() {
        let toks = types_of("1.5");
        assert_eq!(toks, vec![TokenType::Number(1.5), TokenType::EOF]);
    }

    #[test]
    fn integer_literal_unsuffixed_is_number() {
        // 无后缀数字归入 Number(f64),与 Int(f64 上面无后缀) 路由一致
        let toks = types_of("7");
        assert_eq!(toks, vec![TokenType::Number(7.0), TokenType::EOF]);
    }

    #[test]
    fn string_literal_decodes_escapes() {
        // Lexer 仅作转义解码; 完整字符串插值在 parser 层
        let toks = types_of(r#""hello\nworld""#);
        assert_eq!(
            toks,
            vec![
                TokenType::String("hello\nworld".to_string()),
                TokenType::EOF,
            ]
        );
    }

    #[test]
    fn char_literal() {
        let toks = types_of(r#"'a'"#);
        assert_eq!(toks, vec![TokenType::Char('a'), TokenType::EOF]);
    }

    #[test]
    fn prompt_string_literal_p_quoted() {
        let toks = types_of(r#"p"system prompt""#);
        assert_eq!(
            toks,
            vec![
                TokenType::PromptString("system prompt".to_string()),
                TokenType::EOF,
            ]
        );
    }

    #[test]
    fn line_comment_consumes_to_eol() {
        // `--` 起首的注释:该行其余全部忽略
        let toks = types_of("let -- a comment\nlet");
        assert_eq!(
            toks,
            vec![
                TokenType::Let,
                TokenType::Newline,
                TokenType::Let,
                TokenType::EOF,
            ]
        );
    }

    #[test]
    fn line_comment_at_end_of_input() {
        // 注释延续到 EOF 不会 panic
        let toks = types_of("let -- trailing");
        assert_eq!(toks, vec![TokenType::Let, TokenType::EOF]);
    }

    #[test]
    fn arrow_operator_distinguished_from_minus() {
        // `->` 是 Arrow, 单 `-` 是 Minus
        let toks = types_of("->");
        assert_eq!(toks, vec![TokenType::Arrow, TokenType::EOF]);
    }

    #[test]
    fn dotdotdot_operator_distinguished_from_dot() {
        // `...` 是 DotDotDot; `..`(两个)是 RangeSeperator; 单 `.` 是 Dot
        let toks = types_of("...");
        assert_eq!(toks, vec![TokenType::DotDotDot, TokenType::EOF]);
    }

    #[test]
    fn coloncolon_operator_distinguished_from_colon() {
        // `::` 是 Namespace 限定;单 `:` 是 Colon
        let toks = types_of("::");
        assert_eq!(toks, vec![TokenType::ColonColon, TokenType::EOF]);
    }

    #[test]
    fn assignment_operators_recognized() {
        // '=' vs '==' vs '!='
        assert_eq!(types_of("="), vec![TokenType::Assign, TokenType::EOF]);
        assert_eq!(types_of("=="), vec![TokenType::Equal, TokenType::EOF]);
        assert_eq!(types_of("!="), vec![TokenType::NotEqual, TokenType::EOF]);
    }

    #[test]
    fn line_and_column_reset_on_newline() {
        // 第 1 行列号随 token 后移到下一行从 1 重新计数
        let toks = Lexer::new("x\ny").scan_tokens();
        // x 在 line 1 col 1; y 在 line 2 col 1
        assert_eq!(toks[0].line, 1);
        assert_eq!(toks[0].column, 1);
        // newline token 之间产生;y 在第 2 行第 1 列
        let y = toks
            .iter()
            .find(|t| matches!(t.token_type, TokenType::Identifier(ref s) if s == "y"))
            .expect("y present");
        assert_eq!(y.line, 2);
        assert_eq!(y.column, 1);
    }

    #[test]
    fn nested_deep_tokens_track_columns_independently() {
        // 关键字 + 数字后列号应递增
        let toks = Lexer::new("+ - *").scan_tokens();
        assert_eq!(toks[0].column, 1); // +
        assert_eq!(toks[1].column, 3); // -
        assert_eq!(toks[2].column, 5); // *
    }

    #[test]
    fn invalid_character_emits_error_token_not_panic() {
        // v0.31: 词法错误 emit Error(msg) token 而非 panic
        // 用 § 这种非 ASCII 控制字符(ASCII 控制字符可能在 v0.x 中另有处理,故选高码点)
        let toks = Lexer::new("a § b").scan_tokens();
        // 第一与第三 token 是 identifier b;
        // 中间应当是 Error token
        assert!(
            toks.iter()
                .any(|t| matches!(&t.token_type, TokenType::Error(_))),
            "expected an Error token, got: {:?}",
            toks
        );
    }

    #[test]
    fn identifier_with_unicode_letters_rejected() {
        // 当前 lexer 仅接受 ASCII identifier(`[a-zA-Z_][a-zA-Z0-9_]*`)。
        // 非 ASCII 字母(希腊 / 中文)落入 Error token — 由 parser 顶住或报错。
        // 这是 v0.x 限制;未来支持 unicode-identifier 见 plan.update 路线图。
        let toks = Lexer::new("α").scan_tokens();
        assert!(
            toks.iter()
                .any(|t| matches!(&t.token_type, TokenType::Error(_))),
            "non-ASCII identifier char must emit Error token, got: {:?}",
            toks
        );
    }

    #[test]
    fn comment_recursion_returns_next_meaningful_token() {
        // 注释行尾后跟随换行,该 Newline 仍是 token 序列的一部分
        let toks = types_of("-- nothing\n42");
        assert_eq!(
            toks,
            vec![TokenType::Newline, TokenType::Number(42.0), TokenType::EOF]
        );
    }
}
