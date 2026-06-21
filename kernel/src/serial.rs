/// Serial port driver — COM1 (0x3F8), 115200 8N1.
///
/// Provides a `SerialPort` struct with `core::fmt::Write`, a generic
/// `SpinLock<T>` backed by a single `AtomicBool`, and a global `SERIAL`
/// instance.  The `kprint!` / `kprintln!` macros are defined here and
/// exported to the crate root.
///
/// No heap is required; everything is statically allocated.

use core::{
    cell::UnsafeCell,
    fmt,
    hint,
    sync::atomic::{AtomicBool, Ordering},
};

// ── ANSI terminal color codes ──────────────────────────────────────────────
pub const TAG:  &str = "\x1b[1;36m"; // bold cyan — subsystem tag
pub const RST:  &str = "\x1b[0m";    // reset all attributes
pub const OK:   &str = "\x1b[32m";   // green — success/passed
pub const STAT: &str = "\x1b[33m";   // yellow — stats/numbers
pub const VRB:  &str = "\x1b[2m";    // dim — verbose/secondary info
pub const WIN:  &str = "\x1b[1;32m"; // bold green — final success

// ── I/O port helpers ───────────────────────────────────────────────────────

unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    unsafe {
        let val: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") val,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
        val
    }
}

// ── SerialPort ─────────────────────────────────────────────────────────────

/// COM1 base I/O port.
pub const COM1: u16 = 0x3F8;

// Register offsets from base port
const OFF_RBR:     u16 = 0; // Receive Buffer Register    (read,  DLAB=0)
const OFF_THR:     u16 = 0; // Transmit Holding Register  (write, DLAB=0)
const OFF_IER:     u16 = 1; // Interrupt Enable Register
const OFF_FCR:     u16 = 2; // FIFO Control Register
const OFF_LCR:     u16 = 3; // Line Control Register  (bit 7 = DLAB)
const OFF_MCR:     u16 = 4; // Modem Control Register
const OFF_LSR:     u16 = 5; // Line Status Register   (bit 5 = THR empty, bit 0 = DR)
const OFF_DLAB_LO: u16 = 0; // Divisor Latch Low  (DLAB=1)
const OFF_DLAB_HI: u16 = 1; // Divisor Latch High (DLAB=1)

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    pub const fn new(base: u16) -> Self {
        SerialPort { base }
    }

    /// Initialise the UART: 115200 baud, 8N1, FIFO enabled, no interrupts.
    pub fn init(&mut self) {
        unsafe {
            outb(self.base + OFF_IER,     0x00); // disable UART interrupts
            outb(self.base + OFF_LCR,     0x80); // set DLAB to access baud divisor
            outb(self.base + OFF_DLAB_LO, 0x01); // divisor = 1  →  115200 baud
            outb(self.base + OFF_DLAB_HI, 0x00);
            outb(self.base + OFF_LCR,     0x03); // 8 data bits, no parity, 1 stop; clear DLAB
            outb(self.base + OFF_FCR,     0x01); // enable FIFO, 1-byte threshold (polls via LSR)
            outb(self.base + OFF_MCR,     0x0B); // assert DTR + RTS
        }
    }

    /// Return `true` if at least one byte is waiting in the receive FIFO.
    ///
    /// Non-destructive: reads only LSR bit 0 (Data Ready), never RBR.
    /// Use this to distinguish a plain ESC keypress from the start of an
    /// escape sequence without consuming any bytes.
    pub fn data_ready(&self) -> bool {
        unsafe { inb(self.base + OFF_LSR) & 0x01 != 0 }
    }

    /// Try to read one byte from the receive FIFO without blocking.
    ///
    /// Returns `Some(byte)` if the Data Ready bit (LSR bit 0) is set, `None`
    /// otherwise.  Call from a loop with `yield_task()` to implement a
    /// blocking read without burning the CPU.
    pub fn try_read_byte(&mut self) -> Option<u8> {
        unsafe {
            if inb(self.base + OFF_LSR) & 0x01 != 0 {
                Some(inb(self.base + OFF_RBR))
            } else {
                None
            }
        }
    }

    /// Write a single byte, blocking until the Transmit Holding Register is empty.
    pub fn write_byte(&mut self, b: u8) {
        unsafe {
            // Poll Line Status Register bit 5 (THRE — Transmit Holding Register Empty)
            while inb(self.base + OFF_LSR) & 0x20 == 0 {
                hint::spin_loop();
            }
            outb(self.base + OFF_THR, b);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.write_byte(b);
        }
        Ok(())
    }
}

// ── SpinLock<T> ────────────────────────────────────────────────────────────
//
// TTAS (test-and-test-and-set) spinlock backed by a single AtomicBool.
// Sufficient for the early kernel where contention is rare and there is no
// scheduler yet.  Will be replaced by a proper futex-based mutex post-Step 7.

pub struct SpinLock<T> {
    locked: AtomicBool,
    data:   UnsafeCell<T>,
}

pub struct SpinLockGuard<'a, T> {
    lock:   &'a SpinLock<T>,
    rflags: u64,  // RFLAGS saved before cli — restored on drop
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        SpinLock {
            locked: AtomicBool::new(false),
            data:   UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        // Save RFLAGS and disable interrupts before spinning.  Without this,
        // the APIC timer can preempt us while we hold the lock and switch to
        // another task that also calls kprintln, causing a deadlock.
        let rflags: u64;
        unsafe {
            core::arch::asm!(
                "pushfq",
                "pop {rf}",
                "cli",
                rf = out(reg) rflags,
            );
        }
        loop {
            // Fast path: try to acquire immediately
            if self.locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            // Slow path: spin with a relaxed load to avoid bus-lock saturation
            while self.locked.load(Ordering::Relaxed) {
                hint::spin_loop();
            }
        }
        SpinLockGuard { lock: self, rflags }
    }
}

// SAFETY: SpinLock guarantees mutual exclusion; T need only be Send.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        // Restore the interrupt flag to its state before lock().
        unsafe {
            core::arch::asm!(
                "push {rf}",
                "popfq",
                rf = in(reg) self.rflags,
            );
        }
    }
}

impl<T> core::ops::Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

// ── Global instance ────────────────────────────────────────────────────────

pub static SERIAL: SpinLock<SerialPort> = SpinLock::new(SerialPort::new(COM1));

/// Initialise COM1.  Must be called once before any `kprint!` / `kprintln!`.
pub fn init() {
    SERIAL.lock().init();
}

// ── kprint! / kprintln! ────────────────────────────────────────────────────

/// Print to the kernel serial console without a trailing newline.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = $crate::serial::SERIAL.lock().write_fmt(format_args!($($arg)*));
    }};
}

/// Print to the kernel serial console with a `\r\n` line ending.
#[macro_export]
macro_rules! kprintln {
    ()            => { $crate::kprint!("\r\n") };
    ($($arg:tt)*) => {{
        $crate::kprint!($($arg)*);
        $crate::kprint!("\r\n");
    }};
}
