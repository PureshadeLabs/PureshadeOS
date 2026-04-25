//! Synchronisation primitives for cask userspace.
//!
//! All locks are spinlock-based — cask tasks are single-threaded processes
//! that don't share memory, so there is no OS-backed blocking needed.
//!
//! Re-exports `Arc` and `Weak` from `alloc::sync`.

pub use alloc::sync::{Arc, Weak};

use core::{
    cell::UnsafeCell,
    fmt,
    hint,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

// ── Mutex<T> ──────────────────────────────────────────────────────────────────

/// A mutual-exclusion spinlock.
///
/// Unlike `std::sync::Mutex`, this never poisons on panic — it simply unlocks
/// when the guard is dropped.
pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    data:   UnsafeCell<T>,
}

/// Guard returned by `Mutex::lock`. Releases the lock on drop.
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}
// MutexGuard is !Send (guard must be released on the same task).
unsafe impl<T: Sync> Sync for MutexGuard<'_, T> {}

impl<T> Mutex<T> {
    /// Create a new unlocked `Mutex` wrapping `data`.
    pub const fn new(data: T) -> Self {
        Mutex { locked: AtomicBool::new(false), data: UnsafeCell::new(data) }
    }

    /// Consume the mutex and return the inner value.
    pub fn into_inner(self) -> T { self.data.into_inner() }
}

impl<T: ?Sized> Mutex<T> {
    /// Spin until the lock is acquired, then return a guard.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if self.locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return MutexGuard { mutex: self };
            }
            while self.locked.load(Ordering::Relaxed) {
                hint::spin_loop();
            }
        }
    }

    /// Try to acquire the lock without blocking. Returns `None` if contended.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard { mutex: self })
    }

    /// Returns `true` if the mutex is currently locked.
    pub fn is_locked(&self) -> bool { self.locked.load(Ordering::Relaxed) }

    /// Get a mutable reference to the inner value (requires exclusive access).
    pub fn get_mut(&mut self) -> &mut T { self.data.get_mut() }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self { Mutex::new(T::default()) }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(g) => f.debug_struct("Mutex").field("data", &&*g).finish(),
            None    => f.write_str("Mutex(<locked>)"),
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { unsafe { &*self.mutex.data.get() } }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T { unsafe { &mut *self.mutex.data.get() } }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) { self.mutex.locked.store(false, Ordering::Release); }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

// ── RwLock<T> ─────────────────────────────────────────────────────────────────
//
// State is a single AtomicU32:
//   bits [30:0] = active reader count
//   bit  [31]   = writer lock held
//
// Writers wait for all readers to leave; readers wait for the writer bit.

/// A reader-writer spinlock.
///
/// Multiple simultaneous readers are allowed; a writer gets exclusive access.
pub struct RwLock<T: ?Sized> {
    state: AtomicU32,
    data:  UnsafeCell<T>,
}

pub struct RwLockReadGuard<'a, T: ?Sized>  { lock: &'a RwLock<T> }
pub struct RwLockWriteGuard<'a, T: ?Sized> { lock: &'a RwLock<T> }

const WRITER_BIT: u32 = 1 << 31;

unsafe impl<T: Send + Sync> Send for RwLock<T> {}
unsafe impl<T: Send + Sync> Sync for RwLock<T> {}
unsafe impl<T: Sync> Sync for RwLockReadGuard<'_, T> {}

impl<T> RwLock<T> {
    pub const fn new(data: T) -> Self {
        RwLock { state: AtomicU32::new(0), data: UnsafeCell::new(data) }
    }
    pub fn into_inner(self) -> T { self.data.into_inner() }
}

impl<T: ?Sized> RwLock<T> {
    /// Acquire a shared read lock (spins while a writer is active).
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        loop {
            let s = self.state.load(Ordering::Relaxed);
            if s & WRITER_BIT == 0 {
                if self.state
                    .compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return RwLockReadGuard { lock: self };
                }
            }
            hint::spin_loop();
        }
    }

    /// Try to acquire a read lock without blocking.
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        let s = self.state.load(Ordering::Relaxed);
        if s & WRITER_BIT != 0 { return None; }
        self.state
            .compare_exchange(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| RwLockReadGuard { lock: self })
    }

    /// Acquire an exclusive write lock (spins until all readers have left).
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        loop {
            if self.state
                .compare_exchange_weak(0, WRITER_BIT, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return RwLockWriteGuard { lock: self };
            }
            hint::spin_loop();
        }
    }

    /// Try to acquire a write lock without blocking.
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.state
            .compare_exchange(0, WRITER_BIT, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| RwLockWriteGuard { lock: self })
    }

    pub fn get_mut(&mut self) -> &mut T { self.data.get_mut() }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self { RwLock::new(T::default()) }
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { unsafe { &*self.lock.data.get() } }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) { self.lock.state.fetch_sub(1, Ordering::Release); }
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { unsafe { &*self.lock.data.get() } }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T { unsafe { &mut *self.lock.data.get() } }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) { self.lock.state.store(0, Ordering::Release); }
}

// ── OnceLock<T> ───────────────────────────────────────────────────────────────

/// A cell which can be written exactly once and then read freely.
///
/// Equivalent to `std::sync::OnceLock`.
pub struct OnceLock<T> {
    inited: AtomicBool,
    data:   UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send>        Send for OnceLock<T> {}
unsafe impl<T: Send + Sync> Sync for OnceLock<T> {}

impl<T> OnceLock<T> {
    /// Create an uninitialised `OnceLock`.
    pub const fn new() -> Self {
        OnceLock { inited: AtomicBool::new(false), data: UnsafeCell::new(MaybeUninit::uninit()) }
    }

    /// Get the value if already initialised.
    pub fn get(&self) -> Option<&T> {
        if self.inited.load(Ordering::Acquire) {
            Some(unsafe { (*self.data.get()).assume_init_ref() })
        } else {
            None
        }
    }

    /// Get or initialise: calls `f` at most once, stores the result.
    pub fn get_or_init<F: FnOnce() -> T>(&self, f: F) -> &T {
        if !self.inited.load(Ordering::Acquire) {
            unsafe { (*self.data.get()).write(f()) };
            self.inited.store(true, Ordering::Release);
        }
        unsafe { (*self.data.get()).assume_init_ref() }
    }

    /// Try to set the value. Returns `Err(val)` if already set.
    pub fn set(&self, val: T) -> core::result::Result<(), T> {
        if self.inited.load(Ordering::Acquire) { return Err(val); }
        unsafe { (*self.data.get()).write(val) };
        self.inited.store(true, Ordering::Release);
        Ok(())
    }

    /// Consume the lock and return the inner value, if set.
    pub fn into_inner(mut self) -> Option<T> {
        if *self.inited.get_mut() {
            Some(unsafe { self.data.get_mut().assume_init_read() })
        } else {
            None
        }
    }
}

impl<T> Default for OnceLock<T> {
    fn default() -> Self { Self::new() }
}

impl<T: fmt::Debug> fmt::Debug for OnceLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Some(v) => write!(f, "OnceLock({:?})", v),
            None    => f.write_str("OnceLock(<uninit>)"),
        }
    }
}

impl<T> Drop for OnceLock<T> {
    fn drop(&mut self) {
        if *self.inited.get_mut() {
            unsafe { self.data.get_mut().assume_init_drop() };
        }
    }
}
