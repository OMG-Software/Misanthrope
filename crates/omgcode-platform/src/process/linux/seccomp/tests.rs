//! Filter tests. A small classic-BPF interpreter evaluates the built program
//! against synthetic `seccomp_data`, so every branch is checked without
//! installing the filter in the test process.

use crate::process::UnixSocketAccess;

use super::{
    AUDIT_ARCH, FilterScope, SECCOMP_RET_ALLOW, SECCOMP_RET_ERRNO, SECCOMP_RET_KILL_PROCESS, filter,
};

const DENIED: u32 = SECCOMP_RET_ERRNO | 1;

/// One system call as the filter sees it: number, architecture, arguments.
struct Call {
    nr: u32,
    arch: u32,
    args: [u64; 6],
}

impl Call {
    fn new(number: libc::c_long, args: &[u64]) -> Self {
        let mut all = [0u64; 6];
        for (slot, value) in all.iter_mut().zip(args) {
            *slot = *value;
        }
        Self {
            nr: u32::try_from(number).expect("syscall number"),
            arch: AUDIT_ARCH,
            args: all,
        }
    }

    /// The little-endian `struct seccomp_data` layout.
    fn bytes(&self) -> [u8; 64] {
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(&self.nr.to_le_bytes());
        data[4..8].copy_from_slice(&self.arch.to_le_bytes());
        for (index, value) in self.args.iter().enumerate() {
            let start = 16 + index * 8;
            data[start..start + 8].copy_from_slice(&value.to_le_bytes());
        }
        data
    }
}

/// Evaluate the subset of classic BPF the builder emits.
fn evaluate(program: &[libc::sock_filter], call: &Call) -> u32 {
    let data = call.bytes();
    let mut accumulator = 0u32;
    let mut pc = 0usize;
    for _ in 0..program.len() {
        let instruction = program[pc];
        match instruction.code {
            0x20 => {
                let at = usize::try_from(instruction.k).expect("offset");
                accumulator = u32::from_le_bytes(data[at..at + 4].try_into().expect("word"));
                pc += 1;
            }
            0x54 => {
                accumulator &= instruction.k;
                pc += 1;
            }
            0x15 | 0x35 => {
                let taken = if instruction.code == 0x15 {
                    accumulator == instruction.k
                } else {
                    accumulator >= instruction.k
                };
                let skip = if taken {
                    instruction.jt
                } else {
                    instruction.jf
                };
                pc += 1 + usize::from(skip);
            }
            0x06 => return instruction.k,
            code => panic!("unexpected instruction {code:#x}"),
        }
    }
    panic!("the program did not return");
}

fn confined() -> Vec<libc::sock_filter> {
    filter(FilterScope::Confined(UnixSocketAccess::PairsOnly)).expect("confined filter")
}

fn host_services() -> Vec<libc::sock_filter> {
    filter(FilterScope::Confined(UnixSocketAccess::HostServices)).expect("host-services filter")
}

fn group_only() -> Vec<libc::sock_filter> {
    filter(FilterScope::ProcessGroup).expect("process-group filter")
}

fn af(family: libc::c_int) -> u64 {
    u64::try_from(family).expect("family")
}

#[test]
fn every_jump_lands_inside_the_program() {
    for program in [confined(), host_services(), group_only()] {
        assert_eq!(
            program.last().map(|last| last.code),
            Some(0x06),
            "the program must end in a return"
        );
        for (index, instruction) in program.iter().enumerate() {
            if matches!(instruction.code, 0x15 | 0x35) {
                for skip in [instruction.jt, instruction.jf] {
                    assert!(index + 1 + usize::from(skip) < program.len());
                }
            }
        }
    }
}

