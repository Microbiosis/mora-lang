//! 交错搜索（interleaving search）—— 回溯作为引擎数据流。
//!
//! 引擎是显式状态机：FIFO 任务队列，每个任务是「待执行目标链 + 替换
//! 快照」。合取推进压队首（继续当前逻辑链）；析取与关系子句备选压队尾
//! （与其他待定链交错轮转）。这给出跨分支公平交错：无限左分支不会饿死
//! 右侧备选（miniKanren interleave 的完备性目标，以队列轮转实现）。
//!
//! 回溯无需「撤销」：每个备选分支持有独立替换快照（HAMT O(1) 结构共享）。
//!
//! 引擎不依赖 VM：关系子句解析与投影宿主函数执行经 [`RelHost`] 回调注入，
//! 由解释器层实现（Foundation 层零反向依赖）。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::value::Value;

use super::goal::{Clause, Goal, ProjectFn, Term};
use super::subst::Subst;
use super::unify::unify;

/// 搜索期宿主回调（解释器层实现）。
pub trait RelHost {
    /// 按名解析关系子句（子句体中递归 `Invoke` 无自含子句时调用）。
    fn relation_clauses(&mut self, name: &str) -> Result<Arc<Vec<Clause>>, String>;

    /// 运行投影宿主函数（确定性求值；效果按搜索路径重放）。
    /// `func` 为具名引用或已求值可调用值，宿主负责解析具名引用。
    fn run_project(&mut self, func: &ProjectFn, args: Vec<Value>) -> Result<Value, String>;
}

/// 搜索状态机。
pub struct Search {
    queue: VecDeque<(VecDeque<Goal>, Subst)>,
    next_var: u64,
}

impl Search {
    /// 从初始目标与「下一个可用逻辑变量 id」构造搜索。
    /// `next_var` 应大于 solve 查询变量已占用的 id 区间。
    pub fn new(goal: Goal, next_var: u64) -> Search {
        let mut queue = VecDeque::new();
        queue.push_back((VecDeque::from([goal]), Subst::new()));
        Search { queue, next_var }
    }

    /// 引擎当前变量分配水位（诊断用）。
    pub fn next_var(&self) -> u64 {
        self.next_var
    }

    /// 取下一个解；队列耗尽返回 `None`。
    pub fn next_solution(&mut self, host: &mut dyn RelHost) -> Result<Option<Subst>, String> {
        while let Some((mut chain, s)) = self.queue.pop_front() {
            let Some(goal) = chain.pop_front() else {
                return Ok(Some(s)); // 链耗尽 = 一个解
            };
            match goal {
                Goal::Succeed => self.queue.push_front((chain, s)),
                Goal::Fail => {}
                Goal::Unify(a, b) => {
                    let av = expect_val(&a)?.clone();
                    let bv = expect_val(&b)?.clone();
                    if let Some(s2) = unify(&av, &bv, &s) {
                        self.queue.push_front((chain, s2));
                    }
                }
                Goal::Conj(gs) => {
                    for g in gs.into_iter().rev() {
                        chain.push_front(g);
                    }
                    self.queue.push_front((chain, s));
                }
                Goal::Disj(gs) => {
                    for g in gs {
                        let mut alt = VecDeque::new();
                        alt.push_back(g);
                        alt.extend(chain.iter().cloned());
                        self.queue.push_back((alt, s.clone()));
                    }
                }
                Goal::Invoke { name, clauses, args } => {
                    self.invoke(host, &name, clauses, &args, chain, s)?;
                }
                Goal::Project { func, args, result } => {
                    self.project(host, func, &args, &result, chain, s)?;
                }
            }
        }
        Ok(None)
    }

