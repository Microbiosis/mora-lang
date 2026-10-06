//! v0.01: 词法分析器 — TokenType 枚举 + Token/Lexer（单字符 token 经 simple_token 收敛，v0.75.69）。

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
    Match,
    Fn,
    As,
    Do,
    Break,
    Continue,
    // v0.06.7: 移除 v0.04 云服务原生关键字 Serve/Http/Mcp/Repl/Stdio/On
    // 云服务走显式 API: Router::new() / McpServer::new()
    // v0.75.19: 语法面收敛 — 移除无前端可达的死关键字（lexer 有 token、
    // ParserV3 不解析、MirInst 由手工构造驱动；运行时原语集不变）。
    // v0.20: 宏关键字
    Macro,
    // v0.86: Lisp homoiconicity — quote 冻结表达式为数据，完成 eval-apply-quote 三元组。
    Quote,
    // v0.25: Multi-Agent 协调关键字
    Orchestrate,
    Loop,
    MaxRounds,
    // v0.26: prompt section 块 — 用于声明一段 system prompt 分段
    // 注意：与 p"..." 模板字符串(prompt_string)互不干扰,后者必须在 'p"' 双字符触发
    Prompt,
    // v0.27: Document 块（与 prompt 块语义类似）
    Document,
    // v0.85: with 块（配置桥接）— with mock_llm = [...] end
    // 与 handle/perform 同模式但显式 TokenType，支持 §1.1 "语言一等公民"。
    With,
    // v0.88: TEA app 块关键字 — app Counter ... end
    App,
    // v0.102: 声明式范式（逻辑式/关系式）关键字
    Rel,
    Solve,
    // 注意: HTTP 方法 (GET/POST/PUT/DELETE/PATCH) 不作关键字
    // —— 保持 Identifier,显式 API Router.route() 按字符串匹配
    Identifier(String),
    String(String),
    /// v0.x: 单字符字面量（`'a'`）
    Char(char),
    PromptString(String), // v0.04.0: p"..."
    // v0.38: numeric tower — Int and Float tokens.
    Int(i64),
    Float(f64),
    // v0.91: BigInt 字面量 — `<digits>n` 后缀
    // 持有 num_bigint::BigInt 直接解析（避免 i64 范围限制）
    BigInt(num_bigint::BigInt),
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
    Pipe, // `|>` pipeline operator
    // v0.85: standalone `|` — used for union type annotations (`string | number`).
    // `|` alone = Or; `|>` = Pipe. Distinct tokens to avoid ambiguity.
    Or,
    // v0.30: `!` 前缀 (逻辑非) 和 `@` 装饰符 (如 @start, @exit graph 节点)
    Bang,
    At,
    // v0.31: 词法错误时 emit (不 panic), 携带错误信息
    Error(String),
    Arrow,
    FatArrow, // v0.55: `=>` 用于 match arm
    // v0.06.2: ? 操作符（expr? 传播 Result 错误）
    Question,
    // v0.07.1: :: 操作符（Namespace qualification like Router::new）
    ColonColon,
    // v0.08: dyn / Self
    Dyn,
    Self_,
    // v0.88: Quasiquote — 反引号 quasiquote 语法（Lisp 系 quasiquote/unquote/unquote-splice）。
    // 反引号 `` ` `` 开始 quasiquote 上下文；逗号在 quasiquote 内语义为 unquote。
    // CommaComma (`,,`) 在 quasiquote 内为 unquote-splice，在普通上下文中为语法错误。
    Backtick,
    CommaComma,
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
    DotDot,    // v0.87: '..' for list rest pattern
    DotDotDot, // v0.16: '...' for list rest pattern
    Comma,
    Colon,
    Amp,              // v0.21: '&' 借用
    AmpMut,           // v0.21: '&mut' 可变借用
    Lifetime(String), // v0.21: 'a 生命周期标注
    /// v0.104.6 D202：`--` 行注释的**正文**（不含 `--`）。
    ///
    /// 仅在 `Lexer::keep_comments(true)` 时产生；默认（parser 走的路径）
    /// 注释仍被**静默丢弃**，与修前逐字一致 —— 见该分支的说明。
    Comment(String),
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
    /// v0.104.6 D202：`--` 注释是否**作为 token 发出**。
    ///
    /// 默认 `false` —— 注释被静默丢弃。**只有** `LSP 格式化器`
    /// （`lsp/providers/formatting.rs`）打开它，因为那条路径要从 token 流
    /// **重建整份文档**，lexer 不发的东西就等于不存在（见 D202）。
    ///
    /// 保持默认关闭的收益：parser / typeck / interpreter **一行都不用改**，
    /// 也就不会有「parser 突然开始看见 `Comment` token」的风险。
    keep_comments: bool,
}

