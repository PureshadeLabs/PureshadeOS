//! Unified kernel logger — fans every `kprint!` / `kprintln!` out to both
//! sinks: the COM1 serial port and the framebuffer console.
//!
//! Log levels are encoded at call sites as ANSI SGR sequences (the `TAG`,
//! `OK`, `STAT`, `VRB`, `WIN` constants in `serial.rs`).  The logger parses
//! those sequences and splits them:
//!
//! - **serial** receives the text with SGR (colour) sequences stripped, so
//!   headless captures (CI, `-nographic`, `-serial file:`) stay clean.  All
//!   other CSI sequences (cursor movement, screen clear, DSR queries) pass
//!   through untouched — full-screen userspace apps (rkilo) drive the
//!   attached terminal through SYS_LOG and need them intact.
//! - **framebuffer console** receives the text with the SGR colour applied
//!   as a glyph foreground colour (mapped to the Catppuccin Mocha palette);
//!   non-SGR CSI sequences are dropped (the console has no cursor
//!   addressing).
//!
//! Sink selection is runtime: the console sink is skipped until
//! `console::init()` has bound a framebuffer, so a machine without one
//! (no Limine framebuffer response) logs to serial alone.
//!
//! Lock order is LOGGER → SERIAL → CONSOLE; nothing takes them in another
//! order (`print!` takes CONSOLE alone, syscall readers take SERIAL alone).

use core::fmt;

use crate::console;
use crate::serial::{self, SpinLock};

// ── SGR colour mapping (Catppuccin Mocha) ─────────────────────────────────────

const DIM: u32 = 0x009399B2; // overlay2 — SGR 2 (dim / verbose)

/// SGR 30–37 → Mocha accents.
const ANSI16: [u32; 8] = [
    0x0045475A, // 30 black   — surface1
    0x00F38BA8, // 31 red
    0x00A6E3A1, // 32 green
    0x00F9E2AF, // 33 yellow
    0x0089B4FA, // 34 blue
    0x00F5C2E7, // 35 magenta — pink
    0x0089DCEB, // 36 cyan    — sky
    0x00BAC2DE, // 37 white   — subtext1
];

/// SGR 90–97 (bright) — same accents, brighter black/white.
const ANSI16_BRIGHT: [u32; 8] = [
    0x00585B70, // 90 bright black — surface2
    0x00F38BA8, 0x00A6E3A1, 0x00F9E2AF, 0x0089B4FA, 0x00F5C2E7, 0x0089DCEB,
    0x00CDD6F4, // 97 bright white — text
];

// ── Escape-sequence parser ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum EscState {
    Normal,
    Esc,      // seen ESC, expecting '['
    Csi,      // inside CSI, accumulating parameters
}

/// Fan-out writer with a persistent ANSI parser.
///
/// Parser state lives here (not on the stack) because `write_fmt` delivers
/// formatted output in fragments — an SGR sequence assembled from a `{TAG}`
/// interpolation can be split across `write_str` calls.
pub struct Logger {
    state:   EscState,
    params:  [u16; 8],
    nparams: usize,
    cur:     u16,
    has_cur: bool, // digit seen since the last push — distinguishes "[m" from "[0m"
    // Raw bytes of the in-flight escape sequence ("\x1b[…"), so non-SGR
    // sequences can be replayed verbatim to serial once the final byte
    // reveals what they are.  32 bytes covers any real CSI; longer sequences
    // are truncated (only their tail is lost, and only on the serial sink).
    raw:     [u8; 32],
    raw_len: usize,
}

impl Logger {
    pub const fn new() -> Self {
        Logger {
            state: EscState::Normal, params: [0; 8], nparams: 0, cur: 0, has_cur: false,
            raw: [0; 32], raw_len: 0,
        }
    }

    fn raw_push(&mut self, c: char) {
        // CSI bytes are all single-byte ASCII; multi-byte chars can only
        // appear via a malformed sequence and are dropped from the replay.
        if self.raw_len < self.raw.len() && (c as u32) < 0x80 {
            self.raw[self.raw_len] = c as u8;
            self.raw_len += 1;
        }
    }

    /// Apply accumulated SGR parameters to the console foreground colour.
    fn apply_sgr(&self, con: &mut console::Console) {
        // "\x1b[m" is shorthand for "\x1b[0m".
        if self.nparams == 0 {
            con.reset_fg();
            return;
        }
        for &p in &self.params[..self.nparams] {
            match p {
                0       => con.reset_fg(),
                2       => con.set_fg(DIM),
                1 | 22  => {} // bold / normal intensity — single-weight font
                30..=37 => con.set_fg(ANSI16[(p - 30) as usize]),
                90..=97 => con.set_fg(ANSI16_BRIGHT[(p - 90) as usize]),
                39      => con.reset_fg(),
                _       => {} // unsupported attribute — ignore
            }
        }
    }

    fn push_param(&mut self) {
        if self.nparams < self.params.len() {
            self.params[self.nparams] = self.cur;
            self.nparams += 1;
        }
        self.cur = 0;
        self.has_cur = false;
    }
}

