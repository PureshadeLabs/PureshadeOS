use core::sync::atomic::{AtomicU8, Ordering};
use core::cell::UnsafeCell;

const INCOMPLETE: u8 = 0;
const RUNNING:    u8 = 1;
const COMPLETE:   u8 = 2;
const POISONED:   u8 = 3;

/// A one-shot synchronisation point (equivalent to `std::sync::Once`).
pub struct Once {
    state: AtomicU8,
}

impl Once {
    pub const fn new() -> Self { Once { state: AtomicU8::new(INCOMPLETE) } }

    pub fn call_once<F: FnOnce()>(&self, f: F) {
        if self.state.load(Ordering::Acquire) == COMPLETE { return; }
        self.call_inner(f);
    }

    fn call_inner<F: FnOnce()>(&self, f: F) {
        match self.state.compare_exchange(INCOMPLETE, RUNNING, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => {
                f();
                self.state.store(COMPLETE, Ordering::Release);
            }
            Err(COMPLETE) => {}
            Err(_) => {
                while self.state.load(Ordering::Acquire) == RUNNING {
                    crate::sys::thread_impl::yield_now();
                }
            }
        }
    }

    pub fn is_completed(&self) -> bool {
        self.state.load(Ordering::Acquire) == COMPLETE
    }
}

/// `OnceState` argument to `call_once_force` (not commonly needed; provided for compat).
pub struct OnceState {
    pub(crate) poisoned: bool,
}

impl OnceState {
    pub fn is_poisoned(&self) -> bool { self.poisoned }
}

// ── OnceLock ──────────────────────────────────────────────────────────────────

/// A cell which can be written once and then read many times.
pub struct OnceLock<T> {
    once:  Once,
    value: UnsafeCell<core::mem::MaybeUninit<T>>,
}

unsafe impl<T: Send> Send for OnceLock<T> {}
unsafe impl<T: Sync> Sync for OnceLock<T> {}

impl<T> OnceLock<T> {
    pub const fn new() -> Self {
        OnceLock {
            once:  Once::new(),
            value: UnsafeCell::new(core::mem::MaybeUninit::uninit()),
        }
    }

    pub fn get(&self) -> Option<&T> {
        if self.once.is_completed() {
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    pub fn get_or_init<F: FnOnce() -> T>(&self, f: F) -> &T {
        self.once.call_once(|| {
            let v = f();
            unsafe { (*self.value.get()).write(v); }
        });
        unsafe { (*self.value.get()).assume_init_ref() }
    }

    pub fn set(&self, value: T) -> Result<(), T> {
        let mut opt = Some(value);
        let mut failed = false;
        self.once.call_once(|| {
            let v = opt.take().unwrap();
            unsafe { (*self.value.get()).write(v); }
        });
        match opt {
            None    => Ok(()),
            Some(v) => Err(v),
        }
    }
}

impl<T> Drop for OnceLock<T> {
    fn drop(&mut self) {
        if self.once.is_completed() {
            unsafe { (*self.value.get()).assume_init_drop(); }
        }
    }
}
