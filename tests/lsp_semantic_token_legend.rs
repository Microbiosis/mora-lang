//! v0.104.6 D201：`semanticTokens` 发出的**每一个** token 类型索引都**越界**
//! legend —— 语义高亮对所有 token 失效（已修）。
//!
//! ## 缺陷
//!
//! `lsp/providers/semantic.rs` 此前**硬编码**了四个索引：
//!
//! ```rust
//! const TOKEN_KIND_VARIABLE: f64 = 13.0;
//! const TOKEN_KIND_STRING:   f64 = 12.0;
//! const TOKEN_KIND_NUMBER:   f64 = 10.0;
//! const TOKEN_KIND_FUNCTION: f64 =  9.0;
//! ```
//!
//! 而 `server.rs` 在 capabilities 里声明的 legend 只有 **8** 项：
//!
//! ```json
//! "tokenTypes": ["keyword","function","variable","string","float","comment","type","operator"]
//! ```
//!
//! 按 LSP 协议，客户端是 `legend.tokenTypes[type]` 取名的。
//! 那些索引**既越界**（8 项的 legend 没有 9/10/12/13），
//! **又和 legend 的含义对不上**（9/10/12/13 恰是标准 LSP `SemanticTokenTypes`
//! 的 property/enumMember/function/method —— 连「用标准枚举」这条退路都不成立）。
//!
//! 真实 `mora-lsp.exe` 实测（`let a: Int = 5 / let b = 7.5 / print(a + 1)`）：
//!
//! ```text
//! data = [0,13,1,10, 1,8,1,10, 1,0,1,9, 0,0,1,9, 0,6,1,13, 0,4,1,10]
//!                                 ↑    ↑     ↑    ↑     ↑    ↑   —— 6 个 token 全部越界
//! ```
//!
//! ## 修法：legend 与索引**同源**
//!
//! `semantic.rs` 导出 `TOKEN_TYPES` 数组；`server.rs` 的 capabilities
//! 从它构造 legend，索引常量按它在数组里的位置写死并注明「不得手写别的数字」。
//! 与 D175（`module_method_names`）、D186（MCP 名字目录）同一条纪律：
//! **共用同一张表，而不是维护两份清单。**
//!
//! ## 判据
//!
//! 把真实应答的 `data` 按 4 元组解码，逐个查它声明的 legend ——
//! **每一个都必须能取到名字**。光断言「token 数量 > 0」**测不出**越界。

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d201_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    /// 起一次 LSP 会话：initialize → didOpen → semanticTokens/full。
    /// 返回 (legend 里的类型名, data 里的整数数组)。
    fn semantic(&self, tag: &str, text: &str) -> (Vec<String>, Vec<i64>) {
        fn frame(json: &str) -> String {
            format!("Content-Length: {}\r\n\r\n{}", json.len(), json)
        }
        fn esc(s: &str) -> String {
            s.replace('\\', "\\\\").replace('"', "\\\"")
        }
        let uri = format!("file:///tmp/{tag}.mora");
        let input = self.0.join(format!("{tag}.bin"));
        let body = format!(
            "{}{}{}{}",
            frame(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#
            ),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&format!(
                r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":"{}"}}}}}}"#,
                esc(text)
            )),
            frame(&format!(
                r#"{{"jsonrpc":"2.0","id":50,"method":"textDocument/semanticTokens/full","params":{{"textDocument":{{"uri":"{uri}"}}}}}}"#
            ))
        );
        std::fs::write(&input, body).expect("写输入");
        let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
            .current_dir(&self.0)
            .stdin(Stdio::from(std::fs::File::open(&input).expect("打开输入")))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .expect("跑 mora-lsp");
        let raw = String::from_utf8_lossy(&out.stdout).into_owned();

        // legend
        let leg_at = raw
            .find("\"tokenTypes\":[")
            .unwrap_or_else(|| panic!("没找到 tokenTypes legend:\n{}", raw))
            + "\"tokenTypes\":[".len();
        let leg_end = raw[leg_at..].find(']').expect("legend 缺 ]") + leg_at;
        let legend: Vec<String> = raw[leg_at..leg_end]
            .split('"')
            .filter(|s| !s.is_empty() && *s != ",")
            .map(str::to_string)
            .collect();

        // data —— 注意：**解析不出来的文档**服务器会回 `result: []`（数组，
        // 没有 `data` 字段），那同样意味着「没有 token」，按空处理。
        let data: Vec<i64> = match raw.find("\"data\":[") {
            None => Vec::new(),
            Some(at0) => {
                let at = at0 + "\"data\":[".len();
                let end = raw[at..].find(']').expect("data 缺 ]") + at;
                raw[at..end]
                    .split(',')
                    .filter_map(|s| s.trim().parse::<i64>().ok())
                    .collect()
            }
        };
        (legend, data)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SRC: &str = "let a: Int = 5\nlet b = 7.5\nprint(a + 1)\n";

