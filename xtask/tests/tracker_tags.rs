use std::{env, fs, process::Command};

struct TestRepository(std::path::PathBuf);

impl TestRepository {
    fn new(name: &str) -> Self {
        let root = env::var_os("SWAGE_TEST_SCRATCH_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(env::temp_dir)
            .join(format!(
                "tracker-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for TestRepository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn tracker_tags_ignore_deleted_tracked_files() {
    let repository = TestRepository::new("deleted");
    let root = &repository.0;
    assert!(
        Command::new("git")
            .arg("init")
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join("tracked.rs"), "// useful explanation\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(root)
            .args(["add", "tracked.rs"])
            .status()
            .unwrap()
            .success()
    );
    fs::remove_file(root.join("tracked.rs")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(root)
        .arg("check-tracker-tags")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tracker_tags_ignore_developer_local_files() {
    let repository = TestRepository::new("tracker");
    let root = &repository.0;
    assert!(
        Command::new("git")
            .arg("init")
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join(".gitignore"), ".venv/\n.opencode/\n.motes/\n").unwrap();
    fs::write(root.join("tracked.rs"), "// useful explanation\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(root)
            .args(["add", ".gitignore", "tracked.rs"])
            .status()
            .unwrap()
            .success()
    );
    for directory in [".venv", ".opencode", ".motes"] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("local.ts"), "// [a1b2]\n").unwrap();
        fs::write(root.join(directory).join("binary.rs"), [0xff]).unwrap();
    }
    let scan = || {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .current_dir(root)
            .arg("check-tracker-tags")
            .output()
            .unwrap()
    };
    let output = scan();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(root.join("untracked.ts"), "// [a1b2]\n").unwrap();
    let output = scan();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("untracked.ts:1:4"));
    fs::remove_file(root.join("untracked.ts")).unwrap();
    fs::write(root.join("tracked.rs"), "// [8uce]\n").unwrap();
    let output = scan();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("tracked.rs:1:4"));
}

#[test]
fn tracker_tags_report_javascript_comments() {
    let repository = TestRepository::new("javascript");
    let root = &repository.0;
    assert!(
        Command::new("git")
            .arg("init")
            .arg(root)
            .status()
            .unwrap()
            .success()
    );
    for extension in ["js", "mjs", "cjs"] {
        fs::write(root.join(format!("source.{extension}")), "// [a1b2]\n").unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(root)
        .arg("check-tracker-tags")
        .output()
        .unwrap();
    assert!(!output.status.success(), "JavaScript comments were skipped");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for extension in ["js", "mjs", "cjs"] {
        assert!(
            stderr.contains(&format!("source.{extension}:1:4")),
            "{stderr}"
        );
    }
}
