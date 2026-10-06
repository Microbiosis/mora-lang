//! v0.104.6 D76：`ai.tokens().calls()` 把**输入 token 数**当作**调用次数**。
//!
//! ## 缺陷
//!
//! ```rust
//! // interpreter/builtins/ai_tokens.rs（修前）
//! "calls" => Ok(Value::Float(self.ai.token_usage.input as f64)),
//! ```
//!
//! 而 `TokenUsage` 当时**只有 `input` / `output` 两个字段，根本没有 `calls`**
//! —— 是复制粘贴留下的。真实调用下二者必然不同：一次调用往往带来几百个
//! input token，于是 `ai.tokens().calls()` 会返回一个比真值**大两个数量级**
//! 的数字，且无任何提示。
//!
//! ## 为什么本缺陷无法用 CLI 观测
//!
//! 运行期填充 `token_usage` 的唯一入口是 `interpreter/ai_helpers.rs` 的
//! `account_tokens`，而 `ai.chat` 在 **mock 模式**（无 `OPENAI_API_KEY`）
//! 下直接返回、**不经过** `account_tokens` —— 所以 `ai.tokens().*` 恒为 0，
//! `calls` 与 `input` 看起来「一样对」。
//!
//! 结论：真实路径的修复只能靠**单元测试**观测。
//! `src/runtime/ai.rs::record_tokens_counts_calls_not_input_tokens` 是本机
//! 唯一能钉住它的地方（同文件内）。
//!
//! ## 附带修的：typeck 签名缺口
//!
//! `ai.tokens()` 与 `AiTokens` 值上的四个计数器方法此前**完全没有 typeck
//! 签名** —— `let v: Int = ai.tokens()` 静默通过。

/// **只跑类型检查**，不执行 —— 本文件断言的都是「标注是否被 typeck 接受」。
fn typeck(src: &str) -> Result<(), String> {
    let (_func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if errs.is_empty() {
        Ok(())
    } else {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        Err(format!("TYPECK: {msgs:?}"))
    }
}

/// `ai.tokens()` 返回 `Value::Builtin(AiTokens)` → 声明为 `Type::Builtin`。
#[test]
fn ai_tokens_return_type_is_enforced() {
    for ann in ["Int", "String", "Float", "bool"] {
        let src = format!("let v: {ann} = ai.tokens()\nprint(1)\n");
        assert!(
            typeck(&src).is_err(),
            "[ai.tokens] 返回 Builtin，配 `{ann}` 标注必须被拒 —— \
             若通过了，说明它又没有 typeck 签名、结果类型退化成 TypeVar"
        );
    }
    assert!(
        typeck("let v: any = ai.tokens()\nprint(1)\n").is_ok(),
        "`any` 必须照常放行"
    );
    assert!(
        typeck("let t = ai.tokens()\nprint(t.total())\n").is_ok(),
        "无标注调用必须照常"
    );
}

/// `AiTokens` 值上的四个计数器方法一律返 `Float`（`ai_tokens.rs` 逐条核对）。
#[test]
fn ai_tokens_counter_methods_return_float() {
    for m in ["input", "output", "total", "calls"] {
        let bad = format!("let t = ai.tokens()\nlet v: Int = t.{m}()\nprint(1)\n");
        assert!(
            typeck(&bad).is_err(),
            "[ai.tokens().{m}] 返回 Float，必须拒绝 Int —— 修前该方法无签名，\
             任何标注都被接受"
        );
        let ok = format!("let t = ai.tokens()\nlet v: Float = t.{m}()\nprint(1)\n");
        assert!(typeck(&ok).is_ok(), "[ai.tokens().{m}] 必须接受 Float");
        let num = format!("let t = ai.tokens()\nlet v: number = t.{m}()\nprint(1)\n");
        assert!(typeck(&num).is_ok(), "[ai.tokens().{m}] 必须接受 number");
    }
}

/// 对照组：`ai.chat` / `ai.critic` 的签名**本来就存在**（D75 之前），
/// 用来证明「同一命名空间里有的方法有签名、有的没有」不是本测试的误判。
#[test]
fn ai_chat_and_critic_already_had_signatures() {
    assert!(
        typeck("let v: Int = ai.chat(\"hi\")\nprint(1)\n").is_err(),
        "`ai.chat` 返回 String，必须拒绝 Int"
    );
    assert!(
        typeck("let v: Int = ai.critic(\"a\")\nprint(1)\n").is_err(),
        "`ai.critic` 返回 Dict，必须拒绝 Int"
    );
}
