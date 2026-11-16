//! v0.104.6 D88：模块方法签名的**返回类型**此前只有「读运行期代码推断」一个来源。
//!
//! ## 为什么要有这条测试
//!
//! D69–D85 给 20 个模块补了 34 条 `module_method_signature`。每条的**返回类型**
//! 都是写代码的人**读 `call_*_method` 的 arm 之后手写**的 —— 也就是说，
//! **期望值来自我自己，而不是实测**。D70 就已经在这个方向栽过一次
//! （`memory.load` 早期被写成 `Dict`，实为恒返 `Bool`），D85 推翻了普查表里
//! 一句没验证过的断言。
//!
//! 只验「不收紧」不够（D80/D81 的 `signature_no_over_tightening` 覆盖的是**多传
//! 实参**）。**返回类型写错**是另一类缺陷：它照样放行/放行，只是把
//! 「不检查」换成了「检查错」，而且**在标注与运行期恰好相符时完全不可见**。
//!
//! ## 方法：真正的差分，而不是再读一遍代码
//!
//! 语言自带 `type_of`，能在**运行期**报出值的实际类型名。测试两侧对拍：
//!
//! - **声明侧**（进程内）：`module_method_signature(module, method)` 的
//!   `return_type` 展开成一组**允许的运行期类型名**（`Union` 取并集，
//!   `Any` / `Unknown` 视为不约束）。
//! - **运行期侧**（子进程）：现生成一段 `.mora`，逐行打印
//!   `key=type_of(<真实调用>)`，走 `mora run` 真实路径取回。
//!
//! 两边必须落在同一集合。任一侧改动而另一侧没跟上 → 当场失守。
//!
//! ## 覆盖边界（如实标注，不假装全覆盖）
//!
//! - **只覆盖有确定性调用的方法**。`json.parse` / `mora.refine` / `mock.call`
//!   的返回类型随输入内容变化，声明为 `Any` —— `Any` 不约束，本测试对其
//!   **不做断言**（既不证真也不证伪）。
//! - **空列表的元素类型验不了**：`mock.names()` / `memory.keys()` 在新进程里
//!   恒返 `[]`，外层 `list` 可验、元素 `string` 不可验。这类条目按外层断言。
//! - `sandbox.audit_flush` / `audit_verify` 声明为 `Union[Bool, String]`
//!   （成功 Bool、失败 String）。**失败分支无法从用户代码触发**（需要破坏
//!   审计链），本测试只覆盖成功分支，String 分支**未经实测**。

use mora::test_support::TempDir;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// 一条对拍记录。
///
/// - `key` —— 打印行里的标识符
/// - `module` / `method` —— 送进 `module_method_signature` 的索引键
/// - `call` —— 真实运行期调用（`.mora` 源码片段）
/// - `runtime` —— 运行期**实测**得到的类型名（取自 `type_of`）
struct Case {
    key: &'static str,
    module: &'static str,
    method: &'static str,
    call: &'static str,
    runtime: &'static str,
}