/// 数值字面量**超出 f64 可表示范围**时报错，而非静默产出 `inf` / `0.0`。
///
/// v0.104.6 D27：超范围字面量**静默变成 `inf`**。
///
/// 本语言所有裸数字字面量都是 `Value::Float`（本函数的调用点，无 `i`/`f`/
/// `n` 后缀时统一走 `TokenType::Float`），而 `f64::from_str` 对溢出是
/// **饱和**的 —— 超出 f64 max（约 1.797e308）时返回 `inf` 而非 `Err`：
///
/// ```text
/// print(1` + 309 个 0 + `)   # inf，exit 0，无任何提示
/// print(` + 400 个 9 + `)    # inf，exit 0，无任何提示
/// ```
///
/// 这与本语言 v0.104.6 D21（BigInt 越 i64 后改走原生大数、**不再静默饱和**）
/// 直接矛盾：用户能写 `100000000000000000000n` 拿到精确值，却写不出同一个
/// 数的普通字面量形式 —— 少写一个 `n` 就从精确值变成 `inf`。
///
/// 判据与 Rust 自身一致（`let x = 1e400f64;` 在 Rust 里是编译错误
/// "literal out of range for f64"）：**字面量必须可表示**。
/// 下溢到 `0.0` 同样处理 —— Rust 对 `1e-400f64` 也报 out of range。
/// 真正的 `0` / `0.0` / `0e5` 文本里没有非零数字位，不受影响。
///
/// 返回 `Some(msg)` 表示该字面量不可表示；可表示时返回 `None`。
fn literal_range_error(text: &str, parsed: f64) -> Option<String> {
    let has_nonzero_digit = text.chars().any(|c| ('1'..='9').contains(&c));
    if parsed.is_infinite() {
        let shown = if text.len() > 24 {
            format!("{}… ({} 位)", &text[..24], text.len())
        } else {
            text.to_string()
        };
        return Some(format!(
            "Float literal out of range: {} exceeds f64 max (~1.797e308). \
             Use a BigInt literal (append `n`) for exact large integers.",
            shown
        ));
    }
    if parsed == 0.0 && has_nonzero_digit {
        return Some(format!(
            "Float literal out of range: {} underflows to 0 (f64 min normal ~2.2e-308)",
            text
        ));
    }
    None
}

impl Lexer {
    /// v0.75.69: 单 token 构造辅助 — 消除 33 处重复的 Token { line, column } 构造
    /// （line/column 恒为 start_line/start_col，多字符 token 由 *_from 处理）。
    fn simple_token(token_type: TokenType, line: usize, column: usize) -> Option<Token> {
        Some(Token {
            token_type,
            line,
            column,
        })
    }

    pub fn new(source: &str) -> Self {
        Self {
            source: source.chars().collect(),
            current: 0,
            line: 1,
            column: 1,
            keep_comments: false,
        }
    }

