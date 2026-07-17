//! Eval golden tests: semantics per docs/shade/03-semantics.md and values /
//! coercions per docs/shade/04-values.md, plus each MVP builtin (07).

use std::sync::Arc;

use shadec::error::ErrorKind;
use shadec::eval::Evaluator;
use shadec::io::HostIo;

fn run_in(src: &str, base: &str) -> Result<String, shadec::error::EvalError> {
    // Values are Rc-based (not Send); evaluate entirely inside a worker
    // thread with a large stack so the MAX_DEPTH resource guard trips
    // before the OS stack does (the CLI does the same).
    let src = src.to_string();
    let base = base.to_string();
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || -> Result<String, shadec::error::EvalError> {
            let io = HostIo;
            let mut ev = Evaluator::new(&io);
            let expr = shadec::parser::parse_str(&src, Arc::from("<test>"), &base)?;
            let env = ev.initial_env();
            let v = ev.eval(&expr, &env)?;
            let pos = shadec::error::Pos { file: Arc::from("<test>"), line: 0, col: 0 };
            shadec::print::show_value(&mut ev, &v, true, &pos)
        })
        .unwrap()
        .join()
        .unwrap()
}

fn run(src: &str) -> String {
    run_in(src, "/base").unwrap_or_else(|e| panic!("eval failed for {src:?}: {e}"))
}

fn run_err(src: &str) -> shadec::error::EvalError {
    match run_in(src, "/base") {
        Ok(v) => panic!("expected error for {src:?}, got {v}"),
        Err(e) => e,
    }
}

