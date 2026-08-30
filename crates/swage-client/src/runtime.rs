//! Runtime metadata for a running Swage gateway instance.
//!
//! The gateway writes a structured metadata file on startup containing its PID,
//! listening address, and start time. The CLI reads this to discover and manage
//! the running gateway. The file is removed during graceful shutdown; stale
//! files (from crashed gateways) are detected and cleaned up.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static METADATA_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Metadata written by the gateway on startup, read by the CLI for discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeMetadata {
    /// OS process ID of the running gateway.
    pub pid: u32,
    /// Address the gateway is listening on (e.g. `http://127.0.0.1:3000`).
    pub address: String,
    /// UTC timestamp from when the gateway started.
    pub started_at: DateTime<Utc>,
}

impl RuntimeMetadata {
    /// Constructs a new metadata record with the current timestamp.
    pub fn new(pid: u32, address: String) -> Self {
        Self {
            pid,
            address,
            started_at: Utc::now(),
        }
    }

    /// Resolves the path to the metadata file.
    ///
    /// Uses `$SWAGE_HOME/run/metadata.json` when an explicit data root is set,
    /// otherwise prefers `$XDG_RUNTIME_DIR/swage/metadata.json` and falls back
    /// to `~/.swage/run/metadata.json`.
    pub fn metadata_path() -> PathBuf {
        let explicit_data_root = std::env::var("SWAGE_HOME").ok().map(PathBuf::from);
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from);
        Self::metadata_path_for(
            explicit_data_root.as_deref(),
            runtime_dir.as_deref(),
            &effective_swage_root_path(),
        )
    }

    /// Resolves runtime metadata from an already-resolved process environment.
    pub fn metadata_path_for(
        explicit_data_root: Option<&Path>,
        runtime_dir: Option<&Path>,
        data_root: &Path,
    ) -> PathBuf {
        if let Some(data_root) = explicit_data_root {
            data_root.join("run").join("metadata.json")
        } else if let Some(runtime_dir) = runtime_dir {
            runtime_dir.join("swage").join("metadata.json")
        } else {
            data_root.join("run").join("metadata.json")
        }
    }

    /// Serialises this metadata to JSON and writes it to the metadata file.
    ///
    /// Creates parent directories if they do not exist.
    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::metadata_path())
    }

    /// Reads and deserialises metadata from the metadata file.
    pub fn load() -> io::Result<Self> {
        Self::load_from(&Self::metadata_path())
    }

    /// Deletes the metadata file.
    ///
    /// Returns `Ok(())` if the file did not exist — removes are idempotent.
    pub fn remove() -> io::Result<()> {
        Self::remove_at(&Self::metadata_path())
    }

    /// Returns `true` if the recorded PID is no longer running.
    ///
    /// On Linux, checks `/proc/{pid}` for the process directory. Returns
    /// `false` (not stale) on platforms where the check is unavailable, to
    /// avoid false positives.
    pub fn is_stale(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            !Path::new(&format!("/proc/{}", self.pid)).exists()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = self.pid;
            false
        }
    }

    // --- Path-parameterised helpers (enable test isolation without env mutation) ---

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        // Create parent directories if they don't exist yet.
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let file_name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata path has no file name",
            )
        })?;
        let temporary = path.with_file_name(format!(
            ".{}-{}-{}.tmp",
            file_name.to_string_lossy(),
            std::process::id(),
            METADATA_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        let write_result = (|| -> io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        write_result
    }

    pub fn load_from(path: &Path) -> io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn remove_at(path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            // Missing file is not an error — removal is idempotent.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

fn effective_swage_root_path() -> PathBuf {
    if let Ok(swage_home) = std::env::var("SWAGE_HOME") {
        PathBuf::from(swage_home)
    } else {
        // HOME is virtually always present; /tmp is an emergency fallback.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".swage")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;
    // Shared crate-wide lock — prevents races with http.rs tests that also
    // manipulate XDG_RUNTIME_DIR during parallel test execution.
    use crate::test_helpers::ENV_LOCK;

    /// Run `f` with `XDG_RUNTIME_DIR` pointing at a temporary directory,
    /// cleaning up the variable afterwards regardless of panics.
    fn with_temp_xdg<F: FnOnce(&TempDir)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        // SAFETY: single-threaded within the lock; no concurrent env reads.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn new_sets_fields_and_records_current_time() {
        let before = Utc::now();
        let meta = RuntimeMetadata::new(42, "http://127.0.0.1:3000".to_string());
        let after = Utc::now();
        assert_eq!(meta.pid, 42);
        assert_eq!(meta.address, "http://127.0.0.1:3000");
        assert!(meta.started_at >= before && meta.started_at <= after);
    }

    #[test]
    fn metadata_path_uses_xdg_runtime_dir() {
        with_temp_xdg(|dir| {
            let path = RuntimeMetadata::metadata_path();
            assert_eq!(path, dir.path().join("swage").join("metadata.json"));
        });
    }

    #[test]
    fn explicit_swage_root_owns_its_runtime_metadata() {
        let _guard = ENV_LOCK.lock().unwrap();
        let root = TempDir::new().unwrap();
        let runtime = TempDir::new().unwrap();
        let original_home = std::env::var("SWAGE_HOME").ok();
        let original_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: all environment mutation in this crate is serialised by ENV_LOCK.
        unsafe {
            std::env::set_var("SWAGE_HOME", root.path());
            std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
        }

        let path = RuntimeMetadata::metadata_path();

        match original_home {
            Some(value) => unsafe { std::env::set_var("SWAGE_HOME", value) },
            None => unsafe { std::env::remove_var("SWAGE_HOME") },
        }
        match original_runtime {
            Some(value) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", value) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        assert_eq!(path, root.path().join("run").join("metadata.json"));
    }

    #[test]
    fn metadata_path_falls_back_to_swage_root() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        let original_swage_home = std::env::var("SWAGE_HOME").ok();
        let root = TempDir::new().unwrap();
        // SAFETY: single-threaded within the lock.
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        unsafe { std::env::set_var("SWAGE_HOME", root.path()) };
        let path = RuntimeMetadata::metadata_path();
        if let Some(val) = original_xdg {
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", val) };
        }
        if let Some(val) = original_swage_home {
            unsafe { std::env::set_var("SWAGE_HOME", val) };
        } else {
            unsafe { std::env::remove_var("SWAGE_HOME") };
        }
        assert_eq!(path, root.path().join("run").join("metadata.json"));
    }

    #[test]
    fn save_creates_file_and_directory() {
        with_temp_xdg(|_dir| {
            let meta = RuntimeMetadata::new(1234, "http://127.0.0.1:3000".to_string());
            meta.save().unwrap();
            let path = RuntimeMetadata::metadata_path();
            assert!(path.exists(), "metadata file should exist after save");
        });
    }

    #[test]
    fn round_trip_save_and_load() {
        with_temp_xdg(|_dir| {
            let original = RuntimeMetadata::new(5678, "http://127.0.0.1:9090".to_string());
            original.save().unwrap();
            let loaded = RuntimeMetadata::load().unwrap();
            assert_eq!(loaded.pid, original.pid);
            assert_eq!(loaded.address, original.address);
            // Chrono preserves sub-second precision through serialisation, so
            // we can compare the full timestamp rather than just seconds.
            assert_eq!(loaded.started_at, original.started_at);
        });
    }

    #[test]
    fn remove_deletes_file() {
        with_temp_xdg(|_dir| {
            let meta = RuntimeMetadata::new(999, "http://127.0.0.1:3000".to_string());
            meta.save().unwrap();
            RuntimeMetadata::remove().unwrap();
            let path = RuntimeMetadata::metadata_path();
            assert!(
                !path.exists(),
                "metadata file should not exist after remove"
            );
        });
    }

    #[test]
    fn remove_is_idempotent_when_file_absent() {
        with_temp_xdg(|_dir| {
            // No file written — remove should still return Ok.
            RuntimeMetadata::remove().expect("remove on absent file should succeed");
        });
    }

    // --- Path-parameterised helper tests (no env mutation required) ---

    #[test]
    fn save_to_creates_parent_directory() {
        let tmp = TempDir::new().unwrap();
        // Deeply nested — none of these directories exist yet.
        let path = tmp.path().join("nested").join("dir").join("metadata.json");
        let meta = RuntimeMetadata::new(std::process::id(), "http://127.0.0.1:3000".to_string());

        meta.save_to(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn round_trip_via_path_helpers() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("metadata.json");
        let meta = RuntimeMetadata::new(std::process::id(), "http://127.0.0.1:3000".to_string());

        meta.save_to(&path).unwrap();
        let loaded = RuntimeMetadata::load_from(&path).unwrap();

        assert_eq!(loaded.pid, meta.pid);
        assert_eq!(loaded.address, meta.address);
        assert_eq!(loaded.started_at, meta.started_at);
    }

    #[test]
    fn save_to_never_exposes_partial_metadata_to_contending_readers() {
        const READERS: usize = 4;
        const READS_PER_READER: usize = 300;

        let tmp = TempDir::new().unwrap();
        let path = Arc::new(tmp.path().join("metadata.json"));
        RuntimeMetadata::new(1, "http://127.0.0.1:3000".to_string())
            .save_to(path.as_ref())
            .unwrap();

        let start = Arc::new(Barrier::new(READERS + 1));
        std::thread::scope(|scope| {
            for _ in 0..READERS {
                let path = Arc::clone(&path);
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..READS_PER_READER {
                        let metadata = RuntimeMetadata::load_from(path.as_ref())
                            .expect("readers must see either complete metadata version");
                        assert!(metadata.address.starts_with("http://127.0.0.1:"));
                    }
                });
            }

            start.wait();
            for port in 3001..=3300 {
                RuntimeMetadata::new(1, format!("http://127.0.0.1:{port}"))
                    .save_to(path.as_ref())
                    .unwrap();
            }
        });
    }

    #[test]
    fn remove_at_deletes_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("metadata.json");
        let meta = RuntimeMetadata::new(std::process::id(), "http://127.0.0.1:3000".to_string());

        meta.save_to(&path).unwrap();
        assert!(path.exists());
        RuntimeMetadata::remove_at(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn remove_at_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        // No file at this path — remove_at should still return Ok.
        RuntimeMetadata::remove_at(&path).expect("remove_at on absent file should succeed");
    }

    #[test]
    fn load_from_missing_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        assert!(RuntimeMetadata::load_from(&path).is_err());
    }

    #[test]
    fn load_from_invalid_json_returns_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("metadata.json");
        std::fs::write(&path, b"not valid json").unwrap();
        assert!(RuntimeMetadata::load_from(&path).is_err());
    }

    #[test]
    fn is_stale_returns_false_for_current_process() {
        let meta = RuntimeMetadata::new(std::process::id(), "http://127.0.0.1:3000".to_string());
        assert!(!meta.is_stale(), "current process should not be stale");
    }

    #[test]
    fn is_stale_returns_true_for_nonexistent_pid() {
        // 4_194_304 is the Linux default max_pid; this entry is never assigned
        // to a real process under normal conditions.
        let meta = RuntimeMetadata::new(4_194_304, "http://127.0.0.1:3000".to_string());
        #[cfg(target_os = "linux")]
        assert!(meta.is_stale(), "non-existent PID should be stale");
        #[cfg(not(target_os = "linux"))]
        {
            // is_stale() always returns false on non-Linux — just consume the binding.
            let _ = meta;
        }
    }
}
