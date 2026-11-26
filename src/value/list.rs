use crate::value::Value;
use std::sync::Arc as Rc;

/// 不可变列表：克隆 **O(1)**，追加 **O(n/32) 但只复制指针**。
///
/// ## 解决的是什么问题
///
/// `Value::List(Vec<Value>.into())` 是**深值语义**：`clone()` 逐个 `Value` 复制。
/// 累积器写法 `while i < n { let xs = xs.push(i) }` 每轮要复制整个列表 → O(n²)。
/// 实测 n=20000 需 **44 秒**。
///
/// 本类型把列表切成 32 个元素一片的**不可变块**，块用 `Rc` 共享：
///
/// - `push` 只克隆**块指针数组**（n/32 个引用计数加一）+ **最后一个块**
///   （32 个 `Value`），**从不复制既有元素**；
/// - `get(i)` = `chunks[i/32][i%32]`，O(1)；
/// - `clone()` 整个列表 = n/32 次引用计数加一。
///
/// 实测收益见文件末的 `bench_push_accumulation`。
///
/// ## 与「真正的 O(1) 持久化向量」的差距（如实记录）
///
/// 本实现是 **O(n/32) 追加**，不是 O(1)。要真正做到 O(1) 追加 + O(log₃₂ n)
/// 索引，需要 B 叉 trie **且树高增长时既有元素一个都不搬家** —— 后者极难：
/// 纯 digit 方案（`第 k 层用 (i >> 5k) % 32`）一加层，所有元素的 digit
/// 解释整体偏移，等于全体搬家，结构共享直接失效。Clojure 的 PersistentVector
/// 靠「下标 0 特判进 root 槽 + `arrayFor(i) = (i>>5)+1` 只依赖绝对下标 +
/// 长高时旧根放新根 slot 0 + tail 单独拿出」四点配合才成立。
///
/// 本项目已试过三版 trie（纯 digit / 顶层恒 0 / 深度可变），均因上述 digit
/// 漂移而索引错乱（症状：2 元素列表 `get(1)` 返回 `None`）。不再重复第四次。
///
/// **但对本仓库的实际热点，当前实现已经够了**：`push` 的成本从「复制 n 个
/// `Value`」降到「复制 n/32 个指针 + 32 个 `Value`」，且不碰既有元素。
///
/// ## 实测（`cargo test --release --lib value::list -- --nocapture`）
///
/// ```text
/// PUSH n=  2000 chunked=    1.43ms  |  deep Vec n=2000 =   135.63ms
/// PUSH n=  8000 chunked=    5.92ms  |  deep Vec n=4000 =   206.28ms
/// PUSH n= 20000 chunked=   20.33ms  |  deep Vec n=4000 =   122.60ms
/// ```
///
/// n=20000 建表 **20.33ms**。深值 `Vec` 在 n=4000 就要 120–200ms，二次增长
/// 外推到 n=20000 是数秒 —— 而真实 Mora 程序
/// `while i < n { let xs = xs.push(i) }` 还要**每轮 3 次**这种复制
/// （`Environment::get` / 方法接收者 / `ControlInst::StoreLocal` 写回），实测 n=20000 需 **44 秒**。
///
/// 即：本类型把累积建表那一段从「数十秒」压到「几十毫秒」。
#[derive(Clone)]
struct ListInner {
    chunks: Vec<Rc<[Value; 32]>>,
    count: usize,
}

#[derive(Clone)]
pub struct List {
    inner: Rc<ListInner>,
}

const B: usize = 32;

impl Default for List {
    fn default() -> Self {
        Self::new()
    }
}

