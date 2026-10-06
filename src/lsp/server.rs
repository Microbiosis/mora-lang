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
            if *self
                .shutdown
                .lock()
                .map_err(|_| io::Error::other("shutdown mutex poisoned"))?
            {
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
            return self.handle_notification(&method, params);
        }

        // request：必须回复（成功或错误）
        if let Some(id) = id {
            match self.handle_request(&method, params) {
                Ok(result) => self.send_response(id, Some(result), None)?,
                Err(err_msg) => self.send_response(id, None, Some(err_msg))?,
            }
        }
        Ok(())
    }

    fn handle_notification(&mut self, method: &str, params: Value) -> io::Result<()> {
        match method {
            "initialized" => {
                // 客户端握手完成标志；什么都不做
            }
            "exit" => {
                *self
                    .shutdown
                    .lock()
                    .map_err(|_| io::Error::other("shutdown mutex poisoned"))? = true;
            }
            "textDocument/didOpen" => {
                if let Some(doc) = parse_doc_params(&params) {
                    let diags = self.check_diagnostics(&doc.text);
                    let uri = doc.uri.clone();
                    let mut docs = self
                        .docs
                        .lock()
                        .map_err(|_| io::Error::other("docs mutex poisoned"))?;
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
                if let Some((uri, version, changes)) = parse_change_params(&params) {
                    // v0.104.6 D194：变更要**基于当前文档**应用 —— 增量变更
                    // 必须拿到现有文本才能按 `range` 拼接（修前取最后一条的
                    // `text` 当整份文档，增量编辑会丢掉整个文件）。
                    let base = {
                        let docs = self
                            .docs
                            .lock()
                            .map_err(|_| io::Error::other("docs mutex poisoned"))?;
                        docs.get(&uri).map(|d| d.text.clone()).unwrap_or_default()
                    };
                    let text = apply_content_changes(&base, &changes);
                    let diags = self.check_diagnostics(&text);
                    let mut docs = self
                        .docs
                        .lock()
                        .map_err(|_| io::Error::other("docs mutex poisoned"))?;
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
                    self.docs
                        .lock()
                        .map_err(|_| io::Error::other("docs mutex poisoned"))?
                        .remove(uri);
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
                        .map_err(|_| io::Error::other("docs mutex poisoned"))?
                        .get(uri)
                        .map(|d| d.text.clone());
                    if let Some(text) = text_opt {
                        let diags = self.check_diagnostics(&text);
                        let mut docs = self
                            .docs
                            .lock()
                            .map_err(|_| io::Error::other("docs mutex poisoned"))?;
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
        Ok(())
    }

    fn handle_request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "initialize" => Ok(self.handle_initialize(params)),
            "shutdown" => {
                *self
                    .shutdown
                    .lock()
                    .map_err(|_| "shutdown mutex poisoned".to_string())? = true;
                Ok(Value::Null)
            }
            "textDocument/hover" => self.handle_hover(params),
            "textDocument/completion" => self.handle_completion(params),
            "textDocument/definition" => self.handle_definition(params),
            "textDocument/references" => self.handle_references(params),
            "textDocument/documentSymbol" => self.handle_document_symbol(params),
            "textDocument/formatting" => self.handle_formatting(params, false),
            "textDocument/rangeFormatting" => self.handle_formatting(params, true),
            "textDocument/rename" => self.handle_rename(params),
            "textDocument/semanticTokens/full" => self.handle_semantic_tokens(params),
            "textDocument/foldingRange" => self.handle_folding_range(params),
            _ => Err(format!("method not supported: {}", method)),
        }
    }

    // ============================================================
    // initialize
    // ============================================================
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
        // v0.104.6 D195：撤掉 `":"` 这个 triggerCharacter。
        //
        // 此前 capabilities 声明 `triggerCharacters: [":"]` —— 告诉编辑器
        // 「用户一打冒号就向我请求补全」。而实测**在那个位置返回空**：
        //
        // ```text
        // 普通位置 print(x) 的 →  →  35 条
        // 紧跟 `let x: `            →   0 条
        // with 块内缩进行            →   0 条
        // ```
        //
        // 即**宣称会在某个位置提供补全，却在那里什么都不给** ——
        // 与 D175 的 `methods_of` 空集、D186 的 MCP 名字目录同族。
        // 用户每打一个冒号（类型标注 / dict 字面量 / `with` 块）都会
        // 闪一个空列表。
        //
        // **撤声明而不是补实现**：真正兑现这个触发需要一份「类型名清单」，
        // 而那必然是**第三份**要维护的名字表（已有 `Type` 枚举与
        // `typeck` 的类型名映射），正是 D175/D189 记过的那种漂移陷阱。
        // 等真要做补全时连同内容一起加，并把这行注释改回去。
        //
        // 判据 `tests/lsp_completion_trigger_honesty.rs` 是**自适应**的：
        // 它允许将来重新声明 `":"` —— 条件是那时在冒号后**确实**返回条目。
        cp.insert("triggerCharacters".to_string(), Value::Array(Vec::new()));
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
        // v0.104.6 D201：legend 与 semantic.rs 里的索引**同源** ——
        // 此前两边各写各的，服务器发出的索引全部越界。
        let mut token_types = Vec::new();
        for t in super::providers::semantic::TOKEN_TYPES {
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
        server_info.insert(
            "version".to_string(),
            Value::String_(crate::VERSION.to_string()),
        );

        let mut result = std::collections::BTreeMap::new();
        result.insert("capabilities".to_string(), Value::Object(capabilities));
        result.insert("serverInfo".to_string(), Value::Object(server_info));
        Value::Object(result)
    }

    // ============================================================
    // 各 LSP method 的占位实现 — 真正逻辑在 providers 模块
    // ============================================================
    fn handle_hover(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        super::providers::hover_v3(&docs, &params)
    }

    fn handle_completion(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::completion_v3(&docs, &params))
    }

    fn handle_definition(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::definition_v3(&docs, &params))
    }

    fn handle_references(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::references_v3(&docs, &params))
    }

    fn handle_document_symbol(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::document_symbol_v3(&docs, &params))
    }

    fn handle_formatting(&self, params: Value, range: bool) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::formatting(&docs, &params, range))
    }

    fn handle_rename(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::rename_v3(&docs, &params))
    }

    fn handle_semantic_tokens(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::semantic_tokens_v3(&docs, &params))
    }

    fn handle_folding_range(&self, params: Value) -> Result<Value, String> {
        let docs = self
            .docs
            .lock()
            .map_err(|_| "docs mutex poisoned".to_string())?;
        Ok(super::providers::folding_range_v3(&docs, &params))
    }

    // ============================================================
    // Diagnostics（typeck → LSP Diagnostic）
    // ============================================================
    /// v0.104.6 D101：把 parser 的错误消息转成一条 LSP `Diagnostic`。
    ///
    /// 行号优先取消息里的 `at line N`（parser 的位置格式，1-based → 0-based），
    /// 退化到 `line N` / `第 N 行`，都没有则落在第 0 行。列号一律 0 ——
    /// parser 的错误消息不带列号，与其编一个不如留 0（编辑器会指向行首）。
    fn parse_error_diagnostic(msg: &str) -> Diagnostic {
        let line_1 = ["at line ", "line ", "第 "]
            .iter()
            .find_map(|marker| {
                let i = msg.find(marker)? + marker.len();
                let rest = &msg[i..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if digits.is_empty() {
                    None
                } else {
                    digits.parse::<usize>().ok()
                }
            })
            .unwrap_or(1);
        Diagnostic {
            line: line_1.saturating_sub(1),
            column: 0,
            end_line: line_1.saturating_sub(1),
            end_column: 1,
            severity: 1, // Error
            message: msg.to_string(),
            source: "mora-parser".to_string(),
        }
    }

    fn check_diagnostics(&self, text: &str) -> Vec<Diagnostic> {
        // v0.75.40: 单遍编译（compile 直接产出 witness），typeck 直接消费
        let (_, witnesses) = match crate::parser_v3::ParserV3::compile(text) {
            Ok(pair) => pair,
            // v0.104.6 D101：此前 `Err(_) => return Vec::new()` —— **语法错误
            // 产生零诊断**。实测 `let = = =` 在 `mora run` 下报
            // 「Parse error: Expected variable name after 'let' at line 1」，
            // 而 LSP 推送 `diagnostics: []`，即**告诉用户「代码没问题」**。
            // 语法错误是语言服务器最基本的能力，这一吞让整条诊断链在此处失效。
            //
            // 改为把 parse 错误本身作为一条 Error 诊断推出。行号从消息里的
            // `at line N` 提取（1-based → 0-based）；提取不到时落在第 0 行。
            Err(e) => return vec![Self::parse_error_diagnostic(&e)],
        };
        let errs = crate::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
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

/// 从 `contentChanges` 的某一条里取出 `(line, character)`。
fn pos_of(v: Option<&Value>) -> (usize, usize) {
    let p = match v {
        Some(p) => p,
        None => return (0, 0),
    };
    // v0.104.6 D245：转发到 `providers::parsed_doc_v3::pos_of` —— 那才是
    // 全部入站位置的**唯一收口**。此前本函数自带 `.max(0)`（D194）而
    // `definition` / `hover` / `formatting` 三处各写一遍且**都没有**守卫，
    // 同一个仓库里两套行为。现四处共用一处。
    super::providers::parsed_doc_v3::pos_of(p.get("line"), p.get("character"))
}

/// v0.104.6 D194：按 LSP 规范应用 `contentChanges`。
///
/// 服务器在 capabilities 里声明 `textDocumentSync.change: 1`（**Incremental**），
/// 于是**带 `range` 的变更必须按范围拼接**。
///
/// 修前 `parse_change_params` 只取**最后一条**的 `text` 当作整份文档
/// （注释还写着「Full sync: 只取最后一条」）—— 全量同步下碰巧正确，
/// 但增量同步下**每个字符都会把整个文件替换掉**。实测（真实 `mora-lsp.exe`）：
///
/// ```text
/// didOpen  "let a = 1\nprint(a)\n"
/// didChange range=(0,8)-(0,9) text="2"      ← 只把 '1' 改成 '2'
///   → 文档实际变成 "2"
///   → documentSymbol 返回 []（编辑前返回 [a]）
///   → hover 在 (0,4) 报 "variable 2"
/// ```
///
/// 而全量变更（无 `range`）一切正常 —— 于是「只在真实编辑里出现」。
/// 影响：任何按声明使用增量同步的编辑器，**敲第一个键就丢掉整个文件**，
/// 之后 hover / 符号 / 诊断 / 补全全部空转。
pub fn apply_content_changes(base: &str, changes: &[Value]) -> String {
    use crate::lsp::providers::parsed_doc_v3::position_to_offset;
    let mut text = base.to_string();
    for ch in changes {
        let new_text = ch.get("text").and_then(|t| t.as_str()).unwrap_or("");
        match ch.get("range") {
            // 增量：按范围拼接
            Some(r) => {
                let (sl, sc) = pos_of(r.get("start"));
                let (el, ec) = pos_of(r.get("end"));
                let s = position_to_offset(&text, sl, sc);
                let e = position_to_offset(&text, el, ec).max(s);
                text.replace_range(s..e, new_text);
            }
            // 全量：整份替换（规范允许同一次请求里混合）
            None => text = new_text.to_string(),
        }
    }
    text
}

/// v0.104.6 D194：返回 `(uri, version, contentChanges 原样)`。
///
/// 变更**怎么应用**交给 `apply_content_changes` —— 它要按 `range` 拼接，
/// 而拼接必须基于当前文档，不能在这里就把文本定死。
pub fn parse_change_params(params: &Value) -> Option<(String, i64, Vec<Value>)> {
    let td = params.get("textDocument")?;
    let uri = td.get("uri")?.as_str()?.to_string();
    let version = td.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
    let changes = params.get("contentChanges")?.as_array()?.clone();
    Some((uri, version, changes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_notification_without_id_no_panic() {
        // v0.34: 没有 id 的 JSON-RPC notification 不应 panic
        let mut server = Server::new();
        let msg = Value::Object({
            let mut m = std::collections::BTreeMap::new();
            m.insert(
                "method".to_string(),
                Value::String_("initialized".to_string()),
            );
            m
        });
        // 不应 panic
        server.handle_message(msg).unwrap();
    }
}
