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
