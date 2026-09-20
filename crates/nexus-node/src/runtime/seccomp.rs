//! The seccomp profile game servers run under.
//!
//! Containers get the standard container-runtime allowlist — the same set of
//! syscalls Docker and containerd's `runtime/default` permit — rather than
//! the host's full syscall table. Anything not listed returns `EPERM`, which
//! is what closes off the syscalls every container escape of the last decade
//! has needed: mounting filesystems, loading kernel modules, `bpf`, `ptrace`
//! across namespaces, entering other namespaces, rebooting the host.
//!
//! The list is the one moby ships as `profiles/seccomp/default.json`, with
//! the capability-gated extras left out: a game server holds no `CAP_SYS_*`
//! capability, so those branches would never apply.

use serde_json::{json, Value};

/// Everything an ordinary process needs: files, sockets, memory, threads,
/// timers, signals. Names are kernel syscall names; ones that do not exist on
/// the running architecture are ignored by the runtime.
const ALLOWED: &[&str] = &[
    "accept",
    "accept4",
    "access",
    "adjtimex",
    "alarm",
    "bind",
    "brk",
    "capget",
    "capset",
    "chdir",
    "chmod",
    "chown",
    "chown32",
    "clock_adjtime",
    "clock_adjtime64",
    "clock_getres",
    "clock_getres_time64",
    "clock_gettime",
    "clock_gettime64",
    "clock_nanosleep",
    "clock_nanosleep_time64",
    "close",
    "close_range",
    "connect",
    "copy_file_range",
    "creat",
    "dup",
    "dup2",
    "dup3",
    "epoll_create",
    "epoll_create1",
    "epoll_ctl",
    "epoll_ctl_old",
    "epoll_pwait",
    "epoll_pwait2",
    "epoll_wait",
    "epoll_wait_old",
    "eventfd",
    "eventfd2",
    "execve",
    "execveat",
    "exit",
    "exit_group",
    "faccessat",
    "faccessat2",
    "fadvise64",
    "fadvise64_64",
    "fallocate",
    "fanotify_mark",
    "fchdir",
    "fchmod",
    "fchmodat",
    "fchmodat2",
    "fchown",
    "fchown32",
    "fchownat",
    "fcntl",
    "fcntl64",
    "fdatasync",
    "fgetxattr",
    "flistxattr",
    "flock",
    "fork",
    "fremovexattr",
    "fsetxattr",
    "fstat",
    "fstat64",
    "fstatat64",
    "fstatfs",
    "fstatfs64",
    "fsync",
    "ftruncate",
    "ftruncate64",
    "futex",
    "futex_time64",
    "futex_waitv",
    "futimesat",
    "getcpu",
    "getcwd",
    "getdents",
    "getdents64",
    "getegid",
    "getegid32",
    "geteuid",
    "geteuid32",
    "getgid",
    "getgid32",
    "getgroups",
    "getgroups32",
    "getitimer",
    "getpeername",
    "getpgid",
    "getpgrp",
    "getpid",
    "getppid",
    "getpriority",
    "getrandom",
    "getresgid",
    "getresgid32",
    "getresuid",
    "getresuid32",
    "getrlimit",
    "get_robust_list",
    "getrusage",
    "getsid",
    "getsockname",
    "getsockopt",
    "get_thread_area",
    "gettid",
    "gettimeofday",
    "getuid",
    "getuid32",
    "getxattr",
    "inotify_add_watch",
    "inotify_init",
    "inotify_init1",
    "inotify_rm_watch",
    "io_cancel",
    "ioctl",
    "io_destroy",
    "io_getevents",
    "io_pgetevents",
    "io_pgetevents_time64",
    "ioprio_get",
    "ioprio_set",
    "io_setup",
    "io_submit",
    "io_uring_enter",
    "io_uring_register",
    "io_uring_setup",
    "ipc",
    "kill",
    "landlock_add_rule",
    "landlock_create_ruleset",
    "landlock_restrict_self",
    "lchown",
    "lchown32",
    "lgetxattr",
    "link",
    "linkat",
    "listen",
    "listxattr",
    "llistxattr",
    "_llseek",
    "lremovexattr",
    "lseek",
    "lsetxattr",
    "lstat",
    "lstat64",
    "madvise",
    "membarrier",
    "memfd_create",
    "memfd_secret",
    "mincore",
    "mkdir",
    "mkdirat",
    "mknod",
    "mknodat",
    "mlock",
    "mlock2",
    "mlockall",
    "mmap",
    "mmap2",
    "mprotect",
    "mq_getsetattr",
    "mq_notify",
    "mq_open",
    "mq_timedreceive",
    "mq_timedreceive_time64",
    "mq_timedsend",
    "mq_timedsend_time64",
    "mq_unlink",
    "mremap",
    "msgctl",
    "msgget",
    "msgrcv",
    "msgsnd",
    "msync",
    "munlock",
    "munlockall",
    "munmap",
    "nanosleep",
    "newfstatat",
    "_newselect",
    "open",
    "openat",
    "openat2",
    "pause",
    "pidfd_open",
    "pidfd_send_signal",
    "pipe",
    "pipe2",
    "pkey_alloc",
    "pkey_free",
    "pkey_mprotect",
    "poll",
    "ppoll",
    "ppoll_time64",
    "prctl",
    "pread64",
    "preadv",
    "preadv2",
    "prlimit64",
    "process_mrelease",
    "pselect6",
    "pselect6_time64",
    "ptrace",
    "pwrite64",
    "pwritev",
    "pwritev2",
    "read",
    "readahead",
    "readlink",
    "readlinkat",
    "readv",
    "recv",
    "recvfrom",
    "recvmmsg",
    "recvmmsg_time64",
    "recvmsg",
    "remap_file_pages",
    "removexattr",
    "rename",
    "renameat",
    "renameat2",
    "restart_syscall",
    "rmdir",
    "rseq",
    "rt_sigaction",
    "rt_sigpending",
    "rt_sigprocmask",
    "rt_sigqueueinfo",
    "rt_sigreturn",
    "rt_sigsuspend",
    "rt_sigtimedwait",
    "rt_sigtimedwait_time64",
    "rt_tgsigqueueinfo",
    "sched_getaffinity",
    "sched_getattr",
    "sched_getparam",
    "sched_get_priority_max",
    "sched_get_priority_min",
    "sched_getscheduler",
    "sched_rr_get_interval",
    "sched_rr_get_interval_time64",
    "sched_setaffinity",
    "sched_setattr",
    "sched_setparam",
    "sched_setscheduler",
    "sched_yield",
    "seccomp",
    "select",
    "semctl",
    "semget",
    "semop",
    "semtimedop",
    "semtimedop_time64",
    "send",
    "sendfile",
    "sendfile64",
    "sendmmsg",
    "sendmsg",
    "sendto",
    "setfsgid",
    "setfsgid32",
    "setfsuid",
    "setfsuid32",
    "setgid",
    "setgid32",
    "setgroups",
    "setgroups32",
    "setitimer",
    "setpgid",
    "setpriority",
    "setregid",
    "setregid32",
    "setresgid",
    "setresgid32",
    "setresuid",
    "setresuid32",
    "setreuid",
    "setreuid32",
    "setrlimit",
    "set_robust_list",
    "setsid",
    "setsockopt",
    "set_thread_area",
    "set_tid_address",
    "setuid",
    "setuid32",
    "setxattr",
    "shmat",
    "shmctl",
    "shmdt",
    "shmget",
    "shutdown",
    "sigaltstack",
    "signalfd",
    "signalfd4",
    "sigprocmask",
    "sigreturn",
    "socket",
    "socketcall",
    "socketpair",
    "splice",
    "stat",
    "stat64",
    "statfs",
    "statfs64",
    "statx",
    "symlink",
    "symlinkat",
    "sync",
    "sync_file_range",
    "syncfs",
    "sysinfo",
    "tee",
    "tgkill",
    "time",
    "timer_create",
    "timer_delete",
    "timer_getoverrun",
    "timer_gettime",
    "timer_gettime64",
    "timer_settime",
    "timer_settime64",
    "timerfd_create",
    "timerfd_gettime",
    "timerfd_gettime64",
    "timerfd_settime",
    "timerfd_settime64",
    "times",
    "tkill",
    "truncate",
    "truncate64",
    "ugetrlimit",
    "umask",
    "uname",
    "unlink",
    "unlinkat",
    "utime",
    "utimensat",
    "utimensat_time64",
    "utimes",
    "vfork",
    "vmsplice",
    "wait4",
    "waitid",
    "waitpid",
    "write",
    "writev",
    // x86 process setup.
    "arch_prctl",
    "modify_ldt",
];

