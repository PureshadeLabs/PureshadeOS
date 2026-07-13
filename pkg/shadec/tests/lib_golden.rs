//! Golden tests for the MVP `lib` surface (07 §3) — lib is Shade code, so
//! these also exercise file imports (06 §2) end to end.

use std::sync::Arc;

use shadec::eval::Evaluator;
use shadec::io::HostIo;

fn run(src: &str) -> String {
    let src = format!(
        "let lib = import {}/lib; in {src}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let io = HostIo;
            let mut ev = Evaluator::new(&io);
            let expr = shadec::parser::parse_str(&src, Arc::from("<test>"), "/base")
                .unwrap_or_else(|e| panic!("parse: {e}"));
            let env = ev.initial_env();
            let v = ev
                .eval(&expr, &env)
                .unwrap_or_else(|e| panic!("eval failed: {e}"));
            let pos = shadec::error::Pos { file: Arc::from("<test>"), line: 0, col: 0 };
            shadec::print::show_value(&mut ev, &v, true, &pos).unwrap()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn strings() {
    assert_eq!(run(r#"lib.hasPrefix "foo" "foobar""#), "true");
    assert_eq!(run(r#"lib.hasPrefix "bar" "foobar""#), "false");
    assert_eq!(run(r#"lib.hasSuffix "bar" "foobar""#), "true");
    assert_eq!(run(r#"lib.hasSuffix "foobar-long" "bar""#), "false"); // suffix longer than s
    assert_eq!(run(r#"lib.optionalString true "x""#), "\"x\"");
    assert_eq!(run(r#"lib.optionalString false "x""#), "\"\"");
    assert_eq!(run(r#"lib.boolToString false"#), "\"false\"");
    assert_eq!(run(r#"lib.splitString "," "a,b,c""#), r#"[ "a" "b" "c" ]"#);
    assert_eq!(run(r#"lib.splitString ", " "a, b""#), r#"[ "a" "b" ]"#);
    assert_eq!(run(r#"lib.splitString "," "abc""#), r#"[ "abc" ]"#);
    assert_eq!(run(r#"lib.splitString "," ",a,""#), r#"[ "" "a" "" ]"#);
    assert_eq!(run(r#"lib.strings.concatStringsSep "-" [ "a" "b" ]"#), "\"a-b\"");
}

#[test]
fn lists() {
    assert_eq!(run("lib.optional true 1"), "[ 1 ]");
    assert_eq!(run("lib.optional false 1"), "[ ]");
    assert_eq!(run("lib.optionals true [ 1 2 ]"), "[ 1 2 ]");
    assert_eq!(run("lib.range 2 5"), "[ 2 3 4 5 ]");
    assert_eq!(run("lib.range 3 2"), "[ ]");
    assert_eq!(run("lib.flatten [ 1 [ 2 [ 3 4 ] ] [ ] 5 ]"), "[ 1 2 3 4 5 ]");
    assert_eq!(run("lib.unique [ 1 2 1 3 2 ]"), "[ 1 2 3 ]");
    assert_eq!(run("lib.last [ 1 2 3 ]"), "3");
}

#[test]
fn attrsets() {
    assert_eq!(
        run(r#"lib.filterAttrs (n: v: v > 1) { a = 1; b = 2; c = 3; }"#),
        "{ b = 2; c = 3; }"
    );
    assert_eq!(run("lib.optionalAttrs true { a = 1; }"), "{ a = 1; }");
    assert_eq!(run("lib.optionalAttrs false { a = 1; }"), "{ }");
    assert_eq!(
        run("lib.recursiveUpdate { n = { x = 1; y = 2; }; k = 1; } { n = { y = 9; }; }"),
        "{ k = 1; n = { x = 1; y = 9; }; }"
    );
    assert_eq!(run(r#"lib.attrByPath [ "a" "b" ] 42 { a = { b = 7; }; }"#), "7");
    assert_eq!(run(r#"lib.attrByPath [ "a" "z" ] 42 { a = { b = 7; }; }"#), "42");
    assert_eq!(run(r#"lib.nameValuePair "k" 1"#), "{ name = \"k\"; value = 1; }");
    assert_eq!(
        run(r#"lib.mapAttrsToList (n: v: "${n}=${toString v}") { a = 1; b = 2; }"#),
        r#"[ "a=1" "b=2" ]"#
    );
}

#[test]
fn trivial_and_derivation_helpers() {
    assert_eq!(run("lib.id 5"), "5");
    assert_eq!(run("lib.const 1 2"), "1");
    assert_eq!(run("lib.flip (a: b: a - b) 1 10"), "9");
    assert_eq!(run("lib.isDerivation { type = \"derivation\"; }"), "true");
    assert_eq!(run("lib.isDerivation { }"), "false");
    assert_eq!(run("lib.isDerivation 4"), "false");
    // the literal build sigil, never expanded at eval time (05 §3.1)
    assert_eq!(run(r#"lib.placeholder "out""#), "\"$out\"");
    assert_eq!(run(r#""install foo ${lib.placeholder "out"}/bin/foo""#), "\"install foo $out/bin/foo\"");
}

#[test]
fn import_json_toml_stubbed() {
    // TODO(open) stubs must throw, not silently misbehave
    let src = format!(
        "let lib = import {}/lib; in (builtins.tryEval (lib.importJSON ./x.json)).success",
        env!("CARGO_MANIFEST_DIR")
    );
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let io = HostIo;
            let mut ev = Evaluator::new(&io);
            let expr = shadec::parser::parse_str(&src, Arc::from("<t>"), "/base").unwrap();
            let env = ev.initial_env();
            let v = ev.eval(&expr, &env).unwrap();
            let pos = shadec::error::Pos { file: Arc::from("<t>"), line: 0, col: 0 };
            assert_eq!(shadec::print::show_value(&mut ev, &v, true, &pos).unwrap(), "false");
        })
        .unwrap()
        .join()
        .unwrap();
}
