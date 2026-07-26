# Sub-project A: interpreter.rs Architectural Refactor — Design Spec

**Date:** 2026-06-23
**Status:** Draft (awaiting user review)
**Author:** zcode (brainstorming session)
**Parent initiative:** (ureq 2→3 , 1 )
**Series order:** A (this) → B (unsafe/panic ) → C () → D ()

---

## 1. 

### 1.1 

`src/interpreter.rs` **4413 **, 6 :

|  |  |  |  |
|---|---|---|---|
|  | 15–225 | 210 | `Value`, `Environment`, `StreamReader` |
| AI retry  | 228–272 | 45 | `ai_retry_max`, `is_retryable_error`, `retry_sleep_ms` |
| **`impl Interpreter`** | 505–3235 | 2730 | 104 , |
| (eval/json/util) | 3360–3860 | 500 | `is_truthy`, `eval_binary`, `json_to_value` |
| Embedding/ | 3826–3870 | 50 | `cosine_similarity`, `dot_product`, `l2_norm` |
|  | 4070–4413 | 343 | trait dispatch (`#[cfg(test)]`) |

### 1.2 

- **`impl Interpreter` **:104  2700 ,( /  /  / AI&HTTP /  IO / )
- **AI  3 **:`web.fetch`(2523–2572)`ai.chat`(2628–2706)`real_ai_chat_with_tools`(2720–2776) Agent  +  + 
- ** JSON **:`json_to_value` 250 (3586–3825), 3  AI , `mora::json_to_value`  API 
- ****:230  trait ,

### 1.3  A()

 B(unsafe )C()**** B , unsafe/panic , 4413 

---

## 2. 

### 2.1 

1. `interpreter.rs` ** 1000–1500 **( dispatch +  + ), 66–77%
2. `impl Interpreter`  5–6 , impl  < 800 
3. AI/HTTP  **3  → 1  + 3 **,
4. AI  `String` → `thiserror`- `AiError` enum, `Result<Value, String>` 
5. ****, `tests/` 
6. ****:
   - 84/84 
   - (`mora` / `mora-lsp`  binary) `--help``--version` 
   -  API (`mora::Value`, `mora::Interpreter`, `mora::json_to_value`)
7. release  ** < 5%**(,)

### 2.2 

- ****( sub-project C )
- **** unsafe ( sub-project B )
- **** `#[allow(dead_code)]` ( v0.x ,)
- **** `json_to_value`  serde_json()
- **** `Value` ()

---

## 3. 

### 3.1 

```
src/
 lib.rs                       ( mod , +20 )
 main.rs                      ()
 ast.rs                       ()
 lexer.rs                     ()
 parser.rs                    ()
 typeck.rs                    ()
 value.rs                     () Value, Environment, FlowSignal, StreamReader
 interpreter.rs               ()  dispatch + Interpreter struct, ~1200 
 flow.rs                      () is_truthy, eval_binary, numeric_op, values_equal
 json_compat.rs               ()  json_to_value + 
 eval/                        ()
    mod.rs                   () eval_expr / eval_stmt  + 
    call.rs                  () call_function / call_task / call_closure / call_method
    methods.rs               () call_file_method 
    prompt.rs                () eval_prompt_parts / eval_route_arg
 builtin/                     ()
    mod.rs                   ()  + 
    io.rs                    () read/write  IO
    http.rs                  () web.fetch
    ai/                      ()
        mod.rs               () ai.*  + 
        client.rs            () ureq (AiClient struct +  Agent )
        chat.rs              () ai.chat + chat_with_tools
        agent.rs             () run_agent
        critic.rs            () run_critic
        embedding.rs         () cosine / dot / l2 / mock_bow
 http_server.rs               ()
 mcp_server.rs                ()
 trace_collector.rs           ()
 ai_error.rs                  () thiserror AiError enum + Into<String> 
```

****(): `#[cfg(test)]` ,** `tests/` ** §4.3

### 3.2 

```
                    
                        main.rs       
                        lsp/mcp       
                    
                              uses
                             

                  interpreter.rs                       
   ( dispatch loop + Interpreter struct)           

                                      
                                      
   
 value.rs flow.rs eval/       builtin/        
                   mod.rs     mod.rs          
   
                                      
                                      
                     
                   eval/call   builtin/ai/  
                   eval/...     mod.rs      
                     
                                        
                                        
                                 
                                 ai/          
                                  client.rs    ai_error.rs
                                  chat.rs     
                                  agent.rs    
                                  critic.rs   
                                  embedding.rs
                                 
     
     
    
  json_compat.rs         ast.rs         
       parser.rs      
                           lexer.rs       
                           typeck.rs      
                        
```