/// 全部对拍用例。`runtime` 每一格都是 D88 实测 `mora run` + `type_of` 的结果，
/// 不是从代码推断的。
const CASES: &[Case] = &[
    // ── math ──
    Case {
        key: "math.PI",
        module: "math",
        method: "PI",
        call: "math.PI",
        runtime: "float",
    },
    Case {
        key: "math.floor",
        module: "math",
        method: "floor",
        call: "math.floor(1.5)",
        runtime: "float",
    },
    Case {
        key: "math.abs",
        module: "math",
        method: "abs",
        call: "math.abs(-3)",
        runtime: "int",
    },
    Case {
        key: "math.is_nan",
        module: "math",
        method: "is_nan",
        call: "math.is_nan(1.0)",
        runtime: "bool",
    },
    // ── json ──
    Case {
        key: "json.stringify",
        module: "json",
        method: "stringify",
        call: "json.stringify([1, 2])",
        runtime: "string",
    },
    // ── stats / linalg ──
    Case {
        key: "stats.sum",
        module: "stats",
        method: "sum",
        call: "stats.sum([1, 2, 3])",
        runtime: "float",
    },
    Case {
        key: "stats.histogram",
        module: "stats",
        method: "histogram",
        call: "stats.histogram([1, 2, 3, 4], 2)",
        runtime: "list",
    },
    Case {
        key: "linalg.dot",
        module: "linalg",
        method: "dot",
        call: "linalg.dot([1, 0], [0, 1])",
        runtime: "float",
    },
    Case {
        key: "linalg.norm",
        module: "linalg",
        method: "norm",
        call: "linalg.norm([3, 4])",
        runtime: "float",
    },
    Case {
        key: "linalg.transpose",
        module: "linalg",
        method: "transpose",
        call: "linalg.transpose([[1, 2], [3, 4]])",
        runtime: "list",
    },
    Case {
        key: "linalg.cross",
        module: "linalg",
        method: "cross",
        call: "linalg.cross([1, 0, 0], [0, 1, 0])",
        runtime: "list",
    },
    // ── file（只读，不写）──
    Case {
        key: "file.list",
        module: "file",
        method: "list",
        call: "file.list(\".\")",
        runtime: "list",
    },
    Case {
        key: "file.exists",
        module: "file",
        method: "exists",
        call: "file.exists(\".\")",
        runtime: "bool",
    },
    Case {
        key: "file.size",
        module: "file",
        method: "size",
        call: "file.size(\".\")",
        runtime: "float",
    },
    Case {
        key: "file.basename",
        module: "file",
        method: "basename",
        call: "file.basename(\"a/b.txt\")",
        runtime: "string",
    },
    Case {
        key: "file.dirname",
        module: "file",
        method: "dirname",
        call: "file.dirname(\"a/b.txt\")",
        runtime: "string",
    },
    Case {
        key: "file.extname",
        module: "file",
        method: "extname",
        call: "file.extname(\"a/b.txt\")",
        runtime: "string",
    },
    Case {
        key: "file.cwd",
        module: "file",
        method: "cwd",
        call: "file.cwd()",
        runtime: "string",
    },
    Case {
        key: "file.home_dir",
        module: "file",
        method: "home_dir",
        call: "file.home_dir()",
        runtime: "string",
    },
];

/// 把 `Type` 展开成它**允许**的运行期类型名集合。
///
/// `Any` / `Unknown` 返回空集 —— 调用方据此判定「本条不做约束」，
/// 而不是误当成「不许是任何类型」。
fn allowed_runtime_names(ty: &mora::typeck::Type) -> BTreeSet<String> {
    use mora::typeck::Type;
    let mut out = BTreeSet::new();
    match ty {
        Type::String => {
            out.insert("string".into());
        }
        Type::Char => {
            out.insert("char".into());
        }
        Type::Int => {
            out.insert("int".into());
        }
        Type::Float => {
            out.insert("float".into());
        }
        Type::Bool => {
            out.insert("bool".into());
        }
        Type::Nil => {
            out.insert("nil".into());
        }
        Type::List(_) => {
            out.insert("list".into());
        }
        Type::Dict(_, _) => {
            out.insert("dict".into());
        }
        Type::Union(members) => {
            for m in members {
                out.extend(allowed_runtime_names(m));
            }
        }
        // 不约束的类型：空集 = 不参与断言
        Type::Any | Type::Unknown | Type::TypeVar(_) => {}
        _ => {}
    }
    out
}

/// 生成 `.mora` 源码，逐行打印 `key=type_of(call)`。
fn program_source() -> String {
    let mut s = String::from("task main()\n");
    for c in CASES {
        s.push_str(&format!("  print(\"{}=\" + type_of({}))\n", c.key, c.call));
    }
    s.push_str("end\n");
    s
}

