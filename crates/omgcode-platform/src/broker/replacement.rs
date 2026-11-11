//! Native publication types for the qualified platforms.

#[cfg(target_os = "linux")]
pub use super::linux::replacement::{ReplacementStage, ReplacementTarget};
#[cfg(windows)]
pub use super::windows::replacement::{ReplacementStage, ReplacementTarget};