#[test]
fn arithmetic() {
    assert_eq!(run("1 + 2 * 3"), "7");
    assert_eq!(run("7 / 2"), "3");
    assert_eq!(run("-7 / 2"), "-3"); // truncation toward zero (04 §2.1)
    assert_eq!(run("1 - 5"), "-4");
    // wrapping overflow (04 §2.1, flagged decision)
    assert_eq!(run("9223372036854775807 + 1"), "-9223372036854775808");
    assert_eq!(run_err("1 / 0").kind, ErrorKind::Type);
    assert_eq!(run_err(r#"1 + "a""#).kind, ErrorKind::Type); // mixed int/string
}

#[test]
fn booleans_and_conditionals() {
    assert_eq!(run("true && false"), "false");
    assert_eq!(run("false || true"), "true");
    assert_eq!(run("false -> false"), "true");
    assert_eq!(run("!true"), "false");
    assert_eq!(run("if 1 < 2 then 10 else 20"), "10");
    // short-circuit: rhs never evaluated
    assert_eq!(run("false && (throw \"no\")"), "false");
    assert_eq!(run("true || (throw \"no\")"), "true");
    assert_eq!(run_err("if 1 then 2 else 3").kind, ErrorKind::Type);
}

#[test]
fn laziness_and_memoization() {
    // unused bindings never evaluate
    assert_eq!(run("let x = throw \"never\"; in 42"), "42");
    // list elements stay thunks until forced (03 §2)
    assert_eq!(run("builtins.length [ (throw \"a\") (throw \"b\") ]"), "2");
    // attrset values stay thunks
    assert_eq!(run("builtins.attrNames { a = throw \"x\"; b = 2; }"), r#"[ "a" "b" ]"#);
}

#[test]
fn infinite_recursion_blackhole() {
    let e = run_err("let x = x; in x");
    assert_eq!(e.kind, ErrorKind::InfiniteRecursion);
    // mutual cycle
    let e = run_err("let a = b; b = a; in a");
    assert_eq!(e.kind, ErrorKind::InfiniteRecursion);
    // not catchable by tryEval (03 §8)
    let e = run_err("builtins.tryEval (let x = x; in x)");
    assert_eq!(e.kind, ErrorKind::InfiniteRecursion);
}

#[test]
fn scoping() {
    assert_eq!(run("let x = 1; in let x = 2; in x"), "2");
    // let is mutually recursive, order irrelevant (03 §4.2)
    assert_eq!(run("let a = b + 1; b = 1; in a"), "2");
    // rec attrset
    assert_eq!(run("(rec { a = b + 1; b = 1; }).a"), "2");
    // non-rec binds do not see each other
    assert_eq!(run("let b = 10; in ({ b = 1; a = b; }).a"), "10");
    // inherit copies from outer scope, even in rec (03 §4.2)
    assert_eq!(run("let x = 5; in (rec { inherit x; y = x + 1; }).y"), "6");
    assert_eq!(run("let s = { v = 3; }; in ({ inherit (s) v; }).v"), "3");
    // shadowing initial-scope names is legal (03 §4.1)
    assert_eq!(run("let true = 0; in true"), "0");
}

#[test]
fn with_scoping() {
    assert_eq!(run("with { a = 1; }; a"), "1");
    // with is weaker than any lexical binding (03 §4.3)
    assert_eq!(run("let a = 1; in with { a = 2; }; a"), "1");
    // innermost with wins among withs
    assert_eq!(run("with { a = 1; }; with { a = 2; }; a"), "2");
    // a name reachable only through with and absent is undefined-variable
    assert_eq!(run_err("with { a = 1; }; b").kind, ErrorKind::UndefinedVar);
    assert_eq!(run_err("nosuch").kind, ErrorKind::UndefinedVar);
}

#[test]
fn lambdas_and_patterns() {
    assert_eq!(run("(x: x + 1) 2"), "3");
    assert_eq!(run("(x: y: x - y) 10 4"), "6");
    assert_eq!(run("({ a, b }: a + b) { a = 1; b = 2; }"), "3");
    // defaults may reference other formals (03 §3.2.2)
    assert_eq!(run("({ a, b ? a + 1 }: b) { a = 5; }"), "6");
    // @-binding sees the original argument including extras
    assert_eq!(run("(args@{ a, ... }: args.b) { a = 1; b = 7; }"), "7");
    // presence checked at application time even if unused (03 §3.2, normative)
    assert_eq!(run_err("({ a }: 1) {}").kind, ErrorKind::Type);
    // unexpected attribute without ellipsis
    assert_eq!(run_err("({ a }: a) { a = 1; b = 2; }").kind, ErrorKind::Type);
    // with ellipsis extras are fine
    assert_eq!(run("({ a, ... }: a) { a = 1; b = 2; }"), "1");
    // argument values stay lazy
    assert_eq!(run("({ a }: 1) { a = throw \"never\"; }"), "1");
    assert_eq!(run_err("1 2").kind, ErrorKind::Type);
}

#[test]
fn equality() {
    assert_eq!(run("1 == 1"), "true");
    assert_eq!(run("\"a\" == \"a\""), "true");
    assert_eq!(run("null == null"), "true");
    assert_eq!(run("[ 1 2 ] == [ 1 2 ]"), "true");
    assert_eq!(run("{ a = 1; } == { a = 1; }"), "true");
    assert_eq!(run("{ a = 1; } == { a = 2; }"), "false");
    // different types compare false, never an error (03 §7)
    assert_eq!(run("1 == \"1\""), "false");
    // path and string are never == (03 §7)
    assert_eq!(run("./x == \"/base/x\""), "false");
    assert_eq!(run("./x == ./x"), "true");
    // functions are never equal, even to themselves
    assert_eq!(run("let f = x: x; in f == f"), "false");
    assert_eq!(run("1 != 2"), "true");
}

#[test]
fn ordering() {
    assert_eq!(run("\"a\" < \"b\""), "true");
    assert_eq!(run("[ 1 2 ] < [ 1 3 ]"), "true");
    assert_eq!(run("[ 1 ] < [ 1 2 ]"), "true"); // shorter prefix is less
    assert_eq!(run_err("1 < \"a\"").kind, ErrorKind::Type);
    assert_eq!(run_err("true < false").kind, ErrorKind::Type);
}

#[test]
fn attrsets_ops() {
    assert_eq!(run("{ a = 1; b = 2; }.a"), "1");
    assert_eq!(run("{ a = 1; }.b or 42"), "42");
    assert_eq!(run("(1).b or 42"), "42"); // non-set select with default (03/Nix behavior)
    assert_eq!(run_err("{ a = 1; }.b").kind, ErrorKind::MissingAttr);
    assert_eq!(run("{ a.b = 1; } ? a.b"), "true");
    assert_eq!(run("{ a = 1; } ? b"), "false");
    // // is shallow, right biased (04 §4)
    assert_eq!(run("({ a = 1; n = { x = 1; }; } // { n = { y = 2; }; }).n.y"), "2");
    assert_eq!(run("(({ a = 1; n = { x = 1; }; } // { n = { y = 2; }; }).n) ? x"), "false");
    // dynamic attrs
    assert_eq!(run("let k = \"key\"; in { ${k} = 1; }.key"), "1");
    assert_eq!(run_err("let k = \"a\"; in { a = 1; ${k} = 2; }").kind, ErrorKind::Type);
}

#[test]
fn strings_and_interp() {
    assert_eq!(run(r#""a" + "b""#), r#""ab""#);
    assert_eq!(run(r#"let x = 3; in "n=${toString x}""#), r#""n=3""#);
    // int interpolates per the 04 §4.1 table
    assert_eq!(run(r#""n=${1}""#), r#""n=1""#);
    // bool/null/list are not coercible (04 §4.1, anti-footgun divergence)
    assert_eq!(run_err(r#""x${null}""#).kind, ErrorKind::Type);
    assert_eq!(run_err(r#""x${true}""#).kind, ErrorKind::Type);
    // `+` rhs is stricter than interpolation: no bare int (04 §4.3)
    assert_eq!(run_err(r#""a" + 1"#).kind, ErrorKind::Type);
    // __toString hook
    assert_eq!(run(r#"toString { __toString = self: "v" + self.x; x = "1"; }"#), r#""v1""#);
    // outPath fallback for path-like sets
    assert_eq!(run(r#"toString { outPath = "/some/where"; }"#), r#""/some/where""#);
}

#[test]
fn paths() {
    assert_eq!(run("./x"), "/base/x");
    // path + string → path, normalized (04 §2.4)
    assert_eq!(run("./a + \"/b/../c\""), "/base/a/c");
    // path + path → path
    assert_eq!(run("./a + ./b"), "/base/a/base/b");
    // string + path would ingest (string result) — needs a real file; see cdf tests
    assert_eq!(run_err("./a + 1").kind, ErrorKind::Type);
    assert_eq!(run("builtins.baseNameOf ./a/b.txt"), "\"b.txt\"");
    assert_eq!(run("builtins.dirOf ./a/b.txt"), "/base/a");
    assert_eq!(run("builtins.dirOf \"a/b\""), "\"a\"");
}

#[test]
fn errors_and_try_eval() {
    assert_eq!(run_err("throw \"boom\"").kind, ErrorKind::Throw);
    assert_eq!(run_err("abort \"boom\"").kind, ErrorKind::Abort);
    assert_eq!(run_err("assert 1 == 2; 3").kind, ErrorKind::Assert);
    assert_eq!(run("assert 1 == 1; 3"), "3");
    // tryEval catches throw/assert/type (07 §2.1)
    assert_eq!(
        run("(builtins.tryEval (throw \"x\")).success"),
        "false"
    );
    assert_eq!(run("(builtins.tryEval (assert false; 1)).value"), "false");
    assert_eq!(run("(builtins.tryEval (1 + \"a\")).success"), "false");
    assert_eq!(run("(builtins.tryEval 42).value"), "42");
    // abort is NOT catchable (03 §8 table, 07 §2.1)
    assert_eq!(run_err("builtins.tryEval (abort \"x\")").kind, ErrorKind::Abort);
}

#[test]
fn seq_and_deep_seq() {
    assert_eq!(run("builtins.seq 1 2"), "2");
    assert_eq!(run_err("builtins.seq (throw \"a\") 2").kind, ErrorKind::Throw);
    // seq forces to WHNF only: a throw inside a list spine survives
    assert_eq!(run("builtins.seq [ (throw \"a\") ] 2"), "2");
    // deepSeq forces recursively
    assert_eq!(run_err("builtins.deepSeq [ (throw \"a\") ] 2").kind, ErrorKind::Throw);
    assert_eq!(run("builtins.deepSeq [ 1 { a = 2; } ] 3"), "3");
}

#[test]
fn type_of_and_predicates() {
    assert_eq!(run("builtins.typeOf 1"), "\"int\"");
    assert_eq!(run("builtins.typeOf \"s\""), "\"string\"");
    assert_eq!(run("builtins.typeOf null"), "\"null\"");
    assert_eq!(run("builtins.typeOf true"), "\"bool\"");
    assert_eq!(run("builtins.typeOf ./x"), "\"path\"");
    assert_eq!(run("builtins.typeOf [ ]"), "\"list\"");
    assert_eq!(run("builtins.typeOf { }"), "\"set\"");
    assert_eq!(run("builtins.typeOf (x: x)"), "\"lambda\"");
    assert_eq!(run("builtins.isString \"a\""), "true");
    assert_eq!(run("isNull null"), "true");
    assert_eq!(run("builtins.isFunction builtins.map"), "true");
    // curried partial application is still a function
    assert_eq!(run("builtins.isFunction (builtins.map (x: x))"), "true");
}

#[test]
fn list_builtins() {
    assert_eq!(run("map (x: x * 2) [ 1 2 3 ]"), "[ 2 4 6 ]");
    assert_eq!(run("builtins.filter (x: x < 3) [ 1 2 3 4 ]"), "[ 1 2 ]");
    assert_eq!(run("builtins.elem 2 [ 1 2 3 ]"), "true");
    assert_eq!(run("builtins.elem 9 [ 1 2 3 ]"), "false");
    assert_eq!(run("builtins.concatLists [ [ 1 ] [ 2 3 ] [ ] ]"), "[ 1 2 3 ]");
    assert_eq!(run("builtins.foldl' (a: b: a + b) 0 [ 1 2 3 ]"), "6");
    assert_eq!(run("builtins.genList (i: i * i) 4"), "[ 0 1 4 9 ]");
    assert_eq!(run("builtins.sort builtins.lessThan [ 3 1 2 ]"), "[ 1 2 3 ]");
    assert_eq!(run("builtins.sort (a: b: a < b) [ \"b\" \"a\" ]"), "[ \"a\" \"b\" ]");
    assert_eq!(run("builtins.head [ 1 2 ]"), "1");
    assert_eq!(run("builtins.tail [ 1 2 3 ]"), "[ 2 3 ]");
    assert_eq!(run("builtins.elemAt [ 1 2 3 ] 1"), "2");
    assert_eq!(run("builtins.length [ 1 2 3 ]"), "3");
    assert_eq!(run("[ 1 ] ++ [ 2 ]"), "[ 1 2 ]");
    assert_eq!(run_err("builtins.head [ ]").kind, ErrorKind::Type);
    assert_eq!(run_err("builtins.elemAt [ 1 ] 5").kind, ErrorKind::Type);
    // map is lazy in elements
    assert_eq!(run("builtins.length (map (x: throw \"no\") [ 1 2 ])"), "2");
}

#[test]
fn attr_builtins() {
    assert_eq!(run("builtins.attrNames { b = 1; a = 2; }"), "[ \"a\" \"b\" ]"); // sorted bytewise
    assert_eq!(run("builtins.attrValues { b = 1; a = 2; }"), "[ 2 1 ]"); // attrNames order
    assert_eq!(run("builtins.getAttr \"a\" { a = 42; }"), "42");
    assert_eq!(run("builtins.hasAttr \"a\" { a = 1; }"), "true");
    assert_eq!(run("removeAttrs { a = 1; b = 2; } [ \"a\" ]"), "{ b = 2; }");
    assert_eq!(
        run("builtins.mapAttrs (name: v: name + toString v) { a = 1; }"),
        "{ a = \"a1\"; }"
    );
    assert_eq!(
        run("builtins.listToAttrs [ { name = \"a\"; value = 1; } { name = \"a\"; value = 2; } ]"),
        "{ a = 2; }" // later duplicate wins
    );
    assert_eq!(run_err("builtins.getAttr \"x\" { }").kind, ErrorKind::MissingAttr);
}

#[test]
fn string_builtins() {
    assert_eq!(run("builtins.stringLength \"abc\""), "3");
    assert_eq!(run("builtins.substring 1 2 \"abcd\""), "\"bc\"");
    assert_eq!(run("builtins.substring 2 100 \"abcd\""), "\"cd\""); // clamps
    assert_eq!(run("builtins.substring 9 1 \"ab\""), "\"\"");
    assert_eq!(
        run("builtins.replaceStrings [ \"a\" \"b\" ] [ \"1\" \"2\" ] \"abcab\""),
        "\"12c12\""
    );
    assert_eq!(run("builtins.concatStringsSep \", \" [ \"a\" \"b\" ]"), "\"a, b\"");
    assert_eq!(run("toString 42"), "\"42\"");
    assert_eq!(run("toString \"s\""), "\"s\"");
    assert_eq!(run_err("toString [ 1 ]").kind, ErrorKind::Type);
}

#[test]
fn file_imports() {
    // real files: hermetic import, directory default.shade, cycle detection
    let dir = std::env::temp_dir().join(format!("shadec-test-{}", std::process::id()));
    let sub = dir.join("lib");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(dir.join("a.shade"), "{ x = 1; y = import ./b.shade; }").unwrap();
    std::fs::write(dir.join("b.shade"), "40 + 2").unwrap();
    std::fs::write(sub.join("default.shade"), "{ v = \"from-dir\"; }").unwrap();
    std::fs::write(dir.join("cyc1.shade"), "import ./cyc2.shade").unwrap();
    std::fs::write(dir.join("cyc2.shade"), "import ./cyc1.shade").unwrap();
    // imports are hermetic: b.shade cannot see the importer's scope
    std::fs::write(dir.join("scope1.shade"), "let secret = 1; in import ./scope2.shade").unwrap();
    std::fs::write(dir.join("scope2.shade"), "secret").unwrap();

    let base = dir.to_str().unwrap();
    assert_eq!(run_in("(import ./a.shade).y", base).unwrap(), "42");
    assert_eq!(run_in("(import ./lib).v", base).unwrap(), "\"from-dir\"");
    // memoized by resolved path: same value, evaluated once
    assert_eq!(
        run_in("(import ./b.shade) + (import ./b.shade)", base).unwrap(),
        "84"
    );
    let e = run_in("import ./cyc1.shade", base).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Import);
    assert!(e.msg.contains("cycle"), "got: {}", e.msg);
    let e = run_in("import ./scope1.shade", base).unwrap_err();
    assert_eq!(e.kind, ErrorKind::UndefinedVar);

    // tracked reads
    std::fs::write(dir.join("data.txt"), "hello").unwrap();
    assert_eq!(run_in("builtins.readFile ./data.txt", base).unwrap(), "\"hello\"");
    assert_eq!(run_in("builtins.pathExists ./data.txt", base).unwrap(), "true");
    assert_eq!(run_in("builtins.pathExists ./nope.txt", base).unwrap(), "false");
    assert_eq!(
        run_in("(builtins.readDir ./.).\"data.txt\"", base).unwrap(),
        "\"regular\""
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Regression (evaluator import fix): a directory import resolves to
/// `<dir>/default.shade` **before** the import cache and the eval-input set see
/// it. So `import ./lib` and `import ./lib/default.shade` are the *same* import
/// — one cache entry, one recorded eval input, both the resolved file, never
/// the bare directory (06 §1 step 2). A regression that cached/tracked the
/// unresolved directory path would double-evaluate and record `file:<dir>`.
#[test]
fn import_resolves_directory_before_caching_and_tracking() {
    let dir = std::env::temp_dir().join(format!("shadec-import-{}", std::process::id()));
    let sub = dir.join("lib");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("default.shade"), "{ v = 7; }").unwrap();

    let base = dir.to_str().unwrap().to_string();
    let default_path = sub.join("default.shade").to_str().unwrap().to_string();
    let bare_dir = sub.to_str().unwrap().to_string();

    // Both spellings must coincide on the same value and the same import.
    let src = "(import ./lib).v + (import ./lib/default.shade).v".to_string();
    let (result, inputs, cache_keys) = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let io = HostIo;
            let mut ev = Evaluator::new(&io);
            let expr = shadec::parser::parse_str(&src, Arc::from("<test>"), &base).unwrap();
            let env = ev.initial_env();
            let v = ev.eval(&expr, &env).unwrap();
            let pos = shadec::error::Pos { file: Arc::from("<test>"), line: 0, col: 0 };
            let shown = shadec::print::show_value(&mut ev, &v, true, &pos).unwrap();
            let inputs: Vec<String> = ev.eval_inputs.iter().cloned().collect();
            let cache_keys: Vec<String> = ev.import_cache.keys().cloned().collect();
            (shown, inputs, cache_keys)
        })
        .unwrap()
        .join()
        .unwrap();

    assert_eq!(result, "14"); // 7 + 7 — one underlying value
    // Tracked as the resolved default.shade, never the bare directory.
    assert!(inputs.contains(&format!("file:{default_path}")), "inputs: {inputs:?}");
    assert!(
        !inputs.iter().any(|i| i == &format!("file:{bare_dir}")),
        "bare dir was tracked: {inputs:?}"
    );
    // Exactly one cache entry, keyed on the resolved file — dir ≡ file.
    assert!(cache_keys.contains(&default_path), "cache keys: {cache_keys:?}");
    assert!(!cache_keys.contains(&bare_dir), "cache keyed on bare dir: {cache_keys:?}");
    assert_eq!(
        cache_keys.iter().filter(|k| k.ends_with("/lib/default.shade")).count(),
        1,
        "directory import should not create a second cache entry: {cache_keys:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resource_limit() {
    let e = run_err("let f = x: f x; in f 1"); // diverges: depth guard trips
    assert_eq!(e.kind, ErrorKind::ResourceLimit);
}

#[test]
fn indented_string_eval() {
    assert_eq!(run("''\n  foo\n  bar\n''"), "\"foo\\nbar\\n\"");
    assert_eq!(run("let x = \"X\"; in ''\n  a${x}b\n''"), "\"aXb\\n\"");
}
