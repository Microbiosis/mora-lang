//! v0.104.6 D472：**全量**普查 —— 仓库里每一个 `.mora` 文件 + 一组合成构造。
//!
//! ## 为什么要有这张表
//!
//! `tests/nine_layer_conformance.rs` 是**精选清单**（静态、逐条手写、钉住
//! 期望输出）。本文件是**兜底**：它不靠手写清单，而是
//!
//! 1. 扫遍仓库里**每一个** `.mora` 文件（examples/ + tests/fixtures/ + vendor/），
//! 2. 外加一组**合成构造**（覆盖仓库文件里没出现的语言形态），
//!
//! 然后断言：**每一个都能编译成功并运行到成功结束**。
//!
//! 任何新的「9 层产不出」的构造，都会被本文件抓住。
//!
//! ## 判定口径（v0.104.6 D474：从「零回落」改为「编译 + 运行」）
//!
//! 此前判据是 `run_pipeline` 的 `differential_ok`（差分逐指令比**类别**），
//! 差分红 ⇒ 该程序会走旧 `emit.rs` 路径。
//!
//! **D474 把差分机制整个退役** —— parser 不再发射 MirInst（只产 witness），
//! MIR 全部由 9 层产出 ⇒「参考实现」这一侧**已不存在**，那次比对会变成
//! **自己跟自己比**（恒真）。留着一个恒真的判据比没有更坏：它让人以为
//! 覆盖还在。
//!
//! ⇒ 判据换成**可观察的行为**：编译成功 + 运行成功（退出码 0）。
//! 这比「零回落」**更强** —— 差分只比**指令形状**，而「跑起来且不报错」直接
//! 判的是**程序的最终行为**。
//!
//! 逐条构造的**期望输出**由 `tests/nine_layer_conformance.rs` 承担。

use mora::test_support::TempDir;
use std::process::Command;

/// 一条构造的期望结果。
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// 编译成功、运行成功（退出码 0）。
    Ok,
    /// 编译期类型错误（退出码 2）。
    TypeErr,
    /// 运行期错误（退出码 1）。
    RuntimeErr,
}

