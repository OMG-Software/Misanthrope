# 0001: Adapt contributor rules to Rust

- Status: Accepted
- Date: 2026-11-08

## Context

The project owner uses established C++ rules in Merovingian and requested a Rust
adaptation. Their design and verification goals apply, but several language and
platform conventions do not. Misanthrope also has a different security boundary:
it runs model-directed development tools on Windows and Linux.

## Decision

Make [coding-rules.md](../coding-rules.md) the authority for project-wide coding
conventions and require it from the root contributor guidance. Keep detailed
security and testing requirements in their linked owning guides.

Preserve explicit ownership, small interfaces, reviewed shared ownership,
controlled errors, bounded concurrency, regression tests, and verified outcomes.
Adapt the language mechanisms as follows:

| C++ convention | Rust rule |
| --- | --- |
| Headers and matching implementation files | Cohesive modules; definitions and implementations can share a module |
| Type name matches filename case | Snake-case module files; UpperCamelCase types |
| One class/struct/enum per file | Separate independent responsibilities; keep tightly related helpers together |
| Member-variable prefixes | Standard snake-case fields without an m_ prefix |
| Unique ownership and borrowed views | Owned values, Box when needed, references, and slices |
| Reviewed shared_ptr | Necessary Arc/Rc ownership with documented lifetime and cycle risks |
| Prohibit a shared pointer around a mutex | Permit reviewed Arc<Mutex<T>> when shared mutable ownership is needed |
| expected and nodiscard | Result/Option contracts, must-use checking, and purposeful error handling |
| RAII resource wrappers | Owned resources and Drop, with explicit fallible or async shutdown |
| C API isolation | Small reviewed unsafe/FFI boundaries with stated safety obligations |
| Clang formatting and C++ include rules | Pinned Rust toolchain and standard rustfmt formatting |
| Catch2 and Meson test rules | Native Rust behaviour tests, Cargo targets, and separate doc-test evidence |

A shared-ownership rationale belongs where ownership is introduced. It need not
be copied onto every Arc clone. Keep task completion and process cleanup
explicit; compiler memory-safety checks cannot establish those properties.

## Alternatives considered

- Copy the C++ rules literally: this would add artificial file splits, naming,
  and ownership restrictions while missing Rust task and unsafe-code risks.
- Adopt only generic Rust style: this would omit Misanthrope's permission,
  sandbox, credential, and evidence requirements.
- Duplicate all requirements in contributor guidance: duplicated rules can
  disagree. Mandatory links keep one authoritative source for each rule.

## Consequences

Contributors can follow Rust conventions while retaining the owner's design
and verification requirements. Platform code needs a narrow reviewed unsafe
boundary. A Cargo scaffold and enforcement tooling are follow-up implementation;
this documentation does not claim they already exist.

## References

- [Merovingian coding rules](https://github.com/OMG-Software/Merovingian/blob/main/docs/coding-rules.md)
- [Rust naming conventions](https://rust-lang.github.io/api-guidelines/naming.html)
- [Rust ownership](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html)
- [Cargo test selection](https://doc.rust-lang.org/cargo/commands/cargo-test.html)
