//! PS/2 keyboard driver — i8042 controller, scan code set 2 → ASCII.
//!
//! Disables i8042's built-in set-2→set-1 translation so the IRQ handler
//! receives raw set-2 bytes directly.  Make codes are decoded to ASCII
//! (or ANSI escape sequences for arrow/navigation keys) and pushed into a
//! 256-byte ring buffer that `SYS_SERIAL_READ` drains alongside COM1.
//!
//! IRQ1 / GSI 1 / IDT vector 36.

use core::arch::global_asm;
use core::hint;
use core::sync::atomic::{AtomicU8, Ordering};
use crate::serial::SpinLock;

// ── Vector assignment ─────────────────────────────────────────────────────────

pub const VECTOR_KBD: u8 = 36;

// ── I/O port constants ────────────────────────────────────────────────────────

const DATA_PORT:   u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const CMD_PORT:    u16 = 0x64;

const CMD_DISABLE_P2: u8 = 0xA7;
const CMD_DISABLE_P1: u8 = 0xAD;
const CMD_ENABLE_P1:  u8 = 0xAE;
const CMD_READ_CFG:   u8 = 0x20;
const CMD_WRITE_CFG:  u8 = 0x60;

const CFG_P1_IRQ:  u8 = 1 << 0; // port 1 interrupt enable
const CFG_P1_XLAT: u8 = 1 << 6; // set-2→set-1 translation (we clear this)

const STATUS_OBF: u8 = 1 << 0; // output buffer full — data waiting at 0x60
const STATUS_IBF: u8 = 1 << 1; // input buffer full  — wait before writing
const STATUS_AUX: u8 = 1 << 5; // OBF data is from the aux (mouse) port

// ── I/O helpers ───────────────────────────────────────────────────────────────

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port, in("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    unsafe {
        let v: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") v, in("dx") port,
            options(nomem, nostack, preserves_flags),
        );
        v
    }
}

fn wait_write() {
    for _ in 0..100_000u32 {
        if unsafe { inb(STATUS_PORT) } & STATUS_IBF == 0 { return; }
        hint::spin_loop();
    }
}

fn wait_read() -> bool {
    for _ in 0..100_000u32 {
        if unsafe { inb(STATUS_PORT) } & STATUS_OBF != 0 { return true; }
        hint::spin_loop();
    }
    false
}

fn flush_output() {
    while unsafe { inb(STATUS_PORT) } & STATUS_OBF != 0 {
        unsafe { inb(DATA_PORT) };
    }
}

fn ctrl_cmd(cmd: u8) {
    wait_write();
    unsafe { outb(CMD_PORT, cmd) };
}

fn ctrl_write(val: u8) {
    wait_write();
    unsafe { outb(DATA_PORT, val) };
}

fn ctrl_read() -> Option<u8> {
    if wait_read() { Some(unsafe { inb(DATA_PORT) }) } else { None }
}

// ── Scan code set 2 → ASCII tables ───────────────────────────────────────────
//
// Index = set-2 make code byte.  Value = ASCII character, or 0 if the key
// generates no printable output (modifiers, Fn keys, etc.).

const fn make_unshifted() -> [u8; 256] {
    let mut t = [0u8; 256];
    // number row
    t[0x16] = b'1'; t[0x1E] = b'2'; t[0x26] = b'3'; t[0x25] = b'4'; t[0x2E] = b'5';
    t[0x36] = b'6'; t[0x3D] = b'7'; t[0x3E] = b'8'; t[0x46] = b'9'; t[0x45] = b'0';
    t[0x4E] = b'-'; t[0x55] = b'='; t[0x66] = 0x7F; // Backspace → DEL
    t[0x0E] = b'`';
    // QWERTY row
    t[0x0D] = b'\t';
    t[0x15] = b'q'; t[0x1D] = b'w'; t[0x24] = b'e'; t[0x2D] = b'r'; t[0x2C] = b't';
    t[0x35] = b'y'; t[0x3C] = b'u'; t[0x43] = b'i'; t[0x44] = b'o'; t[0x4D] = b'p';
    t[0x54] = b'['; t[0x5B] = b']'; t[0x5D] = b'\\';
    // ASDF row
    t[0x1C] = b'a'; t[0x1B] = b's'; t[0x23] = b'd'; t[0x2B] = b'f'; t[0x34] = b'g';
    t[0x33] = b'h'; t[0x3B] = b'j'; t[0x42] = b'k'; t[0x4B] = b'l'; t[0x4C] = b';';
    t[0x52] = b'\''; t[0x5A] = b'\r';
    // ZXCV row
    t[0x1A] = b'z'; t[0x22] = b'x'; t[0x21] = b'c'; t[0x2A] = b'v'; t[0x32] = b'b';
    t[0x31] = b'n'; t[0x3A] = b'm'; t[0x41] = b','; t[0x49] = b'.'; t[0x4A] = b'/';
    // misc
    t[0x29] = b' ';
    t[0x76] = 0x1B; // Escape
    t
}

