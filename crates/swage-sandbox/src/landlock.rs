//! Landlock LSM filesystem access control.
//!
//! Kernel-enforced write restrictions as a complement to bubblewrap's
//! namespace isolation: the sandboxed process reads and executes everywhere
//! it can see, and writes only the policy's declared write set, `/tmp`, and
//! the standard device nodes. Landlock rules only ever allow, so what the
//! policy masks is hidden by the mount layer, not here. Gracefully degrades
//! on older kernels.
//!
//! This module runs inside the child process, early in agent startup — not
//! in the host. Applying restrictions here holds the write set even where
//! a bind mount makes a path writable.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use swage_core::SandboxError;
use tracing::{debug, warn};

use crate::config::SandboxConfig;

// ---------------------------------------------------------------------------
// Landlock access rights
//
// These constants come from <linux/landlock.h> and are not (yet) exported by
// the libc crate. We define them locally so the crate stays self-contained.
// ---------------------------------------------------------------------------

// ABI 1 (Linux 5.13+) — baseline filesystem access rights.
const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;

// ABI 2 (Linux 5.19+) — adds cross-directory rename support.
const ACCESS_FS_REFER: u64 = 1 << 13;

// ABI 3 (Linux 6.2+) — adds truncate support.
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

// ABI 5 (Linux 6.12+) — restricts device ioctls.
const ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

/// All access flags defined in ABI 1 (Linux 5.13).
const ABI_V1_FLAGS: u64 = ACCESS_FS_EXECUTE
    | ACCESS_FS_WRITE_FILE
    | ACCESS_FS_READ_FILE
    | ACCESS_FS_READ_DIR
    | ACCESS_FS_REMOVE_DIR
    | ACCESS_FS_REMOVE_FILE
    | ACCESS_FS_MAKE_CHAR
    | ACCESS_FS_MAKE_DIR
    | ACCESS_FS_MAKE_REG
    | ACCESS_FS_MAKE_SYM
    | ACCESS_FS_MAKE_SOCK
    | ACCESS_FS_MAKE_FIFO
    | ACCESS_FS_MAKE_BLOCK;

/// All access flags defined in ABI 2 (Linux 5.19).
const ABI_V2_FLAGS: u64 = ABI_V1_FLAGS | ACCESS_FS_REFER;

/// All access flags defined in ABI 3 (Linux 6.2).
const ABI_V3_FLAGS: u64 = ABI_V2_FLAGS | ACCESS_FS_TRUNCATE;

/// All filesystem access flags defined in ABI 5 (Linux 6.12).
///
/// ABI 4 adds network rules but no filesystem flag, so its filesystem mask is
/// identical to ABI 3's.
const ABI_V5_FLAGS: u64 = ABI_V3_FLAGS | ACCESS_FS_IOCTL_DEV;

/// The highest ABI version this implementation is aware of.
const MAX_KNOWN_ABI: i64 = 5;

/// Flag for landlock_create_ruleset() to query the ABI version.
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;

/// Rule type identifier for path-beneath filesystem rules.
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

// Access rights granted to read-only paths.
const READ_ALLOWED: u64 = ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_EXECUTE;

// Access rights granted to read-write paths (ABI 1 baseline).
const WRITE_ALLOWED_BASE: u64 = READ_ALLOWED
    | ACCESS_FS_WRITE_FILE
    | ACCESS_FS_MAKE_REG
    | ACCESS_FS_MAKE_DIR
    | ACCESS_FS_MAKE_SYM
    | ACCESS_FS_REMOVE_FILE
    | ACCESS_FS_REMOVE_DIR;

/// Access required for the device nodes supplied by bubblewrap's `--dev`.
///
/// Each rule names one standard node rather than granting the `/dev`
/// directory. The access bits mirror the node's ordinary role: entropy and
/// zero are read sources, full is a write sink, null is both, and tty supports
/// interactive read/write use when a child has a controlling terminal.
const SYSTEM_DEVICE_RULES: &[(&str, u64)] = &[
    ("/dev/full", ACCESS_FS_WRITE_FILE),
    ("/dev/null", ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE),
    ("/dev/random", ACCESS_FS_READ_FILE),
    ("/dev/tty", ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE),
    ("/dev/urandom", ACCESS_FS_READ_FILE),
    ("/dev/zero", ACCESS_FS_READ_FILE),
];

