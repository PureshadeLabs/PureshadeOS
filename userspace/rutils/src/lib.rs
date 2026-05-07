//! rutils — OROS utility library.
//!
//! All commands accept fully-resolved absolute paths; callers (e.g. lysh)
//! are responsible for resolving relative paths against cwd before calling.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use lythos_std::{
    print, println,
    file_type, TaskInfo,
    sys_close, sys_create, sys_exec, sys_mem_stat, sys_open, sys_read_fd,
    sys_readdir, sys_stat, sys_task_kill, sys_task_list, sys_time,
    sys_unlink, sys_write_fd,
};

pub fn cmd_echo(args: &[&str]) {
    for (i, arg) in args.iter().enumerate() {
        if i > 0 { print!(" "); }
        print!("{}", arg);
    }
    println!();
}

pub fn cmd_ps() {
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

pub fn cmd_uptime() {
    let ms = sys_time();
    let secs  = ms / 1000;
    let mins  = secs / 60;
    let hours = mins / 60;
    let days  = hours / 24;

    let ms_r = ms   % 1000;
    let s_r  = secs % 60;
    let m_r  = mins % 60;
    let h_r  = hours % 24;

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

pub fn cmd_free() {
    let frames = sys_mem_stat();
    let kib = frames * 4;
    let mib = kib / 1024;
    println!("{} MiB free ({} frames, {} KiB)", mib, frames, kib);
}

pub fn cmd_kill(args: &[&str]) {
    let Some(tid_str) = args.first() else {
        println!("usage: kill <tid>");
        return;
    };
    let mut tid: u64 = 0;
    let mut valid = !tid_str.is_empty();
    for ch in tid_str.chars() {
        match ch.to_digit(10) {
            Some(d) => tid = tid.saturating_mul(10).saturating_add(d as u64),
            None    => { valid = false; break; }
        }
    }
    if !valid {
        println!("kill: '{}': invalid task ID", tid_str);
        return;
    }
    if sys_task_kill(tid) {
        println!("killed task {}", tid);
    } else {
        println!("kill: {}: no such task (or protected)", tid);
    }
}

/// List directory at `path` (already absolute).
pub fn cmd_ls(path: &str) {
    const RESET:     &str = "\x1b[0m";
    const BOLD_BLUE: &str = "\x1b[1;34m"; // directories
    const CYAN:      &str = "\x1b[36m";   // symlinks
    const LINE_COLS: usize = 80;

    let entries = match sys_readdir(path) {
        Some(e) => e,
        None    => match sys_stat(path) {
            Some(s) if !s.is_dir() => { println!("{}", path); return; }
            _                       => { println!("ls: {}: no such file or directory", path); return; }
        }
    };

    if entries.is_empty() {
        println!("(empty)");
        return;
    }

    // Build display names with type suffix, then sort dirs-first alphabetically.
    let mut items: Vec<(u8, alloc::string::String)> = entries.iter().map(|e| {
        let mut name = alloc::string::String::from(e.name());
        match e.file_type {
            file_type::DIR     => name.push('/'),
            file_type::SYMLINK => name.push('@'),
            _                  => {}
        }
        (e.file_type, name)
    }).collect();

    items.sort_unstable_by(|a, b| {
        let ad = a.0 == file_type::DIR;
        let bd = b.0 == file_type::DIR;
        match (ad, bd) {
            (true, false) => core::cmp::Ordering::Less,
            (false, true) => core::cmp::Ordering::Greater,
            _             => a.1.as_str().cmp(b.1.as_str()),
        }
    });

    // Multi-column layout: pad each name to (max_name_len + 2).
    let max_len   = items.iter().map(|(_, n)| n.len()).max().unwrap_or(1);
    let col_width = (max_len + 2).max(4);
    let cols      = (LINE_COLS / col_width).max(1);

    for (i, (ft, name)) in items.iter().enumerate() {
        let (color, reset) = match *ft {
            file_type::DIR     => (BOLD_BLUE, RESET),
            file_type::SYMLINK => (CYAN,      RESET),
            _                  => ("",        ""),
        };
        print!("{}{}{}", color, name, reset);

        let is_last_in_row = (i + 1) % cols == 0;
        let is_final       = i + 1 == items.len();
        if is_last_in_row || is_final {
            println!();
        } else {
            for _ in 0..(col_width - name.len()) { print!(" "); }
        }
    }
    println!("{} entries", items.len());
}

/// Print file at `path` (already absolute).
pub fn cmd_cat(path: &str) {
    let fd = match sys_open(path) {
        Ok(fd)  => fd,
        Err(()) => { println!("cat: {}: no such file or directory", path); return; }
    };
    let mut buf = [0u8; 512];
    loop {
        match sys_read_fd(fd, &mut buf) {
            Ok(0) | Err(()) => break,
            Ok(n) => match core::str::from_utf8(&buf[..n]) {
                Ok(s)  => print!("{}", s),
                Err(_) => { println!("\n[binary data]"); break; }
            }
        }
    }
    sys_close(fd);
}

/// Copy file from `src` to `dst` (both already absolute).
pub fn cmd_cp(src: &str, dst: &str) {
    let stat = match sys_stat(src) {
        Some(s) => s,
        None    => { println!("cp: {}: not found", src); return; }
    };
    if stat.is_dir() { println!("cp: {}: is a directory", src); return; }
    if stat.size > 64 * 1024 * 1024 { println!("cp: {}: file too large", src); return; }

    let src_fd = match sys_open(src) {
        Ok(fd)  => fd,
        Err(()) => { println!("cp: {}: cannot open", src); return; }
    };
    let dst_fd = match sys_create(dst) {
        Ok(fd)  => fd,
        Err(()) => { sys_close(src_fd); println!("cp: {}: cannot create", dst); return; }
    };

    let mut buf = [0u8; 4096];
    let mut ok = true;
    loop {
        match sys_read_fd(src_fd, &mut buf) {
            Ok(0) | Err(()) => break,
            Ok(n) => if sys_write_fd(dst_fd, &buf[..n]).is_err() {
                println!("cp: write error");
                ok = false;
                break;
            }
        }
    }
    sys_close(src_fd);
    sys_close(dst_fd);
    if !ok { let _ = sys_unlink(dst); }
}

/// Delete file at `path` (already absolute).
pub fn cmd_rm(path: &str) {
    if let Err(()) = sys_unlink(path) {
        println!("rm: {}: cannot remove", path);
    }
}

/// Load and exec ELF at `path` (already absolute).
pub fn cmd_exec(path: &str) {
    let stat = match sys_stat(path) {
        Some(s) => s,
        None    => { println!("exec: {}: not found", path); return; }
    };
    if stat.is_dir() { println!("exec: {}: is a directory", path); return; }
    if stat.size == 0 || stat.size > 64 * 1024 * 1024 {
        println!("exec: {}: file too large or empty", path);
        return;
    }

    let fd = match sys_open(path) {
        Ok(fd)  => fd,
        Err(()) => { println!("exec: {}: cannot open", path); return; }
    };

    let mut elf: Vec<u8> = alloc::vec![0u8; stat.size as usize];
    let mut off = 0usize;
    let mut chunk = [0u8; 4096];
    loop {
        match sys_read_fd(fd, &mut chunk) {
            Ok(0) | Err(()) => break,
            Ok(n) => {
                let copy_n = n.min(elf.len() - off);
                elf[off..off + copy_n].copy_from_slice(&chunk[..copy_n]);
                off += copy_n;
                if off >= elf.len() { break; }
            }
        }
    }
    sys_close(fd);

    if off < elf.len() {
        println!("exec: {}: short read ({} of {} bytes)", path, off, elf.len());
        return;
    }

    match sys_exec(&elf, &[]) {
        Ok(tid) => println!("exec: spawned task {}", tid),
        Err(_)  => println!("exec: {}: exec failed (bad ELF?)", path),
    }
}

pub fn print_help() {
    println!("rutils commands:");
    println!("  cat <path>       print file contents");
    println!("  cp <src> <dst>   copy a file");
    println!("  echo [args]      print arguments");
    println!("  exec <path>      load and run an ELF");
    println!("  free             print free physical memory");
    println!("  kill <tid>       terminate a task by ID");
    println!("  ls [path]        list directory");
    println!("  ps               list running tasks");
    println!("  rm <path>        delete a file");
    println!("  uptime           print time since boot");
}