    /// v0.104.6 D202：让 `--` 注释**作为 token 发出**（供格式化器重建文档用）。
    ///
    /// 默认关闭。见 `keep_comments` 字段的说明。
    pub fn keep_comments(mut self, on: bool) -> Self {
        self.keep_comments = on;
        self
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
            '+' => Self::simple_token(TokenType::Plus, start_line, start_col),
            '-' => {
                if self.match_char('-') {
                    // v0.104.6 D202：注释正文（不含 `--`）。
                    //
                    // 修前这个分支**只前进不产出** —— 注释在 token 流里彻底不存在。
                    // 对 parser 而言这是对的（它不需要注释），但 `LSP 格式化器`
                    // 是**从 token 流重建整份文档**的：lexer 不发的东西
                    // 就等于不存在，于是「格式化」会**静默删光文件里所有注释**
                    // （实测 5 行含 3 处 `--` 的源码 → 4 行 0 注释，
                    //  而服务器声明了 `documentFormattingProvider: true`，
                    //  编辑器「保存时格式化」一按，用户的注释就没了）。
                    //
                    // 故加 `keep_comments` 开关：**只有**格式化器打开它，
                    // parser 那条路径行为逐字不变。
                    let mut body = String::new();
                    while self.peek() != '\n' && !self.is_at_end() {
                        body.push(self.advance());
                    }
                    if self.keep_comments {
                        // 去掉行尾 `\r`（CRLF 文件）—— 它不是注释内容
                        let body = body.strip_suffix('\r').unwrap_or(&body);
                        Self::simple_token(
                            TokenType::Comment(body.to_string()),
                            start_line,
                            start_col,
                        )
                    } else {
                        self.next_token()
                    }
                } else if self.match_char('>') {
                    Self::simple_token(TokenType::Arrow, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Minus, start_line, start_col)
                }
            }
            '*' => Self::simple_token(TokenType::Star, start_line, start_col),
            '/' => Self::simple_token(TokenType::Slash, start_line, start_col),
            '%' => Self::simple_token(TokenType::Percent, start_line, start_col),
            '(' => Self::simple_token(TokenType::LParen, start_line, start_col),
            ')' => Self::simple_token(TokenType::RParen, start_line, start_col),
            '[' => Self::simple_token(TokenType::LBracket, start_line, start_col),
            ']' => Self::simple_token(TokenType::RBracket, start_line, start_col),
            '{' => Self::simple_token(TokenType::LBrace, start_line, start_col),
            '}' => Self::simple_token(TokenType::RBrace, start_line, start_col),
            '.' => {
                if self.match_char('.') {
                    if self.match_char('.') {
                        // '...' 三个点 → DotDotDot
                        Self::simple_token(TokenType::DotDotDot, start_line, start_col)
                    } else {
                        // '..' 两个点 → DotDot
                        Self::simple_token(TokenType::DotDot, start_line, start_col)
                    }
                } else {
                    Self::simple_token(TokenType::Dot, start_line, start_col)
                }
            }
            ',' => {
                // v0.88: `,,` → CommaComma（quasiquote unquote-splice）；单 `,` → Comma。
                if self.match_char(',') {
                    Self::simple_token(TokenType::CommaComma, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Comma, start_line, start_col)
                }
            }
            '`' => Self::simple_token(TokenType::Backtick, start_line, start_col),
            ':' => {
                if self.match_char(':') {
                    Self::simple_token(TokenType::ColonColon, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Colon, start_line, start_col)
                }
            }
            '|' => {
                if self.match_char('>') {
                    Self::simple_token(TokenType::Pipe, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Or, start_line, start_col)
                }
            }
            '>' => {
                if self.match_char('=') {
                    Self::simple_token(TokenType::GreaterEqual, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Greater, start_line, start_col)
                }
            }
            '<' => {
                if self.match_char('=') {
                    Self::simple_token(TokenType::LessEqual, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Less, start_line, start_col)
                }
            }
            '=' => {
                if self.match_char('=') {
                    Self::simple_token(TokenType::Equal, start_line, start_col)
                } else if self.match_char('>') {
                    // v0.55: `=>` fat arrow for match arms
                    Self::simple_token(TokenType::FatArrow, start_line, start_col)
                } else {
                    Self::simple_token(TokenType::Assign, start_line, start_col)
                }
            }
            '!' => {
                if self.match_char('=') {
                    Self::simple_token(TokenType::NotEqual, start_line, start_col)
                } else {
                    // v0.30: `!` 作为前缀操作符 (逻辑非), 在 parser 阶段处理
                    // (mora 同时支持 `not` 关键字, 两者等价)
                    Self::simple_token(TokenType::Bang, start_line, start_col)
                }
            }
            // v0.06.2: ? 操作符
            '?' => Self::simple_token(TokenType::Question, start_line, start_col),
            // v0.21: & 借用操作符
            '&' => {
                if self.match_char('m') && self.peek() == 'u' {
                    // '&mut' 可变借用
                    self.advance(); // consume 'u'
                    self.advance(); // consume 't'
                    Self::simple_token(TokenType::AmpMut, start_line, start_col)
                } else {
                    // '&' 不可变借用
                    Self::simple_token(TokenType::Amp, start_line, start_col)
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
                        Self::simple_token(TokenType::Lifetime(lifetime), start_line, start_col)
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
                Self::simple_token(TokenType::Newline, start_line, start_col)
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
                    // 只 emit `@` 本身——parser 自行 consume_identifier 取节点名
                    Self::simple_token(TokenType::At, start_line, start_col)
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
        // v0.91: 加 `n` / `N` 后缀 → BigInt 字面量
        let value: String = self.source[start..self.current].iter().collect();
        let mut suffix: Option<char> = None;
        // v0.104.6 D356：宽度数字**只被消费、不混进 `value`**。
        //
        // 修前 `value.push(self.advance())` 把宽度直接追加到数值串上，
        // 而 `'f'` 分支是 `value.parse()`（**不做** take_while 截断）⇒
        // 宽度被当成**数值**拼进去：
        //
        // ```text
        // 1.5f32  → 1.532      ← 1.5 + "32" 被解析
        // 1f32    → 132.0
        // ```
        //
        // `i` / `u` / `n` 分支靠 `take_while(is_ascii_digit)` **侥幸**没受影响
        // —— 那是巧合（依赖宽度首字符是 ASCII 数字），不是设计。
        // Mora 不建模位宽（见 `'u'` 分支注释），宽度只需被消费。
        if matches!(self.peek(), 'i' | 'I' | 'u' | 'U' | 'f' | 'F' | 'n' | 'N') {
            suffix = Some(self.advance());
            // Optional width: 8/16/32/64（其它数字也照常消费，不报错 —— 见下）。
            while self.peek().is_ascii_digit() {
                self.advance();
            }
        }
        let tt = if let Some(s) = suffix {
            match s {
                'i' | 'I' => {
                    // 宽度已由上面单独收集（D356），`value` 本身就是纯数字。
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
                    // v0.104.6 D356：宽度已单独收集，此处 `value` 是**纯数值串**，
                    // 可直接 parse（修前宽度混在串里 ⇒ `1.5f32` → 1.532）。
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
                    if let Some(msg) = literal_range_error(&value, num) {
                        return self.error_token(start_line, start_col, &msg);
                    }
                    TokenType::Float(num)
                }
                // v0.91: BigInt 字面量 `<digits>n` — 任意精度
                'n' | 'N' => {
                    let digits: String = value
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '-')
                        .collect();
                    match digits.parse::<num_bigint::BigInt>() {
                        Ok(n) => TokenType::BigInt(n),
                        Err(_) => {
                            return self.error_token(
                                start_line,
                                start_col,
                                &format!("Invalid bigint literal: {}", value),
                            );
                        }
                    }
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
            if let Some(msg) = literal_range_error(&value, num) {
                return self.error_token(start_line, start_col, &msg);
            }
            TokenType::Float(num)
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
            "match" => TokenType::Match,
            "fn" => TokenType::Fn,
            "as" => TokenType::As,
            "do" => TokenType::Do,
            "on" => TokenType::Identifier("on".to_string()),
            "break" => TokenType::Break,
            "continue" => TokenType::Continue,
            // v0.06.7: serve/as/mcp/repl/stdio/http/on 不再是关键字——移除
            "repl" => TokenType::Identifier("repl".to_string()),
            "stdio" => TokenType::Identifier("stdio".to_string()),
            "mcp" => TokenType::Identifier("mcp".to_string()),
            "http" => TokenType::Identifier("http".to_string()),
            "macro" => TokenType::Macro,
            // v0.86: Lisp homoiconicity — quote 冻结表达式为数据
            "quote" => TokenType::Quote,
            // v0.08: dyn / Self
            "dyn" => TokenType::Dyn,
            "Self" => TokenType::Self_,
            // v0.23: 类型系统增强
            "type" => TokenType::Type,
            "enum" => TokenType::Enum,
            "struct" => TokenType::Struct,
            // v0.25: Multi-Agent 协调
            "orchestrate" => TokenType::Orchestrate,
            "loop" => TokenType::Loop,
            "max_rounds" => TokenType::MaxRounds,
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
            // v0.85: with 块（配置桥接）— 与 handle/perform 同语义但显式关键字
            "with" => TokenType::With,
            // v0.88: TEA app 块关键字
            "app" => TokenType::App,
            // v0.102: 声明式范式关键字
            "rel" => TokenType::Rel,
            "solve" => TokenType::Solve,
            _ => TokenType::Identifier(value),
        };
        Token {
            token_type,
            line: start_line,
            column: start_col,
        }
    }
}
