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
    sys_close, sys_create, sys_mem_stat, sys_mkdir, sys_open,
    sys_read_fd, sys_readdir, sys_serial_read, sys_setgid, sys_setuid,
    sys_stat, sys_task_kill, sys_task_list,
    sys_time, sys_unlink, sys_write_fd, sys_yield,
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
    cmd_exec_argv(path, &[], &[])
}

/// Like [`cmd_exec`], forwarding `caps` (handles in the caller's table) to
/// the spawned task.  lysh delegates its Memory capability per-exec to an
/// allowlist of apps (e.g. rkilo) that need SYS_BRK heap growth beyond the
/// 64 KiB bootstrap arena; generic children get no caps.
pub fn cmd_exec_with_caps(path: &str, caps: &[u64]) {
    cmd_exec_argv(path, &[], caps)
}

/// Like [`cmd_exec_with_caps`], passing command-line arguments: the child's
/// argv is `[path, args...]` (argv[0] = program path, per convention).
pub fn cmd_exec_argv(path: &str, args: &[&str], caps: &[u64]) {
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

    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(path);
    argv.extend_from_slice(args);
    match lythos_rt::sys_exec_argv(&elf, caps, &argv) {
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
    println!("  passwd [user]                   set or change a password");
    println!("  su [user]                       switch to another user (default: root)");
}

// ── user / group management ───────────────────────────────────────────────────

const PASSWD: &str = "/etc/passwd";
const GROUP:  &str = "/etc/group";
const SHADOW: &str = "/etc/shadow";

// ── SHA-256 (inline, no_std, no external crate) ───────────────────────────────

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg: Vec<u8> = Vec::with_capacity(data.len() + 64);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while (msg.len() % 64) != 56 { msg.push(0); }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i*4], block[i*4+1], block[i*4+2], block[i*4+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17)  ^ w[i-2].rotate_right(19)  ^ (w[i-2]  >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let [mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut hh] = h;
        for i in 0..64 {
            let s1    = e.rotate_right(6)  ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch    = (e & f) ^ ((!e) & g);
            let t1    = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0    = a.rotate_right(2)  ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj   = (a & b) ^ (a & c) ^ (b & c);
            let t2    = s0.wrapping_add(maj);
            hh=g; g=f; f=e; e=d.wrapping_add(t1); d=c; c=b; b=a; a=t1.wrapping_add(t2);
        }
        h[0]=h[0].wrapping_add(a); h[1]=h[1].wrapping_add(b);
        h[2]=h[2].wrapping_add(c); h[3]=h[3].wrapping_add(d);
        h[4]=h[4].wrapping_add(e); h[5]=h[5].wrapping_add(f);
        h[6]=h[6].wrapping_add(g); h[7]=h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, &word) in h.iter().enumerate() { out[i*4..i*4+4].copy_from_slice(&word.to_be_bytes()); }
    out
}

fn hex_encode(data: &[u8]) -> alloc::string::String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = alloc::string::String::with_capacity(data.len() * 2);
    for &b in data { s.push(HEX[b as usize >> 4] as char); s.push(HEX[b as usize & 0xf] as char); }
    s
}

/// Hash a password. Empty string → empty field (no password). Otherwise `$sha256$hexdigest`.
fn hash_password(pw: &str) -> alloc::string::String {
    if pw.is_empty() { return alloc::string::String::new(); }
    alloc::format!("$sha256${}", hex_encode(&sha256(pw.as_bytes())))
}

/// Read a line from serial without echoing characters (for password prompts).
pub fn read_secret_line() -> alloc::string::String {
    let mut buf = alloc::string::String::new();
    let mut byte = [0u8; 1];
    loop {
        match sys_serial_read(&mut byte) {
            Ok(0) | Err(_) => { sys_yield(); continue; }
            Ok(_) => {}
        }
        match byte[0] {
            b'\r' | b'\n' => { println!(); return buf; }
            0x7F | 0x08   => { buf.pop(); }
            0x20..=0x7E   => { buf.push(byte[0] as char); }
            _ => {}
        }
    }
}

// ── shadow helpers ────────────────────────────────────────────────────────────

fn parse_shadow(text: &str) -> Vec<(alloc::string::String, alloc::string::String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(2, ':');
        let name = match parts.next() { Some(s) if !s.is_empty() => s, _ => continue };
        let hash = parts.next().unwrap_or("").trim_end();
        out.push((alloc::string::String::from(name), alloc::string::String::from(hash)));
    }
    out
}

fn encode_shadow(entries: &[(alloc::string::String, alloc::string::String)]) -> alloc::string::String {
    let mut s = alloc::string::String::new();
    for (name, hash) in entries { s.push_str(name); s.push(':'); s.push_str(hash); s.push('\n'); }
    s
}

/// True if `user` appears in /etc/passwd.
pub fn user_exists(name: &str) -> bool {
    let text = read_text(PASSWD);
    parse_passwd(&text).iter().any(|e| e.name == name)
}

