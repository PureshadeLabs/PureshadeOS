//! Purity suite (docs/shade/03-semantics.md §5): the forbidden operations
//! must actually reject. Most restrictions hold *structurally* — the names
//! do not exist — and these tests pin that they stay gone.

use std::sync::Arc;

use shadec::error::ErrorKind;
use shadec::eval::Evaluator;
use shadec::io::HostIo;

fn try_run(src: &str, base: &str) -> Result<String, shadec::error::EvalError> {
    let src = src.to_string();
    let base = base.to_string();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || -> Result<String, shadec::error::EvalError> {
            let io = HostIo;
            let mut ev = Evaluator::new(&io);
            let expr = shadec::parser::parse_str(&src, Arc::from("<purity>"), &base)?;
            let env = ev.initial_env();
            let v = ev.eval(&expr, &env)?;
            let pos = shadec::error::Pos { file: Arc::from("<purity>"), line: 0, col: 0 };
            shadec::print::show_value(&mut ev, &v, true, &pos)
        })
        .unwrap()
        .join()
        .unwrap()
}

fn expect_err(src: &str) -> shadec::error::EvalError {
    match try_run(src, "/base") {
        Ok(v) => panic!("purity violation possible: {src:?} evaluated to {v}"),
        Err(e) => e,
    }
}

/// No environment access (03 §5.1): getEnv is omitted entirely, not
/// present-but-empty.
#[test]
fn no_env_access() {
    assert_eq!(expect_err("builtins.getEnv \"HOME\"").kind, ErrorKind::MissingAttr);
    assert_eq!(expect_err("getEnv \"HOME\"").kind, ErrorKind::UndefinedVar);
}

/// No wall-clock / entropy / ambient system (03 §5.1): currentTime and
/// currentSystem are absent; `system` is an explicit derivation argument.
#[test]
fn no_clock_or_ambient_system() {
    assert_eq!(expect_err("builtins.currentTime").kind, ErrorKind::MissingAttr);
    assert_eq!(expect_err("builtins.currentSystem").kind, ErrorKind::MissingAttr);
    assert_eq!(expect_err("builtins.random").kind, ErrorKind::MissingAttr);
}

/// No network except fixed-output fetches (03 §5.1): there is no fetchurl /
/// fetchTarball, and the fetch builtins hard-require pinned identities.
#[test]
fn network_is_hash_gated() {
    assert_eq!(expect_err("builtins.fetchurl \"https://x\"").kind, ErrorKind::MissingAttr);
    assert_eq!(expect_err("builtins.fetchTarball \"https://x\"").kind, ErrorKind::MissingAttr);
    // missing hash
    let e = expect_err(r#"builtins.fetchCratesIo { crate = "x"; version = "1.0"; }"#);
    assert!(e.msg.contains("missing required"), "{}", e.msg);
    // empty / placeholder hash
    let e = expect_err(r#"builtins.fetchCratesIo { crate = "x"; version = "1.0"; sha256 = ""; }"#);
    assert!(e.msg.contains("hex"), "{}", e.msg);
    let e = expect_err(
        r#"builtins.fetchCratesIo { crate = "x"; version = "1.0";
             sha256 = "XXXX000000000000000000000000000000000000000000000000000000000000"; }"#,
    );
    assert!(e.msg.contains("hex"), "{}", e.msg);
    // symbolic git refs resolve at lock time, never at eval (05 §5)
    let e = expect_err(r#"builtins.fetchGit { url = "https://a/b.git"; commit = "v1.2.0"; }"#);
    assert!(e.msg.contains("resolved"), "{}", e.msg);
}

/// Impure path syntaxes are not part of the grammar (02 §2.5).
#[test]
fn impure_path_syntax_removed() {
    assert_eq!(expect_err("<shadepkgs>").kind, ErrorKind::Parse); // search paths removed
    assert_eq!(expect_err("import <shadepkgs>").kind, ErrorKind::Parse);
    // `~/…` home paths removed: `~` is not a token at all
    assert_eq!(expect_err("~/x").kind, ErrorKind::Parse);
    // URI literals removed
    assert_eq!(expect_err("https://example.org/x").kind, ErrorKind::Parse);
}

/// import takes a filesystem *reference* — a context-free string is
/// rejected (06 §1); reads are tracked (03 §5.2-5.3).
#[test]
fn import_and_reads_are_disciplined() {
    let e = expect_err(r#"import "/etc/hosts""#);
    assert_eq!(e.kind, ErrorKind::Type);
    let e = expect_err(r#"builtins.readFile "/etc/hosts""#);
    assert_eq!(e.kind, ErrorKind::Type);

    // v1 decision (03 §5.2): reads outside the evaluation roots are
    // *tracked, not blocked* — succeed and land in the eval-input set
    let dir = std::env::temp_dir().join(format!("shadec-purity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("outside.txt"), "data").unwrap();
    let outside = dir.join("outside.txt");
    let outside = outside.to_str().unwrap();
    // tracked read via a path literal pointing outside the base dir
    let got = try_run(&format!("builtins.readFile {outside}"), "/base").unwrap();
    assert_eq!(got, "\"data\"");
    std::fs::remove_dir_all(&dir).ok();
}

/// IFD is not supported in v1 (06 §5): importing an unrealized store path
/// is an eval error, not a build trigger.
#[test]
fn no_import_from_derivation() {
    let e = expect_err(
        r#"import (derivation {
             name = "gen"; version = "1.0"; system = "s"; toolchain = "t";
             outputs = { share = [ "gen.shade" ]; };
           })"#,
    );
    assert_eq!(e.kind, ErrorKind::Import);
    assert!(e.msg.contains("import-from-derivation"), "{}", e.msg);
}

/// Trace is diagnostic, not a value (07 §2.1) — and there is no exec/eval
/// escape hatch.
#[test]
fn no_exec_hatches() {
    assert_eq!(expect_err("builtins.exec [ \"ls\" ]").kind, ErrorKind::MissingAttr);
    assert_eq!(expect_err("builtins.storePath \"/shade/store/x\"").kind, ErrorKind::MissingAttr);
}
