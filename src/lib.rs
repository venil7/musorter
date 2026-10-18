//! musorter — a music collection sorter.
//!
//! Library crate so the CLI binary (`main.rs`) and the integration tests
//! share the exact same implementation.

pub mod cli;
pub mod error;
pub mod exec;
pub mod hash;
pub mod meta;
pub mod model;
pub mod plan;
pub mod prune;
pub mod report;
pub mod sanitize;
pub mod scan;
pub mod template;
