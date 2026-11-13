//! Windows process-tree supervision with a Job Object.
//!
//! The process is created suspended and placed in a new job at creation through
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so it cannot start a descendant outside the
//! job. The job disallows breakaway and kills every member when its last handle
//! closes. Only the three standard handles are inherited, through
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. An unconfined run applies no token
//! restriction. A confined run also passes
//! `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` with the workspace container
//! SID and no capabilities, so the whole tree runs in that AppContainer.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{
    SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, WinCapabilityInternetClientSid,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectAssociateCompletionPortInformation, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemServices::{
    JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT, JOB_OBJECT_MSG_JOB_MEMORY_LIMIT, SE_GROUP_ENABLED,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

use super::{
    AppliedConfinement, CancelSignal, Capture, Confinement, NetworkAccess, ProcessError,
    ProcessLimits, ProcessOutcome, ProcessSpec, ProcessTree, ResourceLimit, Termination,
    TreeLimits, TreeUsage, UnixSocketAccess, is_batch_script, stop_tree, supervise,
};
use crate::container::ContainerAccess;
use crate::container::windows::{derive_sid, granted_access, profile_exists};
use crate::private_store::windows::Sid;

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
    confinement: Option<&Confinement>,
    limits: ProcessLimits,
    cancel: &CancelSignal,
    mut disk: Option<super::DiskMonitor>,
) -> Result<ProcessOutcome, ProcessError> {
    let container = confinement
        .map(|confinement| prepare_container(spec, confinement))
        .transpose()?;
    let network = confinement.map_or(NetworkAccess::Denied, |confinement| confinement.network);
    let applied = if container.is_some() {
        AppliedConfinement::AppContainer { network }
    } else {
        AppliedConfinement::Unconfined
    };
    let application = wide_nul(spec.program.as_os_str());
    let mut command_line = command_line(spec)?;
    let environment = environment_block(spec);
    let directory = wide_nul(spec.working_directory.as_os_str());
    let (job, limits_state) = create_job(limits.tree).map_err(ProcessError::Spawn)?;
    let null_input = OpenOptions::new()
        .read(true)
        .open("NUL")
        .map_err(ProcessError::Spawn)?;
    let (stdout_read, stdout_write) = pipe().map_err(ProcessError::Spawn)?;
    let (stderr_read, stderr_write) = pipe().map_err(ProcessError::Spawn)?;
    // Start sampling before the program exists, so a sampler that cannot start
    // starts nothing.
    if let Some(disk) = &mut disk {
        disk.start()?;
    }

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
        let result = AttributeList::new(
            inherited,
            job.as_raw_handle() as HANDLE,
            container.as_ref(),
            network,
        )
        .and_then(|mut attributes| {
            create_suspended(
                &application,
                &mut command_line,
                &environment,
                &directory,
                inherited,
                &mut attributes,
            )
        });
        // The child has its own copies; ours must close so the pipes can end.
        drop(null_input);
        drop(stdout_write);
        drop(stderr_write);
        result.map_err(ProcessError::Spawn)?
    };

    let mut tree = WindowsTree {
        job,
        process: created.process,
        limits: limits_state,
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
    supervise(&mut tree, capture, started, limits, cancel, applied, disk)
}

