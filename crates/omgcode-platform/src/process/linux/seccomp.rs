//! Classic BPF seccomp filters for the runner's child.
//!
//! Every filter kills the process for a foreign ABI (32-bit or x32) and makes
//! `setsid` and `setpgid` fail with `EPERM`, so no descendant can leave the
//! runner's process group. A confined filter also refuses the `io_uring` calls,
//! which can create sockets without `socket`, and every non-Unix address
//! family. Its Unix-domain rule follows [`UnixSocketAccess`]: by default only
//! `socketpair` of stream or sequenced-packet type is allowed; the host-services
//! mode also allows `socket(AF_UNIX)`. Refused calls fail with `EPERM`.
//! Descendants inherit the filter.

use crate::process::{ProcessError, UnixSocketAccess};

// Stable kernel ABI values for a classic BPF seccomp program, combined from
// the class, size, mode, and operation bits in `linux/bpf_common.h`.
/// `BPF_LD (0x00) | BPF_W (0x00) | BPF_ABS (0x20)`.
const BPF_LD_W_ABS: u16 = 0x20;
/// `BPF_ALU (0x04) | BPF_AND (0x50) | BPF_K (0x00)`.
const BPF_ALU_AND_K: u16 = 0x54;
/// `BPF_JMP (0x05) | BPF_JEQ (0x10) | BPF_K (0x00)`.
const BPF_JMP_JEQ_K: u16 = 0x15;
/// `BPF_JMP (0x05) | BPF_JGE (0x30) | BPF_K (0x00)`.
const BPF_JMP_JGE_K: u16 = 0x35;
/// `BPF_RET (0x06) | BPF_K (0x00)`.
const BPF_RET_K: u16 = 0x06;
pub(super) const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub(super) const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
pub(super) const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
/// Offsets in `struct seccomp_data`: `nr`, `arch`, then the low 32 bits of
/// `args[0]` and `args[1]` on a little-endian host. The kernel reads the
/// `socketpair` domain and type as `int`, so the high halves are ignored.
const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;
const SECCOMP_DATA_ARG0_LOW: u32 = 16;
const SECCOMP_DATA_ARG1_LOW: u32 = 24;
/// `SOCK_TYPE_MASK`: the socket type without `SOCK_CLOEXEC` or `SOCK_NONBLOCK`.
const SOCK_TYPE_MASK: u32 = 0xf;

#[cfg(target_arch = "x86_64")]
pub(super) const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
pub(super) const AUDIT_ARCH: u32 = 0xc000_00b7;
/// System calls with this bit set use the x32 ABI, which the filter refuses.
#[cfg(target_arch = "x86_64")]
pub(super) const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// Which restrictions a filter carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilterScope {
    /// Process-group escape denial only, for an unconfined run.
    ProcessGroup,
    /// Process-group escape denial plus socket and `io_uring` denial.
    Confined(UnixSocketAccess),
}

/// Build the filter for `scope` on this architecture.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub(super) fn filter(scope: FilterScope) -> Result<Vec<libc::sock_filter>, ProcessError> {
    let mut program = Program::default();
    program.load(SECCOMP_DATA_ARCH);
    program.jump_eq(AUDIT_ARCH, Target::Next, Target::Kill);
    program.load(SECCOMP_DATA_NR);
    #[cfg(target_arch = "x86_64")]
    program.jump_ge(X32_SYSCALL_BIT, Target::Kill, Target::Next);
    program.deny(libc::SYS_setsid)?;
    program.deny(libc::SYS_setpgid)?;
    match scope {
        FilterScope::ProcessGroup => {}
        FilterScope::Confined(access) => {
            program.deny(libc::SYS_io_uring_setup)?;
            program.deny(libc::SYS_io_uring_enter)?;
            program.deny(libc::SYS_io_uring_register)?;
            unix_sockets(&mut program, access)?;
        }
    }
    program.finish()
}

/// Append the socket rules. Every instruction after them is a verdict, so a
/// call that is neither `socket` nor `socketpair` is allowed here.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn unix_sockets(program: &mut Program, access: UnixSocketAccess) -> Result<(), ProcessError> {
    let socket = syscall_number(libc::SYS_socket)?;
    let socketpair = syscall_number(libc::SYS_socketpair)?;
    match access {
        UnixSocketAccess::PairsOnly => {
            program.jump_eq(socket, Target::Deny, Target::Next);
            program.jump_eq(socketpair, Target::Next, Target::Allow);
            program.load(SECCOMP_DATA_ARG0_LOW);
            program.jump_eq(af_unix()?, Target::Next, Target::Deny);
            // A datagram pair could send to a host path with `sendto`; stream
            // and sequenced-packet sockets refuse or ignore a destination.
            program.load(SECCOMP_DATA_ARG1_LOW);
            program.and(SOCK_TYPE_MASK);
            program.jump_eq(socket_type(libc::SOCK_STREAM)?, Target::Allow, Target::Next);
            program.jump_eq(
                socket_type(libc::SOCK_SEQPACKET)?,
                Target::Allow,
                Target::Deny,
            );
        }
        UnixSocketAccess::HostServices => {
            // `socket` skips the socketpair test and joins the domain check.
            program.jump_eq(socket, Target::Skip(1), Target::Next);
            program.jump_eq(socketpair, Target::Next, Target::Allow);
            program.load(SECCOMP_DATA_ARG0_LOW);
            program.jump_eq(af_unix()?, Target::Allow, Target::Deny);
        }
    }
    Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(super) fn filter(_scope: FilterScope) -> Result<Vec<libc::sock_filter>, ProcessError> {
    Err(ProcessError::Unsupported(
        "the seccomp filter supports only x86_64 and aarch64",
    ))
}

