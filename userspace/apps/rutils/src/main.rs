//! rutils — standalone binary entry point.
//!
//! Prints a listing of all available utilities and exits.
//! In the future this will support multi-call dispatch via argv.

#![no_std]
#![no_main]

use lythos_rt::{println, sys_task_exit};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("rutils 0.1 — OROS utility suite");
    println!();
    rutils::print_help();
    println!();
    println!("Invoke commands through lysh.");
    sys_task_exit(0)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    lythos_rt::sys_log("[rutils] panic\n");
    sys_task_exit(0)
}
