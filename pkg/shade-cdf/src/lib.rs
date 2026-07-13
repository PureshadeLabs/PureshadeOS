//! shade-cdf — the one shared CDF canonicalizer.
//!
//! Canonical Derivation Form byte format per `docs/shade-pkg/02-store.md`
//! §3.2 (format rules) and §3.3 (key set). This crate is the factored
//! canonicalizer `docs/shade/08-interop.md` §1 calls for: shadec's emitter
//! and the store services both link it; the byte format lives nowhere else.
//! Any byte divergence between producers silently shifts store paths and
//! breaks input-addressing — that is why this is one crate.
//!
//! Also here: the store-path digest (BLAKE3-160 base32, 02 §3.1), the store
//! path grammar (02 §2), name/version normalization (03 §2), and the source
//! tree-hash manifest (04 §3.3) — the identity byte formats the evaluator
//! and store layer must agree on.
//!
//! `no_std` + `alloc`: host tools and OROS userspace both link this.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::fmt;

/// CDF format version, emitted as line 1 `shade-drv=1` (02 §3.2 rule 3).
pub const FORMAT_VERSION: u32 = 1;
/// The line-1 header key.
pub const HEADER_KEY: &str = "shade-drv";
/// Store prefix (02 §2).
pub const STORE_PREFIX: &str = "/shade/store/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdfError {
    /// Key does not match the CDF key charset.
    BadKey(String),
    /// Key inserted twice — canonicalization is total, duplicates are a producer bug.
    DuplicateKey(String),
    /// Attempt to insert the reserved header key.
    ReservedKey,
    /// Name fails normalization (03 §2: ASCII-lowercase; anything else is an error, no guessing).
    BadName(String),
    /// Version fails `[0-9a-z.+-]+`, ≤ 32 bytes (02 §2).
    BadVersion(String),
}

impl fmt::Display for CdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CdfError::BadKey(k) => write!(f, "invalid CDF key: {k:?}"),
            CdfError::DuplicateKey(k) => write!(f, "duplicate CDF key: {k:?}"),
            CdfError::ReservedKey => write!(f, "key `shade-drv` is the reserved header"),
            CdfError::BadName(n) => write!(f, "invalid package name: {n:?}"),
            CdfError::BadVersion(v) => write!(f, "invalid version: {v:?}"),
        }
    }
}

/// Percent-escape a CDF value: bytes LF, CR, `%` become `%0A`, `%0D`, `%25`;
/// everything else is literal (02 §3.2 rule 4). Keys are never escaped.
pub fn escape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'\n' => out.push_str("%0A"),
            b'\r' => out.push_str("%0D"),
            b'%' => out.push_str("%25"),
            _ => out.push(b as char),
        }
    }
    out
}

/// Reverse of [`escape_value`]: decode `%0A`, `%0D`, `%25`. Returns `None`
/// on any other `%` sequence — canonical CDF bytes never contain one, so a
/// bad escape means the input is not CDF output (rule 4 is total).
pub fn unescape_value(v: &str) -> Option<String> {
    let mut out = String::with_capacity(v.len());
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            match b.get(i + 1..i + 3)? {
                b"0A" => out.push('\n'),
                b"0D" => out.push('\r'),
                b"25" => out.push('%'),
                _ => return None,
            }
            i += 3;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    Some(out)
}

/// A CDF document read back from its canonical bytes: the key/value entries,
/// header excluded, values unescaped. Parsing is the strict inverse of
/// [`CdfBuilder::build`] — anything a conforming producer cannot emit is an
/// error, never repaired (canonicalization is total; consumers must not
/// widen it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdfParseError {
    /// Not valid UTF-8 (canonical CDF is ASCII-safe by construction).
    NotUtf8,
    /// Line 1 is not `shade-drv=<version>`.
    MissingHeader,
    /// Header names a format version this parser does not read.
    UnsupportedVersion(String),
    /// A line (1-based) has no `=`, an invalid key, or a bad escape.
    BadLine(usize),
    /// The same key appears twice — impossible from a conforming producer.
    DuplicateKey(String),
    /// Missing trailing LF (rule 5: one LF per line, including the last).
    MissingTrailingNewline,
}

impl fmt::Display for CdfParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CdfParseError::NotUtf8 => write!(f, "CDF is not valid UTF-8"),
            CdfParseError::MissingHeader => write!(f, "line 1 is not the `shade-drv` header"),
            CdfParseError::UnsupportedVersion(v) => write!(f, "unsupported CDF version {v:?}"),
            CdfParseError::BadLine(n) => write!(f, "malformed CDF line {n}"),
            CdfParseError::DuplicateKey(k) => write!(f, "duplicate CDF key {k:?}"),
            CdfParseError::MissingTrailingNewline => write!(f, "CDF missing trailing newline"),
        }
    }
}