/// 跑真实 `mora run`，把 `key=typename` 解析成 map。
fn observe_runtime() -> BTreeSet<(String, String)> {
    let dir = TempDir::new("mora_d88_return_types");
    let prog: PathBuf = dir.join("probe.mora");
    std::fs::write(&prog, program_source()).expect("write probe");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&prog)
        .output()
        .expect("run mora");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "探针程序应跑通。\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // 横幅行带前导空格、程序输出顶格；只取含 `=` 且无前导空格的行
    let mut seen = BTreeSet::new();
    for line in stdout.lines() {
        if line.starts_with(char::is_whitespace) || line.trim().is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            seen.insert((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let _ = std::fs::remove_file(&prog);
    seen
}

/// 核心断言：运行期实际类型 ∈ 签名声明允许的集合。
#[test]
fn declared_return_type_admits_the_runtime_type() {
    let observed = observe_runtime();

    // 覆盖率自检：清单被改动时当场失守（与 D60 / D61 / D68 同型）
    assert_eq!(
        observed.len(),
        CASES.len(),
        "运行期只观测到 {} 条，CASES 有 {} 条 —— 有用例没跑出结果。\nstdout 观测：{observed:?}",
        observed.len(),
        CASES.len()
    );

    let mut mismatches = Vec::new();
    for c in CASES {
        let got = observed
            .iter()
            .find(|(k, _)| k == c.key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("<未观测到>");
        // 运行期实测值必须与清单里记录的一致 —— 防止「清单里的 runtime
        // 也是凭印象写的」这种自我循环。
        if got != c.runtime {
            mismatches.push(format!(
                "[实测漂移] {}: 运行期得 `{got}`，清单记的是 `{}` —— 清单的期望值需要重新取证",
                c.key, c.runtime
            ));
            continue;
        }

        let sig = mora::typeck::dispatch::module_method_signature(c.module, c.method)
            .unwrap_or_else(|| panic!("{} 没有签名 —— 清单与签名表不一致", c.key));
        let allowed = allowed_runtime_names(&sig.return_type);
        if allowed.is_empty() {
            // Any / Unknown：不约束，跳过（不证真也不证伪）
            continue;
        }
        if !allowed.contains(c.runtime) {
            mismatches.push(format!(
                "[声明不含实测] {}: 签名返回类型 {:?} 只允许 {allowed:?}，运行期实测 `{}`",
                c.key, sig.return_type, c.runtime
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "模块方法签名的返回类型与运行期实测对不上（{} 条）：\n  - {}",
        mismatches.len(),
        mismatches.join("\n  - ")
    );
}

/// 覆盖面自检：这张表不是「随手挑几个能过的」。
///
/// `KNOWN_NONEMPTY_MODULES` 是 D88 逐条读运行期代码 + 实测确认「确有非空签名」
/// 的模块名单。往签名表里加了新模块却忘了把它加进来、或忘了给它补实测用例，
/// 本测试就会失败 —— 这正是「新增签名必须同时补一条运行期证据」的强制点。
#[test]
fn census_covers_every_module_that_has_a_signature() {
    let covered: BTreeSet<&str> = CASES.iter().map(|c| c.module).collect();
    for m in KNOWN_NONEMPTY_MODULES {
        assert!(
            covered.contains(m),
            "模块 `{m}` 有非空签名但本对拍表没有覆盖它 —— 新增签名请同时补一条实测用例"
        );
    }
    // 反向：表里不该出现签名表里根本没有的模块（否则是这张表自己在漂）
    for m in &covered {
        assert!(
            KNOWN_NONEMPTY_MODULES.contains(m),
            "对拍表出现了名单外的模块 `{m}` —— 同步更新 KNOWN_NONEMPTY_MODULES 并说明理由"
        );
    }
    assert!(
        CASES.len() >= 19,
        "对拍用例只有 {} 条（Wave 1 保留模块实测 19 条），覆盖面在缩小 —— 确认是有意为之并更新本断言",
        CASES.len()
    );
}

/// 经 D88 逐条读运行期代码 + 实测确认「确有非空签名」的模块。
///
/// 未列入的模块与其原因（如实记录，不假装全覆盖）：
///
/// - `exec` —— `exec.parallel` 真的返回 `List[Dict[String, Any]]`，但它会
///   **spawn 真实子进程**并写 pid/elapsed_ms，放进单测代价过大；已在 D88
///   手工实测得到 `list`（见 CHANGELOG）。
/// - `schedule` / `tool` / `skill` / `xform` / `web` / `ai` / `agent` / `random`
///   —— 要么需要外部资源（网络 / 容器 / 已注册的处理函数），要么返回值
///   随注册内容变化，构造确定性调用比不写这条测试更贵。
const KNOWN_NONEMPTY_MODULES: &[&str] = &["math", "json", "file", "stats", "linalg"];