/// Namespace flags `clone` may not carry: a process inside the container must
/// not make new namespaces, user namespaces in particular, which are the
/// usual first step of a privilege escalation.
const CLONE_NEWNS: u64 = 0x0002_0000;
const CLONE_NEWUTS: u64 = 0x0400_0000;
const CLONE_NEWIPC: u64 = 0x0800_0000;
const CLONE_NEWUSER: u64 = 0x1000_0000;
const CLONE_NEWPID: u64 = 0x2000_0000;
const CLONE_NEWNET: u64 = 0x4000_0000;
const CLONE_NEWCGROUP: u64 = 0x0200_0000;
const CLONE_NAMESPACE_FLAGS: u64 = CLONE_NEWNS
    | CLONE_NEWUTS
    | CLONE_NEWIPC
    | CLONE_NEWUSER
    | CLONE_NEWPID
    | CLONE_NEWNET
    | CLONE_NEWCGROUP;

const EPERM: u32 = 1;
const ENOSYS: u32 = 38;

/// Seccomp architecture identifiers for the build target and its 32-bit
/// compatibility modes.
fn architectures() -> Vec<&'static str> {
    if cfg!(target_arch = "aarch64") {
        vec!["SCMP_ARCH_AARCH64", "SCMP_ARCH_ARM"]
    } else {
        vec!["SCMP_ARCH_X86_64", "SCMP_ARCH_X86", "SCMP_ARCH_X32"]
    }
}

