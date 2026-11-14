//! Tree limits through a child of the runner's delegated cgroup v2.
//!
//! The runner must already run in a cgroup that is delegated to its user, for
//! example a scope started with `systemd-run --user --scope -p Delegate=yes`.
//! On first use, the runner moves itself into a leaf, `omgcode-supervisor`,
//! because cgroup v2 lets a cgroup enable controllers for its children only
//! while it holds no processes. It then enables the `memory` and `pids`
//! controllers for the delegated cgroup's children. Each limited run gets its
//! own child cgroup, which the program joins before `exec`, so every
//! descendant is counted from the start. Nothing is created outside the
//! delegated cgroup, and the runner never asks the system for one.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::super::{ProcessError, ResourceLimit, TreeLimits, TreeUsage};

/// Where the unified cgroup hierarchy is mounted.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// The leaf the runner moves itself into.
const SUPERVISOR: &str = "omgcode-supervisor";
/// Controllers a run needs from the delegated cgroup.
const CONTROLLERS: [&str; 2] = ["memory", "pids"];
/// How long a removal may retry while the kernel finishes releasing tasks.
const REMOVE_WAIT: Duration = Duration::from_secs(1);
const REMOVE_POLL: Duration = Duration::from_millis(10);

/// What to tell an operator whose runner is not in a delegated cgroup.
const NOT_DELEGATED: &str = "tree limits need a delegated cgroup v2; start omgcode in its own \
     scope with `systemd-run --user --scope -p Delegate=yes`";

/// The prepared delegated cgroup, shared by every run in this process.
static BASE: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Distinguishes the run cgroups of one process.
static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

/// Check that tree limits can be applied, preparing the delegated cgroup on
/// first use. This moves the calling process into `omgcode-supervisor`.
pub(in crate::process) fn ensure_available() -> Result<(), ProcessError> {
    delegated_base().map(|_| ())
}

fn delegated_base() -> Result<PathBuf, ProcessError> {
    // The guarded value is only a cached path; a panic while preparing it
    // leaves `None`, and the next caller prepares again.
    let mut base = BASE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = base.as_ref() {
        return Ok(path.clone());
    }
    let prepared = prepare_base()?;
    *base = Some(prepared.clone());
    Ok(prepared)
}

/// Find this process's delegated cgroup, move the process into a leaf below
/// it, and enable the controllers runs need.
fn prepare_base() -> Result<PathBuf, ProcessError> {
    let current = current_cgroup()?;
    // A process started inside a runner's supervisor leaf shares its base.
    let base = if current.file_name().is_some_and(|name| name == SUPERVISOR) {
        current
            .parent()
            .ok_or(ProcessError::Unsupported(NOT_DELEGATED))?
            .to_path_buf()
    } else {
        current.clone()
    };
    if base == Path::new(CGROUP_ROOT) {
        return Err(ProcessError::Unsupported(NOT_DELEGATED));
    }
    let user = rustix::process::geteuid().as_raw();
    for file in [
        base.clone(),
        base.join("cgroup.procs"),
        base.join("cgroup.subtree_control"),
    ] {
        let owner = fs::symlink_metadata(&file)
            .map_err(|_| ProcessError::Unsupported(NOT_DELEGATED))?
            .uid();
        if owner != user {
            return Err(ProcessError::Unsupported(NOT_DELEGATED));
        }
    }
    let available =
        read_text(&base.join("cgroup.controllers")).map_err(ProcessError::Confinement)?;
    if !CONTROLLERS
        .iter()
        .all(|wanted| available.split_whitespace().any(|name| name == *wanted))
    {
        return Err(ProcessError::Unsupported(
            "the delegated cgroup lacks the memory or pids controller",
        ));
    }
    if !base.join("cgroup.kill").exists() {
        return Err(ProcessError::Unsupported(
            "tree limits need cgroup.kill (Linux 5.14 or later)",
        ));
    }
    if current == base {
        let supervisor = base.join(SUPERVISOR);
        match fs::create_dir(&supervisor) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(ProcessError::Confinement(error)),
        }
        write_value(
            &supervisor.join("cgroup.procs"),
            &rustix::process::getpid().as_raw_nonzero().to_string(),
        )
        .map_err(ProcessError::Confinement)?;
    }
    let enabled =
        read_text(&base.join("cgroup.subtree_control")).map_err(ProcessError::Confinement)?;
    let missing: Vec<String> = CONTROLLERS
        .iter()
        .filter(|wanted| !enabled.split_whitespace().any(|name| name == **wanted))
        .map(|name| format!("+{name}"))
        .collect();
    if !missing.is_empty() {
        write_value(&base.join("cgroup.subtree_control"), &missing.join(" ")).map_err(
            |error| {
                if error.raw_os_error() == Some(libc::EBUSY) {
                    ProcessError::Confinement(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        "the delegated cgroup holds other processes; start omgcode in a scope of its own",
                    ))
                } else {
                    ProcessError::Confinement(error)
                }
            },
        )?;
    }
    Ok(base)
}

