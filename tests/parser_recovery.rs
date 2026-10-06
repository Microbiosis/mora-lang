//! v0.104.6 D44 / D44b / D45：`parser_v3` 的**恢复路径**三连缺陷。
//!
//! 三者同源：块体/子体的语句循环在「解析不出内容」时都选择了**静默**——
//! 前进一 token 继续、或干脆跳过。三种静默各自的后果分别是：
//!
//! | # | 位置 | 修前行为（真实 CLI `mora run` 实测） |
//! |---|------|--------------------------------------|
//! | D44 | `std::mem::replace(&mut self.emit, …)` 后的 `?` | 父上下文被丢弃 → **外层函数体剩余部分全被发射进一次性子上下文后静默消失** |
//! | D44b | match arm 解析失败 | `self.advance()` 吞掉该 arm → 少一个分支，**静默给出错误答案，exit 0** |
//! | D45 | 5 个块体的语句循环 `if let Some(..) {}` | 语句解析失败**不消费任何 token** → 循环条件恒真 → **死循环，编译器永不返回** |
//!
//! ## D44 的因果链（由本文件 B1 用例钉住）
//!
//! ```mora
//! task w()
//!   match 1 with
//!     1 -> 5
//!     2 -> return 9   // arm 体只接受 expr（spec §14.2）→ arm 发射失败
//!   end
//! end
//! w()
//! ```
//!
//! * **修前**：`emit_match_arm_w` 在 `std::mem::replace` 之后 `?` 早退，父上下文
//!   被丢弃；此后 `emit_match_w` 的 `MatchExpr`、`alloc_reg`、以及外层
//!   `emit_tail_return` 全部落进那个一次性上下文。**匹配对象 `1` 的
//!   `Const` 指令随父上下文一起消失** → `MatchExpr { val: 0 }` 读一个从未写入的
//!   寄存器 → 好 arm 也匹配不上 → `w()` 返回 `nil`。
//! * 只修好 arm（仍静默吞）后：好 arm 恢复，坏 arm 消失 → 返回 `1.0`。
//! * 三处都修好：编译期报错。
//!
//! 一次「修对一处」就能看到 `nil` → `1.0` 的位移，是 D44 因果的**判决实验**。
//!
//! ## 为什么 D45 的测试可以「直接断言编译失败」
//!
//! 修法里有一条**显式不变量**：块体循环每轮必须至少消费一个 token
//! （`let before = self.current; … if self.current == before { 报错 }`）。
//! 只要该不变量在，空转就不可能发生 —— 故本文件可以直接断言
//! 「畸形输入必须返回错误」。若将来有人删掉不变量，症状是**测试套件整体
//! 超时**而不是某条断言失败；这在 CI 里是同样醒目的失败，且比「悄悄挂住」
//! 好（挂住的测试无法区分是真挂死还是机器慢）。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::value::Value;

fn compile(src: &str) -> Result<(), String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).map(|_| ())
}

/// 返回 `Ok(None)` 表示运行期结果是 `Nil`（无匹配 arm 时的正确结果）。
fn run_f64(src: &str) -> Result<Option<f64>, String> {
    let (func, _w) = mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(&arc, &mut interp, &mut env, &mut Effects::new()) {
        Ok(Value::Float(f)) => Ok(Some(f)),
        Ok(Value::Nil) => Ok(None),
        Ok(other) => Err(format!("期望 Float/Nil，实得 {other:?}")),
        Err(e) => Err(e),
    }
}

// ===================================================================
// D44b：畸形 match arm 必须报错
// ===================================================================