/// **主判据（有牙齿）**：每个 token 的 type 索引都必须在 legend 范围内。
///
/// 修前 6 个 token 的 type 是 10/10/9/9/13/10，而 legend 只有 8 项 ——
/// **全部越界**，客户端按 `legend.tokenTypes[type]` 取名会取不到。
#[test]
fn d201_every_semantic_token_type_is_within_the_declared_legend() {
    let dir = WorkDir::new("legend");
    let (legend, data) = dir.semantic("t1", SRC);

    assert!(
        legend.len() >= 2,
        "legend 至少要有两项（function/variable/…），实得 {:?}",
        legend
    );
    assert!(
        data.len() >= 5 && data.len() % 5 == 0,
        "data 应是 **5 元组**序列（deltaLine / deltaStart / length / \
         tokenType / tokenModifiers），实得 {} 个数: {:?}\n\
         ⚠ v0.104.6 D212：本条判据原先断言的是 **4 元组**（`% 4 == 0`）——\
         它是为了按「服务端实际发出的形状」切分而写的，却把**缺陷当成了期望**：\
         规范要求 5 元组，服务端漏发 `tokenModifiers`，于是客户端从**第二个** \
         token 起全部错位。此处按规范更正。",
        data.len(),
        data
    );

    let mut out_of_range = Vec::new();
    for t in data.chunks(5) {
        let ty = t[3] as usize;
        if ty >= legend.len() {
            out_of_range.push(ty);
        }
    }
    assert!(
        out_of_range.is_empty(),
        "这些 token 的 type 索引越界（legend 共 {} 项: {:?}）: {:?}\n\
         客户端按 legend.tokenTypes[type] 取名，取不到即语义高亮失效。\
         **光断言「token 数量 > 0」测不出这个问题** —— 必须逐个查表。",
        legend.len(),
        legend,
        out_of_range
    );
}

// ===================================================================
// D212：token 的**形状与落点**
// ===================================================================

/// **主判据（有牙齿）**：`data` 必须是 **5 元组**序列，且每个 token 解出来的
/// 范围在源码里**正好等于**该 token 的原文。
///
/// 规范原文：semantic token 是
/// `[deltaLine, deltaStart, length, tokenType, tokenModifiers]` —— **5** 个数。
///
/// 修前 `push_token` 只推 **4** 个数（漏了 `tokenModifiers`），
/// 客户端按 5 个一组切分 ⇒ **从第二个 token 起全部错位**；
/// 且 `length` 硬编码 `1.0`、`deltaLine` 可为负（遍历非文档顺序）、
/// 根节点被发两遍。
///
/// 这条判据写成「**按客户端的方式解码，再核对原文**」——
/// 断言数字只能锁住一部分，解出来对不上才是真的坏了。
#[test]
fn d212_tokens_decode_to_their_own_source_text() {
    for (tag, src) in [
        ("d212_a", "let a = 1\nprint(a)\n"),
        ("d212_b", "let a = 1\nlet b = 2\nprint(a + b)\n"),
        ("d212_c", "let total = 7\nprint(\"😀\", total)\n"),
        // 嵌套块 + 多种字面量：多行、多层父子
        (
            "d212_d",
            "for i in [1, 2]\n  let x = i * 3\n  print(x)\nend\nprint(0)\n",
        ),
        // 方法调用 + 字符字面量
        ("d212_e", "let s = \"a b c\"\nprint(len(s))\n"),
    ] {
        let dir = WorkDir::new(tag);
        let (_legend, data) = dir.semantic(tag, src);
        let lines: Vec<&str> = src.trim_end().split('\n').collect();

        assert!(
            data.len() % 5 == 0,
            "[{tag}] `data` 必须是 5 的倍数（5 元组），实得 {} 个: {:?}\n\
             修前只推 4 个（漏 tokenModifiers），客户端从第二个 token 起全部错位。",
            data.len(),
            data
        );

        let mut cur_line = 0usize;
        let mut cur_col = 0usize;
        for (i, t) in data.chunks(5).enumerate() {
            let (dl, ds, len) = (t[0], t[1], t[2]);
            assert!(
                dl >= 0,
                "[{tag}] token #{i} 的 deltaLine 为**负**（{dl}）—— \
                 delta 编码要求 token 按**文档顺序**排列"
            );
            assert!(
                len > 0,
                "[{tag}] token #{i} 的 length 为 0 —— 修前 length 硬编码、\
                 这里说明定位失败被静默发出来了"
            );
            if dl == 0 {
                cur_col = (cur_col as i64 + ds) as usize;
            } else {
                cur_line = (cur_line as i64 + dl) as usize;
                cur_col = ds as usize;
            }
            assert!(
                cur_line < lines.len(),
                "[{tag}] token #{i} 落在第 {} 行，而文档只有 {} 行",
                cur_line,
                lines.len()
            );
            let u: Vec<u16> = lines[cur_line].encode_utf16().collect();
            assert!(
                cur_col + len as usize <= u.len(),
                "[{tag}] token #{i} 的范围 {cur_col}..{} **越出该行**（行长 {}）",
                cur_col + len as usize,
                u.len()
            );
            let got = String::from_utf16(&u[cur_col..(cur_col + len as usize).min(u.len())])
                .unwrap_or_else(|_| "<非法 UTF-16>".into());
            // ⚠ 判据**不能**要求「看起来像标识符」—— 字符串字面量的 token 就是
            // 任意文本（实测 `😀` 是正确落点）。只查「非空 + 不是纯标点/空白」：
            // 落点错到 `(` `,` `=` 上时这条会红。
            assert!(
                !got.trim().is_empty()
                    && got
                        .chars()
                        .any(|c| c.is_alphanumeric() || c == '_' || !c.is_ascii()),
                "[{tag}] token #{i} 解出来是 `{got}`（行 {cur_line} 列 {cur_col} 长 {len}）—— \
                 落点落在了标点/空白上。\n源码第 {cur_line} 行: {:?}",
                lines[cur_line]
            );
        }
    }
}

