//! Windows process-tree supervision with a Job Object.
//!
//! The process is created suspended and placed in a new job at creation through
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so it cannot start a descendant outside the
//! job. The job disallows breakaway and kills every member when its last handle
//! closes. Only the three standard handles are inherited, through
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. No AppContainer or token restriction is
//! applied yet; this is not filesystem or network confinement.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::{
    AppliedConfinement, CancelSignal, Capture, ProcessError, ProcessLimits, ProcessOutcome,
    ProcessSpec, ProcessTree, Termination, is_batch_script, stop_tree, supervise,
};

/// Longest command line accepted by `CreateProcessW`, including the final NUL.
const MAX_COMMAND_LINE_UNITS: usize = 32_767;

/// Serializes the short window in which this module's pipe handles are
/// inheritable. It cannot serialize `std::process` spawns in other threads,
/// which may inherit those handles; capture then reports cleanup unconfirmed.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn validate_spec(spec: &ProcessSpec) -> Result<(), ProcessError> {
    let program = &spec.program;
    crate::reject_windows_device_path(program)
        .map_err(|_| ProcessError::InvalidSpec("program must not be a device path"))?;
    if program
        .as_os_str()
        .encode_wide()
        .any(|unit| unit == u16::from(b'"'))
    {
        return Err(ProcessError::InvalidSpec(
            "program path must not contain a quote",
        ));
    }
    let Some(Component::Normal(name)) = program.components().next_back() else {
        return Err(ProcessError::InvalidSpec("program must name a file"));
    };
    let name = name.to_string_lossy();
    // Win32 path normalization drops trailing dots and spaces, and ':' selects
    // a stream; either could turn an allowed name into a different file.
    if name.ends_with('.') || name.ends_with(' ') || name.contains(':') {
        return Err(ProcessError::InvalidSpec(
            "program name must not end in '.' or ' ' or name a stream",
        ));
    }
    if is_batch_script(program) {
        return Err(ProcessError::InvalidSpec(
            "batch scripts are refused because cmd.exe re-parses their arguments",
        ));
    }
    let is_exe = Path::new(name.as_ref())
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return Err(ProcessError::InvalidSpec("program must be an .exe file"));
    }
    command_line(spec).map(|_| ())
}

pub(super) fn run(
    spec: &ProcessSpec,
    limits: ProcessLimits,
    cancel: &CancelSignal,
) -> Result<ProcessOutcome, ProcessError> {
    let application = wide_nul(spec.program.as_os_str());
    let mut command_line = command_line(spec)?;
    let environment = environment_block(spec);
    let directory = wide_nul(spec.working_directory.as_os_str());
    let job = create_job().map_err(ProcessError::Spawn)?;
    let null_input = OpenOptions::new()
        .read(true)
        .open("NUL")
        .map_err(ProcessError::Spawn)?;
    let (stdout_read, stdout_write) = pipe().map_err(ProcessError::Spawn)?;
    let (stderr_read, stderr_write) = pipe().map_err(ProcessError::Spawn)?;

    let created = {
        // The lock guards no data, so a poisoned lock carries no broken invariant.
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let inherited = [
            null_input.as_raw_handle() as HANDLE,
            stdout_write.as_raw_handle() as HANDLE,
            stderr_write.as_raw_handle() as HANDLE,
        ];
        for handle in inherited {
            set_inheritable(handle, true).map_err(ProcessError::Spawn)?;
        }
        let result = AttributeList::new(inherited, job.as_raw_handle() as HANDLE).and_then(
            |mut attributes| {
                create_suspended(
                    &application,
                    &mut command_line,
                    &environment,
                    &directory,
                    inherited,
                    &mut attributes,
                )
            },
        );
        // The child has its own copies; ours must close so the pipes can end.
        drop(null_input);
        drop(stdout_write);
        drop(stderr_write);
        result.map_err(ProcessError::Spawn)?
    };

    let mut tree = WindowsTree {
        job,
        process: created.process,
    };
    if let Err(error) = verify_member(&tree).and_then(|()| resume(&created.thread)) {
        // Nothing has run yet; closing the job kills the suspended process.
        terminate_process(&tree.process);
        return Err(ProcessError::Spawn(error));
    }
    let started = Instant::now();
    let capture = match Capture::start(File::from(stdout_read), File::from(stderr_read), limits) {
        Ok(capture) => capture,
        Err(error) => {
            stop_tree(&mut tree, limits.cleanup_timeout)?;
            return Err(ProcessError::Supervision(error));
        }
    };
    supervise(
        &mut tree,
        capture,
        started,
        limits,
        cancel,
        AppliedConfinement::Unconfined,
    )
}

