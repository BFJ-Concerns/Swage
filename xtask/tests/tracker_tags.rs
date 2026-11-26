use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

struct TestRepository(PathBuf);

impl TestRepository {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "swage-tracker-tags-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir(&root).unwrap();
        let repo = Self(root);
        repo.git(&["init", "--quiet"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, path: &str, contents: &str) {
        fs::write(self.0.join(path), contents).unwrap();
    }

    fn scan(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .current_dir(&self.0)
            .arg("check-tracker-tags")
            .output()
            .unwrap()
    }
}

impl Drop for TestRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn catches_tags_in_comments_and_strings_across_tracked_file_types() {
    let repo = TestRepository::new();
    repo.write(
        "source.rs",
        &format!("let text = \"[{}]\"; // [{}] [{}]", "a1b2", "8uce", "utf8"),
    );
    repo.write("notes.md", &format!("<!-- [{}] -->", "v7qg"));
    repo.git(&["add", "--", "source.rs", "notes.md"]);

    let output = repo.scan();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["source.rs", "notes.md", "a1b2", "8uce", "v7qg"] {
        assert!(stderr.contains(expected), "{stderr}");
    }
    assert!(!stderr.contains("utf8"), "{stderr}");
}

#[test]
fn skips_local_files_deleted_paths_and_known_non_tags() {
    let repo = TestRepository::new();
    repo.write(".gitignore", "/ignored.rs\n");
    let tag = format!("// [{}]\n", "a1b2");
    repo.write("ignored.rs", &tag);
    repo.write("untracked.rs", &tag);
    repo.write("deleted.rs", &tag);
    repo.write(
        "ordinary.txt",
        &format!(
            "[main] [1234] [A1b2] [a1b23] [{}] [{}] [{}]",
            "0xff", "89ab", "utf8"
        ),
    );
    repo.git(&["add", "--", ".gitignore", "deleted.rs", "ordinary.txt"]);
    repo.git(&["add", "--force", "--", "ignored.rs"]);
    fs::remove_file(repo.0.join("deleted.rs")).unwrap();

    let output = repo.scan();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
