//! THE acceptance gate (task spec + docs/shade/08-interop.md §3): CDF bytes
//! produced by evaluating a recipe must be byte-identical to bytes built
//! directly with the shared canonicalizer for the same key set. Any
//! divergence silently shifts store paths and breaks input-addressing.

use std::sync::Arc;

use shadec::error::Pos;
use shadec::eval::Evaluator;
use shadec::io::HostIo;
use shadec::value::Value;

fn pos() -> Pos {
    Pos { file: Arc::from("<test>"), line: 0, col: 0 }
}

/// Run `f` inside a big-stack worker (values are Rc-based, keep everything
/// in one thread).
fn with_eval<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn eval_expr<'io>(ev: &mut Evaluator<'io>, src: &str, base: &str) -> Value {
    let expr = shadec::parser::parse_str(src, Arc::from("<test>"), base)
        .unwrap_or_else(|e| panic!("parse: {e}"));
    let env = ev.initial_env();
    ev.eval(&expr, &env).unwrap_or_else(|e| panic!("eval: {e}"))
}

fn attr_string(ev: &mut Evaluator, v: &Value, name: &str) -> String {
    let Value::Attrs(m) = v else { panic!("expected set") };
    ev.force_attr_string(m, name, &pos()).unwrap_or_else(|e| panic!("{name}: {e}")).s.to_string()
}