impl List {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(ListInner {
                chunks: Vec::new(),
                count: 0,
            }),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.count == 0
    }

    /// 从 `Vec` 构造：一次全量分块，之后所有 push 都是 O(1)。
    pub fn from_vec(v: Vec<Value>) -> Self {
        let count = v.len();
        let mut chunks: Vec<Rc<[Value; 32]>> = Vec::with_capacity(count / B + 1);
        let mut cur: [Value; 32] = std::array::from_fn(|_| Value::Nil);
        let mut n = 0usize;
        for x in v {
            cur[n] = x;
            n += 1;
            if n == B {
                chunks.push(Rc::new(cur));
                cur = std::array::from_fn(|_| Value::Nil);
                n = 0;
            }
        }
        if n > 0 {
            chunks.push(Rc::new(cur));
        }
        Self {
            inner: Rc::new(ListInner { chunks, count }),
        }
    }

    /// 追加一个元素，返回**新**列表；原列表不变。
    pub fn push(&self, value: Value) -> Self {
        let mut chunks = self.inner.chunks.clone();
        let new_count = self.inner.count + 1;
        let ci = self.inner.count / B;
        let off = self.inner.count % B;
        if off == 0 {
            // 新开一片
            let mut fresh: [Value; B] = std::array::from_fn(|_| Value::Nil);
            fresh[0] = value;
            chunks.push(Rc::new(fresh));
        } else {
            // 复制**当前最后一片**（32 个元素），不动其它片
            let mut fresh: [Value; B] = (*chunks[ci]).clone();
            fresh[off] = value;
            let n = chunks.len();
            chunks[n - 1] = Rc::new(fresh);
        }
        Self {
            inner: Rc::new(ListInner {
                chunks,
                count: new_count,
            }),
        }
    }

    /// P6-1 Perceus/FBIP: 追加元素并消费所有权 —— 当列表脊柱唯一持有（`Arc::strong_count == 1`）时
    /// **原地 O(1) 摊还推入**（若尾片也唯一持有则零拷贝原地写槽），返回 `(new_list, reused)`。
    #[inline]
    pub fn push_owned(mut self, value: Value) -> (Self, bool) {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            let ci = inner.count / B;
            let off = inner.count % B;
            if off == 0 {
                let mut fresh: [Value; B] = std::array::from_fn(|_| Value::Nil);
                fresh[0] = value;
                inner.chunks.push(Rc::new(fresh));
            } else if let Some(tail) = Rc::get_mut(&mut inner.chunks[ci]) {
                tail[off] = value;
            } else {
                let mut fresh: [Value; B] = (*inner.chunks[ci]).clone();
                fresh[off] = value;
                inner.chunks[ci] = Rc::new(fresh);
            }
            inner.count += 1;
            (self, true)
        } else {
            (self.push(value), false)
        }
    }

    /// 按绝对下标取元素 —— O(1)。
    #[inline]
    pub fn get(&self, i: usize) -> Option<&Value> {
        if i >= self.inner.count {
            return None;
        }
        self.inner.chunks.get(i / B).and_then(|c| c.get(i % B))
    }

    /// 解构只可移动独占脊柱上独占块中的元素。
    pub(crate) fn get_mut_if_unique(&mut self, i: usize) -> Option<&mut Value> {
        let inner = Rc::get_mut(&mut self.inner)?;
        if i >= inner.count {
            return None;
        }
        Rc::get_mut(&mut inner.chunks[i / B]).map(|chunk| &mut chunk[i % B])
    }

    pub fn iter(&self) -> ListIter<'_> {
        ListIter {
            list: self,
            front: 0,
            back: self.inner.count,
        }
    }

    pub fn to_vec(&self) -> Vec<Value> {
        self.iter().cloned().collect()
    }

    pub fn first(&self) -> Option<&Value> {
        self.get(0)
    }

    pub fn last(&self) -> Option<&Value> {
        if self.inner.count == 0 {
            None
        } else {
            self.get(self.inner.count - 1)
        }
    }

    pub fn contains(&self, v: &Value) -> bool {
        self.iter().any(|x| x == v)
    }

    pub fn slice(&self, start: usize, end: usize) -> List {
        let end = end.min(self.inner.count);
        let start = start.min(end);
        let mut out = List::new();
        for i in start..end {
            if let Some(v) = self.get(i) {
                out = out.push_owned(v.clone()).0;
            }
        }
        out
    }

    // ── 为「接管 `Value::List`」补的兼容 API ──────────────────────────
    // 目标：让 `Value::List(items) => items.iter()/len()/…` 这类**模式匹配点
    // 零改动**，把迁移量从「逐处重写」压到「构造点加 `.into()`」。

    /// 降级出口：需要真正的 `Vec` 手术（`drain`/`sort`/`retain`…）时用。
    ///
    /// 本类型不可变，这类操作必须显式「取回一份可改的 Vec」——调用点要么
    /// `let mut v = l.to_vec(); …; Value::List(v.into())`，要么把结果整体
    /// 重新装回。语义是明确的（值语义），只是不再原地。
    pub fn to_mut_vec(&self) -> Vec<Value> {
        self.to_vec()
    }

    /// 排序（不可变 → 返回新列表）。
    pub fn sorted_by(&self, cmp: impl FnMut(&Value, &Value) -> std::cmp::Ordering) -> List {
        let mut v = self.to_vec();
        v.sort_by(cmp);
        List::from_vec(v)
    }

    pub fn sorted_by_key<K: Ord, F: FnMut(&Value) -> K>(&self, mut f: F) -> List {
        let mut v = self.to_vec();
        v.sort_by_key(|x| f(x));
        List::from_vec(v)
    }

    pub fn reversed(&self) -> List {
        let mut v = self.to_vec();
        v.reverse();
        List::from_vec(v)
    }

    pub fn dedup(&self) -> List {
        let mut v = self.to_vec();
        v.dedup();
        List::from_vec(v)
    }

    pub fn retain(&self, keep: impl FnMut(&Value) -> bool) -> List {
        let mut v = self.to_vec();
        v.retain(keep);
        List::from_vec(v)
    }

    pub fn extend(&self, other: impl IntoIterator<Item = Value>) -> List {
        let mut v = self.to_vec();
        v.extend(other);
        List::from_vec(v)
    }

    /// P6-1 Perceus/FBIP: 拼接两个列表并消费 `self` 所有权 —— 当 `self` 唯一持有（`Arc::strong_count == 1`）时
    /// 原地扩展块脊柱与尾块（若 `self` 恰好块对齐则直接 O(chunks) 共享追加 `other` 的块指针），返回 `(merged, reused)`。
    pub fn extend_owned(mut self, other: List) -> (Self, bool) {
        if other.is_empty() {
            return (self, false);
        }
        if self.is_empty() {
            return (other, false);
        }
        let reused = Rc::get_mut(&mut self.inner).is_some();
        let inner = Rc::make_mut(&mut self.inner);
        if inner.count.is_multiple_of(B) {
            let rhs = Rc::unwrap_or_clone(other.inner);
            inner.count += rhs.count;
            inner.chunks.extend(rhs.chunks);
        } else {
            for value in other {
                self = self.push_owned(value).0;
            }
        }
        (self, reused)
    }

    /// 取出末元素（不改列表）。对应 `Vec::pop` 的取值部分。
    pub fn pop_last(&self) -> Option<Value> {
        if self.inner.count == 0 {
            None
        } else {
            self.get(self.inner.count - 1).cloned()
        }
    }

    /// 定长分窗（`Vec::chunks`）。不可变实现返回物化结果。
    pub fn windows(&self, n: usize) -> Vec<Vec<Value>> {
        if n == 0 {
            return vec![];
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.inner.count {
            let end = (i + n).min(self.inner.count);
            out.push(self.to_vec()[i..end].to_vec());
            i = end;
        }
        out
    }

    pub fn with_capacity(_n: usize) -> List {
        List::new()
    }

    /// **不可变**按下标写入，返回新列表（原列表不变）。
    pub fn set(&self, i: usize, value: Value) -> List {
        let mut chunks = self.inner.chunks.clone();
        let ci = i / B;
        let off = i % B;
        if let Some(chunk) = chunks.get_mut(ci) {
            let mut fresh: [Value; B] = (**chunk).clone();
            fresh[off] = value;
            *chunk = std::sync::Arc::new(fresh);
        }
        Self {
            inner: Rc::new(ListInner {
                chunks,
                count: self.inner.count,
            }),
        }
    }

    /// P6-1 Perceus/FBIP: 按下标写入并消费所有权 —— 唯一持有时原地 O(1) 写回。
    #[inline]
    pub fn set_owned(mut self, i: usize, value: Value) -> (Self, bool) {
        if let Some(inner) = Rc::get_mut(&mut self.inner) {
            let ci = i / B;
            let off = i % B;
            if let Some(chunk) = inner.chunks.get_mut(ci) {
                if let Some(arr) = Rc::get_mut(chunk) {
                    arr[off] = value;
                } else {
                    let mut fresh: [Value; B] = (**chunk).clone();
                    fresh[off] = value;
                    *chunk = Rc::new(fresh);
                }
            }
            (self, true)
        } else {
            (self.set(i, value), false)
        }
    }

    /// 稳定身份 —— 用外层 `Arc<ListInner>` 地址标识列表实例（原地复用时保持同一 `id()`）。
    #[inline]
    pub fn id(&self) -> usize {
        Rc::as_ptr(&self.inner) as usize
    }

    pub fn any(&self, p: impl FnMut(&Value) -> bool) -> bool {
        self.iter().any(p)
    }

    pub fn all(&self, p: impl FnMut(&Value) -> bool) -> bool {
        self.iter().all(p)
    }

    pub fn position(&self, p: impl FnMut(&Value) -> bool) -> Option<usize> {
        self.iter().position(p)
    }

    pub fn filter(&self, mut p: impl FnMut(&Value) -> bool) -> List {
        List::from_vec(self.iter().filter(|x| p(x)).cloned().collect())
    }

    pub fn map<U: Clone, F: FnMut(&Value) -> U>(&self, f: F) -> Vec<U> {
        self.iter().map(f).collect()
    }
}

