//! rkilo — kilo text editor ported to OROS (lythos userspace).
//!
//! Ported from antirez/kilo (© 2016 Salvatore Sanfilippo, BSD-2-Clause).
//!
//! OROS adaptations vs. the C original:
//!   - No termios: `SYS_SERIAL_READ` is already raw, character-at-a-time.
//!   - Terminal size: ANSI CPR trick (no TIOCGWINSZ / ioctl).
//!   - Time: `SYS_TIME` returns ms since boot (5-second status = 5000 ms).
//!   - File save: `SYS_UNLINK + SYS_CREATE + SYS_WRITE` (no `ftruncate`).
//!   - No SIGWINCH (no signals in OROS).
//!   - No argv yet: rkilo prompts for a filename at startup.
//!   - No syntax highlighting (v1).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use core::fmt::Write as FmtWrite;
use lythos_std::{
    sys_close, sys_create, sys_log, sys_open, sys_read_fd, sys_serial_avail,
    sys_serial_read, sys_stat, sys_task_exit, sys_time, sys_unlink, sys_write_fd,
};

const RKILO_VERSION: &str = "0.1.0";
const TAB_STOP: usize = 8;
const QUIT_TIMES: u32 = 3;

// ── Key codes ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Char(u8),
    Enter,
    Backspace,
    Esc,
    ArrowLeft, ArrowRight, ArrowUp, ArrowDown,
    DelKey, HomeKey, EndKey,
    PageUp, PageDown,
    CtrlC, CtrlF, CtrlH, CtrlL, CtrlQ, CtrlS, CtrlU,
}

// ── Editor row ────────────────────────────────────────────────────────────────

struct Row {
    chars:  Vec<u8>,  // raw content
    render: Vec<u8>,  // tab-expanded, ready to display
}

impl Row {
    fn new(s: &[u8]) -> Self {
        let mut row = Row { chars: s.to_vec(), render: Vec::new() };
        row.update_render();
        row
    }

    fn update_render(&mut self) {
        let mut r = Vec::with_capacity(self.chars.len());
        let mut col = 0usize;
        for &b in &self.chars {
            if b == b'\t' {
                r.push(b' ');
                col += 1;
                while col % TAB_STOP != 0 { r.push(b' '); col += 1; }
            } else {
                r.push(b);
                col += 1;
            }
        }
        self.render = r;
    }

    fn cx_to_rx(&self, cx: usize) -> usize {
        let mut rx = 0usize;
        for &b in self.chars.iter().take(cx) {
            if b == b'\t' { rx += TAB_STOP - (rx % TAB_STOP); } else { rx += 1; }
        }
        rx
    }

    fn rx_to_cx(&self, rx_target: usize) -> usize {
        let mut cur_rx = 0usize;
        for (cx, &b) in self.chars.iter().enumerate() {
            if b == b'\t' { cur_rx += TAB_STOP - (cur_rx % TAB_STOP); } else { cur_rx += 1; }
            if cur_rx > rx_target { return cx; }
        }
        self.chars.len()
    }

    fn insert_char(&mut self, at: usize, c: u8) {
        let at = at.min(self.chars.len());
        self.chars.insert(at, c);
        self.update_render();
    }

    fn del_char(&mut self, at: usize) {
        if at < self.chars.len() { self.chars.remove(at); self.update_render(); }
    }

    fn append_bytes(&mut self, s: &[u8]) {
        self.chars.extend_from_slice(s);
        self.update_render();
    }
}

// ── Editor state ──────────────────────────────────────────────────────────────

struct Editor {
    cx: usize, cy: usize,           // cursor in file-space (chars)
    rx: usize,                       // rendered cursor x (after tab expansion)
    rowoff: usize, coloff: usize,   // scroll offsets
    screenrows: usize, screencols: usize,
    rows: Vec<Row>,
    dirty: bool,
    filename: Option<String>,
    statusmsg: String,
    statusmsg_time: u64,            // ms from sys_time(); 0 = no active message
    quit_times: u32,
}

