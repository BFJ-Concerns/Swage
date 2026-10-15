//! Fetches the prebuilt V8 static library the `v8` crate links and verifies it
//! against the pin in `v8-archive.toml`, so the workspace never links an
//! archive nobody checked.
//!
//! The pin names a rusty_v8 release version and, per target, the release asset
//! and its sha256. The version must match the `v8` package Cargo.lock resolves;
//! a dependency upgrade that moves V8 fails here until the pin is updated on
//! purpose. The verified archive lands at `vendor/v8/librusty_v8.a.gz`, where
//! `.cargo/config.toml` points the v8 crate's build script, and is also left
//! in the cargo home cache the v8 crate reads (`$CARGO_HOME/.rusty_v8/`), so
//! a second worktree on the same machine copies rather than downloads.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sha2::{Digest, Sha256};

const PIN_FILE: &str = "v8-archive.toml";
const DESTINATION: &str = "vendor/v8/librusty_v8.a.gz";
const RELEASES: &str = "https://github.com/denoland/rusty_v8/releases/download";

type Result<T> = std::result::Result<T, String>;

struct Pin {
    version: String,
    asset: String,
    sha256: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String> {
    let root = workspace_root()?;
    let target = build_target()?;
    let pin = read_pin(&root.join(PIN_FILE), &target)?;
    check_lock_version(&root.join("Cargo.lock"), &pin.version)?;

    let destination = root.join(DESTINATION);
    if destination.is_file() && sha256_of(&destination)? == pin.sha256 {
        return Ok(format!(
            "V8 {} archive for {target} present and verified at {DESTINATION}",
            pin.version
        ));
    }

    let url = format!("{RELEASES}/v{}/{}", pin.version, pin.asset);
    let cached = cargo_home().map(|home| home.join(".rusty_v8").join(escape(&url)));
    let staged = destination.with_extension("gz.partial");
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }

    let obtained = match &cached {
        Some(cached) if cached.is_file() && sha256_of(cached)? == pin.sha256 => {
            fs::copy(cached, &staged)
                .map_err(|error| format!("copying {}: {error}", cached.display()))?;
            format!("copied from {}", cached.display())
        }
        _ => {
            download(&url, &staged)?;
            format!("downloaded from {url}")
        }
    };

    let actual = sha256_of(&staged)?;
    if actual != pin.sha256 {
        let _ = fs::remove_file(&staged);
        return Err(format!(
            "V8 archive {} does not match {PIN_FILE}: the pin records sha256 {} but the archive \
             {obtained} is {actual}. Either the release asset changed under its version — \
             check it before trusting it — or the pin is stale and needs updating on purpose.",
            pin.asset, pin.sha256
        ));
    }
    fs::rename(&staged, &destination)
        .map_err(|error| format!("placing {}: {error}", destination.display()))?;

    if let Some(cached) = &cached
        && !cached.is_file()
        && let Some(parent) = cached.parent()
        && fs::create_dir_all(parent).is_ok()
    {
        // Best effort: the cache saves the next worktree a download, nothing
        // depends on it.
        let _ = fs::copy(&destination, cached);
    }

    Ok(format!(
        "V8 {} archive for {target} {obtained}, verified (sha256 {}) and placed at {DESTINATION}",
        pin.version, pin.sha256
    ))
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("CARGO_MANIFEST_DIR is unset; run this through cargo")?,
    );
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "the fetch-v8 package sits at the workspace root".to_owned())
}

/// The target the workspace builds for: an explicit `CARGO_BUILD_TARGET`, or
/// the host rustc reports.
fn build_target() -> Result<String> {
    if let Some(target) = std::env::var_os("CARGO_BUILD_TARGET") {
        return Ok(target.to_string_lossy().into_owned());
    }
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|error| format!("running rustc -vV: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|host| host.trim().to_owned())
        .ok_or_else(|| "rustc -vV reported no host target".to_owned())
}

fn read_pin(path: &Path, target: &str) -> Result<Pin> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|error| format!("parsing {PIN_FILE}: {error}"))?;
    let version = table
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or("{PIN_FILE} has no `version`")?
        .to_owned();
    let archive = table
        .get("archive")
        .and_then(toml::Value::as_table)
        .and_then(|archives| archives.get(target))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| {
            format!(
                "{PIN_FILE} pins no archive for target {target}: add an [archive.{target}] table \
                 with the release asset's name and sha256"
            )
        })?;
    let field = |name: &str| {
        archive
            .get(name)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{PIN_FILE}: [archive.{target}] has no `{name}`"))
    };
    Ok(Pin {
        version,
        asset: field("name")?,
        sha256: field("sha256")?.to_ascii_lowercase(),
    })
}

/// The `v8` package version Cargo.lock resolves must be the pinned one, so
/// the archive and the bindings never drift apart.
fn check_lock_version(lock: &Path, pinned: &str) -> Result<()> {
    let text =
        fs::read_to_string(lock).map_err(|error| format!("reading {}: {error}", lock.display()))?;
    let resolved = text
        .split("[[package]]")
        .find_map(|block| {
            let mut lines = block.lines().map(str::trim);
            lines.find(|line| *line == r#"name = "v8""#)?;
            lines.find_map(|line| line.strip_prefix("version = "))
        })
        .map(|version| version.trim_matches('"').to_owned())
        .ok_or("Cargo.lock resolves no `v8` package")?;
    if resolved == pinned {
        Ok(())
    } else {
        Err(format!(
            "Cargo.lock resolves v8 {resolved} but {PIN_FILE} pins {pinned}: update the pin to the \
             archive of the new release (its name and sha256) before building"
        ))
    }
}

fn sha256_of(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

/// The v8 crate's own cache key: the URL with every non-alphanumeric
/// character replaced by an underscore.
fn escape(url: &str) -> String {
    url.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn download(url: &str, to: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(to)
        .arg(url)
        .status()
        .map_err(|error| format!("running curl: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        let _ = fs::remove_file(to);
        Err(format!("downloading {url} failed ({status})"))
    }
}
