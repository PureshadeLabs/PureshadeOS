//! lysh — minimal interactive shell for OROS.
//!
//! Reads lines from COM1 serial (stdin under QEMU `-serial stdio`), parses
//! them into a command and space-separated arguments, and dispatches either
//! to built-in shell commands or to the rutils library.
//!
//! ## Built-in commands (shell state required)
//!
//! | Command        | Effect                                  |
//! |----------------|-----------------------------------------|
//! | `cd [path]`    | Change working directory (default: /)   |
//! | `clear`        | Clear the terminal (ANSI)               |
//! | `exit`         | Terminate the shell task                |
//! | `help`         | List all commands                       |
//!
//! ## rutils commands (delegated)
//!
//! cat  cp  echo  exec  free  kill  ls  ps  rm  uptime
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
    sys_serial_avail, sys_serial_read, sys_stat, sys_task_exit, sys_yield,
};

const RESET:     &str = "\x1b[0m";
const BOLD_GRN:  &str = "\x1b[1;32m";
const BOLD_BLU:  &str = "\x1b[1;34m";

const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

const BUILTINS: &[&str] = &[
    "cat", "cd", "clear", "cp", "echo", "exec", "exit", "free", "groupadd",
    "groupdel", "groups", "help", "id", "kill", "ls", "mkdir", "ps", "rm",
    "uptime", "useradd", "userdel", "whoami",
];

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!();
    println!("lysh 0.4 — OROS interactive shell");
    println!("Type 'help' for available commands.");
    println!();

    let mut history: Vec<String> = Vec::new();
    let mut cwd = String::from("/");

    loop {
        print!("{}lysh{}:{}{}{} $ ", BOLD_GRN, RESET, BOLD_BLU, cwd, RESET);
        let line = read_line(&history, &cwd);
        if !line.is_empty() {
            if history.last().map(|s| s.as_str()) != Some(line.as_str()) {
                history.push(line.clone());
            }
            dispatch(&line, &mut cwd);
        }
    }
}

// ── Path resolution ───────────────────────────────────────────────────────────

/// Resolve `path` against `cwd`, then normalize away `.` and `..` components.
fn resolve(cwd: &str, path: &str) -> String {
    let raw = if path.starts_with('/') {
        String::from(path)
    } else if cwd == "/" {
        alloc::format!("/{}", path)
    } else {
        alloc::format!("{}/{}", cwd, path)
    };
    normalize(&raw)
}

/// Collapse `.` and `..` in an absolute path.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s    => parts.push(s),
        }
    }
    if parts.is_empty() {
        return String::from("/");
    }
    let mut out = String::new();
    for p in &parts {
        out.push('/');
        out.push_str(p);
    }
    out
}

// ── Command dispatch ──────────────────────────────────────────────────────────

fn dispatch(line: &str, cwd: &mut String) {
    let mut parts = line.split_ascii_whitespace();
    let cmd = match parts.next() {
        Some(c) => c,
        None    => return,
    };
    let args: Vec<&str> = parts.collect();

    match cmd {
        "help"  => cmd_help(),
        "clear" => print!("{}", CLEAR_SCREEN),
        "exit"  => { println!("Goodbye."); sys_task_exit() }
        "cd"    => cmd_cd(args.first().copied(), cwd),

        // rutils — resolve any path args against cwd first
        "echo"   => rutils::cmd_echo(&args),
        "ps"     => rutils::cmd_ps(),
        "uptime" => rutils::cmd_uptime(),
        "free"   => rutils::cmd_free(),
        "kill"   => rutils::cmd_kill(&args),
        "whoami"   => rutils::cmd_whoami(),
        "id"       => rutils::cmd_id(),
        "groups"   => rutils::cmd_groups(&args),
        "useradd"  => rutils::cmd_useradd(&args),
        "userdel"  => rutils::cmd_userdel(&args),
        "groupadd" => rutils::cmd_groupadd(&args),
        "groupdel" => rutils::cmd_groupdel(&args),

        "ls" => {
            let path = resolve(cwd, args.first().copied().unwrap_or("."));
            rutils::cmd_ls(&path);
        }
        "cat" => match args.first() {
            Some(p) => rutils::cmd_cat(&resolve(cwd, p)),
            None    => println!("usage: cat <path>"),
        },
        "cp" => {
            if args.len() < 2 {
                println!("usage: cp <src> <dst>");
            } else {
                rutils::cmd_cp(&resolve(cwd, args[0]), &resolve(cwd, args[1]));
            }
        }
        "rm" => match args.first() {
            Some(p) => rutils::cmd_rm(&resolve(cwd, p)),
            None    => println!("usage: rm <path>"),
        },
        "mkdir" => match args.first() {
            Some(p) => rutils::cmd_mkdir(&resolve(cwd, p)),
            None    => println!("usage: mkdir <path>"),
        },
        "exec" => match args.first() {
            Some(p) => rutils::cmd_exec(&resolve(cwd, p)),
            None    => println!("usage: exec <path>"),
        },

        other => {
            // Search PATH directories for an executable matching the command.
            let mut found = false;
            for dir in &["/bin"] {
                let path = alloc::format!("{}/{}", dir, other);
                if lythos_std::sys_stat(&path).map(|s| !s.is_dir()).unwrap_or(false) {
                    rutils::cmd_exec(&path);
                    found = true;
                    break;
                }
            }
            if !found {
                println!("lysh: {}: command not found", other);
            }
        }
    }
}

