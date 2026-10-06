//! v0.104.6 D353 / D354 —— **关键字可作声明位的名字** ⇒ 僵尸绑定（已修）
//!
//! ## 缺陷：`token_to_identifier_name` 的兜底**泄漏到声明位**
//!
//! `token_to_identifier_name`（`src/parser_v3/mod.rs`）把 26 个关键字 token
//! 映射回标识符名，它的**本意**（注释原文）是：
//!
//! > the lexer tokenizes `tool`, `task`, etc. as dedicated token types,
//! > but they can appear in identifier positions (**method names**,
//! > variable references **after `.` or `::`**).
//!
//! 也就是**引用位**。但 `consume_identifier` 的兜底分支被**三个声明位**
//! 共用，于是：
//!
//! ```text
//! let if = 5          → exit 0   声明成功
//! print(if)           → 解析失败  永远引用不到
//! ```
//!
//! 用户拿到的是**三重坑**：① 声明静默成功 ② 引用永远失败
//! ③ 诊断**指错行**（指向使用处，不指向声明处）。
//!
//! | 轮次 | 声明位 | 修前 | 修后 |
//! |---|---|---|---|
//! | D353 | `let` 变量名 | exit 0 僵尸 | exit 2，报 line 1 |
//! | D354 | `for` 循环变量 | exit 0 僵尸 | exit 2，报**声明行** |
//! | D354 | 闭包参数 `fn(…)` | exit 0 僵尸 | exit 2 |
//! | D354 | `macro` 参数 | exit 0 僵尸 | exit 2 |
//!
//! ## 修法：按**调用点**拆严格度（不动共享 helper）
//!
//! 新增 `consume_plain_identifier`（`tokens.rs`）—— **只认真正的
//! `TokenType::Identifier`**，声明位改用它。`consume_identifier` 的宽容
//! 兜底**原样保留**给引用位（方法名 / `::` / dict 键 / 字段名…）。
//!
//! ## 零依赖
//!
//! `tests/fixtures/**` + `examples/**` 下 **56 个** `.mora` 全量扫描，
//! 关键字作 `let` 变量名 / `for` 循环变量 / 闭包参数 / `macro` 参数
//! 的命中数 **0 / 0 / 0** ⇒ 修复不破坏任何既有代码。
//!
//! ## `document` 是**例外**，且是**有意**的
//!
//! lexer（`src/lexer.rs:862`）对 `document` 做**上下文退化**：
//! 下一个 token 是字符串字面量 ⇒ `TokenType::Document`（块语句），
//! 否则退化成 `TokenType::Identifier`（模块名 `document.parse(…)`）。
//! 所以 `let document = 5` **合法**且 exit 0 —— 这是注释明写的设计，
//! 本判据把它钉住，防止将来"顺手收紧"时误伤。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 25 个**真正**作关键字 token 的词面（`document` 除外，见文件头说明）。
const KEYWORDS: [&str; 25] = [
    "if",
    "let",
    "fn",
    "end",
    "then",
    "match",
    "return",
    "for",
    "break",
    "continue",
    "in",
    "import",
    "type",
    "enum",
    "struct",
    "macro",
    "loop",
    "orchestrate",
    "prompt",
    "dyn",
    "as",
    "do",
    "max_rounds",
    "quote",
    "task",
];

