//! lysh — minimal interactive shell for OROS.
//!
//! Reads lines from COM1 serial (stdin under QEMU `-serial stdio`), parses
//! them into a command and space-separated arguments, and dispatches to a
//! small set of built-in commands.  Exec of external programs is not yet
//! available because there is no filesystem; attempting an unknown command
//! prints a clear error.
//!
//! ## Built-in commands
//!
//! | Command         | Effect                                     |
//! |-----------------|--------------------------------------------|
//! | `help`          | List all built-in commands                 |
//! | `echo [args…]`  | Print arguments                            |
//! | `ps`            | List running tasks                         |
//! | `uptime`        | Print milliseconds since boot              |
//! | `free`          | Print free physical memory                 |
//! | `kill <tid>`    | Terminate a task by ID                     |
//! | `clear`         | Clear the terminal (ANSI)                  |
//! | `exit`          | Terminate the shell task                   |
//!
//! ## Command history
//!
//! Up/down arrow keys scroll through previously entered commands.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use lythos_std::{
    print, println,
    sys_mem_stat, sys_serial_read, sys_task_exit, sys_task_kill, sys_task_list, sys_time,
    TaskInfo,
};

// ── Terminal constants ────────────────────────────────────────────────────────

const PROMPT:       &str = "lysh> ";
const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!();
    println!("lysh 0.2 — OROS interactive shell");
    println!("Type 'help' for available commands.");
    println!();

    let mut history: Vec<String> = Vec::new();

    loop {
        print!("{}", PROMPT);
        let line = read_line(&history);
        if !line.is_empty() {
            // Avoid duplicate consecutive entries.
            if history.last().map(|s| s.as_str()) != Some(line.as_str()) {
                history.push(line.clone());
            }
            dispatch(&line);
        }
    }
}

// ── Line reader ───────────────────────────────────────────────────────────────

/// Read one line of input from COM1, echoing printable characters back.
///
/// Handles:
/// - Enter (`\r`/`\n`) — submit line.
/// - Backspace (DEL 0x7F / BS 0x08) — erase last character.
/// - Up arrow (`ESC [ A`) — load previous history entry.
/// - Down arrow (`ESC [ B`) — load next history entry (or clear).
/// - Other control characters are silently ignored.
fn read_line(history: &[String]) -> String {
    let mut buf   = String::new();
    let mut byte  = [0u8; 1];
    // History position: history.len() = "not in history" (current draft).
    let mut hist_pos = history.len();

    loop {
        match sys_serial_read(&mut byte) {
            Ok(0) | Err(_) => continue,
            Ok(_) => {}
        }

        match byte[0] {
            // Enter
            b'\r' | b'\n' => {
                println!();
                return buf;
            }

            // Backspace: DEL (0x7F) or BS (0x08)
            0x7F | 0x08 => {
                if !buf.is_empty() {
                    buf.pop();
                    print!("\x08 \x08");
                }
            }

            // ESC — start of escape sequence
            0x1B => {
                // Expect '[' next.
                match sys_serial_read(&mut byte) {
                    Ok(1) if byte[0] == b'[' => {}
                    _ => continue, // not CSI — ignore
                }
                // Expect the final byte.
                match sys_serial_read(&mut byte) {
                    Ok(1) => {}
                    _ => continue,
                }
                match byte[0] {
                    b'A' => {
                        // Up arrow — move back in history.
                        if hist_pos == 0 { continue; }
                        hist_pos -= 1;
                        replace_line(&buf, &history[hist_pos]);
                        buf = history[hist_pos].clone();
                    }
                    b'B' => {
                        // Down arrow — move forward in history.
                        if hist_pos >= history.len() { continue; }
                        hist_pos += 1;
                        let new = if hist_pos < history.len() {
                            history[hist_pos].as_str()
                        } else {
                            ""
                        };
                        replace_line(&buf, new);
                        buf = String::from(new);
                    }
                    _ => {} // ignore other sequences (F-keys, etc.)
                }
            }

            // Printable ASCII
            0x20..=0x7E => {
                let ch = byte[0] as char;
                buf.push(ch);
                print!("{}", ch);
                hist_pos = history.len(); // any typing resets history cursor
            }

            _ => {}
        }
    }
}

/// Erase the current input on the terminal and print `new` in its place.
fn replace_line(current: &str, new: &str) {
    // Move cursor back over every character in `current`.
    for _ in 0..current.len() {
        print!("\x08");
    }
    // Overwrite with spaces.
    for _ in 0..current.len() {
        print!(" ");
    }
    // Move back again and print new content.
    for _ in 0..current.len() {
        print!("\x08");
    }
    print!("{}", new);
    // If new is shorter, spaces already erased the extra chars and we moved back,
    // so nothing more to do.
}