/// This process's cgroup, which must be in the unified v2 hierarchy.
fn current_cgroup() -> Result<PathBuf, ProcessError> {
    let membership =
        read_text(Path::new("/proc/self/cgroup")).map_err(ProcessError::Confinement)?;
    let mut lines = membership.lines();
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return Err(ProcessError::Unsupported(
            "tree limits need the unified cgroup v2 hierarchy",
        ));
    };
    let relative = line.strip_prefix("0::/").ok_or(ProcessError::Unsupported(
        "tree limits need the unified cgroup v2 hierarchy",
    ))?;
    if !Path::new(CGROUP_ROOT).join("cgroup.controllers").exists() {
        return Err(ProcessError::Unsupported(
            "tree limits need cgroup v2 mounted at /sys/fs/cgroup",
        ));
    }
    let relative = Path::new(relative);
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ProcessError::Unsupported(
            "this process's cgroup path is not usable",
        ));
    }
    Ok(Path::new(CGROUP_ROOT).join(relative))
}

/// One run's cgroup. Dropping it kills any remaining member and removes it,
/// best effort; [`RunCgroup::remove`] reports a failed removal.
pub(super) struct RunCgroup {
    path: PathBuf,
    /// The run cgroup's `cgroup.procs`, open for the child to join.
    procs: File,
    max_cpu_micros: u64,
    reached: Option<ResourceLimit>,
    removed: bool,
}

impl RunCgroup {
    /// Create a child of the delegated cgroup with these limits. Swap is set
    /// to zero where the kernel accounts it, so memory cannot overflow there,
    /// and an out-of-memory kill takes the whole group.
    pub(super) fn create(limits: TreeLimits) -> Result<Self, ProcessError> {
        let base = delegated_base()?;
        let name = format!(
            "omgcode-run-{}-{}",
            rustix::process::getpid().as_raw_nonzero(),
            NEXT_RUN.fetch_add(1, Ordering::Relaxed)
        );
        let path = base.join(name);
        fs::create_dir(&path).map_err(ProcessError::Confinement)?;
        let max_cpu_micros = u64::try_from(limits.max_cpu_time.as_micros())
            .map_err(|_| ProcessError::InvalidLimits("tree CPU-time limit is too large"))?;
        let procs = match OpenOptions::new()
            .write(true)
            .open(path.join("cgroup.procs"))
        {
            Ok(procs) => procs,
            Err(error) => {
                // Nothing joined it yet; removal is best effort.
                let _unreportable = fs::remove_dir(&path);
                return Err(ProcessError::Confinement(error));
            }
        };
        let cgroup = Self {
            path,
            procs,
            max_cpu_micros,
            reached: None,
            removed: false,
        };
        cgroup
            .configure(limits)
            .map_err(ProcessError::Confinement)?;
        Ok(cgroup)
    }

    fn configure(&self, limits: TreeLimits) -> io::Result<()> {
        write_value(
            &self.path.join("memory.max"),
            &limits.max_memory_bytes.to_string(),
        )?;
        let swap = self.path.join("memory.swap.max");
        if swap.exists() {
            write_value(&swap, "0")?;
        }
        write_value(&self.path.join("memory.oom.group"), "1")?;
        write_value(
            &self.path.join("pids.max"),
            &limits.max_processes.to_string(),
        )
    }

    /// The descriptor the child writes to before `exec` to join this cgroup.
    /// It is close-on-exec.
    pub(super) fn procs_fd(&self) -> RawFd {
        self.procs.as_raw_fd()
    }

