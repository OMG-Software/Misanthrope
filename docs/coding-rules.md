# Coding rules

These requirements apply to Misanthrope production code, tests, generated code,
and project tooling. Workflow requirements also cover documentation.
**Must** means required. **Prefer** defines the default; explain a departure
where the choice can be reviewed.

The project has no implementation yet. These are contribution requirements,
not a claim that automated enforcement already exists.

## Authority and scope

- Read the root and relevant directory contributor guidance before changes.
  Resolve conflicting requirements before changing the affected area.
- This file owns project-wide coding conventions. Follow the
  [security rules](security-coding-rules.md),
  [testing standards](testing-standards.md), and
  [platform targets](platform-support.md) as well.
- Provider work must follow the [authentication contract](provider-authentication.md).
  Runner and toolchain work must follow the [development-tool contract](development-tools.md).
- Check provider and operating-system contracts against their official
  documentation. Another assistant's output or a passing test is not authority.
- Document assumptions, limits, and failure behaviour. Do not claim that Rust's
  memory safety establishes sandbox containment or application correctness.
- Apply frontend security and workflow rules to TypeScript as well. Rust naming
  and ownership rules do not apply to TypeScript syntax.

## Design, modules, and naming

- Prefer straightforward control flow and small interfaces. Make security
  boundaries visible in the design.
- Organize code by a cohesive responsibility. Split independent public types
  and unrelated behaviour. Small supporting types may stay with their primary
  type. Do not impose a fixed one-type-per-file count.
- Use `snake_case` for module files, modules, functions, variables, and fields.
  Use `UpperCamelCase` for types, traits, and enum variants, and
  `SCREAMING_SNAKE_CASE` for constants. Do not add an `m_` field prefix.
- Prefer `module.rs` with a `module/` directory for submodules. Keep a type's
  implementation with its definition unless responsibility or size warrants a
  split. Rust has no separate public-header tree.
- Keep items private unless another module needs them. Prefer `pub(crate)` to
  `pub` for crate-internal interfaces. Avoid exposing representation fields
  that let callers bypass an invariant.
- Model distinct identifiers and valid states with types and enums where useful.
  Avoid strings and boolean combinations that allow invalid states.
- Use traits for a real abstraction or replaceable boundary. Do not create a
  trait for every struct solely to make it mockable.
- Prefer ordinary functions and explicit types to complex macros or generic
  machinery when they provide the same behaviour.
- Use explicit imports. Wildcard imports are allowed for a local test module's
  `use super::*`; justify other exceptions. Let rustfmt control import layout.
- Write `///` documentation for public items and contracts that are not obvious.
  Explain purpose, errors, ownership, side effects, cancellation, required locks,
  and security assumptions where relevant. Private helpers need comments when
  their reason or contract is unclear; do not restate an obvious function name.

## Ownership and resource lifetime

- Start with owned values, collections, and borrowing. Use references, slices,
  and `&str` when a callee only needs borrowed data.
- Use `Box<T>` when single-owner heap indirection is needed. Do not use shared
  ownership to avoid deciding which component owns a resource.
- Do not add `clone()` or a `'static` bound merely to silence a lifetime error.
  First check the ownership design. Clone when copying or shared ownership is
  intentional; consider the cost and secret lifetime.
- Use `Arc<T>` for necessary cross-thread shared ownership and `Rc<T>` only in
  deliberately single-threaded code. At the introduction of shared ownership,
  document the owners, destruction point, and cycle risk. One clear ownership
  explanation can cover subsequent clones.
- Reference counting does not protect mutable data. Use a suitable lock,
  atomic, or single-owner message loop. Use `Weak<T>` where a back-reference
  must not keep its owner alive, and handle failed upgrades.
- Interior mutability is allowed when its synchronization or borrowing contract
  is clear. `Arc<Mutex<T>>` is a valid pattern when shared mutable ownership is
  required; review contention, lock order, and lifetime.
- Use ownership and `Drop` for resource cleanup on normal and error paths.
  Prefer standard or reviewed owned file, socket, and handle wrappers. Use each
  operating-system resource's correct release operation.
- Keep raw pointers and raw handle ownership conversions inside the reviewed
  platform/FFI boundary. A borrowed raw handle must not outlive its owner.
- Destructors must not panic. Cleanup that can fail needs an explicit operation
  returning `Result` and a documented fallback for `Drop`.
