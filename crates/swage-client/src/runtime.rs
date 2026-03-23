//! Runtime metadata for a running Swage gateway instance.
//!
//! The gateway writes a structured metadata file on startup containing its PID,
//! listening address, and start time. The CLI reads this to discover and manage
//! the running gateway. The file is removed during graceful shutdown; stale
//! files (from crashed gateways) are detected and cleaned up.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

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
    /// Prefers `$XDG_RUNTIME_DIR/swage/metadata.json`; falls back to
    /// `~/.swage/run/metadata.json` when the XDG variable is absent.
    pub fn metadata_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(xdg).join("swage").join("metadata.json")
        } else {
            // HOME is virtually always present; /tmp is an emergency fallback.
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home)
                .join(".swage")
                .join("run")
                .join("metadata.json")
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

    fn save_to(&self, path: &Path) -> io::Result<()> {
        // Create parent directories if they don't exist yet.
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    fn load_from(path: &Path) -> io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn remove_at(path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            // Missing file is not an error — removal is idempotent.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serialise env-touching tests to prevent variable pollution across parallel runs.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn metadata_path_falls_back_to_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: single-threaded within the lock.
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        let path = RuntimeMetadata::metadata_path();
        if let Some(val) = original_xdg {
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", val) };
        }
        assert!(
            path.to_str().unwrap().contains(".swage/run/metadata.json"),
            "home fallback path should contain .swage/run/metadata.json, got: {}",
            path.display()
        );
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
            assert!(!path.exists(), "metadata file should not exist after remove");
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
        let meta = RuntimeMetadata::new(
            std::process::id(),
            "http://127.0.0.1:3000".to_string(),
        );
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