// ── Command dispatch ──────────────────────────────────────────────────────────

fn dispatch(line: &str) {
    let mut parts = line.split_ascii_whitespace();
    let cmd = match parts.next() {
        Some(c) => c,
        None    => return,
    };
    let args: Vec<&str> = parts.collect();

    match cmd {
        "help"   => cmd_help(),
        "echo"   => cmd_echo(&args),
        "ps"     => cmd_ps(),
        "uptime" => cmd_uptime(),
        "free"   => cmd_free(),
        "kill"   => cmd_kill(&args),
        "clear"  => print!("{}", CLEAR_SCREEN),
        "exit"   => {
            println!("Goodbye.");
            sys_task_exit()
        }
        other => {
            println!("lysh: {}: command not found", other);
            println!("      (exec unavailable — no filesystem yet; use 'help' to list builtins)");
        }
    }
}

fn cmd_help() {
    println!("Built-in commands:");
    println!("  help           display this help message");
    println!("  echo [args]    print arguments to the terminal");
    println!("  ps             list running tasks");
    println!("  uptime         print time since boot");
    println!("  free           print free physical memory");
    println!("  kill <tid>     terminate a task by ID");
    println!("  clear          clear the terminal screen");
    println!("  exit           exit the shell (lythd will restart it)");
    println!();
    println!("Up/down arrow keys scroll through command history.");
    println!("Note: external program execution is not yet available.");
}

fn cmd_ps() {
    let mut buf: [TaskInfo; 64] = unsafe { core::mem::zeroed() };
    let n = sys_task_list(&mut buf);
    println!("{:<6}  {:<8}  {}", "TID", "STATE", "TYPE");
    println!("------  --------  --------");
    for i in 0..n {
        let t = &buf[i];
        let state = match t.state {
            1 => "ready",
            2 => "blocked",
            _ => "?",
        };
        let kind = if t.kind == 1 { "user" } else { "kernel" };
        println!("{:<6}  {:<8}  {}", t.id, state, kind);
    }
    println!("{} task(s)", n);
}

fn cmd_uptime() {
    let ms = sys_time();
    let secs  = ms / 1000;
    let mins  = secs / 60;
    let hours = mins / 60;
    let days  = hours / 24;

    let ms_r  = ms   % 1000;
    let s_r   = secs % 60;
    let m_r   = mins % 60;
    let h_r   = hours % 24;

    if days > 0 {
        println!("up {}d {:02}h {:02}m {:02}s", days, h_r, m_r, s_r);
    } else if hours > 0 {
        println!("up {}h {:02}m {:02}s", h_r, m_r, s_r);
    } else if mins > 0 {
        println!("up {}m {:02}s", m_r, s_r);
    } else {
        println!("up {}.{:03}s", s_r, ms_r);
    }
}

fn cmd_free() {
    let frames = sys_mem_stat();
    let kib    = frames * 4;
    let mib    = kib / 1024;
    println!("{} MiB free ({} frames, {} KiB)", mib, frames, kib);
}

fn cmd_kill(args: &[&str]) {
    let Some(tid_str) = args.first() else {
        println!("usage: kill <tid>");
        return;
    };
    // Parse decimal task ID.
    let mut tid: u64 = 0;
    let mut valid = !tid_str.is_empty();
    for ch in tid_str.chars() {
        match ch.to_digit(10) {
            Some(d) => tid = tid.saturating_mul(10).saturating_add(d as u64),
            None    => { valid = false; break; }
        }
    }
    if !valid {
        println!("lysh: kill: '{}': invalid task ID", tid_str);
        return;
    }
    if sys_task_kill(tid) {
        println!("killed task {}", tid);
    } else {
        println!("lysh: kill: {}: no such task (or protected)", tid);
    }
}

fn cmd_echo(args: &[&str]) {
    for (i, arg) in args.iter().enumerate() {
        if i > 0 { print!(" "); }
        print!("{}", arg);
    }
    println!();
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    lythos_std::sys_log("[lysh] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_std::sys_log(": ");
        lythos_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        lythos_std::sys_log(" at ");
        lythos_std::sys_log(loc.file());
        lythos_std::sys_log("\n");
    } else {
        lythos_std::sys_log("\n");
    }
    sys_task_exit()
}
