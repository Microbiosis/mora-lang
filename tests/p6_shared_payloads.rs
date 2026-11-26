//! P6-1：沿 VM 的实际调用与成功匹配分支，统计与测试字符串等长的物理分配。
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use mora::mir::control::{OptLevel, optimize_program};
use mora::parser_v3::ParserV3;
use mora::runtime::{CoreRuntime, vm::execute_program};
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

const PAYLOAD_BYTES: usize = 100_003;
thread_local! {
    static PAYLOAD_ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct PayloadAllocator;

// 与 System 使用相同的分配、释放契约，仅记录本测试线程的指定长度。
unsafe impl GlobalAlloc for PayloadAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() == PAYLOAD_BYTES {
            PAYLOAD_ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: PayloadAllocator = PayloadAllocator;

fn measure(template: &str, n: usize) -> usize {
    let source = template
        .replace("$TEXT", &"x".repeat(PAYLOAD_BYTES))
        .replace("$COUNT", &n.to_string());
    let (mut program, witnesses) = ParserV3::compile(&source).expect("编译共享读取测试");
    let errors = check_program_witnesses(&witnesses);
    assert!(errors.is_empty(), "{errors:?}");
    optimize_program(&mut program, OptLevel::Basic);
    let mut core = CoreRuntime::default();
    let program = Arc::new(program);
    PAYLOAD_ALLOCATIONS.with(|count| count.set(0));
    let (value, _) = execute_program(program, &mut core).expect("执行共享读取测试");
    let copies = PAYLOAD_ALLOCATIONS.with(Cell::get);
    assert_eq!(value, Value::Int(n as i64));
    copies
}

fn assert_reads_do_not_copy(source: &str) {
    // 两次运行的编译、构造与入口调用相同，只有共享读取的次数发生变化。
    let baseline = measure(source, 1);
    assert!(baseline > 0, "未观察到字符串的初始分配");
    assert_eq!(measure(source, 9), baseline, "额外的共享读取复制了完整载荷");
}

#[test]
fn result_tag_reads_do_not_copy_live_payload() {
    assert_reads_do_not_copy(
        r#"
task inspect(r: result<string, string>, n: int): int
  let hits = 0
  let i = 0
  while i < n
    assign hits = hits + int(r.is_ok() and not r.is_err())
    assign i = i + 1
  end
  return hits
end
inspect(Ok("$TEXT"), $COUNT)
"#,
    );
}

#[test]
fn inspecting_a_shared_enum_and_list_only_copies_successful_bindings() {
    assert_reads_do_not_copy(
        r#"
enum Packet
  Data(list<string>)
end
task inspect(p: Packet, n: int): int
  let hits = 0
  let i = 0
  while i < n
    let tag = match p with
      Packet::Data([unused, "0"]) -> 0
      Packet::Data([_, count]) -> count.len()
      _ -> -1
    end
    assign hits = hits + tag
    assign i = i + 1
  end
  return hits
end
inspect(Packet::Data(["$TEXT", "1"]), $COUNT)
"#,
    );
}

#[test]
fn wildcard_on_shared_enum_does_not_copy_payload() {
    assert_reads_do_not_copy(
        r#"
enum Packet
  Data(string)
end
task inspect(p: Packet, n: int): int
  let hits = 0
  let i = 0
  while i < n
    let tag = match p with
      Packet::Data(_) -> 1
      _ -> 0
    end
    assign hits = hits + tag
    assign i = i + 1
  end
  return hits
end
inspect(Packet::Data("$TEXT"), $COUNT)
"#,
    );
}