fn get_attr(ev: &mut Evaluator, v: &Value, name: &str) -> Value {
    let Value::Attrs(m) = v else { panic!("expected set") };
    let t = m.get(name).unwrap_or_else(|| panic!("no attr {name}")).clone();
    ev.force(&t, &pos()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The 05 §7 worked mapping: the rkilo recipe, with lythos-libstd as a real
/// dependency derivation. The evaluator's bytes must equal a hand-built
/// canonicalizer document over the same key set.
#[test]
fn gate_rkilo_worked_mapping() {
    with_eval(|| {
        let src = r#"
let
  lythos-libstd = derivation {
    name = "lythos-libstd";
    version = "0.3.0";
    system = "x86_64-oros";
    toolchain = "rustc-1.86.0-adf2135f0";
    phases = [ "cargo build --release" ];
    outputs = { lib = [ "liblythos_libstd*" ]; };
  };
in {
  dep = lythos-libstd;
  pkg = derivation {
    name = "rkilo";
    version = "1.2.0";
    system = "x86_64-oros";
    toolchain = "rustc-1.86.0-adf2135f0";
    sources = [
      (builtins.fetchCratesIo {
        crate = "rkilo"; version = "1.2.0";
        sha256 = "9f1c2ab34c1d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f";
      })
    ];
    deps = [ lythos-libstd ];
    env = { RUSTFLAGS = "-C opt-level=3"; };
    phases = [
      "cargo build --release --offline --target x86_64-oros"
      "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo"
    ];
    outputs = { bin = [ "rkilo" ]; lib = []; share = []; };
  };
}
"#;
        let io = HostIo;
        let mut ev = Evaluator::new(&io);
        let v = eval_expr(&mut ev, src, "/base");
        let dep = get_attr(&mut ev, &v, "dep");
        let pkg = get_attr(&mut ev, &v, "pkg");
        let dep_out = attr_string(&mut ev, &dep, "outPath");
        let pkg_drv = attr_string(&mut ev, &pkg, "drvPath");
        let pkg_out = attr_string(&mut ev, &pkg, "outPath");
        let got = ev.drvs.get(&pkg_drv).expect("CDF recorded for pkg").clone();

        // expected bytes: the same key set through the canonicalizer directly
        let mut b = shade_cdf::CdfBuilder::new();
        b.insert("dep.0", &dep_out).unwrap();
        // recipe writes RUSTFLAGS; the CDF key is its lowercase fold (02 §3.3)
        b.insert("env.rustflags", "-C opt-level=3").unwrap();
        b.insert("name", "rkilo").unwrap();
        b.insert("output.0", "bin/rkilo").unwrap();
        b.insert("phase.0", "cargo build --release --offline --target x86_64-oros").unwrap();
        b.insert("phase.1", "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo")
            .unwrap();
        b.insert("sandbox", "1").unwrap();
        b.insert("source.0.crate", "rkilo").unwrap();
        b.insert(
            "source.0.sha256",
            "9f1c2ab34c1d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f",
        )
        .unwrap();
        b.insert("source.0.type", "crates-io").unwrap();
        b.insert("source.0.version", "1.2.0").unwrap();
        b.insert("system", "x86_64-oros").unwrap();
        b.insert("toolchain", "rustc-1.86.0-adf2135f0").unwrap();
        b.insert("version", "1.2.0").unwrap();
        let expected = b.build();

        assert_eq!(
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&expected),
            "CDF bytes must be identical — input-addressing invariant"
        );

        // store paths derive from those bytes (shade-pkg 02 §2)
        let paths = shade_cdf::store_paths("rkilo", "1.2.0", &expected).unwrap();
        assert_eq!(pkg_drv, paths.drv_path);
        assert_eq!(pkg_out, paths.out_path);

        // derivation value surface (04 §6)
        assert_eq!(attr_string(&mut ev, &pkg, "type"), "derivation");
        assert_eq!(attr_string(&mut ev, &pkg, "outputName"), "out");
        assert_eq!(attr_string(&mut ev, &pkg, "name"), "rkilo");
    });
}

/// Two recipes that reduce to the same key set produce byte-identical CDF —
/// the property input-addressing depends on (05 §3).
#[test]
fn gate_equivalent_recipes_same_bytes() {
    with_eval(|| {
        let a = r#"
derivation {
  name = "demo"; version = "1.0"; system = "x86_64-oros"; toolchain = "tc-1";
  env = { FOO = "bar"; };
  outputs = { bin = [ "demo" ]; };
}"#;
        // different surface syntax, same key set: computed name, // update,
        // interpolated env value, null-dropped optional
        let b = r#"
let base = { name = "de" + "mo"; version = "1.0"; };
in derivation (base // {
  system = "x86_64-oros"; toolchain = "tc-1";
  env = { FOO = "${"b"}ar"; BAZ = null; };
  sandbox = 1;
  outputs = { bin = [ "demo" ]; lib = []; };
  description = "not hashed";
})"#;
        let io = HostIo;
        let mut ev = Evaluator::new(&io);
        let va = eval_expr(&mut ev, a, "/base");
        let vb = eval_expr(&mut ev, b, "/base");
        let da = attr_string(&mut ev, &va, "drvPath");
        let db = attr_string(&mut ev, &vb, "drvPath");
        assert_eq!(da, db, "same key set ⇒ same drvPath");
        // derivation equality is drvPath equality (03 §7)
        let eq = ev.eq_values(&va, &vb, &pos()).unwrap();
        assert!(eq);
    });
}

/// Implicit deps via string context (05 §2.2): a derivation interpolated
/// into a phase string becomes dep.<i> without appearing in `deps`.
#[test]
fn gate_context_becomes_dep() {
    with_eval(|| {
        let src = r#"
let
  tool = derivation {
    name = "tool"; version = "0.1"; system = "x86_64-oros"; toolchain = "tc-1";
    outputs = { bin = [ "tool" ]; };
  };
in {
  inherit tool;
  pkg = derivation {
    name = "user"; version = "0.1"; system = "x86_64-oros"; toolchain = "tc-1";
    phases = [ "${tool}/bin/tool run" ];
    outputs = { bin = [ "user" ]; };
  };
}"#;
        let io = HostIo;
        let mut ev = Evaluator::new(&io);
        let v = eval_expr(&mut ev, src, "/base");
        let tool = get_attr(&mut ev, &v, "tool");
        let pkg = get_attr(&mut ev, &v, "pkg");
        let tool_out = attr_string(&mut ev, &tool, "outPath");
        let pkg_drv = attr_string(&mut ev, &pkg, "drvPath");
        let cdf = String::from_utf8((**ev.drvs.get(&pkg_drv).unwrap()).clone()).unwrap();
        assert!(
            cdf.contains(&format!("dep.0={tool_out}\n")),
            "context-carried dep must appear as dep.0; cdf:\n{cdf}"
        );
        assert!(cdf.contains(&format!("phase.0={tool_out}/bin/tool run\n")));
    });
}

