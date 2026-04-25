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
//! | Command         | Effect                          |
//! |-----------------|---------------------------------|
//! | `help`          | List all built-in commands      |
//! | `echo [args…]`  | Print arguments                 |
//! | `clear`         | Clear the terminal (ANSI)       |
//! | `exit`          | Terminate the shell task        |

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use cask_std::{print, println, sys_serial_read, sys_task_exit};

// ── Terminal constants ────────────────────────────────────────────────────────

const PROMPT:       &str = "lysh> ";
const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!();
    println!("lysh 0.1 — OROS interactive shell");
    println!("Type 'help' for available commands.");
    println!();

    loop {
        print!("{}", PROMPT);
        let line = read_line();
        if !line.is_empty() {
            dispatch(&line);
        }
    }
}

// ── Line reader ───────────────────────────────────────────────────────────────

/// Read one line of input from COM1, echoing printable characters back.
///
/// Blocks until the user presses Enter (`\r` or `\n`).
/// Handles backspace (DEL 0x7F, BS 0x08) with erase-on-terminal.
/// Control characters other than BS/DEL/CR/LF are silently ignored.
fn read_line() -> String {
    let mut buf   = String::new();
    let mut byte  = [0u8; 1];

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
                    // Erase character on terminal: move back, space over, move back
                    print!("\x08 \x08");
                }
            }
            // Printable ASCII
            0x20..=0x7E => {
                let ch = byte[0] as char;
                buf.push(ch);
                print!("{}", ch);
            }
            // Everything else (ESC sequences, other control): ignore
            _ => {}
        }
    }
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
        "help"  => cmd_help(),
        "echo"  => cmd_echo(&args),
        "clear" => print!("{}", CLEAR_SCREEN),
        "exit"  => {
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
    println!("  clear          clear the terminal screen");
    println!("  exit           exit the shell (lythd will restart it)");
    println!();
    println!("Note: external program execution is not yet available.");
    println!("      A filesystem driver is required before 'exec' can work.");
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
    cask_std::sys_log("[lysh] PANIC");
    if let Some(msg) = info.message().as_str() {
        cask_std::sys_log(": ");
        cask_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        cask_std::sys_log(" at ");
        cask_std::sys_log(loc.file());
        cask_std::sys_log("\n");
    } else {
        cask_std::sys_log("\n");
    }
    sys_task_exit()
}