- Shutdown that requires waiting must be explicit. Dropping a process or task
  handle does not prove that the process or task stopped.

## Errors, panics, and contracts

- Return `Result<T, E>` for recoverable failures. Use `Option<T>` for absence
  when absence is not an error. Prefer `?` for propagation with safe context.
- Prefer typed errors at domain and component boundaries. Preserve the cause
  when converting errors. General error containers may be used at application
  reporting boundaries; do not erase a failure category that policy depends on.
- Invalid input, denied permissions, provider failures, missing files, and
  unavailable sandbox features must return controlled failures.
- Do not use `unwrap()`, `expect()`, `panic!`, unchecked indexing, or
  `unreachable!` to handle untrusted input or routine runtime failure.
  A production invariant panic requires a local explanation of why supported
  inputs cannot reach it and why recovery would be invalid. Tests may use
  `expect()` with useful messages.
- Do not use panic catching as ordinary error handling. Account for panic
  behaviour at FFI boundaries, task supervision, and cleanup.
- Handle every `Result`. An intentional discard needs a narrow explanation;
  `let _ =` is not a way to hide a failure. Add `#[must_use]` to project
  outcomes whose omission would be a bug; `Result` already carries it.
- Define how poisoned locks and partially completed operations are handled.
  Do not silently continue with state whose invariants are uncertain.
- Use checked conversions and arithmetic for sizes, offsets, counters, and
  externally supplied numbers. An `as` cast does not check a numeric range.
- Bound input sizes, allocations, nesting, streaming buffers, queues, output,
  and retries. Never turn an error, timeout, or incomplete result into success.

## Unsafe code and foreign interfaces

- Safe Rust is the default. Portable policy, session, and provider code must
  forbid unsafe code in their crate boundary when the crates are introduced.
- Keep necessary unsafe code in a small, identified platform or FFI module or
  crate. Explain why a safe interface is insufficient and review the boundary.
- Each unsafe block and unsafe trait implementation needs a local
  `// SAFETY:` justification covering its actual obligations, such as pointer
  validity, initialization, ownership, aliasing, and thread access.
- An unsafe function needs a documented `# Safety` caller contract. Prefer a
  safe wrapper that checks inputs and preserves that contract.
- Review unsafe extern declarations and unsafe attributes for correct ABI,
  signatures, symbol names, and linking assumptions. Document their obligations.
- Deny `unsafe_op_in_unsafe_fn`. Unsafe operations still need explicit blocks
  inside an unsafe function. Do not use `unsafe impl Send` or `Sync` to
  bypass an ownership or concurrency design error.
- Do not use `static mut`, unchecked conversions, or custom memory-management
  tricks in ordinary application code.
- Review dependency build scripts, procedural macros, and unsafe dependencies.
  Forbidding unsafe in this project's crate does not audit its dependencies.

## Async work, concurrency, and shutdown

- Prefer a single owner with bounded message channels for mutable service state.
  Use shared locks where they fit the access pattern.
- Keep critical sections small. Do not hold a synchronous lock guard across
  `.await`, a blocking call, or network I/O. An async lock held across
  `.await` requires an explicit ownership and cancellation design; prefer
  taking a snapshot or transferring work outside the lock.
- Document lock ordering when more than one lock can be acquired. Review
  reentrancy, callbacks, and error paths as part of that order.
- Keep blocking filesystem, database, process-wait, and CPU-heavy work off async
  executor workers. Use a bounded blocking facility or a supervised process.
- Give every spawned task a responsible supervisor. Retain its completion
  handle, handle its errors, and define cancellation and shutdown behaviour.
- A Tokio `JoinHandle` detaches on drop. An abort request needs completion
  to be observed. Started `spawn_blocking` work cannot be stopped by abort;
  use cooperative cancellation or a process when forced termination is needed.
- Design cancellation at every suspension point that can leave partial state.
  Do not assume dropping a future rolls back filesystem or external effects.
- Cap parallel work, queue length, time, and output. Release permits and guards
  through owned scopes on all paths.
- Terminate and reap supervised child processes and their descendants. A stopped
  root process or a dropped process handle is not evidence of tree cleanup.
- Thread-safe types and successful compilation do not prove that a state
  transition, permission revocation, or shutdown sequence is correct.

## Files, persistence, and platform boundaries

- Use `Path`/`PathBuf` and `OsStr`/`OsString` for native filesystem paths
  and process arguments. Do not require UTF-8 without an explicit interface
  contract. Lossy text is for display, never authorization or reopening a path.
