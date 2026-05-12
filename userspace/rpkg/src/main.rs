//! rpkg — Lythos package manager (stub).
//!
//! Full implementation pending. This stub registers the binary so it appears
//! in /lth/bin and responds to --version and help so scripts can detect it.

#![no_std]
#![no_main]

extern crate alloc;

use lythos_std::{println, sys_task_exit};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // argv not yet plumbed through the ABI; read raw args from the environment
    // once that's available. For now just print the usage screen.
    print_usage();
    sys_task_exit()
}

fn print_usage() {
    println!("rpkg 0.1 — Lythos package manager (stub)");
    println!();
    println!("USAGE:");
    println!("  rpkg <command> [args]");
    println!();
    println!("COMMANDS:");
    println!("  install <pkg>      install a package from the store");
    println!("  remove  <pkg>      remove an installed package");
    println!("  update             fetch and apply system updates");
    println!("  list               list installed packages");
    println!("  search  <query>    search available packages");
    println!("  snapshot           create a rollback snapshot");
    println!("  rollback           revert to the previous snapshot");
    println!("  status             show pending rollback state");
    println!();
    println!("This is a stub — commands are not yet implemented.");
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    lythos_std::sys_log("[rpkg] panic\n");
    sys_task_exit()
}
