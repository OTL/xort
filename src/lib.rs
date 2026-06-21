#![warn(missing_docs)]
//! xort — a fast, modern, parallel replacement for the Unix `sort` command.
//!
//! Library surface so the binary stays thin and the engine is unit/integration
//! testable. The module layout mirrors the project plan: text sorting lives in
//! [`engine`], multi-key/typed comparison in [`key`] and [`compare`], structured
//! formats (CSV/JSON) in [`format`](mod@format), the >RAM path in [`external`], and output
//! polish (color, rich `--check`) in [`diag`].

pub mod cli;
pub mod compare;
pub mod compress;
pub mod config;
pub mod diag;
pub mod engine;
pub mod external;
pub mod format;
pub mod input;
pub mod key;

pub use config::Config;
pub use engine::{run, Outcome, Stats};
