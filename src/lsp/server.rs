//! LSP server 核心：消息循环 + 路由
//!
//! 设计：
//! - 单线程同步循环：read → parse → dispatch → write response
//! - DocumentManager 持有打开文档（uri → 文本 + AST + typeck errors）
//! - 路由表：method 字符串 → handler 函数
//!
//! 不实现：cancel（v1 协议太复杂）、progress（用不上）、window/workDoneProgress。

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::sync::Mutex;

use super::json::{Parser, Value};
use super::transport;

pub struct Server {
    stdin: BufReader<io::Stdin>,
    docs: Mutex<HashMap<String, DocumentState>>,
    shutdown: Mutex<bool>,
}

/// 一份打开文档的内部状态
pub struct DocumentState {
    pub uri: String,
    pub text: String,
    pub version: i64,
    /// 最新一次 typeck 的错误（用 LSP 推送 diagnostics）
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub line: usize,   // 0-based
    pub column: usize, // 0-based
    pub end_line: usize,
    pub end_column: usize,
    pub severity: u8, // 1=Error, 2=Warning, 3=Info, 4=Hint
    pub message: String,
    pub source: String, // "mora-typeck"
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Self {
            stdin: BufReader::new(io::stdin()),
            docs: Mutex::new(HashMap::new()),
            shutdown: Mutex::new(false),
        }
    }

    /// 主循环
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            let raw = match transport::read_message(&mut self.stdin)? {
                Some(s) => s,
                None => return Ok(()), // EOF
            };
            let msg = match Parser::new(&raw).parse_value() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[mora-lsp] failed to parse incoming message: {}", e);
                    continue;
                }
            };
            self.handle_message(msg)?;
            if *self.shutdown.lock().expect("shutdown mutex poisoned") {
                break;
            }
        }
        Ok(())
    }

    fn handle_message(&mut self, msg: Value) -> io::Result<()> {
        let method = msg
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // notification：没有 id 的消息
        if id.is_none() {
            self.handle_notification(&method, params);
            return Ok(());
        }

        // request：必须回复（成功或错误）
        let id = id.expect("id should exist");
        match self.handle_request(&method, params) {
            Ok(result) => self.send_response(id, Some(result), None)?,
            Err(err_msg) => self.send_response(id, None, Some(err_msg))?,
        }
        Ok(())
    }

    fn handle_notification(&mut self, method: &str, params: Value) {
        match method {
            "initialized" => {
                // 客户端握手完成标志；什么都不做
            }
            "exit" => {
                *self.shutdown.lock().expect("shutdown mutex poisoned") = true;
            }
            "textDocument/didOpen" => {
                if let Some(doc) = parse_doc_params(&params) {
                    let diags = self.check_diagnostics(&doc.text);
                    let uri = doc.uri.clone();
                    let mut docs = self.docs.lock().expect("docs mutex poisoned");
                    docs.insert(
                        uri.clone(),
                        DocumentState {
                            diagnostics: diags.clone(),
                            ..doc
                        },
                    );
                    drop(docs);
                    let _ = self.publish_diagnostics(&uri, &diags);
                }
            }
            "textDocument/didChange" => {
                if let Some((uri, version, text)) = parse_change_params(&params) {
                    let diags = self.check_diagnostics(&text);
                    let mut docs = self.docs.lock().expect("docs mutex poisoned");
                    docs.insert(
                        uri.clone(),
                        DocumentState {
                            uri: uri.clone(),
                            text,
                            version,
                            diagnostics: diags.clone(),
                        },
                    );
                    drop(docs);
                    let _ = self.publish_diagnostics(&uri, &diags);
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = params
                    .get("textDocument")
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str())
                {
                    self.docs.lock().expect("docs mutex poisoned").remove(uri);
                }
            }
            "textDocument/didSave" => {
                if let Some(uri) = params
                    .get("textDocument")
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str())
                {
                    let text_opt = self
                        .docs
                        .lock()
                        .expect("docs mutex poisoned")
                        .get(uri)
                        .map(|d| d.text.clone());
                    if let Some(text) = text_opt {
                        let diags = self.check_diagnostics(&text);
                        let mut docs = self.docs.lock().expect("docs mutex poisoned");
                        if let Some(d) = docs.get_mut(uri) {
                            d.diagnostics = diags.clone();
                        }
                        drop(docs);
                        let _ = self.publish_diagnostics(uri, &diags);
                    }
                }
            }
            _ => {
                // 忽略未知 notification
            }
        }
    }

    fn handle_request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(self.handle_initialize(params)),
            "shutdown" => {
                *self.shutdown.lock().expect("shutdown mutex poisoned") = true;
                Ok(Value::Null)
            }
            "textDocument/hover" => self.handle_hover(params),
            "textDocument/completion" => Ok(self.handle_completion(params)),
            "textDocument/definition" => Ok(self.handle_definition(params)),
            "textDocument/references" => Ok(self.handle_references(params)),
            "textDocument/documentSymbol" => Ok(self.handle_document_symbol(params)),
            "textDocument/formatting" => Ok(self.handle_formatting(params, false)),
            "textDocument/rangeFormatting" => Ok(self.handle_formatting(params, true)),
            "textDocument/rename" => Ok(self.handle_rename(params)),
            "textDocument/semanticTokens/full" => Ok(self.handle_semantic_tokens(params)),
            "textDocument/foldingRange" => Ok(self.handle_folding_range(params)),
            _ => Err(format!("method not supported: {}", method)),
        }
    }

    // ---------------------------------------------------------------
    // initialize
    // ---------------------------------------------------------------
    fn handle_initialize(&self, _params: Value) -> Value {
        // 报告 server 能力
        let mut capabilities: std::collections::BTreeMap<String, Value> =
            std::collections::BTreeMap::new();

        // textDocumentSync
        let mut tds = std::collections::BTreeMap::new();
        tds.insert("openClose".to_string(), Value::Bool(true));
        tds.insert("change".to_string(), Value::Number(1.0));
        tds.insert("save".to_string(), Value::Bool(true));
        capabilities.insert("textDocumentSync".to_string(), Value::Object(tds));

        capabilities.insert("hoverProvider".to_string(), Value::Bool(true));

        // completionProvider
        let mut cp = std::collections::BTreeMap::new();
        cp.insert(
            "triggerCharacters".to_string(),
            Value::Array(vec![Value::String_(":".to_string())]),
        );
        capabilities.insert("completionProvider".to_string(), Value::Object(cp));

        capabilities.insert("definitionProvider".to_string(), Value::Bool(true));
        capabilities.insert("referencesProvider".to_string(), Value::Bool(true));
        capabilities.insert("documentSymbolProvider".to_string(), Value::Bool(true));
        capabilities.insert("documentFormattingProvider".to_string(), Value::Bool(true));
        capabilities.insert(
            "documentRangeFormattingProvider".to_string(),
            Value::Bool(true),
        );
        capabilities.insert("renameProvider".to_string(), Value::Bool(true));
        capabilities.insert("foldingRangeProvider".to_string(), Value::Bool(true));

        // semanticTokensProvider
        let mut token_types = Vec::new();
        for t in [
            "keyword", "function", "variable", "string", "number", "comment", "type", "operator",
        ] {
            token_types.push(Value::String_(t.to_string()));
        }
        let mut token_mods = Vec::new();
        for t in ["declaration", "definition"] {
            token_mods.push(Value::String_(t.to_string()));
        }
        let mut legend = std::collections::BTreeMap::new();
        legend.insert("tokenTypes".to_string(), Value::Array(token_types));
        legend.insert("tokenModifiers".to_string(), Value::Array(token_mods));
        let mut stp = std::collections::BTreeMap::new();
        stp.insert("legend".to_string(), Value::Object(legend));
        stp.insert("full".to_string(), Value::Bool(true));
        capabilities.insert("semanticTokensProvider".to_string(), Value::Object(stp));

        let mut server_info = std::collections::BTreeMap::new();
        server_info.insert("name".to_string(), Value::String_("mora-lsp".to_string()));
        server_info.insert("version".to_string(), Value::String_("0.1".to_string()));

        let mut result = std::collections::BTreeMap::new();
        result.insert("capabilities".to_string(), Value::Object(capabilities));
        result.insert("serverInfo".to_string(), Value::Object(server_info));
        Value::Object(result)
    }

    // ---------------------------------------------------------------
    // 各 LSP method 的占位实现 — 真正逻辑在 providers 模块
    // ---------------------------------------------------------------
    fn handle_hover(&self, params: Value) -> Result<Value, String> {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::hover_v2(&docs, &params)
    }

    fn handle_completion(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::completion_v2(&docs, &params)
    }

    fn handle_definition(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::definition_v2(&docs, &params)
    }

    fn handle_references(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::references_v2(&docs, &params)
    }

    fn handle_document_symbol(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::document_symbol_v2(&docs, &params)
    }

    fn handle_formatting(&self, params: Value, range: bool) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::formatting(&docs, &params, range)
    }

    fn handle_rename(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::rename_v2(&docs, &params)
    }

    fn handle_semantic_tokens(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::semantic_tokens(&docs, &params)
    }

    fn handle_folding_range(&self, params: Value) -> Value {
        let docs = self.docs.lock().expect("docs mutex poisoned");
        super::providers::folding_range_v2(&docs, &params)
    }

    // ---------------------------------------------------------------
    // Diagnostics（typeck → LSP Diagnostic）
    // ---------------------------------------------------------------
    fn check_diagnostics(&self, text: &str) -> Vec<Diagnostic> {
        use crate::typeck;

        let stmts = crate::interpreter::parse_code(text);
        let errs = typeck::check_program(&stmts);
        errs.into_iter()
            .map(|e| {
                // v0.05: line/column 都是 1-based (typeck)，LSP 是 0-based
                //   column 默认 0 表示"未知"，减 1 后 = -1 → saturating_sub 保证不溢出
                let line_0 = e.line.saturating_sub(1);
                let col_0 = e.column.saturating_sub(1);
                // v0.05: 把 expected/actual/hint 拼到 message 里（LSP 暂不支持结构化字段）
                let mut message = e.message.clone();
                if e.expected.is_some() || e.actual.is_some() || e.hint.is_some() {
                    message.push('\n');
                    if let Some(exp) = &e.expected {
                        message.push_str(&format!("  expected: {}\n", exp));
                    }
                    if let Some(act) = &e.actual {
                        message.push_str(&format!("  actual:   {}\n", act));
                    }
                    if let Some(hint) = &e.hint {
                        message.push_str(&format!("  hint:     {}\n", hint));
                    }
                    // 去掉末尾换行
                    message = message.trim_end_matches('\n').to_string();
                }
                // v0.05: end 列号策略
                //   - column > 0 → 精确定位 (col_0 + 1)
                //   - column = 0 (未知) → 整行标记 (end_column = 1，让 VS Code 高亮行首)
                let end_col_0 = if e.column == 0 { 1 } else { col_0 + 1 };
                Diagnostic {
                    line: line_0,
                    column: col_0,
                    end_line: line_0,
                    end_column: end_col_0,
                    severity: 1,
                    message,
                    source: "mora-typeck".to_string(),
                }
            })
            .collect()
    }

    fn publish_diagnostics(&self, uri: &str, diags: &[Diagnostic]) -> io::Result<()> {
        let mut params = std::collections::BTreeMap::new();
        params.insert("uri".to_string(), Value::String_(uri.to_string()));
        let arr: Vec<Value> = diags
            .iter()
            .map(|d| {
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "range".to_string(),
                    Value::Object({
                        let mut r = std::collections::BTreeMap::new();
                        r.insert(
                            "start".to_string(),
                            Value::Object({
                                let mut p = std::collections::BTreeMap::new();
                                p.insert("line".to_string(), Value::Number(d.line as f64));
                                p.insert("character".to_string(), Value::Number(d.column as f64));
                                p
                            }),
                        );
                        r.insert(
                            "end".to_string(),
                            Value::Object({
                                let mut p = std::collections::BTreeMap::new();
                                p.insert("line".to_string(), Value::Number(d.end_line as f64));
                                p.insert(
                                    "character".to_string(),
                                    Value::Number(d.end_column as f64),
                                );
                                p
                            }),
                        );
                        r
                    }),
                );
                m.insert("severity".to_string(), Value::Number(d.severity as f64));
                m.insert("source".to_string(), Value::String_(d.source.clone()));
                m.insert("message".to_string(), Value::String_(d.message.clone()));
                Value::Object(m)
            })
            .collect();
        params.insert("diagnostics".to_string(), Value::Array(arr));

        let mut notif = std::collections::BTreeMap::new();
        notif.insert("jsonrpc".to_string(), Value::String_("2.0".to_string()));
        notif.insert(
            "method".to_string(),
            Value::String_("textDocument/publishDiagnostics".to_string()),
        );
        notif.insert("params".to_string(), Value::Object(params));
        let body = Value::Object(notif).to_string();
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        transport::write_message(&mut lock, &body)
    }

    fn send_response(
        &self,
        id: Value,
        result: Option<Value>,
        err: Option<String>,
    ) -> io::Result<()> {
        let mut msg = std::collections::BTreeMap::new();
        msg.insert("jsonrpc".to_string(), Value::String_("2.0".to_string()));
        msg.insert("id".to_string(), id);
        match (result, err) {
            (Some(r), None) => {
                msg.insert("result".to_string(), r);
            }
            (None, Some(e)) => {
                let mut err_obj = std::collections::BTreeMap::new();
                err_obj.insert("code".to_string(), Value::Number(-32603.0));
                err_obj.insert("message".to_string(), Value::String_(e));
                msg.insert("error".to_string(), Value::Object(err_obj));
            }
            _ => {
                msg.insert("result".to_string(), Value::Null);
            }
        }
        let body = Value::Object(msg).to_string();
        let stdout = io::stdout();
        let mut lock = stdout.lock();
        transport::write_message(&mut lock, &body)
    }
}

pub fn parse_doc_params(params: &Value) -> Option<DocumentState> {
    let td = params.get("textDocument")?;
    let uri = td.get("uri")?.as_str()?.to_string();
    let text = td.get("text")?.as_str()?.to_string();
    let version = td.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(DocumentState {
        uri,
        text,
        version,
        diagnostics: vec![],
    })
}

pub fn parse_change_params(params: &Value) -> Option<(String, i64, String)> {
    let td = params.get("textDocument")?;
    let uri = td.get("uri")?.as_str()?.to_string();
    let version = td.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
    let changes = params.get("contentChanges")?.as_array()?;
    // Full sync: 只取最后一条（按 LSP 规范 full sync 只发一条）
    let last = changes.last()?;
    let text = last.get("text")?.as_str()?.to_string();
    Some((uri, version, text))
}
