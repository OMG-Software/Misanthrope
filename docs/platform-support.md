# Platform support

## Prototype and intended first release

Windows and Linux are first-release targets. This is a target statement, not a
claim of release-qualified application or sandbox support. A Rust prototype
exists. The core records workspace configuration and authorizes a bounded file
broker. The platform crate enforces those broker operations through native
handle-relative access. Neither feature implements whole-user or process
containment.

| Item | Current position |
| --- | --- |
| Windows versions and architectures | CI uses Windows Server 2025; supported client versions and architectures are not selected |
| Linux distributions, kernels, and architectures | CI uses Ubuntu 24.04; supported distributions, kernels, and architectures are not selected |
| Rust edition and minimum version | Edition 2024; workspace `rust-version` is 1.99.1 |
| Pinned development toolchain | Rust 1.99.1 in `rust-toolchain.toml`, with rustfmt and Clippy |
| Webview and frontend prerequisites | Not selected; no desktop frontend exists |
| User-project toolchains | Required; native installed/managed profile matrix not yet qualified |
| Workspace directory identity | Configuration-only metadata check; required native ID and creation time must be available |
| Workspace file broker | Bounded read/list/exclusive-create/single-directory creation through trusted CLI commands; release filesystem support is not qualified |
| Model file tools | Disabled; `workspace_file_tools` remains false |
| Linux broker prerequisites | Linux 5.6 or later for required `openat2`; `/proc/self/fd` must be available for pinned-file reads; unsupported features fail closed |
| macOS | Outside the first-release target |

## Support requirements

- Define a tested OS/version/architecture matrix before the first release.
  Record sandbox primitives and prerequisites for each supported entry.
- Check required capabilities at startup and at sandbox creation. Refuse
  affected operations when required containment cannot be established.
- Exercise native paths, file access, process trees, networking, and IPC on the
  actual target OS. A successful cross-build is insufficient.
- Keep platform implementations behind narrow Rust interfaces. Preserve the
  same policy meaning across backends; report unavailable capabilities clearly.
- The current `misanthrope-platform` crate checks an already-open journal file
  handle for regular-file status, reparse-point status on Windows, and a single
  hard link. Its Windows implementation borrows the handle for
  `GetFileInformationByHandle`; it does not close or transfer the handle.
- Workspace broker operations use retained root/workspace handles and relative
  native operations. Linux requires `openat2` with beneath-root, no-symlink,
  no-magic-link, and no-mount-crossing resolution for workspace paths, plus
  `/proc/self/fd` to reopen a pinned regular file. There is no weaker fallback.
  Windows uses native handle-relative opens and reparse-point checks in narrow
  platform wrappers. See [workspace file access](file-access.md) for exact
  operation limits and failures.
- Resolve the Windows directory-option contract gap described in the
  [file-access guide](file-access.md) before qualifying a Windows backend.
  A successful native test run does not settle an omission in the API contract.
- Workspace registration also records a configuration-time directory identity.
  Linux uses the live handle's device and inode values. It also requires a
  creation time from the filesystem (`statx` birth time, `btime`); if the kernel
  or filesystem does not provide it, capture fails. Do not substitute a
  modification or change time. Windows reads the volume serial and 128-bit file
  ID from an open directory handle with
  `GetFileInformationByHandleEx(FileIdInfo)`, and records its creation time.
  These checks do not authorize later file access, secure every parent path,
  prevent every path-swap race, or contain processes. Filesystem support for
  creation-time metadata is not release-qualified.
- The core and CLI crates forbid unsafe Rust. The platform crate keeps Windows
  FFI in narrow journal and broker handle wrappers. Linux uses Rustix safe
  wrappers for the required native operations. This is not a general permission
  to add unsafe code outside a reviewed platform boundary.
- Treat WSL as Linux execution on a Windows host. A WSL test does not verify
  native Windows process containment.
- Review a virtual-machine backend as a separate design if native containment
  cannot meet the contract. Do not silently change the execution environment.
- Document filesystem, toolchain, privilege, kernel, and virtualization
  requirements when a backend is selected. Do not invent support claims now.
- Qualify representative compiler/linker/SDK, interpreter, formatter, build, and
  test workflows under the [toolchain contract](development-tools.md), including
  a native Windows compiler/SDK profile. A discovered executable is not proof
  that its dependencies and child programs work in confinement.
- Verify subscription sign-in and usage under the
  [authentication contract](provider-authentication.md) on both target systems.
- Qualify LM Studio model connections and both MCP transports on Windows/Linux.
  Verify local server confinement, credential separation, and cleanup.
- Verify [session storage](session-storage.md) on supported filesystems: per-user
  protection, native filename/path handling, exclusive ownership, durable append,
  interrupted-write recovery, and archive/restore across restart.

The workflow matrix in `.github/workflows/rust.yml` runs Rust checks on Ubuntu
24.04 and Windows Server 2025. These CI hosts are not the release support matrix.
Native filesystem, sandbox, process-tree, networking, and toolchain behavior
remain unqualified for release. Windows inherits directory ACLs, and directory
entry power-loss durability is not qualified there. Linux broker behavior
requires an available `openat2` implementation and procfs; distributions and
filesystems have not been selected as supported targets.

Read the [security rules](security-coding-rules.md) and
[testing standards](testing-standards.md) for the acceptance requirements.
