//! Keeps file locks held by a test process out of concurrently spawned children.
//!
//! On Unix an advisory `flock` belongs to the open file description. A child
//! cloned while the test process has a store open shares that description until
//! `exec`, so the lock can outlive the test's `close` and a later open reports
//! `Busy`. This is a race between parallel test threads, not store contention.
//! Windows does not inherit these handles; the gate is harmless there.
//!
//! The gate makes the two activities mutually exclusive. Spawning takes the
//! shared side. Opening a lock-holding store takes the exclusive side, and the
//! store must close before the section ends. A child therefore can never be
//! cloned while such a store is open, so it never receives the description.
//! Retrying on `Busy` would hide real contention, so tests must not do that.

use std::io;
use std::process::{Child, Command};
use std::sync::{PoisonError, RwLock};

/// Protects no data, so a panicking test leaves no invariant to distrust.
static SPAWN_GATE: RwLock<()> = RwLock::new(());

/// Starts a child once no in-process store section is active.
///
/// The gate covers only the clone. Callers wait for the child outside it, so a
/// long-running child does not block other tests.
pub(crate) fn spawn(command: &mut Command) -> io::Result<Child> {
    let _gate = SPAWN_GATE.read().unwrap_or_else(PoisonError::into_inner);
    command.spawn()
}

/// Runs `action` while no test thread can start a child.
///
/// `action` must open and close every lock-holding store it uses and must not
/// return one. It must not start a child itself: that would wait on this gate
/// from the thread holding it and never finish.
#[allow(
    dead_code,
    reason = "test binaries that hold no in-process store only use spawn"
)]
pub(crate) fn without_child_spawns<T>(action: impl FnOnce() -> T) -> T {
    let _gate = SPAWN_GATE.write().unwrap_or_else(PoisonError::into_inner);
    action()
}
