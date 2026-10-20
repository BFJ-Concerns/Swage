use std::{env, fs, process::Command};

#[test]
fn tracker_tags_ignore_deleted_tracked_files() {
    let root =
        std::path::PathBuf::from(env::var_os("SWAGE_HOME").unwrap()).join("deleted-repository");
    fs::create_dir(&root).unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join("tracked.rs"), "// useful explanation\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(&root)
            .args(["add", "tracked.rs"])
            .status()
            .unwrap()
            .success()
    );
    fs::remove_file(root.join("tracked.rs")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&root)
        .arg("check-tracker-tags")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tracker_tags_ignore_developer_local_files() {
    let root =
        std::path::PathBuf::from(env::var_os("SWAGE_HOME").unwrap()).join("tracker-repository");
    fs::create_dir(&root).unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    fs::write(root.join(".gitignore"), ".venv/\n.opencode/\n.motes/\n").unwrap();
    fs::write(root.join("tracked.rs"), "// useful explanation\n").unwrap();
    assert!(
        Command::new("git")
            .current_dir(&root)
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
            .current_dir(&root)
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
    fs::remove_dir_all(root).unwrap();
}