****`ai_error.rs` , `ai/*`  `builtin/ai/mod.rs` 

### 3.3  API 

```rust
// lib.rs ()
pub mod ast;
pub mod interpreter;
pub mod lexer;
pub mod parser;
pub mod typeck;
pub mod trace_collector;

//  (, 99% )
pub mod value;
pub mod flow;
pub mod json_compat;
pub mod ai_error;

// ()
mod eval;
mod builtin;

// ()
pub use interpreter::Interpreter;
pub use value::{Value, Environment, FlowSignal};
pub use json_compat::json_to_value;
```

****:`pub use value::Value`  re-export  `mora::Value` , 84  `examples/*.mora` 

---

## 4. 

### 4.1 `ai_error.rs` ( thiserror )

```rust
// src/ai_error.rs
use thiserror::Error;

/// v0.x , builtin/ai/*  mora::Result<Value, String> 
/// 
#[derive(Debug, Error)]
pub enum AiError {
    #[error("HTTP {0} from {1}")]
    HttpStatus(u16, String),  // status_code, url

    #[error("network error connecting to {url}: {source}")]
    Network {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("failed to read response body: {0}")]
    BodyRead(String),

    #[error("failed to parse AI response: {0}")]
    Parse(String),

    #[error("retry exhausted after {attempts} attempts; last error: {last}")]
    RetryExhausted {
        attempts: u32,
        #[source]
        last: Box<AiError>,
    },
}

impl AiError {
    ///  —  stringly-typed is_retryable_error
    pub fn is_retryable(&self) -> bool {
        match self {
            AiError::Network { .. } => true,
            AiError::HttpStatus(429, _) => true,        // rate limit
            AiError::HttpStatus(500..=599, _) => true,  // server errors
            AiError::BodyRead(_) => false,
            AiError::Parse(_) => false,
            AiError::RetryExhausted { last, .. } => last.is_retryable(),
        }
    }
}

/// AiError → String , builtin/ai/*  Result<_, String>
impl From<AiError> for String {
    fn from(e: AiError) -> String {
        e.to_string()
    }
}
```

