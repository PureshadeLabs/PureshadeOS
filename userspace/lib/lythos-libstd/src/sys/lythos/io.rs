// PAL — I/O primitives.
//
// Lythos does not have a POSIX file-descriptor layer.  Standard I/O is
// implemented as follows:
//
//   stdout / stderr → SYS_LOG  (kernel debug log, serial)
//   stdin           → SYS_SERIAL_READ

/// Write `buf` to the kernel log (stdout/stderr).
pub fn log_write(buf: &[u8]) -> Result<usize, i32> {
    // sys_log takes &str; convert lossily so we never panic.
    let s = core::str::from_utf8(buf).unwrap_or("<non-utf8>");
    lythos_rt::sys_log(s);
    Ok(buf.len())
}

/// Read up to `buf.len()` bytes from the serial console (stdin).
///
/// Blocks until at least one byte is available.
pub fn serial_read(buf: &mut [u8]) -> Result<usize, i32> {
    lythos_rt::sys_serial_read(buf).map_err(|_| -1i32)
}
