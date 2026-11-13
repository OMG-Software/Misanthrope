//! Sampled disk-usage enforcement for a run's writable roots.
//!
//! Neither platform offers a per-process or per-job disk quota that works
//! without administrative setup. Windows Job Objects have no disk limit, and a
//! cgroup v2 has no disk controller. The supervisor therefore measures the
//! bytes under the confinement's writable paths on a separate thread and stops
//! the tree when the measured growth passes the limit.
//!
//! This is a sampled control, not a kernel-enforced quota. A tree can pass the
//! limit between two samples, and the growth is net: bytes the tree removes
//! are subtracted. Two totals are tracked: allocated bytes and file lengths.
//! Each file contributes to both, the limit applies to each separately, and
//! the larger net growth is recorded. Tracking length catches writes that a
//! filesystem has not written back yet; tracking allocation catches a
//! pre-existing sparse file being filled in. Filesystem metadata, directory
//! entries, inode or MFT use, and cluster slack are not counted. See
//! `docs/execution.md`.
//!
//! Measurement fails closed. If the supervisor cannot read part of a writable
//! root, the run ends as a supervision failure rather than claiming a limit it
//! cannot measure.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Confinement, ProcessError};

/// How often the monitor samples the writable roots.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How often it checks for a stop request while waiting to sample.
const STOP_CHECK: Duration = Duration::from_millis(100);
/// Most entries one sample may examine. A larger tree fails closed because the
/// supervisor cannot finish the measurement it started.
const MAX_SAMPLED_ENTRIES: usize = 4_000_000;

/// Disk usage measured above the run's baseline.
pub(super) struct DiskMonitor {
    state: Arc<DiskState>,
    thread: Option<JoinHandle<()>>,
    roots: Vec<PathBuf>,
    /// File sizes in the writable roots immediately before the program
    /// started.
    baseline: Baseline,
    /// Greatest net growth the run may add.
    limit: u64,
}

#[derive(Default)]
struct DiskState {
    stop: AtomicBool,
    exceeded: AtomicBool,
    /// Highest net growth observed, in bytes.
    peak_growth: AtomicU64,
    /// The first measurement error, if any. Taken once by the supervisor.
    error: Mutex<Option<io::Error>>,
}

/// Two views of the same files. Allocated bytes catch sparse allocation;
/// file lengths catch writes that a filesystem has not written back yet. The
/// limit applies to each separately, and the larger net growth is recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Baseline {
    allocated: u64,
    logical: u64,
}

/// The greatest allocated and logical totals a run may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ceiling {
    allocated: u64,
    logical: u64,
}

struct Measured {
    /// Allocated bytes under the roots. When `exceeded` is set this is a
    /// partial count that already passed a ceiling.
    allocated: u64,
    /// File lengths under the roots, likewise partial when exceeded.
    logical: u64,
    exceeded: bool,
}

impl std::fmt::Debug for Measured {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Measured")
            .field("allocated", &self.allocated)
            .field("logical", &self.logical)
            .field("exceeded", &self.exceeded)
            .finish()
    }
}

impl DiskMonitor {
    /// Measure the writable roots before the program starts.
    ///
    /// `None` means no disk limit. A limit without a confined run, or without
    /// a writable path to measure, is refused before anything is recorded.
    pub(super) fn prepare(
        confinement: Option<&Confinement>,
        limit: Option<u64>,
    ) -> Result<Option<Self>, ProcessError> {
        let Some(limit) = limit else {
            return Ok(None);
        };
        let confinement = confinement.ok_or(ProcessError::InvalidLimits(
            "a disk limit needs a confined run",
        ))?;
        let roots = disk_roots(confinement).map_err(ProcessError::DiskMeasurement)?;
        if roots.is_empty() {
            return Err(ProcessError::InvalidLimits(
                "a disk limit needs at least one writable path",
            ));
        }
        let baseline = measure(&roots, None).map_err(ProcessError::DiskMeasurement)?;
        Ok(Some(Self {
            state: Arc::new(DiskState::default()),
            thread: None,
            roots,
            baseline: Baseline {
                allocated: baseline.allocated,
                logical: baseline.logical,
            },
            limit,
        }))
    }

