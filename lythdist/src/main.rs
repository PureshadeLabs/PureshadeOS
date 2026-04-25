//! lythdist — capability distributor daemon for OROS (Open Runtime Operating System).
//!
//! lythdist is the long-lived holder of the root Memory capability.  After
//! lythd bootstraps the system, services that need a derived Memory cap send a
//! `CapGrantReq` here; lythdist calls `SYS_CAP_GRANT` on their behalf and
//! replies with the handle index the grantee received.
//!
//! ## Capability handles at entry
//!
//! | Handle | Kind   | Contents                                    |
//! |--------|--------|---------------------------------------------|
//! | 0      | Memory | Root memory cap — ALL rights                |
//! | 1      | Ipc    | Request endpoint — lythd → lythdist         |
//! | 2      | Ipc    | Response endpoint — lythdist → lythd        |
//!
//! ## Protocol (64-byte messages)
//!
//! **CapGrantReq** (kind = 0):
//!
//! | Bytes  | Field   | Meaning                                       |
//! |--------|---------|-----------------------------------------------|
//! | 0      | kind    | 0                                             |
//! | 1      | rights  | requested rights bitmask (`cap_rights::*`)    |
//! | 2..10  | task_id | target task to receive the derived cap (u64 LE) |
//! | 10..64 | _pad    | zero                                          |
//!
//! **CapGrantAck** (kind = 1) — success:
//!
//! | Bytes | Field  | Meaning                                         |
//! |-------|--------|-------------------------------------------------|
//! | 0     | kind   | 1                                               |
//! | 1..9  | handle | cap handle index in the target's table (u64 LE) |
//! | 9..64 | _pad   | zero                                            |
//!
//! **CapGrantNack** (kind = 2) — failure:
//!
//! | Bytes | Field | Meaning |
//! |-------|-------|---------|
//! | 0     | kind  | 2       |
//! | 1..64 | _pad  | zero    |

#![no_std]
#![no_main]

extern crate alloc;

use cask_std::{cap_rights, ipc::Endpoint, println, eprintln, sys_cap_grant, sys_task_exit};

// ── Capability handles at entry ───────────────────────────────────────────────

const MEM_CAP:      u64 = 0;
const REQ_EP:       u64 = 1;   // request  endpoint: lythd sends,    lythdist receives
const RSP_EP:       u64 = 2;   // response endpoint: lythdist sends, lythd receives
const REGISTRY_CAP: u64 = 3;   // lythd service registry — used once at startup

// ── CapGrant protocol message kinds ──────────────────────────────────────────

const KIND_GRANT_REQ:  u8 = 0;
const KIND_GRANT_ACK:  u8 = 1;
const KIND_GRANT_NACK: u8 = 2;

// ── Service registry protocol (shared with lythd) ────────────────────────────
//
// | Byte(s) | Field    | Meaning                             |
// |---------|----------|-------------------------------------|
// | 0       | kind     | 0=Register  1=Lookup  2=Ack  3=Nack |
// | 1       | name_len | length of service name (≤32)        |
// | 2..34   | name     | service name, ASCII, null-padded    |
// | 34..42  | task_id  | spawned TaskId  (Register only)     |
// | 42..50  | cap      | IPC cap handle for the service      |
// | 50..64  | _pad     | reserved, zero                      |

const REG_KIND_REGISTER: u8 = 0;

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[lythdist] capability distributor online (mem_cap={} req={} rsp={})",
             MEM_CAP, REQ_EP, RSP_EP);

    // ── Register with the lythd service registry ──────────────────────────
    //
    // The registry endpoint is a one-way channel (services → lythd only).
    // We send the Register message and do NOT wait for an ACK: lythd reads
    // the message and processes it asynchronously.  Waiting for an ACK on
    // the same bidirectional ring would race with lythd reading the Register
    // from that ring before lythdist can receive the ACK.
    {
        let reg_ep = Endpoint::from_raw(REGISTRY_CAP);
        let name   = b"lythdist";
        let mut frame = [0u8; 64];
        frame[0] = REG_KIND_REGISTER;
        frame[1] = name.len() as u8;
        frame[2..2 + name.len()].copy_from_slice(name);
        // task_id bytes 34..42 — left as zero (lythdist has no SYS_GETPID yet)
        // cap bytes 42..50 — advertise REQ_EP as the service endpoint
        frame[42..50].copy_from_slice(&REQ_EP.to_le_bytes());
        reg_ep.send_frame(&frame).expect("lythdist: registry register send failed");
        println!("[lythdist] registered with service registry (async)");
    }

    let req_ep = Endpoint::from_raw(REQ_EP);
    let rsp_ep = Endpoint::from_raw(RSP_EP);

    loop {
        let frame = match req_ep.recv_frame() {
            Ok(f)  => f,
            Err(e) => {
                eprintln!("[lythdist] recv error: {:?}", e);
                continue;
            }
        };

        match frame[0] {
            KIND_GRANT_REQ => {
                let rights  = frame[1] & cap_rights::ALL;   // clamp to known bits
                let task_id = u64::from_le_bytes(frame[2..10].try_into().unwrap_or([0; 8]));

                if rights == 0 || task_id == 0 {
                    eprintln!("[lythdist] bad GrantReq: rights=0x{:02x} task_id={}", rights, task_id);
                    send_nack(&rsp_ep);
                    continue;
                }

                match sys_cap_grant(MEM_CAP, task_id, rights) {
                    Ok(handle) => {
                        println!("[lythdist] granted Memory(0x{:02x}) to task {} → handle {}",
                                 rights, task_id, handle);
                        let mut ack = [0u8; 64];
                        ack[0] = KIND_GRANT_ACK;
                        ack[1..9].copy_from_slice(&handle.to_le_bytes());
                        let _ = rsp_ep.send_frame(&ack);
                    }
                    Err(e) => {
                        eprintln!("[lythdist] cap_grant failed for task {}: {:?}", task_id, e);
                        send_nack(&rsp_ep);
                    }
                }
            }

            kind => {
                eprintln!("[lythdist] unknown msg kind={}", kind);
            }
        }
    }
}

fn send_nack(ep: &Endpoint) {
    let mut nack = [0u8; 64];
    nack[0] = KIND_GRANT_NACK;
    let _ = ep.send_frame(&nack);
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    cask_std::sys_log("[lythdist] PANIC");
    if let Some(msg) = info.message().as_str() {
        cask_std::sys_log(": ");
        cask_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        cask_std::sys_log(" at ");
        cask_std::sys_log(loc.file());
        cask_std::sys_log(":");
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
    sys_task_exit()
}