impl Editor {
    fn new(screenrows: usize, screencols: usize) -> Self {
        Editor {
            cx: 0, cy: 0, rx: 0,
            rowoff: 0, coloff: 0,
            screenrows, screencols,
            rows: Vec::new(),
            dirty: false,
            filename: None,
            statusmsg: String::new(),
            statusmsg_time: 0,
            quit_times: QUIT_TIMES,
        }
    }

    fn set_status(&mut self, msg: &str) {
        self.statusmsg.clear();
        self.statusmsg.push_str(msg);
        self.statusmsg_time = sys_time();
    }

    // ── Scroll ────────────────────────────────────────────────────────────────

    fn scroll(&mut self) {
        self.rx = if self.cy < self.rows.len() {
            self.rows[self.cy].cx_to_rx(self.cx)
        } else { 0 };

        if self.cy < self.rowoff { self.rowoff = self.cy; }
        if self.cy >= self.rowoff + self.screenrows {
            self.rowoff = self.cy - self.screenrows + 1;
        }
        if self.rx < self.coloff { self.coloff = self.rx; }
        if self.rx >= self.coloff + self.screencols {
            self.coloff = self.rx - self.screencols + 1;
        }
    }

    // ── Row operations ────────────────────────────────────────────────────────

    fn insert_row(&mut self, at: usize, s: &[u8]) {
        if at > self.rows.len() { return; }
        self.rows.insert(at, Row::new(s));
        self.dirty = true;
    }

    fn del_row(&mut self, at: usize) {
        if at >= self.rows.len() { return; }
        self.rows.remove(at);
        self.dirty = true;
    }

    fn rows_to_bytes(&self) -> Vec<u8> {
        let total: usize = self.rows.iter().map(|r| r.chars.len() + 1).sum();
        let mut buf = Vec::with_capacity(total);
        for row in &self.rows {
            buf.extend_from_slice(&row.chars);
            buf.push(b'\n');
        }
        buf
    }

    // ── Character editing ─────────────────────────────────────────────────────

    fn insert_char(&mut self, c: u8) {
        if self.cy == self.rows.len() { self.insert_row(self.rows.len(), b""); }
        self.rows[self.cy].insert_char(self.cx, c);
        self.cx += 1;
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        if self.cx == 0 {
            self.insert_row(self.cy, b"");
        } else {
            let rest = self.rows[self.cy].chars[self.cx..].to_vec();
            self.rows[self.cy].chars.truncate(self.cx);
            self.rows[self.cy].update_render();
            self.insert_row(self.cy + 1, &rest);
        }
        self.cy += 1;
        self.cx = 0;
    }

    fn del_char(&mut self) {
        if self.cy == self.rows.len() || (self.cx == 0 && self.cy == 0) { return; }
        if self.cx > 0 {
            self.rows[self.cy].del_char(self.cx - 1);
            self.cx -= 1;
            self.dirty = true;
        } else {
            let prev_len = self.rows[self.cy - 1].chars.len();
            let cur = self.rows[self.cy].chars.clone();
            self.rows[self.cy - 1].append_bytes(&cur);
            self.del_row(self.cy);
            self.cy -= 1;
            self.cx = prev_len;
        }
    }

    // ── File I/O ──────────────────────────────────────────────────────────────

