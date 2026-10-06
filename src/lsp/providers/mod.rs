//! LSP Provider implementations (v0.55: all V3 Mir-native)

mod completion;
mod definition;
mod folding;
mod formatting;
mod hover;
pub(crate) mod parsed_doc_v3;
mod references;
mod rename;
pub(crate) mod semantic;
mod symbols;

pub use completion::completion_v3;
pub use definition::definition_v3;
pub use folding::folding_range_v3;
pub use formatting::formatting;
pub use hover::hover_v3;
pub use references::references_v3;
pub use rename::rename_v3;
pub use semantic::semantic_tokens_v3;
pub use symbols::document_symbol_v3;

// v0.104.6 D211：把 UTF-16 ↔ char 的列转换提到 `pub` ——
// 它们是 LSP 协议里**最容易被搞错**的一层（见 D211 的实测：含 emoji 的行上
// rename 会把文件改坏），值得让集成测试能直接钉住其不变式，
// 而不必每次都起一个 `mora-lsp` 进程。
pub use parsed_doc_v3::{char_to_utf16_col, position_to_offset, utf16_to_char_col};
