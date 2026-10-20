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
    /// 词法器用 `error_token()` 发射 `TokenType::Error(msg)` 携带精确原因，
    /// 但该变体此前**全仓零消费点** —— 解析器只当它是陌生物 token，报一句
    /// 无信息量的 "Failed to parse at line N" 就把真正的原因丢了：
    ///
    /// ```text
    /// print("abc     →  Failed to parse at line 1     （真实原因：字符串未闭合）
    /// ```
    ///
    /// ⚠ **实测校正**：本注释此前举例 `print(1e400)` 声称会报「浮点超范围」。
    /// 那**不成立** —— 当前词法器把 `1e400` 切成 `Float(1.0)` + `Identifier("e400")`
    /// 两个正常 token，不产生 `Error`。实测能产生 `Error` 的只有字符串未闭合
    /// （"Unterminated string"）与字符字面量长度不对（"Char literal must
    /// contain exactly one character"）两类。若日后词法器补上浮点/大整数越界
    /// 的 `Error`，本函数无需改动即可带上它。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parser(src: &str) -> ParserV3 {
        let tokens = Lexer::new(src).scan_tokens();
        ParserV3::new(tokens, src)
    }

    /// `is_at_end` 必须把 **EOF token** 也当作结尾，而不只是「下标越界」。
    ///
    /// `check`/`match_token` 都建立在它之上：若 EOF 不算结尾，
    /// 解析器会在文件末尾反复看到 EOF 并把「已结束」误判成「还有一个 token」。
    #[test]
    fn eof_token_counts_as_end() {
        let mut p = parser("1 + 2");
        assert!(!p.is_at_end());
        while !p.is_at_end() {
            p.advance();
        }
        // 停在 EOF 上；再 advance 不应越界 panic
        assert!(p.is_at_end());
        assert!(p.advance().is_some() || p.is_at_end());
    }

    /// `advance` 到底后不得越界，`previous` 在开头处返回 `None`。
    #[test]
    fn advance_saturates_at_end_and_previous_is_none_at_start() {
        let mut p = parser("x");
        assert!(
            p.previous().is_none(),
            "游标在最开头时 previous() 必须是 None"
        );
        for _ in 0..50 {
            p.advance();
        }
        assert!(p.is_at_end(), "反复 advance 不得越界");
    }

    /// D353：声明位必须**只认真正的标识符**。
    ///
    /// `consume_identifier` 的兜底分支会把关键字 token 映射回标识符名，
    /// 那条路径的本意是「方法名 / `.` / `::` 之后的引用」位置。若声明位
    /// 借用它，`let if = 5` 会**声明成功**而 `print(if)` 永远引用不到 ——
    /// 用户拿到「声明成功、引用失败、诊断还指错行」的三重坑。
    ///
    /// 两条 API 必须对同一个关键字给出**相反**的答案，这正是 D353 的内容。
    #[test]
    fn d353_plain_identifier_rejects_keywords_that_lenient_one_accepts() {
        let mut strict = parser("if");
        let mut lenient = parser("if");

        assert!(
            strict
                .consume_plain_identifier("expected identifier")
                .is_none(),
            "consume_plain_identifier 不得接受关键字 `if`"
        );
        assert!(
            lenient.consume_identifier("expected identifier").is_some(),
            "consume_identifier 仍须接受关键字（引用位置需要）"
        );
    }

    /// `consume_plain_identifier` 对真标识符必须正常消费并推进游标。
    #[test]
    fn plain_identifier_consumes_and_advances() {
        let mut p = parser("alpha beta");
        assert_eq!(
            p.consume_plain_identifier("expected identifier").as_deref(),
            Some("alpha")
        );
        assert_eq!(
            p.consume_plain_identifier("expected identifier").as_deref(),
            Some("beta")
        );
        assert!(p.consume_plain_identifier("expected identifier").is_none());
    }

    /// D30：`lexical_error_ahead` 必须把词法器的**真实原因**带到解析失败处。
    ///
    /// 此前 `TokenType::Error(msg)` 全仓零消费点，`print(1e400)` 只报一句
    /// 无信息量的「Failed to parse at line 1」，把「超出 f64 范围」丢了。
    #[test]
    fn d30_lexical_error_ahead_surfaces_the_real_reason() {
        let (msg, line, _col) = parser("print(\"abc")
            .lexical_error_ahead()
            .expect("未闭合字符串应产生词法错误 token");
        assert!(
            msg.contains("Unterminated string"),
            "诊断应指向真实病因（未闭合字符串），实得：{msg}"
        );
        assert!(line >= 1);

        // 无词法错误时必须是 None（成功路径零成本，不得误报）
        assert!(parser("print(1 + 2)").lexical_error_ahead().is_none());
    }

    /// 词法错误必须**从游标之后**开始找：已经走过的不再上报。
    ///
    /// 否则一次已经被消费掉的错误会被反复当作「当前病因」重新抛出。
    #[test]
    fn lexical_error_ahead_only_scans_ahead_of_the_cursor() {
        let mut p = parser("print(\"abc");
        // 先把错误 token 走掉
        while !p.is_at_end()
            && !matches!(p.peek().map(|t| &t.token_type), Some(TokenType::Error(_)))
        {
            p.advance();
        }
        assert!(
            matches!(p.peek().map(|t| &t.token_type), Some(TokenType::Error(_))),
            "前置条件：游标应停在错误 token 上"
        );
        p.advance();
        assert!(
            p.lexical_error_ahead().is_none(),
            "游标已越过该错误，不应再被当作待报病因"
        );
    }

    /// `consume_binary_op` 双重把关：**只消费白名单内的**，且映射正确。
    ///
    /// 若它无视白名单直接 advance，`a = 1` 里的 `=` 会被当成二元运算符吃掉。
    #[test]
    fn consume_binary_op_honours_the_accepted_set() {
        let mut p = parser("+ * =");
        assert_eq!(p.consume_binary_op(&[TokenType::Plus]), Some(BinaryOp::Add));
        assert_eq!(p.consume_binary_op(&[TokenType::Star]), Some(BinaryOp::Mul));
        // 赋值号不在白名单 ⇒ 不得被消费，游标必须留在原地
        assert_eq!(
            p.consume_binary_op(&[TokenType::Star, TokenType::Slash]),
            None
        );
        assert_eq!(
            p.peek().map(|t| &t.token_type),
            Some(&TokenType::Assign),
            "未命中白名单时游标不得推进"
        );
    }

    /// 白名单内的运算符必须映射成**对应的** `BinaryOp`，不能张冠李戴。
    #[test]
    fn consume_binary_op_maps_each_token_to_its_own_op() {
        let cases: &[(TokenType, BinaryOp)] = &[
            (TokenType::Plus, BinaryOp::Add),
            (TokenType::Minus, BinaryOp::Sub),
            (TokenType::Star, BinaryOp::Mul),
            (TokenType::Slash, BinaryOp::Div),
            (TokenType::Percent, BinaryOp::Mod),
            (TokenType::Equal, BinaryOp::Equal),
            (TokenType::NotEqual, BinaryOp::NotEqual),
            (TokenType::Greater, BinaryOp::Greater),
            (TokenType::Less, BinaryOp::Less),
            (TokenType::GreaterEqual, BinaryOp::GreaterEqual),
            (TokenType::LessEqual, BinaryOp::LessEqual),
        ];
        for (tt, expected) in cases {
            let src = match tt {
                TokenType::Plus => "+",
                TokenType::Minus => "-",
                TokenType::Star => "*",
                TokenType::Slash => "/",
                TokenType::Percent => "%",
                TokenType::Equal => "==",
                TokenType::NotEqual => "!=",
                TokenType::Greater => ">",
                TokenType::Less => "<",
                TokenType::GreaterEqual => ">=",
                TokenType::LessEqual => "<=",
                _ => unreachable!(),
            };
            let mut p = parser(src);
            assert_eq!(
                p.consume_binary_op(std::slice::from_ref(tt)),
                Some(expected.clone()),
                "{src} 应映射到 {expected:?}"
            );
        }
    }

    /// `span_of_current` / `current_line` 在游标越过末尾时必须给出确定的
    /// 零值，而不是 panic —— 错误路径上它们常被调用来拼诊断信息。
    #[test]
    fn position_helpers_are_total_on_an_exhausted_stream() {
        // `advance()` 在 EOF 处停住，所以「越过末尾」要靠**空 token 流**构造：
        // 此时 `peek()` 返回 None，走的是 `unwrap_or` 兜底分支。
        let mut p = ParserV3::new(Vec::new(), "");
        assert!(p.is_at_end());
        for _ in 0..10 {
            p.advance();
        }
        assert_eq!(p.current_line(), 0);
        assert_eq!(p.span_of_current(), Span { line: 0, column: 0 });
        assert!(!p.check(&TokenType::Plus));
        assert!(!p.peek_is_identifier("x"));
    }
}
