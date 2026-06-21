use core::sync::atomic::{AtomicI32, Ordering};
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

// State encoding: 0 = unlocked, -1 = write-locked, N>0 = N readers.
const UNLOCKED: i32 = 0;
const WRITE_LOCKED: i32 = -1;

pub struct RwLock<T: ?Sized> {
    state: AtomicI32,
    data:  UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    pub const fn new(val: T) -> Self {
        RwLock { state: AtomicI32::new(UNLOCKED), data: UnsafeCell::new(val) }
    }
    pub fn into_inner(self) -> T { self.data.into_inner() }
}

impl<T: ?Sized> RwLock<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        loop {
            let s = self.state.load(Ordering::Relaxed);
            if s >= 0 {
                if self.state.compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    return RwLockReadGuard { lock: self };
                }
            }
            crate::sys::thread_impl::yield_now();
        }
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        loop {
            if self.state
                .compare_exchange_weak(UNLOCKED, WRITE_LOCKED, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return RwLockWriteGuard { lock: self };
            }
            crate::sys::thread_impl::yield_now();
        }
    }

    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        let s = self.state.load(Ordering::Relaxed);
        if s >= 0 {
            self.state.compare_exchange(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                .ok()
                .map(|_| RwLockReadGuard { lock: self })
        } else {
            None
        }
    }

    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.state
            .compare_exchange(UNLOCKED, WRITE_LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| RwLockWriteGuard { lock: self })
    }

    pub fn get_mut(&mut self) -> &mut T { unsafe { &mut *self.data.get() } }
}

pub struct RwLockReadGuard<'a, T: ?Sized> { lock: &'a RwLock<T> }
pub struct RwLockWriteGuard<'a, T: ?Sized> { lock: &'a RwLock<T> }

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { unsafe { &*self.lock.data.get() } }
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { unsafe { &*self.lock.data.get() } }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T { unsafe { &mut *self.lock.data.get() } }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) { self.lock.state.fetch_sub(1, Ordering::Release); }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) { self.lock.state.store(UNLOCKED, Ordering::Release); }
}
