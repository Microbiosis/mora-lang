//! v0.104.6 D320：索引运算的**穷尽矩阵** —— 两个候选发现均被**证伪**。
//!
//! D319 的收获是「同一族里不同类型给出有的干净报错、有的静默错」。本轮把该方法
//! 用在索引运算上（`vm::index_value` / `checked_index` / `index_assign_value`
//! 同样是按 `(容器类型, 索引类型)` 分派的高频路径）。
//!
//! ## 矩阵
//!
//! 容器 4 类（`List` / `String` ASCII / `String` CJK / `Dict`）× 索引
//! 15 种取值（整数、负数、越界、小数、整值浮点、BigInt、错配类型…）。
//!
//! ## 两个候选发现，都是**有据的非发现**
//!
//! ### 一、「小数下标静默截断」= **D3 的既定设计**
//!
//! 实测 `xs[1.5]` → `xs[1]`、`xs[2.9]` → `xs[2]`，exit 0 零诊断。D320 一度
//! 参照 D28（负下标）与 `method_dispatch.rs` v0.104.6（越界吞成 nil）的原则
//! 「下标算错必须暴露」，把它改成报错 —— **被既有判据打回**：
//!
//! ```text
//! tests/depth_and_diagnostics.rs:324
//!   // 浮点下标按既有约定向零截断（D3 的设计决定）
//!   assert_eq!(run("…\nxs[1.9]").unwrap(), "Float(2.0)");
//!   assert_eq!(run("…\ns[1.9]").unwrap(), "Char('b')");
//! ```
//!
//! ⇒ **D28 / `method_dispatch` 讲的是「越界」与「负数」两条，不覆盖小数**。
//! 不能拿那条原则去推「小数也该报错」—— **原则的适用边界要看既有判据钉了什么**。
//! D320 的修改已回退。
//!
//! ### 二、「`index_assign_value` 是死代码」= **MIR 层仍可达**
//!
//! `xs[1] = 99` 与 `assign xs[1] = 99` 都是解析错误（`List` 自 v0.104.6 起
//! 不可变），看起来该函数没用了。但 `handlers/values.rs:156` 的
//! `MirInst::IndexAssign` 仍在调它 —— 手写 MIR / SSA 降级路径可达。
//! `tests/list_semantics.rs:14-19` **早已精确记录**了这一点。
//!
//! ## 矩阵实际结论
//!
//! `checked_index` 已被 D28 加固得相当完整：NaN、负数、越界、整值浮点、
//! 字符串按**字符**（非字节）计数、CJK 正常、类型错配给出可读消息。
//! **本轮没有找到新的缺陷。**

use std::process::Command;