struct WindowsTree {
    job: OwnedHandle,
    process: OwnedHandle,
}

impl ProcessTree for WindowsTree {
    #[allow(unsafe_code)]
    fn try_exit(&mut self) -> io::Result<Option<Termination>> {
        // SAFETY: the process handle is owned by `self` and live for the call.
        let wait = unsafe { WaitForSingleObject(self.process.as_raw_handle() as HANDLE, 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0u32;
        // SAFETY: the live process handle is borrowed, and `code` is writable.
        let result =
            unsafe { GetExitCodeProcess(self.process.as_raw_handle() as HANDLE, &raw mut code) };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Termination::Exited {
            code: i64::from(code),
        }))
    }

    #[allow(unsafe_code)]
    fn terminate(&mut self) -> io::Result<()> {
        // SAFETY: the job handle is owned by `self` and live for the call.
        let result = unsafe { TerminateJobObject(self.job.as_raw_handle() as HANDLE, 1) };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    fn is_empty(&mut self) -> io::Result<bool> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let length = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
            .map_err(|_| io::Error::other("job accounting structure is too large"))?;
        // SAFETY: the job handle is live; `info` is writable storage of exactly
        // `length` bytes for the matching information class.
        let result = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle() as HANDLE,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                length,
                null_mut(),
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses == 0)
    }
}

struct Created {
    process: OwnedHandle,
    thread: OwnedHandle,
}