    fn invoke(
        &mut self,
        host: &mut dyn RelHost,
        name: &str,
        clauses: Option<Arc<Vec<Clause>>>,
        args: &[Term],
        chain: VecDeque<Goal>,
        s: Subst,
    ) -> Result<(), String> {
        let cls: Arc<Vec<Clause>> = match clauses {
            Some(c) => c,
            None => host.relation_clauses(name)?,
        };
        let mut arg_vals = Vec::with_capacity(args.len());
        for a in args {
            arg_vals.push(expect_val(a)?.clone());
        }
        for clause in cls.iter() {
            if clause.head.len() != arg_vals.len() {
                return Err(format!(
                    "关系 {} 调用实参数量不匹配：期望 {}，得到 {}",
                    name,
                    clause.head.len(),
                    arg_vals.len()
                ));
            }
            let mut map = HashMap::new();
            let head: Vec<Value> = clause
                .head
                .iter()
                .map(|t| rename_term(t, &mut map, &mut self.next_var))
                .collect();
            let body = rename_goal(&clause.body, &mut map, &mut self.next_var);
            let mut s2 = s.clone();
            let mut ok = true;
            for (a, h) in arg_vals.iter().zip(head.iter()) {
                match unify(a, h, &s2) {
                    Some(n) => s2 = n,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                let mut alt = VecDeque::new();
                alt.push_back(body);
                alt.extend(chain.iter().cloned());
                self.queue.push_back((alt, s2));
            }
        }
        Ok(())
    }

    fn project(
        &mut self,
        host: &mut dyn RelHost,
        func: ProjectFn,
        args: &[Term],
        result: &Term,
        chain: VecDeque<Goal>,
        s: Subst,
    ) -> Result<(), String> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let v = s.walk(expect_val(a)?);
            if s.has_unbound(&v) {
                return Err(
                    "project 实参含未绑定逻辑变量 —— 投影要求完全绑定的实参（可先用 unify 绑定）"
                        .to_string(),
                );
            }
            vals.push(v);
        }
        let out = host.run_project(&func, vals)?;
        let rv = expect_val(result)?.clone();
        if let Some(s2) = unify(&rv, &out, &s) {
            self.queue.push_front((chain, s2));
        }
        Ok(())
    }
}

/// 运行期形态检查：搜索只消费 rename 后的目标（项全为 `Val`）。
fn expect_val(t: &Term) -> Result<&Value, String> {
    match t {
        Term::Val(v) => Ok(v),
        _ => Err("内部错误：编译期子句未 rename 就进入搜索（Param/构造形态残留）".to_string()),
    }
}

/// 子句参数槽 → fresh 逻辑变量 id（同一子句调用内同名槽映射一致）。
fn fresh_param(i: usize, map: &mut HashMap<usize, u64>, next: &mut u64) -> u64 {
    if let Some(id) = map.get(&i) {
        return *id;
    }
    let id = *next;
    *next += 1;
    map.insert(i, id);
    id
}

/// 把编译期项模板展开为运行期 Value 树（Param → LogicVar）。
pub fn rename_term(t: &Term, map: &mut HashMap<usize, u64>, next: &mut u64) -> Value {
    match t {
        Term::Val(v) => v.clone(),
        Term::Param(i) => Value::LogicVar(fresh_param(*i, map, next)),
        Term::Cons(a, b) => Value::Cons {
            car: Box::new(rename_term(a, map, next)),
            cdr: Box::new(rename_term(b, map, next)),
        },
        Term::List(xs) => Value::List(xs.iter().map(|x| rename_term(x, map, next)).collect()),
        Term::Dict(entries) => Value::Dict(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), rename_term(v, map, next)))
                .collect(),
        ),
    }
}

