//! lythd — PID 1 init process for OROS (Open Runtime Operating System).
//!
//! ## Boot sequence
//!
//! 1. Receive the 64-byte `BootInfo` message on cap handle 2 (pre-queued by
//!    the kernel before `exec`).
//! 2. Print system info.
//! 3. Create the **service registry** IPC endpoint.
//! 4. Spawn core services (lythdist, lysh) from /bin/ on the RFS disk.
//! 5. Enter the **supervisor loop**: handle `Register` / `Lookup` requests
//!    and check service health on each iteration.
//!
//! ## Capability handles at entry
//!
//! | Handle | Kind     | Contents                                   |
//! |--------|----------|--------------------------------------------|
//! | 0      | Memory   | Root memory cap — all free physical frames |
//! | 1      | Rollback | `SYS_ROLLBACK` gate                        |
//! | 2      | Ipc      | Boot-info endpoint — one pre-queued msg    |
//!
//! ## Health monitoring
//!
//! lythd tracks every spawned service as a `ManagedSvc`.  After each registry
//! message it calls `check_and_restart` on all services.  If a service has
//! exited unexpectedly it is restarted by reloading its ELF from disk (up to
//! 3 times) before being marked `Failed`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lythos_std::{
    BootInfo,
    ipc::Endpoint,
    println, eprintln,
    sys_close, sys_open, sys_read_fd, sys_rollback, sys_stat, sys_task_exit,
    task::{yield_now, TaskId, TaskStatus},
};

// ── Capability handle constants ───────────────────────────────────────────────

const MEM_CAP:       u64 = 0;
const _ROLLBACK_CAP: u64 = 1;
const BOOT_INFO_CAP: u64 = 2;

// ── Disk ELF loader ───────────────────────────────────────────────────────────

/// Read an entire ELF from `path` on the RFS filesystem into a `Vec<u8>`.
fn load_elf(path: &str) -> Option<Vec<u8>> {
    let stat = sys_stat(path)?;
    let size = stat.size as usize;
    if size == 0 || size > 32 * 1024 * 1024 {
        eprintln!("[lythd] load_elf: {} bad size {}", path, size);
        return None;
    }
    let fd = match sys_open(path) {
        Ok(fd)  => fd,
        Err(()) => { eprintln!("[lythd] load_elf: cannot open {}", path); return None; }
    };
    let mut buf = alloc::vec![0u8; size];
    let mut off = 0usize;
    let mut chunk = [0u8; 4096];
    loop {
        match sys_read_fd(fd, &mut chunk) {
            Ok(0) | Err(()) => break,
            Ok(n) => {
                let copy_n = n.min(buf.len() - off);
                buf[off..off + copy_n].copy_from_slice(&chunk[..copy_n]);
                off += copy_n;
                if off >= buf.len() { break; }
            }
        }
    }
    sys_close(fd);
    if off < size {
        eprintln!("[lythd] load_elf: short read on {} ({}/{})", path, off, size);
        return None;
    }
    Some(buf)
}

fn spawn_from_disk(path: &str, caps: &[u64]) -> Option<TaskId> {
    let elf = load_elf(path)?;
    lythos_std::task::spawn(&elf, caps).ok()
}

// ── Service registry protocol ─────────────────────────────────────────────────
//
// All messages are exactly 64 bytes.
//
// | Byte(s) | Field    | Meaning                               |
// |---------|----------|---------------------------------------|
// | 0       | kind     | 0=Register  1=Lookup  2=Ack  3=Nack   |
// | 1       | name_len | length of service name (≤32)          |
// | 2..34   | name     | service name, ASCII, null-padded      |
// | 34..42  | task_id  | spawned TaskId  (Register only)       |
// | 42..50  | cap      | IPC cap handle for the service        |
// | 50..64  | _pad     | reserved, zero                        |

const KIND_REGISTER: u8 = 0;
const KIND_LOOKUP:   u8 = 1;
const KIND_ACK:      u8 = 2;
const KIND_NACK:     u8 = 3;

// ── Service registry table entry ──────────────────────────────────────────────

struct Service {
    name:    [u8; 32],
    task_id: u64,
    cap:     u64,
}

impl Service {
    fn name_str(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..n]).unwrap_or("<invalid>")
    }
}

// ── Health monitoring ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceState {
    Running,
    Restarting(u8),
    Failed,
}

/// A service spawned and supervised by lythd.
struct ManagedSvc {
    name:      &'static str,
    /// Disk path to load the ELF from on spawn and respawn.
    path:      &'static str,
    task_id:   TaskId,
    state:     ServiceState,
    caps:      [u64; 8],
    cap_count: usize,
}

impl ManagedSvc {
    fn new(name: &'static str, path: &'static str, task_id: TaskId, caps: &[u64]) -> Self {
        let mut caps_buf = [0u64; 8];
        let cap_count = caps.len().min(8);
        caps_buf[..cap_count].copy_from_slice(&caps[..cap_count]);
        ManagedSvc { name, path, task_id, state: ServiceState::Running, caps: caps_buf, cap_count }
    }

