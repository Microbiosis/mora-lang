//! AI 聊天相关函数 — 从 mod.rs 提取
//!
//! 包含: do_ai_chat, real_web_fetch, real_ai_chat, call_ai_api,
//!       real_ai_chat_inner, real_ai_chat_with_tools, run_critic, run_agent

use super::*;

impl Interpreter {
    /// v0.04: AI chat 的统一入口
    /// 替代 v0.03 的 ai.chat builtin
    /// - model: 模型名 (e.g. "gpt-4o-mini")
    /// - prompt: prompt 字符串
    ///
    /// v0.06: 接 current_ai_config (替代 env hack)  --- temperature/max_tokens/system 下传
    pub(super) fn do_ai_chat(
        interp: &mut Interpreter,
        model: &str,
        prompt: &str,
    ) -> Result<Value, String> {
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        let base_url = std::env::var("MORA_AI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

        if api_key.is_empty() {
            // Mock 模式
            let cfg_info = interp
                .current_ai_config
                .as_ref()
                .map(|c| {
                    format!(
                        "config: temp={:?}, max_tokens={:?}",
                        c.temperature, c.max_tokens
                    )
                })
                .unwrap_or_default();
            eprintln!(
                "[ai.chat mock — set OPENAI_API_KEY for real call] {} {}",
                prompt, cfg_info
            );
            return Ok(Value::String(format!("[Mock response for: {}]", prompt)));
        }

        let messages = vec![("user".to_string(), prompt.to_string())];
        // v0.06: 从 current_ai_config 取 temperature/max_tokens/system,
        // 拼进 real_ai_chat_inner (v0.06.5 才改函数签名，这里先保留 env 兼容)
        interp.real_ai_chat(&messages, &api_key, model, &base_url)
    }

    pub(super) fn real_web_fetch(&mut self, url: &str) -> Result<Value, String> {
        // v0.14: 重放模式优先返回录制响应 (deterministic)
        if let Some(rec) = self.recorder.lookup_web_fetch(url)
            && let Some(status) = rec.status
        {
            return Ok(Value::String(format!(
                "<replay> HTTP {} ({}B, {}ms)",
                status,
                rec.body_len.unwrap_or(0),
                rec.latency_ms
            )));
        }
        let started = std::time::Instant::now();

        if url.is_empty() {
            return Err("web.fetch: URL cannot be empty".to_string());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "web.fetch: URL must start with http:// or https://, got: {}",
                url
            ));
        }