    fn open(&mut self, path: &str) {
        self.filename = Some(String::from(path));

        let size = sys_stat(path).map(|s| s.size as usize).unwrap_or(0);
        if size == 0 { return; } // new or empty file

        let fd = match sys_open(path) {
            Ok(fd) => fd,
            Err(()) => return,
        };

        let mut buf = alloc::vec![0u8; size];
        let mut off = 0usize;
        let mut chunk = [0u8; 4096];
        loop {
            match sys_read_fd(fd, &mut chunk) {
                Ok(0) | Err(()) => break,
                Ok(n) => {
                    let copy = n.min(buf.len().saturating_sub(off));
                    buf[off..off + copy].copy_from_slice(&chunk[..copy]);
                    off += copy;
                    if off >= buf.len() { break; }
                }
            }
        }
        sys_close(fd);
        buf.truncate(off);

        // split into rows, stripping \r\n
        let mut start = 0usize;
        for i in 0..=buf.len() {
            let at_end = i == buf.len();
            if at_end || buf[i] == b'\n' {
                if at_end && start >= i { break; } // no trailing empty row
                let line = &buf[start..i];
                let line = if line.last() == Some(&b'\r') { &line[..line.len() - 1] } else { line };
                self.rows.push(Row::new(line));
                start = i + 1;
            }
        }
        self.dirty = false;
    }

    fn save(&mut self) -> Result<usize, &'static str> {
        if self.filename.is_none() {
            if let Some(name) = self.prompt("Save as: ") {
                self.filename = Some(name);
            } else {
                return Err("save cancelled");
            }
        }
        let path = self.filename.as_ref().unwrap().clone();
        let content = self.rows_to_bytes();
        let len = content.len();

