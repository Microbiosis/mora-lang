//! AI 辅助函数
//!
//! 从 interpreter/mod.rs 提取的 AI 相关辅助方法：
//! - build_chat_messages_json: 构建 ChatMessage JSON
//! - extract_chat_response: 解析 AI 响应
//! - extract_usage: 提取 token 使用量
//! - track_tokens: token 预算追踪
//! - extract_ai_content: 提取 AI 内容
//! - read_next_sse_token: SSE 流读取
//! - parse_critic_response: 解析 critic 响应
//! - mock_critic: mock critic
//! - create_mock_stream: mock 流

use super::*;
use crate::value::Value;

impl Interpreter {
    pub(super) fn build_chat_messages_json(messages: &[ChatMessage]) -> String {
        let parts: Vec<String> = messages.iter().map(|msg| {
            match msg {
                ChatMessage::User { content } => {
                    let esc = content.replace('\\', "\\\\").replace('"', "\\\"")
                        .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                    format!(r#"{{"role":"user","content":"{}"}}"#, esc)
                }
                ChatMessage::Assistant { content, tool_calls } => {
                    let mut parts = vec![r#""role":"assistant""#.to_string()];
                    match content {
                        Some(c) => {
                            let esc = c.replace('\\', "\\\\").replace('"', "\\\"")
                                .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                            parts.push(format!(r#""content":"{}""#, esc));
                        }
                        None => parts.push(r#""content":null"#.to_string()),
                    }
                    if !tool_calls.is_empty() {
                        let tc_json: Vec<String> = tool_calls.iter().map(|tc| {
                            format!(
                                r#"{{"id":"{}","type":"function","function":{{"name":"{}","arguments":"{}"}}}}"#,
                                tc.id.replace('\\', "\\\\").replace('"', "\\\""),
                                tc.name.replace('\\', "\\\\").replace('"', "\\\""),
                                tc.arguments.replace('\\', "\\\\").replace('"', "\\\"")
                            )
                        }).collect();
                        parts.push(format!(r#""tool_calls":[{}]"#, tc_json.join(",")));
                    }
                    format!("{{{}}}", parts.join(","))
                }
                ChatMessage::Tool { tool_call_id, content } => {
                    let esc_id = tool_call_id.replace('\\', "\\\\").replace('"', "\\\"");
                    let esc_content = content.replace('\\', "\\\\").replace('"', "\\\"")
                        .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
                    format!(r#"{{"role":"tool","tool_call_id":"{}","content":"{}"}}"#, esc_id, esc_content)
                }
            }
        }).collect();
        parts.join(",")
    }

    /// 从 API 响应中提取 content 和 tool_calls
    pub(super) fn extract_chat_response(
        json_text: &str,
    ) -> Result<(Option<String>, Vec<ToolCall>), String> {
        let root = json_to_value(json_text)?;
        if let Value::Dict(map) = root
            && let Some(Value::List(choices)) = map.get("choices")
            && let Some(Value::Dict(choice_map)) = choices.first()
            && let Some(Value::Dict(msg_map)) = choice_map.get("message")
        {
            // 提取 content
            let content = match msg_map.get("content") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            // 提取 tool_calls
            let mut tool_calls = Vec::new();
            if let Some(Value::List(tc_list)) = msg_map.get("tool_calls") {
                for tc_val in tc_list {
                    if let Value::Dict(tc_map) = tc_val {
                        let id = match tc_map.get("id") {
                            Some(Value::String(s)) => s.clone(),
                            _ => format!("call_{}", tool_calls.len()),
                        };
                        if let Some(Value::Dict(func_map)) = tc_map.get("function") {
                            let name = match func_map.get("name") {
                                Some(Value::String(s)) => s.clone(),
                                _ => continue,
                            };
                            let arguments = match func_map.get("arguments") {
                                Some(Value::String(s)) => s.clone(),
                                _ => "{}".to_string(),
                            };
                            tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments,
                            });
                        }
                    }
                }
            }
            return Ok((content, tool_calls));
        }
        Err("Could not parse chat response".to_string())
    }

    /// 从 API 响应中提取 usage（prompt_tokens, completion_tokens）
    pub(super) fn extract_usage(json_text: &str) -> (usize, usize) {
        if let Ok(Value::Dict(map)) = json_to_value(json_text)
            && let Some(Value::Dict(usage)) = map.get("usage")
        {
            // LLM API 偶发不返回 usage 字段或返回负值；缺省按 0 计费统计。
            let input = match usage.get("prompt_tokens") {
                Some(v) => crate::flow::usize_from_value(v, "usage.prompt_tokens").unwrap_or(0),
                None => 0,
            };
            let output = match usage.get("completion_tokens") {
                Some(v) => crate::flow::usize_from_value(v, "usage.completion_tokens").unwrap_or(0),
                None => 0,
            };
            return (input, output);
        }
        (0, 0)
    }

    /// 记录 token 消耗并检查预算
    pub(super) fn track_tokens(&mut self, input: usize, output: usize) -> Result<(), String> {
        // v0.15: 检查每次调用上限
        if let Some(ref budget) = self.ai.token_budget
            && let Some(per_call) = budget.per_call
        {
            let call_total = input + output;
            if call_total > per_call {
                return Err(format!(
                    "Token per-call limit exceeded: this call used {}, limit is {}",
                    call_total, per_call
                ));
            }
        }

        self.ai.token_usage.input += input;
        self.ai.token_usage.output += output;
        self.ai.trace.record_tokens(input as u64, output as u64);
        let total_used = self.ai.token_usage.input + self.ai.token_usage.output;
        if let Some(ref budget) = self.ai.token_budget {
            if total_used > budget.total {
                return Err(format!(
                    "Token budget exceeded: used {}/{}",
                    total_used, budget.total
                ));
            }
            let ratio = total_used as f64 / budget.total as f64;
            if ratio >= budget.alert_threshold {
                eprintln!(
                    "[ai.budget warning] Token usage at {:.0}% ({}/{})",
                    ratio * 100.0,
                    total_used,
                    budget.total
                );
            }
        }
        Ok(())
    }

    // 检查预算是否已耗尽

    pub(super) fn extract_ai_content(&self, json_text: &str) -> Result<Value, String> {
        let root = json_to_value(json_text)?;

        // root 应该是 Dict，提取 "choices" 数组
        if let Value::Dict(map) = &root {
            if let Some(Value::List(choices)) = map.get("choices")
                && let Some(Value::Dict(choice_map)) = choices.first()
            {
                // 标准格式: choices[0].message.content
                if let Some(Value::Dict(msg_map)) = choice_map.get("message")
                    && let Some(Value::String(content)) = msg_map.get("content")
                {
                    return Ok(Value::String(content.clone()));
                }
                // 兼容格式: choices[0].text (旧版 completions API)
                if let Some(Value::String(text)) = choice_map.get("text") {
                    return Ok(Value::String(text.clone()));
                }
            }
            // 兼容某些 API 的顶层 "content" 字段
            if let Some(Value::String(content)) = map.get("content") {
                return Ok(Value::String(content.clone()));
            }
        }

        Err("Could not extract content from API response".to_string())
    }

    // ===================================================================
    // v0.03: 流式输出 (ai.stream)
    // ===================================================================

    /// 从 SSE 流中读取下一个 token
    /// 返回 Ok(Some(token)) — 有新 token
    /// 返回 Ok(None) — 流结束 [DONE]
    /// 返回 Err — 解析错误
    pub(super) fn read_next_sse_token(
        reader: &mut BufReader<Box<dyn Read + Send + Sync>>,
    ) -> Result<Option<String>, String> {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return Ok(None), // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Some(data) = trimmed.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return Ok(None);
                        }
                        // 解析 JSON，提取 choices[0].delta.content
                        if let Ok(Value::Dict(map)) = json_to_value(data)
                            && let Some(Value::List(choices)) = map.get("choices")
                            && let Some(Value::Dict(choice_map)) = choices.first()
                        {
                            if let Some(Value::Dict(delta)) = choice_map.get("delta")
                                && let Some(Value::String(content)) = delta.get("content")
                                && !content.is_empty()
                            {
                                return Ok(Some(content.clone()));
                            }
                            // finish_reason 字段出现但无 content，跳过
                            if choice_map.contains_key("finish_reason") {
                                continue;
                            }
                        }
                        // JSON 解析失败或无 content，跳过此行
                    }
                    // 非 data: 开头的行（event:, id:, retry:），跳过
                }
                Err(e) => return Err(format!("SSE read error: {}", e)),
            }
        }
    }

    /// 解析 critic 响应为结构化 Dict
    pub(super) fn parse_critic_response(&self, response: &str, has_context: bool) -> Value {
        let mut m = HashMap::new();
        let mut score = 5.0;
        let mut verdict = "unknown".to_string();
        let mut issues = "none".to_string();
        let mut suggestion = "none".to_string();

        for line in response.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("score:") {
                if let Ok(n) = val.trim().parse::<f64>() {
                    score = n;
                }
            } else if let Some(val) = line.strip_prefix("verdict:") {
                verdict = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("issues:") {
                issues = val.trim().to_string();
            } else if let Some(val) = line.strip_prefix("suggestion:") {
                suggestion = val.trim().to_string();
            }
        }

        m.insert("score".to_string(), Value::Number(score));
        m.insert("verdict".to_string(), Value::String(verdict));
        m.insert("issues".to_string(), Value::String(issues));
        m.insert("suggestion".to_string(), Value::String(suggestion));
        if has_context {
            m.insert("hallucination_check".to_string(), Value::Bool(true));
        }
        Value::Dict(m)
    }

    /// Mock critic：基于简单启发式
    pub(super) fn mock_critic(&self, answer: &str, context: Option<&str>) -> Value {
        let mut m = HashMap::new();
        let len = answer.len();
        let score = if len < 10 {
            3.0
        } else if len < 50 {
            6.0
        } else {
            8.0
        };

        let (verdict, issues) = if let Some(ctx) = context {
            // 简单检查：回答中的词是否在上下文中出现
            let ctx_lower = ctx.to_lowercase();
            let answer_words: Vec<&str> = answer.split_whitespace().collect();
            let matched = answer_words
                .iter()
                .filter(|w| ctx_lower.contains(&w.to_lowercase()))
                .count();
            let ratio = if answer_words.is_empty() {
                0.0
            } else {
                matched as f64 / answer_words.len() as f64
            };
            if ratio > 0.5 {
                ("supported".to_string(), "none".to_string())
            } else if ratio > 0.2 {
                (
                    "partial".to_string(),
                    "some claims may not be grounded in context".to_string(),
                )
            } else {
                (
                    "hallucinated".to_string(),
                    "most claims not found in context".to_string(),
                )
            }
        } else {
            if score >= 7.0 {
                ("good".to_string(), "none".to_string())
            } else if score >= 5.0 {
                (
                    "acceptable".to_string(),
                    "could be more detailed".to_string(),
                )
            } else {
                ("poor".to_string(), "too short, lacks detail".to_string())
            }
        };

        m.insert("score".to_string(), Value::Number(score));
        m.insert("verdict".to_string(), Value::String(verdict));
        m.insert("issues".to_string(), Value::String(issues));
        m.insert(
            "suggestion".to_string(),
            Value::String("set OPENAI_API_KEY for real evaluation".to_string()),
        );
        if context.is_some() {
            m.insert("hallucination_check".to_string(), Value::Bool(true));
        }
        Value::Dict(m)
    }

    // Mock 工具调用（无 API Key 时，调用第一个注册的工具）

    /// v0.04补: mock 流占位, 无 builtin caller, 留作 v1.0 复活点
    #[allow(dead_code)]
    pub(super) fn create_mock_stream(prompt: &str) -> Value {
        let mock_text = format!("[Mock stream for: {}]", prompt);
        let mut sse_data = String::new();
        for ch in mock_text.chars() {
            let escaped = match ch {
                '\\' => "\\\\".to_string(),
                '"' => "\\\"".to_string(),
                '\n' => "\\n".to_string(),
                _ => ch.to_string(),
            };
            sse_data.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
                escaped
            ));
        }
        sse_data.push_str("data: [DONE]\n\n");

        let cursor = std::io::Cursor::new(sse_data.into_bytes());
        let reader: Box<dyn Read + Send + Sync> = Box::new(cursor);
        Value::Stream {
            reader: StreamReader::new(BufReader::new(reader)),
            done: Arc::new(Mutex::new(false)),
        }
    }
}