fn syscall_number(number: libc::c_long) -> Result<u32, ProcessError> {
    u32::try_from(number)
        .map_err(|_| ProcessError::Unsupported("system call number is out of range"))
}

fn af_unix() -> Result<u32, ProcessError> {
    u32::try_from(libc::AF_UNIX)
        .map_err(|_| ProcessError::Unsupported("address family is out of range"))
}

fn socket_type(kind: libc::c_int) -> Result<u32, ProcessError> {
    u32::try_from(kind).map_err(|_| ProcessError::Unsupported("socket type is out of range"))
}

/// Where a conditional jump continues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The next instruction.
    Next,
    /// Skip this many following instructions.
    Skip(u8),
    /// Allow the system call.
    Allow,
    /// Fail the system call with `EPERM`.
    Deny,
    /// Kill the process.
    Kill,
}

#[derive(Clone, Copy, Debug)]
enum Instruction {
    Load(u32),
    And(u32),
    JumpEq(u32, Target, Target),
    JumpGe(u32, Target, Target),
}

/// A forward-only program that ends in three shared verdicts. Falling off the
/// last instruction allows the call. Building jump offsets here, instead of by
/// hand, keeps every branch pointed at the verdict it names.
#[derive(Default)]
struct Program {
    instructions: Vec<Instruction>,
}

impl Program {
    fn load(&mut self, offset: u32) {
        self.instructions.push(Instruction::Load(offset));
    }

    fn and(&mut self, mask: u32) {
        self.instructions.push(Instruction::And(mask));
    }

    fn jump_eq(&mut self, value: u32, on_true: Target, on_false: Target) {
        self.instructions
            .push(Instruction::JumpEq(value, on_true, on_false));
    }

    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    fn jump_ge(&mut self, value: u32, on_true: Target, on_false: Target) {
        self.instructions
            .push(Instruction::JumpGe(value, on_true, on_false));
    }

    /// Fail this system call with `EPERM`. The accumulator must hold `nr`.
    fn deny(&mut self, number: libc::c_long) -> Result<(), ProcessError> {
        self.jump_eq(syscall_number(number)?, Target::Deny, Target::Next);
        Ok(())
    }

    fn finish(self) -> Result<Vec<libc::sock_filter>, ProcessError> {
        let allow = self.instructions.len();
        let mut filter = Vec::with_capacity(allow.saturating_add(3));
        for (index, instruction) in self.instructions.iter().enumerate() {
            let offset = |target| jump_offset(index, allow, target);
            filter.push(match *instruction {
                Instruction::Load(at) => statement(BPF_LD_W_ABS, at),
                Instruction::And(mask) => statement(BPF_ALU_AND_K, mask),
                Instruction::JumpEq(value, on_true, on_false) => {
                    jump(BPF_JMP_JEQ_K, value, offset(on_true)?, offset(on_false)?)
                }
                Instruction::JumpGe(value, on_true, on_false) => {
                    jump(BPF_JMP_JGE_K, value, offset(on_true)?, offset(on_false)?)
                }
            });
        }
        filter.push(statement(BPF_RET_K, SECCOMP_RET_ALLOW));
        filter.push(statement(
            BPF_RET_K,
            SECCOMP_RET_ERRNO | errno_data(libc::EPERM),
        ));
        filter.push(statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS));
        Ok(filter)
    }
}

/// The relative jump from instruction `index` to `target`. The verdicts sit at
/// `allow`, `allow + 1`, and `allow + 2`.
fn jump_offset(index: usize, allow: usize, target: Target) -> Result<u8, ProcessError> {
    let destination = match target {
        Target::Next => return Ok(0),
        Target::Skip(count) => return Ok(count),
        Target::Allow => allow,
        Target::Deny => allow.saturating_add(1),
        Target::Kill => allow.saturating_add(2),
    };
    destination
        .checked_sub(index.saturating_add(1))
        .and_then(|distance| u8::try_from(distance).ok())
        .ok_or(ProcessError::Unsupported("the seccomp filter is too long"))
}

const fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

const fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

const fn errno_data(errno: libc::c_int) -> u32 {
    // SECCOMP_RET_DATA is the low 16 bits; EPERM fits.
    (errno as u32) & 0xffff
}

#[cfg(test)]
mod tests;
