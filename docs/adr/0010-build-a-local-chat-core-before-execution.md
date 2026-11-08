# 0010: Build a local chat core before execution

- Status: Accepted
- Date: 2026-11-08

## Context

The planned coding agent needs reliable prompt assembly, provider requests, and
durable sessions before it can safely dispatch commands. Windows and Linux are
both first-release targets. An early demonstration must not acquire general
host execution merely because a model can emit tool calls.

## Decision

Introduce a Rust 1.99.1, edition 2024 workspace with a portable core, a developer
CLI, and a small native-platform crate. Pin the toolchain and commit Cargo.lock.
Use LM Studio's local Chat Completions surface for the first runnable increment.
Require an explicit model and validated context profile. Restrict this adapter
to literal-loopback HTTP with no redirects, proxies, retries, or cloud fallback.

Compile the visible base prompt and retain user additions at user priority.
Record the request's instructions and model profile before inference. Write
bounded typed JSONL events with exclusive writer ownership. Resume never
executes historical actions. Strictly reject malformed or incomplete journals
until a separate evidence-preserving repair implementation is reviewed.

Advertise no tools and reject all structured model tool requests. Do not expose
workspace reads/writes, processes, Git, MCP, or an unrestricted fallback while
their runtime controls are absent. This is a prototype sequence, not a change
to the required first-release capabilities or subscription-authentication gates.

Keep unsafe code out of core and CLI. Use one safe Windows wrapper around
GetFileInformationByHandle in the platform crate to inspect the already opened
journal's link count and reparse attributes. Stable Rust's Windows metadata API
does not expose this count. Limit its unsafe allowance to that function and
document handle lifetime, output initialization, and ownership locally.

The [development guide](../development.md) owns commands, dependency rationale,
implemented limits, and the distinction between current and planned behaviour.

## Alternatives considered

- Build the complete UI and execution runner first: rejected for this increment
  because it expands the unverified trust boundary before persistence and model
  request handling can be exercised independently.
- Let the model invoke an ordinary host shell temporarily: rejected because
  prompts cannot enforce the required filesystem and process restrictions.
- Use containers or another assistant's runtime as an implicit fallback:
  rejected because availability alone does not establish the required native
  policy, lifecycle, credential, and descendant-process guarantees.
- Use unstable Windows metadata APIs or omit hard-link checks: rejected in
  favour of a narrow stable native API wrapper with native tests.
- Implement an HTTP stack or cryptographic digest locally: rejected in favour
  of maintained libraries with deliberately selected features.

## Consequences

The first build can test a real local conversation and session lifecycle on
both operating systems without exposing an execution route. It cannot yet do
coding-agent work. The CLI is temporary; its trusted raw journal path must not
be exposed as future web IPC. Scope-aware settings, the project registry,
credential storage, streamed output, compaction, usage journaling, and recovery
UI remain required. File-handle checks do not establish containment, protected
Windows directory ACLs, or power-loss durability. Qualify these separately.

## References

- [Instruction contract](../agent-instructions.md)
- [Session contract](../session-storage.md)
- [Security rules](../security-coding-rules.md)
- [GetFileInformationByHandle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle)
- [BY_HANDLE_FILE_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/ns-fileapi-by_handle_file_information)