/// Where the broad read rule is rooted: the sandboxed process reads and
/// executes everything it can see. Private paths are hidden by the mount
/// layer before this rule applies.
const READ_EVERYWHERE: &str = "/";

// ---------------------------------------------------------------------------
// Kernel ABI structs
//
// Defined with #[repr(C)] to match the layout expected by the kernel.
// landlock_path_beneath_attr is `packed` in the kernel header; since the
// natural alignment already places fields at the correct offsets here, the
// practical effect is only on trailing padding.
// ---------------------------------------------------------------------------

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Outcome of applying a Landlock ruleset.
#[derive(Debug, Clone, PartialEq)]
pub enum LandlockStatus {
    /// All requested restrictions were enforced at the highest known ABI.
    Applied,

    /// Restrictions were partially applied — the running kernel supports an
    /// older ABI and some features were unavailable.
    Partial {
        /// Names of features that could not be enforced.
        missing_features: Vec<String>,
    },

    /// The kernel does not support Landlock (< 5.13 or LSM disabled).
    Unavailable,
}

// ---------------------------------------------------------------------------
// Ruleset
// ---------------------------------------------------------------------------

/// Kernel-enforced filesystem access control for a sandboxed agent.
///
/// Calling [`apply`][`LandlockRuleset::apply`] restricts the calling thread
/// (and any threads it creates) to only the paths listed in the ruleset. This
/// is a one-way operation — once applied, restrictions cannot be loosened.
pub struct LandlockRuleset {
    /// Paths the agent is allowed to read (and execute from): the root.
    read_paths: Vec<PathBuf>,
    /// Paths the agent is allowed to read and write: the policy's write set
    /// and `/tmp`. The working directory is among them only when the policy
    /// says so.
    write_paths: Vec<PathBuf>,
    /// Explicit rules for bubblewrap's standard device nodes. No rule grants
    /// access to the `/dev` directory itself.
    device_rules: Vec<(PathBuf, u64)>,
}

impl LandlockRuleset {
    /// Builds a ruleset from a [`SandboxConfig`]: read everywhere, write the
    /// policy's write set and `/tmp`.
    pub fn new(config: &SandboxConfig) -> Self {
        let read_paths = vec![PathBuf::from(READ_EVERYWHERE)];
        let mut write_paths = config.write_paths.clone();
        let tmp = PathBuf::from("/tmp");
        if !write_paths.contains(&tmp) {
            write_paths.push(tmp);
        }
        let device_rules = SYSTEM_DEVICE_RULES
            .iter()
            .map(|(path, access)| (PathBuf::from(path), *access))
            .collect();
        Self {
            read_paths,
            write_paths,
            device_rules,
        }
    }

