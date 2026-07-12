//! Landlock LSM filesystem access control.
//!
//! Kernel-enforced per-path read/write restrictions as a complement to
//! bubblewrap's namespace isolation. Gracefully degrades on older kernels.
//!
//! This module runs inside the child process (via `pre_exec` or early in agent
//! startup) — not in the host. Applying restrictions here prevents the agent
//! from accessing any filesystem path not explicitly whitelisted, even if the
//! bubblewrap bind-mounts make those paths visible.

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
const ACCESS_FS_MAKE_SYM: u64 = 1 << 9;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 10;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 11;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 12;
const ACCESS_FS_MAKE_IPC: u64 = 1 << 13;

// ABI 2 (Linux 5.19+) — adds cross-directory rename support.
const ACCESS_FS_REFER: u64 = 1 << 14;

// ABI 3 (Linux 6.2+) — adds truncate support.
const ACCESS_FS_TRUNCATE: u64 = 1 << 15;

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
    | ACCESS_FS_MAKE_BLOCK
    | ACCESS_FS_MAKE_IPC;

/// All access flags defined in ABI 2 (Linux 5.19).
const ABI_V2_FLAGS: u64 = ABI_V1_FLAGS | ACCESS_FS_REFER;

/// All access flags defined in ABI 3 (Linux 6.2).
const ABI_V3_FLAGS: u64 = ABI_V2_FLAGS | ACCESS_FS_TRUNCATE;

/// The highest ABI version this implementation is aware of.
const MAX_KNOWN_ABI: i64 = 3;

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

/// Read/execute paths required for sandboxed process tools to start ordinary
/// system commands such as `/bin/sh`. Writes remain limited to the configured
/// workspace paths and `/tmp`.
const SYSTEM_EXECUTION_READ_PATHS: &[&str] = &[
    "/bin",
    "/lib",
    "/lib64",
    "/usr/bin",
    "/usr/lib",
    "/usr/lib64",
    "/etc/ld.so.cache",
];

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
    /// Paths the agent is allowed to read (and execute from).
    read_paths: Vec<PathBuf>,
    /// Paths the agent is allowed to read and write.
    ///
    /// Always includes the working directory and `/tmp` in addition to
    /// whatever was specified in [`SandboxConfig::landlock_write_paths`].
    write_paths: Vec<PathBuf>,
}

impl LandlockRuleset {
    /// Builds a ruleset from a [`SandboxConfig`].
    ///
    /// Automatically adds the system binary/library paths needed to execute
    /// commands, plus `working_directory` and `/tmp` to the write-allowed set
    /// so the agent can function normally in its workspace.
    pub fn new(config: &SandboxConfig) -> Self {
        let mut read_paths: Vec<PathBuf> = SYSTEM_EXECUTION_READ_PATHS
            .iter()
            .map(PathBuf::from)
            .collect();
        read_paths.extend(config.landlock_read_paths.iter().cloned());

        // Always allow writes to the working directory and /tmp; append any
        // explicit write paths from the config on top.
        let mut write_paths = vec![config.working_directory.clone(), PathBuf::from("/tmp")];
        write_paths.extend(config.landlock_write_paths.iter().cloned());

        Self {
            read_paths,
            write_paths,
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
        let write_flags = if abi >= 3 {
            (WRITE_ALLOWED_BASE | ACCESS_FS_TRUNCATE) & handled_flags
        } else {
            WRITE_ALLOWED_BASE & handled_flags
        };

        for path in &self.write_paths {
            add_path_rule(ruleset_fd, path, write_flags);
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

    let flags = if abi >= 3 {
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
    // LandlockRuleset::new — path collection
    // -----------------------------------------------------------------------

    #[test]
    fn new_collects_read_paths_from_config() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .landlock_read_path("/home/user/space")
            .landlock_read_path("/usr/lib")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert!(
            ruleset
                .read_paths
                .contains(&PathBuf::from("/home/user/space"))
        );
        assert!(ruleset.read_paths.contains(&PathBuf::from("/usr/lib")));
    }

    #[test]
    fn new_auto_includes_system_execution_read_paths() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        for path in SYSTEM_EXECUTION_READ_PATHS {
            assert!(
                ruleset.read_paths.contains(&PathBuf::from(path)),
                "system execution path {path} must be readable; got: {:?}",
                ruleset.read_paths
            );
        }
    }

    #[test]
    fn new_auto_includes_working_directory_in_write_paths() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert!(
            ruleset
                .write_paths
                .contains(&PathBuf::from("/tmp/agent-workspace")),
            "working_directory must be in write_paths; got: {:?}",
            ruleset.write_paths
        );
    }

    #[test]
    fn new_auto_includes_tmp_in_write_paths() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert!(
            ruleset.write_paths.contains(&PathBuf::from("/tmp")),
            "/tmp must be automatically included in write_paths; got: {:?}",
            ruleset.write_paths
        );
    }

    #[test]
    fn new_appends_explicit_write_paths_after_defaults() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .landlock_write_path("/mnt/output")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        // Both auto-included defaults and explicit write paths must be present.
        assert!(ruleset.write_paths.contains(&PathBuf::from("/tmp/agent")));
        assert!(ruleset.write_paths.contains(&PathBuf::from("/tmp")));
        assert!(ruleset.write_paths.contains(&PathBuf::from("/mnt/output")));
    }

    #[test]
    fn new_with_no_config_paths_has_default_read_paths_and_two_write_paths() {
        // The default read paths support command execution; writes remain
        // limited to the workspace and /tmp.
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        let ruleset = LandlockRuleset::new(&config);

        assert_eq!(ruleset.read_paths.len(), SYSTEM_EXECUTION_READ_PATHS.len());
        assert_eq!(ruleset.write_paths.len(), 2);
    }

    // -----------------------------------------------------------------------
    // flags_for_abi
    // -----------------------------------------------------------------------

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
        assert!(flags & ACCESS_FS_TRUNCATE == 0);
        assert!(!missing.iter().any(|m| m.contains("REFER")));
        assert!(missing.iter().any(|m| m.contains("TRUNCATE")));
    }

    #[test]
    fn abi_3_flags_are_complete() {
        let (flags, missing) = flags_for_abi(3);
        assert_eq!(flags, ABI_V3_FLAGS);
        assert!(flags & ACCESS_FS_REFER != 0);
        assert!(flags & ACCESS_FS_TRUNCATE != 0);
        assert!(missing.is_empty());
    }

    #[test]
    fn future_abi_uses_max_known_flags() {
        // ABI > 3 should still use ABI_V3_FLAGS — we don't know future flags.
        let (flags, missing) = flags_for_abi(99);
        assert_eq!(flags, ABI_V3_FLAGS);
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
            ACCESS_FS_MAKE_IPC,
            ACCESS_FS_REFER,
            ACCESS_FS_TRUNCATE,
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
        // 14 flags set in bits 0..=13.
        assert_eq!(ABI_V1_FLAGS, 0x3FFF);
    }

    #[test]
    fn abi_v2_flags_adds_one_bit() {
        assert_eq!(ABI_V2_FLAGS, ABI_V1_FLAGS | (1 << 14));
    }

    #[test]
    fn abi_v3_flags_adds_one_more_bit() {
        assert_eq!(ABI_V3_FLAGS, ABI_V2_FLAGS | (1 << 15));
    }
}
