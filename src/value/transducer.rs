//! v0.83: Clojure-style transducers — 流式管道的底层原语。
//!
//! 设计：transducer 是「reducing-fn transformer」——
//! `(ReducingFn<A,R>) -> ReducingFn<B,R>`，
//! composition = 普通函数 composition。
//!
//! 与 Iterator 的区别：transducer 是**推送式**（push-based），
//! 每个元素主动推入 `step`，而非被动拉取。这匹配 SSE 流的推送语义。
//!
//! 参考：Clojure `clojure.core/transduce` + `clojure.core/comp`。

use std::marker::PhantomData;

/// Transducer 核心 trait — 推送式流变换器。
///
/// `A` = 输入元素类型，`B` = 输出元素类型。
/// `step` 返回 `Some(B)` 表示产出元素，`None` 表示终止流。
/// `complete` 在流结束时调用（flush 内部状态）。
///
/// `Send + Sync + Debug` bounds 让 transducer 可存于 `Value::Stream`（要求 Send+Sync+Debug）
/// 并跨线程传递（用于 Pregel BSP worker 池）。
pub trait Transducer<A: Clone + Send + Sync, B: Clone + Send + Sync>: Send + Sync + std::fmt::Debug {
    /// 处理一个输入元素，返回 `Some(output)` 或 `None`（终止）。
    fn step(&mut self, next: A) -> Option<B>;
    /// 流结束时调用（默认 no-op）。
    fn complete(&mut self) {}
}

/// Map transducer — 对每个元素应用函数 `f: A -> B`。
pub struct Map<A, B, F: FnMut(A) -> B>(pub F, pub PhantomData<(A, B)>);

impl<A, B, F: FnMut(A) -> B> std::fmt::Debug for Map<A, B, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Map(<fn>)")
    }
}

impl<A: Clone + Send + Sync, B: Clone + Send + Sync, F: FnMut(A) -> B + Send + Sync>
    Transducer<A, B> for Map<A, B, F>
{
    fn step(&mut self, next: A) -> Option<B> {
        Some((self.0)(next))
    }
}

/// Filter transducer — 只让满足谓词的元素通过。
pub struct Filter<A, F: FnMut(&A) -> bool>(pub F, pub PhantomData<A>);

impl<A, F: FnMut(&A) -> bool> std::fmt::Debug for Filter<A, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Filter(<fn>)")
    }
}

impl<A: Clone + Send + Sync, F: FnMut(&A) -> bool + Send + Sync> Transducer<A, A>
    for Filter<A, F>
{
    fn step(&mut self, next: A) -> Option<A> {
        if (self.0)(&next) { Some(next) } else { None }
    }
}

/// Take transducer — 只取前 N 个元素，之后终止流。
pub struct Take<A>(pub usize, pub PhantomData<A>);

impl<A> std::fmt::Debug for Take<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Take({})", self.0)
    }
}

impl<A: Clone + Send + Sync> Transducer<A, A> for Take<A> {
    fn step(&mut self, next: A) -> Option<A> {
        if self.0 > 0 {
            self.0 -= 1;
            Some(next)
        } else {
            None
        }
    }
}

/// Comp transducer — 顺序组合两个 transducer。
/// `(comp xf1 xf2)(arg)` == `xf1(xf2(arg))`（先 xf2 后 xf1，与 Clojure 一致）。
pub struct Comp<A, B, C, X1, X2>
where
    A: Clone + Send + Sync,
    B: Clone + Send + Sync,
    C: Clone + Send + Sync,
    X1: Transducer<B, C>,
    X2: Transducer<A, B>,
{
    pub first: X2,   // 先应用（内层）
    pub second: X1,  // 后应用（外层）
    pub _phantom: PhantomData<(A, B, C)>,
}

impl<A, B, C, X1, X2> std::fmt::Debug for Comp<A, B, C, X1, X2>
where
    A: Clone + Send + Sync,
    B: Clone + Send + Sync,
    C: Clone + Send + Sync,
    X1: Transducer<B, C>,
    X2: Transducer<A, B>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Comp({:?}, {:?})", self.second, self.first)
    }
}