    /// Applies the Landlock ruleset to the calling thread.
    ///
    /// Detects the kernel's Landlock ABI version, creates the ruleset, adds
    /// per-path rules, and calls `landlock_restrict_self`.
    ///
    /// Returns [`LandlockStatus::Unavailable`] when the kernel predates
    /// Linux 5.13, logging a warning so operators know restrictions are off.
    pub fn apply(&self) -> Result<LandlockStatus, SandboxError> {
        let abi = detect_abi();

        if abi <= 0 {
            warn!("Landlock not supported by this kernel — filesystem restrictions disabled");
            return Ok(LandlockStatus::Unavailable);
        }

        debug!("Landlock ABI version {abi} detected");

        let (handled_flags, missing) = flags_for_abi(abi);

        // Set no-new-privs — required by the kernel before landlock_restrict_self.
        // Safety: prctl() is safe to call with these arguments.
        let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        if rc != 0 {
            return Err(SandboxError::SpawnFailed {
                message: format!(
                    "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
                    std::io::Error::last_os_error()
                ),
            });
        }

        // Create the ruleset fd.
        let attr = LandlockRulesetAttr {
            handled_access_fs: handled_flags,
        };
        let ruleset_fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &raw const attr,
                std::mem::size_of::<LandlockRulesetAttr>(),
                0u32,
            )
        };
        if ruleset_fd < 0 {
            return Err(SandboxError::SpawnFailed {
                message: format!(
                    "landlock_create_ruleset failed: {}",
                    std::io::Error::last_os_error()
                ),
            });
        }
        let ruleset_fd = ruleset_fd as i32;

        // Add rules for each read path.
        for path in &self.read_paths {
            add_path_rule(ruleset_fd, path, READ_ALLOWED & handled_flags);
        }

        // Add rules for each write path, capping to available ABI flags.
        // REFER is denied by default once handled (ABI 2), and a rename or
        // link across directories needs it on both; a writable tree where
        // `mv a/x b/x` fails is not a writable tree, so it is granted with the
        // write set wherever the kernel knows it.
        let write_flags = write_flags_for_abi(abi) & handled_flags;

        for path in &self.write_paths {
            add_path_rule(ruleset_fd, path, write_flags);
        }

        for (path, allowed_access) in &self.device_rules {
            add_path_rule(ruleset_fd, path, allowed_access & handled_flags);
        }

        // Apply the ruleset to this thread (and all future threads it spawns).
        let rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32) };

        // Safety: close() is always safe when given a valid fd.
        unsafe { libc::close(ruleset_fd) };

        if rc != 0 {
            return Err(SandboxError::SpawnFailed {
                message: format!(
                    "landlock_restrict_self failed: {}",
                    std::io::Error::last_os_error()
                ),
            });
        }

        if missing.is_empty() {
            debug!("Landlock restrictions applied (full ABI {abi})");
            Ok(LandlockStatus::Applied)
        } else {
            warn!(
                "Landlock restrictions partially applied (ABI {abi} < {MAX_KNOWN_ABI}): missing {:?}",
                missing
            );
            Ok(LandlockStatus::Partial {
                missing_features: missing,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Queries the kernel Landlock ABI version.
///
/// Returns the ABI version (>= 1 for Linux 5.13+), or <= 0 if unavailable.
fn detect_abi() -> i64 {
    // Passing NULL attr and size=0 with the VERSION flag returns the ABI version.
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<LandlockRulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    }
}

/// The rights a write path receives at `abi`: the baseline, cross-directory
/// rename and link from ABI 2, truncation from ABI 3.
fn write_flags_for_abi(abi: i64) -> u64 {
    let mut flags = WRITE_ALLOWED_BASE;
    if abi >= 2 {
        flags |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        flags |= ACCESS_FS_TRUNCATE;
    }
    flags
}

/// Returns (flags_to_use, missing_feature_names) for the given ABI version.
///
/// The flags represent all rights controlled by the ruleset. Features not
/// available in the detected ABI are reported as missing.
fn flags_for_abi(abi: i64) -> (u64, Vec<String>) {
    let mut missing = Vec::new();

    if abi < 2 {
        missing.push("ACCESS_FS_REFER (ABI 2, Linux 5.19)".into());
    }

    if abi < 3 {
        missing.push("ACCESS_FS_TRUNCATE (ABI 3, Linux 6.2)".into());
    }

    if abi < 5 {
        missing.push("ACCESS_FS_IOCTL_DEV (ABI 5, Linux 6.12)".into());
    }

    let flags = if abi >= 5 {
        ABI_V5_FLAGS
    } else if abi >= 3 {
        ABI_V3_FLAGS
    } else if abi >= 2 {
        ABI_V2_FLAGS
    } else {
        ABI_V1_FLAGS
    };

    (flags, missing)
}

/// Opens `path` with `O_PATH | O_CLOEXEC` and adds a Landlock path-beneath
/// rule to the given `ruleset_fd` with the specified `allowed_access`.
///
/// Failures are logged as warnings rather than propagated — a missing or
/// inaccessible path should not abort the entire sandboxing setup.
fn add_path_rule(ruleset_fd: i32, path: &std::path::Path, allowed_access: u64) {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        warn!("Landlock: skipping path with embedded NUL: {path:?}");
        return;
    };

    // Safety: open() with O_PATH is safe — it doesn't actually read file data.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        warn!(
            "Landlock: skipping {path:?} — open(O_PATH) failed: {}",
            std::io::Error::last_os_error()
        );
        return;
    }

    let attr = LandlockPathBeneathAttr {
        allowed_access,
        parent_fd: fd,
    };

    // Safety: landlock_add_rule() is safe when the fd and attr are valid.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &raw const attr,
            0u32,
        )
    };

    // Always close the path fd regardless of rule-add outcome.
    unsafe { libc::close(fd) };

    if rc != 0 {
        warn!(
            "Landlock: failed to add rule for {path:?}: {}",
            std::io::Error::last_os_error()
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SandboxConfig;

    // -----------------------------------------------------------------------
    // LandlockRuleset::new — the policy's shape
    // -----------------------------------------------------------------------

    #[test]
    fn new_reads_everywhere_and_nowhere_in_particular() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .masked_path("/home/u/.ssh")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert_eq!(ruleset.read_paths, vec![PathBuf::from("/")]);
    }

    #[test]
    fn new_writes_only_the_declared_set_and_tmp() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .write_path("/home/u/.cache")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert_eq!(
            ruleset.write_paths,
            vec![
                PathBuf::from("/tmp/agent"),
                PathBuf::from("/home/u/.cache"),
                PathBuf::from("/tmp"),
            ]
        );
    }

    #[test]
    fn a_working_directory_outside_the_write_set_is_not_writable() {
        let config = SandboxConfig::builder()
            .working_directory("/srv/read-only-checkout")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert_eq!(ruleset.write_paths, vec![PathBuf::from("/tmp")]);
    }

    #[test]
    fn new_grants_only_standard_device_nodes_with_needed_access() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert_eq!(
            ruleset.device_rules,
            vec![
                (PathBuf::from("/dev/full"), ACCESS_FS_WRITE_FILE),
                (
                    PathBuf::from("/dev/null"),
                    ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE,
                ),
                (PathBuf::from("/dev/random"), ACCESS_FS_READ_FILE),
                (
                    PathBuf::from("/dev/tty"),
                    ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE,
                ),
                (PathBuf::from("/dev/urandom"), ACCESS_FS_READ_FILE),
                (PathBuf::from("/dev/zero"), ACCESS_FS_READ_FILE),
            ],
            "device grants must remain restricted to each standard node's required file access"
        );
        assert!(
            ruleset
                .device_rules
                .iter()
                .all(|(path, _)| path != &PathBuf::from("/dev"))
        );
    }

    // -----------------------------------------------------------------------
    // flags_for_abi
    // -----------------------------------------------------------------------

    #[test]
    fn a_write_path_may_rename_across_directories_from_abi_2_and_truncate_from_abi_3() {
        assert_eq!(write_flags_for_abi(1) & ACCESS_FS_REFER, 0);
        assert_ne!(write_flags_for_abi(2) & ACCESS_FS_REFER, 0);
        assert_eq!(write_flags_for_abi(2) & ACCESS_FS_TRUNCATE, 0);
        assert_ne!(write_flags_for_abi(3) & ACCESS_FS_TRUNCATE, 0);
        assert_ne!(write_flags_for_abi(5) & ACCESS_FS_REFER, 0);
    }

    #[test]
    fn abi_1_flags_exclude_refer_and_truncate() {
        let (flags, missing) = flags_for_abi(1);
        assert_eq!(flags, ABI_V1_FLAGS);
        assert!(missing.iter().any(|m| m.contains("REFER")));
        assert!(missing.iter().any(|m| m.contains("TRUNCATE")));
    }

    #[test]
    fn abi_2_flags_include_refer_but_not_truncate() {
        let (flags, missing) = flags_for_abi(2);
        assert_eq!(flags, ABI_V2_FLAGS);
        assert!(flags & ACCESS_FS_REFER != 0);
        assert_eq!(flags & ACCESS_FS_TRUNCATE, 0);
        assert!(!missing.iter().any(|m| m.contains("REFER")));
        assert!(missing.iter().any(|m| m.contains("TRUNCATE")));
    }

    #[test]
    fn abi_3_flags_are_complete() {
        let (flags, missing) = flags_for_abi(3);
        assert_eq!(flags, ABI_V3_FLAGS);
        assert!(flags & ACCESS_FS_REFER != 0);
        assert!(flags & ACCESS_FS_TRUNCATE != 0);
        assert!(missing.iter().any(|m| m.contains("IOCTL_DEV")));
    }

    #[test]
    fn abi_5_flags_include_device_ioctl_restrictions() {
        let (flags, missing) = flags_for_abi(5);
        assert_eq!(flags, ABI_V5_FLAGS);
        assert!(flags & ACCESS_FS_IOCTL_DEV != 0);
        assert!(missing.is_empty());
    }

    #[test]
    fn future_abi_uses_max_known_flags() {
        // ABI > 5 should still use ABI_V5_FLAGS — we don't know future flags.
        let (flags, missing) = flags_for_abi(99);
        assert_eq!(flags, ABI_V5_FLAGS);
        assert!(missing.is_empty());
    }

    // -----------------------------------------------------------------------
    // LandlockStatus
    // -----------------------------------------------------------------------

    #[test]
    fn status_applied_is_not_unavailable() {
        assert_ne!(LandlockStatus::Applied, LandlockStatus::Unavailable);
    }

    #[test]
    fn status_partial_holds_missing_features() {
        let status = LandlockStatus::Partial {
            missing_features: vec!["ACCESS_FS_TRUNCATE".into()],
        };
        match status {
            LandlockStatus::Partial { missing_features } => {
                assert_eq!(missing_features.len(), 1);
                assert!(missing_features[0].contains("TRUNCATE"));
            }
            _ => panic!("expected Partial"),
        }
    }

    #[test]
    fn status_debug_includes_variant_name() {
        let debug = format!("{:?}", LandlockStatus::Applied);
        assert!(debug.contains("Applied"));

        let debug = format!("{:?}", LandlockStatus::Unavailable);
        assert!(debug.contains("Unavailable"));
    }

    // -----------------------------------------------------------------------
    // Access flag values — sanity checks
    // -----------------------------------------------------------------------

    #[test]
    fn access_flags_are_distinct_powers_of_two() {
        let flags = [
            ACCESS_FS_EXECUTE,
            ACCESS_FS_WRITE_FILE,
            ACCESS_FS_READ_FILE,
            ACCESS_FS_READ_DIR,
            ACCESS_FS_REMOVE_DIR,
            ACCESS_FS_REMOVE_FILE,
            ACCESS_FS_MAKE_CHAR,
            ACCESS_FS_MAKE_DIR,
            ACCESS_FS_MAKE_REG,
            ACCESS_FS_MAKE_SYM,
            ACCESS_FS_MAKE_SOCK,
            ACCESS_FS_MAKE_FIFO,
            ACCESS_FS_MAKE_BLOCK,
            ACCESS_FS_REFER,
            ACCESS_FS_TRUNCATE,
            ACCESS_FS_IOCTL_DEV,
        ];

        // Each must be a power of two.
        for f in flags {
            assert_eq!(f.count_ones(), 1, "flag {f:#x} is not a single bit");
        }

        // Each must be unique.
        let union_all: u64 = flags.iter().fold(0, |acc, &f| acc | f);
        assert_eq!(union_all.count_ones(), flags.len() as u32, "flags overlap");
    }

    #[test]
    fn abi_v1_flags_matches_expected_bitmask() {
        // 13 flags set in bits 0..=12.
        assert_eq!(ABI_V1_FLAGS, 0x1FFF);
    }

    #[test]
    fn abi_v2_flags_adds_one_bit() {
        assert_eq!(ABI_V2_FLAGS, ABI_V1_FLAGS | (1 << 13));
    }

    #[test]
    fn abi_v3_flags_adds_one_more_bit() {
        assert_eq!(ABI_V3_FLAGS, ABI_V2_FLAGS | (1 << 14));
    }

    #[test]
    fn abi_v5_flags_adds_device_ioctl_bit() {
        assert_eq!(ABI_V5_FLAGS, ABI_V3_FLAGS | (1 << 15));
    }
}
