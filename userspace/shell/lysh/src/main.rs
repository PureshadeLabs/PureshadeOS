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
use lythos_rt::{
    print, println,
    pipe_capture_start, pipe_capture_end, pipe_stdin_set, pipe_stdin_clear,
    pipe_stdin_active, pipe_stdin_read_all,
    sys_serial_avail, sys_serial_read, sys_stat, sys_task_exit, sys_yield,
    sys_open, sys_read_fd, sys_close, sys_create, sys_write_fd, sys_unlink,
};

const RESET:     &str = "\x1b[0m";
const BOLD_GRN:  &str = "\x1b[1;32m";
const BOLD_BLU:  &str = "\x1b[1;34m";

const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

const BUILTINS: &[&str] = &[
    "cat", "cd", "clear", "cp", "echo", "exec", "exit", "free", "groupadd",
    "groupdel", "groups", "help", "id", "kill", "ls", "mkdir", "poweroff", "ps", "rm",
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

/// Parse `>` and `<` redirection operators out of a command line.
///
/// Returns `(cleaned_cmd, stdin_file, stdout_file)` where `cleaned_cmd` has
/// the redirect tokens removed.  Both file names are path strings; `None`
/// means no redirect for that direction.
fn parse_redirects(line: &str) -> (String, Option<String>, Option<String>) {
    let mut cmd_parts: Vec<&str> = Vec::new();
    let mut stdin_file:  Option<String> = None;
    let mut stdout_file: Option<String> = None;

    let mut tokens = line.split_ascii_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        if tok == ">" {
            stdout_file = tokens.next().map(String::from);
        } else if tok == "<" {
            stdin_file = tokens.next().map(String::from);
        } else if let Some(path) = tok.strip_prefix('>') {
            if path.is_empty() {
                stdout_file = tokens.next().map(String::from);
            } else {
                stdout_file = Some(String::from(path));
            }
        } else if let Some(path) = tok.strip_prefix('<') {
            if path.is_empty() {
                stdin_file = tokens.next().map(String::from);
            } else {
                stdin_file = Some(String::from(path));
            }
        } else {
            cmd_parts.push(tok);
        }
    }

    let mut cleaned = String::new();
    for (i, p) in cmd_parts.iter().enumerate() {
        if i > 0 { cleaned.push(' '); }
        cleaned.push_str(p);
    }
    (cleaned, stdin_file, stdout_file)
}

/// Run a (possibly piped) command line.
///
/// Splits on `|`, runs each stage left-to-right: every stage except the last
/// has its stdout captured; the captured bytes become the next stage's piped
/// stdin.  The final stage runs normally (output goes to serial).
fn dispatch(line: &str, cwd: &mut String) {
    let stages: Vec<&str> = line.split('|').collect();
    if stages.len() == 1 {
        dispatch_single(line.trim(), cwd);
        return;
    }
    let mut piped: Option<alloc::vec::Vec<u8>> = None;
    for (i, stage) in stages.iter().enumerate() {
        let is_last = i + 1 == stages.len();
        if let Some(ref data) = piped {
            pipe_stdin_set(data);
        }
        if is_last {
            dispatch_single(stage.trim(), cwd);
            pipe_stdin_clear();
        } else {
            pipe_capture_start();
            dispatch_single(stage.trim(), cwd);
            pipe_stdin_clear();
            piped = Some(pipe_capture_end());
        }
    }
}

fn dispatch_single(line: &str, cwd: &mut String) {
    // Parse I/O redirections first.
    let (cleaned, stdin_redir, stdout_redir) = parse_redirects(line);
    let line = cleaned.as_str();

    // Set up stdin redirect: read file into pipe buffer.
    let mut stdin_from_file = false;
    if let Some(ref path) = stdin_redir {
        let resolved = resolve(cwd, path);
        match sys_open(&resolved) {
            Ok(fd) => {
                let mut data: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                let mut tmp = [0u8; 512];
                loop {
                    match sys_read_fd(fd, &mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n)          => data.extend_from_slice(&tmp[..n]),
                    }
                }
                sys_close(fd);
                pipe_stdin_set(&data);
                stdin_from_file = true;
            }
            Err(_) => {
                println!("lysh: {}: no such file", path);
                return;
            }
        }
    }

    // Capture stdout if output redirect requested.
    let capture_stdout = stdout_redir.is_some();
    if capture_stdout { pipe_capture_start(); }

    // Dispatch the command.
    let mut parts = line.split_ascii_whitespace();
    let cmd = match parts.next() {
        Some(c) => c,
        None    => {
            if capture_stdout { let _ = pipe_capture_end(); }
            if stdin_from_file { pipe_stdin_clear(); }
            return;
        }
    };
    let args: Vec<&str> = parts.collect();

    match cmd {
        "help"  => cmd_help(),
        "clear" => print!("{}", CLEAR_SCREEN),
        "exit"     => { println!("Goodbye."); sys_task_exit() }
        "poweroff" => { println!("Shutting down..."); lythos_rt::sys_poweroff() }
        "cd"    => cmd_cd(args.first().copied(), cwd),
        "pwd"   => println!("{}", cwd),

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
        "cat" => {
            if pipe_stdin_active() {
                print!("{}", pipe_stdin_read_all());
            } else {
                match args.first() {
                    Some(p) => rutils::cmd_cat(&resolve(cwd, p)),
                    None    => println!("usage: cat <path>"),
                }
            }
        }
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
            for dir in &["/lth/bin", "/bin", "/sbin"] {
                let path = alloc::format!("{}/{}", dir, other);
                if lythos_rt::sys_stat(&path).map(|s| !s.is_dir()).unwrap_or(false) {
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

    // Flush captured output to file if `>` was specified.
    if capture_stdout {
        let data = pipe_capture_end();
        if let Some(ref path) = stdout_redir {
            let resolved = resolve(cwd, path);
            let _ = sys_unlink(&resolved); // truncate if exists
            match sys_create(&resolved) {
                Ok(fd) => {
                    let _ = sys_write_fd(fd, &data);
                    sys_close(fd);
                }
                Err(_) => println!("lysh: {}: cannot create", path),
            }
        }
    }

    // Clear stdin redirect if we set it.
    if stdin_from_file { pipe_stdin_clear(); }
}

fn cmd_help() {
    println!("Shell built-ins:");
    println!("  cd [path]        change working directory (default: /)");
    println!("  clear            clear the terminal screen");
    println!("  exit             exit the shell");
    println!("  help             display this help message");
    println!("  pwd              print working directory");
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
    lythos_rt::sys_log("[lysh] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_rt::sys_log(": ");
        lythos_rt::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        lythos_rt::sys_log(" at ");
        lythos_rt::sys_log(loc.file());
        lythos_rt::sys_log("\n");
    } else {
        lythos_rt::sys_log("\n");
    }
    sys_task_exit()
}
