//! P6-1: Perceus / Functional-But-In-Place (FBIP) `Arc`-backed copy-on-write dictionary (`Value::Dict`).
//!
//! Architectural guarantees:
//! - Cloning a [`Dict`] is **$O(1)$** (single `Arc::clone` instead of $O(n)$ deep `HashMap` copy).
//! - When uniquely owned (`Arc::strong_count == 1`, statically unlocked at last-use reads via
//!   Perceus `MoveLocal` / `MoveGlobal` / `inst_kill_masks`), [`Dict::set_owned`],
//!   [`Dict::remove_owned`], and [`Dict::insert_mut`] mutate the underlying `HashMap` **in place
//!   in $O(1)$ amortized time**, turning functional immutable builder chains (`McpServer.tool(...)`,
//!   `Router.get(...)`, `d.set("a", 1).set("b", 2)`) from $O(n^2)$ into $O(n)$ with 100% pure
//!   value semantics.
//! - When shared (`Arc::strong_count > 1`), mutations perform a lazy single Copy-on-Write clone,
//!   preserving referential transparency across aliases, closures, channels, and continuations.

use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use crate::value::Value;

/// Immutable-by-semantics, `Arc`-shared dictionary with $O(1)$ FBIP in-place mutation when unique.
#[derive(Clone, Default)]
pub struct Dict(Arc<HashMap<String, Value>>);

impl Dict {
    /// Functional `.set(key, value)` consuming `self`: mutates in place in $O(1)$ when uniquely
    /// owned (`reused = true`), or clones once on write when shared (`reused = false`).
    #[inline]
    pub fn set_owned(mut self, key: String, value: Value) -> (Self, bool) {
        if let Some(inner) = Arc::get_mut(&mut self.0) {
            inner.insert(key, value);
            (self, true)
        } else {
            Arc::make_mut(&mut self.0).insert(key, value);
            (self, false)
        }
    }

    /// Functional `.remove(key)` consuming `self`: mutates in place in $O(1)$ when uniquely owned
    /// (`reused = true`), or clones once on write when shared (`reused = false`).
    #[inline]
    pub fn remove_owned(mut self, key: &str) -> (Self, bool) {
        if let Some(inner) = Arc::get_mut(&mut self.0) {
            inner.remove(key);
            (self, true)
        } else {
            Arc::make_mut(&mut self.0).remove(key);
            (self, false)
        }
    }

    /// Mutate `&mut self` (e.g. `IndexAssign` `d[k] = v`), reusing in place when unique (`true`)
    /// or cloning on write when shared (`false`).
    #[inline]
    pub fn insert_mut(&mut self, key: String, value: Value) -> bool {
        if let Some(inner) = Arc::get_mut(&mut self.0) {
            inner.insert(key, value);
            true
        } else {
            Arc::make_mut(&mut self.0).insert(key, value);
            false
        }
    }

    /// Extract an owned `HashMap<String, Value>`, unwrapping zero-copy when unique or cloning when shared.
    #[inline]
    pub fn into_hashmap(self) -> HashMap<String, Value> {
        Arc::unwrap_or_clone(self.0)
    }

    /// Obtain a mutable reference to the underlying `HashMap` via Copy-on-Write (`Arc::make_mut`).
    #[inline]
    pub fn make_mut(&mut self) -> &mut HashMap<String, Value> {
        Arc::make_mut(&mut self.0)
    }

    /// Destructively extract `key` in $O(1)$ **only** when this [`Dict`] handle uniquely owns its
    /// underlying `HashMap` (`Arc::strong_count == 1`), returning `None` if shared or absent.
    #[inline]
    pub(crate) fn remove_if_unique(&mut self, key: &str) -> Option<Value> {
        Arc::get_mut(&mut self.0).and_then(|inner| inner.remove(key))
    }

    pub(crate) fn get_mut_if_unique(&mut self, key: &str) -> Option<&mut Value> {
        Arc::get_mut(&mut self.0)?.get_mut(key)
    }
}

impl Deref for Dict {
    type Target = HashMap<String, Value>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<HashMap<String, Value>> for Dict {
    #[inline]
    fn from(map: HashMap<String, Value>) -> Self {
        Self(Arc::new(map))
    }
}

impl<const N: usize> From<[(String, Value); N]> for Dict {
    #[inline]
    fn from(arr: [(String, Value); N]) -> Self {
        Self(Arc::new(HashMap::from(arr)))
    }
}

impl From<Dict> for HashMap<String, Value> {
    #[inline]
    fn from(d: Dict) -> Self {
        d.into_hashmap()
    }
}

impl FromIterator<(String, Value)> for Dict {
    #[inline]
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Self(Arc::new(iter.into_iter().collect()))
    }
}

impl IntoIterator for Dict {
    type Item = (String, Value);
    type IntoIter = std::collections::hash_map::IntoIter<String, Value>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.0).into_iter()
    }
}

impl<'a> IntoIterator for &'a Dict {
    type Item = (&'a String, &'a Value);
    type IntoIter = std::collections::hash_map::Iter<'a, String, Value>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl PartialEq for Dict {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl PartialEq<HashMap<String, Value>> for Dict {
    #[inline]
    fn eq(&self, other: &HashMap<String, Value>) -> bool {
        &*self.0 == other
    }
}

impl fmt::Debug for Dict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}