impl Logger {
    fn write_str_inner(&mut self, s: &str, use_console: bool) -> fmt::Result {
        let mut ser = serial::SERIAL.lock();
        // Runtime sink selection: no framebuffer (or serial-only caller,
        // e.g. the periodic idle diagnostics via `kdiagln!`) → serial only.
        let mut con = if use_console && console::is_active() {
            Some(console::CONSOLE.lock())
        } else {
            None
        };

        for c in s.chars() {
            match self.state {
                EscState::Normal => {
                    if c == '\x1b' {
                        self.state = EscState::Esc;
                        self.nparams = 0;
                        self.cur = 0;
                        self.has_cur = false;
                        self.raw_len = 0;
                        self.raw_push(c);
                    } else {
                        // Emit to both sinks; escapes never reach here.
                        let mut buf = [0u8; 4];
                        for &b in c.encode_utf8(&mut buf).as_bytes() {
                            ser.write_byte(b);
                        }
                        if let Some(con) = con.as_mut() {
                            con.put_char(c);
                        }
                    }
                }
                EscState::Esc => {
                    // Only CSI is recognised; any other escape is swallowed.
                    if c == '[' {
                        self.raw_push(c);
                        self.state = EscState::Csi;
                    } else {
                        self.state = EscState::Normal;
                    }
                }
                EscState::Csi => match c {
                    '0'..='9' => {
                        self.cur = self.cur.saturating_mul(10) + (c as u16 - '0' as u16);
                        self.has_cur = true;
                        self.raw_push(c);
                    }
                    ';' => {
                        self.push_param();
                        self.raw_push(c);
                    }
                    'm' => {
                        // SGR: colour the console, strip from serial.
                        // Bare "\x1b[m" keeps nparams == 0 → apply_sgr resets.
                        if self.has_cur {
                            self.push_param();
                        }
                        if let Some(con) = con.as_mut() {
                            self.apply_sgr(con);
                        }
                        self.state = EscState::Normal;
                    }
                    // Any other final byte: not SGR — replay the whole
                    // sequence raw to serial (terminal control from
                    // userspace) and interpret the common cursor/erase
                    // controls on the framebuffer console so full-screen
                    // apps (rkilo) and `clear` work there too.
                    '\x40'..='\x7e' => {
                        self.raw_push(c);
                        for i in 0..self.raw_len {
                            ser.write_byte(self.raw[i]);
                        }
                        if self.has_cur {
                            self.push_param();
                        }
                        let p0 = if self.nparams > 0 { self.params[0] } else { 0 };
                        if let Some(con) = con.as_mut() {
                            match c {
                                // Erase-in-display: 2/3 = whole screen + home.
                                // (0/1 partial clears unimplemented — unused
                                // by our userspace.)
                                'J' if p0 >= 2 => con.clear(),
                                // Cursor position (1-based; missing = 1).
                                'H' | 'f' => {
                                    let r  = if self.nparams > 0 { self.params[0].max(1) } else { 1 };
                                    let cl = if self.nparams > 1 { self.params[1].max(1) } else { 1 };
                                    con.set_cursor(r as usize - 1, cl as usize - 1);
                                }
                                'K' => con.clear_line(p0),
                                'A' => con.move_cursor(-(p0.max(1) as isize), 0),
                                'B' => con.move_cursor(p0.max(1) as isize, 0),
                                'C' => con.move_cursor(0, p0.max(1) as isize),
                                'D' => con.move_cursor(0, -(p0.max(1) as isize)),
                                // ?25l/?25h (cursor visibility) and anything
                                // else: no-op — no cursor is rendered.
                                _ => {}
                            }
                            // DSR cursor-position query: answer on behalf of
                            // the console by injecting the reply into the
                            // keyboard ring, where SYS_SERIAL_READ picks it
                            // up.  Apps probe screen size by moving the
                            // cursor to 999;999 (clamped) and asking — so
                            // this reports the console's real dimensions.
                            if c == 'n' && p0 == 6 {
                                let (r, cl) = con.cursor_1based();
                                let mut reply = [0u8; 16];
                                let mut n = 0;
                                for &b in b"\x1b[" { reply[n] = b; n += 1; }
                                n += fmt_usize(r, &mut reply[n..]);
                                reply[n] = b';'; n += 1;
                                n += fmt_usize(cl, &mut reply[n..]);
                                reply[n] = b'R'; n += 1;
                                crate::keyboard::inject(&reply[..n]);
                            }
                        }
                        self.state = EscState::Normal;
                    }
                    _ => self.raw_push(c), // separators (e.g. '?', ':') — keep scanning
                },
            }
        }
        Ok(())
    }
}

impl fmt::Write for Logger {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_str_inner(s, true)
    }
}

/// Adapter that runs the same parser (SGR stripping included) but skips the
/// framebuffer console sink.
struct SerialOnly<'a>(&'a mut Logger);

impl fmt::Write for SerialOnly<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_str_inner(s, false)
    }
}

/// Format `v` as decimal ASCII into `out`; returns the byte count.
fn fmt_usize(v: usize, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 20];
    let mut n = 0;
    let mut v = v;
    if v == 0 { out[0] = b'0'; return 1; }
    while v > 0 && n < tmp.len() {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in 0..n {
        out[i] = tmp[n - 1 - i];
    }
    n
}

// ── Global instance ───────────────────────────────────────────────────────────

pub static LOGGER: SpinLock<Logger> = SpinLock::new(Logger::new());

/// Write formatted output to all active log sinks.
/// Target of the `kprint!` / `kprintln!` macros.
pub fn write_fmt(args: fmt::Arguments) {
    use fmt::Write as _;
    let _ = LOGGER.lock().write_fmt(args);
}

/// Write formatted output to the serial sink only.
/// Target of the `kdiagln!` macro — periodic diagnostics ([ram-idle],
/// [heap-stat], [serial-diag], [task-diag]) that would otherwise scroll the
/// framebuffer console forever on an idle system.
pub fn write_fmt_serial(args: fmt::Arguments) {
    use fmt::Write as _;
    let mut logger = LOGGER.lock();
    let _ = SerialOnly(&mut logger).write_fmt(args);
}
