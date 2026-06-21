//! rutils — OROS utility library.
//!
//! All commands accept fully-resolved absolute paths; callers (e.g. lysh)
//! are responsible for resolving relative paths against cwd before calling.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use lythos_rt::{
    print, println,
    file_type, TaskInfo,
    sys_close, sys_create, sys_exec, sys_mem_stat, sys_mkdir, sys_open,
    sys_read_fd, sys_readdir, sys_stat, sys_task_kill, sys_task_list,
    sys_time, sys_unlink, sys_write_fd,
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

/// Create directory at `path` (already absolute).
pub fn cmd_mkdir(path: &str) {
    match sys_mkdir(path) {
        Ok(())  => {}
        Err(()) => println!("mkdir: {}: cannot create directory", path),
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
        Ok(tid) => {
            // Block until the child task exits so the shell doesn't race for
            // serial input while an interactive program (e.g. rkilo) is running.
            let _ = lythos_rt::sys_task_wait(tid);
        }
        Err(_) => println!("exec: {}: exec failed (bad ELF?)", path),
    }
}

pub fn print_help() {
    println!("rutils commands:");
    println!("  cat <path>                      print file contents");
    println!("  cp <src> <dst>                  copy a file");
    println!("  echo [args]                     print arguments");
    println!("  exec <path>                     load and run an ELF");
    println!("  free                            print free physical memory");
    println!("  groupadd <name> [-g gid]        create a group");
    println!("  groupdel <name>                 delete a group");
    println!("  groups [user]                   print group memberships");
    println!("  id                              print uid/gid");
    println!("  kill <tid>                      terminate a task by ID");
    println!("  ls [path]                       list directory");
    println!("  mkdir <path>                    create a directory");
    println!("  ps                              list running tasks");
    println!("  rm <path>                       delete a file");
    println!("  uptime                          print time since boot");
    println!("  useradd <name> [-u uid] [-g gid] [-d home] [-s shell] [-c comment]");
    println!("  userdel <name>                  delete a user");
    println!("  whoami                          print current username");
}

// ── user / group management ───────────────────────────────────────────────────

const PASSWD: &str = "/etc/passwd";
const GROUP:  &str = "/etc/group";

fn read_text(path: &str) -> alloc::string::String {
    let fd = match sys_open(path) {
        Ok(fd) => fd,
        Err(()) => return alloc::string::String::new(),
    };
    let mut out = alloc::string::String::new();
    let mut buf = [0u8; 512];
    loop {
        match sys_read_fd(fd, &mut buf) {
            Ok(0) | Err(()) => break,
            Ok(n) => if let Ok(s) = core::str::from_utf8(&buf[..n]) { out.push_str(s); }
        }
    }
    sys_close(fd);
    out
}

fn write_text(path: &str, data: &str) -> bool {
    let _ = sys_unlink(path);
    let fd = match sys_create(path) {
        Ok(fd) => fd,
        Err(()) => return false,
    };
    let ok = sys_write_fd(fd, data.as_bytes()).is_ok();
    sys_close(fd);
    ok
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let mut n: u32 = 0;
    for c in s.chars() {
        n = n.checked_mul(10)?.checked_add(c.to_digit(10)?)?;
    }
    Some(n)
}

fn passwd_line(name: &str, uid: u32, gid: u32, gecos: &str, home: &str, shell: &str) -> alloc::string::String {
    alloc::format!("{}:x:{}:{}:{}:{}:{}\n", name, uid, gid, gecos, home, shell)
}

fn group_line(name: &str, gid: u32, members: &str) -> alloc::string::String {
    alloc::format!("{}:x:{}:{}\n", name, gid, members)
}

// ── passwd helpers ────────────────────────────────────────────────────────────

struct PwEntry {
    name:  alloc::string::String,
    uid:   u32,
    gid:   u32,
    gecos: alloc::string::String,
    home:  alloc::string::String,
    shell: alloc::string::String,
}

fn parse_passwd(text: &str) -> Vec<PwEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.splitn(7, ':');
        let name  = match f.next() { Some(s) if !s.is_empty() => s, _ => continue };
        let _x    = f.next();
        let uid   = match f.next().and_then(parse_u32) { Some(n) => n, None => continue };
        let gid   = match f.next().and_then(parse_u32) { Some(n) => n, None => continue };
        let gecos = alloc::string::String::from(f.next().unwrap_or(""));
        let home  = alloc::string::String::from(f.next().unwrap_or(""));
        let shell = alloc::string::String::from(f.next().map(|s| s.trim_end()).unwrap_or(""));
        out.push(PwEntry { name: name.into(), uid, gid, gecos, home, shell });
    }
    out
}

