//! Core contracts for the OMGCode coding-assistant prototype.
#![forbid(unsafe_code)]

pub mod control;
pub mod edit;
mod file_audit;
pub mod files;
pub mod packages;
pub mod policy;
pub mod prompt;
pub mod provider;
pub mod replacement;
pub mod run;
pub mod run_journal;
pub mod session;
mod storage;
pub mod turn;
mod workspace;
