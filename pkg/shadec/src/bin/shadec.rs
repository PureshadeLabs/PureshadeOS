//! shadec — host CLI, the seed-shadec vehicle (docs/shade-pkg/09-bootstrap.md §2).
//!
//! Subcommands:
//!   shadec eval [--strict] [--inputs] [--toolchain ID] <file-or-expr>
//!   shadec cdf  [--toolchain ID] <file-or-expr>
//!
//! `shadec cdf` is byte-normative (docs/shade/08-interop.md §3): it writes
//! exactly the bytes that would become the `.drv`, nothing else.
//!
//! In the target system this dispatch lives behind `shade eval` / `shade
//! cdf` in the unified OROS `shade` binary — blocked on argv plumbing
//! through the ABI (see pkg/shade/src/main.rs).

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use shadec::error::Pos;
use shadec::eval::Evaluator;
use shadec::io::HostIo;
use shadec::value::Value;

struct Opts {
    strict: bool,
    inputs: bool,
    toolchain: Option<String>,
    target: String,
}

fn usage() -> ! {
    eprintln!("usage: shadec <eval|cdf> [--strict] [--inputs] [--toolchain ID] <file-or-expr>");
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else { usage() };
    let mut opts =
        Opts { strict: false, inputs: false, toolchain: None, target: String::new() };
    let mut positional: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--strict" => opts.strict = true,
            "--inputs" => opts.inputs = true,
            "--toolchain" => match args.next() {
                Some(t) => opts.toolchain = Some(t),
                None => usage(),
            },
            _ => positional.push(a),
        }
    }
    if positional.len() != 1 {
        usage();
    }
    opts.target = positional.remove(0);

    let is_cdf = match cmd.as_str() {
        "eval" => false,
        "cdf" => true,
        _ => usage(),
    };

    // big worker stack: the MAX_DEPTH resource guard must trip before the
    // OS stack does
    let handle = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(is_cdf, opts))
        .expect("spawn worker");
    match handle.join() {
        Ok(code) => code,
        Err(_) => ExitCode::from(101),
    }
}

fn run(is_cdf: bool, opts: Opts) -> ExitCode {
    let io = HostIo;
    let mut ev = Evaluator::new(&io);
    ev.toolchain = opts.toolchain.clone();

    // file reference vs inline expression: explicit path syntax or an
    // existing .shade file is a file; anything else is an expression
    // evaluated with the cwd as its base directory
    let t = &opts.target;
    let is_file_ref = t.starts_with("./")
        || t.starts_with("../")
        || t.starts_with('/')
        || (t.ends_with(".shade") && std::fs::metadata(t).is_ok());

    let cwd = match std::env::current_dir() {
        Ok(d) => d.to_string_lossy().into_owned(),
        Err(e) => {
            eprintln!("shadec: cannot determine working directory: {e}");
            return ExitCode::from(1);
        }
    };

    let pos = Pos { file: Arc::from("<cli>"), line: 0, col: 0 };
    let result = (|| -> shadec::error::Result<()> {
        let value = if is_file_ref {
            let abs = if t.starts_with('/') {
                shadec::parser::normalize_path(t)
            } else {
                shadec::parser::normalize_path(&format!("{cwd}/{t}"))
            };
            ev.import(&abs, &pos)?
        } else {
            let expr = shadec::parser::parse_str(t, Arc::from("<expr>"), &cwd)?;
            let env = ev.initial_env();
            ev.eval(&expr, &env)?
        };

        if is_cdf {
            // full emission procedure (05 §3); output is exactly the .drv
            // bytes (08 §3, byte-normative)
            let Value::Attrs(m) = &value else {
                return Err(shadec::error::EvalError::at(
                    shadec::error::ErrorKind::Type,
                    format!("shadec cdf: expression evaluated to {}, expected a derivation (package-set selectors are the driver's job, 02 §6)", value.type_of()),
                    &pos,
                ));
            };
            if !ev.attrs_is_derivation(m, &pos)? {
                return Err(shadec::error::EvalError::at(
                    shadec::error::ErrorKind::Type,
                    "shadec cdf: expression evaluated to a set that is not a derivation",
                    &pos,
                ));
            }
            let drv_path = ev.force_attr_string(m, "drvPath", &pos)?;
            let bytes = ev
                .drvs
                .get(&*drv_path.s)
                .expect("emission recorded the CDF")
                .clone();
            std::io::stdout().write_all(&bytes).expect("write stdout");
        } else {
            let shown = shadec::print::show_value(&mut ev, &value, opts.strict, &pos)?;
            println!("{shown}");
            if opts.inputs {
                for i in &ev.eval_inputs {
                    println!("input: {i}");
                }
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("shadec: {e}");
            ExitCode::from(1)
        }
    }
}
