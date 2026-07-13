//! Golden AST tests for the full grammar (docs/shade/02-grammar.md).
//! Each case is (source, expected s-expression). The s-expr printer is
//! `ast::to_sexpr`; goldens pin both parse shape and precedence.

use std::sync::Arc;

fn ast(src: &str) -> String {
    let e = shadec::parser::parse_str(src, Arc::from("<test>"), "/base")
        .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e}"));
    shadec::ast::to_sexpr(&e)
}

fn parse_err(src: &str) -> String {
    match shadec::parser::parse_str(src, Arc::from("<test>"), "/base") {
        Ok(e) => panic!("expected parse error for {src:?}, got {}", shadec::ast::to_sexpr(&e)),
        Err(e) => e.msg,
    }
}

#[test]
fn literals() {
    assert_eq!(ast("42"), "(int 42)");
    assert_eq!(ast("x"), "(var x)");
    assert_eq!(ast("true"), "(var true)"); // ordinary identifier (02 §2.2)
    assert_eq!(ast(r#""hi""#), r#"(str "hi")"#);
    assert_eq!(ast(r#""""#), "(str)");
    assert_eq!(ast(r#""a\n\t\"\\\$b""#), "(str \"a\\n\\t\\\"\\\\$b\")");
    assert_eq!(ast(r#""a${x}b""#), r#"(str "a" (interp (var x)) "b")"#);
    // bare $ not followed by { is literal (02 §2.6)
    assert_eq!(ast(r#""a$b""#), r#"(str "a$b")"#);
}

#[test]
fn identifiers_with_dashes() {
    // a-b is one identifier; a - b and a- b are subtractions (02 §2.3)
    assert_eq!(ast("a-b"), "(var a-b)");
    assert_eq!(ast("a - b"), "(- (var a) (var b))");
    assert_eq!(ast("a- b"), "(- (var a) (var b))");
    assert_eq!(ast("lythos-libstd"), "(var lythos-libstd)");
}

#[test]
fn paths() {
    // relative paths resolve against the file's directory at parse time (04 §2.4)
    assert_eq!(ast("./x"), "(path /base/x)");
    assert_eq!(ast("../y/z"), "(path /y/z)");
    assert_eq!(ast("/etc/foo"), "(path /etc/foo)");
    assert_eq!(ast("./a/../b"), "(path /base/b)");
    // a/b is a division, not a path (02 §2.5)
    assert_eq!(ast("a/b"), "(/ (var a) (var b))");
    assert_eq!(ast("a / b"), "(/ (var a) (var b))");
}

#[test]
fn indented_strings() {
    assert_eq!(ast("''\n  foo\n  bar\n''"), "(str \"foo\\nbar\\n\")");
    assert_eq!(ast("''\n  foo\n    bar\n''"), "(str \"foo\\n  bar\\n\")");
    // interpolation counts as non-space content (02 §2.6 step 2)
    assert_eq!(ast("''\n  a${x}\n''"), "(str \"a\" (interp (var x)) \"\\n\")");
    // ''' escape and ''${ escape
    assert_eq!(ast("''a'''b''"), "(str \"a''b\")");
    assert_eq!(ast("''a''${x}b''"), "(str \"a${x}b\")");
}

#[test]
fn precedence_table() {
    // levels 6/7: * binds tighter than +
    assert_eq!(ast("1 + 2 * 3"), "(+ (int 1) (* (int 2) (int 3)))");
    // level 2: application binds tighter than arithmetic
    assert_eq!(ast("f x + 1"), "(+ (apply (var f) (var x)) (int 1))");
    // level 3: unary minus under multiplication operand position
    assert_eq!(ast("-a * b"), "(* (neg (var a)) (var b))");
    // level 5: ++ right-assoc, tighter than *
    assert_eq!(ast("a ++ b ++ c"), "(++ (var a) (++ (var b) (var c)))");
    // level 9: // right-assoc (right biased)
    assert_eq!(ast("a // b // c"), "(// (var a) (// (var b) (var c)))");
    // level 8: ! above //
    assert_eq!(ast("!a // b"), "(// (not (var a)) (var b))");
    // level 14: -> right-assoc, loosest
    assert_eq!(ast("a -> b -> c"), "(-> (var a) (-> (var b) (var c)))");
    assert_eq!(ast("a || b -> c"), "(-> (|| (var a) (var b)) (var c))");
    // levels 12/13
    assert_eq!(ast("a && b || c"), "(|| (&& (var a) (var b)) (var c))");
    // level 1: select tighter than application
    assert_eq!(ast("f a.b"), "(apply (var f) (select (var a) [b]))");
    // level 4: has-attr
    assert_eq!(ast("a ? b.c"), "(hasattr (var a) [b c])");
}

#[test]
fn non_associative_levels() {
    assert!(parse_err("a < b < c").contains("non-associative"));
    assert!(parse_err("a == b == c").contains("non-associative"));
    assert!(parse_err("a ? b ? c").contains("non-associative"));
}

#[test]
fn select_and_default() {
    assert_eq!(ast("a.b.c"), "(select (var a) [b c])");
    assert_eq!(ast("a.b or 1"), "(select (var a) [b] or (int 1))");
    assert_eq!(ast(r#"a."b c""#), "(select (var a) [b c])");
    assert_eq!(ast("a.${x}"), "(select (var a) [(dyn (var x))])");
    // `or` binds to the nearest preceding selection (02 §3.5)
    assert_eq!(
        ast("f a.b or c"),
        "(apply (var f) (select (var a) [b] or (var c)))"
    );
}

#[test]
fn lists_bind_at_select_level() {
    assert_eq!(ast("[ f x ]"), "(list (var f) (var x))"); // two elements, no application
    assert_eq!(ast("[ (f x) ]"), "(list (apply (var f) (var x)))");
    assert_eq!(ast("[ a.b 1 ./p ]"), "(list (select (var a) [b]) (int 1) (path /base/p))");
    assert_eq!(ast("[]"), "(list)");
}

#[test]
fn attrsets() {
    assert_eq!(ast("{}"), "(attrs)");
    assert_eq!(ast("{ a = 1; b = 2; }"), "(attrs (a = (int 1)) (b = (int 2)))");
    assert_eq!(ast("rec { a = 1; }"), "(rec-attrs (a = (int 1)))");
    // nested attrpath desugar + prefix merge (02 §3.3)
    assert_eq!(
        ast("{ a.b = 1; a.c = 2; }"),
        "(attrs (a = (attrs (b = (int 1)) (c = (int 2)))))"
    );
    // merge into a plain attrset literal
    assert_eq!(
        ast("{ a = { b = 1; }; a.c = 2; }"),
        "(attrs (a = (attrs (b = (int 1)) (c = (int 2)))))"
    );
    // inherit forms
    assert_eq!(ast("{ inherit x y; }"), "(attrs (inherit x) (inherit y))");
    assert_eq!(
        ast("{ inherit (e) x; }"),
        "(attrs (inherit-from (var e) x))"
    );
    // dynamic attrs in non-rec
    assert_eq!(ast("{ ${k} = 1; }"), "(attrs (dyn (var k) = (int 1)))");
    assert_eq!(
        ast(r#"{ "a${x}" = 1; }"#),
        "(attrs (dyn (str \"a\" (interp (var x))) = (int 1)))"
    );
    // interpolation-free string attr is static
    assert_eq!(ast(r#"{ "a b" = 1; }"#), "(attrs (a b = (int 1)))");
}

#[test]
fn attrset_errors() {
    assert!(parse_err("{ a = 1; a = 2; }").contains("duplicate"));
    assert!(parse_err("{ a = 1; a.b = 2; }").contains("duplicate"));
    assert!(parse_err("rec { ${x} = 1; }").contains("dynamic"));
    assert!(parse_err("let ${x} = 1; in x").contains("dynamic"));
    assert!(parse_err("{ a = rec { b = 1; }; a.c = 2; }").contains("duplicate"));
}

#[test]
fn let_with_assert_if() {
    assert_eq!(
        ast("let x = 1; in x"),
        "(let (rec-attrs (x = (int 1))) (var x))"
    );
    assert_eq!(
        ast("let inherit (e) x; in x"),
        "(let (rec-attrs (inherit-from (var e) x)) (var x))"
    );
    assert_eq!(ast("with e; x"), "(with (var e) (var x))");
    assert_eq!(ast("assert c; x"), "(assert (var c) (var x))");
    assert_eq!(
        ast("if c then 1 else 2"),
        "(if (var c) (int 1) (int 2))"
    );
    assert!(parse_err("let in x").contains("at least one"));
}

#[test]
fn lambdas() {
    assert_eq!(ast("x: x"), "(lambda x (var x))");
    assert_eq!(ast("x: y: x"), "(lambda x (lambda y (var x)))");
    assert_eq!(ast("{}: 1"), "(lambda {} (int 1))");
    assert_eq!(
        ast("{ a, b ? 1, ... }: a"),
        "(lambda {a, b ? (int 1), ...} (var a))"
    );
    assert_eq!(ast("args@{ a }: a"), "(lambda {a} @ args (var a))");
    assert_eq!(ast("{ a }@args: a"), "(lambda {a} @ args (var a))");
    // empty {} followed by : is the empty pattern; else empty attrset (02 §4.1)
    assert_eq!(ast("{}"), "(attrs)");
    assert!(parse_err("{ a, a }: a").contains("duplicate formal"));
    assert!(parse_err("a@{ a }: a").contains("duplicate formal"));
}

#[test]
fn application_chains() {
    assert_eq!(ast("f a b"), "(apply (apply (var f) (var a)) (var b))");
    // keywords terminate application (02 §4.2)
    assert_eq!(
        ast("if f x then 1 else 2"),
        "(if (apply (var f) (var x)) (int 1) (int 2))"
    );
    // rec attrset is a valid application argument (expr-simple)
    assert_eq!(
        ast("f rec { a = 1; }"),
        "(apply (var f) (rec-attrs (a = (int 1))))"
    );
}

#[test]
fn comments() {
    assert_eq!(ast("1 # line\n"), "(int 1)");
    assert_eq!(ast("/* block */ 1"), "(int 1)");
    assert!(parse_err("/* unterminated").contains("block comment"));
}

#[test]
fn misc_errors() {
    assert!(!parse_err("").is_empty()); // empty file is a parse error (02 §1)
    assert!(parse_err(r#""a\qb""#).contains("escape")); // unknown escape errors (02 §2.6)
    assert!(!parse_err("or").is_empty()); // `or` reserved outside select-default
}
