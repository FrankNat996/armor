//! armor's JIT (milestone M6, ADR-0021, docs/tech/jit.md).
//!
//! - `encoder`: the AArch64 encoder, checked against the GNU assembler by
//!   `cargo xtask encoder-check`;
//! - `buffer`: the executable code buffer;
//! - `translator`: x86-64 basic blocks to AArch64 code (safe code);
//! - `engine`: the run loop, which calls translated blocks and falls back
//!   to the interpreter;
//! - `lockstep`: the debugging mode that checks every translated
//!   instruction against the interpreter;
//! - `stats`: translation counters (`ARMOR_JIT_STATS`);
//! - `cache`: translations kept on disk for later runs (ADR-0048);
//! - `check`: the encoder's samples for `cargo xtask encoder-check`.
//!
//! All of armor's code-generation `unsafe` lives in `buffer` and `engine`.
//!
//! Without the `std` feature only `encoder` and `translator` are built,
//! with `core` and `alloc`: the Windows backend (ADR-0047) has its own
//! engine around them.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "std")]
pub mod buffer;
#[cfg(feature = "std")]
pub mod cache;
#[cfg(feature = "std")]
#[doc(hidden)]
pub mod check;
pub mod encoder;
#[cfg(feature = "std")]
pub mod engine;
#[cfg(feature = "std")]
mod lockstep;
#[cfg(feature = "std")]
pub mod stats;
pub mod translator;

#[cfg(test)]
mod tests;

#[cfg(feature = "std")]
pub use engine::Jit;