const fn make_shifted() -> [u8; 256] {
    let mut t = [0u8; 256];
    // number row shifted
    t[0x16] = b'!'; t[0x1E] = b'@'; t[0x26] = b'#'; t[0x25] = b'$'; t[0x2E] = b'%';
    t[0x36] = b'^'; t[0x3D] = b'&'; t[0x3E] = b'*'; t[0x46] = b'('; t[0x45] = b')';
    t[0x4E] = b'_'; t[0x55] = b'+'; t[0x66] = 0x7F;
    t[0x0E] = b'~';
    // QWERTY row shifted
    t[0x0D] = b'\t';
    t[0x15] = b'Q'; t[0x1D] = b'W'; t[0x24] = b'E'; t[0x2D] = b'R'; t[0x2C] = b'T';
    t[0x35] = b'Y'; t[0x3C] = b'U'; t[0x43] = b'I'; t[0x44] = b'O'; t[0x4D] = b'P';
    t[0x54] = b'{'; t[0x5B] = b'}'; t[0x5D] = b'|';
    // ASDF row shifted
    t[0x1C] = b'A'; t[0x1B] = b'S'; t[0x23] = b'D'; t[0x2B] = b'F'; t[0x34] = b'G';
    t[0x33] = b'H'; t[0x3B] = b'J'; t[0x42] = b'K'; t[0x4B] = b'L'; t[0x4C] = b':';
    t[0x52] = b'"'; t[0x5A] = b'\r';
    // ZXCV row shifted
    t[0x1A] = b'Z'; t[0x22] = b'X'; t[0x21] = b'C'; t[0x2A] = b'V'; t[0x32] = b'B';
    t[0x31] = b'N'; t[0x3A] = b'M'; t[0x41] = b'<'; t[0x49] = b'>'; t[0x4A] = b'?';
    // misc
    t[0x29] = b' ';
    t[0x76] = 0x1B;
    t
}

static SC2_UNSHIFTED: [u8; 256] = make_unshifted();
static SC2_SHIFTED:   [u8; 256] = make_shifted();

// ── Decoder state ─────────────────────────────────────────────────────────────
//
// Packed into a single AtomicU8 since the IRQ handler is the sole writer and
// always runs on the BSP with IF=0 — no concurrent modification is possible.

const STATE_SHIFT:     u8 = 1 << 0; // a shift key is held
const STATE_CTRL:      u8 = 1 << 1; // a ctrl key is held
const STATE_EXT:       u8 = 1 << 2; // received 0xE0 prefix
const STATE_BREAK:     u8 = 1 << 3; // received 0xF0 prefix
const STATE_EXT_BREAK: u8 = 1 << 4; // received 0xE0 0xF0 prefix

static KBD_STATE: AtomicU8 = AtomicU8::new(0);

// ── Ring buffer ───────────────────────────────────────────────────────────────

const BUF_CAP: usize = 256;

struct KbdBuf {
    data: [u8; BUF_CAP],
    head: usize,
    tail: usize,
}

impl KbdBuf {
    const fn new() -> Self {
        Self { data: [0; BUF_CAP], head: 0, tail: 0 }
    }

    fn push(&mut self, b: u8) {
        let next = (self.tail + 1) % BUF_CAP;
        if next != self.head {
            self.data[self.tail] = b;
            self.tail = next;
        }
        // silently drop if full
    }

    fn push_bytes(&mut self, bs: &[u8]) {
        for &b in bs { self.push(b); }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail { return None; }
        let b = self.data[self.head];
        self.head = (self.head + 1) % BUF_CAP;
        Some(b)
    }

    fn is_empty(&self) -> bool { self.head == self.tail }
}

static KBD_BUF: SpinLock<KbdBuf> = SpinLock::new(KbdBuf::new());