impl<A, B, C, X1, X2> Comp<A, B, C, X1, X2>
where
    A: Clone + Send + Sync,
    B: Clone + Send + Sync,
    C: Clone + Send + Sync,
    X1: Transducer<B, C>,
    X2: Transducer<A, B>,
{
    pub fn new(first: X2, second: X1) -> Self {
        Comp {
            first,
            second,
            _phantom: PhantomData,
        }
    }
}

impl<A, B, C, X1, X2> Transducer<A, C> for Comp<A, B, C, X1, X2>
where
    A: Clone + Send + Sync,
    B: Clone + Send + Sync,
    C: Clone + Send + Sync,
    X1: Transducer<B, C>,
    X2: Transducer<A, B>,
{
    fn step(&mut self, next: A) -> Option<C> {
        // 先过内层（first），再过外层（second）
        let intermediate = self.first.step(next)?;
        self.second.step(intermediate)
    }

    fn complete(&mut self) {
        self.first.complete();
        self.second.complete();
    }
}

/// 便捷构造：Map transducer。
pub fn map<A: Clone + Send + Sync, B: Clone + Send + Sync, F: FnMut(A) -> B + Send + Sync>(
    f: F,
) -> Map<A, B, F> {
    Map(f, PhantomData)
}

/// 便捷构造：Filter transducer。
pub fn filter<A: Clone + Send + Sync, F: FnMut(&A) -> bool + Send + Sync>(
    f: F,
) -> Filter<A, F> {
    Filter(f, PhantomData)
}

/// 便捷构造：Take transducer。
pub fn take<A: Clone + Send>(n: usize) -> Take<A> {
    Take(n, PhantomData)
}

/// 便捷构造：Comp transducer（先 first 后 second）。
pub fn comp<A, B, C, X1, X2>(first: X2, second: X1) -> Comp<A, B, C, X1, X2>
where
    A: Clone + Send + Sync,
    B: Clone + Send + Sync,
    C: Clone + Send + Sync,
    X1: Transducer<B, C> + Send + Sync,
    X2: Transducer<A, B> + Send + Sync,
{
    Comp::new(first, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_doubles_values() {
        let mut m = map(|x: i32| x * 2);
        assert_eq!(m.step(1), Some(2));
        assert_eq!(m.step(5), Some(10));
    }

    #[test]
    fn filter_passes_matching() {
        let mut f = filter(|x: &i32| *x > 3);
        assert_eq!(f.step(1), None);
        assert_eq!(f.step(5), Some(5));
    }

    #[test]
    fn take_stops_after_n() {
        let mut t = take(2);
        assert_eq!(t.step(1), Some(1));
        assert_eq!(t.step(2), Some(2));
        assert_eq!(t.step(3), None);
    }

    #[test]
    fn comp_map_then_filter() {
        // comp(filter(>5), map(*2)) — 先 map 后 filter
        let mut c = comp(
            map(|x: i32| x * 2),
            filter(|x: &i32| *x > 5),
        );
        assert_eq!(c.step(1), None); // 1*2=2, 2>5=false
        assert_eq!(c.step(3), Some(6)); // 3*2=6, 6>5=true
    }

    #[test]
    fn comp_filter_then_map() {
        // comp(map(*2), filter(>5)) — 先 filter 后 map
        let mut c = comp(
            filter(|x: &i32| *x > 5),
            map(|x: i32| x * 2),
        );
        assert_eq!(c.step(3), None); // 3>5=false
        assert_eq!(c.step(6), Some(12)); // 6>5=true, 6*2=12
    }

    #[test]
    fn take_zero_immediately_terminates() {
        let mut t: Take<i32> = take(0);
        assert_eq!(t.step(1), None);
    }

    #[test]
    fn map_string_transform() {
        let mut m = map(|s: String| s.to_uppercase());
        assert_eq!(m.step("hello".to_string()), Some("HELLO".to_string()));
    }

    #[test]
    fn comp_three_chain() {
        // comp(comp(map(+1), filter(>2)), map(*10))
        let inner = comp(
            map(|x: i32| x + 1),
            filter(|x: &i32| *x > 2),
        );
        let mut outer = comp(inner, map(|x: i32| x * 10));
        assert_eq!(outer.step(1), None); // 1+1=2, 2>2=false
        assert_eq!(outer.step(2), Some(30)); // 2+1=3, 3>2=true, 3*10=30
    }
}