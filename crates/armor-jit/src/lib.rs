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
//! - `check`: the encoder's samples for `cargo xtask encoder-check`.
//!
//! All of armor's code-generation `unsafe` lives in `buffer` and `engine`.

pub mod buffer;
#[doc(hidden)]
pub mod check;
pub mod encoder;
pub mod engine;
mod lockstep;
pub mod stats;
pub mod translator;

#[cfg(test)]
mod tests;

pub use engine::Jit;