        let _ = sys_unlink(&path); // remove old file (ignore error — may not exist)
        let fd = sys_create(&path).map_err(|_| "create failed")?;
        let mut written = 0usize;
        while written < content.len() {
            match sys_write_fd(fd, &content[written..]) {
                Ok(0) | Err(()) => { sys_close(fd); return Err("write error"); }
                Ok(n) => written += n,
            }
        }
        sys_close(fd);
        self.dirty = false;
        Ok(len)
    }

    // ── Prompt (status-bar interactive input) ─────────────────────────────────

    fn prompt(&mut self, prompt_str: &str) -> Option<String> {
        let mut input = String::new();
        loop {
            let mut msg = String::from(prompt_str);
            msg.push_str(&input);
            self.set_status(&msg);
            self.refresh_screen();

            match read_key() {
                Key::Enter => {
                    self.set_status("");
                    return if input.is_empty() { None } else { Some(input) };
                }
                Key::Esc | Key::CtrlC => {
                    self.set_status("");
                    return None;
                }
                Key::Backspace | Key::CtrlH => { input.pop(); }
                Key::Char(c) if c >= 0x20 && c < 0x7f => { input.push(c as char); }
                _ => {}
            }
        }
    }

    // ── Find ──────────────────────────────────────────────────────────────────

    fn find(&mut self) {
        let saved_cx     = self.cx;
        let saved_cy     = self.cy;
        let saved_coloff = self.coloff;
        let saved_rowoff = self.rowoff;

        let mut query = String::new();
        let mut last_match: Option<usize> = None;
        let mut direction: i32 = 1;

        loop {
            let mut msg = String::from("Search: ");
            msg.push_str(&query);
            msg.push_str("  (ESC/Enter cancel | Arrows next/prev)");
            self.set_status(&msg);
            self.refresh_screen();

            let key = read_key();
            match key {
                Key::Backspace | Key::CtrlH => {
                    query.pop();
                    last_match = None;
                }
                Key::Esc | Key::Enter => {
                    if key == Key::Esc {
                        self.cx = saved_cx;
                        self.cy = saved_cy;
                        self.coloff = saved_coloff;
                        self.rowoff = saved_rowoff;
                    }
                    self.set_status("");
                    return;
                }
                Key::ArrowRight | Key::ArrowDown => direction = 1,
                Key::ArrowLeft  | Key::ArrowUp   => direction = -1,
                Key::Char(c) if c >= 0x20 && c < 0x7f => {
                    query.push(c as char);
                    last_match = None;
                }
                _ => {}
            }

            let nrows = self.rows.len();
            if nrows == 0 || query.is_empty() { continue; }
            if last_match.is_none() { direction = 1; }

            let start = last_match.unwrap_or(if direction > 0 { nrows - 1 } else { 0 });
            let mut current = start;

            'search: for _ in 0..nrows {
                if direction > 0 {
                    current = (current + 1) % nrows;
                } else {
                    current = if current == 0 { nrows - 1 } else { current - 1 };
                }
                let render_bytes = &self.rows[current].render;
                // search as bytes (handles non-UTF-8 files gracefully)
                if let Some(pos) = find_bytes(render_bytes, query.as_bytes()) {
                    last_match = Some(current);
                    self.cy = current;
                    self.cx = self.rows[current].rx_to_cx(pos);
                    self.rowoff = nrows; // force scroll: match will land near top
                    break 'search;
                }
            }
        }
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

    fn move_cursor(&mut self, key: Key) {
        let row_len = if self.cy < self.rows.len() { self.rows[self.cy].chars.len() } else { 0 };

        match key {
            Key::ArrowLeft => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.rows[self.cy].chars.len();
                }
            }
            Key::ArrowRight => {
                if self.cx < row_len {
                    self.cx += 1;
                } else if self.cy < self.rows.len() {
                    self.cy += 1;
                    self.cx = 0;
                }
            }
            Key::ArrowUp   => { if self.cy > 0 { self.cy -= 1; } }
            Key::ArrowDown => { if self.cy < self.rows.len() { self.cy += 1; } }
            _ => {}
        }

        // clamp cx to new row's length
        let row_len = if self.cy < self.rows.len() { self.rows[self.cy].chars.len() } else { 0 };
        if self.cx > row_len { self.cx = row_len; }
    }

    // ── Screen refresh ────────────────────────────────────────────────────────

    fn refresh_screen(&mut self) {
        self.scroll();

        let mut buf = String::new();
        buf.push_str("\x1b[?25l"); // hide cursor
        buf.push_str("\x1b[H");    // cursor to home

        // ── Content rows ──────────────────────────────────────────────────────
        for y in 0..self.screenrows {
            let filerow = self.rowoff + y;
            if filerow >= self.rows.len() {
                if self.rows.is_empty() && y == self.screenrows / 3 {
                    let mut welcome = String::from("rkilo v");
                    welcome.push_str(RKILO_VERSION);
                    welcome.push_str(" -- Ctrl-S save | Ctrl-Q quit | Ctrl-F find");
                    let wlen = welcome.len().min(self.screencols);
                    let pad  = self.screencols.saturating_sub(wlen) / 2;
                    if pad > 0 {
                        buf.push('~');
                        for _ in 0..pad - 1 { buf.push(' '); }
                    }
                    buf.push_str(&welcome[..wlen]);
                } else {
                    buf.push('~');
                }
            } else {
                let render = &self.rows[filerow].render;
                let start  = self.coloff.min(render.len());
                // render visible slice, replacing non-printable bytes with '?'
                for &b in render[start..].iter().take(self.screencols) {
                    buf.push(if b >= 0x20 && b < 0x7f { b as char } else { '?' });
                }
            }
            buf.push_str("\x1b[0K\r\n"); // erase to end-of-line, newline
        }

        // ── Status bar (reverse video) ─────────────────────────────────────────
        buf.push_str("\x1b[0K\x1b[7m");
        let fname = self.filename.as_deref().unwrap_or("[No Name]");
        let dirty = if self.dirty { " (modified)" } else { "" };

        let mut left = String::new();
        // truncate filename at 20 chars (safe: filenames are ASCII)
        let fname_cut = fname.len().min(20);
        let _ = write!(left, "{} - {} lines{}", &fname[..fname_cut], self.rows.len(), dirty);

        let mut right = String::new();
        let _ = write!(right, "{}/{}", self.cy + 1, self.rows.len());

        let left_cut = left.len().min(self.screencols);
        buf.push_str(&left[..left_cut]);

        let mut len = left_cut;
        while len < self.screencols {
            if self.screencols - len == right.len() {
                buf.push_str(&right);
                break;
            }
            buf.push(' ');
            len += 1;
        }
        buf.push_str("\x1b[0m\r\n");

        // ── Status message ─────────────────────────────────────────────────────
        buf.push_str("\x1b[0K");
        if !self.statusmsg.is_empty() {
            let age = sys_time().saturating_sub(self.statusmsg_time);
            if age < 5000 {
                let cut = self.statusmsg.len().min(self.screencols);
                buf.push_str(&self.statusmsg[..cut]);
            }
        }

        // ── Cursor position ────────────────────────────────────────────────────
        let cursor_row = (self.cy - self.rowoff) + 1;
        let cursor_col = (self.rx - self.coloff) + 1;
        let _ = write!(buf, "\x1b[{};{}H", cursor_row, cursor_col);
        buf.push_str("\x1b[?25h"); // show cursor

        sys_log(&buf);
    }

    // ── Key processing ────────────────────────────────────────────────────────

    fn process_keypress(&mut self) {
        let key = read_key();

        match key {
            Key::Enter => self.insert_newline(),

            Key::CtrlQ => {
                if self.dirty && self.quit_times > 0 {
                    let n = self.quit_times;
                    let mut msg = String::from("Unsaved changes! Ctrl-Q ");
                    let _ = write!(msg, "{}", n);
                    msg.push_str(" more time(s) to quit.");
                    self.set_status(&msg);
                    self.quit_times -= 1;
                    return;
                }
                sys_log("\x1b[2J\x1b[H");
                sys_task_exit();
            }

            Key::CtrlS => {
                match self.save() {
                    Ok(n) => {
                        let mut msg = String::new();
                        let _ = write!(msg, "{} bytes written to disk.", n);
                        self.set_status(&msg);
                    }
                    Err(e) => self.set_status(e),
                }
            }

            Key::CtrlF => self.find(),

            Key::Backspace | Key::CtrlH => self.del_char(),

            Key::DelKey => {
                self.move_cursor(Key::ArrowRight);
                self.del_char();
            }

            Key::PageUp | Key::PageDown => {
                if key == Key::PageUp {
                    self.cy = self.rowoff;
                } else {
                    self.cy = (self.rowoff + self.screenrows - 1).min(self.rows.len());
                }
                let times = self.screenrows;
                let dir = if key == Key::PageUp { Key::ArrowUp } else { Key::ArrowDown };
                for _ in 0..times { self.move_cursor(dir); }
            }

            Key::HomeKey => self.cx = 0,
            Key::EndKey => {
                if self.cy < self.rows.len() {
                    self.cx = self.rows[self.cy].chars.len();
                }
            }

            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight => {
                self.move_cursor(key);
            }

            Key::CtrlL | Key::Esc => {} // no-op; screen refreshes anyway

            Key::Char(c) => self.insert_char(c),

            _ => {} // CtrlC, CtrlU, CtrlD — ignored in v1
        }

        self.quit_times = QUIT_TIMES; // reset on any non-quit key
    }
}