/// Check a confinement and derive its container SID before anything starts.
///
/// Access is decided by the persistent grants at each open; this check makes
/// a missing or weaker grant fail before the program runs instead of partway.
fn prepare_container(spec: &ProcessSpec, confinement: &Confinement) -> Result<Sid, ProcessError> {
    if confinement.unix_sockets == UnixSocketAccess::HostServices {
        return Err(ProcessError::Unsupported(
            "host-service Unix-socket access is not available on Windows",
        ));
    }
    let name = confinement
        .container
        .as_ref()
        .ok_or(ProcessError::InvalidSpec(
            "Windows confinement needs a container name",
        ))?;
    let sid = derive_sid(name).map_err(ProcessError::Confinement)?;
    if !profile_exists(name).map_err(ProcessError::Confinement)? {
        return Err(ProcessError::Confinement(io::Error::new(
            io::ErrorKind::NotFound,
            "the container profile does not exist; grant it a directory first",
        )));
    }
    // Windows derives the container's own folder from this variable and
    // replaces it, TEMP, and TMP; without it the process cannot be created.
    if !spec
        .environment
        .iter()
        .any(|(variable, _)| variable.eq_ignore_ascii_case("LOCALAPPDATA"))
    {
        return Err(ProcessError::InvalidSpec(
            "Windows confinement needs LOCALAPPDATA in the environment",
        ));
    }
    let rules = [
        (&confinement.read_only, ContainerAccess::ReadExecute),
        (&confinement.read_write, ContainerAccess::ReadWrite),
    ];
    for (paths, needed) in rules {
        for path in paths {
            let access = granted_access(path, &sid).map_err(ProcessError::Confinement)?;
            if !covers(access, needed) {
                return Err(ProcessError::Confinement(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "a confinement path is not granted to the container at its listed level",
                )));
            }
        }
    }
    // `CreateProcessW` maps the image with the caller's token, so a program
    // the container cannot read would still start. Refuse it here instead.
    let program = granted_access(&spec.program, &sid).map_err(ProcessError::Confinement)?;
    if !covers(program, ContainerAccess::ReadExecute) {
        return Err(ProcessError::Confinement(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the program is not readable and executable by the container",
        )));
    }
    Ok(sid)
}

const fn covers(granted: Option<ContainerAccess>, needed: ContainerAccess) -> bool {
    matches!(
        (granted, needed),
        (Some(ContainerAccess::ReadWrite), _)
            | (
                Some(ContainerAccess::ReadExecute),
                ContainerAccess::ReadExecute
            )
    )
}

struct WindowsTree {
    job: OwnedHandle,
    process: OwnedHandle,
    limits: Option<JobLimits>,
}

/// Limit state for a job with [`TreeLimits`]. The job itself enforces memory
/// and active processes and posts a message to `port` when either is hit.
struct JobLimits {
    port: OwnedHandle,
    /// CPU-time limit in 100-nanosecond units, as the job accounts it.
    max_cpu_units: u64,
    max_memory_bytes: u64,
    max_processes: u32,
    reached: Option<ResourceLimit>,
}

/// Job messages taken from the port in one poll, so a tree that keeps
/// starting processes cannot hold the supervisor in the drain.
const MAX_DRAINED_MESSAGES: usize = 256;

/// Batches of [`MAX_DRAINED_MESSAGES`] taken once the program has exited, so a
/// limit message queued behind other messages is still seen. Bounded because
/// the program's descendants can post messages until the tree is stopped.
const MAX_EXIT_BATCHES: usize = 64;

/// The limit messages one drain took, and whether it stopped at its bound
/// with messages possibly left in the queue.
#[derive(Debug, Default, PartialEq, Eq)]
struct Drained {
    memory: bool,
    processes: bool,
    full: bool,
}

/// Basic accounting of a job and every job nested in it.
#[allow(unsafe_code)]
fn job_accounting(job: &OwnedHandle) -> io::Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let length = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
        .map_err(|_| io::Error::other("job accounting structure is too large"))?;
    // SAFETY: the job handle is live; `info` is writable storage of exactly
    // `length` bytes for the matching information class.
    let result = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            length,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// User plus kernel time of every process the job has held, in 100-nanosecond
/// units.
fn job_cpu_units(job: &OwnedHandle) -> io::Result<u64> {
    let info = job_accounting(job)?;
    let user = u64::try_from(info.TotalUserTime).unwrap_or(0);
    let kernel = u64::try_from(info.TotalKernelTime).unwrap_or(0);
    Ok(user.saturating_add(kernel))
}

/// The highest memory the whole job has committed at once.
#[allow(unsafe_code)]
fn job_peak_memory(job: &OwnedHandle) -> io::Result<u64> {
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let length = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("job limit structure is too large"))?;
    // SAFETY: the job handle is live; `info` is writable storage of exactly
    // `length` bytes for the matching information class.
    let result = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            (&raw mut info).cast(),
            length,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::try_from(info.PeakJobMemoryUsed).unwrap_or(u64::MAX))
}

