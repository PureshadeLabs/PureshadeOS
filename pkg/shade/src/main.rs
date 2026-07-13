//! shade — Lythos package manager (stub).
//!
//! Full implementation pending. This stub registers the binary so it appears
//! in /lth/bin, responds to --version and help so scripts can detect it, and
//! echoes its argv — the on-target proof that argv now flows through the ABI
//! (SYS_EXEC a5/a6 → initial stack frame → `lythos_rt::entry!`/`args`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lythos_rt::println;

lythos_rt::entry!(main);

fn main() {
    let args: Vec<&str> = lythos_rt::args::args().collect();

    // Echo what arrived — `shade build foo` from lysh must print
    // `argv = ["/lth/bin/shade", "build", "foo"]`.
    println!("shade: argv = {:?}", args);

    match args.get(1).copied() {
        None => print_usage(),
        Some("--version") => println!("shade 0.1 — Lythos package manager (stub)"),
        Some("help") | Some("--help") => print_usage(),
        Some(cmd) => {
            println!("shade: `{}` is not implemented yet (stub)", cmd);
            print_usage();
        }
    }
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
    // the shadec/shade-build libraries is blocked on one remaining gap: an
    // OROS EvalIo implementation over lythos-libstd's VFS. (The argv gap is
    // closed — subcommand selection above works on target.) Until then the
    // host `shadec` binary (pkg/shadec) is the evaluator vehicle and the
    // host `shade-build` binary (pkg/shade-build) is the build-executor
    // vehicle, per the seed model in shade-pkg 09 §2; see
    // docs/shade/build-executor.md.
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    lythos_rt::sys_log("[shade] panic\n");
    lythos_rt::sys_task_exit()
}