fn slug(s: &str) -> String {
    let mut out = String::from("d353_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 独立进程 + 隔离 `HOME`；**stdout 与 stderr 都采**（D351 教训：
/// 解析错误走 stderr，只读 stdout 会看到"空输出 + exit 非 0"，
/// 那时候要先怀疑采集器，再怀疑产品）。
fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d353_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **D353 主断言**：25 个关键字在 `let` 声明位**全部**被拒。
///
/// 计数下界断言放在最后（D338/D340 教训）：防止上面的循环因
/// 常量被清空而"静默通过 0 个"仍然全绿。
#[test]
fn d353_let_declaration_rejects_every_keyword() {
    let mut checked = 0usize;
    for kw in KEYWORDS {
        let (code, got) = ev(&format!("let {kw} = 5\n"));
        assert_eq!(
            code, 2,
            "关键字 `{kw}` 作 `let` 变量名应被拒（修前 exit 0 僵尸绑定）; 实得 exit={code} out={got}"
        );
        checked += 1;
    }
    assert_eq!(checked, 25, "关键字表被清空 ⇒ 本条会假绿");
}

/// **D354**：`for` 循环变量位（修前的第二个泄漏点）。
#[test]
fn d354_for_loop_variable_rejects_every_keyword() {
    let mut checked = 0usize;
    for kw in KEYWORDS {
        // 注意：**不在体内引用**。若在体内引用，修前也会 exit 2 ——
        // 那验的是"引用失败"，不是"声明被拒"，会把结论带偏。
        // 这里要验的恰恰是**修前 exit 0** 的那个形态。
        let body = format!("let xs = [1]\nfor {kw} in xs\n  print(1)\nend\nprint(\"OK\")\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 2,
            "关键字 `{kw}` 作 `for` 循环变量应被拒（修前 exit 0 僵尸绑定）; 实得 exit={code} out={got}"
        );
        checked += 1;
    }
    assert_eq!(checked, 25, "关键字表被清空 ⇒ 本条会假绿");
}

/// **D354**：闭包参数位（第三个泄漏点）。
#[test]
fn d354_closure_parameter_rejects_every_keyword() {
    let mut checked = 0usize;
    for kw in KEYWORDS {
        let body = format!("let f = fn({kw}) 1 end\nprint(\"OK\")\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 2,
            "关键字 `{kw}` 作闭包参数应被拒（修前 exit 0 僵尸绑定）; 实得 exit={code} out={got}"
        );
        checked += 1;
    }
    assert_eq!(checked, 25, "关键字表被清空 ⇒ 本条会假绿");
}

/// **D354**：`macro` 参数位（第四个泄漏点）。
#[test]
fn d354_macro_parameter_rejects_every_keyword() {
    let mut checked = 0usize;
    for kw in KEYWORDS {
        let body = format!("macro m({kw})\n  print(1)\nend\nprint(\"OK\")\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 2,
            "关键字 `{kw}` 作 macro 参数应被拒（修前 exit 0 僵尸绑定）; 实得 exit={code} out={got}"
        );
        checked += 1;
    }
    assert_eq!(checked, 25, "关键字表被清空 ⇒ 本条会假绿");
}

/// **诊断必须指对行** —— 修前 `let if = 5` 的错误报在 line 1 是对的，
/// 但 `for if in xs` 的失败点在**体内使用处**（line 3），
/// 用户查 line 3 而那里完全没问题。现在必须指向**声明行**。
#[test]
fn d354_diagnostic_points_at_the_declaration_line_not_the_use_site() {
    // for 声明在 line 2
    let (code, got) = ev("let xs = [1]\nfor if in xs\n  print(1)\nend\nprint(\"OK\")\n");
    assert_eq!(code, 2, "应被拒; 实得 exit={code} out={got}");
    assert!(
        got.contains("line 2"),
        "`for` 在 line 2，诊断必须指 line 2（修前指 line 3 的使用处，用户查不到）; 实得: {got}"
    );

    // for 声明在 line 4（前面有干扰行）
    let (code, got) =
        ev("let xs = [1]\nlet a = 2\nlet b = 3\nfor end in xs\n  print(1)\nend\nprint(\"OK\")\n");
    assert_eq!(code, 2, "应被拒; 实得 exit={code} out={got}");
    assert!(
        got.contains("line 4"),
        "`for` 在 line 4，诊断必须指 line 4; 实得: {got}"
    );
}

/// **正向兼容性**：正常标识符在四个声明位**全部**照常工作。
///
/// 这条是本判据的**另一半牙齿** —— 只验否定侧的话，
/// 「把声明位改成什么都拒绝」也能全绿。
#[test]
fn d354_normal_identifiers_still_declare_and_bind() {
    let cases: [(&str, &str); 4] = [
        ("let", "let zz = 5\nprint(zz)\n"),
        (
            "for",
            "let xs = [1, 2]\nlet s = 0\nfor x in xs\n  s = s + x\nend\nprint(s)\n",
        ),
        ("closure", "let f = fn(a, b) a + b end\nprint(f(1, 2))\n"),
        ("macro", "macro m(a)\n  print(a)\nend\nm(9)\n"),
    ];
    for (which, body) in cases {
        let (code, got) = ev(body);
        assert_eq!(
            code, 0,
            "`{which}` 声明位用正常标识符必须照常工作; 实得 exit={code} out={got}"
        );
    }
}

/// **正向兼容性 2**：正常循环必须**真的执行**（不只是 exit 0）。
///
/// `let` 那条验的是 `5.0`；`for` 验的是求和结果 `3.0`
/// —— 否则「循环体被跳过」也会 exit 0 蒙混过关。
#[test]
fn d354_for_loop_body_actually_executes() {
    let (code, got) = ev("let xs = [1, 2]\nlet s = 0\nfor x in xs\n  s = s + x\nend\nprint(s)\n");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert!(got.contains("3"), "循环应把 [1,2] 累加成 3; 实得: {got}");
}

/// **共享 helper 的宽容必须原样保留** —— 这正是 `token_to_identifier_name`
/// 的本意（方法名 / `.` / `::` 引用位）。D353/D354 **只**拆声明位，
/// 若连引用位一起收紧，本条变红。
#[test]
fn d354_reference_positions_still_accept_keywords() {
    let cases: [(&str, &str); 4] = [
        // dict 字面量的 `type` 键 —— v0.103 起显式支持（spec §18.1 schema 写法）
        ("dict key `type`", "print({type: 1}[\"type\"])\n"),
        // 对象属性名
        ("attr `tool`", "let o = {tool: 1}\nprint(o[\"tool\"])\n"),
        // 方法名
        ("method `len`", "let xs = [1, 2]\nprint(xs.len())\n"),
        // builtin 名本身不是关键字，这条验的是普通标识符未被误伤
        ("plain ident", "let http = 5\nprint(http)\n"),
    ];
    for (which, body) in cases {
        let (code, got) = ev(body);
        assert_eq!(
            code, 0,
            "引用位 `{which}` 必须仍接受（D353/D354 只拆声明位）; 实得 exit={code} out={got}"
        );
    }
}

/// **`document` 是上下文退化的关键字，必须仍然合法**。
///
/// lexer（`src/lexer.rs:862`）注释明写：下一 token 是字符串 ⇒ Document 块，
/// 否则退化成 Identifier（模块名）。`let document = 5` 走的是后者，
/// **有意允许**。本条防的是"将来顺手收紧时误伤 `document.parse(…)`"。
#[test]
fn d354_document_stays_a_valid_identifier_by_design() {
    let (code, got) = ev("let document = 5\nprint(document)\n");
    assert_eq!(
        code, 0,
        "`document` 走 lexer 的上下文退化分支，本就是合法标识符，不该被收紧; 实得 exit={code} out={got}"
    );
    assert!(got.contains("5"), "应绑定成功并打印 5; 实得: {got}");
}

/// **零依赖回归钉**：56 个真实 `.mora` 的**解析/类型失败数不得超过基线**。
///
/// 这是"修复没破坏既有代码"的**唯一硬证据** —— 前面的用例只验了
/// 四行探针，验不到真实文件。
///
/// ## 为什么是「不新增」而不是「零失败」
///
/// 首版这里写的是 `assert!(failed.is_empty())`，**首跑就红**：56 个里
/// 有 **2 个**本来就 exit 2 ——
///
/// | 文件 | 既有诊断 |
/// |---|---|
/// | `tests/fixtures/e2e/lisp.mora` | `line 26:46` 类型不匹配 |
/// | `examples/integration_v0_34.mora` | `line 12:31` 类型不匹配 |
///
/// 两者都是 **2026 年之前就存在**的类型错误，与本判据无关
/// （D353/D354 只动 `parser_v3` 的声明位，不动 typeck）。
///
/// 「零失败」这个断言形态的陷阱和 D338 的**计数下界**是同一条：
/// 基线非零时，硬写「必须为空」会让判据**永远红**，于是被后人
/// 当成 flaky 删掉 —— 比没有判据更糟。正确形态是**钉住基线数值**，
/// 任何新增失败都会让数字变大而变红。
#[test]
fn d354_all_real_fixtures_still_parse() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for sub in ["tests/fixtures", "examples"] {
        collect(&std::path::Path::new(root).join(sub), &mut files);
    }
    // 计数下界：fixture 被搬走/改名时本条会立刻红，而不是静默通过 0 个。
    assert!(
        files.len() >= 50,
        "真实 .mora 只找到 {} 个（基线 56），fixture 布局可能变了; files={files:?}",
        files.len()
    );
    let mut failed: Vec<String> = Vec::new();
    for f in &files {
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .arg(f)
        .output()
        .expect("跑 mora");
        // 只看**解析/类型**阶段的失败（exit 2）；运行期错误（exit 1）
        // 与本判据无关 —— 有些 fixture 本来就需要外部输入。
        if out.status.code() == Some(2) {
            failed.push(f.display().to_string());
        }
    }
    // 基线 2（见函数文档）—— 允许**恰好**这两个既有问题，不允许新增。
    assert_eq!(
        failed.len(),
        2,
        "解析失败的真实 .mora 数应停在基线 2；若变少，说明有人修好了既有错误，应同步更新基线；若变多，是本轮引入的回归:\n{}",
        failed.join("\n")
    );
    for expected in ["e2e/lisp.mora", "examples/integration_v0_34.mora"] {
        assert!(
            failed
                .iter()
                .any(|f| f.replace('\\', "/").ends_with(expected)),
            "基线里的 `{expected}` 不该消失（那是别人修的，不是本轮）; 现存失败: {failed:?}"
        );
    }
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("mora") {
            out.push(p);
        }
    }
}
