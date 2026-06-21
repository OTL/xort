//! xort — a fast, modern, parallel replacement for the Unix `sort` command.
//!
//! Library surface so the binary stays thin and the engine is unit/integration
//! testable. The module layout mirrors the project plan; milestone 1 implements
//! a correct, parallel plain sort with the global key and the core flags.

pub mod cli;
pub mod compare;
pub mod config;
pub mod diag;
pub mod engine;
pub mod external;
pub mod format;
pub mod input;
pub mod key;

pub use config::Config;
pub use engine::{run, Outcome, Stats};
