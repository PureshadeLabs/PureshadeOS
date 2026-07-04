# Shade — Grammar

The exact lexical and syntactic grammar of `.shade` files. Normative: an
implementation accepts exactly this language. Semantics are
[`03`](03-semantics.md); this doc is form only.

Notation: EBNF. `*` zero-or-more, `+` one-or-more, `?` optional, `|`
alternation, `( )` grouping, `" "` literal, `%xNN` byte value, `-` in a
character class is a range. Rule names are `kebab-case`; token names are
`UPPER`.

---

## 1. Source form

- Encoding: UTF-8, no BOM. Invalid UTF-8 is a parse error.
- A source file is one `expr` surrounded by optional whitespace/comments.
  Empty file (after stripping whitespace/comments) is a parse error.
- Newlines have no syntactic significance beyond whitespace.

## 2. Lexical grammar

### 2.1 Whitespace and comments

```
WS         = %x20 / %x09 / %x0A / %x0D
comment    = line-comment / block-comment
line-comment  = "#" *( any byte except %x0A ) ( %x0A / EOF )
block-comment = "/*" *( any byte ) "*/"          ; non-nesting, shortest match
```

Whitespace and comments may appear between any two tokens and are otherwise
ignored. An unterminated block comment is a parse error.

### 2.2 Keywords and reserved words

```
if  then  else  let  in  rec  with  inherit  assert  or
```

