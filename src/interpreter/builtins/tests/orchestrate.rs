//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v044_orchestrate_validate {
    use crate::parser_v3::ParserV3;

    /// v0.44.0 / v0.92: orchestrate block syntax validation (witness 路径).
    /// 旧 `parse()`（MirExpr）已迁移到 `compile()`（MirWitness）。
    fn compile_ok(src: &str) -> bool {
        ParserV3::compile(src)
            .map(|(_func, witnesses)| !witnesses.is_empty())
            .unwrap_or_else(|e| panic!("ParserV3::compile failed: {}", e))
    }

    #[test]
    fn orchestrate_sequential_parses() {
        let src = r#"
orchestrate sequential x -> y
  agent a(x) => "a:" + x
  agent b(x) => "b:" + x
end
"#;
        assert!(compile_ok(src));
    }

    #[test]
    fn orchestrate_loop_with_on_predicate_parses() {
        let src = r#"
orchestrate loop x -> y, max_rounds: 5
  on: x == "done"
  agent a(x) => x
end
"#;
        assert!(compile_ok(src));
    }

    #[test]
    fn orchestrate_graph_with_predicate_edges_parses() {
        let src = r#"
orchestrate graph x -> y
  @start -> a
  @start -> b on: x == "research"
  a -> @exit
  b -> @exit
end
"#;
        assert!(compile_ok(src));
    }
}
