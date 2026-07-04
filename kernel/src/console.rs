//! Framebuffer text console.
//!
//! Renders an 8x16 bitmap font (see `font8x16.rs`) onto the linear
//! framebuffer mapped by `framebuffer::init_from_limine`. Replaces VGA text
//! mode, which is unavailable on UEFI hardware.
//!
//! The global console lives behind a `SpinLock`; `print!` / `println!`
//! macros write to it. Both are no-ops until `console::init()` has run
//! (which requires an active framebuffer), so early boot output stays on
//! the serial path via `kprint!` / `kprintln!`.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::font8x16::{FONT8X16, GLYPH_HEIGHT, GLYPH_WIDTH};
use crate::framebuffer;
use crate::serial::SpinLock;

// Catppuccin Mocha
pub const BG: u32 = 0x001E_1E2E; // base
pub const FG: u32 = 0x00CD_D6F4; // text

const TAB_STOP: usize = 8;

pub struct Console {
    // Framebuffer parameters, copied at init.
    virt:   u64,
    pitch:  u64,
    fb_w:   u32,
    fb_h:   u32,
    // Character-cell geometry.
    cols: usize,
    rows: usize,
    col:  usize,
    row:  usize,
    fg: u32,
    bg: u32,
}

impl Console {
    const fn inactive() -> Self {
        Console {
            virt: 0, pitch: 0, fb_w: 0, fb_h: 0,
            cols: 0, rows: 0, col: 0, row: 0,
            fg: FG, bg: BG,
        }
    }

    fn is_active(&self) -> bool { self.virt != 0 }

    /// Bind to the mapped framebuffer and clear it to the background colour.
    fn init(&mut self, virt: u64, pitch: u64, fb_w: u32, fb_h: u32) {
        self.virt  = virt;
        self.pitch = pitch;
        self.fb_w  = fb_w;
        self.fb_h  = fb_h;
        self.cols  = fb_w as usize / GLYPH_WIDTH;
        self.rows  = fb_h as usize / GLYPH_HEIGHT;
        self.col   = 0;
        self.row   = 0;
        self.fill_rows(0, fb_h as usize, self.bg);
    }

    #[inline]
    fn pixel_ptr(&self, x: usize, y: usize) -> *mut u32 {
        (self.virt + y as u64 * self.pitch + x as u64 * 4) as *mut u32
    }

    /// Fill the pixel rectangle `[x0, x1) × [y0, y1)`.
    fn fill_rect(&self, x0: usize, x1: usize, y0: usize, y1: usize, color: u32) {
        for y in y0..y1 {
            let row = self.pixel_ptr(x0, y);
            // Plain slice fill — the framebuffer is ordinary write-combining
            // memory; per-pixel write_volatile defeated vectorisation and made
            // full-screen clears visibly slow.
            unsafe {
                core::slice::from_raw_parts_mut(row, x1 - x0).fill(color);
            }
        }
    }

    /// Fill pixel rows `[y0, y1)` across the full width.
    fn fill_rows(&self, y0: usize, y1: usize, color: u32) {
        self.fill_rect(0, self.fb_w as usize, y0, y1, color);
    }

