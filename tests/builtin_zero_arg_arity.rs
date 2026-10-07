//! v0.104.6 D327 —— 一元内建缺参时**静默**当成 `nil`（**已修**）
//!
//! ## 缺陷
//!
//! `interpreter/builtin_impls.rs` 里 `str` / `int` / `float` / `bool` / `atom`
//! 五个入口此前都是同一行：
//!
//! ```rust
//! let v = args.first().cloned().unwrap_or(Value::Nil);
//! ```
//!
//! 少传参数时**不报错**，而是当 `nil` 继续算：
//!
//! | 调用 | 修前 | 修后 |
//! |---|---|---|
//! | `str()` | **`"nil"`，exit 0** ❌ | exit 1「str() requires 1 argument」|
//! | `bool()` | **`false`，exit 0** ❌ | exit 1「bool() requires 1 argument」|
//! | `atom()` | **`Atom(Nil)`，exit 0** ❌ | exit 1「atom() requires 1 argument」|
//! | `int()` | exit 1「int() **does not accept nil**」⚠ | exit 1「int() requires 1 argument」|
//! | `float()` | exit 1「float() **does not accept nil**」⚠ | exit 1「float() requires 1 argument」|
//!
//! `int` / `float` 之所以**碰巧**报错，是随后撞上「nil 不可转换」——
//! 而那句话把「没给参数」误报成「给了个不能用的 nil」，排查方向会被带偏。
//!
//! ## 方向不是随手选的：既有契约已经钉死「缺参报错」
//!
//! - `typeck/hm/builtin.rs` 把这五个**全部**登记成**一元**箭头
//!   （`str: α → String` / `bool: α → Bool` / `atom: α → Atom` …）；
//! - 同族的 `type_of()` / `deref()` / `methods_of()` / `len()` / `compose()`
//!   本来就**全都**对缺参报错。
//!
//! ⇒ 本条不是新增约束，只是**让实现回到已有签名上**。
//!
//! ## 与「不能收紧上限」的关系
//!
//! `tests/signature_no_over_tightening.rs` 定下的规矩是「运行期普遍忽略多余
//! 实参，所以补签名时**只能收紧下限、不能收紧上限**」。本条收紧的正是
//! **下限**（0 → 1），方向合规；多余实参的行为一条未动（见下方对照）。
//!
//! ## 对照组
//!
//! 末尾的 `d327_extra_args_still_rejected_by_typeck_not_by_this_change` 专门对照
//! **上限**这侧：多传实参仍然被拒，但拒绝点在 typeck（柯里化箭头），
//! 与本条收紧的下限无关。真正逐字钉住「只查下限」的是 `require_1` 的源码层断言
//! —— 它必须用 `args.first()`，不得写成 `args.len() == 1`。

use std::path::PathBuf;
use std::process::Command;

/// 这五个在 `typeck/hm/builtin.rs` 里被登记成一元箭头的内建。
const UNARY: [&str; 5] = ["str", "int", "float", "bool", "atom"];

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d327_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        WorkDir(d)
    }
    fn run(&self, src: &str) -> (i32, String) {
        let f = self.0.join("t.mora");
        std::fs::write(&f, src).expect("写脚本");
        let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
        let out = Command::new(exe)
            .current_dir(&self.0)
            .arg(&f)
            .output()
            .expect("跑 mora");
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push('\n');
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), s)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **缺参必须报错，且报错要指名是哪个内建**。
#[test]
fn d327_zero_arg_unary_builtins_are_rejected() {
    let wd = WorkDir::new("zero");
    for name in UNARY {
        let (code, out) = wd.run(&format!("print({name}())\n"));
        assert_ne!(
            code, 0,
            "`{name}()` 缺参必须非零退出（修前静默当成 nil 继续算）; out={out}"
        );
        assert!(
            out.contains(&format!("{name}() requires 1 argument")),
            "`{name}()` 的报错必须说清「缺 1 个参数」; out={out}"
        );
        assert!(
            !out.contains("does not accept nil"),
            "`{name}()` 的报错不得说「不接受 nil」—— 真实原因是**没给参数**，\
             那个说法会把排查方向带到「你的 nil 哪来的」上; out={out}"
        );
    }
}

