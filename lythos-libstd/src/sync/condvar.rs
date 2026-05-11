use super::mutex::MutexGuard;
use crate::time::Duration;

/// A condition variable backed by cooperative spin-yield.
///
/// On Lythos there is no futex; this implementation yields the CPU between
/// predicate checks.  For fine-grained signalling prefer IPC channels.
pub struct Condvar(());

impl Condvar {
    pub const fn new() -> Self { Condvar(()) }

    /// Block until `guard`'s lock is re-acquired and `f()` returns `true`.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        // We have no park/unpark; re-acquire the lock on the next quantum.
        // The caller typically uses a loop: `while !cond { guard = cv.wait(guard); }`.
        let mutex = guard.mutex;
        drop(guard);
        crate::sys::thread_impl::yield_now();
        mutex.lock()
    }

    /// `wait` with a timeout (spin-based).
    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        dur: Duration,
    ) -> (MutexGuard<'a, T>, bool) {
        let mutex = guard.mutex;
        let deadline = crate::sys::time_impl::read_nanos() + dur.as_nanos() as u64;
        drop(guard);
        loop {
            crate::sys::thread_impl::yield_now();
            if crate::sys::time_impl::read_nanos() >= deadline {
                return (mutex.lock(), true); // timed out
            }
            return (mutex.lock(), false); // woken (optimistic: one yield then return)
        }
    }

    pub fn notify_one(&self) { /* no-op: waiting tasks will re-check naturally */ }
    pub fn notify_all(&self) { /* no-op */ }
}