#[test]
fn foreign_architectures_are_killed() {
    for program in [confined(), host_services(), group_only()] {
        let mut call = Call::new(libc::SYS_getpid, &[]);
        call.arch = 0x4000_0003;
        assert_eq!(evaluate(&program, &call), SECCOMP_RET_KILL_PROCESS);
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn x32_calls_are_killed() {
    for program in [confined(), host_services(), group_only()] {
        let mut call = Call::new(libc::SYS_getpid, &[]);
        call.nr |= super::X32_SYSCALL_BIT;
        assert_eq!(evaluate(&program, &call), SECCOMP_RET_KILL_PROCESS);
    }
}

#[test]
fn leaving_the_process_group_is_denied_in_both_scopes() {
    for program in [confined(), host_services(), group_only()] {
        for number in [libc::SYS_setsid, libc::SYS_setpgid] {
            assert_eq!(evaluate(&program, &Call::new(number, &[])), DENIED);
        }
    }
}

#[test]
fn ordinary_calls_are_allowed() {
    for program in [confined(), host_services(), group_only()] {
        for number in [
            libc::SYS_read,
            libc::SYS_openat,
            libc::SYS_clone,
            libc::SYS_execve,
        ] {
            assert_eq!(
                evaluate(&program, &Call::new(number, &[])),
                SECCOMP_RET_ALLOW
            );
        }
    }
}

#[test]
fn every_socket_family_is_denied_when_confined() {
    let program = confined();
    for family in [
        libc::AF_UNIX,
        libc::AF_INET,
        libc::AF_INET6,
        libc::AF_NETLINK,
        libc::AF_PACKET,
    ] {
        let call = Call::new(libc::SYS_socket, &[af(family), 1, 0]);
        assert_eq!(evaluate(&program, &call), DENIED, "family {family}");
    }
}

#[test]
fn io_uring_is_denied_in_both_unix_socket_modes() {
    for program in [confined(), host_services()] {
        for number in [
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
        ] {
            assert_eq!(evaluate(&program, &Call::new(number, &[])), DENIED);
        }
    }
}

#[test]
fn host_services_allow_unix_sockets_but_no_other_family() {
    let program = host_services();
    let datagram = u64::try_from(libc::SOCK_DGRAM).expect("type");
    for number in [libc::SYS_socket, libc::SYS_socketpair] {
        for kind in [1, datagram, 5] {
            let call = Call::new(number, &[af(libc::AF_UNIX), kind, 0]);
            assert_eq!(evaluate(&program, &call), SECCOMP_RET_ALLOW);
        }
        for family in [
            libc::AF_INET,
            libc::AF_INET6,
            libc::AF_NETLINK,
            libc::AF_PACKET,
        ] {
            let call = Call::new(number, &[af(family), 1, 0]);
            assert_eq!(evaluate(&program, &call), DENIED, "family {family}");
        }
    }
}

#[test]
fn only_unix_stream_and_seqpacket_pairs_are_allowed() {
    let program = confined();
    let unix = af(libc::AF_UNIX);
    let flags = u64::try_from(libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK).expect("flags");
    let stream = u64::try_from(libc::SOCK_STREAM).expect("type");
    let seqpacket = u64::try_from(libc::SOCK_SEQPACKET).expect("type");
    let datagram = u64::try_from(libc::SOCK_DGRAM).expect("type");
    let allowed = [
        [unix, stream],
        [unix, stream | flags],
        [unix, seqpacket],
        [unix, seqpacket | flags],
        // The kernel reads the domain as `int`, so the high half is ignored.
        [unix | (1 << 32), stream],
    ];
    for args in allowed {
        let call = Call::new(libc::SYS_socketpair, &args);
        assert_eq!(evaluate(&program, &call), SECCOMP_RET_ALLOW, "{args:?}");
    }
    let denied = [
        [unix, datagram],
        [unix, datagram | flags],
        [af(libc::AF_INET), stream],
        [af(libc::AF_INET6), stream],
        [unix, u64::try_from(libc::SOCK_RAW).expect("type")],
    ];
    for args in denied {
        let call = Call::new(libc::SYS_socketpair, &args);
        assert_eq!(evaluate(&program, &call), DENIED, "{args:?}");
    }
}

#[test]
fn the_process_group_filter_leaves_sockets_alone() {
    let program = group_only();
    let socket = Call::new(libc::SYS_socket, &[af(libc::AF_INET), 1, 0]);
    assert_eq!(evaluate(&program, &socket), SECCOMP_RET_ALLOW);
    let datagram = u64::try_from(libc::SOCK_DGRAM).expect("type");
    let pair = Call::new(libc::SYS_socketpair, &[af(libc::AF_UNIX), datagram]);
    assert_eq!(evaluate(&program, &pair), SECCOMP_RET_ALLOW);
}
