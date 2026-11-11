//! Turn an operator interrupt (Ctrl-C) into a run cancellation.
//!
//! A supervised tree runs in its own process group on Linux and in a job on
//! Windows, so a terminal interrupt does not reach it directly. Instead, the
//! handler installed here records the interrupt, and a watcher thread raises
//! the run's [`CancelSignal`]. The runner then terminates the whole tree and
//! reports `Cancelled`, so the caller can still record the outcome.
//!
//! Linux handles `SIGINT` and `SIGTERM`; Windows handles `CTRL_C_EVENT` and
//! `CTRL_BREAK_EVENT`. The handler stays installed for the life of the
//! process, so later interrupts are absorbed the same way. A forced kill,
//! such as `SIGKILL` or closing the console, still ends the caller without an
//! outcome record.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::process::CancelSignal;

/// Set by the native handler. Only an atomic store happens in the handler.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// How often the watcher checks for a recorded interrupt.
const WATCH_INTERVAL: Duration = Duration::from_millis(20);

/// Install the interrupt handler and raise `cancel` when an interrupt arrives.
///
/// The watcher thread holds a clone of `cancel` and ends after it raises it.
/// If the handler cannot be installed, nothing is started and the error is
/// returned; the caller decides whether to run without interrupt handling.
pub fn cancel_on_interrupt(cancel: &CancelSignal) -> io::Result<()> {
    install()?;
    let cancel = cancel.clone();
    thread::Builder::new()
        .name("omgcode-interrupt".to_owned())
        .spawn(move || {
            while !INTERRUPTED.load(Ordering::SeqCst) {
                thread::sleep(WATCH_INTERVAL);
            }
            cancel.cancel();
        })?;
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn install() -> io::Result<()> {
    extern "C" fn on_signal(_signal: libc::c_int) {
        // Only an async-signal-safe atomic store happens here.
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: a zeroed `sigaction` is a valid starting value; the mask is
        // initialised with `sigemptyset` before use.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: `action.sa_mask` is a valid, writable signal set.
        if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `action` is fully initialised, the handler only performs an
        // atomic store, and a null old-action pointer is allowed.
        if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn install() -> io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: `on_console_event` has the handler signature and stays valid
    // for the life of the process.
    if unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Absorb Ctrl-C and Ctrl-Break; let other events, such as closing the
/// console, take their default action.
#[cfg(windows)]
extern "system" fn on_console_event(event: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
        INTERRUPTED.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

#[cfg(not(any(unix, windows)))]
fn install() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "interrupt handling is implemented only on Unix and Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// An interrupt delivered to this process raises the run's cancel signal.
    /// This is the only test that touches the process-wide handler.
    #[test]
    fn an_interrupt_raises_the_cancel_signal() {
        let cancel = CancelSignal::new();
        cancel_on_interrupt(&cancel).expect("install the handler");
        assert!(!cancel.is_cancelled());
        deliver_interrupt();
        let until = Instant::now() + Duration::from_secs(5);
        while !cancel.is_cancelled() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(cancel.is_cancelled(), "the watcher did not cancel the run");
    }

    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn deliver_interrupt() {
        // SAFETY: the handler for SIGINT was installed above and only stores
        // an atomic flag.
        assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0);
    }

    /// Generating a real console event would reach every process on the test
    /// console, so call the installed handler directly.
    #[cfg(windows)]
    fn deliver_interrupt() {
        use windows_sys::Win32::System::Console::CTRL_C_EVENT;
        assert_eq!(on_console_event(CTRL_C_EVENT), 1);
    }
}