/// Whether the job itself shows the limit a message reported. Windows sends a
/// nested job's limit messages to the ports of all its ancestors, so a tool's
/// own stricter job could otherwise end the run.
///
/// A process start fails only at the limit, so the job then holds that many
/// processes. An allocation fails only when it would pass the limit, so a
/// memory message counts once the job's peak commit came within a sixteenth of
/// the limit; a larger allocation refused further below is enforced but not
/// attributed.
///
/// This narrows attribution but does not identify the sending job: a message
/// carries the receiving port's own completion key and, for memory, only a
/// process ID. A nested job's memory message that arrives while the run is
/// already within a sixteenth of its own limit is still counted.
fn job_confirms(job: &OwnedHandle, limits: &JobLimits, limit: ResourceLimit) -> io::Result<bool> {
    Ok(match limit {
        ResourceLimit::Processes => job_accounting(job)?.ActiveProcesses >= limits.max_processes,
        ResourceLimit::Memory => {
            let threshold = limits.max_memory_bytes - limits.max_memory_bytes / 16;
            job_peak_memory(job)? >= threshold
        }
        ResourceLimit::CpuTime => job_cpu_units(job)? >= limits.max_cpu_units,
        // The job has no disk limit; the supervisor samples the writable
        // roots instead. A disk result never comes from a job message.
        ResourceLimit::Disk => false,
    })
}

/// The limit the job has reached, if any: a confirmed limit message, or its
/// CPU time. The first limit found is kept.
///
/// A poll takes one batch of messages. Once the program has exited, it takes
/// up to [`MAX_EXIT_BATCHES`], until the queue is empty or a limit is
/// confirmed, while the tree still holds the processes that confirm it. If
/// messages remain after those batches and CPU time is within its limit, the
/// check is incomplete: it fails rather than let the run end as a normal exit.
fn job_limit_reached(
    job: &OwnedHandle,
    limits: &mut JobLimits,
    exited: bool,
) -> io::Result<Option<ResourceLimit>> {
    if limits.reached.is_some() {
        return Ok(limits.reached);
    }
    let batches = if exited { MAX_EXIT_BATCHES } else { 1 };
    let mut queued = false;
    for _ in 0..batches {
        let drained = drain_limit_messages(&limits.port);
        for (reported, limit) in [
            (drained.memory, ResourceLimit::Memory),
            (drained.processes, ResourceLimit::Processes),
        ] {
            if reported && job_confirms(job, limits, limit)? {
                limits.reached = Some(limit);
                return Ok(limits.reached);
            }
        }
        queued = drained.full;
        if !queued {
            break;
        }
    }
    if job_confirms(job, limits, ResourceLimit::CpuTime)? {
        limits.reached = Some(ResourceLimit::CpuTime);
    } else if exited && queued {
        return Err(io::Error::other(
            "job messages were still queued when the program exited, so its tree limits \
             could not be checked",
        ));
    }
    Ok(limits.reached)
}

/// Take queued job messages without waiting, at most
/// [`MAX_DRAINED_MESSAGES`], and return the limits they report. Windows does
/// not guarantee delivery of these messages; the job enforces the limits
/// either way.
#[allow(unsafe_code)]
fn drain_limit_messages(port: &OwnedHandle) -> Drained {
    let mut drained = Drained {
        full: true,
        ..Drained::default()
    };
    for _ in 0..MAX_DRAINED_MESSAGES {
        let mut message = 0u32;
        let mut key = 0usize;
        let mut overlapped: *mut OVERLAPPED = null_mut();
        // SAFETY: the port handle is owned and live; the three outputs are
        // writable locals. A zero timeout never blocks. Job messages carry a
        // process ID in `overlapped`, which is never dereferenced.
        let result = unsafe {
            GetQueuedCompletionStatus(
                port.as_raw_handle() as HANDLE,
                &raw mut message,
                &raw mut key,
                &raw mut overlapped,
                0,
            )
        };
        if result == 0 {
            // No packet: the queue is empty (or the port failed, which a
            // later poll or the job's own enforcement still covers).
            drained.full = false;
            break;
        }
        match message {
            JOB_OBJECT_MSG_JOB_MEMORY_LIMIT => drained.memory = true,
            JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT => drained.processes = true,
            _ => {}
        }
    }
    drained
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

    fn is_empty(&mut self) -> io::Result<bool> {
        Ok(job_accounting(&self.job)?.ActiveProcesses == 0)
    }

    fn limit_reached(&mut self) -> io::Result<Option<ResourceLimit>> {
        match &mut self.limits {
            Some(limits) => job_limit_reached(&self.job, limits, false),
            None => Ok(None),
        }
    }

    fn limit_reached_at_exit(&mut self) -> io::Result<Option<ResourceLimit>> {
        match &mut self.limits {
            Some(limits) => job_limit_reached(&self.job, limits, true),
            None => Ok(None),
        }
    }

    fn usage(&mut self) -> io::Result<Option<TreeUsage>> {
        if self.limits.is_none() {
            return Ok(None);
        }
        let units = job_cpu_units(&self.job)?;
        Ok(Some(TreeUsage {
            cpu_time: Duration::from_nanos(units.saturating_mul(100)),
            peak_memory_bytes: Some(job_peak_memory(&self.job)?),
            peak_disk_bytes: None,
        }))
    }
}