/// 把编译期目标树整体展开为运行期目标（所有项变为 `Val`）。
pub fn rename_goal(g: &Goal, map: &mut HashMap<usize, u64>, next: &mut u64) -> Goal {
    match g {
        Goal::Succeed => Goal::Succeed,
        Goal::Fail => Goal::Fail,
        Goal::Unify(a, b) => Goal::Unify(
            Term::Val(rename_term(a, map, next)),
            Term::Val(rename_term(b, map, next)),
        ),
        Goal::Conj(gs) => Goal::Conj(gs.iter().map(|g| rename_goal(g, map, next)).collect()),
        Goal::Disj(gs) => Goal::Disj(gs.iter().map(|g| rename_goal(g, map, next)).collect()),
        Goal::Invoke { name, clauses, args } => Goal::Invoke {
            name: name.clone(),
            clauses: clauses.clone(),
            args: args.iter().map(|t| Term::Val(rename_term(t, map, next))).collect(),
        },
        Goal::Project { func, args, result } => Goal::Project {
            func: func.clone(),
            args: args.iter().map(|t| Term::Val(rename_term(t, map, next))).collect(),
            result: Term::Val(rename_term(result, map, next)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试宿主：关系表 + 投影函数走本地闭包。
    struct TestHost {
        rels: HashMap<String, Arc<Vec<Clause>>>,
    }

    impl TestHost {
        fn with(name: &str, clauses: Vec<Clause>) -> TestHost {
            let mut rels = HashMap::new();
            rels.insert(name.to_string(), Arc::new(clauses));
            TestHost { rels }
        }
    }

    impl RelHost for TestHost {
        fn relation_clauses(&mut self, name: &str) -> Result<Arc<Vec<Clause>>, String> {
            self.rels
                .get(name)
                .cloned()
                .ok_or_else(|| format!("未定义关系 {}", name))
        }
        fn run_project(&mut self, func: &ProjectFn, args: Vec<Value>) -> Result<Value, String> {
            // 测试宿主只支持「返回绑定的常量」形态
            match func {
                ProjectFn::Value(Value::String(out)) => {
                    assert!(args.is_empty());
                    Ok(Value::String(out.clone()))
                }
                ProjectFn::Name(n) => Err(format!("TestHost 未解析具名投影 {}", n)),
                _ => Err("TestHost 不支持该投影函数".into()),
            }
        }
    }

    fn ground(v: Value) -> Term {
        Term::Val(v)
    }

    fn p(i: usize) -> Term {
        Term::Param(i)
    }

    fn collect(search: &mut Search, host: &mut dyn RelHost, cap: usize) -> Vec<Subst> {
        let mut out = Vec::new();
        while out.len() < cap {
            match search.next_solution(host).expect("search") {
                Some(s) => out.push(s),
                None => break,
            }
        }
        out
    }

    #[test]
    fn fact_query_binds_var() {
        // rel edge("a", "b") / rel edge("a", "c")
        let host = &mut TestHost::with(
            "edge",
            vec![
                Clause::fact(vec![], vec![ground(Value::String("a".into())), ground(Value::String("b".into()))]),
                Clause::fact(vec![], vec![ground(Value::String("a".into())), ground(Value::String("c".into()))]),
            ],
        );
        // solve { edge("a", ?x) }
        let goal = Goal::Invoke {
            name: "edge".into(),
            clauses: None,
            args: vec![
                ground(Value::String("a".into())),
                ground(Value::LogicVar(0)),
            ],
        };
        let mut search = Search::new(goal, 1);
        let sols = collect(&mut search, host, 10);
        assert_eq!(sols.len(), 2);
        let x: Vec<Value> = sols.iter().map(|s| s.walk(&Value::LogicVar(0))).collect();
        assert!(x.contains(&Value::String("b".into())));
        assert!(x.contains(&Value::String("c".into())));
    }

    #[test]
    fn conjunction_threads_bindings() {
        // rel e("a","b") / rel f("b")
        // solve { e(?x, ?y), f(?y) }  → ?x="a", ?y="b"
        let mut host = TestHost::with(
            "e",
            vec![Clause::fact(
                vec![],
                vec![ground(Value::String("a".into())), ground(Value::String("b".into()))],
            )],
        );
        host.rels.insert(
            "f".into(),
            Arc::new(vec![Clause::fact(vec![], vec![ground(Value::String("b".into()))])]),
        );
        let goal = Goal::Conj(vec![
            Goal::Invoke {
                name: "e".into(),
                clauses: None,
                args: vec![ground(Value::LogicVar(0)), ground(Value::LogicVar(1))],
            },
            Goal::Invoke {
                name: "f".into(),
                clauses: None,
                args: vec![ground(Value::LogicVar(1))],
            },
        ]);
        let mut search = Search::new(goal, 2);
        let sols = collect(&mut search, &mut host, 10);
        assert_eq!(sols.len(), 1);
        let s = &sols[0];
        assert_eq!(s.walk(&Value::LogicVar(0)), Value::String("a".into()));
        assert_eq!(s.walk(&Value::LogicVar(1)), Value::String("b".into()));
    }

    #[test]
    fn disjunction_enumerates_alternatives() {
        // solve { succeed, fail, succeed } → 2 个解
        let host = &mut TestHost::with("e", vec![]);
        let goal = Goal::Disj(vec![Goal::Succeed, Goal::Fail, Goal::Succeed]);
        let mut search = Search::new(goal, 0);
        assert_eq!(collect(&mut search, host, 10).len(), 2);
    }

    #[test]
    fn recursive_relation_transitive_closure() {
        // rel path(x,y) e(x,y) end
        // rel path(x,z) e(x,y), path(y,z) end
        // e: a→b, b→c
        let e = vec![
            Clause::fact(vec![], vec![ground(Value::String("a".into())), ground(Value::String("b".into()))]),
            Clause::fact(vec![], vec![ground(Value::String("b".into())), ground(Value::String("c".into()))]),
        ];
        let path = vec![
            Clause::rule(
                vec!["x".into(), "y".into()],
                vec![p(0), p(1)],
                Goal::Invoke { name: "e".into(), clauses: None, args: vec![p(0), p(1)] },
            ),
            Clause::rule(
                vec!["x".into(), "z".into()],
                vec![p(0), p(1)],
                Goal::Conj(vec![
                    Goal::Invoke { name: "e".into(), clauses: None, args: vec![p(0), p(2)] },
                    Goal::Invoke { name: "path".into(), clauses: None, args: vec![p(2), p(1)] },
                ]),
            ),
        ];
        let mut host = TestHost::with("path", path);
        host.rels.insert("e".into(), Arc::new(e));
        // solve { path(?x, "c") }
        let goal = Goal::Invoke {
            name: "path".into(),
            clauses: None,
            args: vec![ground(Value::LogicVar(0)), ground(Value::String("c".into()))],
        };
        let mut search = Search::new(goal, 1);
        let sols = collect(&mut search, &mut host, 10);
        let xs: Vec<Value> = sols.iter().map(|s| s.walk(&Value::LogicVar(0))).collect();
        assert_eq!(xs.len(), 2);
        assert!(xs.contains(&Value::String("a".into())));
        assert!(xs.contains(&Value::String("b".into())));
    }

    #[test]
    fn clause_rename_isolates_invocations() {
        // rel dbl(x, y) unify(x, y) end
        // 同名查询变量跨两次调用：?a 先绑 1，再要求 2=?a → 无解（rename 后
        // 两次调用的参数是不同的 fresh 变量，绑定只经共享的 ?a 传播）
        let dbl = vec![Clause::rule(
            vec!["x".into(), "y".into()],
            vec![p(0), p(1)],
            Goal::Unify(p(0), p(1)),
        )];
        let host = &mut TestHost::with("dbl", dbl);
        let goal = Goal::Conj(vec![
            Goal::Invoke {
                name: "dbl".into(),
                clauses: None,
                args: vec![ground(Value::Int(1)), ground(Value::LogicVar(0))],
            },
            Goal::Invoke {
                name: "dbl".into(),
                clauses: None,
                args: vec![ground(Value::Int(2)), ground(Value::LogicVar(0))],
            },
        ]);
        let mut search = Search::new(goal, 2);
        assert!(collect(&mut search, host, 10).is_empty());

        // 对照：不同变量各自独立 → 恰一个解（?a=1, ?b=2）
        let goal = Goal::Conj(vec![
            Goal::Invoke {
                name: "dbl".into(),
                clauses: None,
                args: vec![ground(Value::Int(1)), ground(Value::LogicVar(0))],
            },
            Goal::Invoke {
                name: "dbl".into(),
                clauses: None,
                args: vec![ground(Value::Int(2)), ground(Value::LogicVar(1))],
            },
        ]);
        let mut search = Search::new(goal, 2);
        let sols = collect(&mut search, host, 10);
        assert_eq!(sols.len(), 1);
        assert_eq!(sols[0].walk(&Value::LogicVar(0)), Value::Int(1));
        assert_eq!(sols[0].walk(&Value::LogicVar(1)), Value::Int(2));
    }

    #[test]
    fn interleave_fair_across_infinite_left_branch() {
        // rel any(x) succeed end
        // rel any(x) any(x) end   -- 无限左递归
        // solve 3 { any(?x) } → 公平交错下右侧备选不被饿死，得 3 个解
        let any = vec![
            Clause::rule(
                vec!["x".into()],
                vec![p(0)],
                Goal::Succeed,
            ),
            Clause::rule(
                vec!["x".into()],
                vec![p(0)],
                Goal::Invoke { name: "any".into(), clauses: None, args: vec![p(0)] },
            ),
        ];
        let host = &mut TestHost::with("any", any);
        let goal = Goal::Invoke {
            name: "any".into(),
            clauses: None,
            args: vec![ground(Value::LogicVar(0))],
        };
        let mut search = Search::new(goal, 1);
        let sols = collect(&mut search, host, 3);
        assert_eq!(sols.len(), 3, "交错调度必须让 succeed 备选持续产出解");
    }

    #[test]
    fn project_runs_host_and_unifies_result() {
        // rel const_src() succeed end — 用 Project 统一结果
        let host = &mut TestHost::with("e", vec![]);
        let goal = Goal::Project {
            func: ProjectFn::Value(Value::String("hello".into())),
            args: vec![],
            result: ground(Value::LogicVar(0)),
        };
        let mut search = Search::new(goal, 1);
        let sols = collect(&mut search, host, 10);
        assert_eq!(sols.len(), 1);
        assert_eq!(sols[0].walk(&Value::LogicVar(0)), Value::String("hello".into()));
    }

    #[test]
    fn project_with_unbound_arg_is_error() {
        let host = &mut TestHost::with("e", vec![]);
        let goal = Goal::Project {
            func: ProjectFn::Value(Value::String("hello".into())),
            args: vec![ground(Value::LogicVar(0))],
            result: ground(Value::LogicVar(1)),
        };
        let mut search = Search::new(goal, 2);
        assert!(search.next_solution(host).is_err());
    }

    #[test]
    fn self_referential_unify_fails_via_occur_check() {
        // rel cyc(x) unify(x, cons(x, nil)) end → solve { cyc(?x) } 无解
        let cyc = vec![Clause::rule(
            vec!["x".into()],
            vec![p(0)],
            Goal::Unify(
                p(0),
                Term::Cons(Box::new(p(0)), Box::new(Term::Val(Value::Nil))),
            ),
        )];
        let host = &mut TestHost::with("cyc", cyc);
        let goal = Goal::Invoke {
            name: "cyc".into(),
            clauses: None,
            args: vec![ground(Value::LogicVar(0))],
        };
        let mut search = Search::new(goal, 1);
        assert!(collect(&mut search, host, 10).is_empty());
    }

    #[test]
    fn cons_head_template_unifies_cons_chains() {
        // rel first(cons(h, t), h) end → solve { first(cons(7, nil), ?x) } → 7
        let first = vec![Clause::rule(
            vec!["h".into(), "t".into()],
            vec![Term::Cons(Box::new(p(0)), Box::new(p(1))), p(0)],
            Goal::Succeed,
        )];
        let host = &mut TestHost::with("first", first);
        let goal = Goal::Invoke {
            name: "first".into(),
            clauses: None,
            args: vec![
                ground(Value::Cons {
                    car: Box::new(Value::Int(7)),
                    cdr: Box::new(Value::Nil),
                }),
                ground(Value::LogicVar(0)),
            ],
        };
        let mut search = Search::new(goal, 1);
        let sols = collect(&mut search, host, 10);
        assert_eq!(sols.len(), 1);
        assert_eq!(sols[0].walk(&Value::LogicVar(0)), Value::Int(7));
    }

    #[test]
    fn arity_mismatch_is_error() {
        let host = &mut TestHost::with(
            "e",
            vec![Clause::fact(vec![], vec![ground(Value::Int(1))])],
        );
        let goal = Goal::Invoke {
            name: "e".into(),
            clauses: None,
            args: vec![ground(Value::Int(1)), ground(Value::Int(2))],
        };
        let mut search = Search::new(goal, 0);
        assert!(search.next_solution(host).is_err());
    }

    #[test]
    fn unknown_relation_is_error() {
        let host = &mut TestHost::with("e", vec![]);
        let goal = Goal::Invoke {
            name: "nope".into(),
            clauses: None,
            args: vec![],
        };
        let mut search = Search::new(goal, 0);
        assert!(search.next_solution(host).is_err());
    }

    #[test]
    fn embedded_clauses_skip_host_lookup() {
        // 自含子句的 Invoke 不查宿主
        let host = &mut TestHost::with("e", vec![]);
        let clauses = Arc::new(vec![Clause::fact(
            vec!["x".into()],
            vec![p(0)],
        )]);
        let goal = Goal::Invoke {
            name: "embedded".into(),
            clauses: Some(clauses),
            args: vec![ground(Value::LogicVar(0))],
        };
        let mut search = Search::new(goal, 1);
        let sols = collect(&mut search, host, 10);
        assert_eq!(sols.len(), 1);
        assert!(matches!(sols[0].walk(&Value::LogicVar(0)), Value::LogicVar(_)));
    }
}
