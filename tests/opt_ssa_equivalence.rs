//! v0.104.6 D187：`--opt>=1` 让程序**在第一个 `for` 循环处静默中止** ——
//! exit 0、无任何诊断，而**在此之前**的输出照常打印。
//!
//! ## 缺陷（真实 CLI 实测，非库内推断）
//!
//! `mora --help` 写着
//! `mora --opt=1 file.mora  Run with SSA optimization (0=off/1=basic/>=2=aggressive)`。
//! 实测：
//!
//! ```text
//! # a.mora:  print("A") / for … end / print("B") / print(acc)
//! $ mora a.mora        →  A ; B ; 6.0            exit 0
//! $ mora --opt=1 a.mora →  A                       exit 0   ← 停在循环处
//! ```
//!
//! **不是「输出被吞」，是执行中止。** 且中止得**静悄悄**：无错误、无警告、
//! 退出码 0 —— 用户看到「打印了 A，程序正常结束」。
//!
//! ## 覆盖面：8 个常用构造里 5 个在 `--opt>=1` 下坏掉
//!
//! | 构造 | opt=off | opt=1 | opt=2 |
//! |---|---|---|---|
//! | `for` 累加 / `for` 逐项 print | `6.0` / `1;2;3` | **空** | **空** |
//! | `let a = 1` + `print(a)` | `1.0` | `1.0` | **空** |
//! | `dict.get` | `1.0` | `1.0` | **空** |
//! | `[1,2,3].map(闭包)` | `[11,12,13]` | **内部错误** exit 1 | 同 |
//! | `if` **常量**条件 + `else` | `7.0` | **`7.0 ; 8.0`（两分支都跑）** | 同 |
//! | `if` 变量条件 | `SMALL` | `SMALL` | `SMALL` |
//! | `while` | `3.0` | `3.0` | `3.0` |
//! | 裸 `print(1)` | `1.0` | `1.0` | `1.0` |
//!
//! `if` 那条尤其严重：**常量条件的死分支没有被消掉，两个分支都执行** ——
//! 副作用会**重复发生**（重复写文件、重复扣款、重复发送）。变量条件正常，
//! 说明是**常量折叠**路径上的缺陷。
//!
//! ## 与既有测试的关系（本轮最刺眼的发现）
//!
//! `tests/mir_ssa_roundtrip.rs::basic_pipeline_runs_without_panic` 里
//! **正好有我这个失败的 `for` 用例**：
//!
//! ```rust
//! optimize_without_panic(
//!     "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
//!     OptLevel::Basic,
//! );
//! ```
//!
//! 它的判据是 **`optimize_without_panic` —— 「不 panic 即通过」**，
//! **完全不检查输出**。把那段源码拿真实 CLI 跑：
//!
//! ```text
//! opt=off → 6.0     opt=1 → 空（exit 0）     opt=2 → 空（exit 0）
//! ```
//!
//! 也就是说：**一条判据无法区分「跑对了」与「静默什么都没做」的测试，
//! 正在「守」着一个文档化、宣传、且大面积坏掉的功能。**
//!
//! 同族形状本会话已记过两次（D171「正对照对假绿零判别力」、
//! D182「命令跑完了 ≠ 命令做成了」），这是第三次，但这次守的是**优化器**。
//!
//! ## 本文件的性质：**现状断言**，不是正确行为断言
//!
//! 修 `--opt` 是优化器可达性/常量折叠的深层工程（`mir/opt.rs` + SSA 管线），
//! 远超本轮能力，**未擅自实施**。故本文件按 `tests/ai_namespace_reachability.rs`
//! （D59）既有的做法：把**当前坏掉的现状**钉成断言，并写清「这是缺陷现状，
//! 不是正确行为」。修好之后应把 `KNOWN_DIVERGENT` 清空、把断言翻转为等价。
//!
//! 与 D59 的区别要说清：那里断言的是**「不可达」这个现状**（因为该修复属
//! 文法设计决定）；这里断言的是**「跑不对」这个现状**（属实现缺陷，
//! 只是本轮未修）。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d187_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, body).expect("写脚本");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