/// **不回归**：同一 token 不得在**同一位置**出现两次。
///
/// 修前 `emit_tokens` 先发根节点、再 `walk_witness`（**含根**）⇒ 根发两遍。
#[test]
fn d212_no_token_is_emitted_twice_at_the_same_position() {
    let dir = WorkDir::new("dup");
    let (_legend, data) = dir.semantic("dup", "let a = 1\nprint(a)\n");
    let mut seen: Vec<(usize, usize)> = Vec::new();
    let mut cur_line = 0usize;
    let mut cur_col = 0usize;
    for t in data.chunks(5) {
        let (dl, ds) = (t[0], t[1]);
        if dl == 0 {
            cur_col = (cur_col as i64 + ds) as usize;
        } else {
            cur_line = (cur_line as i64 + dl) as usize;
            cur_col = ds as usize;
        }
        let pos = (cur_line, cur_col);
        assert!(
            !seen.contains(&pos),
            "位置 {pos:?} 上出现了**重复** token —— 修前根节点被发两遍。\ndata: {data:?}"
        );
        seen.push(pos);
    }
}

/// **不回归**：长度必须等于 token 自身的**字符数**（修前硬编码 1）。
#[test]
fn d212_token_length_is_not_hardcoded_to_one() {
    let dir = WorkDir::new("len");
    let (_legend, data) = dir.semantic("len", "let a = 1\nprint(a)\n");
    let lengths: Vec<i64> = data.chunks(5).map(|t| t[2]).collect();
    assert!(
        lengths.iter().any(|&l| l > 1),
        "所有 token 的 length 都是 1 —— `print` 有 5 个字符。修前硬编码 1.0。\n\
         lengths: {lengths:?}"
    );
}

/// 修后应当**真能取到名字**，且类型分布合理（数值是 float、调用是 function、变量是 variable）。

#[test]
fn d201_token_types_resolve_to_meaningful_names() {
    let dir = WorkDir::new("names");
    let (legend, data) = dir.semantic("t2", SRC);

    let mut names: Vec<String> = data
        .chunks(5)
        .map(|t| legend[t[3] as usize].clone())
        .collect();
    names.sort();
    names.dedup();

    for want in ["function", "variable", "float"] {
        assert!(
            names.iter().any(|n| n == want),
            "应至少发出一种 `{}` token; 实得 {:?}（legend {:?}）",
            want,
            names,
            legend
        );
    }
}

/// **对照组**：不产生 token 的源码不应凭空造 token。
#[test]
fn d201_no_tokens_stays_empty() {
    let dir = WorkDir::new("empty");
    let (_legend, data) = dir.semantic("t3", "\n\n");
    assert!(
        data.is_empty(),
        "空文档不应产生 semantic token; 实得 {:?}",
        data
    );
}
