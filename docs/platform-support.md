# Platform support

## Intended first release

Windows and Linux are first-release targets. This is a target statement, not a
claim of tested application support. No runtime or sandbox backend exists yet.

| Item | Current position |
| --- | --- |
| Windows versions and architectures | To be selected after native sandbox/toolchain prototypes |
| Linux distributions, kernels, and architectures | To be selected after isolation-capability prototypes |
| Rust edition and minimum version | To be selected with the first Cargo workspace |
| Pinned development toolchain | Not selected yet |
| Webview and frontend prerequisites | To be selected with the desktop scaffold |
| User-project toolchains | Required; native installed/managed profile matrix not yet qualified |
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

Read the [security rules](security-coding-rules.md) and
[testing standards](testing-standards.md) for the acceptance requirements.