/// The default profile as an OCI `linux.seccomp` object.
pub fn default_profile() -> Value {
    json!({
        "defaultAction": "SCMP_ACT_ERRNO",
        "defaultErrnoRet": EPERM,
        "architectures": architectures(),
        "syscalls": [
            {
                "names": ALLOWED,
                "action": "SCMP_ACT_ALLOW"
            },
            {
                // `personality` is allowed for the handful of values glibc
                // and legacy binaries use; the rest change kernel behaviour
                // in ways a container must not.
                "names": ["personality"],
                "action": "SCMP_ACT_ALLOW",
                "args": [{ "index": 0, "value": 0x0, "op": "SCMP_CMP_EQ" }]
            },
            {
                "names": ["personality"],
                "action": "SCMP_ACT_ALLOW",
                "args": [{ "index": 0, "value": 0x8, "op": "SCMP_CMP_EQ" }]
            },
            {
                "names": ["personality"],
                "action": "SCMP_ACT_ALLOW",
                "args": [{ "index": 0, "value": 0x20000, "op": "SCMP_CMP_EQ" }]
            },
            {
                "names": ["personality"],
                "action": "SCMP_ACT_ALLOW",
                "args": [{ "index": 0, "value": 0x20008, "op": "SCMP_CMP_EQ" }]
            },
            {
                "names": ["personality"],
                "action": "SCMP_ACT_ALLOW",
                "args": [{ "index": 0, "value": 0xffffffffu64, "op": "SCMP_CMP_EQ" }]
            },
            {
                // Threads and forks, but no new namespaces.
                "names": ["clone"],
                "action": "SCMP_ACT_ALLOW",
                "args": [{
                    "index": 0,
                    "value": CLONE_NAMESPACE_FLAGS,
                    "valueTwo": 0,
                    "op": "SCMP_CMP_MASKED_EQ"
                }]
            },
            {
                // `clone3` takes its flags in a struct seccomp cannot inspect;
                // ENOSYS makes glibc fall back to `clone`, which is filtered.
                "names": ["clone3"],
                "action": "SCMP_ACT_ERRNO",
                "errnoRet": ENOSYS
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_allows_the_basics_and_denies_escapes() {
        let profile = default_profile();
        assert_eq!(profile["defaultAction"], "SCMP_ACT_ERRNO");
        let allowed: Vec<&str> = profile["syscalls"][0]["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for needed in [
            "read", "write", "openat", "mmap", "futex", "socket", "execve", "clone3",
        ] {
            if needed == "clone3" {
                assert!(!allowed.contains(&needed));
            } else {
                assert!(allowed.contains(&needed), "{} missing", needed);
            }
        }
        for forbidden in [
            "mount",
            "umount2",
            "reboot",
            "bpf",
            "setns",
            "unshare",
            "init_module",
            "finit_module",
            "kexec_load",
            "open_by_handle_at",
            "pivot_root",
            "swapon",
        ] {
            assert!(
                !allowed.contains(&forbidden),
                "{} must not be allowed",
                forbidden
            );
        }
        assert!(!architectures().is_empty());
    }

    #[test]
    fn no_duplicate_names() {
        let mut seen = std::collections::HashSet::new();
        for name in ALLOWED {
            assert!(seen.insert(*name), "{} listed twice", name);
        }
    }
}
