//! Codex-style `apply_patch` tool.
//!
//! One patch can add, update, delete, or move several files. Every operation is
//! parsed and checked against an in-memory simulation before anything is
//! written; once committing starts, operations run in order and stop at the
//! first failure or cancellation without rollback.
//!
//! The syntax and matcher follow deepseek-harness `fs-patch`, which adapts the
//! OpenAI Codex parser and `seek_sequence` behavior (Apache-2.0).

mod apply;
mod commit;
mod parse;
mod plan;
mod tool;

pub use tool::ApplyPatch;

#[cfg(test)]
mod tests;