struct Created {
    process: OwnedHandle,
    thread: OwnedHandle,
}

/// Put the calling process in a new job with these limits, nested in any job
/// it is already in, as a tool with its own stricter job would. Tests only.
#[cfg(test)]
#[allow(unsafe_code)]
pub(super) fn join_nested_job(tree: TreeLimits) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let (job, _port) = create_job(Some(tree))?;
    // SAFETY: the job handle is owned and live; the current-process
    // pseudo-handle needs no closing.
    let result =
        unsafe { AssignProcessToJobObject(job.as_raw_handle() as HANDLE, GetCurrentProcess()) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Create the run's job. With tree limits, the job also caps its committed
/// memory and active processes, and reports either limit to a new completion
/// port. The port is associated before any process joins the job.
#[allow(unsafe_code)]
fn create_job(tree: Option<TreeLimits>) -> io::Result<(OwnedHandle, Option<JobLimits>)> {
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
    let state = match tree {
        Some(tree) => {
            limits.BasicLimitInformation.LimitFlags |=
                JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.JobMemoryLimit = usize::try_from(tree.max_memory_bytes)
                .map_err(|_| io::Error::other("the memory limit does not fit this platform"))?;
            limits.BasicLimitInformation.ActiveProcessLimit = tree.max_processes;
            let port = associate_port(&job)?;
            // The job accounts CPU time in 100-nanosecond units.
            let max_cpu_units = u64::try_from(tree.max_cpu_time.as_nanos() / 100)
                .map_err(|_| io::Error::other("the CPU-time limit is too large"))?;
            Some(JobLimits {
                port,
                max_cpu_units,
                max_memory_bytes: tree.max_memory_bytes,
                max_processes: tree.max_processes,
                reached: None,
            })
        }
        None => None,
    };
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
    Ok((job, state))
}

/// Create a completion port and associate it with `job`, so the job posts
/// its limit messages there.
#[allow(unsafe_code)]
fn associate_port(job: &OwnedHandle) -> io::Result<OwnedHandle> {
    // SAFETY: an invalid file handle with a null existing port creates a new
    // port with one concurrent thread; no pointer is passed.
    let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle was just returned by `CreateIoCompletionPort` and is
    // owned here.
    let port = unsafe { OwnedHandle::from_raw_handle(handle) };
    let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        // The key is never read; the port serves this job only.
        CompletionKey: null_mut(),
        CompletionPort: port.as_raw_handle() as HANDLE,
    };
    let length = u32::try_from(size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>())
        .map_err(|_| io::Error::other("job port structure is too large"))?;
    // SAFETY: both handles are live; `association` is a readable structure of
    // exactly `length` bytes for the matching information class. The tree
    // keeps the port open for as long as it keeps the job.
    let result = unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as HANDLE,
            JobObjectAssociateCompletionPortInformation,
            (&raw const association).cast(),
            length,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(port)
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

/// A process/thread attribute list that names the inherited handles, the job,
/// and, for a confined run, the AppContainer.
///
/// The handle and job arrays, the capabilities structure, and the SIDs it
/// points to are heap-allocated because the list keeps pointers to them until
/// `DeleteProcThreadAttributeList`, which runs in `Drop`.
struct AttributeList {
    buffer: Vec<u64>,
    handles: Box<[HANDLE; 3]>,
    jobs: Box<[HANDLE; 1]>,
    capabilities: Option<Box<SECURITY_CAPABILITIES>>,
    container: Option<Sid>,
    /// The `internetClient` capability SID, for a run with network access.
    internet_client: Option<Sid>,
    /// The capability array `capabilities` points to.
    capability_list: Option<Box<[SID_AND_ATTRIBUTES; 1]>>,
}

impl AttributeList {
    #[allow(unsafe_code)]
    fn new(
        handles: [HANDLE; 3],
        job: HANDLE,
        container: Option<&Sid>,
        network: NetworkAccess,
    ) -> io::Result<Self> {
        let count = if container.is_some() { 3 } else { 2 };
        let mut size = 0usize;
        // SAFETY: a null list with a writable size queries the required size.
        // The expected result is failure with ERROR_INSUFFICIENT_BUFFER.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &raw mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = size.div_ceil(size_of::<u64>());
        let mut list = Self {
            buffer: vec![0u64; words],
            handles: Box::new(handles),
            jobs: Box::new([job]),
            capabilities: None,
            container: container.cloned(),
            internet_client: None,
            capability_list: None,
        };
        // SAFETY: the buffer is writable, 8-byte aligned, and at least `size` bytes.
        let result =
            unsafe { InitializeProcThreadAttributeList(list.pointer(), count, 0, &raw mut size) };
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
        if let Some(sid) = &list.container {
            // Without network access, no capabilities: no network, no
            // loopback, no brokered resources. With it, only `internetClient`:
            // outbound internet, still no private network or loopback.
            let (capability_pointer, capability_count) = match network {
                NetworkAccess::Denied => (null_mut(), 0),
                NetworkAccess::Internet => {
                    let internet = Sid::well_known(WinCapabilityInternetClientSid)?;
                    let mut entries = Box::new([SID_AND_ATTRIBUTES {
                        Sid: internet.as_psid(),
                        Attributes: SE_GROUP_ENABLED.unsigned_abs(),
                    }]);
                    let pointer = entries.as_mut_ptr();
                    list.internet_client = Some(internet);
                    list.capability_list = Some(entries);
                    (pointer, 1)
                }
            };
            let capabilities = Box::new(SECURITY_CAPABILITIES {
                AppContainerSid: sid.as_psid(),
                Capabilities: capability_pointer,
                CapabilityCount: capability_count,
                Reserved: 0,
            });
            let pointer: *const SECURITY_CAPABILITIES = &raw const *capabilities;
            list.capabilities = Some(capabilities);
            // SAFETY: the list is initialized. The boxed structure, the
            // capability array, and the SID storage they point to are owned by
            // the list and outlive it; moving a box or a `Sid` into the list
            // does not move the heap storage the pointers name.
            let result = unsafe {
                UpdateProcThreadAttribute(
                    list.pointer(),
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    pointer.cast(),
                    size_of::<SECURITY_CAPABILITIES>(),
                    null_mut(),
                    null(),
                )
            };
            if result == 0 {
                return Err(io::Error::last_os_error());
            }
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

#[cfg(test)]
mod limit_tests {
    use super::*;
    use windows_sys::Win32::System::IO::PostQueuedCompletionStatus;
    use windows_sys::Win32::System::SystemServices::JOB_OBJECT_MSG_NEW_PROCESS;

    fn limited_job() -> (OwnedHandle, JobLimits) {
        let (job, limits) = create_job(Some(TreeLimits {
            max_memory_bytes: 64 * 1024 * 1024,
            max_processes: 4,
            max_cpu_time: Duration::from_secs(60),
            max_disk_bytes: None,
        }))
        .expect("job");
        (job, limits.expect("job limits"))
    }

    /// Queue a job message as Windows would, including one a nested job
    /// raised and Windows routed to this ancestor's port.
    #[allow(unsafe_code)]
    fn post(port: &OwnedHandle, message: u32) {
        // SAFETY: the port handle is owned and live; a null overlapped pointer
        // is valid for a posted packet.
        let result = unsafe {
            PostQueuedCompletionStatus(port.as_raw_handle() as HANDLE, message, 0, null_mut())
        };
        assert_ne!(result, 0, "post a job message");
    }

    /// An empty job has reached neither limit, so messages that report them
    /// came from elsewhere, such as a nested job, and do not end the run.
    #[test]
    fn unconfirmed_limit_messages_are_not_the_jobs_limit() {
        let (job, mut limits) = limited_job();
        post(&limits.port, JOB_OBJECT_MSG_JOB_MEMORY_LIMIT);
        post(&limits.port, JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT);
        assert_eq!(
            job_limit_reached(&job, &mut limits, true).expect("check"),
            None
        );
        assert_eq!(drain_limit_messages(&limits.port), Drained::default());
    }

    /// One poll takes a bounded number of messages and leaves the rest for
    /// the next, so a stream of process messages cannot stall supervision.
    #[test]
    fn one_drain_takes_a_bounded_number_of_messages() {
        let (_job, limits) = limited_job();
        for _ in 0..MAX_DRAINED_MESSAGES {
            post(&limits.port, JOB_OBJECT_MSG_NEW_PROCESS);
        }
        post(&limits.port, JOB_OBJECT_MSG_JOB_MEMORY_LIMIT);
        let first = drain_limit_messages(&limits.port);
        assert_eq!(
            first,
            Drained {
                full: true,
                ..Drained::default()
            }
        );
        let second = drain_limit_messages(&limits.port);
        assert_eq!(
            second,
            Drained {
                memory: true,
                ..Drained::default()
            }
        );
    }

    /// A process that stays alive, reading standard input until it closes.
    fn sleeper() -> std::process::Child {
        std::process::Command::new("findstr.exe")
            .arg("x")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("start a sleeper")
    }

    /// Watch a program that has already exited, in a job that holds its one
    /// allowed process, with `ahead` other messages queued before the job's
    /// process-limit report.
    #[allow(unsafe_code)]
    fn watch_exited_program(ahead: usize) -> io::Result<Termination> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let (job, limits) = create_job(Some(TreeLimits {
            max_memory_bytes: 64 * 1024 * 1024,
            max_processes: 1,
            max_cpu_time: Duration::from_secs(60),
            max_disk_bytes: None,
        }))
        .expect("job");
        let limits = limits.expect("job limits");
        // The job holds its one allowed process, so a process-limit message
        // is the job's own.
        let mut held = sleeper();
        // SAFETY: both handles are live; the sleeper is in no job of ours yet.
        let joined = unsafe {
            AssignProcessToJobObject(
                job.as_raw_handle() as HANDLE,
                held.as_raw_handle() as HANDLE,
            )
        };
        assert_ne!(joined, 0, "put the sleeper in the job");
        for _ in 0..ahead {
            post(&limits.port, JOB_OBJECT_MSG_NEW_PROCESS);
        }
        post(&limits.port, JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT);
        let mut program = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "exit 0"])
            .spawn()
            .expect("start the program");
        program.wait().expect("the program exits");
        let mut tree = WindowsTree {
            job,
            process: OwnedHandle::from(program),
            limits: Some(limits),
        };
        let watched = super::super::watch(
            &mut tree,
            std::time::Instant::now(),
            Duration::from_secs(60),
            &super::super::CancelSignal::new(),
            None,
        );
        drop(tree);
        // The job's kill-on-close has ended the sleeper.
        held.wait().expect("the sleeper ends");
        watched
    }

    /// A limit reported behind more than one poll's worth of other messages
    /// still decides the run once the program has exited, and is confirmed
    /// before the tree is stopped.
    #[test]
    fn an_exit_takes_every_queued_limit_report() {
        let termination = watch_exited_program(MAX_DRAINED_MESSAGES + 44).expect("watch");
        assert_eq!(
            termination,
            Termination::LimitExceeded {
                limit: ResourceLimit::Processes
            }
        );
    }

    /// When more messages are queued at exit than the bounded check takes,
    /// the run is not reported as a normal exit.
    #[test]
    fn an_exit_beyond_the_message_budget_is_not_a_normal_exit() {
        let watched = watch_exited_program(MAX_DRAINED_MESSAGES * MAX_EXIT_BATCHES + 1);
        let error = watched.expect_err("the check could not finish");
        assert!(error.to_string().contains("still queued"), "{error}");
    }
}
