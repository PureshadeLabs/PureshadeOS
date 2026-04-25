//! lythd — PID 1 init process for OROS (Open Runtime Operating System).
//!
//! ## Boot sequence
//!
//! 1. Receive the 64-byte `BootInfo` message on cap handle 2 (pre-queued by
//!    the kernel before `exec`).
//! 2. Print system info.
//! 3. Create the **service registry** IPC endpoint.
//! 4. Spawn core services (lythdist, lythmsg) once their ELF blobs exist.
//! 5. Enter the **supervisor loop**: handle `Register` / `Lookup` requests.
//!
//! ## Capability handles at entry
//!
//! | Handle | Kind     | Contents                                   |
//! |--------|----------|--------------------------------------------|
//! | 0      | Memory   | Root memory cap — all free physical frames |
//! | 1      | Rollback | `SYS_ROLLBACK` gate                        |
//! | 2      | Ipc      | Boot-info endpoint — one pre-queued msg    |

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use cask_std::{
    BootInfo,
    ipc::Endpoint,
    println, eprintln,
    sys_rollback, sys_task_exit,
    task::yield_now,
};

// ── Embedded service binaries ─────────────────────────────────────────────────

/// lythdist ELF — compiled by build.rs before lythd is built.
static LYTHDIST_ELF: &[u8] = include_bytes!(env!("LYTHDIST_ELF"));

// ── Capability handle constants ───────────────────────────────────────────────

const _MEM_CAP:      u64 = 0;
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

// ── Service table ─────────────────────────────────────────────────────────────

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

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // ── 1. Consume the boot-info message ─────────────────────────────────
    let boot_ep = Endpoint::from_raw(BOOT_INFO_CAP);
    let frame   = boot_ep.recv_frame().expect("lythd: boot-info recv failed");
    let info    = BootInfo::from_bytes(&frame).expect("lythd: boot-info sig mismatch");

    let mem_mib = { info.mem_bytes   } / (1024 * 1024);
    let frames  = { info.free_frames };
    println!("[lythd] cask init — {} MiB free ({} frames), cpu: {}",
             mem_mib, frames, info.vendor_str());

    // ── 2. Create the service registry endpoint ───────────────────────────
    let registry = Endpoint::create().expect("lythd: registry endpoint alloc failed");
    println!("[lythd] service registry online (cap {})", registry.as_raw());

    // ── 3. Spawn core services ────────────────────────────────────────────
    //
    // lythdist — capability distributor:
    //   caps[0] = Memory cap    (lythdist will sub-grant it to future services)
    //   caps[1] = dist_req_ep   (lythd sends CapGrantReq here)
    //   caps[2] = dist_rsp_ep   (lythdist sends CapGrantAck/Nack here)
    //   caps[3] = registry      (lythdist registers itself on startup)

    let dist_req_ep = Endpoint::create().expect("lythd: dist req endpoint alloc failed");
    let dist_rsp_ep = Endpoint::create().expect("lythd: dist rsp endpoint alloc failed");

    let lythdist_task = cask_std::task::spawn(
        LYTHDIST_ELF,
        &[_MEM_CAP, dist_req_ep.as_raw(), dist_rsp_ep.as_raw(), registry.as_raw()],
    ).expect("lythd: lythdist spawn failed");
    println!("[lythd] lythdist spawned (task {})", lythdist_task);

    // ── 4. Supervisor loop ────────────────────────────────────────────────
    println!("[lythd] entering supervisor loop");

    let mut services: Vec<Service> = Vec::new();

    loop {
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

                // ACK
                let mut ack = [0u8; 64];
                ack[0] = KIND_ACK;
                let _ = registry.send(&ack);
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
    cask_std::sys_log("[lythd] PANIC");
    if let Some(msg) = info.message().as_str() {
        cask_std::sys_log(": ");
        cask_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        cask_std::sys_log(" at ");
        cask_std::sys_log(loc.file());
        cask_std::sys_log(":");
        // print line number manually
        let line = loc.line();
        let mut buf = [0u8; 10];
        let mut n = 0usize;
        let mut v = line;
        if v == 0 { buf[0] = b'0'; n = 1; } else {
            while v > 0 { buf[n] = b'0' + (v % 10) as u8; n += 1; v /= 10; }
            buf[..n].reverse();
        }
        if let Ok(s) = core::str::from_utf8(&buf[..n]) { cask_std::sys_log(s); }
        cask_std::sys_log("\n");
    } else {
        cask_std::sys_log("\n");
    }
    // Attempt rollback — handle 1 is the Rollback cap.
    let _ = sys_rollback();
    sys_task_exit()
}
