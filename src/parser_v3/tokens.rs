//! v0.92: ParserV3 的 token 流导航原语（自 parse.rs 迁出，P1.4 拆分）。
//!
//! 这些是纯粹的词法游标操作（advance/peek/consume/match_token 等），
//! 不依赖任何语法语义。emit.rs / syntax.rs 共用。

use super::*;

impl ParserV3 {
    // ── 游标 ──
    pub(super) fn advance(&mut self) -> Option<&Token> {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    pub(super) fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.current)
    }

    pub(super) fn previous(&self) -> Option<&Token> {
        if self.current > 0 {
            self.tokens.get(self.current - 1)
        } else {
            None
        }
    }

    pub(super) fn is_at_end(&self) -> bool {
        if self.current >= self.tokens.len() {
            return true;
        }
        match self.tokens.get(self.current) {
            Some(t) => t.token_type == TokenType::EOF,
            None => true,
        }
    }

    // ── 匹配 ──
    pub(super) fn check(&self, token_type: &TokenType) -> bool {
        self.peek()
            .map(|t| &t.token_type == token_type)
            .unwrap_or(false)
    }

    pub(super) fn match_token(&mut self, types: &[TokenType]) -> bool {
        for tt in types {
            if self.check(tt) {
                self.advance();
                return true;
            }
        }
        false
    }

    pub(super) fn match_token_exact(&mut self, token_type: TokenType) -> bool {
        if self.check(&token_type) {
            self.advance();
            true
        } else {
            false
        }
    }

    // ── 消费（带错误信息 @current_line）──
    pub(super) fn consume(&mut self, token_type: TokenType, message: &str) -> Option<()> {
        if self.check(&token_type) {
            self.advance();
            Some(())
        } else {
            eprintln!("Parse error: {} at line {}", message, self.current_line());
            None
        }
    }

    /// **只认真正的标识符**，不吃 `token_to_identifier_name` 的关键字兜底。
    ///
    /// v0.104.6 D353：`consume_identifier` 的兜底分支会把**关键字 token**
    /// 映射回标识符名（`if` / `let` / `fn` / `end` / … 共 20+ 个），
    /// 而那个映射的**本意**是「方法名 / `.` / `::` 之后的引用」位置。
    /// `let` 的**声明位**借了同一条路径 ⇒ **僵尸绑定**：
    ///
    /// ```text
    /// let if = 5   → exit 0（声明成功）
    /// print(if)    → 解析失败（永远引用不到）
    /// ```
    ///
    /// 声明位必须**严格**，否则用户会得到「声明成功、引用失败、
    /// 且诊断指错行」的三重坑。
    pub(super) fn consume_plain_identifier(&mut self, message: &str) -> Option<String> {
        match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                Some(name)
            }
            _ => {
                eprintln!("Parse error: {} at line {}", message, self.current_line());
                None
            }
        }
    }

    pub(super) fn consume_identifier(&mut self, message: &str) -> Option<String> {
        match self.peek().cloned() {
            Some(Token {
                token_type: TokenType::Identifier(name),
                ..
            }) => {
                self.advance();
                Some(name)
            }
            Some(ref tok) => {
                if let Some(name) = token_to_identifier_name(&tok.token_type) {
                    self.advance();
                    return Some(name.to_string());
                }
                eprintln!("Parse error: {} at line {}", message, self.current_line());
                None
            }
            _ => {
                eprintln!("Parse error: {} at line {}", message, self.current_line());
                None
            }
        }
    }

    /// 消费一个二元运算符 token（若匹配 accepted 集合），映射到 BinaryOp。
    pub(super) fn consume_binary_op(&mut self, accepted: &[TokenType]) -> Option<BinaryOp> {
        if !accepted.iter().any(|token_type| self.check(token_type)) {
            return None;
        }

        let token = self.advance()?.token_type.clone();
        match token {
            TokenType::Plus => Some(BinaryOp::Add),
            TokenType::Minus => Some(BinaryOp::Sub),
            TokenType::Star => Some(BinaryOp::Mul),
            TokenType::Slash => Some(BinaryOp::Div),
            TokenType::Percent => Some(BinaryOp::Mod),
            TokenType::Equal => Some(BinaryOp::Equal),
            TokenType::NotEqual => Some(BinaryOp::NotEqual),
            TokenType::Greater => Some(BinaryOp::Greater),
            TokenType::Less => Some(BinaryOp::Less),
            TokenType::GreaterEqual => Some(BinaryOp::GreaterEqual),
            TokenType::LessEqual => Some(BinaryOp::LessEqual),
            _ => None,
        }
    }

    // ── 位置 / span ──
    pub(super) fn current_line(&self) -> u32 {
        self.peek()
            .map(|t| t.line)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(0)
    }

    /// v0.104.6 D30：取当前游标**之后**第一个词法错误 token 的信息。
    ///
    /// 词法器一直用 `error_token()` 发射 `TokenType::Error(msg)` 携带精确
    /// 原因（"Invalid float literal: …" / "Invalid bigint literal: …" /
    /// "unterminated string" / "Float literal out of range: …"），但该变体
    /// **全仓零消费点** —— 解析器只当它是陌生物 token，报一句无信息量的
    /// "Failed to parse at line N" 就把真正的原因丢了：
    ///
    /// ```text
    /// print(1e400)   →  Failed to parse at line 1     （真实原因：超 f64 范围）
    /// ```
    ///
    /// 解析失败时先来这里取词法层的真实诊断。仅在**失败路径**上调用
    /// （一次 O(n) 扫描换一条能指向病因的信息），成功路径零成本。
    pub(super) fn lexical_error_ahead(&self) -> Option<(String, u32, u32)> {
        self.tokens
            .iter()
            .skip(self.current)
            .find_map(|t| match &t.token_type {
                TokenType::Error(msg) => Some((msg.clone(), t.line as u32, t.column as u32)),
                _ => None,
            })
    }

    pub(super) fn span_of_current(&self) -> Span {
        self.peek()
            .map(|t| Span {
                line: t.line,
                column: t.column,
            })
            .unwrap_or(Span { line: 0, column: 0 })
    }

    /// 当前 token 是否为指定名字的标识符（含关键字别名）。
    pub(super) fn peek_is_identifier(&self, name: &str) -> bool {
        self.peek()
            .map(|t| {
                matches!(&t.token_type, TokenType::Identifier(s) if s == name)
                    || token_to_identifier_name(&t.token_type) == Some(name)
            })
            .unwrap_or(false)
    }
}