/// Look up the uid and gid for `user` from /etc/passwd.
pub fn lookup_uid_gid(user: &str) -> Option<(u32, u32)> {
    let text = read_text(PASSWD);
    parse_passwd(&text).iter()
        .find(|e| e.name == user)
        .map(|e| (e.uid, e.gid))
}

/// Verify a password against /etc/shadow.
/// Empty hash field = no password required (any input matches, including empty).
/// `*` = locked (always fails).
pub fn verify_password(user: &str, pw: &str) -> bool {
    let text = read_text(SHADOW);
    for (name, hash) in parse_shadow(&text) {
        if name != user { continue; }
        if hash.is_empty() { return true; }
        if hash == "*"     { return false; }
        if let Some(expected) = hash.strip_prefix("$sha256$") {
            return hex_encode(&sha256(pw.as_bytes())) == expected;
        }
        return false;
    }
    false
}

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

pub fn cmd_whoami(user: &str) {
    println!("{}", user);
}

pub fn cmd_id(user: &str) {
    let ptext = read_text(PASSWD);
    let pw    = parse_passwd(&ptext);
    let (uid, gid) = pw.iter()
        .find(|e| e.name == user)
        .map(|e| (e.uid, e.gid))
        .unwrap_or((0, 0));
    let gtext = read_text(GROUP);
    let grps  = parse_group(&gtext);
    let gname = grps.iter().find(|g| g.gid == gid).map(|g| g.name.as_str()).unwrap_or(user);
    print!("uid={}({}) gid={}({}) groups={}({})", uid, user, gid, gname, gid, gname);
    for g in &grps {
        let is_member = g.members.split(',').any(|m| m.trim() == user);
        if is_member && g.gid != gid { print!(",{}({})", g.gid, g.name); }
    }
    println!();
}

pub fn cmd_passwd(args: &[&str], current_user: &str) {
    let target = args.first().copied().unwrap_or(current_user);
    // Only root (uid=0) can change another user's password; others can only change their own.
    // We approximate this: non-root cannot specify a different user.
    if target != current_user && current_user != "root" {
        println!("passwd: permission denied");
        return;
    }
    // For non-root changing own password, verify current password first.
    if current_user != "root" || target == current_user {
        print!("Current password: ");
        let cur = read_secret_line();
        if !verify_password(target, &cur) {
            println!("passwd: authentication failure");
            return;
        }
    }
    print!("New password: ");
    let p1 = read_secret_line();
    print!("Retype new password: ");
    let p2 = read_secret_line();
    if p1 != p2 {
        println!("passwd: passwords do not match");
        return;
    }
    let new_hash = hash_password(&p1);
    let text = read_text(SHADOW);
    let mut entries = parse_shadow(&text);
    let mut found = false;
    for (name, hash) in entries.iter_mut() {
        if name == target { *hash = new_hash.clone(); found = true; break; }
    }
    if !found {
        entries.push((alloc::string::String::from(target), new_hash));
    }
    if write_text(SHADOW, &encode_shadow(&entries)) {
        println!("passwd: password updated for '{}'", target);
    } else {
        println!("passwd: cannot write {}", SHADOW);
    }
}

/// Authenticate as another user. Drops kernel uid/gid if successful.
/// Returns the new username on success, None on failure.
pub fn cmd_su(args: &[&str]) -> Option<alloc::string::String> {
    let target = args.first().copied().unwrap_or("root");
    let (uid, gid) = match lookup_uid_gid(target) {
        Some(pair) => pair,
        None => { println!("su: user '{}' does not exist", target); return None; }
    };
    print!("Password: ");
    let pw = read_secret_line();
    if !verify_password(target, &pw) {
        println!("su: authentication failure");
        return None;
    }
    // Set gid first (while still potentially root), then uid.
    if !sys_setgid(gid) || !sys_setuid(uid) {
        println!("su: cannot switch to '{}' — insufficient privileges (exit and log in again as root)", target);
        return None;
    }
    Some(alloc::string::String::from(target))
}

pub fn cmd_groups(args: &[&str], current_user: &str) {
    let uname = args.first().copied().unwrap_or(current_user);
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

    // Add locked shadow entry so the account exists but has no usable password.
    let stext = read_text(SHADOW);
    let mut shadow = parse_shadow(&stext);
    if !shadow.iter().any(|(n, _)| n == username) {
        shadow.push((alloc::string::String::from(username), alloc::string::String::from("*")));
        let _ = write_text(SHADOW, &encode_shadow(&shadow));
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
        // Remove shadow entry too.
        let stext = read_text(SHADOW);
        let shadow: Vec<(alloc::string::String, alloc::string::String)> =
            parse_shadow(&stext).into_iter().filter(|(n, _)| n != username).collect();
        let _ = write_text(SHADOW, &encode_shadow(&shadow));
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