// ── Byte search helper ────────────────────────────────────────────────────────

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ── Terminal primitives ───────────────────────────────────────────────────────

fn serial_read_byte() -> u8 {
    loop {
        let mut b = [0u8; 1];
        if sys_serial_read(&mut b).unwrap_or(0) > 0 { return b[0]; }
    }
}

fn read_key() -> Key {
    let c = serial_read_byte();
    match c {
        3  => Key::CtrlC,
        6  => Key::CtrlF,
        8  => Key::CtrlH,
        12 => Key::CtrlL,
        13 => Key::Enter,
        17 => Key::CtrlQ,
        19 => Key::CtrlS,
        21 => Key::CtrlU,
        27 => {
            // Check UART FIFO non-destructively before blocking.
            // Arrow/function keys arrive as a burst so the '[' is already there;
            // a plain ESC has nothing following it yet.
            if !sys_serial_avail() { return Key::Esc; }
            let mut seq = [0u8; 3];
            let n = sys_serial_read(&mut seq[..2]).unwrap_or(0);
            if n == 0 { return Key::Esc; }

            if seq[0] == b'[' {
                let b1 = if n >= 2 { seq[1] } else { serial_read_byte() };
                if b1 >= b'0' && b1 <= b'9' {
                    // extended: \x1b[N~
                    let b2 = serial_read_byte();
                    if b2 == b'~' {
                        return match b1 {
                            b'1' | b'7' => Key::HomeKey,
                            b'3'        => Key::DelKey,
                            b'4' | b'8' => Key::EndKey,
                            b'5'        => Key::PageUp,
                            b'6'        => Key::PageDown,
                            _           => Key::Esc,
                        };
                    }
                } else {
                    return match b1 {
                        b'A' => Key::ArrowUp,
                        b'B' => Key::ArrowDown,
                        b'C' => Key::ArrowRight,
                        b'D' => Key::ArrowLeft,
                        b'H' => Key::HomeKey,
                        b'F' => Key::EndKey,
                        _    => Key::Esc,
                    };
                }
            } else if seq[0] == b'O' {
                let b1 = if n >= 2 { seq[1] } else { serial_read_byte() };
                return match b1 {
                    b'H' => Key::HomeKey,
                    b'F' => Key::EndKey,
                    _    => Key::Esc,
                };
            }
            Key::Esc
        }
        127 => Key::Backspace,
        b   => Key::Char(b),
    }
}

