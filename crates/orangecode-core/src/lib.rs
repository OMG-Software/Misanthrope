//! Core contracts for the OrangeCode coding-assistant prototype.
#![forbid(unsafe_code)]

pub mod control;
mod file_audit;
pub mod files;
pub mod policy;
pub mod prompt;
pub mod provider;
pub mod session;
mod storage;
pub mod turn;
mod workspace;