/// arm 体按 spec §14.2 只接受 `expr`，故下列 arm 全部非法。
///
/// 修前它们**全部 exit 0**：坏 arm 被 `self.advance()` 静默吞掉，match 少分支
/// 或变成零 arm，程序「成功」返回一个错误答案。
#[test]
fn malformed_match_arm_is_rejected() {
    let cases: &[(&str, &str)] = &[
        // arm 体是语句而非表达式
        (
            "arm_return",
            "task w()\n  match 1 with\n    1 -> return 9\n  end\nend\nw()\n",
        ),
        (
            "arm_break",
            "task w()\n  match 1 with\n    1 -> break\n  end\nend\nw()\n",
        ),
        (
            "arm_let",
            "task w()\n  match 1 with\n    1 -> let z = 7\n  end\nend\nw()\n",
        ),
        // 空体 / 纯垃圾
        (
            "arm_empty",
            "task w()\n  match 1 with\n    1 ->\n  end\nend\nw()\n",
        ),
        (
            "arm_garbage",
            "task w()\n  match 1 with\n    1 -> @@@\n  end\nend\nw()\n",
        ),
        // 后一个 arm 坏：修前连**前一个合法 arm** 也失效（返回 nil）
        (
            "second_arm_bad",
            "task w()\n  match 1 with\n    1 -> 5\n    2 -> return 9\n  end\nend\nw()\n",
        ),
        (
            "first_arm_bad",
            "task w()\n  match 1 with\n    1 -> return 9\n    2 -> 5\n  end\nend\nw()\n",
        ),
        // 大括号形态同样静默
        (
            "brace_form",
            "task w()\n  match 1 {\n    1 => return 7,\n  }\nend\nw()\n",
        ),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        if compile(src).is_ok() {
            failures.push(format!("  [{name}] 编译通过（应拒绝）"));
        }
    }
    assert!(
        failures.is_empty(),
        "畸形 match arm 必须编译期报错。修前它们全部 exit 0 —— 坏 arm 被静默吞掉，\
         `second_arm_bad` / `first_arm_bad` 里连**合法的** arm 也一并失效（nil）：\n{}",
        failures.join("\n")
    );
}