/// `v[i]` —— 必须写在 `impl List` **外面**（嵌套 impl 非法）。
impl std::ops::Index<usize> for List {
    type Output = Value;
    fn index(&self, i: usize) -> &Value {
        self.get(i).expect("List 索引越界")
    }
}

pub struct ListIter<'a> {
    list: &'a List,
    /// 前向游标（已消费的前端）。
    front: usize,
    /// 后向游标（**未**消费的尾端，即半开区间的右界）。
    back: usize,
}

impl<'a> Iterator for ListIter<'a> {
    type Item = &'a Value;
    fn next(&mut self) -> Option<&'a Value> {
        if self.front >= self.back {
            return None;
        }
        let v = self.list.get(self.front)?;
        self.front += 1;
        Some(v)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.back.saturating_sub(self.front);
        (n, Some(n))
    }
}

/// 逆序迭代 —— 两端各持一个游标，`next()` 与 `next_back()` 互不吞元素。
///
/// **两端必须各有各的游标**：早先我用单个 `idx` 同时充当前后端，于是
/// `next()` 与 `next_back()` 会互相吞掉元素 —— 混用两端时结果错乱且不报错。
/// 这类「自己写的迭代器语义错」比编译错误危险得多，务必分开。
impl<'a> DoubleEndedIterator for ListIter<'a> {
    fn next_back(&mut self) -> Option<&'a Value> {
        if self.front >= self.back {
            return None;
        }
        self.back -= 1;
        self.list.get(self.back)
    }
}