fn encode_passwd(entries: &[PwEntry]) -> alloc::string::String {
    let mut s = alloc::string::String::new();
    for e in entries {
        s.push_str(&passwd_line(&e.name, e.uid, e.gid, &e.gecos, &e.home, &e.shell));
    }
    s
}

// ── group helpers ─────────────────────────────────────────────────────────────

struct GrEntry {
    name:    alloc::string::String,
    gid:     u32,
    members: alloc::string::String,
}

fn parse_group(text: &str) -> Vec<GrEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.splitn(4, ':');
        let name    = match f.next() { Some(s) if !s.is_empty() => s, _ => continue };
        let _x      = f.next();
        let gid     = match f.next().and_then(parse_u32) { Some(n) => n, None => continue };
        let members = alloc::string::String::from(f.next().map(|s| s.trim_end()).unwrap_or(""));
        out.push(GrEntry { name: name.into(), gid, members });
    }
    out
}

fn encode_group(entries: &[GrEntry]) -> alloc::string::String {
    let mut s = alloc::string::String::new();
    for e in entries {
        s.push_str(&group_line(&e.name, e.gid, &e.members));
    }
    s
}

// ── commands ──────────────────────────────────────────────────────────────────

pub fn cmd_whoami() {
    // All tasks currently run as uid 0; in future the kernel will report real UIDs.
    println!("root");
}

pub fn cmd_id() {
    // Read primary group name from /etc/group for gid 0.
    let gtext = read_text(GROUP);
    let grps  = parse_group(&gtext);
    let gname = grps.iter().find(|g| g.gid == 0).map(|g| g.name.as_str()).unwrap_or("root");
    print!("uid=0(root) gid=0({}) groups=0({})", gname, gname);
    for g in &grps {
        let is_member = g.members.split(',').any(|m| m.trim() == "root");
        if is_member && g.gid != 0 { print!(",{}({})", g.gid, g.name); }
    }
    println!();
}

pub fn cmd_groups(args: &[&str]) {
    let uname = args.first().copied().unwrap_or("root");
    let ptext = read_text(PASSWD);
    let pw    = parse_passwd(&ptext);
    let user  = pw.iter().find(|e| e.name == uname);

    let gtext = read_text(GROUP);
    let grps  = parse_group(&gtext);

    let mut first = true;
    for g in &grps {
        let primary_match = user.map_or(false, |u| u.gid == g.gid);
        let member_match  = g.members.split(',').any(|m| m.trim() == uname);
        if primary_match || member_match {
            if !first { print!(" "); }
            print!("{}", g.name);
            first = false;
        }
    }
    if first { print!("{}", uname); } // fallback: just the username
    println!();
}