/// Ingestion (04 §4.2): coercing a path to a string emits a `local` source
/// derivation with the shade-pkg 04 §3.3 tree hash, and the coerced string
/// is its store path.
#[test]
fn gate_ingestion() {
    with_eval(|| {
        let dir = std::env::temp_dir().join(format!("shadec-ingest-{}", std::process::id()));
        let srcdir = dir.join("mysrc");
        std::fs::create_dir_all(srcdir.join("sub")).unwrap();
        std::fs::write(srcdir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(srcdir.join("sub/util.rs"), "pub fn u() {}").unwrap();

        let io = HostIo;
        let mut ev = Evaluator::new(&io);
        let v = eval_expr(&mut ev, r#""${./mysrc}""#, dir.to_str().unwrap());
        let Value::Str(s) = &v else { panic!("expected string") };
        assert!(s.s.starts_with("/shade/store/"), "coerces to a store path: {}", s.s);
        assert!(s.s.contains("-mysrc-src-"), "source drv naming: {}", s.s);
        // the coerced string carries a context referencing the source drv (04 §4.2)
        assert!(s.ctx.contains(&*s.s.to_string()), "context references the ingested source");

        // trimmed source CDF recorded, keyed by drvPath = outPath + .drv
        let drv_path = format!("{}.drv", s.s);
        let cdf = String::from_utf8((**ev.drvs.get(&drv_path).unwrap()).clone()).unwrap();
        assert!(cdf.starts_with("shade-drv=1\nbuilder=fetch\n"), "trimmed form:\n{cdf}");
        assert!(cdf.contains("source.0.type=local\n"));
        assert!(cdf.contains("source.0.tree="));
        assert!(!cdf.contains("system="), "source drvs omit system (shade-pkg 04 §2)");

        // recorded as an eval input (03 §5.3)
        assert!(ev.eval_inputs.iter().any(|i| i.starts_with("ingest:")), "{:?}", ev.eval_inputs);

        // same tree twice → same store path (memoized, identity is content)
        let v2 = eval_expr(&mut ev, r#""${./mysrc}""#, dir.to_str().unwrap());
        let Value::Str(s2) = &v2 else { panic!() };
        assert_eq!(s.s, s2.s);

        // filterSource prunes before hashing (04 §4.2): dropping sub/ gives
        // a different tree hash / store path
        let v3 = eval_expr(
            &mut ev,
            r#"(builtins.filterSource (p: t: t != "directory") ./mysrc).outPath"#,
            dir.to_str().unwrap(),
        );
        let Value::Str(s3) = &v3 else { panic!() };
        assert_ne!(s.s, s3.s, "filtered tree must hash differently");

        std::fs::remove_dir_all(&dir).ok();
    });
}

/// Purity/closedness rejections around `derivation` and the fetch builtins.
#[test]
fn derivation_rejections() {
    with_eval(|| {
        let io = HostIo;
        let run_err = |src: &str| -> shadec::error::EvalError {
            let mut ev = Evaluator::new(&io);
            let expr = shadec::parser::parse_str(src, Arc::from("<t>"), "/base").unwrap();
            let env = ev.initial_env();
            match ev.eval(&expr, &env) {
                Ok(v) => {
                    // force drvPath if it's a derivation — errors surface at emission
                    if let Value::Attrs(m) = &v {
                        if let Some(t) = m.get("drvPath") {
                            let t = t.clone();
                            if let Err(e) = ev.force(&t, &pos()) {
                                return e;
                            }
                        }
                    }
                    panic!("expected error for {src}")
                }
                Err(e) => e,
            }
        };

        let base = r#"name = "x"; version = "1.0"; system = "s"; toolchain = "t";
                      outputs = { bin = [ "x" ]; };"#;

        // unknown argument: closed schema (05 §1)
        let e = run_err(&format!("derivation {{ {base} wrong = 1; }}"));
        assert!(e.msg.contains("unknown argument"), "{}", e.msg);
        // `unsafe` retired (05 §2)
        let e = run_err(&format!("derivation {{ {base} unsafe = true; }}"));
        assert!(e.msg.contains("retired"), "{}", e.msg);
        // sandbox-fixed env var (shade-pkg 06 §4)
        let e = run_err(&format!("derivation {{ {base} env = {{ PATH = \"x\"; }}; }}"));
        assert!(e.msg.contains("fixed by the sandbox"), "{}", e.msg);
        // bad env key charset
        let e = run_err(&format!("derivation {{ {base} env = {{ lower = \"x\"; }}; }}"));
        assert!(e.msg.contains("A-Z"), "{}", e.msg);
        // outputs must declare at least one entry (shade-pkg 03 §6)
        let e = run_err(
            r#"derivation { name = "x"; version = "1.0"; system = "s"; toolchain = "t";
                          outputs = { bin = []; }; }"#,
        );
        assert!(e.msg.contains("at least one"), "{}", e.msg);
        // name normalization: no guessing (shade-pkg 03 §2)
        let e = run_err(
            r#"derivation { name = "bad name"; version = "1.0"; system = "s"; toolchain = "t";
                          outputs = { bin = [ "x" ]; }; }"#,
        );
        assert!(e.msg.contains("invalid package name"), "{}", e.msg);
        // toolchain absent and no ambient identity (05 §2)
        let e = run_err(
            r#"derivation { name = "x"; version = "1.0"; system = "s";
                          outputs = { bin = [ "x" ]; }; }"#,
        );
        assert!(e.msg.contains("toolchain"), "{}", e.msg);

        // fetch builtins: hash required, pinned identities only (05 §5)
        let e = run_err(r#"builtins.fetchCratesIo { crate = "x"; version = "1.0"; sha256 = ""; }"#);
        assert!(e.msg.contains("64 lowercase hex"), "{}", e.msg);
        let e = run_err(r#"builtins.fetchCratesIo { crate = "x"; version = "1.0"; }"#);
        assert!(e.msg.contains("missing required"), "{}", e.msg);
        let e = run_err(r#"builtins.fetchGit { url = "https://x/y.git"; commit = "main"; }"#);
        assert!(e.msg.contains("40"), "{}", e.msg);
    });
}

/// Source derivations are trimmed CDFs: builder=fetch, no system/toolchain
/// (shade-pkg 04 §2, shade 05 §4.1).
#[test]
fn source_drv_trimmed_form() {
    with_eval(|| {
        let io = HostIo;
        let mut ev = Evaluator::new(&io);
        let v = eval_expr(
            &mut ev,
            r#"builtins.fetchCratesIo {
                 crate = "serde"; version = "1.0.0";
                 sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
               }"#,
            "/base",
        );
        let drv_path = attr_string(&mut ev, &v, "drvPath");
        let cdf = String::from_utf8((**ev.drvs.get(&drv_path).unwrap()).clone()).unwrap();
        let expected = "shade-drv=1\n\
builder=fetch\n\
name=serde-src\n\
source.0.crate=serde\n\
source.0.sha256=0000000000000000000000000000000000000000000000000000000000000000\n\
source.0.type=crates-io\n\
source.0.version=1.0.0\n\
version=1.0.0\n";
        assert_eq!(cdf, expected);
        assert!(drv_path.contains("-serde-src-1.0.0"));
    });
}

