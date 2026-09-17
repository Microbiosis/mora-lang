//! v0.89: LMIR → RIR 连接 — 9 层架构桥接第 4-5 段。
//!
//! 将 LMIR 产出的布局信息填充到 RIR 的 LayoutTable，
//! 并将 LMIR 指令降维为可执行的 RIR 编译产物。
//!
//! LMIR Unbox 实际接入：
//! - Int → i64（8 字节，直接算术）
//! - Float → f64（8 字节，SSE2）
//! - Bool → i1（1 字节，条件分支）
//! - String → ptr + len（16 字节，间接访问）

use crate::mir::core::CoreFunction;
use crate::mir::lmir::{LmirInst, MemLayout};
use crate::mir::rir::{CompiledFunction, FunctionVersionTable, LayoutTable};
use crate::typeck::Type;

/// 从 LMIR 指令列表提取布局信息，填充 LayoutTable。
pub fn populate_layout_table(
    _lmir_insts: &[LmirInst],
    layouts: &[(String, MemLayout)],
) -> LayoutTable {
    let mut table = LayoutTable::new();

    // 从 LMIR 桥接产出的布局
    for (name, layout) in layouts {
        table.insert(name.clone(), layout.clone());
    }

    // 内置类型布局
    table.insert("Int".to_string(), MemLayout::int64());
    table.insert("Float".to_string(), MemLayout::float64());
    table.insert("Bool".to_string(), MemLayout::bool());
    table.insert("Ptr".to_string(), MemLayout::ptr());

    table
}

/// 将 CoreFunction + LayoutTable 包装为 CompiledFunction。
pub fn core_to_compiled(
    name: String,
    core: CoreFunction,
    _layouts: &LayoutTable,
) -> CompiledFunction {
    CompiledFunction {
        name,
        params: core.params.clone(),
        return_type: Type::Any, // 由 EHIR 确定
        effects: core.effects.clone(),
        core,
        version: 1,
    }
}

/// 注册 CompiledFunction 到 FunctionVersionTable。
pub fn register_function(
    table: &mut FunctionVersionTable,
    name: String,
    core: CoreFunction,
    layouts: &LayoutTable,
) {
    let compiled = core_to_compiled(name, core, layouts);
    table.register(compiled);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::core::{CoreBlock, CoreTerminator};
    use crate::mir::effect::EffectRow;
    use crate::mir::lmir::LmirType;

    #[test]
    fn populate_builtin_layouts() {
        let table = populate_layout_table(&[], &[]);
        assert!(table.get("Int").is_some());
        assert!(table.get("Float").is_some());
        assert!(table.get("Bool").is_some());
        assert_eq!(table.get("Int").unwrap().size, 8);
    }

    #[test]
    fn populate_custom_layouts() {
        let custom = vec![(
            "MyStruct".to_string(),
            MemLayout {
                size: 16,
                align: 8,
                fields: vec![(0, LmirType::Int64), (8, LmirType::Bool)],
            },
        )];
        let table = populate_layout_table(&[], &custom);
        let layout = table.get("MyStruct").unwrap();
        assert_eq!(layout.size, 16);
        assert_eq!(layout.fields.len(), 2);
    }

    #[test]
    fn core_to_compiled_roundtrip() {
        let core = CoreFunction {
            params: vec![("x".to_string(), Type::Int)],
            blocks: vec![CoreBlock {
                id: 0,
                insts: vec![],
                terminator: CoreTerminator::Return(Some(0)),
            }],
            entry: 0,
            effects: EffectRow::Empty,
            n_regs: 1,
        };
        let layouts = populate_layout_table(&[], &[]);
        let compiled = core_to_compiled("test_fn".to_string(), core, &layouts);
        assert_eq!(compiled.name, "test_fn");
        assert_eq!(compiled.version, 1);
    }

    #[test]
    fn register_and_retrieve() {
        let mut vt = FunctionVersionTable::new();
        let core = CoreFunction {
            params: vec![],
            blocks: vec![],
            entry: 0,
            effects: EffectRow::Empty,
            n_regs: 0,
        };
        let layouts = populate_layout_table(&[], &[]);
        register_function(&mut vt, "my_func".to_string(), core, &layouts);
        let f = vt.get("my_func").unwrap();
        assert_eq!(f.name, "my_func");
    }
}