    /// Start sampling. Called before the program starts, so a failure starts
    /// nothing.
    pub(super) fn start(&mut self) -> Result<(), ProcessError> {
        if self.thread.is_some() {
            return Ok(());
        }
        let state = Arc::clone(&self.state);
        let roots = self.roots.clone();
        let baseline = self.baseline;
        let ceiling = Ceiling {
            allocated: baseline.allocated.saturating_add(self.limit),
            logical: baseline.logical.saturating_add(self.limit),
        };
        let thread = thread::Builder::new()
            .name("omgcode-disk".into())
            .spawn(move || sample_until_stopped(&roots, baseline, ceiling, &state))
            .map_err(ProcessError::DiskMeasurement)?;
        self.thread = Some(thread);
        Ok(())
    }

    /// Whether a sample has already passed the limit.
    ///
    /// # Errors
    ///
    /// Returns the first measurement error. It is taken once; the caller stops
    /// the run, so later calls do not need it.
    pub(super) fn check(&self) -> io::Result<bool> {
        if let Some(error) = take_error(&self.state) {
            return Err(error);
        }
        Ok(self.state.exceeded.load(Ordering::SeqCst))
    }

    /// Stop sampling, then take one final measurement while the tree is
    /// already stopped.
    pub(super) fn finish(&mut self) -> io::Result<()> {
        self.stop();
        if let Some(error) = take_error(&self.state) {
            return Err(error);
        }
        let ceiling = Ceiling {
            allocated: self.baseline.allocated.saturating_add(self.limit),
            logical: self.baseline.logical.saturating_add(self.limit),
        };
        let measured = measure(&self.roots, Some(ceiling))?;
        self.record(&measured);
        Ok(())
    }

    /// Whether any measurement passed the limit.
    pub(super) fn exceeded(&self) -> bool {
        self.state.exceeded.load(Ordering::SeqCst)
    }

    /// Highest net growth observed; zero if nothing was added.
    pub(super) fn peak_growth(&self) -> u64 {
        self.state.peak_growth.load(Ordering::SeqCst)
    }

    fn record(&self, measured: &Measured) {
        let growth = measured
            .allocated
            .saturating_sub(self.baseline.allocated)
            .max(measured.logical.saturating_sub(self.baseline.logical));
        self.state.peak_growth.fetch_max(growth, Ordering::SeqCst);
        if measured.exceeded {
            self.state.exceeded.store(true, Ordering::SeqCst);
        }
    }

    fn stop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // A sampler panic means the limit was not enforced for part of
            // the run. Record it so `finish` fails closed; `check` sees it if
            // the panic happens before the program exits.
            if thread.join().is_err() {
                let mut slot = self
                    .state
                    .error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(io::Error::other("the disk sampler panicked"));
                }
            }
        }
    }
}

impl Drop for DiskMonitor {
    fn drop(&mut self) {
        // Early returns, including a failed start, must not leave the sampler
        // running. It stops within one `STOP_CHECK` interval.
        self.stop();
    }
}

/// Wake every second until a stop is requested, measuring each time.
fn sample_until_stopped(
    roots: &[PathBuf],
    baseline: Baseline,
    ceiling: Ceiling,
    state: &DiskState,
) {
    loop {
        let until = Instant::now() + SAMPLE_INTERVAL;
        while Instant::now() < until {
            if state.stop.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(STOP_CHECK);
        }
        if state.stop.load(Ordering::SeqCst) {
            return;
        }
        match measure(roots, Some(ceiling)) {
            Ok(measured) => {
                let growth = measured
                    .allocated
                    .saturating_sub(baseline.allocated)
                    .max(measured.logical.saturating_sub(baseline.logical));
                state.peak_growth.fetch_max(growth, Ordering::SeqCst);
                if measured.exceeded {
                    state.exceeded.store(true, Ordering::SeqCst);
                    return;
                }
            }
            Err(error) => {
                // Only the first error matters; the supervisor stops the tree.
                let mut slot = state.error.lock().unwrap_or_else(PoisonError::into_inner);
                if slot.is_none() {
                    *slot = Some(error);
                }
                return;
            }
        }
    }
}