impl ExactSizeIterator for ListIter<'_> {}

impl<'a> IntoIterator for &'a List {
    type Item = &'a Value;
    type IntoIter = ListIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// 按值迭代（`for x in list` / `list.into_iter()`）。
///
/// 无此前 `for page in pl` 这类**按值**循环直接编译不过（`List` 不是
/// iterator），是迁移里 9 处 E0277 的来源。
impl IntoIterator for List {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        match Rc::try_unwrap(self.inner) {
            Ok(inner) => {
                let mut out = Vec::with_capacity(inner.count);
                let mut rem = inner.count;
                for chunk in inner.chunks {
                    let take = rem.min(B);
                    match Rc::try_unwrap(chunk) {
                        Ok(arr) => out.extend(arr.into_iter().take(take)),
                        Err(rc) => out.extend(rc.iter().take(take).cloned()),
                    }
                    rem -= take;
                    if rem == 0 {
                        break;
                    }
                }
                out.into_iter()
            }
            Err(rc) => {
                let mut out = Vec::with_capacity(rc.count);
                for ci in 0..rc.chunks.len() {
                    for off in 0..B {
                        let idx = ci * B + off;
                        if idx >= rc.count {
                            break;
                        }
                        out.push(rc.chunks[ci][off].clone());
                    }
                }
                out.into_iter()
            }
        }
    }
}

impl From<Vec<Value>> for List {
    fn from(v: Vec<Value>) -> Self {
        List::from_vec(v)
    }
}

impl From<List> for Vec<Value> {
    fn from(l: List) -> Self {
        l.to_vec()
    }
}

impl FromIterator<Value> for List {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        let v: Vec<Value> = iter.into_iter().collect();
        List::from_vec(v)
    }
}

impl PartialEq for List {
    fn eq(&self, other: &Self) -> bool {
        if Rc::ptr_eq(&self.inner, &other.inner) {
            return true;
        }
        if self.inner.count != other.inner.count {
            return false;
        }
        (0..self.inner.count).all(|i| self.get(i) == other.get(i))
    }
}

impl std::fmt::Debug for List {
    /// 按**元素**打印（`[1, 2, 3]`），而不是派生的内部结构。
    ///
    /// derive 会打出 `List { chunks: [[1, 2, Nil, …]], count: 3 }` —— 把
    /// 块大小（32）和未用槽位的 `Nil` 全泄进输出。任何比较 `Value` 的
    /// `Debug` 文本的地方（测试断言、错误信息）都会被这堆内部细节污染。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries((0..self.inner.count).filter_map(|i| self.get(i)))
            .finish()
    }
}