Keywords may not be used as identifiers. `or` is a keyword only in the
select-default position (§3.4) and as an operator-like token; it is still
reserved everywhere. `true`, `false`, `null` are **not** keywords — they
are ordinary identifiers bound in the initial scope
([`03 §4.1`](03-semantics.md#4-scoping)); shadowing them is legal and
inadvisable.

### 2.3 Identifiers

```
ID = ( ALPHA / "_" ) *( ALPHA / DIGIT / "_" / "'" / "-" )
ALPHA = %x41-5A / %x61-7A
DIGIT = %x30-39
```

Longest match. A `-` is part of an identifier only when the preceding
characters already form an identifier prefix and the `-` is followed by an
identifier character — i.e. `a-b` is one identifier; `a - b` and `a- b`
are subtractions. (Same disambiguation as Nix: the lexer prefers the
identifier interpretation when the character after `-` continues the
identifier.)

### 2.4 Integers

```
INT = DIGIT+
```

Value range and overflow behavior: [`04 §2.1`](04-values.md#2-primitives).
There is no negative literal; `-5` is unary minus applied to `5`. There
are no float, hex, octal, or underscore-separated literals.

### 2.5 Paths

```
PATH        = rel-path / abs-path
rel-path    = ( "." / ".." ) 1*( "/" path-seg )
abs-path    = 1*( "/" path-seg )
path-seg    = 1*( ALPHA / DIGIT / "." / "_" / "-" / "+" )
```

A path token must contain at least one `/`. `~/…` home paths and `<…>`
search paths are **not** part of the grammar (removed for purity,
[`01 §4`](01-overview.md#4-relation-to-nix-the-language)). Path semantics
(resolution base, ingestion): [`04 §2.4`](04-values.md#24-paths).

Lexing priority: at a position where both a path and another token could
start (e.g. `./x` vs `.` selection), the path wins if the input matches
`PATH`; division `a / b` requires whitespace or a non-path-continuation
around `/` exactly as in Nix — `a/b` where `a` ends an identifier and `b`
begins one lexes as a path only if the whole token matches `PATH` from its
start; since `PATH` cannot begin with `ALPHA`, `a/b` is a division. Only
tokens beginning `./`, `../`, or `/` are path candidates.

### 2.6 Strings

Two forms.

**Quoted string:**

```
STRING       = %x22 *str-part %x22                 ; "…"
str-part     = str-chars / interpolation / str-escape
str-chars    = 1*( any byte except %x22, %x5C, "${" start )
str-escape   = "\" ( %x22 / "\" / "n" / "r" / "t" / "$" )
interpolation = "${" expr "}"
```

Escapes: `\"` → `"`, `\\` → `\`, `\n` → LF, `\r` → CR, `\t` → TAB,
`\$` → `$` (suppresses interpolation; `\${` is a literal `${`). A `\`
before any other character is a parse error (divergence from Nix, which
passes unknown escapes through — silent passthrough hides typos). A bare
`$` not followed by `{` is literal.

**Indented string:**

```
IND-STRING   = "''" *ind-part "''"
ind-part     = ind-chars / interpolation / ind-escape
ind-escape   = "'''"        ; → literal ''
             / "''${"       ; → literal ${
             / "''\" ( "n" / "r" / "t" / "\" / "$" / "'" )   ; → LF CR TAB \ $ '
```

After parsing, **indentation stripping** is applied to the literal parts as
one unit (normative, matches Nix):

1. Split the raw content into lines at LF.
2. Compute the minimal indentation — the smallest count of leading spaces
   (only `%x20`; tabs are not indentation) over all lines that contain a
   character other than spaces. Lines that are entirely spaces, and the
   first line if the `''` opener is immediately followed by a newline, do
   not participate.
3. Remove that many leading spaces from every line (lines shorter than the
   minimum become empty).
4. If the first line is empty after the opener-newline, drop it. Trailing
   spaces-only last line (the closer's indentation) is dropped.

Interpolations count as non-space content at their position for step 2.

### 2.7 Operators and punctuation tokens

```
.  ?  ++  *  /  +  -  !  //  <  <=  >  >=  ==  !=  &&  ||  ->
=  ;  :  ,  @  (  )  [  ]  {  }  ...  ${
```

## 3. Syntactic grammar

Start symbol: `expr`.

```
expr            = lambda
                / "assert" expr ";" expr
                / "with" expr ";" expr
                / "let" binds "in" expr
                / "if" expr "then" expr "else" expr
                / expr-op

lambda          = ID ":" expr
                / pattern ":" expr
                / ID "@" pattern ":" expr
                / pattern "@" ID ":" expr

pattern         = "{" [ formals ] "}"
formals         = formal *( "," formal ) [ "," "..." ]
                / "..."
formal          = ID [ "?" expr ]
```

A duplicate `ID` among a pattern's formals (or between the formals and the
`@` binding) is a parse-time error.

### 3.1 Operator expressions

Precedence, tightest first. Associativity: L = left, R = right,
N = non-associative (chaining is a parse error).

| Lvl | Form | Assoc | Meaning ([`03`](03-semantics.md), [`04`](04-values.md)) |
|----:|------|-------|---------|
| 1 | `e . attrpath` `[ or e ]` | L | attribute selection, optional default |
| 2 | `e1 e2` | L | function application |
| 3 | `- e` | — | arithmetic negation |
| 4 | `e ? attrpath` | N | has-attribute test |
| 5 | `e1 ++ e2` | R | list concatenation |
| 6 | `e1 * e2`, `e1 / e2` | L | multiplication, integer division |
| 7 | `e1 + e2`, `e1 - e2` | L | addition/concatenation, subtraction |
| 8 | `! e` | — | boolean negation |
| 9 | `e1 // e2` | R | attrset update (right biased) |
| 10 | `e1 < e2`, `<=`, `>`, `>=` | N | ordering |
| 11 | `e1 == e2`, `!=` | N | equality |
| 12 | `e1 && e2` | L | boolean and (short-circuit) |
| 13 | `e1 \|\| e2` | L | boolean or (short-circuit) |
| 14 | `e1 -> e2` | R | boolean implication (`!e1 \|\| e2`) |

As grammar:

```
expr-op         = expr-impl
expr-impl       = expr-or [ "->" expr-impl ]
expr-or         = expr-and *( "||" expr-and )
expr-and        = expr-eq *( "&&" expr-eq )
expr-eq         = expr-rel [ ( "==" / "!=" ) expr-rel ]
expr-rel        = expr-update [ ( "<" / "<=" / ">" / ">=" ) expr-update ]
expr-update     = expr-not [ "//" expr-update ]
expr-not        = "!" expr-not / expr-add
expr-add        = expr-mul *( ( "+" / "-" ) expr-mul )
expr-mul        = expr-concat *( ( "*" / "/" ) expr-concat )
expr-concat     = expr-hasattr [ "++" expr-concat ]
expr-hasattr    = expr-neg [ "?" attrpath ]
expr-neg        = "-" expr-neg / expr-app
expr-app        = expr-select *( expr-select )       ; application by juxtaposition
expr-select     = expr-simple [ "." attrpath [ "or" expr-select ] ]
```

### 3.2 Simple expressions

```
expr-simple     = ID / INT / STRING / IND-STRING / PATH
                / "(" expr ")"
                / list
                / attrset
                / "rec" attrset

list            = "[" *expr-select "]"
```

List elements bind at select level: `[ f x ]` is a two-element list, not
an application. Parenthesize to apply: `[ (f x) ]`.

### 3.3 Attribute sets

```
attrset         = "{" *bind "}"
bind            = attrpath "=" expr ";"
                / "inherit" *attr ";"
                / "inherit" "(" expr ")" *attr ";"

attrpath        = attr *( "." attr )
attr            = ID / STRING / "${" expr "}"
```

- `STRING` attrs here must be interpolation-free or their interpolations
  are evaluated like `${…}` attrs (dynamic attributes).
- Dynamic attributes (`${…}` and interpolated strings) are permitted in
  non-`rec` attrset binds only. In `rec` attrsets and in `let` binds they
  are a parse-time error (their names cannot participate in the recursive
  scope).
- Nested `attrpath` binds (`a.b.c = 1;`) desugar to nested attrsets;
  two binds sharing a prefix merge iff every shared level is a plain
  (non-dynamic) attr and no level is bound to a non-attrset expression —
  otherwise a duplicate-attribute error at parse time. Exact duplicate
  names are always an error.
- `inherit x;` ≡ `x = x;` (from lexical scope, [`03 §4`](03-semantics.md#4-scoping));
  `inherit (e) x y;` ≡ `x = e.x; y = e.y;`. `inherit` attrs must be plain
  `ID`s.

### 3.4 `let`

```
binds           = 1*bind        ; same bind rule, dynamic attrs forbidden
```

`let … in e` — all binds are recursive ([`03 §4.2`](03-semantics.md#4-scoping)).
`let { … }` (old Nix form) does not exist.

### 3.5 Select default

`e.a.b or d` — if any step of the attrpath is missing, the value is `d`.
The `or` binds to the nearest preceding selection (level 1).

## 4. Ambiguity notes (normative resolutions)

1. `{` opens either an attrset or a lambda pattern. Resolution: parse
   ahead — it is a pattern iff the brace's content matches `formals`
   **and** the closing `}` is followed by `:` or `@`. Empty `{}` followed
   by `:` is the empty pattern; otherwise `{}` is the empty attrset.
2. `e1 e2` application vs `e1` then unrelated token: application requires
   `e2` to start a valid `expr-select`; keywords terminate application.
3. `or` after a selection is the default form; anywhere else `or` is
   reserved and a parse error (it is not a general identifier).
4. `-` prefix vs infix resolved by grammar position (`expr-neg` under
   `expr-mul` operand position).

## 5. Divergences from Nix syntax (summary)

Removed: floats, `~/` paths, `<search>` paths, URI literals, `let { }`,
`__curPos`/positions-as-syntax, `|>`/`<|` pipes. Changed: unknown string
escapes are errors. Everything else is intended to parse exactly as the
corresponding Nix construct; where this doc and observed Nix behavior
disagree for a construct both accept, **this doc wins** (no bug-for-bug
compatibility, [`01 §2`](01-overview.md#2-non-goals)).