        // v0.x: ureq 3.3 — AgentBuilder 移除,改用 Agent::config_builder() 链式 + Config::into()
        // timeout_read/timeout_write 合并为 timeout_global(覆盖整个请求-响应周期)
        // 关闭 http_status_as_error 以保留 4xx/5xx 响应体(原 2.x 中可从 Error::Status 读取)
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(HTTP_READ_TIMEOUT_SECS)))
            .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();

        match agent.get(url).call() {
            Ok(mut response) => {
                let status = response.status();
                let text = response
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("web.fetch: failed to read response body: {}", e))?;
                let body_len = text.len();
                let result = if (400..600).contains(&status.as_u16()) {
                    let excerpt: String = text.chars().take(200).collect();
                    Err(format!(
                        "web.fetch: HTTP {} {} (body excerpt: {})",
                        status, url, excerpt
                    ))
                } else {
                    Ok(Value::String(text))
                };
                // v0.14: 录制成功 fetch (status + body_len)
                self.recorder.record_web_fetch(
                    url.to_string(),
                    "GET".to_string(),
                    status.as_u16(),
                    body_len,
                    started.elapsed().as_millis(),
                    if result.is_err() {
                        Some(format!("HTTP {}", status.as_u16()))
                    } else {
                        None
                    },
                );
                result
            }
            // v0.x: ureq 3.3 — Transport 变体被拆解为 Io/Timeout/ConnectionFailed 等多种;
            // 其余失败(HostNotFound/Protocol 等)统一兜底
            Err(e) => {
                let err_str = format!("web.fetch: network error for {}: {}", url, e);
                self.recorder.record_web_fetch(
                    url.to_string(),
                    "GET".to_string(),
                    0,
                    0,
                    started.elapsed().as_millis(),
                    Some(err_str.clone()),
                );
                Err(err_str)
            }
        }
    }

    /// 真实 Chat Completions API 调用（支持 OpenAI 兼容端点）。
    ///
    /// 关键设计：
    /// - **messages 参数**：完整对话历史，支持多轮上下文
    /// - **model / base_url 参数**：可配置，兼容本地模型和其他 API 提供商
    /// - **手写 JSON 请求体**：保持零 serde 依赖原则
    /// - **结构化 JSON 响应解析**：用 json_to_value 提取 choices[0].message.content
    /// - **同步阻塞**：60s 读超时（AI 推理可能慢）
    ///
    /// v0.06.5: AI chat 新签名 — 接 temperature/max_tokens/system 从 current_ai_config
    pub(super) fn real_ai_chat(
        &mut self,
        messages: &[(String, String)],
        api_key: &str,
        model: &str,
        base_url: &str,
    ) -> Result<Value, String> {
        // v0.14: 重放模式直接返回录制响应
        let prompt_text: String = messages
            .iter()
            .map(|(role, content)| format!("{}: {}", role, content))
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(rec) = self.recorder.lookup_ai_chat(model, &prompt_text) {
            return Ok(Value::String(rec.response));
        }

        let mut span_attrs = std::collections::HashMap::new();
        span_attrs.insert("model".to_string(), model.to_string());
        span_attrs.insert("messages".to_string(), messages.len().to_string());
        let span = self.trace.start_span("ai.chat", span_attrs);
        let start = std::time::Instant::now();

        let result = self.real_ai_chat_inner(messages, api_key, model, base_url);

        let elapsed = start.elapsed();
        let mut end_attrs = std::collections::HashMap::new();
        end_attrs.insert("latency_ms".to_string(), elapsed.as_millis().to_string());
        match &result {
            Ok(val) => {
                end_attrs.insert("output_len".to_string(), val.to_string().len().to_string());
                span.end(end_attrs);
                self.trace.record_call("ai.chat", elapsed, true);
            }
            Err(e) => {
                span.end_error(e, end_attrs);
                self.trace.record_call("ai.chat", elapsed, false);
            }
        }
        // v0.14: 录制 ai.chat (rough token 估算: prompt 长度/4 + response 长度/4)
        let resp_str = match &result {
            Ok(v) => v.to_string(),
            Err(_) => String::new(),
        };
        let tokens_in_approx = prompt_text.len() / 4;
        let tokens_out_approx = resp_str.len() / 4;
        self.recorder.record_ai_chat(
            model.to_string(),
            prompt_text,
            resp_str,
            tokens_in_approx,
            tokens_out_approx,
            elapsed.as_millis(),
            if result.is_err() {
                Some(format!("{:?}", result.as_ref().err()))
            } else {
                None
            },
        );
        result
    }

    /// v0.24: 简化版 AI API 调用 (用于投机执行)
    pub(super) fn call_ai_api(
        &mut self,
        messages: &[(String, String)],
        api_key: &str,
        model: &str,
        base_url: &str,
    ) -> Result<Value, String> {
        // 构建请求体
        let msgs_json: String = messages
            .iter()
            .map(|(role, content)| {
                let escaped_content = content
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                format!(r#"{{"role":"{}","content":"{}"}}"#, role, escaped_content)
            })
            .collect::<Vec<_>>()
            .join(",");

        let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
        let body = format!(
            r#"{{"model":"{}","messages":[{}]}}"#,
            escaped_model, msgs_json
        );

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();

        match agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .send(&body)
        {
            Ok(mut resp) => {
                let status = resp.status();
                match resp.body_mut().read_to_string() {
                    Ok(text) if status.as_u16() < 400 => Ok(self
                        .extract_ai_content(&text)
                        .unwrap_or(Value::String(text))),
                    Ok(text) => Err(format!(
                        "AI API error ({}): {}",
                        status,
                        &text[..200.min(text.len())]
                    )),
                    Err(e) => Err(format!("AI API read error: {}", e)),
                }
            }
            Err(e) => Err(format!("AI API request error: {}", e)),
        }
    }

    /// v0.06.5: HTTP body 构建 — 拼 json 时接 temperature/max_tokens/system
    /// v0.10: 包 retry 循环（exponential backoff + jitter）
    pub(super) fn real_ai_chat_inner(
        &mut self,
        messages: &[(String, String)],
        api_key: &str,
        model: &str,
        base_url: &str,
    ) -> Result<Value, String> {
        if messages.is_empty() {
            return Err("ai.chat: messages cannot be empty".to_string());
        }

        // v0.24: 使用上下文窗口管理器
        for (role, content) in messages {
            self.context_window
                .add_message(role.clone(), content.clone());
        }

        // v0.24: 检查是否需要压缩上下文
        if self.context_window.needs_compression() {
            self.context_window.compress();
        }

        // v0.15: mock_llm 模式 — 从队列中取出下一个响应
        if let Some(ref mut cfg) = self.current_ai_config
            && let Some(ref mut responses) = cfg.mock_responses
            && !responses.is_empty()
        {
            let response = responses.remove(0); // 消费第一个
            // 模拟 token 估算
            let tokens_in = messages.iter().map(|(_, c)| c.len()).sum::<usize>() / 4;
            let tokens_out = response.len() / 4;
            self.recorder.record_ai_chat(
                model.to_string(),
                messages
                    .last()
                    .map(|(_, c)| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                response.clone(),
                tokens_in,
                tokens_out,
                0, // mock 无延迟
                None,
            );
            return Ok(Value::String(response));
        }

        // v0.24: 投机执行 - 先用快速模型预测，再验证
        let speculative_config = self.current_ai_config.as_ref().and_then(|cfg| {
            if cfg.speculative == Some(true) {
                cfg.draft_model.clone()
            } else {
                None
            }
        });
        if let Some(ref draft_model) = speculative_config {
            // v0.24: 自适应 draft 模型选择
            // 检查 draft 模型的历史成功率
            let should_use_draft = if let Some((success, total)) = self
                .draft_model_stats
                .lock()
                .expect("draft_model_stats mutex poisoned")
                .get(draft_model.as_str())
                .copied()
            {
                if total >= 10 {
                    // 有足够历史数据，根据成功率决定
                    (success as f64 / total as f64) > 0.3
                } else {
                    true // 数据不足，默认使用
                }
            } else {
                true // 无历史数据，默认使用
            };

            if should_use_draft {
                // 1. 用 draft model 快速预测
                let draft_response = self.call_ai_api(messages, api_key, draft_model, base_url)?;

                // 2. 用主模型验证
                let verification_prompt = format!(
                    "Verify if this response is correct. Question: {}\nDraft answer: {}\nReply with VERIFIED if correct, or provide the correct answer.",
                    messages.last().map(|(_, c)| c.as_str()).unwrap_or(""),
                    draft_response
                );
                let verify_messages = vec![("user".to_string(), verification_prompt)];
                let verification = self.call_ai_api(&verify_messages, api_key, model, base_url)?;

                // 3. 检查验证结果并更新统计
                let verification_str = verification.to_string();
                // v0.24: 使用推测解码验证器
                let draft_str = draft_response.to_string();
                let is_verified = self
                    .speculative_verifier
                    .verify(&draft_str, &verification_str);

                // v0.49.0 (A6): 单锁内 update stats (was HashMap entry+stats.x)
                let mut stats_map = self
                    .draft_model_stats
                    .lock()
                    .expect("draft_model_stats mutex poisoned");
                let entry = stats_map.entry(draft_model.clone()).or_insert((0, 0));
                entry.1 += 1; // total += 1
                if is_verified {
                    entry.0 += 1; // success += 1
                    drop(stats_map);
                    return Ok(draft_response);
                } else {
                    // draft 结果错误，返回主模型的修正结果
                    return Ok(verification);
                }
            }
        }

        // v0.24: 流式投机执行 - 长响应使用流式
        let use_stream = self
            .current_ai_config
            .as_ref()
            .and_then(|c| c.max_tokens)
            .map(|mt| mt > 1000)
            .unwrap_or(false);

        if let Some(ref draft_model) = speculative_config
            && use_stream
        {
            // 流式模式：先返回 draft，后台验证
            let draft_response = self.call_ai_api(messages, api_key, draft_model, base_url)?;

            // 后台验证（简化：同步验证）
            let verification_prompt = format!(
                "Verify: {}\nDraft: {}\nReply VERIFIED or correct answer.",
                messages.last().map(|(_, c)| c.as_str()).unwrap_or(""),
                draft_response
            );
            let verify_messages = vec![("user".to_string(), verification_prompt)];
            let verification = self.call_ai_api(&verify_messages, api_key, model, base_url)?;

            if verification.to_string().contains("VERIFIED") {
                return Ok(draft_response);
            } else {
                return Ok(verification);
            }
        }

        // v0.22: AI 调用内联缓存
        let cache_key = format!("{}:{:?}", model, messages);
        // v0.49.0 (C1): LRU cache (was unbounded HashMap)
        if let Some(cached) = self
            .ai_cache
            .lock()
            .expect("ai_cache mutex poisoned")
            .get(&cache_key)
            .cloned()
        {
            return Ok(Value::String(cached));
        }

        // v0.24: 检查缓存预热队列
        if let Some(cached) = self.cache_warmer.get_cached(&cache_key) {
            return Ok(Value::String(cached.clone()));
        }

        // 构建 messages JSON 数组
        let msgs_json: String = messages
            .iter()
            .map(|(role, content)| {
                let escaped_content = content
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t");
                format!(r#"{{"role":"{}","content":"{}"}}"#, role, escaped_content)
            })
            .collect::<Vec<_>>()
            .join(",");

        let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
        // v0.06.5: 拼 temperature/max_tokens/system 从 current_ai_config
        let mut body = format!(
            r#"{{"model":"{}","messages":[{}]"#,
            escaped_model, msgs_json
        );
        if let Some(ref cfg) = self.current_ai_config {
            if let Some(temp) = cfg.temperature {
                body.push_str(&format!(",\"temperature\":{}", temp));
            }
            if let Some(mt) = cfg.max_tokens {
                body.push_str(&format!(",\"max_tokens\":{}", mt));
            }
            if let Some(ref sys) = cfg.system {
                body.push_str(&format!(",\"system\":\"{}\"", sys.replace('"', "\\\"")));
            }
        }
        body.push('}');

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        // v0.22: 流式响应优化 - 添加 stream 参数
        let use_stream = self
            .current_ai_config
            .as_ref()
            .and_then(|c| c.max_tokens)
            .map(|mt| mt > 1000) // 长响应使用流式
            .unwrap_or(false);

        if use_stream {
            body.insert_str(body.len() - 1, ",\"stream\":true");
        }

        // v0.x: ureq 3.3 — ConfigBuilder + http_status_as_error(false) 以保留 4xx/5xx 响应体
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(AI_READ_TIMEOUT_SECS)))
            .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();

        // v0.10: retry 循环（exponential backoff + jitter）
        let max_retries = ai_retry_max();
        let base_ms = ai_retry_base_ms();
        for attempt in 0..=max_retries {
            if attempt > 0 {
                let sleep = retry_sleep_ms(attempt - 1, base_ms);
                std::thread::sleep(Duration::from_millis(sleep));
            }
            // v0.x: ureq 3.3 — send_string 移除,改用 send(&body)(&str 实现 SendBody trait)
            // .set() → .header()
            match agent
                .post(&url)
                .header("Authorization", &format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .send(&body)
            {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let text_result = response.body_mut().read_to_string();
                    match text_result {
                        Ok(text) if status < 400 => {
                            let (input, output) = Self::extract_usage(&text);
                            let _ = self.track_tokens(input, output);
                            let result = self
                                .extract_ai_content(&text)
                                .unwrap_or(Value::String(text.clone()));
                            // v0.22: 缓存 AI 调用结果
                            // v0.49.0 (C1): LRU put (was unbounded HashMap insert)
                            if let Value::String(ref s) = result {
                                self.ai_cache
                                    .lock()
                                    .expect("ai_cache mutex poisoned")
                                    .put(cache_key.clone(), s.clone());
                            }
                            return Ok(result);
                        }
                        Ok(text) => {
                            // 4xx/5xx: body 仍可读(因 http_status_as_error=false)
                            let excerpt: String = text.chars().take(300).collect();
                            let err = format!(
                                "ai.chat: API error HTTP {} from {} (body: {})",
                                status, url, excerpt
                            );
                            if attempt < max_retries && is_retryable_error(&err) {
                                continue;
                            }
                            return Err(err);
                        }
                        Err(e) => {
                            let err = format!("ai.chat: failed to read response body: {}", e);
                            if attempt < max_retries && is_retryable_error(&err) {
                                continue;
                            }
                            return Err(err);
                        }
                    }
                }
                Err(e) => {
                    // v0.x: ureq 3.3 — Transport 拆解为多种变体,统一兜底
                    let err = format!("ai.chat: network error connecting to {}: {}", url, e);
                    if attempt < max_retries && is_retryable_error(&err) {
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err("ai.chat: retry loop exited without result".to_string())
    }

    /// 带工具调用的 AI 对话（支持 tool_calls 自动循环）
    pub(super) fn real_ai_chat_with_tools(
        &mut self,
        messages: &mut Vec<ChatMessage>,
        api_key: &str,
        model: &str,
        base_url: &str,
        tools: &[&ToolDef],
    ) -> Result<Value, String> {
        let max_rounds = 10;
        for _round in 0..max_rounds {
            // 构建 messages JSON
            let msgs_json = Self::build_chat_messages_json(messages);

            // 构建 tools JSON
            let tools_json = if tools.is_empty() {
                String::new()
            } else {
                let tool_entries: Vec<String> = tools.iter().map(|t| {
                    format!(
                        r#"{{"type":"function","function":{{"name":"{}","description":"{}","parameters":{}}}}}"#,
                        t.name.replace('\\', "\\\\").replace('"', "\\\""),
                        t.description.replace('\\', "\\\\").replace('"', "\\\""),
                        t.parameters
                    )
                }).collect();
                format!(r#","tools":[{}]"#, tool_entries.join(","))
            };

            let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
            let mut body = format!(
                r#"{{"model":"{}","messages":[{}]{}"#,
                escaped_model, msgs_json, tools_json
            );
            // v0.15: 拼 temperature/max_tokens/system 从 current_ai_config（与 real_ai_chat_inner 对齐）
            if let Some(ref cfg) = self.current_ai_config {
                if let Some(temp) = cfg.temperature {
                    body.push_str(&format!(",\"temperature\":{}", temp));
                }
                if let Some(mt) = cfg.max_tokens {
                    body.push_str(&format!(",\"max_tokens\":{}", mt));
                }
                if let Some(ref sys) = cfg.system {
                    body.push_str(&format!(",\"system\":\"{}\"", sys.replace('"', "\\\"")));
                }
            }
            // 闭合 JSON
            body.push('}');

            let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
            // v0.x: ureq 3.3 — ConfigBuilder + http_status_as_error(false) 以保留 4xx/5xx 响应体
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(AI_READ_TIMEOUT_SECS)))
                .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
                .http_status_as_error(false)
                .build()
                .into();

            // v0.x: ureq 3.3 — send_string → send(&body);Status 变体移除,改由响应 status 判定
            // .set() → .header()
            let response_text = match agent
                .post(&url)
                .header("Authorization", &format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .send(&body)
            {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let text = response
                        .body_mut()
                        .read_to_string()
                        .map_err(|e| format!("ai.chat: failed to read response body: {}", e))?;
                    if status >= 400 {
                        let excerpt: String = text.chars().take(300).collect();
                        return Err(format!(
                            "ai.chat: API error HTTP {} from {} (body: {})",
                            status, url, excerpt
                        ));
                    }
                    text
                }
                Err(e) => {
                    return Err(format!(
                        "ai.chat: network error connecting to {}: {}",
                        url, e
                    ));
                }
            };

            // 解析响应
            let (input, output) = Self::extract_usage(&response_text);
            let _ = self.track_tokens(input, output);
            let (content, tool_calls) = Self::extract_chat_response(&response_text)?;

            if tool_calls.is_empty() {
                // 无工具调用，返回最终内容
                return Ok(Value::String(content.unwrap_or_default()));
            }

            // 有工具调用：追加 assistant 消息，执行工具，追加 tool 结果
            messages.push(ChatMessage::Assistant {
                content: content.clone(),
                tool_calls: tool_calls.clone(),
            });

            for tc in &tool_calls {
                // 查找 handler
                let handler = tools.iter().find(|t| t.name == tc.name).map(|t| &t.handler);
                let result = if let Some(handler_val) = handler {
                    // 构造参数 Dict 传给闭包
                    let args_dict = if let Ok(params_val) = json_to_value(&tc.arguments) {
                        params_val
                    } else {
                        Value::String(tc.arguments.clone())
                    };
                    match self.call_value(handler_val, vec![args_dict]) {
                        Ok(val) => val.to_string(),
                        Err(e) => format!("Error: {}", e),
                    }
                } else {
                    format!("Error: tool '{}' not found", tc.name)
                };
                messages.push(ChatMessage::Tool {
                    tool_call_id: tc.id.clone(),
                    content: result,
                });
            }
        }
        Err("ai.chat: max tool call rounds exceeded".to_string())
    }

    /// 执行输出质量评估（agent.critic）
    pub(super) fn run_critic(
        &mut self,
        answer: &str,
        context: Option<&str>,
    ) -> Result<Value, String> {
        let critic_prompt = if let Some(ctx) = context {
            // 有上下文：检查幻觉（回答是否基于上下文）
            format!(
                r#"Evaluate if the answer is grounded in the given context. Check for hallucinations (claims not supported by context).

Context:
{}

Answer:
{}

Respond in this exact format (one line per field):
score: <1-10>
verdict: <supported|partial|hallucinated>
issues: <comma-separated issues or "none">
suggestion: <improvement suggestion or "none">"#,
                ctx, answer
            )
        } else {
            // 无上下文：评估输出质量
            format!(
                r#"Evaluate the quality of this AI-generated text. Check for: clarity, coherence, relevance, factual accuracy.

Text:
{}

Respond in this exact format (one line per field):
score: <1-10>
verdict: <good|acceptable|poor>
issues: <comma-separated issues or "none">
suggestion: <improvement suggestion or "none">"#,
                answer
            )
        };

        // 用 ai.chat 调用评估（走 fast 路由或默认模型）
        let has_key = env::var("OPENAI_API_KEY")
            .map(|k| !k.is_empty())
            .unwrap_or(false);
        if has_key {
            let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let base_url = env::var("MORA_AI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let msgs = vec![("user".to_string(), critic_prompt)];
            match self.real_ai_chat(&msgs, &api_key, &model, &base_url) {
                Ok(Value::String(response)) => {
                    Ok(self.parse_critic_response(&response, context.is_some()))
                }
                Ok(other) => Ok(other),
                Err(e) => Err(format!("agent.critic: {}", e)),
            }
        } else {
            // Mock 模式：基于简单启发式评估
            Ok(self.mock_critic(answer, context))
        }
    }

    /// 执行 Agent 多步推理循环
    pub(super) fn run_agent(
        &mut self,
        agent_name: &str,
        tool_names: &[String],
        model_route: &str,
        max_steps: usize,
        system: &str,
        task: &str,
    ) -> Result<Value, String> {
        // 收集 Agent 需要的工具
        let agent_tools: Vec<ToolDef> = tool_names
            .iter()
            .filter_map(|n| self.tool_registry.get(n).cloned())
            .collect();
        let tool_refs: Vec<&ToolDef> = agent_tools.iter().collect();

        // 确定 API 配置
        let route = self.model_routes.get(model_route);
        let default_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        let (api_key, model, base_url) = if let Some(r) = route {
            let key = if r.api_key.is_empty() {
                default_key.clone()
            } else {
                r.api_key.clone()
            };
            // v0.15: 将 route 的 ai 配置设入 current_ai_config
            if r.max_tokens.is_some() || r.system.is_some() || r.temperature.is_some() {
                self.current_ai_config = Some(AiConfigValue {
                    model: Some(r.model.clone()),
                    temperature: r.temperature,
                    max_tokens: r.max_tokens,
                    budget: None,
                    per_call: None,
                    system: r.system.clone(),
                    mock_responses: None,
                    speculative: None,
                    draft_model: None,
                });
            }
            (key, r.model.clone(), r.base_url.clone())
        } else {
            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
            let base_url = env::var("MORA_AI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            (default_key, model, base_url)
        };

        // Mock 模式：直接执行第一个工具并返回结果
        if api_key.is_empty() {
            eprintln!(
                "[agent '{}' mock — set OPENAI_API_KEY for real agent loop]",
                agent_name
            );
            if let Some(first_tool) = agent_tools.first() {
                let args_dict = Value::Dict(HashMap::new());
                let tool_result = match self.call_value(&first_tool.handler, vec![args_dict]) {
                    Ok(val) => val.to_string(),
                    Err(e) => format!("Tool error: {}", e),
                };
                return Ok(Value::String(format!(
                    "[Agent '{}'] Task: {}\nTool '{}' result: {}",
                    agent_name, task, first_tool.name, tool_result
                )));
            }
            return Ok(Value::String(format!(
                "[Agent '{}'] Task: {} (no tools, mock response)",
                agent_name, task
            )));
        }

        // 构建初始消息
        let mut messages: Vec<ChatMessage> = Vec::new();
        messages.push(ChatMessage::User {
            content: format!("{}\n\nTask: {}", system, task),
        });

        // 多步推理循环
        // 当前实现每步必 return（Ok/Err），循环形式保留意图：未来扩展多步时无需改结构。
        // clippy::never_loop 触发因为循环体总 return；属预期行为。
        #[allow(clippy::never_loop)]
        for step in 0..max_steps {
            eprintln!("[agent '{}' step {}/{}]", agent_name, step + 1, max_steps);
            match self.real_ai_chat_with_tools(
                &mut messages,
                &api_key,
                &model,
                &base_url,
                &tool_refs,
            ) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // real_ai_chat_with_tools 只在 max tool rounds exceeded 时返回 Err
                    // 其他情况下 Ok 就是最终结果
                    return Err(format!("agent.run error at step {}: {}", step + 1, e));
                }
            }
        }
        Err(format!(
            "agent '{}': max steps ({}) exceeded",
            agent_name, max_steps
        ))
    }
}