/// Parse canonical CDF bytes back into entries (header verified and
/// excluded). Consumers of `.drv` files — the build executor, registration,
/// GC reference scans — read through this so the byte format still lives in
/// exactly one crate.
pub fn parse(bytes: &[u8]) -> Result<alloc::collections::BTreeMap<String, String>, CdfParseError> {
    let s = core::str::from_utf8(bytes).map_err(|_| CdfParseError::NotUtf8)?;
    let body = s.strip_suffix('\n').ok_or(CdfParseError::MissingTrailingNewline)?;
    let mut lines = body.split('\n');
    match lines.next().and_then(|l| l.split_once('=')) {
        Some((k, v)) if k == HEADER_KEY => {
            if v != format!("{FORMAT_VERSION}") {
                return Err(CdfParseError::UnsupportedVersion(String::from(v)));
            }
        }
        _ => return Err(CdfParseError::MissingHeader),
    }
    let mut entries = alloc::collections::BTreeMap::new();
    for (i, line) in lines.enumerate() {
        let n = i + 2; // 1-based, after the header line
        let (k, v) = line.split_once('=').ok_or(CdfParseError::BadLine(n))?;
        if k == HEADER_KEY || !is_valid_key(k) {
            return Err(CdfParseError::BadLine(n));
        }
        let v = unescape_value(v).ok_or(CdfParseError::BadLine(n))?;
        if entries.insert(String::from(k), v).is_some() {
            return Err(CdfParseError::DuplicateKey(String::from(k)));
        }
    }
    Ok(entries)
}

/// CDF key charset check: **lowercase-only** `[a-z0-9._-]+` (02 §3.2
/// rule 2, resolved 2026-07-06 — keys are hash inputs; case-fold collisions
/// are unacceptable there). Env-var names are recorded as their lowercase
/// fold (02 §3.3 `env.<key>` row); the fold happens in the producer, this
/// check enforces the result.
pub fn is_valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-')
}

/// Builder for one canonical CDF document.
///
/// Keys are collected unordered; `build()` performs canonicalization:
/// header first, then strict bytewise-ascending key order, `key=value` with
/// percent-escaped values, one LF per line, trailing LF (02 §3.2 rules 1-6).
#[derive(Debug, Default, Clone)]
pub struct CdfBuilder {
    entries: alloc::collections::BTreeMap<String, String>,
}

impl CdfBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: &str, value: &str) -> Result<(), CdfError> {
        if key == HEADER_KEY {
            return Err(CdfError::ReservedKey);
        }
        if !is_valid_key(key) {
            return Err(CdfError::BadKey(String::from(key)));
        }
        if self.entries.contains_key(key) {
            return Err(CdfError::DuplicateKey(String::from(key)));
        }
        self.entries.insert(String::from(key), String::from(value));
        Ok(())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Emit the canonical bytes. Total: same key set ⇒ same bytes.
    pub fn build(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str(HEADER_KEY);
        out.push('=');
        out.push_str(&format!("{FORMAT_VERSION}"));
        out.push('\n');
        // BTreeMap<String, _> iterates in bytewise-ascending key order (rule 2).
        for (k, v) in &self.entries {
            out.push_str(k);
            out.push('=');
            out.push_str(&escape_value(v));
            out.push('\n');
        }
        out.into_bytes()
    }
}