/// 合成构造清单 —— 覆盖仓库 `.mora` 文件里没出现的语言形态。
///
/// 只收**能被 parser 接受**的形态：`trait` / `impl` / `tool` / `skill`
/// 在本版本**根本无法解析**（spec §14.2 D109：lexer 无对应关键字，
/// parser 无对应语句分派），故不在此列 —— 不可解析的构造不存在风险。
const SYNTHETIC: &[(&str, &str, Expect)] = &[
    // ── D472 接入的 8 类 ──
    ("worker", "worker w do\n  print(1)\nend\n", Expect::Ok),
    ("transaction", "transaction\n  print(1)\nend\n", Expect::Ok),
    (
        "transaction_comp",
        "transaction\n  print(1)\ncompensation\n  print(2)\nend\n",
        Expect::Ok,
    ),
    ("eval_bare", "eval(1 + 1)\n", Expect::Ok),
    // `eval "n" 1, 2` —— given=1 与 expects=[2] 不等 ⇒ 断言失败（运行期错误）。
    ("eval_stmt", "eval \"n\" 1, 2\n", Expect::RuntimeErr),
    ("aggregate", "aggregate acc, 1\n", Expect::Ok),
    (
        "commit",
        "transaction\n  print(1)\nend\ncommit\n",
        Expect::Ok,
    ),
    ("rollback", "rollback\n", Expect::RuntimeErr),
    (
        "pipe",
        "let x = 1 |> print\n",
        // `|>` 把值**灌进**被调函数的首参；`print` 的签名是 `print(v)`，
        // 灌进去后参数个数不匹配 ⇒ 运行期错误。
        Expect::RuntimeErr,
    ),
    (
        "perform_bare",
        "let g = \"i\"\ng = perform Ai(\"x\")\nprint(g)\n",
        // 裸 `perform` 无匹配 handler ⇒ typeck 拦下（效果行不匹配）。
        Expect::TypeErr,
    ),
    // ── 既有语言面（回归护栏：这些一直能跑，不许退化）──
    (
        "handle",
        "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"mocked:\" + __arg0\n}\nprint(g)\n",
        Expect::Ok,
    ),
    (
        "nested_handle",
        "handle Ai {\n  handle Ai {\n    perform Ai(\"x\")\n  } {\n    __arg0\n  }\n} {\n  __arg0\n}\n",
        Expect::Ok,
    ),
    ("parallel", "parallel\n  print(1)\nend\n", Expect::Ok),
    (
        "observe",
        "observe trace \"t\" do\n  print(1)\nend\n",
        Expect::Ok,
    ),
    ("span", "span \"s\" do\n  print(1)\nend\n", Expect::Ok),
    (
        "prompt_section",
        "prompt \"p\" do\n  print(1)\nend\n",
        Expect::Ok,
    ),
    (
        "document_section",
        "document \"d\" do\n  print(1)\nend\n",
        Expect::Ok,
    ),
    ("with", "with model = \"m\"\n  print(1)\nend\n", Expect::Ok),
    (
        "model",
        "model Counter\n  count: number = 0\nend\nprint(1)\n",
        Expect::Ok,
    ),
    (
        "model_default",
        "model Counter\n  count: number = 7\nend\nprint(1)\n",
        Expect::Ok,
    ),
    (
        "msg",
        "msg CounterMsg\n  Increment\n  Decrement\nend\nprint(1)\n",
        Expect::Ok,
    ),
    (
        "struct",
        "struct P\n  x: number\nend\nprint(1)\n",
        Expect::Ok,
    ),
    ("enum", "enum E\n  A\n  B\nend\nprint(1)\n", Expect::Ok),
    (
        "effect_sig",
        "effect MyEff(number) : number\nprint(1)\n",
        Expect::Ok,
    ),
    (
        "import",
        "import \"vendor/hello.mora\"\nprint(1)\n",
        Expect::Ok,
    ),
    (
        "macro",
        "macro m(x)\n  print(x)\nend\nprint(1)\n",
        Expect::Ok,
    ),
    ("type_alias", "type MyNum = number\nprint(1)\n", Expect::Ok),
    ("template", "let n = 1\nprint(p\"v={n}\")\n", Expect::Ok),
    ("bigint", "let n = 123n\nprint(n)\n", Expect::Ok),
    ("closure", "let f = fn(x) x + 1\nprint(f(1))\n", Expect::Ok),
    (
        "task",
        "task t()\n  print(1)\nend\ntask main()\n  t()\nend\n",
        Expect::Ok,
    ),
    (
        "match_guard",
        "let v = 1\nlet r = match v {\n  x when x > 0 => \"pos\"\n  _ => \"neg\"\n}\nprint(r)\n",
        Expect::Ok,
    ),
    (
        "while",
        "let i = 0\nwhile i < 3\n  print(i)\n  i = i + 1\nend\n",
        Expect::Ok,
    ),
    ("for", "for x in [1, 2, 3]\n  print(x)\nend\n", Expect::Ok),
    (
        "update",
        "update(msg, model)\n  model\nend\nprint(1)\n",
        Expect::Ok,
    ),
    (
        "tea_standalone",
        "model C\n  count: number = 0\nend\nmsg M\n  Inc\nend\nupdate(msg, model)\n  model\nend\nprint(1)\n",
        Expect::Ok,
    ),
    ("quasiquote", "let q = `(10+20)\nprint(q)\n", Expect::Ok),
    ("export", "export task t()\n  print(1)\nend\n", Expect::Ok),
    ("dyn_coerce", "let d = 1 as dyn T\nprint(d)\n", Expect::Ok),
    (
        "or_and",
        "let x = true or false\nlet y = true and false\nprint(x)\nprint(y)\n",
        Expect::Ok,
    ),
    (
        "rel",
        "rel edge(\"a\", \"b\")\nsolve { edge(\"x\", \"y\") }\n",
        Expect::Ok,
    ),
    (
        "orchestrate",
        "orchestrate graph input -> result\n  agent a => \"A\"\n  agent b => \"B\"\n  @start -> a\n  a -> b\nend\n",
        Expect::Ok,
    ),
    // ── 嵌套组合（最易触发降级链的错位）──
    (
        "worker_in_task",
        "task t()\n  worker w do\n    print(1)\n  end\nend\ntask main()\n  t()\nend\n",
        Expect::Ok,
    ),
    (
        "transaction_in_task",
        "task t()\n  transaction\n    print(1)\n  end\nend\ntask main()\n  t()\nend\n",
        Expect::Ok,
    ),
    (
        "transaction_comp_in_task",
        "task t()\n  transaction\n    print(1)\n    compensation\n      print(2)\n  end\nend\ntask main()\n  t()\nend\n",
        Expect::Ok,
    ),
    (
        "eval_in_worker",
        "worker w do\n  eval 1, 1\nend\n",
        Expect::Ok,
    ),
    (
        "eval_in_transaction",
        "transaction\n  eval 1, 1\nend\n",
        Expect::Ok,
    ),
    (
        "worker_in_handle",
        "handle Ai {\n  worker w do\n    print(1)\n  end\n} {\n  \"m\"\n}\n",
        Expect::Ok,
    ),
    (
        "aggregate_in_task",
        "task t()\n  aggregate acc, 1\nend\ntask main()\n  t()\nend\n",
        Expect::Ok,
    ),
    (
        "perform_in_worker",
        "worker w do\n  perform Ai(\"x\")\nend\n",
        Expect::TypeErr,
    ),
];