#[allow(unsafe_code)]
fn create_job() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and a null name create an unnamed job with the
    // caller's default security; the call has no other pointer inputs.
    let handle = unsafe { CreateJobObjectW(null(), null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle was just returned by `CreateJobObjectW` and is owned here.
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // No breakaway flag is set, so members cannot leave the job.
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    let length = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("job limit structure is too large"))?;
    // SAFETY: the job handle is live and `limits` is a readable structure of
    // exactly `length` bytes for the matching information class.
    let result = unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            length,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Create a pipe whose handles are not inheritable.
#[allow(unsafe_code)]
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read: HANDLE = null_mut();
    let mut write: HANDLE = null_mut();
    // SAFETY: both outputs are writable; null attributes make the handles
    // non-inheritable; zero selects the default buffer size.
    let result = unsafe { CreatePipe(&raw mut read, &raw mut write, null(), 0) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success both handles are new and owned by this function.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

#[allow(unsafe_code)]
fn set_inheritable(handle: HANDLE, inheritable: bool) -> io::Result<()> {
    let flags = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
    // SAFETY: the caller passes a live handle that it owns for the call.
    let result = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A process/thread attribute list that names the inherited handles and the job.
///
/// The handle and job arrays are boxed because the list keeps pointers to them
/// until `DeleteProcThreadAttributeList`, which runs in `Drop`.
struct AttributeList {
    buffer: Vec<u64>,
    handles: Box<[HANDLE; 3]>,
    jobs: Box<[HANDLE; 1]>,
}

impl AttributeList {
    #[allow(unsafe_code)]
    fn new(handles: [HANDLE; 3], job: HANDLE) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a null list with a writable size queries the required size.
        // The expected result is failure with ERROR_INSUFFICIENT_BUFFER.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &raw mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = size.div_ceil(size_of::<u64>());
        let mut list = Self {
            buffer: vec![0u64; words],
            handles: Box::new(handles),
            jobs: Box::new([job]),
        };
        // SAFETY: the buffer is writable, 8-byte aligned, and at least `size` bytes.
        let result =
            unsafe { InitializeProcThreadAttributeList(list.pointer(), 2, 0, &raw mut size) };
        if result == 0 {
            // Not initialized, so `Drop` must not delete it.
            list.buffer = Vec::new();
            return Err(io::Error::last_os_error());
        }
        let handle_bytes = size_of::<[HANDLE; 3]>();
        // SAFETY: the list is initialized; the boxed handle array outlives it
        // and is exactly `handle_bytes` long.
        let result = unsafe {
            UpdateProcThreadAttribute(
                list.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                list.handles.as_ptr().cast(),
                handle_bytes,
                null_mut(),
                null(),
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the list is initialized; the boxed job array outlives it and
        // is exactly one handle long.
        let result = unsafe {
            UpdateProcThreadAttribute(
                list.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                list.jobs.as_ptr().cast(),
                size_of::<HANDLE>(),
                null_mut(),
                null(),
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        // SAFETY: a non-empty buffer was initialized by
        // `InitializeProcThreadAttributeList` and is deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

#[allow(unsafe_code)]
fn create_suspended(
    application: &[u16],
    command_line: &mut [u16],
    environment: &[u16],
    directory: &[u16],
    inherited: [HANDLE; 3],
    attributes: &mut AttributeList,
) -> io::Result<Created> {
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
        .map_err(|_| io::Error::other("startup structure is too large"))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    let [input, output, error] = inherited;
    startup.StartupInfo.hStdInput = input;
    startup.StartupInfo.hStdOutput = output;
    startup.StartupInfo.hStdError = error;
    startup.lpAttributeList = attributes.pointer();
    let mut info = PROCESS_INFORMATION::default();
    let flags = CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | CREATE_NO_WINDOW
        | EXTENDED_STARTUPINFO_PRESENT;
    // SAFETY: every string is NUL-terminated UTF-16 that outlives the call;
    // the command line is mutable as required; the environment block ends in
    // two NULs; the startup structure carries an initialized attribute list
    // whose handle list restricts inheritance to the three standard handles.
    let result = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            flags,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            (&raw const startup).cast(),
            &raw mut info,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success both handles are new and owned by the caller.
    Ok(unsafe {
        Created {
            process: OwnedHandle::from_raw_handle(info.hProcess),
            thread: OwnedHandle::from_raw_handle(info.hThread),
        }
    })
}

#[allow(unsafe_code)]
fn verify_member(tree: &WindowsTree) -> io::Result<()> {
    let mut member = 0;
    // SAFETY: both handles are live and owned by `tree`; `member` is writable.
    let result = unsafe {
        IsProcessInJob(
            tree.process.as_raw_handle() as HANDLE,
            tree.job.as_raw_handle() as HANDLE,
            &raw mut member,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    if member == 0 {
        return Err(io::Error::other("the process was not placed in its job"));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn resume(thread: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the thread handle is live and owned by the caller.
    let previous = unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) };
    if previous == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code)]
fn terminate_process(process: &OwnedHandle) {
    // SAFETY: the process handle is live and owned by the caller.
    let _unreportable = unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 1) };
    // A failure leaves the suspended process to `KILL_ON_JOB_CLOSE` when the
    // job handle drops, so the result needs no separate handling.
}

fn wide_nul(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// Build a quoted command line that the Microsoft C runtime splits back into
/// the exact program path and arguments.
pub(super) fn command_line(spec: &ProcessSpec) -> Result<Vec<u16>, ProcessError> {
    let mut line: Vec<u16> = Vec::new();
    // The program path holds no quote (validated), so plain quoting is exact.
    line.push(u16::from(b'"'));
    line.extend(spec.program.as_os_str().encode_wide());
    line.push(u16::from(b'"'));
    for argument in &spec.arguments {
        line.push(u16::from(b' '));
        append_argument(&mut line, argument);
    }
    if line.len() >= MAX_COMMAND_LINE_UNITS {
        return Err(ProcessError::InvalidSpec("command line is too long"));
    }
    line.push(0);
    Ok(line)
}

fn append_argument(line: &mut Vec<u16>, argument: &OsStr) {
    const BACKSLASH: u16 = b'\\' as u16;
    const QUOTE: u16 = b'"' as u16;
    let units: Vec<u16> = argument.encode_wide().collect();
    let plain = !units.is_empty()
        && !units.iter().any(|unit| {
            [b' ', b'\t', b'\n', 0x0b, b'"'].contains(&u8::try_from(*unit).unwrap_or(0xff))
        });
    if plain {
        line.extend(units);
        return;
    }
    line.push(QUOTE);
    let mut backslashes = 0usize;
    for unit in units {
        // Counts are bounded by the validated argument size; saturation only
        // documents that no counter can wrap. The final length check refuses
        // an oversized line.
        if unit == BACKSLASH {
            backslashes = backslashes.saturating_add(1);
            continue;
        }
        // Backslashes are literal unless they precede a quote.
        let count = if unit == QUOTE {
            backslashes.saturating_mul(2).saturating_add(1)
        } else {
            backslashes
        };
        line.extend(std::iter::repeat_n(BACKSLASH, count));
        backslashes = 0;
        line.push(unit);
    }
    // Trailing backslashes precede the closing quote, so they are doubled.
    line.extend(std::iter::repeat_n(
        BACKSLASH,
        backslashes.saturating_mul(2),
    ));
    line.push(QUOTE);
}

/// Build a sorted Unicode environment block ending in two NULs.
fn environment_block(spec: &ProcessSpec) -> Vec<u16> {
    let mut entries: Vec<_> = spec.environment.iter().collect();
    entries.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}
