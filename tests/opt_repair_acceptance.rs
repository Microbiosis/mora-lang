//! v0.104.6 D261 —— **`rename_variables` 修复完成时的验收判据**。
//!
//! ## 它是什么
//!
//! 一条判据：22 个代表性程序，每个在 `--opt=off` / `--opt=1` / `--opt=2`
//! 三档下运行，断言三档输出**完全一致**。
//!
//! ## 状态变迁
//!
//! | 时间 | 三档一致 | 备注 |
//! |---|---|---|
//! | D261 建档 | 16/22 | `#[ignore]`，等三处缺口修好 |
//! | D310 | 17/22 | `map` / `match` / 闭包修好 |
//! | D311 | — | 守卫收窄，48/56 → 54/56 |
//! | **D312** | **22/22** | **解封，改为常驻回归护栏** |
//!
//! D261 时剩下破坏的 6 个**全部是含 `for` 循环的**：
//! `for-accumulate` / `for-print-item` / `for-print-const` /
//! `for-empty` / `nested-for` / `break-in-for`
//!
//! ## 它现在挡住什么
//!
//! D254–D256 已把根因定位到三处代码缺口（`phi.incoming` 从不填充、
//! `BasicBlock.preds` 不完整、重编号只写回普通指令导致撞号）。D312 逐条
//! 转储确认了其中两处的具体形态：
//!
//! - **`phi.incoming`**：`insert_phi_nodes` 以 `incoming: Vec::new()` 建 phi，
//!   而 `rename_variables` 收的是 `&HashMap`（不可变）且内部 `clone()`。
//!   实测 `TOTAL_PHI=7 TOTAL_INCOMING=0`，deconstruct 产物**零条 `Copy`** ——
//!   整个 phi 机制从未生效过。
//! - **`BasicBlock.preds`**：`Return(None)` 的真实语义是**顺序落下**
//!   （`deconstruct.rs:296-300` 明写「不发射，线性执行自然落到最后一条」），
//!   但 CFG 侧把它当终点 ⇒ 落下的边不记 ⇒ 遍历从 `vec![0]` 出发只走得到
//!   block 0，**其余块从不重命名**（实测 block 0 `succs=[]`）。
//!
//! **三处缺口本身都还没修。** D312 取的是保守方向：SSA 里一旦出现 phi 就
//! 整个函数跳过 SSA（`opt.rs`），从而让这三条路径**不可达**。
//!
//! ⇒ **本条现在是那道守卫的活护栏**：谁把守卫改窄、改窄的判据、或删掉它，
//! 本条会红。它从「验收测试」变成了「回归测试」。

use std::path::PathBuf;

const PROGRAMS: &[(&str, &str)] = &[
    ("print-literal", "print(\"A\")\n"),
    ("let-bind", "let a = 1\nprint(a)\n"),
    ("let-two", "let a = 1\nlet b = 2\nprint(a + b)\n"),
    ("arith", "print(1 + 2 * 3)\n"),
    ("compare", "print(3 > 2)\n"),
    (
        "for-accumulate",
        "let acc = 0\nfor i in [1,2,3]\n  acc = acc + i\nend\nprint(acc)\n",
    ),
    ("for-print-item", "for i in [1,2,3]\n  print(i)\nend\n"),
    ("for-print-const", "for i in [1,2,3]\n  print(\"x\")\nend\n"),
    ("for-empty", "for i in [1,2,3]\nend\nprint(\"done\")\n"),
    (
        "while-accumulate",
        "let n = 3\nlet acc = 0\nwhile n > 0\n  acc = acc + n\n  n = n - 1\nend\nprint(acc)\n",
    ),
    ("if-constant", "if 1 == 1\n  print(\"T\")\nend\n"),
    (
        "if-variable",
        "let a = 2\nif a > 1\n  print(\"BIG\")\nend\n",
    ),
    ("dict-get", "let d = {a: 1, b: 2}\nprint(d.get(\"a\"))\n"),
    ("dict-len", "let d = {a: 1, b: 2}\nprint(len(d))\n"),
    ("list-len", "let xs = [1,2,3]\nprint(len(xs))\n"),
    ("list-index", "let xs = [1,2,3]\nprint(xs[1])\n"),
    (
        "list-append",
        "let xs = [1,2]\nlet ys = xs.push(3)\nprint(len(ys))\n",
    ),
    ("string-len", "let s = \"abc\"\nprint(len(s))\n"),
    ("string-concat", "print(\"a\" + \"b\")\n"),
    (
        "nested-for",
        "for i in [1,2]\n  for j in [1,2]\n    print(i)\n  end\nend\n",
    ),
    (
        "break-in-for",
        "for i in [1,2,3]\n  if i == 2\n    break\n  end\n  print(i)\nend\n",
    ),
    ("shadow", "let a = 1\nlet a = a + 1\nprint(a)\n"),
];

const NOISE: &[&str] = &[
    "Mora v",
    "AI ",
    "Built-in",
    "v0.15",
    "⚠",
    "AI 原语",
    "显式 API",
    "Trait",
];

/// `Drop` 守卫：解封后若中途 assert 失败，panic 会跳过后续清理。
struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(exe: &str, dir: &std::path::Path, src: &str, opt: Option<&str>) -> (i32, Vec<String>) {
    let f = dir.join("a.mora");
    std::fs::write(&f, src).expect("write scratch");
    let mut c = std::process::Command::new(exe);
    if let Some(o) = opt {
        c.arg(o);
    }
    let out = c.arg("run").arg(&f).output().expect("run mora");
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.contains(n)))
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

/// 验收判据：`--opt` 不得改变任何程序的**可观察行为**。
///
/// **D312 起常驻运行**（此前 `#[ignore]`）。三处 `rename_variables` 缺口
/// 本身仍未修，是 `opt.rs` 的「SSA 里出现 phi 就整体跳过」守卫让这三条
/// 路径不可达而通过的。守卫一旦被改窄或删除，本条会红。
#[test]
fn d261_opt_levels_never_change_program_behaviour() {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let dir = std::env::temp_dir().join(format!("mora_d261_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let _work = WorkDir(dir.clone());

    let mut broken: Vec<String> = Vec::new();
    for (name, src) in PROGRAMS {
        let (c0, o0) = run(exe, &dir, src, None);
        let (c1, o1) = run(exe, &dir, src, Some("--opt=1"));
        let (c2, o2) = run(exe, &dir, src, Some("--opt=2"));

        assert_eq!(c0, 0, "{name}: opt=off 应成功（对照失效）");
        assert!(!o0.is_empty(), "{name}: opt=off 无输出（对照失效）");
        // 用引用比较：`(c1, o1) != (c0, o0)` 会把 Vec 移走，后面的 format! 就借用不到。
        if (&c1, &o1) != (&c0, &o0) {
            broken.push(format!(
                "{name}: opt=1 exit={c1} {o1:?} ≠ off exit={c0} {o0:?}"
            ));
        }
        if (&c2, &o2) != (&c0, &o0) {
            broken.push(format!(
                "{name}: opt=2 exit={c2} {o2:?} ≠ off exit={c0} {o0:?}"
            ));
        }
    }

    assert!(
        broken.is_empty(),
        "`--opt` 仍然改变了 {} / {} 个程序的行为：\n  - {}",
        broken.len(),
        PROGRAMS.len(),
        broken.join("\n  - ")
    );
}