    /// Render one glyph at character cell (col, row).
    fn draw_glyph(&self, byte: u8, col: usize, row: usize) {
        let px0 = col * GLYPH_WIDTH;
        let py0 = row * GLYPH_HEIGHT;
        let glyph = &FONT8X16[byte as usize * GLYPH_HEIGHT..][..GLYPH_HEIGHT];

        for (dy, &bits) in glyph.iter().enumerate() {
            // Build the scanline locally, then blit it in one copy — one
            // 32-byte store per row instead of 8 volatile pixel writes.
            let mut line = [self.bg; GLYPH_WIDTH];
            for dx in 0..GLYPH_WIDTH {
                if bits & (0x80 >> dx) != 0 { line[dx] = self.fg; }
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    line.as_ptr(),
                    self.pixel_ptr(px0, py0 + dy),
                    GLYPH_WIDTH,
                );
            }
        }
    }

    /// Scroll the whole framebuffer up one text row and clear the last row.
    fn scroll(&mut self) {
        let row_bytes = GLYPH_HEIGHT as u64 * self.pitch;
        let move_bytes = (self.rows - 1) * GLYPH_HEIGHT as usize * self.pitch as usize;

        // Single memmove (overlapping regions, src above dst) — the previous
        // per-u64 volatile loop moved ~4 MB one word at a time and made every
        // scrolled line visibly slow.
        unsafe {
            core::ptr::copy(
                (self.virt + row_bytes) as *const u8,
                self.virt as *mut u8,
                move_bytes,
            );
        }

        let last_top = (self.rows - 1) * GLYPH_HEIGHT;
        self.fill_rows(last_top, last_top + GLYPH_HEIGHT, self.bg);
    }

    // ── Cursor addressing & clears (CSI support — see log.rs dispatch) ────────

    /// Clear the whole screen and home the cursor (`ESC[2J` + `ESC[H`).
    pub fn clear(&mut self) {
        if !self.is_active() { return; }
        self.fill_rows(0, self.fb_h as usize, self.bg);
        self.col = 0;
        self.row = 0;
    }

    /// Move the cursor to (row, col), 0-based, clamped to the screen.
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        if !self.is_active() { return; }
        self.row = row.min(self.rows.saturating_sub(1));
        self.col = col.min(self.cols.saturating_sub(1));
    }

    /// Move the cursor relatively (`ESC[nA/B/C/D`), clamped — no scroll.
    pub fn move_cursor(&mut self, drow: isize, dcol: isize) {
        if !self.is_active() { return; }
        self.row = (self.row as isize + drow)
            .clamp(0, self.rows.saturating_sub(1) as isize) as usize;
        self.col = (self.col as isize + dcol)
            .clamp(0, self.cols.saturating_sub(1) as isize) as usize;
    }

    /// Erase-in-line (`ESC[K`): 0 = cursor→EOL, 1 = start→cursor, 2 = whole line.
    pub fn clear_line(&mut self, mode: u16) {
        if !self.is_active() { return; }
        let (c0, c1) = match mode {
            0 => (self.col, self.cols),
            1 => (0, (self.col + 1).min(self.cols)),
            2 => (0, self.cols),
            _ => return,
        };
        let y0 = self.row * GLYPH_HEIGHT;
        self.fill_rect(c0 * GLYPH_WIDTH, c1 * GLYPH_WIDTH, y0, y0 + GLYPH_HEIGHT, self.bg);
    }

    /// Current cursor position as 1-based (row, col) — for the DSR reply.
    pub fn cursor_1based(&self) -> (usize, usize) {
        (self.row + 1, self.col + 1)
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 >= self.rows {
            self.scroll();
        } else {
            self.row += 1;
        }
    }

    pub fn put_char(&mut self, c: char) {
        if !self.is_active() { return; }
        match c {
            '\n' => self.newline(),
            '\r' => self.col = 0,
            '\t' => {
                let next = (self.col / TAB_STOP + 1) * TAB_STOP;
                while self.col < next.min(self.cols) {
                    self.put_char(' ');
                    if self.col == 0 { break; } // wrapped — tab is done
                }
            }
            '\u{8}' => {
                // Backspace: step back within the line and erase the cell.
                if self.col > 0 {
                    self.col -= 1;
                    self.draw_glyph(b' ', self.col, self.row);
                }
            }
            _ => {
                // Latin-1 glyphs render directly; common typographic
                // punctuation degrades to ASCII; everything else is '?'.
                let byte = match c {
                    '\u{2010}'..='\u{2015}' => b'-',  // hyphens/dashes
                    '\u{2018}' | '\u{2019}' => b'\'', // curly single quotes
                    '\u{201C}' | '\u{201D}' => b'"',  // curly double quotes
                    '\u{2026}'              => b'.',  // ellipsis
                    _ if (c as u32) < 256   => c as u8,
                    _                       => b'?',
                };
                self.draw_glyph(byte, self.col, self.row);
                self.col += 1;
                if self.col >= self.cols {
                    self.newline();
                }
            }
        }
    }

    pub fn put_str(&mut self, s: &str) {
        for c in s.chars() {
            self.put_char(c);
        }
    }

    /// Set the foreground colour for subsequent glyphs.  `rgb` = 0x00RRGGBB.
    pub fn set_fg(&mut self, rgb: u32) {
        self.fg = rgb & 0x00FF_FFFF;
    }

    /// Restore the default foreground colour.
    pub fn reset_fg(&mut self) {
        self.fg = FG;
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.put_str(s);
        Ok(())
    }
}

// ── Global instance ───────────────────────────────────────────────────────────

pub static CONSOLE: SpinLock<Console> = SpinLock::new(Console::inactive());

/// Lock-free "is the console bound?" flag so the unified logger can skip the
/// CONSOLE lock entirely on headless boots (serial-only, requirement of
/// QEMU -nographic / CI runs).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// `true` once `init()` has bound the console to a framebuffer.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Bind the global console to the mapped framebuffer and clear the screen.
/// Must be called after `framebuffer::init_from_limine` succeeds.
/// Returns `false` (leaving `print!` a no-op) if no framebuffer is active.
pub fn init() -> bool {
    match framebuffer::raw() {
        Some((virt, pitch, w, h)) => {
            CONSOLE.lock().init(virt, pitch, w, h);
            ACTIVE.store(true, Ordering::Release);
            true
        }
        None => false,
    }
}

/// Console geometry in character cells: `(cols, rows)`.
pub fn dimensions() -> (usize, usize) {
    let c = CONSOLE.lock();
    (c.cols, c.rows)
}

// ── print! / println! ─────────────────────────────────────────────────────────

/// Print to the framebuffer console. No-op until `console::init()`.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = $crate::console::CONSOLE.lock().write_fmt(format_args!($($arg)*));
    }};
}

/// Print to the framebuffer console with a trailing newline.
#[macro_export]
macro_rules! println {
    ()            => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::print!($($arg)*);
        $crate::print!("\n");
    }};
}