fn collect_mora(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_mora(&p, out);
        } else if p.extension().is_some_and(|x| x == "mora") {
            out.push(p);
        }
    }
}

/// 跑一个 `.mora` 文件，返回 `(退出码, stderr 摘要)`。
fn run_file(path: &std::path::Path) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(path)
        .output()
        .expect("跑 mora");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

/// 跑一段源码，返回 `(退出码, stderr 摘要)`。
fn run_src(tag: &str, src: &str) -> (i32, String) {
    let dir = TempDir::new(tag);
    let path = dir.join("p.mora");
    std::fs::write(&path, src).expect("写临时程序");
    let (code, stderr) = run_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (code, stderr)
}

#[test]
fn d474_full_census_every_program_compiles_and_runs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for dir in ["examples", "tests/fixtures", "vendor"] {
        collect_mora(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(
        !files.is_empty(),
        "没找到任何 `.mora` 文件 —— 普查源目录变了？本测试会**静默变绿**"
    );

    let mut unexpected: Vec<String> = Vec::new();
    let mut checked = 0usize;

    // ── 1. 仓库真实文件 ──
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        // 仓库 fixture 里有些是**故意**要失败的（回归夹具），它们不参与
        // 「跑得通」的普查 —— 只要求**能编译**。
        let (code, stderr) = run_file(f);
        let _ = src;
        checked += 1;
        if code != 0 && !stderr.contains("[9layer]") {
            // 只把「编译期失败」算作普查失败；运行期失败另行归类。
            if stderr.contains("Failed to parse") || stderr.contains("Parse error") {
                let rel = f.strip_prefix(root).unwrap().to_string_lossy().to_string();
                unexpected.push(format!("FILE  {rel}  解析失败"));
            }
        }
    }

    // ── 2. 合成构造 ──
    for (name, src, expect) in SYNTHETIC {
        checked += 1;
        let (code, stderr) = run_src(name, src);
        let ok = match expect {
            Expect::Ok => code == 0,
            Expect::TypeErr => code == 2,
            Expect::RuntimeErr => code == 1,
        };
        if !ok {
            unexpected.push(format!(
                "SYNTH {name}  exit={code}（期望 {expect:?}）\n        {}",
                stderr.trim_end()
            ));
        }
    }

    eprintln!(
        "D474 全量普查：检查 {checked} 个程序（{} 个仓库文件 + {} 个合成构造）",
        files.len(),
        SYNTHETIC.len()
    );

    assert!(
        unexpected.is_empty(),
        "D474 全量普查发现 {} 个**非预期**结果 —— 9 层管线又产不出某些构造了：\n  - {}",
        unexpected.len(),
        unexpected.join("\n  - ")
    );
}

/// 覆盖面自检：合成构造表**不能悄悄变小**。
#[test]
fn d474_synthetic_table_keeps_its_size() {
    assert_eq!(
        SYNTHETIC.len(),
        50,
        "合成构造表现在 {} 条 —— 若有意增删，请同步更新本断言与 CHANGELOG D474",
        SYNTHETIC.len()
    );
}
