//! lythd — PID 1 init process for OROS (Open Runtime Operating System).
//!
//! ## Boot sequence
//!
//! 1. Receive the 64-byte `BootInfo` message on cap handle 2 (pre-queued by
//!    the kernel before `exec`).
//! 2. Print system info.
//! 3. Create the **service registry** IPC endpoint.
//! 4. Spawn core services (lythdist, lysh).
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
//! exited unexpectedly it is restarted with the same capability set (up to 3
//! times) before being marked `Failed`.
//!
//! **Limitation (MVP):** health checks only fire when a registry message
//! arrives.  A future improvement would use a non-blocking IPC poll or a
//! dedicated watchdog task so silent crashes are caught promptly.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lythos_std::{
    BootInfo,
    ipc::Endpoint,
    println, eprintln,
    sys_rollback, sys_task_exit,
    task::{yield_now, TaskId, TaskStatus},
};

// ── Embedded service binaries ─────────────────────────────────────────────────

/// lythdist ELF — compiled by build.rs before lythd is built.
static LYTHDIST_ELF: &[u8] = include_bytes!(env!("LYTHDIST_ELF"));

/// lysh ELF — compiled by build.rs before lythd is built.
static LYSH_ELF: &[u8] = include_bytes!(env!("LYSH_ELF"));

// ── Capability handle constants ───────────────────────────────────────────────

const MEM_CAP:       u64 = 0;
const _ROLLBACK_CAP: u64 = 1;
const BOOT_INFO_CAP: u64 = 2;

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

/// Lifecycle state of a managed service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceState {
    /// Service is running normally (may have previously been restarted).
    Running,
    /// Service has died and been restarted `n` times (1..=3).
    Restarting(u8),
    /// Service has exceeded the restart limit and is no longer managed.
    Failed,
}

/// A service spawned and supervised by lythd.
struct ManagedSvc {
    name:      &'static str,
    task_id:   TaskId,
    state:     ServiceState,
    /// ELF blob used to respawn this service.
    elf:       &'static [u8],
    /// Capability handles passed at spawn time (up to 8).
    caps:      [u64; 8],
    cap_count: usize,
}

impl ManagedSvc {
    fn new(
        name:    &'static str,
        task_id: TaskId,
        elf:     &'static [u8],
        caps:    &[u64],
    ) -> Self {
        let mut caps_buf = [0u64; 8];
        let cap_count = caps.len().min(8);
        caps_buf[..cap_count].copy_from_slice(&caps[..cap_count]);
        ManagedSvc { name, task_id, state: ServiceState::Running, elf, caps: caps_buf, cap_count }
    }

    /// Check whether this service is still alive; restart it if it has died.
    ///
    /// Called from the supervisor loop after each registry message.
    fn check_and_restart(&mut self) {
        // Already permanently failed — nothing to do.
        if self.state == ServiceState::Failed { return; }

        // Query the kernel for the task's current status.
        if lythos_std::task::task_status(self.task_id) != TaskStatus::Dead { return; }

        // Task is dead.  Determine how many restart attempts have been made.
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

        match lythos_std::task::spawn(self.elf, &self.caps[..self.cap_count]) {
            Ok(new_id) => {
                self.task_id = new_id;
                self.state   = ServiceState::Restarting(attempts + 1);
                println!("[lythd] {} restarted as task {}", self.name, new_id);
            }
            Err(e) => {
                eprintln!("[lythd] {} respawn failed: {:?}", self.name, e);
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
    //
    //   caps[0] = Memory cap    (lythdist sub-grants it to future services)
    //   caps[1] = dist_req_ep   (lythd sends CapGrantReq here)
    //   caps[2] = dist_rsp_ep   (lythdist sends CapGrantAck/Nack here)
    //   caps[3] = registry      (lythdist registers itself on startup)

    let dist_req_ep = Endpoint::create().expect("lythd: dist req endpoint alloc failed");
    let dist_rsp_ep = Endpoint::create().expect("lythd: dist rsp endpoint alloc failed");
    let lythdist_caps = [MEM_CAP, dist_req_ep.as_raw(), dist_rsp_ep.as_raw(), registry.as_raw()];

    let lythdist_task = lythos_std::task::spawn(LYTHDIST_ELF, &lythdist_caps)
        .expect("lythd: lythdist spawn failed");
    println!("[lythd] lythdist spawned (task {})", lythdist_task);

    // ── 4. Spawn lysh (interactive shell) ────────────────────────────────
    //
    // lysh requires no special capabilities — it reads serial and calls
    // SYS_LOG/SYS_TASK_EXIT directly.  It also exits when the user types
    // "exit", so we restart it to keep a shell always available.

    let lysh_task = lythos_std::task::spawn(LYSH_ELF, &[])
        .expect("lythd: lysh spawn failed");
    println!("[lythd] lysh spawned (task {})", lysh_task);

    // ── 5. Supervisor loop ────────────────────────────────────────────────

    println!("[lythd] entering supervisor loop");

    let mut services: Vec<Service> = Vec::new();
    let mut managed: [ManagedSvc; 2] = [
        ManagedSvc::new("lythdist", lythdist_task, LYTHDIST_ELF, &lythdist_caps),
        ManagedSvc::new("lysh",     lysh_task,     LYSH_ELF,     &[]),
    ];

    loop {
        // ── Health check: inspect each managed service ────────────────
        for svc in managed.iter_mut() {
            svc.check_and_restart();
        }

        // ── Process one registry message ──────────────────────────────
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
                // No ACK — registry registration is one-way (fire-and-forget).
            }

            KIND_LOOKUP => {
                let name_len = (frame[1] as usize).min(32);
                let query = core::str::from_utf8(&frame[2..2 + name_len])
                    .unwrap_or("");

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

            kind => {
                eprintln!("[lythd] unknown registry msg kind={}", kind);
            }
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
    // Attempt rollback — handle 1 is the Rollback cap.
    let _ = sys_rollback();
    sys_task_exit()
}
