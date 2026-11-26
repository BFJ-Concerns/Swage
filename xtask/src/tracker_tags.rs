use std::{collections::HashSet, process::Command};

use crate::Result;

const ALLOWED_WORDS: &[&[u8]] = &[
    b"0xff", // Rust byte literal in arrays
    b"89ab", // UUID variant character class
    b"utf8", // Encoding name
    // Existing syntax and labels in tracked files.
    b"char", b"conv", b"from", b"held", b"high", b"hitl", b"hold", b"host", b"href", b"json",
    b"land", b"link", b"main", b"name", b"null", b"open", b"page", b"path", b"plan", b"role",
    b"root", b"rule", b"test", b"text", b"tool", b"unit", b"user", b"wght", b"yyyy",
];

pub(crate) fn check_tracker_tags() -> Result<()> {
    let ignored = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--ignored",
            "--exclude-per-directory=.gitignore",
            "-z",
        ])
        .output()?;
    if !ignored.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&ignored.stderr)
        )
        .into());
    }
    let ignored: HashSet<_> = ignored.stdout.split(|&byte| byte == 0).collect();

    let matches = Command::new("git")
        .env("LC_ALL", "C")
        .args([
            "grep",
            "--full-name",
            "--no-color",
            "--no-column",
            "--no-textconv",
            "-nzoIE",
            "-e",
            r"\[[a-z0-9]{4}\]",
            "--",
        ])
        .output()?;
    if !matches.status.success() && matches.status.code() != Some(1) {
        return Err(format!(
            "git grep failed: {}",
            String::from_utf8_lossy(&matches.stderr)
        )
        .into());
    }

    let mut violations = Vec::new();
    // With -z and -o, Git writes one path\0line\0match\n record per match.
    let mut records = matches.stdout.as_slice();
    while !records.is_empty() {
        let path = take_field(&mut records, 0)?;
        let line = take_field(&mut records, 0)?;
        let matched = take_field(&mut records, b'\n')?;

        let word = &matched[1..matched.len() - 1];
        if word.iter().any(u8::is_ascii_lowercase)
            && !ALLOWED_WORDS.contains(&word)
            && !ignored.contains(path)
        {
            violations.push(format!(
                "{}:{} {}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(line),
                String::from_utf8_lossy(matched),
            ));
        }
    }

    if violations.is_empty() {
        println!("==> Tracked text files contain no tracker tags");
        Ok(())
    } else {
        Err(format!(
            "tracker tags found in tracked text files:\n  {}\nRemove the bracketed tracker tag and keep any useful explanation as prose.",
            violations.join("\n  ")
        ).into())
    }
}

fn take_field<'a>(records: &mut &'a [u8], separator: u8) -> Result<&'a [u8]> {
    let mut fields = records.splitn(2, |&byte| byte == separator);
    let field = fields.next().unwrap_or_default();
    *records = fields.next().ok_or("incomplete git grep record")?;
    Ok(field)
}