    /// The limit reached so far. Memory counts an out-of-memory kill;
    /// processes count a refused fork; CPU time compares the cgroup's usage.
    pub(super) fn limit_reached(&mut self) -> io::Result<Option<ResourceLimit>> {
        if self.reached.is_some() {
            return Ok(self.reached);
        }
        let reached = if field(&self.path.join("memory.events"), "oom_kill")?.unwrap_or(0) > 0 {
            Some(ResourceLimit::Memory)
        } else if field(&self.path.join("pids.events"), "max")?.unwrap_or(0) > 0 {
            Some(ResourceLimit::Processes)
        } else if self.cpu_micros()? >= self.max_cpu_micros {
            Some(ResourceLimit::CpuTime)
        } else {
            None
        };
        self.reached = reached;
        Ok(reached)
    }

    fn cpu_micros(&self) -> io::Result<u64> {
        field(&self.path.join("cpu.stat"), "usage_usec")?
            .ok_or_else(|| io::Error::other("cpu.stat has no usage_usec"))
    }

    pub(super) fn usage(&self) -> io::Result<TreeUsage> {
        let peak = self.path.join("memory.peak");
        let peak_memory_bytes = if peak.exists() {
            Some(
                read_text(&peak)?
                    .trim()
                    .parse()
                    .map_err(|_| io::Error::other("memory.peak is not a number"))?,
            )
        } else {
            None
        };
        Ok(TreeUsage {
            cpu_time: Duration::from_micros(self.cpu_micros()?),
            peak_memory_bytes,
            peak_disk_bytes: None,
        })
    }

    /// Kill every process in the cgroup, wherever it is in the tree.
    pub(super) fn kill(&self) -> io::Result<()> {
        write_value(&self.path.join("cgroup.kill"), "1")
    }

    /// Whether no process remains in the cgroup or below it.
    pub(super) fn is_empty(&self) -> io::Result<bool> {
        Ok(field(&self.path.join("cgroup.events"), "populated")? == Some(0))
    }

    /// Remove the empty cgroup. The kernel can report it busy for a moment
    /// after its last member exits, so this retries briefly.
    pub(super) fn remove(&mut self) -> io::Result<()> {
        let until = Instant::now() + REMOVE_WAIT;
        loop {
            match fs::remove_dir(&self.path) {
                Ok(()) => {
                    self.removed = true;
                    return Ok(());
                }
                Err(error)
                    if error.raw_os_error() == Some(libc::EBUSY) && Instant::now() < until =>
                {
                    thread::sleep(REMOVE_POLL);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for RunCgroup {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        // Fallback on an error or unwinding path; the caller already has the
        // error, and nothing here can be reported.
        let _unreportable = self.kill();
        let _unreportable = fs::remove_dir(&self.path);
    }
}

/// Join the cgroup whose `cgroup.procs` is `procs`. Runs in the forked child
/// before `exec`: writing `0` moves the writing process. It allocates nothing.
#[allow(unsafe_code)]
pub(super) fn join(procs: RawFd) -> io::Result<()> {
    let value = b"0";
    // SAFETY: `procs` is a descriptor the parent opened and keeps open until
    // `spawn` returns; `value` is a valid one-byte buffer for the call.
    let written = unsafe { libc::write(procs, value.as_ptr().cast(), value.len()) };
    if written == 1 {
        Ok(())
    } else if written < 0 {
        Err(io::Error::last_os_error())
    } else {
        Err(io::Error::from_raw_os_error(libc::EIO))
    }
}

fn read_text(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Write one value to a cgroup control file in a single `write`.
fn write_value(path: &Path, value: &str) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(path)?
        .write_all(value.as_bytes())
}

/// The number after `key` in a flat-keyed cgroup file such as `memory.events`.
fn field(path: &Path, key: &str) -> io::Result<Option<u64>> {
    let text = read_text(path)?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some(key) {
            return parts
                .next()
                .and_then(|value| value.parse().ok())
                .map(Some)
                .ok_or_else(|| io::Error::other("a cgroup counter is malformed"));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_keyed_fields_are_parsed_exactly() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("memory.events");
        fs::write(
            &path,
            "low 0\nhigh 0\nmax 12\noom 1\noom_kill 3\noom_group_kill 1\n",
        )
        .expect("events");
        assert_eq!(field(&path, "oom_kill").expect("read"), Some(3));
        assert_eq!(field(&path, "max").expect("read"), Some(12));
        assert_eq!(field(&path, "oom_group").expect("read"), None);
        fs::write(&path, "oom_kill many\n").expect("events");
        assert!(field(&path, "oom_kill").is_err());
    }
}
