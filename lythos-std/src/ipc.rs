/// IPC endpoint wrapper.
///
/// An `Endpoint` is a newtype over a capability handle (`u64`).  All
/// operations go through the lythos IPC syscalls; the kernel owns the
/// ring-buffer backing store.

use crate::error::SysError;
use crate::syscall::*;

/// Size of a single IPC message slot (bytes).
pub const MSG_SIZE: usize = 64;

/// A capability handle to a lythos IPC endpoint.
pub struct Endpoint(u64);

impl Endpoint {
    /// Allocate a new IPC endpoint.  Returns the endpoint with full rights.
    pub fn create() -> Result<Self, SysError> {
        let h = unsafe { syscall0(SYS_IPC_CREATE) };
        SysError::from_raw(h).map(Endpoint)
    }

    /// Wrap an existing raw capability handle.
    pub fn from_raw(handle: u64) -> Self { Endpoint(handle) }

    /// Return the underlying capability handle.
    pub fn as_raw(&self) -> u64 { self.0 }

    /// Send up to `MSG_SIZE` bytes to this endpoint.
    ///
    /// Blocks if the ring buffer is full; resumes when the receiver
    /// consumes a slot.
    pub fn send(&self, msg: &[u8]) -> Result<(), SysError> {
        let len = msg.len().min(MSG_SIZE) as u64;
        let r = unsafe { syscall3(SYS_IPC_SEND, self.0, msg.as_ptr() as u64, len) };
        SysError::from_raw(r).map(|_| ())
    }

    /// Receive exactly one 64-byte message frame from this endpoint.
    ///
    /// Blocks if the ring buffer is empty; resumes when a sender posts a
    /// message.  Returns the full fixed-size frame regardless of how many
    /// bytes the sender actually wrote.
    pub fn recv_frame(&self) -> Result<[u8; MSG_SIZE], SysError> {
        let mut buf = [0u8; MSG_SIZE];
        let r = unsafe {
            syscall3(SYS_IPC_RECV, self.0, buf.as_mut_ptr() as u64, MSG_SIZE as u64)
        };
        SysError::from_raw(r).map(|_| buf)
    }
}
