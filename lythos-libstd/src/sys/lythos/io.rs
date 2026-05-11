// PAL — I/O primitives.
//
// Lythos does not have a POSIX file-descriptor layer.  Standard I/O is
// implemented as follows:
//
//   stdout / stderr → SYS_LOG  (kernel debug log, serial)
//   stdin           → SYS_SERIAL_READ

use lythos_std::syscall::{SYS_LOG, SYS_SERIAL_READ, syscall2, syscall3};
use lythos_std::error::SysError;

/// Write `buf` to the kernel log (stdout/stderr).
///
/// The kernel log call takes (ptr, len) and returns 0 on success.
pub fn log_write(buf: &[u8]) -> Result<usize, i32> {
    let ret = unsafe {
        syscall2(SYS_LOG, buf.as_ptr() as u64, buf.len() as u64)
    };
    if SysError::is_err(ret) {
        Err(-1)
    } else {
        Ok(buf.len())
    }
}

/// Read up to `buf.len()` bytes from the serial console (stdin).
///
/// Blocks until at least one byte is available.
pub fn serial_read(buf: &mut [u8]) -> Result<usize, i32> {
    let ret = unsafe {
        syscall2(
            SYS_SERIAL_READ,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if SysError::is_err(ret) {
        Err(-1)
    } else {
        Ok(ret as usize)
    }
}