/// 合法 arm 的**全部**形态必须照常工作 —— D44b 的收紧不得误伤。
///
/// 这条尤其重要：修法要先把 arm 之间的换行清理**前移**到 arm 尝试之前。
/// 此前清理写在尝试**之后**，循环首次进入时 token 必是 Newline →
/// `parse_pattern` 失败 → 落到 `self.advance()` 恢复分支。也就是说
/// **那个恢复分支是承重的**，把它直接换成报错会让所有 match 形式失效
/// （实测 7 个形态全部挂掉）。故本测试钉住「收紧后仍全对」。
#[test]
fn valid_match_forms_still_work() {
    // (名称, 源码, 期望返回值；None = Nil)
    let cases: &[(&str, &str, Option<f64>)] = &[
        (
            "1 arm",
            "let v = match 1 with\n  1 -> 5\nend\nv\n",
            Some(5.0),
        ),
        (
            "2 arms",
            "let v = match 1 with\n  1 -> 5\n  2 -> 9\nend\nv\n",
            Some(5.0),
        ),
        (
            "in task",
            "task w()\n  match 1 with\n    1 -> 5\n  end\nend\nw()\n",
            Some(5.0),
        ),
        ("brace 1", "let v = match 1 {\n  1 => 5,\n}\nv\n", Some(5.0)),
        (
            "brace 2",
            "let v = match 1 {\n  1 => 5,\n  2 => 9,\n}\nv\n",
            Some(5.0),
        ),
        (
            "guard",
            "let v = match 1 with\n  1 when true -> 5\nend\nv\n",
            Some(5.0),
        ),
        // 无任何 arm 匹配 → Nil（不是 0）
        ("no match", "let v = match 9 with\n  1 -> 5\nend\nv\n", None),
    ];
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        match run_f64(src) {
            Ok(got) => {
                if got != *want {
                    failures.push(format!("  [{name}] 期望 {want:?}，实得 {got:?}"));
                }
            }
            Err(e) => failures.push(format!("  [{name}] 运行失败: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "合法 match 的全部形态必须照常工作：\n{}",
        failures.join("\n")
    );
}

// ===================================================================
// D45：块体里的畸形语句必须报错，不能挂死
// ===================================================================

/// 修前这 5 个块体的语句循环是 `if let Some(..) { .. }` —— 解析失败就跳过，
/// 而失败路径**不消费任何 token** → `while !check(End)` 条件恒真 → **死循环**。
///
/// 实测（真实 CLI，限时 8s）：`parallel` / `worker` / `observe` / `span` /
/// `section`(prompt|document) / `with` 六种全部挂死，编译器永不返回。
#[test]
fn unparsable_statement_in_block_is_rejected() {
    let cases: &[(&str, &str)] = &[
        ("parallel", "parallel\n  1\n  @@@\n  2\nend\nprint(111)\n"),
        ("worker", "worker w\n  1\n  @@@\n  2\nend\nprint(111)\n"),
        (
            "observe",
            "observe tr do\n  1\n  @@@\n  2\nend\nprint(111)\n",
        ),
        ("span", "span \"s\" do\n  1\n  @@@\n  2\nend\nprint(111)\n"),
        (
            "section",
            "prompt \"p\" do\n  1\n  @@@\n  2\nend\nprint(111)\n",
        ),
        (
            "with",
            "with model = \"gpt-4o\"\n  1\n  @@@\n  2\nend\nprint(111)\n",
        ),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        if compile(src).is_ok() {
            failures.push(format!("  [{name}] 编译通过（应拒绝）"));
        }
    }
    assert!(
        failures.is_empty(),
        "块体内的畸形语句必须报错。修前这 6 种块体全部**死循环**（编译器永不返回）：\n{}",
        failures.join("\n")
    );
}

/// 其余块体在修前就已经正确报错（它们用 `?` 传播失败，不是 `if let` 跳过）。
/// 钉住它们，防止将来有人把 `?` 改成跳过而引入挂死。
#[test]
fn unparsable_statement_elsewhere_is_rejected() {
    let cases: &[(&str, &str)] = &[
        ("task", "task w()\n  1\n  @@@\n  2\nend\nw()\n"),
        (
            "transaction",
            "transaction\n  1\n  @@@\n  2\nend\nprint(111)\n",
        ),
        ("macro", "macro m(x)\n  @@@\nend\nprint(111)\n"),
        ("update", "update(m)\n  @@@\nend\nprint(111)\n"),
        ("closure", "let f = fn(x) @@@ end\nprint(111)\n"),
        ("top level", "@@@\nprint(111)\n"),
        ("if body", "if true then\n  @@@\nend\nprint(111)\n"),
        ("for body", "for i in [0]\n  @@@\nend\nprint(111)\n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        if compile(src).is_ok() {
            failures.push(format!("  [{name}] 编译通过（应拒绝）"));
        }
    }
    assert!(
        failures.is_empty(),
        "这些位置修前已正确报错，必须保持：\n{}",
        failures.join("\n")
    );
}

// ===================================================================
// D46：字段列表类块（struct / enum / app）的字段循环
// ===================================================================

/// 三个成员的病各不相同，但同源 —— 字段循环缺少失败处理：
///
/// | 块 | 修前行为 |
/// |---|----------|
/// | `enum` | **根本没有 `else` 分支** —— 变体名解析失败时既不 push 也不 advance → 循环条件恒真 → **死循环**，且每次重打错误刷屏 |
/// | `struct` | 字段名失败 → `self.advance()` **静默跳过整行**；类型标注失败 → `if let Some(..)` **静默丢字段** |
/// | `app` | 字段失败 → `self.advance()` **静默跳过整行** |
///
/// `struct` / `app` 的后果是**静默的错误成功**（exit 0）：用户以为 `x: Int`
/// 声明成功了，后续代码照常执行，零提示。
#[test]
fn malformed_field_in_decl_block_is_rejected() {
    let cases: &[(&str, &str)] = &[
        // enum —— 修前死循环
        (
            "enum_bad_variant",
            "enum E\n  A\n  @@@\n  B\nend\nprint(111)\n",
        ),
        // struct —— 修前静默跳过整行
        (
            "struct_bad_field",
            "struct S\n  @@@\n  x: Int\nend\nprint(111)\n",
        ),
        // struct —— 修前静默丢字段
        (
            "struct_bad_type",
            "struct S\n  x: @@@\n  y: Int\nend\nprint(111)\n",
        ),
        (
            "struct_bad_type_only",
            "struct S\n  x: @@@\nend\nprint(111)\n",
        ),
        // app —— 修前静默跳过整行
        (
            "app_bad_field",
            "app a\n  model: M\n  @@@\n  msg: N\nend\nprint(111)\n",
        ),
        (
            "app_bad_init",
            "app a\n  model: M\n  init: @@@\n  msg: N\nend\nprint(111)\n",
        ),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        if compile(src).is_ok() {
            failures.push(format!("  [{name}] 编译通过（应拒绝）"));
        }
    }
    assert!(
        failures.is_empty(),
        "字段列表类块的畸形字段必须报错。修前 `enum` **死循环**；\
         `struct` / `app` 静默跳过并 **exit 0**（用户以为字段声明成功了）：\n{}",
        failures.join("\n")
    );
}

/// 合法声明块必须照常工作 —— D46 的收紧不得误伤。
#[test]
fn valid_decl_blocks_still_work() {
    for (name, src) in [
        ("struct", "struct S\n  x: Int\n  y: Int\nend\nprint(111)\n"),
        ("enum", "enum E\n  A\n  B\nend\nprint(111)\n"),
        ("app", "app a\n  model: M\n  msg: N\nend\nprint(111)\n"),
    ] {
        if let Err(e) = compile(src) {
            panic!("[{name}] 合法声明块必须可编译，实得: {e}");
        }
    }
}