pub fn cmd_useradd(args: &[&str]) {
    let Some(&username) = args.first() else {
        println!("usage: useradd <name> [-u uid] [-g gid] [-d home] [-s shell] [-c comment]");
        return;
    };
    if username.is_empty() || username.contains(':') || username.contains('\n') {
        println!("useradd: invalid username '{}'", username);
        return;
    }

    let mut uid_opt:   Option<u32>  = None;
    let mut gid_opt:   Option<u32>  = None;
    let mut home_opt:  Option<&str> = None;
    let mut shell_opt: Option<&str> = None;
    let mut gecos_opt: Option<&str> = None;
    let mut i = 1usize;
    while i < args.len() {
        match args[i] {
            "-u" | "--uid"     => { i += 1; if i < args.len() { uid_opt   = parse_u32(args[i]); } }
            "-g" | "--gid"     => { i += 1; if i < args.len() { gid_opt   = parse_u32(args[i]); } }
            "-d" | "--home"    => { i += 1; if i < args.len() { home_opt  = Some(args[i]); } }
            "-s" | "--shell"   => { i += 1; if i < args.len() { shell_opt = Some(args[i]); } }
            "-c" | "--comment" => { i += 1; if i < args.len() { gecos_opt = Some(args[i]); } }
            other => { println!("useradd: unknown option '{}'", other); return; }
        }
        i += 1;
    }

    let ptext = read_text(PASSWD);
    let mut entries = parse_passwd(&ptext);

    if entries.iter().any(|e| e.name == username) {
        println!("useradd: user '{}' already exists", username);
        return;
    }

    let max_uid = entries.iter().map(|e| e.uid).max().unwrap_or(999);
    let uid     = uid_opt.unwrap_or_else(|| max_uid.max(999) + 1);
    let gid     = gid_opt.unwrap_or(uid);
    let home    = home_opt.map(alloc::string::String::from)
                          .unwrap_or_else(|| alloc::format!("/user/home/{}", username));
    let shell   = shell_opt.unwrap_or("/lth/bin/lysh");
    let gecos   = gecos_opt.unwrap_or("");

    let home_note = home.clone();
    entries.push(PwEntry {
        name: username.into(), uid, gid,
        gecos: gecos.into(), home, shell: shell.into(),
    });

    if !write_text(PASSWD, &encode_passwd(&entries)) {
        println!("useradd: cannot write {}", PASSWD);
        return;
    }

    // Create matching primary group if it doesn't exist.
    let gtext   = read_text(GROUP);
    let mut grps = parse_group(&gtext);
    if !grps.iter().any(|g| g.gid == gid || g.name == username) {
        let max_gid = grps.iter().map(|g| g.gid).max().unwrap_or(999).max(gid);
        grps.push(GrEntry { name: username.into(), gid: max_gid.max(gid), members: alloc::string::String::new() });
        let _ = write_text(GROUP, &encode_group(&grps));
    }

    // Create home directory; ignore error if it already exists.
    let _ = sys_mkdir(&home_note);

    println!("useradd: '{}' created (uid={} gid={})", username, uid, gid);
}

pub fn cmd_userdel(args: &[&str]) {
    let Some(&username) = args.first() else {
        println!("usage: userdel <name>");
        return;
    };
    if username == "root" {
        println!("userdel: cannot delete root");
        return;
    }
    let ptext   = read_text(PASSWD);
    let before  = ptext.lines().count();
    let entries: Vec<PwEntry> = parse_passwd(&ptext).into_iter()
        .filter(|e| e.name != username).collect();
    if entries.len() == before {
        println!("userdel: user '{}' not found", username);
        return;
    }
    if write_text(PASSWD, &encode_passwd(&entries)) {
        println!("userdel: '{}' removed", username);
    } else {
        println!("userdel: cannot write {}", PASSWD);
    }
}

pub fn cmd_groupadd(args: &[&str]) {
    let Some(&name) = args.first() else {
        println!("usage: groupadd <name> [-g gid]");
        return;
    };
    if name.is_empty() || name.contains(':') {
        println!("groupadd: invalid name '{}'", name);
        return;
    }
    let mut gid_opt: Option<u32> = None;
    let mut i = 1usize;
    while i < args.len() {
        match args[i] {
            "-g" | "--gid" => { i += 1; if i < args.len() { gid_opt = parse_u32(args[i]); } }
            other => { println!("groupadd: unknown option '{}'", other); return; }
        }
        i += 1;
    }
    let gtext   = read_text(GROUP);
    let mut grps = parse_group(&gtext);
    if grps.iter().any(|g| g.name == name) {
        println!("groupadd: group '{}' already exists", name);
        return;
    }
    let max_gid = grps.iter().map(|g| g.gid).max().unwrap_or(999);
    let gid     = gid_opt.unwrap_or_else(|| max_gid.max(999) + 1);
    grps.push(GrEntry { name: name.into(), gid, members: alloc::string::String::new() });
    if write_text(GROUP, &encode_group(&grps)) {
        println!("groupadd: '{}' created (gid={})", name, gid);
    } else {
        println!("groupadd: cannot write {}", GROUP);
    }
}

pub fn cmd_groupdel(args: &[&str]) {
    let Some(&name) = args.first() else {
        println!("usage: groupdel <name>");
        return;
    };
    if name == "root" {
        println!("groupdel: cannot delete root group");
        return;
    }
    let gtext   = read_text(GROUP);
    let before  = gtext.lines().count();
    let entries: Vec<GrEntry> = parse_group(&gtext).into_iter()
        .filter(|g| g.name != name).collect();
    if entries.len() == before {
        println!("groupdel: group '{}' not found", name);
        return;
    }
    if write_text(GROUP, &encode_group(&entries)) {
        println!("groupdel: '{}' removed", name);
    } else {
        println!("groupdel: cannot write {}", GROUP);
    }
}