impl std::fmt::Display for List {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;
        for (i, v) in self.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{v}")?;
        }
        write!(f, "]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(n: usize) -> Vec<Value> {
        (0..n).map(|i| Value::Int(i as i64)).collect()
    }

    #[test]
    fn empty_list() {
        let l = List::new();
        assert_eq!(l.len(), 0);
        assert!(l.is_empty());
        assert_eq!(l.get(0), None);
        assert_eq!(l.to_vec(), vec![]);
    }

    #[test]
    fn one_element() {
        let l = List::new().push(Value::Int(7));
        assert_eq!(l.len(), 1);
        assert_eq!(l.get(0), Some(&Value::Int(7)));
        assert_eq!(l.to_vec(), vec![Value::Int(7)]);
    }

    #[test]
    fn push_is_persistent() {
        let a = List::from_vec(vals(3));
        let b = a.push(Value::Int(99));
        assert_eq!(a.len(), 3);
        assert_eq!(a.get(3), None, "原列表不可变");
        assert_eq!(b.len(), 4);
        assert_eq!(b.get(3), Some(&Value::Int(99)));
    }

    #[test]
    fn exhaustive_roundtrip() {
        // 覆盖块边界（32 的倍数）两侧
        for n in [1usize, 2, 31, 32, 33, 63, 64, 65, 95, 96, 97, 1000, 2000] {
            let mut l = List::new();
            for i in 0..n {
                l = l.push(Value::Int(i as i64));
                assert_eq!(l.len(), i + 1, "n={n} 第 {i} 次 push 后 len");
                assert_eq!(
                    l.get(i),
                    Some(&Value::Int(i as i64)),
                    "n={n} push {i} 后 get({i})"
                );
            }
            assert_eq!(l.to_vec(), vals(n), "n={n} 全量回读");
        }
    }

    #[test]
    fn from_vec_matches_push_chain() {
        for n in [0usize, 1, 31, 32, 33, 100, 1000] {
            let a = List::from_vec(vals(n));
            let mut b = List::new();
            for v in vals(n) {
                b = b.push(v);
            }
            assert_eq!(a.len(), b.len(), "n={n} len");
            assert_eq!(a.to_vec(), b.to_vec(), "n={n} 内容");
        }
    }

    #[test]
    fn from_vec_handles_exact_multiple_of_chunk() {
        for n in [32usize, 64, 96] {
            let l = List::from_vec(vals(n));
            assert_eq!(l.len(), n, "n={n}");
            assert_eq!(l.to_vec(), vals(n), "n={n}");
        }
    }

    #[test]
    fn iter_first_last() {
        let l = List::from_vec(vals(100));
        assert_eq!(l.iter().count(), 100);
        assert_eq!(l.first(), Some(&Value::Int(0)));
        assert_eq!(l.last(), Some(&Value::Int(99)));
        assert_eq!(l.get(100), None);
    }

    #[test]
    fn eq_is_by_content() {
        assert_eq!(List::from_vec(vals(70)), List::from_vec(vals(70)));
        assert_ne!(List::from_vec(vals(70)), List::from_vec(vals(69)));
    }

    #[test]
    fn slice_and_contains() {
        let l = List::from_vec(vals(10));
        assert_eq!(l.slice(2, 5).to_vec(), vals(10)[2..5].to_vec());
        assert_eq!(l.slice(5, 5).len(), 0);
        assert_eq!(l.slice(8, 100).len(), 2, "end 越界应被 clamp");
        assert!(l.contains(&Value::Int(7)));
        assert!(!l.contains(&Value::Int(70)));
    }

    #[test]
    fn conversions() {
        let l: List = vals(37).into_iter().collect();
        assert_eq!(l.len(), 37);
        let back: Vec<Value> = l.into();
        assert_eq!(back, vals(37));
    }

    #[test]
    fn display() {
        let l = List::from_vec(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(format!("{l}"), "[1, 2]");
        assert_eq!(format!("{}", List::new()), "[]");
    }

    /// 累积建表的量测：本类型 vs 深值 `Vec<Value>`。
    #[test]
    fn bench_push_accumulation() {
        use std::time::Instant;

        for n in [2_000usize, 8_000, 20_000] {
            let t = Instant::now();
            let mut l = List::new();
            for i in 0..n {
                l = l.push(Value::Int(i as i64));
            }
            let chunked = t.elapsed();

            // 深值基线（n=20000 要几十秒，故只跑小的 n）
            let small = n.min(4_000);
            let t2 = Instant::now();
            let mut v: Vec<Value> = Vec::new();
            for i in 0..small {
                let mut next = v.clone();
                next.push(Value::Int(i as i64));
                v = next;
            }
            let deep = t2.elapsed();

            eprintln!(
                "PUSH n={n:>6} chunked={:>8.2}ms  |  deep Vec n={small} = {:>8.2}ms",
                chunked.as_secs_f64() * 1e3,
                deep.as_secs_f64() * 1e3
            );
            assert_eq!(l.len(), n);
        }
    }
}