fn take_error(state: &DiskState) -> Option<io::Error> {
    // The lock guards only this `Option`; a poisoned lock leaves it readable.
    state
        .error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// Directories (or files) the confined tree can write. On Windows the
/// container also writes to its own profile folder, which is not listed in
/// `read_write` because it needs no grant.
fn disk_roots(confinement: &Confinement) -> io::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for path in &confinement.read_write {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() || metadata.is_file() => roots.push(path.clone()),
            // A character device such as `/dev/null` uses no disk.
            Ok(_) => {}
            // Confinement setup rejects a missing path with its own error.
            Err(_) => {}
        }
    }
    #[cfg(windows)]
    if let Some(container) = &confinement.container {
        // A missing profile is reported by confinement setup with its own
        // error; measure the workspace meanwhile.
        if let Ok(folder) = crate::container::profile_folder(container)
            && folder.is_dir()
        {
            roots.push(folder);
        }
    }
    // A root inside another root would be counted twice.
    let mut outer: Vec<PathBuf> = Vec::new();
    for root in roots {
        if outer.iter().any(|existing| root.starts_with(existing)) {
            continue;
        }
        outer.retain(|existing| !existing.starts_with(&root));
        outer.push(root);
    }
    Ok(outer)
}

/// Sum the bytes under `roots`, stopping once a ceiling is passed.
fn measure(roots: &[PathBuf], ceiling: Option<Ceiling>) -> io::Result<Measured> {
    measure_entries(roots, ceiling, MAX_SAMPLED_ENTRIES)
}