** `is_retryable_error(&str)` **: `flow.rs`  thin wrapper, builtin/ai/*  `AiError`

### 4.2 `builtin/ai/client.rs` 

```rust
// src/builtin/ai/client.rs
use crate::ai_error::AiError;
use std::time::Duration;

const HTTP_READ_TIMEOUT_SECS: u64 = 30;
const HTTP_WRITE_TIMEOUT_SECS: u64 = 30;
const AI_READ_TIMEOUT_SECS: u64 = 120;

///  HTTP client +  —  3 
pub struct AiClient {
    agent: ureq::Agent,
    retry_max: u32,
    retry_base_ms: u64,
}

impl AiClient {
    pub fn new() -> Result<Self, AiError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(AI_READ_TIMEOUT_SECS)))
            .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();

        Ok(Self {
            agent,
            retry_max: crate::ai_retry_max(),
            retry_base_ms: crate::ai_retry_base_ms(),
        })
    }

    /// GET , 4xx/5xx  AiError::HttpStatus
    pub fn get(&self, url: &str) -> Result<String, AiError> { ... }

    /// POST JSON ,
    pub fn post_json(
        &self,
        url: &str,
        auth_header: Option<(&str, &str)>,
        body: &str,
    ) -> Result<String, AiError> { ... }

    /// ( real_ai_chat_with_tools )
    fn run_with_retry<F, T>(&self, op: F) -> Result<T, AiError>
    where
        F: Fn(&AiClient) -> Result<T, AiError>,
    { ... }
}
```

****( `web.fetch`):
```rust
// : 30+ , AgentBuilder +  + 
let agent = ureq::AgentBuilder::new()...;
match agent.get(url).call() {
    Ok(response) => response.into_string()...,
    Err(ureq::Error::Status(s, r)) => ...,
    Err(ureq::Error::Transport(t)) => ...,
}

// : 5 
let client = AiClient::new()?;
let text = client.get(url)?;
```

### 4.3 

****:`src/interpreter.rs`  230 (`#[cfg(test)] mod tests`  `test_trait_basic_dispatch``test_trait_inherit_construction_checks_parents`  trait dispatch )**** `tests/` 

****:
-  `interpreter.rs` (`run()` helper ), `tests/`  crate, `pub` API
-  `interpreter.rs`  `#[cfg(test)] mod tests` (`cfg(test)`  release )
-  AskUserQuestion " tests/ ",( API),****:, step 8 ( `src/eval/call.rs`  `#[cfg(test)] mod` )

****:
- trait dispatch : `src/eval/call.rs`  `#[cfg(test)] mod tests`( call_function / call_task / call_closure )
-  84 :( `#[cfg(test)] mod tests`)
-  `tests/` 

---

## 5. ()

****, 8 :

| # |  |  | LOC  |
|---|---|---|---|
| 1 |  `value.rs`, interpreter.rs  Value/Environment/FlowSignal/StreamReader |  | 0 |
| 2 |  `flow.rs`, interpreter.rs  is_truthy/eval_binary/numeric_op/values_equal/literal_to_value_static/check_type/type_name/value_to_json/expect_string/is_builtin_object/is_pipe_method |  | 0 |
| 3 |  `json_compat.rs`, interpreter.rs  json_to_value + 6  parse_json_* |  | 0 |
| 4 |  `ai_error.rs`, thiserror , AiError |  | +90 |
| 5 |  `builtin/ai/client.rs`, AiClient() |  | +180 |
| 6 |  3  AI (web.fetch/ai.chat/real_ai_chat_with_tools) AiClient, |  | -120 |
| 7 |  `eval/*`  `builtin/ai/chat.rs` / `agent.rs` / `critic.rs` / `embedding.rs` |  | -800 |
| 8 |  `tests/`  `eval/call.rs`  #[cfg(test)] |  | 0 |

** commit**:
- `cargo build --all-targets` 0 
- `cargo test` 84/84 
- `cargo clippy` 

---

## 6. 

### 6.1 

- `cargo build --all-targets` 0  (debug + release)
- `cargo test` 84/84 ( lexer/parser/typeck/interpreter/LSP/retry/embedding/char tests)
- `cargo +nightly udeps --all-targets` `All deps seem to have been used`
- `cargo audit` 0 vulnerabilities
- `cargo clippy --all-targets` ( preexisting 35  collapsible_if)
- ** API **:`cargo public-api`  API ****(`diff` ):`cargo install cargo-public-api`(), `docs/superpowers/specs/api-baseline.txt` (1064 ,2026-06-23 ):`cargo public-api --simplified > /tmp/api_after.txt && diff docs/superpowers/specs/api-baseline.txt /tmp/api_after.txt` ( `--simplified`  `impl Freeze/Send/Sync/Unpin/UnsafeUnpin/RefUnwindSafe/UnwindSafe` ; `pub fn`/`pub struct`/`pub enum`/`pub trait`  API)

### 6.2 

- `cargo build --release`  < 5%
- `cargo build --release`  < 5%
-  `examples/lsp_smoke.rs` 
- `mora --help` 
-  API (`mora::Value``mora::Interpreter``mora::json_to_value`)

### 6.3 

- `git grep "interpreter::"` ,
- `cargo doc --no-deps` 
- ****:(`value.rs``flow.rs``json_compat.rs``ai_error.rs``eval/*.rs``builtin/ai/*.rs`) `rustc --edition 2024 --crate-type lib --emit=metadata src/<file>.rs` ( dummy extern ;:`cargo build -p mora --all-targets` )
- `interpreter.rs`  LOC < 1500

---

## 7. 

|  |  |  |  |
|---|---|---|---|
| `impl Interpreter` (call_function → call_task → call_closure), |  |  | (), dispatch |
|  `Value` / `Environment`  |  |  |  grep  API , `pub`  |
| thiserror , stringly-typed  |  |  |  `From<AiError> for String` , `?`  String |
| ,, |  |  | : `pub(crate)` ,`use crate::eval::call::call_function` |
| 8 , |  |  |  PR + CI; commit  |

---

## 8. 

|  |  |
|---|---|
| 1–3 () | 30  |
| 4 (thiserror + AiError) | 20  |
| 5 (AiClient ) | 60  |
| 6 (3 ) | 60  |
| 7 ( impl Interpreter ) | 120  |
| 8 () | 20  |
| +commit | 10 × 8 = 80  |
| **** | **~6.5 ** |

---

## 9. (Definition of Done)

- [ ] 8 , commit
- [ ] `interpreter.rs` LOC < 1500
- [ ] `impl Interpreter`  < 30 ( 104)
- [ ] AI  = 0
- [ ] `mora::Value` / `mora::Interpreter` / `mora::json_to_value`  API 
- [ ] `cargo test` 84/84 
- [ ] `cargo build --all-targets` 0 
- [ ] `cargo clippy` 
- [ ] `cargo audit` 0 
- [ ] `cargo +nightly udeps`  use
- [ ] `docs/superpowers/specs/2026-06-23-subproject-a-...-design.md`  commit

---

## 10. 

 spec , review:
1.  `writing-plans` skill, 8 ( 1  commit)
2. ,
3. , sub-project B (unsafe/panic )