/// Full BLAKE3-256 of arbitrary bytes, lowercase hex (02 §3.1: where a full
/// hash is stored, all 32 bytes are kept, lowercase hex).
pub fn blake3_hex(bytes: &[u8]) -> String {
    let hash = blake3::hash(bytes);
    hex_lower(hash.as_bytes())
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// The pinned store-digest base32 alphabet (02 §2): Nix's alphabet, which
/// drops `e o t u` from `0-9a-z` so digests never form words in paths. This
/// is **not** RFC 4648 and **not** any stdlib base32 — it is an explicit,
/// frozen constant. Changing it moves every store path; a change bumps the
/// store format the same way `shade-drv` does. Exactly 32 symbols.
pub const BASE32_ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// The store digest of a CDF: first 160 bits of BLAKE3-256, base32-encoded
/// with the pinned [`BASE32_ALPHABET`], no padding — exactly 32 characters
/// (02 §2). 20 bytes × 8 / 5 = 32 symbols, no partial group.
pub fn store_digest(cdf_bytes: &[u8]) -> String {
    let hash = blake3::hash(cdf_bytes);
    base32(&hash.as_bytes()[..20])
}

/// Base32 over the pinned [`BASE32_ALPHABET`], MSB-first, no padding. Input
/// length must be a multiple of 5 bytes (store digests are 20 bytes →
/// exactly 32 chars, no partial group). Each 5-byte group emits 8 symbols.
pub fn base32(bytes: &[u8]) -> String {
    debug_assert!(bytes.len() % 5 == 0);
    let mut out = String::with_capacity(bytes.len() * 8 / 5);
    for chunk in bytes.chunks(5) {
        let mut acc: u64 = 0;
        for &b in chunk {
            acc = (acc << 8) | b as u64;
        }
        for i in (0..8).rev() {
            out.push(BASE32_ALPHABET[((acc >> (i * 5)) & 0x1f) as usize] as char);
        }
    }
    out
}

/// Normalize a package name (03 §2): ASCII-lowercase; any character outside
/// `[a-zA-Z0-9_-]` is an error (no lossy mapping). Result must match
/// `[a-z0-9][a-z0-9_-]*`, ≤ 64 bytes.
pub fn normalize_name(name: &str) -> Result<String, CdfError> {
    let bad = || CdfError::BadName(String::from(name));
    if name.is_empty() || name.len() > 64 {
        return Err(bad());
    }
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => out.push(b as char),
            b'A'..=b'Z' => out.push(b.to_ascii_lowercase() as char),
            _ => return Err(bad()),
        }
    }
    let first = out.as_bytes()[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(bad());
    }
    Ok(out)
}

/// Validate a version string (02 §2): `[0-9a-z.+-]+`, ≤ 32 bytes.
pub fn validate_version(v: &str) -> Result<(), CdfError> {
    let ok = !v.is_empty()
        && v.len() <= 32
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'+' || b == b'-');
    if ok { Ok(()) } else { Err(CdfError::BadVersion(String::from(v))) }
}

/// Store paths for a CDF (02 §2): the `.drv` and the output directory share
/// one digest. `name` must already be normalized.
pub struct StorePaths {
    pub digest: String,
    pub drv_path: String,
    pub out_path: String,
}

pub fn store_paths(name: &str, version: &str, cdf_bytes: &[u8]) -> Result<StorePaths, CdfError> {
    // Callers normalize; re-check so a bad name can never reach a path.
    let norm = normalize_name(name)?;
    if norm != name {
        return Err(CdfError::BadName(String::from(name)));
    }
    validate_version(version)?;
    let digest = store_digest(cdf_bytes);
    let out_path = format!("{STORE_PREFIX}{digest}-{name}-{version}");
    let drv_path = format!("{out_path}.drv");
    Ok(StorePaths { digest, drv_path, out_path })
}

/// Source tree-hash manifest (docs/shade-pkg/04-sources.md §3.3, normative):
/// one line per entry `<type> <path> <hash>\n`, `type` ∈ f/x/l/d, path
/// /-separated relative, percent-escaped per CDF rule 4; hash is
/// lowercase-hex BLAKE3-256 of content / symlink target, empty for `d`.
/// Lines sorted bytewise, concatenated, BLAKE3-256'd.
pub mod treehash {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EntryKind {
        File,
        ExecFile,
        Symlink,
        Dir,
    }

    impl EntryKind {
        fn tag(self) -> char {
            match self {
                EntryKind::File => 'f',
                EntryKind::ExecFile => 'x',
                EntryKind::Symlink => 'l',
                EntryKind::Dir => 'd',
            }
        }
    }

    /// One canonical manifest line (with trailing LF).
    pub fn manifest_line(kind: EntryKind, rel_path: &str, content_hash_hex: &str) -> String {
        format!("{} {} {}\n", kind.tag(), escape_value(rel_path), content_hash_hex)
    }