fn cmd_help() {
    println!("Shell built-ins:");
    println!("  cd [path]        change working directory (default: /)");
    println!("  clear            clear the terminal screen");
    println!("  exit             exit the shell");
    println!("  help             display this help message");
    println!();
    rutils::print_help();
    println!();
    println!("Up/down arrow: scroll history.  Tab: complete command names.");
}

fn cmd_cd(arg: Option<&str>, cwd: &mut String) {
    let target = arg.unwrap_or("/");
    let resolved = resolve(cwd, target);

    match sys_stat(&resolved) {
        Some(s) if s.is_dir() => *cwd = resolved,
        Some(_) => println!("cd: {}: not a directory", target),
        None    => println!("cd: {}: no such file or directory", target),
    }
}

// ── Line reader ───────────────────────────────────────────────────────────────

fn read_line(history: &[String], cwd: &str) -> String {
    let mut buf      = String::new();
    let mut byte     = [0u8; 1];
    let mut hist_pos = history.len();

    loop {
        match sys_serial_read(&mut byte) {
            Ok(0) | Err(_) => continue,
            Ok(_) => {}
        }

        match byte[0] {
            b'\r' | b'\n' => { println!(); return buf; }

            0x7F | 0x08 => {
                if !buf.is_empty() {
                    buf.pop();
                    print!("\x08 \x08");
                }
            }

            0x1B => {
                // Yield once so the ~87µs-later '[' byte has time to arrive at
                // 115200 baud, then check. Bare ESC still has nothing → ignore.
                sys_yield();
                if !sys_serial_avail() { continue; }
                match sys_serial_read(&mut byte) {
                    Ok(1) if byte[0] == b'[' => {}
                    _ => continue,
                }
                match sys_serial_read(&mut byte) {
                    Ok(1) => {}
                    _ => continue,
                }
                match byte[0] {
                    b'A' => {
                        if hist_pos == 0 { continue; }
                        hist_pos -= 1;
                        replace_line(&buf, &history[hist_pos]);
                        buf = history[hist_pos].clone();
                    }
                    b'B' => {
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
                    _ => {}
                }
            }

            b'\t' => {
                let matches: Vec<&str> = BUILTINS
                    .iter()
                    .copied()
                    .filter(|b| b.starts_with(buf.as_str()))
                    .collect();
                match matches.len() {
                    0 => {}
                    1 => {
                        let suffix = &matches[0][buf.len()..];
                        print!("{}", suffix);
                        buf.push_str(suffix);
                    }
                    _ => {
                        println!();
                        for (i, m) in matches.iter().enumerate() {
                            if i > 0 { print!("  "); }
                            print!("{}", m);
                        }
                        println!();
                        print!("{}lysh{}:{}{}{} $ {}", BOLD_GRN, RESET, BOLD_BLU, cwd, RESET, buf);
                    }
                }
            }

            0x20..=0x7E => {
                let ch = byte[0] as char;
                buf.push(ch);
                print!("{}", ch);
                hist_pos = history.len();
            }

            _ => {}
        }
    }
}

fn replace_line(current: &str, new: &str) {
    for _ in 0..current.len() { print!("\x08"); }
    for _ in 0..current.len() { print!(" "); }
    for _ in 0..current.len() { print!("\x08"); }
    print!("{}", new);
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
