//! shadec — the Shade evaluator (docs/shade/01..08).
//!
//! Frontend only: evaluating a `.shade` expression produces derivation
//! values whose CDF bytes come from the shared `shade-cdf` canonicalizer;
//! everything below the CDF boundary is shade's store layer, consumed
//! unchanged (docs/shade/08-interop.md §1).
//!
//! Core is `no_std`+alloc so the OROS `shade` binary can link it; the host
//! `shadec` binary (feature `cli`) is the seed vehicle
//! (docs/shade-pkg/09-bootstrap.md §2).

#![cfg_attr(not(any(test, feature = "std")), no_std)]

extern crate alloc;

pub mod ast;
pub mod builtins;
pub mod drv;
pub mod error;
pub mod eval;
pub mod io;
pub mod lexer;
pub mod parser;
pub mod print;
pub mod value;
