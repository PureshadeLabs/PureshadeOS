//! shade — Lythos package manager (stub).
//!
//! Full implementation pending. This stub registers the binary so it appears
//! in /lth/bin and responds to --version and help so scripts can detect it.

#![no_std]
#![no_main]

extern crate alloc;

use lythos_rt::{println, sys_task_exit};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // argv not yet plumbed through the ABI; read raw args from the environment
    // once that's available. For now just print the usage screen.
    print_usage();
    sys_task_exit()
}

fn print_usage() {
    println!("shade 0.1 — Lythos package manager (stub)");
    println!();
    println!("USAGE:");
    println!("  shade <command> [args]");
    println!();
    println!("COMMANDS:");
    println!("  build   <recipe>   evaluate + build into the store (shade-build)");
    println!("  install <pkg>      install a package from the store");
    println!("  remove  <pkg>      remove an installed package");
    println!("  update             fetch and apply system updates");
    println!("  list               list installed packages");
    println!("  search  <query>    search available packages");
    println!("  eval    <expr>     evaluate a Shade expression (shadec)");
    println!("  cdf     <expr>     dump canonical CDF bytes (shadec)");
    println!("  snapshot           create a rollback snapshot");
    println!("  rollback           revert to the previous snapshot");
    println!("  status             show pending rollback state");
    println!();
    println!("This is a stub — commands are not yet implemented.");
    // TODO(open): `shade eval` / `shade cdf` / `shade build` dispatch into
    // the shadec/shade-build libraries is blocked on two gaps:
    //   1. argv is not plumbed through the Lythos ABI (noted above) — the
    //      subcommand cannot even be selected yet;
    //   2. an OROS EvalIo implementation over lythos-libstd's VFS.
    // Until then the host `shadec` binary (pkg/shadec) is the evaluator
    // vehicle and the host `shade-build` binary (pkg/shade-build) is the
    // build-executor vehicle, per the seed model in shade-pkg 09 §2; see
    // docs/shade/build-executor.md.
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    lythos_rt::sys_log("[shade] panic\n");
    sys_task_exit()
}