/// **正路径一条都不能被误伤**（收紧下限最容易犯的错）。
#[test]
fn d327_unary_builtins_with_one_arg_still_work() {
    let wd = WorkDir::new("one");
    for (expr, expect) in [
        ("str(45)", "45"),
        ("int(\"42\")", "42"),
        ("float(\"1.5\")", "1.5"),
        ("bool(0)", "false"),
        ("atom(7)", "atom"),
    ] {
        let (code, out) = wd.run(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{expr}` 应成功; out={out}");
        assert!(
            out.contains(expect),
            "`{expr}` 的结果应含 `{expect}`; out={out}"
        );
    }
}

/// **显式传 nil 仍报「不接受 nil」** —— 与「缺参」是**两个不同的诊断**，
/// 不该被合并。
#[test]
fn d327_explicit_nil_keeps_its_own_diagnostic() {
    let wd = WorkDir::new("nil");
    for name in ["int", "float"] {
        let (code, out) = wd.run(&format!("print({name}(nil))\n"));
        assert_ne!(code, 0, "`{name}(nil)` 应报错; out={out}");
        assert!(
            out.contains(&format!("{name}() does not accept nil")),
            "`{name}(nil)` 是**给了个不能用的值**，不是缺参数; out={out}"
        );
        assert!(
            !out.contains("requires 1 argument"),
            "`{name}(nil)` 不该被报成缺参数; out={out}"
        );
    }
}

/// **对照组：多余实参仍然被拒，但那是 typeck 干的、不是本条干的。**
///
/// ⚠ 判据演进记录：这一条一开始写成「多传实参应仍被放行」，跑出来是红的。
/// 量过之后确认**前提错了**：`signature_no_over_tightening.rs` 管的是
/// **模块方法**，那些签名全是 fresh TypeVar ⇒ typeck 无从检查元数，
/// 于是运行期「忽略多余实参」成了必须守住的约定。
///
/// 而 `str` / `bool` / `atom` / `int` / `float` **不在那一类**：
/// 它们在 `hm/builtin.rs` 登记的是**柯里化一元箭头**，多传的实参会被
/// 变成偏应用（`fn (float) -> …`）再与返回类型 `String` / `Bool` 不符
/// ⇒ **编译期就被拒**（`Type mismatch: expected string, got fn (float) -> …`）。
///
/// 所以本条的对照组真正要证明的是两件事：
/// ① 收紧**下限**没有波及**上限**（多传仍被拒，且拒绝点在 typeck 而非本条）；
/// ② 本条的 `require_1` 本身**只查下限**（下方源码层断言逐字钉住）。
#[test]
fn d327_extra_args_still_rejected_by_typeck_not_by_this_change() {
    let wd = WorkDir::new("extra");
    for expr in ["str(1,2,3)", "bool(1,2)", "atom(1,2,3)", "int(1,2)"] {
        let (code, out) = wd.run(&format!("print({expr})\n"));
        assert_ne!(code, 0, "`{expr}` 多传实参应被拒; out={out}");
        // 拒绝点是 **typeck**（柯里化箭头 → 偏应用与返回类型不符），
        // 不是运行期的 `require_1`。用报错形状区分这两层。
        assert!(
            out.contains("Type error") || out.contains("Type mismatch"),
            "`{expr}` 应由 typeck 拒（柯里化箭头），而不是运行期 arity 检查; out={out}"
        );
        assert!(
            !out.contains("requires 1 argument"),
            "`{expr}` 有实参，不该触发「缺 1 个参数」; out={out}"
        );
    }
}

/// **源码层**：五个入口必须走 `require_1`，不得再有 `unwrap_or(Value::Nil)`。
///
/// 顺带钉住一件事：`require_1` 只查**下限**（`args.first()`），
/// 不是 `args.len() == 1` —— 后者会把「多传实参」也拒掉。
#[test]
fn d327_unary_entrypoints_use_require_1() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/builtin_impls.rs"
    ))
    .expect("读 builtin_impls.rs");

    for name in UNARY {
        let sig = format!("fn call_builtin_{name}(");
        let at = src.find(&sig).unwrap_or_else(|| panic!("找不到 `{sig}`"));
        // 按大括号配对取函数体
        let after = &src[at..];
        let mut depth = 0i32;
        let mut end = after.len();
        for (off, ch) in after.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = off + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &after[..end];
        assert!(
            !body.contains("unwrap_or(Value::Nil)"),
            "`call_builtin_{name}` 里仍有 `unwrap_or(Value::Nil)` —— \
             缺参会静默当成 nil。函数体：\n{body}"
        );
        assert!(
            body.contains(&format!("require_1(\"{name}\"")),
            "`call_builtin_{name}` 应走 `require_1(\"{name}\", &args)`; 函数体：\n{body}"
        );
    }

    // require_1 只收紧下限
    let helper_at = src.find("fn require_1(").expect("应有 require_1 辅助函数");
    let helper = &src[helper_at..helper_at + 400];
    assert!(
        helper.contains("args.first()"),
        "`require_1` 应只查 `args.first()`（下限）"
    );
    assert!(
        !helper.contains("args.len() == 1"),
        "`require_1` 不得写成 `args.len() == 1` —— 那会连**多余实参**一起拒，\
         违反 `signature_no_over_tightening` 定下的「只能收紧下限」"
    );
}