    /// Tree hash over manifest lines: sort bytewise, concatenate, BLAKE3-256 hex.
    pub fn tree_hash(mut lines: Vec<String>) -> String {
        lines.sort_unstable();
        let mut buf = Vec::new();
        for l in &lines {
            buf.extend_from_slice(l.as_bytes());
        }
        blake3_hex(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn escape_rule4() {
        assert_eq!(escape_value("a\nb\rc%d"), "a%0Ab%0Dc%25d");
        assert_eq!(escape_value("plain -m755 $out/bin"), "plain -m755 $out/bin");
    }

    #[test]
    fn parse_roundtrips_build() {
        let mut b = CdfBuilder::new();
        b.insert("name", "x").unwrap();
        b.insert("phase.0", "printf 'a\nb' > $out/f && echo 100%").unwrap();
        b.insert("dep.0", "/shade/store/aa-x-1").unwrap();
        let bytes = b.build();
        let m = parse(&bytes).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m["name"], "x");
        assert_eq!(m["phase.0"], "printf 'a\nb' > $out/f && echo 100%");
        assert_eq!(m["dep.0"], "/shade/store/aa-x-1");
        // Re-emitting the parsed entries reproduces the bytes: parse is the
        // strict inverse of build.
        let mut b2 = CdfBuilder::new();
        for (k, v) in &m {
            b2.insert(k, v).unwrap();
        }
        assert_eq!(b2.build(), bytes);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert_eq!(parse(b"nope=1\n"), Err(CdfParseError::MissingHeader));
        assert_eq!(
            parse(b"shade-drv=2\n"),
            Err(CdfParseError::UnsupportedVersion("2".to_string()))
        );
        assert_eq!(parse(b"shade-drv=1\nname=x"), Err(CdfParseError::MissingTrailingNewline));
        assert_eq!(parse(b"shade-drv=1\nnoequals\n"), Err(CdfParseError::BadLine(2)));
        assert_eq!(parse(b"shade-drv=1\nBAD=x\n"), Err(CdfParseError::BadLine(2)));
        assert_eq!(parse(b"shade-drv=1\nk=bad%ZZescape\n"), Err(CdfParseError::BadLine(2)));
        assert_eq!(
            parse(b"shade-drv=1\nk=1\nk=2\n"),
            Err(CdfParseError::DuplicateKey("k".to_string()))
        );
        assert_eq!(parse(&[0xff, 0xfe]), Err(CdfParseError::NotUtf8));
    }

    #[test]
    fn unescape_inverts_escape() {
        for v in ["a\nb\rc%d", "plain", "%", "\n\n", "100%0A"] {
            assert_eq!(unescape_value(&escape_value(v)).as_deref(), Some(v));
        }
        assert_eq!(unescape_value("bad%zz"), None);
        assert_eq!(unescape_value("trunc%0"), None);
    }

    #[test]
    fn header_and_sort() {
        let mut b = CdfBuilder::new();
        b.insert("name", "x").unwrap();
        b.insert("dep.0", "/shade/store/aa-x-1").unwrap();
        b.insert("env.rustflags", "-C opt-level=3").unwrap();
        let bytes = b.build();
        let s = core::str::from_utf8(&bytes).unwrap();
        assert_eq!(s, "shade-drv=1\ndep.0=/shade/store/aa-x-1\nenv.rustflags=-C opt-level=3\nname=x\n");
    }

    #[test]
    fn rejects_dup_reserved_bad() {
        let mut b = CdfBuilder::new();
        b.insert("name", "x").unwrap();
        assert_eq!(b.insert("name", "y"), Err(CdfError::DuplicateKey("name".to_string())));
        assert_eq!(b.insert("shade-drv", "2"), Err(CdfError::ReservedKey));
        assert!(matches!(b.insert("bad key", "v"), Err(CdfError::BadKey(_))));
        // lowercase-only (02 §3.2 rule 2): uppercase keys are invalid;
        // producers fold env-var names before insertion
        assert!(matches!(b.insert("env.RUSTFLAGS", "v"), Err(CdfError::BadKey(_))));
    }

    #[test]
    fn base32_shape() {
        // Pinned alphabet: symbol 0 is '0', so 20 zero bytes -> 32 '0's.
        assert_eq!(base32(&[0u8; 20]), "00000000000000000000000000000000");
        // All-ones 5-byte group -> symbol 31 ('z') eight times.
        assert_eq!(base32(&[0xffu8; 5]), "zzzzzzzz");
        assert_eq!(store_digest(b"anything").len(), 32);
        // Every symbol is in the pinned alphabet, and none of e/o/t/u appear.
        assert!(store_digest(b"anything")
            .bytes()
            .all(|c| BASE32_ALPHABET.contains(&c)));
        assert!(!BASE32_ALPHABET.iter().any(|&c| matches!(c, b'e' | b'o' | b't' | b'u')));
        assert_eq!(BASE32_ALPHABET.len(), 32);
    }

    /// Base32 encoding vectors against the pinned alphabet
    /// `0123456789abcdfghijklmnpqrsvwxyz` (02 §2, MSB-first). Hand-computed:
    /// each 5-bit group indexes the alphabet. Freezes the mapping so an
    /// accidental alphabet or bit-order change is caught, not silently shipped.
    #[test]
    fn base32_pinned_vectors() {
        // Multi-group vector: bytes 00 01 02 03 04, MSB-first 5-bit groups
        // index the alphabet as 0,0,0,16,4,0,24,4 -> '0','0','0','h','4','0','q','4'.
        assert_eq!(base32(&[0x00, 0x01, 0x02, 0x03, 0x04]), "000h40q4");
        // Single-symbol sweep: last group == N, so char [7] == ALPHABET[N].
        // These pin the word-avoiding skips: after '9' comes 'a'; 'd'->'f'
        // skips 'e'; index 22 is 'n' then 'p' skips 'o'; 's' then 'v' skips 't','u'.
        for (n, c) in [(9u8, b'9'), (10, b'a'), (13, b'd'), (14, b'f'),
                       (22, b'n'), (23, b'p'), (26, b's'), (27, b'v')] {
            assert_eq!(base32(&[0, 0, 0, 0, n]).as_bytes()[7], c);
            assert_eq!(BASE32_ALPHABET[n as usize], c);
        }
    }

    #[test]
    fn name_version_rules() {
        assert_eq!(normalize_name("RKilo").unwrap(), "rkilo");
        assert!(normalize_name("has space").is_err());
        assert!(normalize_name("_leading").is_err());
        assert!(validate_version("1.2.0").is_ok());
        assert!(validate_version("1.2.0+Beta").is_err()); // uppercase
    }

    /// The acceptance golden: the rkilo CDF from docs/shade-pkg/02-store.md
    /// §3.3, fully sorted per rule 2 (the doc example shows key kinds and
    /// says the sort rule is normative). Byte-identical or the store paths
    /// move. Env keys are the lowercase fold of the variable name (02 §3.3).
    #[test]
    fn golden_rkilo_cdf() {
        let mut b = CdfBuilder::new();
        b.insert("dep.0", "/shade/store/c4fq3m2z7xj5kx2apwrn6uu3drhtbz3i-lythos-libstd-0.3.0").unwrap();
        b.insert("env.rustflags", "-C opt-level=3").unwrap();
        b.insert("name", "rkilo").unwrap();
        b.insert("output.0", "bin/rkilo").unwrap();
        b.insert("phase.0", "cargo build --release --offline --target x86_64-oros").unwrap();
        b.insert("phase.1", "install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo").unwrap();
        b.insert("sandbox", "1").unwrap();
        b.insert("source.0.crate", "rkilo").unwrap();
        b.insert("source.0.sha256", "9f1c2ab34c1d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f").unwrap();
        b.insert("source.0.type", "crates-io").unwrap();
        b.insert("source.0.version", "1.2.0").unwrap();
        b.insert("system", "x86_64-oros").unwrap();
        b.insert("toolchain", "rustc-1.86.0-adf2135f0").unwrap();
        b.insert("version", "1.2.0").unwrap();
        let bytes = b.build();
        let expected = "shade-drv=1\n\
dep.0=/shade/store/c4fq3m2z7xj5kx2apwrn6uu3drhtbz3i-lythos-libstd-0.3.0\n\
env.rustflags=-C opt-level=3\n\
name=rkilo\n\
output.0=bin/rkilo\n\
phase.0=cargo build --release --offline --target x86_64-oros\n\
phase.1=install -m755 target/x86_64-oros/release/rkilo $out/bin/rkilo\n\
sandbox=1\n\
source.0.crate=rkilo\n\
source.0.sha256=9f1c2ab34c1d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f\n\
source.0.type=crates-io\n\
source.0.version=1.2.0\n\
system=x86_64-oros\n\
toolchain=rustc-1.86.0-adf2135f0\n\
version=1.2.0\n";
        assert_eq!(core::str::from_utf8(&bytes).unwrap(), expected);

        let sp = store_paths("rkilo", "1.2.0", &bytes).unwrap();
        assert_eq!(sp.digest.len(), 32);
        assert!(sp.drv_path.starts_with("/shade/store/"));
        assert!(sp.drv_path.ends_with("-rkilo-1.2.0.drv"));
        assert_eq!(sp.out_path, sp.drv_path.trim_end_matches(".drv"));
    }

    #[test]
    fn treehash_stable() {
        use treehash::*;
        let l1 = manifest_line(EntryKind::File, "src/main.rs", &blake3_hex(b"fn main(){}"));
        let l2 = manifest_line(EntryKind::Dir, "src", "");
        let l3 = manifest_line(EntryKind::ExecFile, "run%weird\nname", &blake3_hex(b"#!/bin/sh"));
        assert!(l3.contains("run%25weird%0Aname"));
        let h1 = tree_hash(alloc::vec![l1.clone(), l2.clone(), l3.clone()]);
        let h2 = tree_hash(alloc::vec![l3, l1, l2]); // order-independent
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }
}