// ── ISR stub ─────────────────────────────────────────────────────────────────

global_asm!(r#"
.section .text
.global kbd_isr_stub
.type   kbd_isr_stub, @function
kbd_isr_stub:
    pushq  %rax
    pushq  %rcx
    pushq  %rdx
    pushq  %rsi
    pushq  %rdi
    pushq  %r8
    pushq  %r9
    pushq  %r10
    pushq  %r11
    call   kbd_irq_handler
    popq   %r11
    popq   %r10
    popq   %r9
    popq   %r8
    popq   %rdi
    popq   %rsi
    popq   %rdx
    popq   %rcx
    popq   %rax
    iretq
"#, options(att_syntax));

unsafe extern "C" { fn kbd_isr_stub(); }

// ── IRQ handler ───────────────────────────────────────────────────────────────

/// Self-check latch: set by the first keyboard IRQ; the handler logs once so
/// boot verification can confirm the IOAPIC → vector → handler path fires.
static FIRST_IRQ_SEEN: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[unsafe(no_mangle)]
pub extern "C" fn kbd_irq_handler() {
    let sc = unsafe { inb(DATA_PORT) };

    if !FIRST_IRQ_SEEN.swap(true, Ordering::Relaxed) {
        crate::kprintln!(
            "[kbd] self-check: vector {} fired (scancode {:#04x}) — EOI via Local APIC",
            VECTOR_KBD, sc
        );
    }

    decode_scancode(sc);
    crate::apic::eoi();
}

/// Decode one raw set-2 scancode byte, updating the prefix/modifier state
/// machine and pushing any resulting output bytes into `KBD_BUF`.
/// Shared by the IRQ handler and the polling fallback ([`poll_hw`]).
fn decode_scancode(sc: u8) {
    let mut state = KBD_STATE.load(Ordering::Relaxed);

    // ── Prefix bytes — update state and wait for the actual key code ──────────

    if sc == 0xE0 {
        if state & STATE_BREAK != 0 {
            // 0xF0 0xE0 (unusual) → treat as extended break prefix
            state = (state & !STATE_BREAK) | STATE_EXT_BREAK;
        } else {
            state |= STATE_EXT;
        }
        KBD_STATE.store(state, Ordering::Relaxed);
        return;
    }

    if sc == 0xF0 {
        if state & STATE_EXT != 0 {
            // 0xE0 0xF0 xx — extended break
            state = (state & !STATE_EXT) | STATE_EXT_BREAK;
        } else {
            state |= STATE_BREAK;
        }
        KBD_STATE.store(state, Ordering::Relaxed);
        return;
    }

    // ── Decode key code ───────────────────────────────────────────────────────

    let is_ext   = state & (STATE_EXT | STATE_EXT_BREAK) != 0;
    let is_break = state & (STATE_BREAK | STATE_EXT_BREAK) != 0;
    state &= !(STATE_EXT | STATE_BREAK | STATE_EXT_BREAK);

    if is_ext {
        // Extended key — only arrow/navigation keys produce output
        if !is_break {
            let seq: &[u8] = match sc {
                0x75 => b"\x1b[A",  // Up
                0x72 => b"\x1b[B",  // Down
                0x74 => b"\x1b[C",  // Right
                0x6B => b"\x1b[D",  // Left
                0x71 => b"\x1b[3~", // Delete
                0x6C => b"\x1b[H",  // Home
                0x69 => b"\x1b[F",  // End
                0x7D => b"\x1b[5~", // PgUp
                0x7A => b"\x1b[6~", // PgDn
                // Extended ctrl/alt/shift — track but no char
                0x14 => { state |= STATE_CTRL; b"" } // Right Ctrl make
                _    => b"",
            };
            if !seq.is_empty() {
                KBD_BUF.lock().push_bytes(seq);
            }
        } else if sc == 0x14 {
            // Right Ctrl break
            state &= !STATE_CTRL;
        }
    } else {
        match sc {
            // Modifiers
            0x12 | 0x59 => {
                if is_break { state &= !STATE_SHIFT; } else { state |= STATE_SHIFT; }
            }
            0x14 => {
                if is_break { state &= !STATE_CTRL; } else { state |= STATE_CTRL; }
            }
            0x11 => { /* Alt — track if needed later */ }
            _ => {
                if !is_break {
                    let ch = if state & STATE_SHIFT != 0 {
                        SC2_SHIFTED[sc as usize]
                    } else {
                        SC2_UNSHIFTED[sc as usize]
                    };
                    if ch != 0 {
                        let out = if state & STATE_CTRL != 0 && ch.is_ascii_alphabetic() {
                            ch.to_ascii_lowercase() & 0x1F // Ctrl+letter → control code
                        } else {
                            ch
                        };
                        KBD_BUF.lock().push(out);
                    }
                }
            }
        }
    }

    KBD_STATE.store(state, Ordering::Relaxed);
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Poll-mode fallback: drain any scancodes waiting in the i8042 output
/// buffer through the shared decoder.  Covers configurations where the
/// IRQ1 route does not deliver (observed under QEMU q35/OVMF); harmless
/// alongside the IRQ path, which consumes bytes first.  The cli-lock stops
/// the IRQ handler from interleaving between the status check and the data
/// read on this CPU.
static POLL_LOCK: SpinLock<()> = SpinLock::new(());

fn poll_hw() {
    let _guard = POLL_LOCK.lock(); // cli until drop
    loop {
        let st = unsafe { inb(STATUS_PORT) };
        if st & STATUS_OBF == 0 { break; }
        let b = unsafe { inb(DATA_PORT) };
        if st & STATUS_AUX != 0 { continue; } // mouse byte — discard
        decode_scancode(b);
    }
}

/// Push raw bytes into the keyboard ring buffer as if typed.  Used by the
/// kernel logger to answer terminal queries (ESC[6n cursor-position report)
/// on behalf of the framebuffer console — userspace reads the reply through
/// the normal SYS_SERIAL_READ path.
pub fn inject(bytes: &[u8]) {
    KBD_BUF.lock().push_bytes(bytes);
}

/// Try to read one decoded byte from the keyboard ring buffer (non-blocking).
pub fn try_read() -> Option<u8> {
    poll_hw();
    KBD_BUF.lock().pop()
}

/// TEMP DEBUG: raw i8042 status byte (no side effects).
pub fn status_raw() -> u8 {
    unsafe { inb(STATUS_PORT) }
}

/// TEMP DEBUG: whether the IRQ path has ever fired.
pub fn irq_seen() -> bool {
    FIRST_IRQ_SEEN.load(Ordering::Relaxed)
}

/// TEMP DEBUG: decoded bytes waiting in the ring buffer.
pub fn buffered() -> usize {
    let b = KBD_BUF.lock();
    (b.tail + BUF_CAP - b.head) % BUF_CAP
}

/// Return `true` if there is at least one byte waiting in the keyboard buffer.
pub fn data_ready() -> bool {
    poll_hw();
    !KBD_BUF.lock().is_empty()
}

/// Initialise the PS/2 i8042 controller and arm the keyboard IRQ.
///
/// Clears the translation bit in the i8042 config so the IRQ handler receives
/// raw scan-code-set-2 bytes.  Resolves ISA IRQ 1 to its GSI (honouring any
/// ACPI MADT Interrupt Source Override, including polarity/trigger flags) and
/// programs the I/O APIC redirection entry for `VECTOR_KBD`.
///
/// Returns the resolved `(gsi, redirection_flags)` for boot-log reporting,
/// or `None` if no i8042 controller responded.
pub fn init() -> Option<(u32, u32)> {
    flush_output();

    // Disable both PS/2 ports while reconfiguring.
    ctrl_cmd(CMD_DISABLE_P1);
    ctrl_cmd(CMD_DISABLE_P2);
    flush_output();

    // Read config, clear translation, set port-1 IRQ enable (command byte bit 0).
    ctrl_cmd(CMD_READ_CFG);
    let cfg = ctrl_read()?; // no i8042 present
    ctrl_cmd(CMD_WRITE_CFG);
    ctrl_write((cfg & !CFG_P1_XLAT) | CFG_P1_IRQ);

    // Re-enable port 1.
    ctrl_cmd(CMD_ENABLE_P1);
    flush_output(); // discard anything queued while reconfiguring

    // Resolve ISA IRQ 1 → GSI (identity + edge/active-high unless the MADT
    // provides an Interrupt Source Override) and arm the redirection entry.
    let (gsi, flags) = crate::acpi::isa_irq_route(1);
    crate::idt::register_irq(VECTOR_KBD, kbd_isr_stub as *const () as u64);
    crate::ioapic::map_irq(gsi, VECTOR_KBD, flags);
    Some((gsi, flags))
}