    fn check_and_restart(&mut self) {
        if self.state == ServiceState::Failed { return; }
        if lythos_std::task::task_status(self.task_id) != TaskStatus::Dead { return; }

        let attempts = match self.state {
            ServiceState::Running        => 0,
            ServiceState::Restarting(n) => n,
            ServiceState::Failed         => return,
        };

        if attempts >= 3 {
            eprintln!("[lythd] {} failed permanently after {} restart(s)", self.name, attempts);
            self.state = ServiceState::Failed;
            return;
        }

        eprintln!("[lythd] {} died — restart {}/3", self.name, attempts + 1);

        match spawn_from_disk(self.path, &self.caps[..self.cap_count]) {
            Some(new_id) => {
                self.task_id = new_id;
                self.state   = ServiceState::Restarting(attempts + 1);
                println!("[lythd] {} restarted as task {}", self.name, new_id);
            }
            None => {
                eprintln!("[lythd] {} respawn failed (could not load {})", self.name, self.path);
                self.state = ServiceState::Failed;
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // ── 1. Consume the boot-info message ─────────────────────────────────
    let boot_ep = Endpoint::from_raw(BOOT_INFO_CAP);
    let frame   = boot_ep.recv_frame().expect("lythd: boot-info recv failed");
    let info    = BootInfo::from_bytes(&frame).expect("lythd: boot-info sig mismatch");

    let mem_mib = { info.mem_bytes   } / (1024 * 1024);
    let frames  = { info.free_frames };
    println!("[lythd] lythos init — {} MiB free ({} frames), cpu: {}",
             mem_mib, frames, info.vendor_str());

    // ── 2. Create the service registry endpoint ───────────────────────────
    let registry = Endpoint::create().expect("lythd: registry endpoint alloc failed");
    println!("[lythd] service registry online (cap {})", registry.as_raw());

    // ── 3. Spawn lythdist (capability distributor) ────────────────────────
    let dist_req_ep  = Endpoint::create().expect("lythd: dist req endpoint alloc failed");
    let dist_rsp_ep  = Endpoint::create().expect("lythd: dist rsp endpoint alloc failed");
    let lythdist_caps = [MEM_CAP, dist_req_ep.as_raw(), dist_rsp_ep.as_raw(), registry.as_raw()];

    let lythdist_task = spawn_from_disk("/bin/lythdist", &lythdist_caps)
        .expect("lythd: /bin/lythdist spawn failed — is it in rootfs/bin/?");
    println!("[lythd] lythdist spawned (task {})", lythdist_task);

    // ── 4. Spawn lysh (interactive shell) ────────────────────────────────
    let lysh_task = spawn_from_disk("/bin/lysh", &[])
        .expect("lythd: /bin/lysh spawn failed — is it in rootfs/bin/?");
    println!("[lythd] lysh spawned (task {})", lysh_task);

    // ── 5. Supervisor loop ────────────────────────────────────────────────
    println!("[lythd] entering supervisor loop");

    let mut services: Vec<Service> = Vec::new();
    let mut managed = [
        ManagedSvc::new("lythdist", "/bin/lythdist", lythdist_task, &lythdist_caps),
        ManagedSvc::new("lysh",     "/bin/lysh",     lysh_task,     &[]),
    ];

    loop {
        for svc in managed.iter_mut() {
            svc.check_and_restart();
        }

        let frame = match registry.recv_frame() {
            Ok(f)  => f,
            Err(e) => {
                eprintln!("[lythd] registry recv error: {:?}", e);
                yield_now();
                continue;
            }
        };

        match frame[0] {
            KIND_REGISTER => {
                let name_len = (frame[1] as usize).min(32);
                let mut name = [0u8; 32];
                name[..name_len].copy_from_slice(&frame[2..2 + name_len]);
                let task_id = u64::from_le_bytes(frame[34..42].try_into().unwrap_or([0; 8]));
                let cap     = u64::from_le_bytes(frame[42..50].try_into().unwrap_or([0; 8]));
                let svc = Service { name, task_id, cap };
                println!("[lythd] registered '{}' task={} cap={}", svc.name_str(), task_id, cap);
                services.push(svc);
            }

            KIND_LOOKUP => {
                let name_len = (frame[1] as usize).min(32);
                let query    = core::str::from_utf8(&frame[2..2 + name_len]).unwrap_or("");
                match services.iter().find(|s| s.name_str() == query) {
                    Some(svc) => {
                        let mut ack = [0u8; 64];
                        ack[0] = KIND_ACK;
                        ack[34..42].copy_from_slice(&svc.task_id.to_le_bytes());
                        ack[42..50].copy_from_slice(&svc.cap.to_le_bytes());
                        let _ = registry.send(&ack);
                    }
                    None => {
                        let mut nack = [0u8; 64];
                        nack[0] = KIND_NACK;
                        let _ = registry.send(&nack);
                    }
                }
            }

            kind => eprintln!("[lythd] unknown registry msg kind={}", kind),
        }
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    lythos_std::sys_log("[lythd] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_std::sys_log(": ");
        lythos_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        lythos_std::sys_log(" at ");
        lythos_std::sys_log(loc.file());
        lythos_std::sys_log(":");
        let line = loc.line();
        let mut buf = [0u8; 10];
        let mut n = 0usize;
        let mut v = line;
        if v == 0 { buf[0] = b'0'; n = 1; } else {
            while v > 0 { buf[n] = b'0' + (v % 10) as u8; n += 1; v /= 10; }
            buf[..n].reverse();
        }
        if let Ok(s) = core::str::from_utf8(&buf[..n]) { lythos_std::sys_log(s); }
        lythos_std::sys_log("\n");
    } else {
        lythos_std::sys_log("\n");
    }
    let _ = sys_rollback();
    sys_task_exit()
}