- Keep byte, character, and frontend text offsets distinct. Validate conversions
  and the content version before applying a patch.
- Follow the security guide for access checks and races. A canonicalized path
  or a string prefix alone does not establish safe access.
- Put platform-specific code behind narrow interfaces and conditional modules.
  Unsupported capabilities must return explicit errors.
- Use parameterized database statements. Keep transactions short and avoid
  holding them across unrelated awaits or network requests.
- Treat migrations as data changes. Number them monotonically, never rewrite
  an applied migration, and test upgrade failure and recovery before adoption.
- Make retries safe for the operation. Do not automatically repeat a write,
  command, commit, or push after an ambiguous result. Reconcile observed state.

## Toolchain, dependencies, and formatting

- This section selects the toolchain for building Misanthrope itself.
  User-project compilers, interpreters, and development tools have separate
  scoped profiles defined in the development-tool contract.
- When the first Cargo project is added, select a stable Rust edition and pin
  an exact toolchain in `rust-toolchain.toml`, including the rustfmt and Clippy
  components. Set the supported minimum version in `rust-version` and verify
  that claim separately where it differs.
- Use the pinned toolchain's default rustfmt style. Put any agreed exceptions
  in a reviewed configuration; do not copy C++ formatting or include-order rules.
- Commit `Cargo.lock` for this application and use locked resolution in normal
  verification. Change dependencies through Cargo and review lockfile changes.
- Declare deliberate dependency features. Minimize optional features and audit
  their security effect. Git dependencies require an immutable revision.
- Justify a new dependency by its purpose, maintenance, license, platform
  support, and executable build-time behaviour. Reuse established libraries for
  cryptography and platform primitives.
- Keep compilation and Clippy warning-free with the selected toolchain. Scope
  lint exceptions to the smallest item and explain why they are valid. Do not
  suppress a warning instead of fixing a defect.
- Deny ignored must-use results and unsafe operations without explicit blocks.
  Select additional Clippy lints deliberately; do not enable all restriction or
  pedantic lints without reviewing their effects.
- The future frontend must use strict TypeScript checks, a committed dependency
  lockfile, and documented formatting/lint rules when it is introduced.
- Build, dependency, and CI jobs must not grant untrusted pull requests secrets
  or elevated permissions. Pin external CI actions to immutable revisions.

## Tests and change workflow

- Specify and test new behaviour before changing production behaviour where
  practical. For bug fixes, first reproduce the failure with a regression test
  and confirm it fails against the unfixed baseline. Explain any unsafe or
  impractical reproduction rather than pretending it ran.
- Follow the testing guide for native Rust behaviour tests, real platform
  integration tests, feature selection, and completed-result evidence.
- Update the changelog and relevant owning guide with each change. Preserve
  existing local work. Use feature branches and reviewable changes.
- Record lasting choices in the [ADR register](adr/index.md). Do not copy
  Merovingian-specific modules, Matrix rules, Meson commands, or BSD limits.
- Never weaken a requirement or test assertion to make a check pass without
  evidence that the requirement itself is wrong.

## Enforcement and verification limits

The [repository-checks guide](repository-checks.md) defines review and analysis.
No Cargo manifest, application tests, or frontend tooling exists yet.
When implementation begins, configure and run the checks in the testing
guide and record what each one actually covers. Rustfmt checks formatting;
Clippy and the compiler check selected properties; tests check exercised cases.
None establishes prompt-injection resistance or sandbox correctness by itself.

## Adaptation and references

This guide adapts the design, ownership, error, concurrency, and verification
principles in [Merovingian's coding rules](https://github.com/OMG-Software/Merovingian/blob/main/docs/coding-rules.md).
[Rust adaptation decision](adr/0001-adapt-contributor-rules-to-rust.md) explains
which C++ conventions were replaced.

- [Rust ownership](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html)
- [Rust error handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [Rust naming conventions](https://rust-lang.github.io/api-guidelines/naming.html)
- [Rust style guide](https://doc.rust-lang.org/style-guide/)
- [Shared ownership with Arc](https://doc.rust-lang.org/std/sync/struct.Arc.html)
- [Unsafe Rust contracts](https://doc.rust-lang.org/reference/unsafe-keyword.html)
- [Cargo manifests and lockfiles](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html)
- [Tokio task lifetime and cancellation](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html)