/// 删除临时目录，**带重试**。
///
/// v0.104.6 D321：`mora.exe` 子进程在 `.output()` 返回后可能**尚未释放**
/// `p.mora` 的文件句柄；Windows 上此时 `remove_dir_all` 直接失败，而
/// `let _ =` 会把错误**静默吞掉** ⇒ 每跑一次判据就漏一个目录
/// （实测 `cargo test --no-fail-fast` 一次 +4 个）。
///
/// Windows 的句柄释放是异步的，重试即可覆盖；仍失败则**如实暴露**，
/// 不再伪装成「清理过了」。
fn cleanup_dir(dir: &std::path::Path) {
    for attempt in 0..8 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 7 => {
                panic!(
                    "D321：临时目录 {dir:?} 清理失败（{e}）。\n\
                     句柄可能仍被 `mora.exe` 子进程占用；重试 8 次仍失败。\n\
                     该目录会逐次累积 —— 请勿忽略。"
                );
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
}

fn run(src: &str, tag: &str) -> (i32, String) {
    // ⚠ 每条用例独立目录：同文件 `#[test]` 并行执行，共用目录会串味
    //   （D317 踩过）。tag 需先 slug —— 表达式里含 `(` `-` 等字符，
    //   直接拼进 %TEMP% 会得到 Windows `Os { code: 123 }`（D319 踩过）。
    let dir = std::env::temp_dir().join(format!("mora_d320_idx_{}", slug(tag)));
    cleanup_dir(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    cleanup_dir(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| {
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
        })
        .unwrap_or("<empty>")
        .replace(&p.to_string_lossy().to_string(), "<TMP>")
        .to_string();
    (out.status.code().unwrap_or(-1), first)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn print_of(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **主判据**：索引矩阵的**全部格子**的行为被钉住。
///
/// 每一格都写明「为什么是这个结果」——其中三格是**设计决定**（浮点截断），
/// 一格是**已记录的 MIR 层限制**（下标赋值语法已移除），其余是错误路径。
#[test]
fn d320_index_matrix_behaviour_is_pinned() {
    // ── List：正常取值 ──
    for (e, want) in [
        ("([10,20,30])[0]", "10.0"),
        ("([10,20,30])[2]", "30.0"),
        ("[1,2,3][len([1,2,3]) - 1]", "3.0"),
    ] {
        let (code, got) = print_of(e);
        assert_eq!(code, 0, "`{e}` 应成功; exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 {want}; 实得 {got}");
    }

    // ── List：越界 / 负数必须**显式报错**（D28 + v0.104.6 加固的）──
    for (e, needle) in [
        ("([10,20,30])[3]", "out of bounds"),
        ("([10,20,30])[10]", "out of bounds"),
        ("([10,20,30])[-1]", "negative index"),
        ("([10,20,30])[-5]", "negative index"),
    ] {
        let (code, got) = print_of(e);
        assert_ne!(
            code, 0,
            "`{e}` 必须报错而不是静默给值; 实得 exit=0 out={got}"
        );
        assert!(
            got.contains(needle),
            "`{e}` 的报错应含 `{needle}`; 实得: {got}"
        );
    }

    // ── **设计决定（D3）**：浮点下标向零截断，**不是缺陷** ──
    // ⚠ 改这一格前先读 `tests/depth_and_diagnostics.rs::d28_positive_index_unaffected`
    //   的注释：「浮点下标按既有约定向零截断（D3 的设计决定）」。
    for (e, want) in [
        ("([10,20,30])[1.0]", "20.0"),
        ("([10,20,30])[1.5]", "20.0"),
        ("([10,20,30])[2.9]", "30.0"),
    ] {
        let (code, got) = print_of(e);
        assert_eq!(
            code, 0,
            "`{e}` 应成功（浮点下标截断是 D3 的设计）; exit={code}"
        );
        assert_eq!(
            got, want,
            "`print({e})` 应向零截断得 {want}。\n\
             ⚠ 若本条失败，说明有人改了浮点下标的截断语义 —— 那是**语义变更**，\
             请先回到 CHANGELOG D320 记录裁决依据（D320 曾试图改成报错，已回退）。"
        );
    }

    // ── String：按**字符**计数（v0.104.6 起与 `len()` 同口径）──
    for (e, want) in [
        ("(\"abc\")[0]", "a"),
        ("(\"abc\")[2]", "c"),
        ("(\"中文\")[1]", "文"),
    ] {
        let (code, got) = print_of(e);
        assert_eq!(code, 0, "`{e}` 应成功; exit={code} out={got}");
        assert_eq!(got, want, "`print({e})` 应得 {want}; 实得 {got}");
    }
    let (code, got) = print_of("(\"中文\")[2]");
    assert_ne!(code, 0, "CJK 串 2 个字符，下标 2 必须越界报错; 实得 {got}");
    assert!(
        got.contains("string index"),
        "措辞应带 `string index`; 实得 {got}"
    );

    // ── 类型错配：给出**可读**消息（v0.104.6 修过 `{:?}` 打印 HashMap 序不定）──
    for (e, needle) in [
        ("([1,2])[\"a\"]", "cannot index"),
        ("({a:1})[0]", "cannot index"),
        ("({a:1})[0i]", "cannot index"),
        ("true[0]", "cannot index"),
        ("nil[0]", "cannot index"),
    ] {
        let (code, got) = print_of(e);
        assert_ne!(code, 0, "`{e}` 类型错配必须报错; 实得 out={got}");
        assert!(got.contains(needle), "`{e}` 应报 `{needle}`; 实得: {got}");
    }

    // ── Dict 缺键：返回 `nil`（exit 0），**不是** out-of-bounds 错误 ──
    //    list 越界即错，dict 缺键给 nil —— 两者语义本就不同（既有约定）。
    let (code, got) = print_of("({a:1,b:2})[\"zz\"]");
    assert_eq!(
        code, 0,
        "dict 缺键应返回 nil 而非报错; exit={code} out={got}"
    );
    assert_eq!(
        got, "nil",
        "`print(({{a:1}})[\"zz\"])` 应得 nil; 实得 {got}"
    );
}

/// **已记录的 MIR 层限制**：下标赋值语法在 v0.104.6 移除，但 `index_assign_value`
/// 仍被 `MirInst::IndexAssign`（手写 MIR / SSA 降级）调用，**不是死代码**。
///
/// `tests/list_semantics.rs` 已记录这一点；本条把它与「源码层确实是解析错误」
/// 一起钉住，防止将来有人照着解析错误把该函数删掉。
#[test]
fn d320_index_assign_is_syntax_error_but_mir_handler_still_exists() {
    for src in [
        "let xs = [1,2,3]\nxs[1] = 99\nprint(xs)\n",
        "let xs = [1,2,3]\nassign xs[1] = 99\nprint(xs)\n",
    ] {
        let (code, got) = run(src, &slug(src));
        assert_eq!(
            code, 2,
            "`List` 自 v0.104.6 起不可变，下标赋值应是**解析错误**（exit 2）; \
             实得 exit={code} out={got}\n  源码: {src}"
        );
    }
    // 该函数仍存在且被 handlers 调用（编译期即可见：能取到它的地址）。
    // 源码层无调用方 ≠ 死代码 —— MIR 层可达。
    let _f: fn(
        &mut mora::value::Value,
        &mora::value::Value,
        &mora::value::Value,
    ) -> Result<(), String> = mora::mir::vm::index_assign_value;
}