/// 跑一段 Mora，把**程序自己的输出**（剥掉横幅）与退出码取出来。
fn run(dir: &Path, opt: Option<&str>, file: &str) -> (Vec<String>, i32) {
    let mut cmd = Command::new(mora_exe());
    cmd.current_dir(dir)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL");
    // ⚠ `--opt=` **必须放在文件前面** —— CLI 只扫第一个非选项参数之前的
    // 选项。放文件后面会被**静默忽略**，于是 opt=off 与 opt=1 跑出同一结果，
    // 差异测不出来（我第一版就这么写的，5 条全红）。
    if let Some(l) = opt {
        cmd.arg(format!("--opt={l}"));
    }
    cmd.arg(file);
    let out = cmd.output().expect("跑 mora");
    // ⚠ **必须合并 stdout 与 stderr** —— 运行时错误（如 `.map` 那条的内部
    // 不变量）走的是 stderr，只读 stdout 会把「报错了」看成「什么都没输出」。
    // 我第一版只读 stdout，`map` 那条因此空跑成 `[]`。
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines: Vec<String> = text
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
        })
        .map(str::to_string)
        .collect();
    (lines, out.status.code().unwrap_or(-1))
}

/// **`--opt=1` 曾让程序在 `for` 处**静默中止**（v0.104.6 D312 已修）**。
///
/// **本条曾是现状判据**（钉住「opt=1 只输出 A 就结束」），D312 修好后按其自身
/// 指示**翻转为「与 opt=off 等价」**。修前是静默中止：循环体读的物理寄存器
/// 首次迭代时**无任何生产者**（SSA 把一个循环携带寄存器拆成两套物理编号，
/// 初始化值与循环体永久失联）⇒ DAG 节点永不激活 ⇒ 整条链饿死，退出码仍是 0。
///
/// 详见 `tests/opt_phi_guard.rs`。
#[test]
fn d187_opt1_runs_a_for_loop_equivalently() {
    let dir = WorkDir::new("for");
    let file = dir.script(
        "a.mora",
        "print(\"A\")\nlet acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(\"B\")\nprint(acc)\n",
    );
    let p = file.to_str().unwrap();

    let (base, base_code) = run(dir.path(), None, p);
    assert_eq!(
        base,
        vec!["A".to_string(), "B".to_string(), "6.0".to_string()],
        "前提：不带 --opt 时程序应完整执行（opt=off exit={}）",
        base_code
    );

    let (got, code) = run(dir.path(), Some("1"), p);
    assert_eq!(code, 0, "`--opt=1` 应成功执行; 实得 exit={code}");
    assert_eq!(
        got, base,
        "**D312**：`--opt=1` 下的 `for` 循环必须与 opt=off **逐行等价**。\n\
         修前是静默中止（只输出 [\"A\"]，退出码仍是 0，零诊断）。\n  实得: {got:?}"
    );
}

/// `if` 的**常量条件**在 `--opt>=1` 下**曾两个分支都执行** —— 死分支没消掉。
///
/// 副作用会重复发生（重复写文件、重复扣款、重复发送），比「静默中止」
/// 更危险：程序**看起来在正常工作**。
///
/// **v0.104.6 D313 已修**，本条按其自身指示**翻转**为正确行为断言。
/// 根因不是「死分支没消掉」，而是 `IfSimplifyRule` 删掉 `JumpIfNot` 后留下
/// 的一条**越界跳转**（`if` 是最后一句时 `end == body.len()`）被
/// `construct` / `deconstruct` 拆散。详见 `tests/opt_out_of_range_jump_guard.rs`。
#[test]
fn d187_opt1_executes_only_the_taken_branch_of_a_constant_if() {
    let dir = WorkDir::new("ifconst");
    let file = dir.script("b.mora", "if 1 > 0\n  print(7)\nelse\n  print(8)\nend\n");
    let p = file.to_str().unwrap();

    let (base, _) = run(dir.path(), None, p);
    assert_eq!(base, vec!["7.0".to_string()], "前提：只应走 then 分支");

    let (got, code) = run(dir.path(), Some("1"), p);
    assert_eq!(code, 0, "`--opt=1` 应成功执行; 实得 exit={code}");
    assert_eq!(
        got, base,
        "**D313**：`--opt=1` 下常量条件的 if/else 必须只走 then 分支。\n\
         修前是**两个分支都执行**（副作用重复，程序看起来却一切正常）。\n  实得: {got:?}"
    );
}