// Send the ANSI CPR sequence and parse the response.
fn get_cursor_pos() -> Option<(usize, usize)> {
    sys_log("\x1b[6n");
    let mut buf = [0u8; 32];
    let mut len = 0usize;
    loop {
        let mut b = [0u8; 1];
        if sys_serial_read(&mut b).unwrap_or(0) == 0 { break; }
        buf[len] = b[0];
        len += 1;
        if b[0] == b'R' || len >= buf.len() { break; }
    }
    if len < 6 || buf[0] != b'\x1b' || buf[1] != b'[' { return None; }
    let s = core::str::from_utf8(&buf[2..len - 1]).ok()?;
    let mut it = s.splitn(2, ';');
    let rows: usize = it.next()?.parse().ok()?;
    let cols: usize = it.next()?.parse().ok()?;
    Some((rows, cols))
}

fn get_window_size() -> (usize, usize) {
    // Move cursor to bottom-right, then query position.
    sys_log("\x1b[999C\x1b[999B");
    get_cursor_pos().unwrap_or((24, 80))
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let (rows, cols) = get_window_size();

    // rows - 2: reserve 1 row for status bar + 1 for status message
    let mut e = Editor::new(rows.saturating_sub(2).max(1), cols.max(10));

    // OROS has no argv yet (ELF loader writes argc=0).
    // Prompt the user for a filename to open or create.
    sys_log("\x1b[2J\x1b[H"); // clear screen before prompt
    if let Some(filename) = e.prompt("Open/create file: ") {
        e.open(&filename);
    }
    // if user pressed ESC/Enter with no filename, start with an empty buffer

    e.set_status("Ctrl-S save | Ctrl-Q quit | Ctrl-F find");

    loop {
        e.refresh_screen();
        e.process_keypress();
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    sys_log("\x1b[2J\x1b[H"); // clear screen
    sys_log("[rkilo] PANIC");
    if let Some(msg) = info.message().as_str() {
        sys_log(": ");
        sys_log(msg);
    }
    if let Some(loc) = info.location() {
        sys_log(" at ");
        sys_log(loc.file());
        sys_log(":");
        let mut tmp = [0u8; 10];
        let mut n = 0usize;
        let mut v = loc.line();
        if v == 0 { tmp[0] = b'0'; n = 1; } else {
            while v > 0 { tmp[n] = b'0' + (v % 10) as u8; n += 1; v /= 10; }
            tmp[..n].reverse();
        }
        if let Ok(s) = core::str::from_utf8(&tmp[..n]) { sys_log(s); }
    }
    sys_log("\n");
    sys_task_exit()
}
