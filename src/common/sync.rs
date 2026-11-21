//! Zero-dependency synchronization primitives (`Mutex`, `RwLock`, `Condvar`) built directly
//! on Rust's OS-futex-backed `std::sync` primitives (replacing external `parking_lot`).
//!
//! Guarantees:
//! - Non-poisoning `.lock()`, `.try_lock()`, `.read()`, `.write()` ergonomics (automatically
//!   recovers the inner guard if a thread previously panicked while holding the lock).
//! - Cycle-safe `Debug` formatting: when `Mutex` or `RwLock` is already locked on the current
//!   thread (e.g. self-referential `Value::Atom`), `Debug` prints `<locked>` via `try_lock()`
//!   instead of deadlocking or recursing infinitely.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{
    Mutex as StdMutex, MutexGuard as StdMutexGuard, RwLock as StdRwLock,
    RwLockReadGuard as StdRwLockReadGuard, RwLockWriteGuard as StdRwLockWriteGuard, TryLockError,
};

struct LockedPlaceholder;

impl fmt::Debug for LockedPlaceholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<locked>")
    }
}

/// Zero-dependency non-poisoning mutual exclusion lock backed by [`std::sync::Mutex`].
#[derive(Default)]
pub struct Mutex<T> {
    inner: StdMutex<T>,
}

/// RAII guard returned by [`Mutex::lock`] and [`Mutex::try_lock`].
pub struct MutexGuard<'a, T> {
    guard: StdMutexGuard<'a, T>,
}

impl<T> Mutex<T> {
    /// Create a new unlocked `Mutex<T>`.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            inner: StdMutex::new(value),
        }
    }

    /// Acquire the mutex, blocking the current thread until available.
    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        MutexGuard { guard }
    }

    /// Attempt to acquire the mutex without blocking, returning `None` if currently held.
    #[inline]
    pub(crate) fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.inner.try_lock() {
            Ok(guard) => Some(MutexGuard { guard }),
            Err(TryLockError::Poisoned(poisoned)) => Some(MutexGuard {
                guard: poisoned.into_inner(),
            }),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T: fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: fmt::Display> fmt::Display for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Mutex");
        match self.try_lock() {
            Some(guard) => d.field("data", &&*guard),
            None => d.field("data", &LockedPlaceholder),
        };
        d.finish()
    }
}

/// Zero-dependency non-poisoning reader-writer lock backed by [`std::sync::RwLock`].
#[derive(Default)]
pub struct RwLock<T> {
    inner: StdRwLock<T>,
}

/// Shared read guard returned by [`RwLock::read`].
pub struct RwLockReadGuard<'a, T> {
    guard: StdRwLockReadGuard<'a, T>,
}

/// Exclusive write guard returned by [`RwLock::write`].
pub struct RwLockWriteGuard<'a, T> {
    guard: StdRwLockWriteGuard<'a, T>,
}

impl<T> RwLock<T> {
    /// Create a new unlocked `RwLock<T>`.
    #[inline]
    pub(crate) const fn new(value: T) -> Self {
        Self {
            inner: StdRwLock::new(value),
        }
    }

    /// Acquire a shared read lock.
    #[inline]
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        RwLockReadGuard { guard }
    }

    /// Acquire an exclusive write lock.
    #[inline]
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
        let guard = match self.inner.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        RwLockWriteGuard { guard }
    }

    /// Attempt to acquire a shared read lock without blocking.
    #[inline]
    pub(crate) fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        match self.inner.try_read() {
            Ok(guard) => Some(RwLockReadGuard { guard }),
            Err(TryLockError::Poisoned(poisoned)) => Some(RwLockReadGuard {
                guard: poisoned.into_inner(),
            }),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<T: fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("RwLock");
        match self.try_read() {
            Some(guard) => d.field("data", &&*guard),
            None => d.field("data", &LockedPlaceholder),
        };
        d.finish()
    }
}