/// **对照组**：变量条件的 `if/else` 当前是**正确**的。
///
/// 这条很重要 —— 它把缺陷范围**收窄**到「常量条件」这一支，
/// 避免把「`if` 整个坏了」这种过宽的说法钉进断言。
#[test]
fn d187_variable_condition_if_is_still_correct_under_opt() {
    let dir = WorkDir::new("ifvar");
    let file = dir.script(
        "c.mora",
        "let n = 5\nif n > 100\n  print(\"BIG\")\nelse\n  print(\"SMALL\")\nend\n",
    );
    let p = file.to_str().unwrap();

    let (base, _) = run(dir.path(), None, p);
    let (got, _) = run(dir.path(), Some("1"), p);
    assert_eq!(
        base,
        vec!["SMALL".to_string()],
        "前提：不带 --opt 应走 else"
    );
    assert_eq!(
        got, base,
        "变量条件的 if/else 在 --opt=1 下**当前是正确的** —— \
         缺陷只在常量条件那一支（本条是它的对照组）"
    );
}

/// **对照组**：`while` 与裸 `print` 在 `--opt=1` 下当前是**正确**的。
#[test]
fn d187_while_and_plain_print_survive_opt1() {
    let dir = WorkDir::new("ok");
    for (name, src) in [
        (
            "w.mora",
            "let i = 0\nwhile i < 3\n  i = i + 1\nend\nprint(i)\n",
        ),
        ("p.mora", "print(1)\n"),
    ] {
        let file = dir.script(name, src);
        let p = file.to_str().unwrap();
        let (base, _) = run(dir.path(), None, p);
        let (got, _) = run(dir.path(), Some("1"), p);
        assert_eq!(got, base, "[{}] 在 --opt=1 下应与 opt=off 等价", name);
    }
}

/// `.map(闭包)` 在 `--opt>=1` 下**必须与 opt=off 等价**（D310 已修）。
///
/// 原始记档：它在 opt≥1 下把**内部不变量**抛给用户
/// （`DAG node 0 references register 8 … a unit-statement emitter returned an
/// unallocated sentinel register`）；D304 修好 `n_regs` 越界后变成「静默无输出」；
/// D310 修好根因（透传指令与 SSA 重编号的两套寄存器空间）。
#[test]
fn d187_opt1_map_closure_leaks_an_internal_invariant() {
    let dir = WorkDir::new("map");
    let file = dir.script("m.mora", "print([1,2,3].map(fn(v) v + 10))\n");
    let p = file.to_str().unwrap();

    let (base, _) = run(dir.path(), None, p);
    assert_eq!(
        base,
        vec!["[11.0, 12.0, 13.0]".to_string()],
        "前提：opt=off 应正常"
    );

    let (got, code) = run(dir.path(), Some("1"), p);
    // v0.104.6 D310：这条**修好了**，断言已从「现状」翻转为「正确行为」。
    //
    // 演进链：
    //   D187 原始记档 —— opt≥1 下把内部不变量抛给用户
    //         （`DAG node 0 references register 8 … a unit-statement emitter
    //         returned an unallocated sentinel register`）；
    //   D304 修好 `n_regs` 越界 ⇒ 内部消息不再泄漏，但变成「静默无输出」；
    //   D310 修好根因 —— `Closure` / `MatchExpr` 等**透传指令**的寄存器不参与
    //         SSA 重编号，而同一函数里其余指令会重编号 ⇒ 两套寄存器空间
    //         （`Closure` 写 r8 而 `map` 读 r2）。修法：含透传指令的函数
    //         **整体跳过 SSA**。
    assert_eq!(code, 0, "`--opt=1` 应成功执行；实得 {code}");
    assert!(
        !got.iter().any(|l| l.contains("sentinel register")),
        "实现内部细节（`sentinel register` 等）**不得**出现在用户输出里。\
         实得: {got:?}"
    );
    assert_eq!(
        got,
        vec!["[11.0, 12.0, 13.0]".to_string()],
        "**D310**：`--opt=1` 下 map+闭包必须与 opt=off 等价。\
         修前是「静默无输出」。实得: {got:?}"
    );
}