/// [`measure`] with an explicit entry budget, so the fail-closed bound can be
/// tested without creating millions of entries.
fn measure_entries(
    roots: &[PathBuf],
    ceiling: Option<Ceiling>,
    max_entries: usize,
) -> io::Result<Measured> {
    let mut allocated = 0u64;
    let mut logical = 0u64;
    let mut exhausted = false;
    let mut examined = 0usize;
    let mut pending: Vec<PathBuf> = roots.to_vec();
    while let Some(path) = pending.pop() {
        examined = examined.saturating_add(1);
        if examined > max_entries {
            return Err(io::Error::other(
                "the writable roots have too many entries to measure for the disk limit",
            ));
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if is_link_or_reparse(&metadata) {
            continue;
        }
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for entry in entries {
                match entry {
                    Ok(entry) => pending.push(entry.path()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        } else if file_type.is_file() {
            let (file_allocated, file_logical) = file_sizes(&path, &metadata)?;
            allocated = allocated.saturating_add(file_allocated);
            logical = logical.saturating_add(file_logical);
            if ceiling
                .is_some_and(|ceiling| allocated > ceiling.allocated || logical > ceiling.logical)
            {
                exhausted = true;
                break;
            }
        }
    }
    Ok(Measured {
        allocated,
        logical,
        exceeded: exhausted,
    })
}

/// A symbolic link or another reparse point is not followed, so a link inside
/// a writable root cannot make the measurement read outside it or loop.
#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// The allocated and logical byte counts for a regular file. The limits apply
/// to each separately: allocation catches a sparse file being filled in, and
/// length catches writes that a filesystem has not written back yet.
#[cfg(target_os = "linux")]
fn file_sizes(_path: &Path, metadata: &fs::Metadata) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    // `st_blocks` counts 512-byte units.
    Ok((metadata.blocks().saturating_mul(512), metadata.len()))
}

/// The allocated and logical byte counts for a regular file. See the Linux
/// variant for why both are tracked.
#[cfg(windows)]
#[allow(unsafe_code)]
fn file_sizes(path: &Path, metadata: &fs::Metadata) -> io::Result<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};

    // Use a verbatim path so long workspace paths are measured rather than
    // failing the Win32 normalization limit.
    let mut wide: Vec<u16> = Vec::new();
    if !path.as_os_str().to_string_lossy().starts_with(r"\\?\") {
        wide.extend(r"\\?\".encode_utf16());
    }
    wide.extend(path.as_os_str().encode_wide());
    wide.push(0);
    let mut high = 0u32;
    // SAFETY: each call takes only value arguments; `wide` is a
    // NUL-terminated path that outlives the size call and `high` is writable
    // storage for the upper size word. `SetLastError` clears the calling
    // thread's error so a real size whose low word is all ones is not
    // mistaken for a failure.
    let low = unsafe {
        SetLastError(0);
        GetCompressedFileSizeW(wide.as_ptr(), &mut high)
    };
    if low == INVALID_FILE_SIZE {
        // SAFETY: `GetLastError` reads the calling thread's last-error value
        // and touches no other state.
        let error = unsafe { GetLastError() };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(error).unwrap_or(i32::MAX),
            ));
        }
    }
    let allocated = (u64::from(high) << 32) | u64::from(low);
    Ok((allocated, metadata.len()))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn file_sizes(_path: &Path, metadata: &fs::Metadata) -> io::Result<(u64, u64)> {
    Ok((metadata.len(), metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(directory: &Path, name: &str, bytes: usize) -> PathBuf {
        let path = directory.join(name);
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(&vec![b'x'; bytes]).expect("write file");
        file.sync_all().expect("sync file");
        path
    }

    #[test]
    fn measurement_counts_regular_files_under_roots() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        write_file(directory.path(), "one.txt", 4096);
        write_file(&nested, "two.txt", 8192);
        let measured = measure(&[directory.path().to_path_buf()], None).expect("measure");
        assert!(!measured.exceeded);
        assert!(
            measured.allocated >= 12 * 1024 && measured.logical >= 12 * 1024,
            "{measured:?}"
        );
    }

    #[test]
    fn measurement_stops_once_the_ceiling_is_passed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_file(directory.path(), "one.txt", 64 * 1024);
        let measured = measure(
            &[directory.path().to_path_buf()],
            Some(Ceiling {
                allocated: 1,
                logical: 1,
            }),
        )
        .expect("measure past the ceiling");
        assert!(measured.exceeded);
        assert!(
            measured.allocated > 1 && measured.logical > 1,
            "{measured:?}"
        );
    }

    #[test]
    fn measurement_skips_links_and_missing_entries() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_file(directory.path(), "one.txt", 4096);
        let missing = directory.path().join("gone.txt");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(directory.path(), directory.path().join("loop"))
                .expect("symbolic link");
            let measured = measure(&[directory.path().to_path_buf()], None).expect("measure");
            assert!(!measured.exceeded);
            assert!(
                measured.allocated >= 4096 && measured.logical >= 4096,
                "{measured:?}"
            );
        }
        // A root that disappears measures nothing rather than failing.
        let measured = measure(&[missing], None).expect("measure a missing root");
        assert_eq!(measured.allocated, 0);
        assert_eq!(measured.logical, 0);
    }

    #[test]
    fn the_monitor_detects_growth_from_its_thread() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let confinement = Confinement {
            read_write: vec![directory.path().to_path_buf()],
            ..Confinement::default()
        };
        let mut monitor = DiskMonitor::prepare(Some(&confinement), Some(16 * 1024 * 1024))
            .expect("prepare")
            .expect("a disk limit");
        monitor.start().expect("start");
        write_file(directory.path(), "growth.bin", 64 * 1024 * 1024);
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until && !monitor.check().expect("check") {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(monitor.check().expect("check"), "the growth was not seen");
        monitor.finish().expect("finish");
        assert!(monitor.exceeded());
        assert!(monitor.peak_growth() >= 16 * 1024 * 1024);
    }

    #[test]
    fn measurement_fails_closed_past_the_entry_budget() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_file(directory.path(), "one.txt", 4096);
        let root = directory.path().to_path_buf();
        let error = measure_entries(std::slice::from_ref(&root), None, 1)
            .expect_err("the entry budget was exceeded");
        assert!(error.to_string().contains("too many entries"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let locked = directory.path().join("locked");
        fs::create_dir(&locked).expect("locked directory");
        write_file(&locked, "one.txt", 4096);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))
            .expect("remove permissions");
        let measured = measure(&[directory.path().to_path_buf()], None);
        // Restore access before the temporary directory is removed.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700))
            .expect("restore permissions");
        assert!(measured.is_err(), "an unreadable directory was measured");
    }

    #[test]
    fn nested_roots_are_counted_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).expect("nested directory");
        write_file(&nested, "one.txt", 4096);
        let confinement = Confinement {
            read_write: vec![directory.path().to_path_buf(), nested.clone()],
            ..Confinement::default()
        };
        let roots = disk_roots(&confinement).expect("roots");
        assert_eq!(roots, vec![directory.path().to_path_buf()]);
        let outer = measure(&roots, None).expect("measure the outer root");
        let inner = measure(std::slice::from_ref(&nested), None).expect("measure the nested root");
        assert_eq!(outer.allocated, inner.allocated);
        assert_eq!(outer.logical, inner.logical);
    }
}
