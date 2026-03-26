//! lythd — PID 1 init process for RaptorOS.
//!
//! Receives the kernel boot-info message, then drives the RaptorOS boot
//! sequence: spawn lythdist → lythmsg → non-critical services → supervisor loop.
//!
//! # Capability handles at entry
//!
//! | Handle | Kind     | Contents                                      |
//! |--------|----------|-----------------------------------------------|
//! | 0      | Memory   | Root memory cap — all free physical frames    |
//! | 1      | Rollback | `SYS_ROLLBACK` gate — exclusive to lythd      |
//! | 2      | Ipc      | Boot-info endpoint — one pre-queued BootInfo  |

#![no_std]
#![no_main]

use lythos_std::{BootInfo, sys_ipc_recv, sys_task_exit};

// ── Capability handle constants ───────────────────────────────────────────────

const MEM_CAP:      u64 = 0;
const ROLLBACK_CAP: u64 = 1;
const BOOT_INFO_CAP: u64 = 2;

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // ── 1. Consume the boot-info message ─────────────────────────────────
    let mut buf = [0u8; 64];
    sys_ipc_recv(BOOT_INFO_CAP, &mut buf).expect("lythd: failed to receive boot-info");

    let _info = BootInfo::from_bytes(&buf).expect("lythd: boot-info signature mismatch");

    // ── 2. TODO: spawn lythdist ───────────────────────────────────────────
    // sys_exec(LYTHDIST_ELF, &[MEM_CAP, ...])

    // ── 3. TODO: spawn lythmsg ────────────────────────────────────────────

    // ── 4. TODO: read service definitions, spawn non-critical services ────

    // ── 5. TODO: supervisor loop ──────────────────────────────────────────

    sys_task_exit()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // TODO: trigger rollback via SYS_ROLLBACK if we hold the cap
    loop {}
}
