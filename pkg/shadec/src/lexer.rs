//! Lexical grammar per `docs/shade/02-grammar.md` §2.
//!
//! String literals are lexed into parts: literal text and interpolations,
//! where an interpolation is a nested token stream (the parser recurses into
//! it). Indented strings have indentation stripping applied here (02 §2.6,
//! normative four-step algorithm).

use alloc::sync::Arc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};

use crate::error::{ErrorKind, EvalError, Pos, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    If,
    Then,
    Else,
    Let,
    In,
    Rec,
    With,
    Inherit,
    Assert,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    Interp(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Id(String),
    Int(i64),
    /// Raw path text as written (`./x`, `../y`, `/a/b`); resolution against
    /// the file's directory happens in the parser (04 §2.4: parse time).
    Path(String),
    Str(Vec<StrPart>),
    IndStr(Vec<StrPart>),
    Kw(Kw),
    Dot,
    Question,
    Concat,      // ++
    Star,
    Slash,
    Plus,
    Minus,
    Not,         // !
    Update,      // //
    Lt,
    Le,
    Gt,
    Ge,
    Eq,          // ==
    Ne,          // !=
    And,         // &&
    Or,          // ||
    Impl,        // ->
    Assign,      // =
    Semi,
    Colon,
    Comma,
    At,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Ellipsis,    // ...
    DollarBrace, // ${ in attribute-name position
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
}

pub fn lex(src: &str, file: Arc<str>) -> Result<Vec<Token>> {
    let mut lx = Lexer {
        src: src.as_bytes(),
        i: 0,
        line: 1,
        col: 1,
        file,
        prev_end: usize::MAX,
        prev_operandish: false,
    };
    let mut out = Vec::new();
    loop {
        let t = lx.next_token()?;
        let end = matches!(t.tok, Tok::Eof);
        out.push(t);
        if end {
            return Ok(out);
        }
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    i: usize,
    line: u32,
    col: u32,
    file: Arc<str>,
    /// Byte offset just past the previous token, and whether that token can
    /// end an operand (Id/Int/Path/Str/`)`/`]`/`}`). Used for the `/`
    /// path-vs-division rule (02 §2.5): `a/b` is a division; a `/` opens a
    /// path only when not glued to a preceding operand.
    prev_end: usize,
    prev_operandish: bool,
}

impl<'a> Lexer<'a> {
    fn pos(&self) -> Pos {
        Pos { file: self.file.clone(), line: self.line, col: self.col }
    }

    fn err(&self, msg: impl Into<String>) -> EvalError {
        EvalError::at(ErrorKind::Parse, msg, &self.pos())
    }

    fn peek(&self, off: usize) -> Option<u8> {
        self.src.get(self.i + off).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.src.get(self.i).copied()?;
        self.i += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn bump_n(&mut self, n: usize) {
        for _ in 0..n {
            self.bump();
        }
    }

    fn skip_ws_comments(&mut self) -> Result<()> {
        loop {
            match self.peek(0) {
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => {
                    self.bump();
                }
                Some(b'#') => {
                    while let Some(b) = self.peek(0) {
                        if b == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some(b'/') if self.peek(1) == Some(b'*') => {
                    let start = self.pos();
                    self.bump_n(2);
                    // Non-nesting, shortest match; unterminated is a parse error.
                    loop {
                        match self.peek(0) {
                            None => {
                                return Err(EvalError::at(
                                    ErrorKind::Parse,
                                    "unterminated block comment",
                                    &start,
                                ));
                            }
                            Some(b'*') if self.peek(1) == Some(b'/') => {
                                self.bump_n(2);
                                break;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn next_token(&mut self) -> Result<Token> {
        self.skip_ws_comments()?;
        let pos = self.pos();
        let start = self.i;
        let Some(b) = self.peek(0) else {
            return Ok(Token { tok: Tok::Eof, pos });
        };
        let glued = self.prev_end == self.i && self.prev_operandish;
        let tok = match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => self.lex_ident(),
            b'0'..=b'9' => self.lex_int()?,
            b'"' => {
                self.bump();
                Tok::Str(self.lex_quoted()?)
            }
            b'\'' if self.peek(1) == Some(b'\'') => {
                self.bump_n(2);
                Tok::IndStr(self.lex_indented()?)
            }
            b'.' => {
                if self.peek(1) == Some(b'/')
                    || (self.peek(1) == Some(b'.') && self.peek(2) == Some(b'/'))
                {
                    self.lex_path()?
                } else if self.peek(1) == Some(b'.') && self.peek(2) == Some(b'.') {
                    self.bump_n(3);
                    Tok::Ellipsis
                } else {
                    self.bump();
                    Tok::Dot
                }
            }
            b'/' => {
                // `/*` was consumed as a comment above; here `/` is either a
                // path start or division. Only a non-glued `/` followed by a
                // path segment character opens a path (02 §2.5).
                if !glued && self.peek(1).is_some_and(is_path_seg_char) {
                    self.lex_path()?
                } else if self.peek(1) == Some(b'/') {
                    self.bump_n(2);
                    Tok::Update
                } else {
                    self.bump();
                    Tok::Slash
                }
            }
            b'+' => {
                if self.peek(1) == Some(b'+') {
                    self.bump_n(2);
                    Tok::Concat
                } else {
                    self.bump();
                    Tok::Plus
                }
            }
            b'-' => {
                if self.peek(1) == Some(b'>') {
                    self.bump_n(2);
                    Tok::Impl
                } else {
                    self.bump();
                    Tok::Minus
                }
            }
            b'*' => {
                self.bump();
                Tok::Star
            }
            b'!' => {
                if self.peek(1) == Some(b'=') {
                    self.bump_n(2);
                    Tok::Ne
                } else {
                    self.bump();
                    Tok::Not
                }
            }
            b'<' => {
                if self.peek(1) == Some(b'=') {
                    self.bump_n(2);
                    Tok::Le
                } else {
                    self.bump();
                    Tok::Lt
                }
            }
            b'>' => {
                if self.peek(1) == Some(b'=') {
                    self.bump_n(2);
                    Tok::Ge
                } else {
                    self.bump();
                    Tok::Gt
                }
            }
            b'=' => {
                if self.peek(1) == Some(b'=') {
                    self.bump_n(2);
                    Tok::Eq
                } else {
                    self.bump();
                    Tok::Assign
                }
            }
            b'&' => {
                if self.peek(1) == Some(b'&') {
                    self.bump_n(2);
                    Tok::And
                } else {
                    return Err(self.err("unexpected `&` (did you mean `&&`?)"));
                }
            }
            b'|' => {
                if self.peek(1) == Some(b'|') {
                    self.bump_n(2);
                    Tok::Or
                } else {
                    return Err(self.err("unexpected `|` (did you mean `||`?)"));
                }
            }
            b'$' => {
                if self.peek(1) == Some(b'{') {
                    self.bump_n(2);
                    Tok::DollarBrace
                } else {
                    return Err(self.err("unexpected `$` outside a string"));
                }
            }
            b'?' => {
                self.bump();
                Tok::Question
            }
            b';' => {
                self.bump();
                Tok::Semi
            }
            b':' => {
                self.bump();
                Tok::Colon
            }
            b',' => {
                self.bump();
                Tok::Comma
            }
            b'@' => {
                self.bump();
                Tok::At
            }
            b'(' => {
                self.bump();
                Tok::LParen
            }
            b')' => {
                self.bump();
                Tok::RParen
            }
            b'[' => {
                self.bump();
                Tok::LBracket
            }
            b']' => {
                self.bump();
                Tok::RBracket
            }
            b'{' => {
                self.bump();
                Tok::LBrace
            }
            b'}' => {
                self.bump();
                Tok::RBrace
            }
            _ => return Err(self.err(format!("unexpected character {:?}", b as char))),
        };
        self.prev_end = self.i;
        self.prev_operandish = matches!(
            tok,
            Tok::Id(_)
                | Tok::Int(_)
                | Tok::Path(_)
                | Tok::Str(_)
                | Tok::IndStr(_)
                | Tok::RParen
                | Tok::RBracket
                | Tok::RBrace
        );
        let _ = start;
        Ok(Token { tok, pos })
    }

    fn lex_ident(&mut self) -> Tok {
        let start = self.i;
        self.bump(); // first char already validated
        loop {
            match self.peek(0) {
                Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'\'' => {
                    self.bump();
                }
                // `-` continues an identifier only when followed by an
                // identifier character (02 §2.3); `a- b` stays a subtraction.
                Some(b'-')
                    if self
                        .peek(1)
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'\'') =>
                {
                    self.bump();
                }
                _ => break,
            }
        }
        let text = core::str::from_utf8(&self.src[start..self.i]).unwrap().to_string();
        match text.as_str() {
            "if" => Tok::Kw(Kw::If),
            "then" => Tok::Kw(Kw::Then),
            "else" => Tok::Kw(Kw::Else),
            "let" => Tok::Kw(Kw::Let),
            "in" => Tok::Kw(Kw::In),
            "rec" => Tok::Kw(Kw::Rec),
            "with" => Tok::Kw(Kw::With),
            "inherit" => Tok::Kw(Kw::Inherit),
            "assert" => Tok::Kw(Kw::Assert),
            "or" => Tok::Kw(Kw::Or),
            _ => Tok::Id(text),
        }
    }

    fn lex_int(&mut self) -> Result<Tok> {
        let start = self.i;
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
        let text = core::str::from_utf8(&self.src[start..self.i]).unwrap();
        // 04 §2.1 fixes the value range at i64; a literal outside it cannot
        // be represented, so it is a parse error (arithmetic overflow wraps,
        // but a literal is not arithmetic).
        text.parse::<i64>()
            .map(Tok::Int)
            .map_err(|_| self.err(format!("integer literal out of range: {text}")))
    }

    fn lex_path(&mut self) -> Result<Tok> {
        let start = self.i;
        // prefix: "." or ".." (relative) or nothing (absolute, starts at /)
        if self.peek(0) == Some(b'.') {
            self.bump();
            if self.peek(0) == Some(b'.') {
                self.bump();
            }
        }
        let mut any_seg = false;
        while self.peek(0) == Some(b'/') && self.peek(1).is_some_and(is_path_seg_char) {
            self.bump(); // '/'
            while self.peek(0).is_some_and(is_path_seg_char) {
                self.bump();
            }
            any_seg = true;
        }
        if !any_seg {
            return Err(self.err("malformed path literal"));
        }
        let text = core::str::from_utf8(&self.src[start..self.i]).unwrap().to_string();
        Ok(Tok::Path(text))
    }

    /// Sub-lex a `${…}` interpolation body: collect tokens until the
    /// matching `}` (tracking nested braces), excluding it.
    fn lex_interp(&mut self) -> Result<Vec<Token>> {
        let open = self.pos();
        let mut depth = 0usize;
        let mut toks = Vec::new();
        loop {
            let t = self.next_token()?;
            match t.tok {
                Tok::Eof => {
                    return Err(EvalError::at(
                        ErrorKind::Parse,
                        "unterminated interpolation",
                        &open,
                    ));
                }
                Tok::LBrace | Tok::DollarBrace => {
                    depth += 1;
                    toks.push(t);
                }
                Tok::RBrace => {
                    if depth == 0 {
                        return Ok(toks);
                    }
                    depth -= 1;
                    toks.push(t);
                }
                _ => toks.push(t),
            }
        }
    }

    fn lex_quoted(&mut self) -> Result<Vec<StrPart>> {
        let open = self.pos();
        let mut parts: Vec<StrPart> = Vec::new();
        let mut lit: Vec<u8> = Vec::new();
        loop {
            match self.peek(0) {
                None => {
                    return Err(EvalError::at(ErrorKind::Parse, "unterminated string", &open));
                }
                Some(b'"') => {
                    self.bump();
                    break;
                }
                Some(b'\\') => {
                    self.bump();
                    let e = self.peek(0).ok_or_else(|| self.err("unterminated escape"))?;
                    // Exhaustive escape set; anything else is a parse error
                    // (02 §2.6 — divergence from Nix, silent passthrough hides typos).
                    let c = match e {
                        b'"' => b'"',
                        b'\\' => b'\\',
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'$' => b'$',
                        other => {
                            return Err(
                                self.err(format!("invalid string escape `\\{}`", other as char))
                            );
                        }
                    };
                    self.bump();
                    lit.push(c);
                }
                Some(b'$') if self.peek(1) == Some(b'{') => {
                    self.bump_n(2);
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(bytes_to_string(core::mem::take(&mut lit))));
                    }
                    parts.push(StrPart::Interp(self.lex_interp()?));
                }
                Some(b) => {
                    self.bump();
                    lit.push(b);
                }
            }
        }
        if !lit.is_empty() {
            parts.push(StrPart::Lit(bytes_to_string(lit)));
        }
        Ok(parts)
    }

    fn lex_indented(&mut self) -> Result<Vec<StrPart>> {
        let open = self.pos();
        let mut parts: Vec<StrPart> = Vec::new();
        let mut lit: Vec<u8> = Vec::new();
        loop {
            match self.peek(0) {
                None => {
                    return Err(EvalError::at(
                        ErrorKind::Parse,
                        "unterminated indented string",
                        &open,
                    ));
                }
                Some(b'\'') if self.peek(1) == Some(b'\'') => {
                    match self.peek(2) {
                        // ''' → literal ''
                        Some(b'\'') => {
                            self.bump_n(3);
                            lit.extend_from_slice(b"''");
                        }
                        // ''${ → literal ${
                        Some(b'$') if self.peek(3) == Some(b'{') => {
                            self.bump_n(4);
                            lit.extend_from_slice(b"${");
                        }
                        // ''\X → escaped char
                        Some(b'\\') => {
                            self.bump_n(3);
                            let e =
                                self.peek(0).ok_or_else(|| self.err("unterminated escape"))?;
                            let c = match e {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'\\' => b'\\',
                                b'$' => b'$',
                                b'\'' => b'\'',
                                other => {
                                    return Err(self.err(format!(
                                        "invalid indented-string escape `''\\{}`",
                                        other as char
                                    )));
                                }
                            };
                            self.bump();
                            lit.push(c);
                        }
                        // plain '' → end of string
                        _ => {
                            self.bump_n(2);
                            break;
                        }
                    }
                }
                Some(b'$') if self.peek(1) == Some(b'{') => {
                    self.bump_n(2);
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(bytes_to_string(core::mem::take(&mut lit))));
                    }
                    parts.push(StrPart::Interp(self.lex_interp()?));
                }
                Some(b) => {
                    self.bump();
                    lit.push(b);
                }
            }
        }
        if !lit.is_empty() {
            parts.push(StrPart::Lit(bytes_to_string(lit)));
        }
        Ok(strip_indentation(parts))
    }
}

fn is_path_seg_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-' || c == b'+'
}

fn bytes_to_string(v: Vec<u8>) -> String {
    // Source was valid UTF-8 and escapes only introduce ASCII.
    String::from_utf8(v).expect("string literal bytes are UTF-8")
}

/// Indentation stripping for indented strings (02 §2.6, steps 1-4).
/// Interpolations count as non-space content at their position.
fn strip_indentation(parts: Vec<StrPart>) -> Vec<StrPart> {
    // Split into lines: each line is a sequence of fragments.
    #[derive(Clone)]
    enum Frag {
        Text(String),
        Interp(Vec<Token>),
    }
    let mut lines: Vec<Vec<Frag>> = vec![Vec::new()];
    for p in parts {
        match p {
            StrPart::Interp(t) => lines.last_mut().unwrap().push(Frag::Interp(t)),
            StrPart::Lit(s) => {
                let mut first = true;
                for piece in s.split('\n') {
                    if !first {
                        lines.push(Vec::new());
                    }
                    first = false;
                    if !piece.is_empty() {
                        lines.last_mut().unwrap().push(Frag::Text(piece.to_string()));
                    }
                }
            }
        }
    }

    // Step 2: minimal indentation over participating lines.
    let mut min_indent: Option<usize> = None;
    for line in &lines {
        let mut indent = 0usize;
        let mut participates = false;
        'frags: for f in line {
            match f {
                Frag::Interp(_) => {
                    participates = true;
                    break 'frags;
                }
                Frag::Text(t) => {
                    for ch in t.bytes() {
                        if ch == b' ' {
                            indent += 1;
                        } else {
                            participates = true;
                            break 'frags;
                        }
                    }
                }
            }
        }
        if participates {
            min_indent = Some(min_indent.map_or(indent, |m| m.min(indent)));
        }
    }
    let min_indent = min_indent.unwrap_or(0);

    // Step 3: remove min_indent leading spaces from every line.
    for line in lines.iter_mut() {
        let mut to_strip = min_indent;
        let mut idx = 0;
        while to_strip > 0 && idx < line.len() {
            match &mut line[idx] {
                Frag::Interp(_) => break,
                Frag::Text(t) => {
                    let leading = t.bytes().take_while(|&b| b == b' ').count();
                    let cut = leading.min(to_strip);
                    *t = t[cut..].to_string();
                    to_strip -= cut;
                    if !t.is_empty() {
                        break;
                    }
                    idx += 1;
                }
            }
        }
    }

    // Step 4a: drop the first line if it is empty (opener followed by LF).
    if lines.len() > 1 && lines[0].iter().all(|f| matches!(f, Frag::Text(t) if t.is_empty())) {
        lines.remove(0);
    }
    // Step 4b: drop a trailing spaces-only last line (the closer's indentation).
    if let Some(last) = lines.last() {
        let spaces_only = last
            .iter()
            .all(|f| matches!(f, Frag::Text(t) if t.bytes().all(|b| b == b' ')));
        if spaces_only && !last.is_empty() {
            *lines.last_mut().unwrap() = Vec::new();
        }
    }

    // Re-join with LF between lines, merging adjacent literals.
    let mut out: Vec<StrPart> = Vec::new();
    let mut lit = String::new();
    let n = lines.len();
    for (li, line) in lines.into_iter().enumerate() {
        for f in line {
            match f {
                Frag::Text(t) => lit.push_str(&t),
                Frag::Interp(toks) => {
                    if !lit.is_empty() {
                        out.push(StrPart::Lit(core::mem::take(&mut lit)));
                    }
                    out.push(StrPart::Interp(toks));
                }
            }
        }
        if li + 1 < n {
            lit.push('\n');
        }
    }
    if !lit.is_empty() {
        out.push(StrPart::Lit(lit));
    }
    out
}
