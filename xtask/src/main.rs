use std::{
    env,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    thread,
    time::Duration,
};

use sha2::{Digest, Sha256};

const APP_PACKAGE: &str = "swage-gateway";
const BINARY_NAME: &str = "swage";
const FRONTEND_DIR: &str = "frontend";
const DESKTOP_DIR: &str = "desktop";
const FRONTEND_DIST_INDEX: &str = "frontend/dist/index.html";
const FRONTEND_DEPS_STAMP: &str = "target/xtask/frontend-deps.sha256";
const FRONTEND_RELEASE_STAMP: &str = "target/xtask/frontend-release.sha256";
const DESKTOP_DEPS_STAMP: &str = "target/xtask/desktop-deps.sha256";
const TARGET_MAX_SIZE_MB_ARG: &str = "50000";
const TARGET_MAX_SIZE_BYTES: u64 = 50_000 * 1024 * 1024;
const DEBUG_INCREMENTAL_DIR: &str = "target/debug/incremental";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("setup") => setup(),
        Some("build-dev") => {
            let options = BuildDevOptions::parse(args)?;
            build_dev(&options)
        }
        Some("dev") => {
            let options = DevOptions::parse(args)?;
            dev(&options)
        }
        Some("fmt") => fmt(),
        Some("fmt-check") => fmt_check(),
        Some("check") => check(),
        Some("clippy") => clippy(),
        Some("test") => test(),
        Some("test-lib") => test_lib(args),
        Some("test-integration") => test_integration(args),
        Some("ci") => ci(),
        Some("sweep") => run_sweep(),
        Some("release" | "build") => {
            let options = ReleaseOptions::parse(args)?;
            release(&options)
        }
        Some("install") => {
            let options = InstallOptions::parse(args)?;
            install(&options)
        }
        Some("-h" | "--help") | None => {
            print_help();
            Ok(())
        }
        Some(command) => Err(format!("unknown xtask command: {command}").into()),
    }
}

#[derive(Default)]
struct BuildDevOptions {
    embed_frontend: bool,
}

impl BuildDevOptions {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self::default();
        for arg in args {
            match arg.as_str() {
                "--embed-frontend" => options.embed_frontend = true,
                "-h" | "--help" => {
                    println!("Usage: cargo run -p xtask -- build-dev [--embed-frontend]");
                    std::process::exit(0);
                }
                _ => return Err(format!("unknown build-dev option: {arg}").into()),
            }
        }
        Ok(options)
    }
}

struct DevOptions {
    gateway_port: String,
    vite_host: String,
    vite_port: String,
}

impl DevOptions {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self {
            gateway_port: "3000".to_string(),
            vite_host: "0.0.0.0".to_string(),
            vite_port: "5173".to_string(),
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--gateway-port" => {
                    options.gateway_port =
                        args.next().ok_or("missing value after --gateway-port")?;
                }
                "--vite-host" => {
                    options.vite_host = args.next().ok_or("missing value after --vite-host")?;
                }
                "--vite-port" => {
                    options.vite_port = args.next().ok_or("missing value after --vite-port")?;
                }
                "-h" | "--help" => {
                    println!(
                        "Usage: cargo run -p xtask -- dev [--gateway-port PORT] [--vite-host HOST] [--vite-port PORT]"
                    );
                    std::process::exit(0);
                }
                _ if arg.starts_with("--gateway-port=") => {
                    options.gateway_port = option_value(&arg).to_string();
                }
                _ if arg.starts_with("--vite-host=") => {
                    options.vite_host = option_value(&arg).to_string();
                }
                _ if arg.starts_with("--vite-port=") => {
                    options.vite_port = option_value(&arg).to_string();
                }
                _ => return Err(format!("unknown dev option: {arg}").into()),
            }
        }

        Ok(options)
    }
}

#[derive(Default)]
struct ReleaseOptions {
    force_frontend: bool,
}

impl ReleaseOptions {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self::default();
        for arg in args {
            match arg.as_str() {
                "--force-frontend" => options.force_frontend = true,
                "-h" | "--help" => {
                    println!("Usage: cargo run -p xtask -- release [--force-frontend]");
                    std::process::exit(0);
                }
                _ => return Err(format!("unknown release option: {arg}").into()),
            }
        }
        Ok(options)
    }
}

struct InstallOptions {
    prefix: PathBuf,
    skip_frontend: bool,
    lan_mode: bool,
}

impl InstallOptions {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self {
            prefix: home_dir()?.join(".local/bin"),
            skip_frontend: false,
            lan_mode: false,
        };

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--prefix" => {
                    options.prefix =
                        PathBuf::from(args.next().ok_or("missing value after --prefix")?);
                }
                "--skip-frontend" => options.skip_frontend = true,
                "--lan" => options.lan_mode = true,
                "-h" | "--help" => {
                    println!(
                        "Usage: cargo run -p xtask -- install [--prefix DIR] [--skip-frontend] [--lan]"
                    );
                    std::process::exit(0);
                }
                _ if arg.starts_with("--prefix=") => {
                    options.prefix = PathBuf::from(option_value(&arg));
                }
                _ => return Err(format!("unknown install option: {arg}").into()),
            }
        }

        Ok(options)
    }
}

fn setup() -> Result<()> {
    require_command("cargo")?;
    require_command("npm")?;
    require_command("node")?;
    install_frontend_deps_if_needed()?;
    Ok(())
}

fn build_dev(options: &BuildDevOptions) -> Result<()> {
    setup()?;

    if options.embed_frontend {
        build_frontend_release_if_needed(false)?;
    }

    run_command(
        "Building swage debug binary",
        command("cargo", ["build", "-p", APP_PACKAGE, "--bin", BINARY_NAME]),
    )?;
    ensure_file(
        "target/debug/swage",
        "debug binary was not produced: target/debug/swage",
    )?;
    println!("==> Built target/debug/{BINARY_NAME}");
    Ok(())
}

fn dev(options: &DevOptions) -> Result<()> {
    build_dev(&BuildDevOptions::default())?;
    warn_if_vite_proxy_mismatch(&options.gateway_port)?;

    println!(
        "==> Starting Swage gateway on port {} (debug binary)",
        options.gateway_port
    );
    let mut gateway = Command::new(format!("target/debug/{BINARY_NAME}"))
        .env("SWAGE_DEV_FRONTEND_PORT", &options.vite_port)
        .arg("gateway")
        .arg("start")
        .arg("--foreground")
        .arg("--port")
        .arg(&options.gateway_port)
        .spawn()?;

    println!(
        "==> Starting Vite dev server on {}:{}",
        options.vite_host, options.vite_port
    );
    let mut vite = npm_command(FRONTEND_DIR, ["run", "dev", "--"])
        .arg("--host")
        .arg(&options.vite_host)
        .arg("--port")
        .arg(&options.vite_port)
        .arg("--strictPort")
        .spawn()?;

    print_dev_urls(options);
    wait_for_first_exit(&mut gateway, &mut vite)?;
    Ok(())
}

fn fmt() -> Result<()> {
    run_command(
        "Formatting Rust workspace",
        command("cargo", ["fmt", "--all"]),
    )?;
    run_command("Formatting justfile", command("just", ["--fmt"]))?;
    Ok(())
}

fn fmt_check() -> Result<()> {
    run_command(
        "Checking Rust formatting",
        command("cargo", ["fmt", "--all", "--", "--check"]),
    )?;
    run_command(
        "Checking justfile formatting",
        command("just", ["--fmt", "--check"]),
    )?;
    Ok(())
}

fn check() -> Result<()> {
    setup()?;
    run_command(
        "Checking Rust workspace",
        command("cargo", ["check", "--workspace"]),
    )?;
    run_command(
        "Checking frontend types",
        npm_command(FRONTEND_DIR, ["run", "check"]),
    )?;
    Ok(())
}

fn clippy() -> Result<()> {
    run_command(
        "Linting Rust workspace",
        command(
            "cargo",
            [
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
    )
}

fn test() -> Result<()> {
    run_with_final_sweep("test", run_test_suite)
}

fn run_test_suite() -> Result<()> {
    setup()?;
    run_command(
        "Running Rust tests",
        command("cargo", ["test", "--workspace"]),
    )?;
    run_command(
        "Running frontend tests",
        npm_command(FRONTEND_DIR, ["test"]),
    )?;
    build_frontend_release_if_needed(false)?;
    install_desktop_deps_if_needed()?;
    run_command(
        "Running desktop smoke test",
        npm_command(DESKTOP_DIR, ["test"]),
    )?;
    Ok(())
}

fn test_lib(mut args: impl Iterator<Item = String>) -> Result<()> {
    let package = args
        .next()
        .ok_or("usage: cargo run -p xtask -- test-lib <package> [filter...]")?;
    let filters: Vec<String> = args.collect();
    let mut command = Command::new("cargo");
    command
        .arg("test")
        .arg("-p")
        .arg(&package)
        .arg("--lib")
        .args(&filters);
    run_command(
        &format!("Running {package} library tests{}", filter_label(&filters)),
        command,
    )
}

fn test_integration(mut args: impl Iterator<Item = String>) -> Result<()> {
    let package = args.next().ok_or(
        "usage: cargo run -p xtask -- test-integration <package> <test-target> [filter...]",
    )?;
    let target = args.next().ok_or(
        "usage: cargo run -p xtask -- test-integration <package> <test-target> [filter...]",
    )?;
    let filters: Vec<String> = args.collect();
    let mut command = Command::new("cargo");
    command
        .arg("test")
        .arg("-p")
        .arg(&package)
        .arg("--test")
        .arg(&target)
        .args(&filters);
    run_command(
        &format!(
            "Running {package} integration test target {target}{}",
            filter_label(&filters)
        ),
        command,
    )
}

fn filter_label(filters: &[String]) -> String {
    if filters.is_empty() {
        String::new()
    } else {
        format!(" matching {}", filters.join(" "))
    }
}

fn ci() -> Result<()> {
    run_with_final_sweep("ci", || {
        fmt_check()?;
        check()?;
        clippy()?;
        run_test_suite()
    })
}

fn run_sweep() -> Result<()> {
    let before = ArtifactBudgetSnapshot::capture()?;
    let result = cargo_sweep();
    let after = ArtifactBudgetSnapshot::capture()?;
    print_artifact_budget_summary("sweep", &before, &after);
    result
}

fn release(options: &ReleaseOptions) -> Result<()> {
    setup()?;
    build_frontend_release_if_needed(options.force_frontend)?;

    run_command(
        "Building swage release binary",
        command(
            "cargo",
            [
                "build",
                "--release",
                "-p",
                APP_PACKAGE,
                "--bin",
                BINARY_NAME,
            ],
        ),
    )?;
    ensure_file(
        "target/release/swage",
        "release binary was not produced: target/release/swage",
    )?;
    println!("==> Built target/release/{BINARY_NAME}");
    Ok(())
}

fn install(options: &InstallOptions) -> Result<()> {
    if options.skip_frontend {
        ensure_file(
            FRONTEND_DIST_INDEX,
            "frontend/dist is missing; re-run without --skip-frontend",
        )?;
        println!("==> Reusing existing frontend/dist");
    } else {
        release(&ReleaseOptions::default())?;
    }

    if options.skip_frontend {
        run_command(
            "Building swage release binary",
            command(
                "cargo",
                [
                    "build",
                    "--release",
                    "-p",
                    APP_PACKAGE,
                    "--bin",
                    BINARY_NAME,
                ],
            ),
        )?;
    }

    fs::create_dir_all(&options.prefix)?;
    let installed_binary = options.prefix.join(BINARY_NAME);
    fs::copy("target/release/swage", &installed_binary)?;
    make_executable(&installed_binary)?;
    println!("==> Installed swage to {}", installed_binary.display());

    if options.lan_mode {
        run_command("Configuring gateway to bind 0.0.0.0:3000 by default", {
            let mut command = Command::new(&installed_binary);
            command
                .arg("gateway")
                .arg("install")
                .arg("--host")
                .arg("0.0.0.0")
                .arg("--port")
                .arg("3000");
            command
        })?;
    }

    print_install_next_steps(&options.prefix, options.lan_mode);
    Ok(())
}

fn install_frontend_deps_if_needed() -> Result<()> {
    let current_hash = deps_hash(FRONTEND_DIR)?;
    let previous_hash = read_stamp(FRONTEND_DEPS_STAMP)?;
    if !Path::new("frontend/node_modules").is_dir()
        || previous_hash.as_deref() != Some(&current_hash)
    {
        run_command(
            "Installing frontend dependencies",
            npm_command(FRONTEND_DIR, ["ci"]),
        )?;
        write_stamp(FRONTEND_DEPS_STAMP, &current_hash)?;
    } else {
        println!("==> Frontend dependencies already installed");
    }
    Ok(())
}

fn install_desktop_deps_if_needed() -> Result<()> {
    let current_hash = deps_hash(DESKTOP_DIR)?;
    let previous_hash = read_stamp(DESKTOP_DEPS_STAMP)?;
    if !Path::new("desktop/node_modules").is_dir()
        || previous_hash.as_deref() != Some(&current_hash)
    {
        run_command(
            "Installing desktop dependencies",
            npm_command(DESKTOP_DIR, ["ci"]),
        )?;
        write_stamp(DESKTOP_DEPS_STAMP, &current_hash)?;
    } else {
        println!("==> Desktop dependencies already installed");
    }
    Ok(())
}

fn build_frontend_release_if_needed(force: bool) -> Result<()> {
    let current_hash = frontend_release_hash()?;
    let previous_hash = read_stamp(FRONTEND_RELEASE_STAMP)?;
    if force
        || previous_hash.as_deref() != Some(&current_hash)
        || !Path::new(FRONTEND_DIST_INDEX).is_file()
    {
        run_command(
            "Building frontend release bundle",
            npm_command(FRONTEND_DIR, ["run", "build"]),
        )?;
        ensure_file(
            FRONTEND_DIST_INDEX,
            "frontend release build did not produce frontend/dist/index.html",
        )?;
        write_stamp(FRONTEND_RELEASE_STAMP, &current_hash)?;
    } else {
        println!("==> Frontend release bundle unchanged");
    }
    Ok(())
}

fn deps_hash(package_dir: &str) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_command_output(&mut hasher, "node", ["--version"])?;
    hash_command_output(&mut hasher, "npm", ["--version"])?;
    hash_file_if_present(
        &mut hasher,
        Path::new(package_dir).join("package.json").as_path(),
    )?;
    hash_file_if_present(
        &mut hasher,
        Path::new(package_dir).join("package-lock.json").as_path(),
    )?;
    hash_file_if_present(
        &mut hasher,
        Path::new(package_dir).join("npm-shrinkwrap.json").as_path(),
    )?;
    Ok(hex_digest(hasher))
}

fn frontend_release_hash() -> Result<String> {
    let mut hasher = Sha256::new();
    hash_command_output(&mut hasher, "node", ["--version"])?;
    hash_command_output(&mut hasher, "npm", ["--version"])?;

    for path in [
        "frontend/index.html",
        "frontend/package.json",
        "frontend/package-lock.json",
        "frontend/npm-shrinkwrap.json",
        "frontend/svelte.config.js",
        "frontend/tsconfig.json",
        "frontend/vite.config.ts",
    ] {
        hash_file_if_present(&mut hasher, Path::new(path))?;
    }

    hash_dir_if_present(&mut hasher, Path::new("frontend/public"))?;
    hash_dir(&mut hasher, Path::new("frontend/src"))?;
    Ok(hex_digest(hasher))
}

fn hash_command_output<const N: usize>(
    hasher: &mut Sha256,
    program: &str,
    args: [&str; N],
) -> Result<()> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(format!("{program} command failed while hashing inputs").into());
    }
    hasher.update(program.as_bytes());
    hasher.update(&output.stdout);
    Ok(())
}

fn hash_file_if_present(hasher: &mut Sha256, path: &Path) -> Result<()> {
    if path.is_file() {
        hash_file(hasher, path)?;
    } else {
        hasher.update(format!("missing:{}", stable_missing_label(path)).as_bytes());
    }
    Ok(())
}

fn hash_dir_if_present(hasher: &mut Sha256, path: &Path) -> Result<()> {
    if path.is_dir() {
        hash_dir(hasher, path)?;
    } else {
        hasher.update(format!("missing-dir:{}", stable_missing_label(path)).as_bytes());
    }
    Ok(())
}

fn hash_dir(hasher: &mut Sha256, path: &Path) -> Result<()> {
    let mut files = Vec::new();
    collect_files(path, &mut files)?;
    files.sort();
    for file in files {
        hash_file(hasher, &file)?;
    }
    Ok(())
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name != "node_modules" && name != "dist" {
                collect_files(&path, files)?;
            }
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn hash_file(hasher: &mut Sha256, path: &Path) -> Result<()> {
    let stable_path = fs::canonicalize(path)?;
    hasher.update(stable_path.to_string_lossy().as_bytes());
    hasher.update(fs::read(path)?);
    Ok(())
}

fn stable_missing_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().to_string(),
    )
}

fn hex_digest(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn read_stamp(path: &str) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_stamp(path: &str, value: &str) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{value}\n"))?;
    Ok(())
}

fn require_command(program: &str) -> Result<()> {
    let status = Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(format!("required command not found: {program}").into())
        }
        Err(error) => Err(error.into()),
    }
}

fn run_command(label: &str, mut command: Command) -> Result<()> {
    println!("==> {label}");
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{label} failed with {status}").into());
    }
    Ok(())
}

fn command<const N: usize>(program: &str, args: [&str; N]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

fn npm_command<const N: usize>(package_dir: &str, args: [&str; N]) -> Command {
    let mut command = Command::new("npm");
    command.arg("--prefix").arg(package_dir).args(args);
    command
}

fn cargo_sweep() -> Result<()> {
    if Command::new("cargo-sweep")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        println!("==> Skipping cargo sweep (install with: cargo install cargo-sweep)");
        return Ok(());
    }

    run_command(
        "Capping target/ at 50 GB",
        command("cargo", ["sweep", "--maxsize", TARGET_MAX_SIZE_MB_ARG]),
    )?;
    prune_debug_incremental_if_needed()
}

fn prune_debug_incremental_if_needed() -> Result<()> {
    if allocated_disk_usage(Path::new("target"))? <= TARGET_MAX_SIZE_BYTES {
        return Ok(());
    }

    let incremental_dir = Path::new(DEBUG_INCREMENTAL_DIR);
    if !incremental_dir.is_dir() {
        return Ok(());
    }

    println!(
        "==> Removing {DEBUG_INCREMENTAL_DIR} because target/ is still above 50 GB after cargo sweep"
    );
    fs::remove_dir_all(incremental_dir)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct ArtifactBudgetSnapshot {
    target: u64,
    debug_deps: u64,
    debug_incremental: u64,
}

impl ArtifactBudgetSnapshot {
    fn capture() -> io::Result<Self> {
        Ok(Self {
            target: allocated_disk_usage(Path::new("target"))?,
            debug_deps: allocated_disk_usage(Path::new("target/debug/deps"))?,
            debug_incremental: allocated_disk_usage(Path::new(DEBUG_INCREMENTAL_DIR))?,
        })
    }
}

fn print_artifact_budget_summary(
    label: &str,
    before: &ArtifactBudgetSnapshot,
    after: &ArtifactBudgetSnapshot,
) {
    let cleaned = before.target.saturating_sub(after.target);
    let growth = after.target.saturating_sub(before.target);

    println!("==> Build artifact budget after {label}");
    println!(
        "    target/debug/deps:        {}",
        format_bytes(after.debug_deps)
    );
    println!(
        "    target/debug/incremental: {}",
        format_bytes(after.debug_incremental)
    );
    println!(
        "    target total:             {}",
        format_bytes(after.target)
    );
    if cleaned > 0 {
        println!("    cleaned:                  {}", format_bytes(cleaned));
    } else if growth > 0 {
        println!(
            "    cleaned:                  0 B (grew by {})",
            format_bytes(growth)
        );
    } else {
        println!("    cleaned:                  0 B");
    }
}

fn allocated_disk_usage(path: &Path) -> io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }

    let metadata = fs::symlink_metadata(path)?;
    let mut size = allocated_size(&metadata);
    if !metadata.file_type().is_dir() {
        return Ok(size);
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        size += allocated_disk_usage(&path)?;
    }
    Ok(size)
}

fn run_with_final_sweep(label: &str, action: impl FnOnce() -> Result<()>) -> Result<()> {
    let before = ArtifactBudgetSnapshot::capture()?;
    let action_result = action();

    // Rust test links can leave large temporary artefacts even when the command
    // fails, so the cleanup is intentionally a final step rather than a success
    // path afterthought.
    let sweep_result = cargo_sweep();
    let after = ArtifactBudgetSnapshot::capture()?;
    print_artifact_budget_summary(label, &before, &after);

    match (action_result, sweep_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(action_error), Err(sweep_error)) => Err(format!(
            "{label} failed: {action_error}; final cargo sweep also failed: {sweep_error}"
        )
        .into()),
    }
}

#[cfg(unix)]
fn allocated_size(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks() * 512
}

#[cfg(not(unix))]
fn allocated_size(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn ensure_file(path: &str, message: &str) -> Result<()> {
    if Path::new(path).is_file() {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn wait_for_first_exit(gateway: &mut Child, vite: &mut Child) -> Result<()> {
    loop {
        if let Some(status) = gateway.try_wait()? {
            let _ = vite.kill();
            let _ = vite.wait();
            return if status.success() {
                Ok(())
            } else {
                Err(format!("Swage gateway exited with {status}").into())
            };
        }

        if let Some(status) = vite.try_wait()? {
            let _ = gateway.kill();
            let _ = gateway.wait();
            return if status.success() {
                Ok(())
            } else {
                Err(format!("Vite dev server exited with {status}").into())
            };
        }

        thread::sleep(Duration::from_millis(250));
    }
}

fn print_dev_urls(options: &DevOptions) {
    println!();
    println!(
        "    Gateway API:       http://127.0.0.1:{}",
        options.gateway_port
    );
    println!(
        "    Web UI:            http://localhost:{}",
        options.vite_port
    );
    println!("    Gateway UI route:  redirects to the live Vite UI");
    println!(
        "    LAN UI:            http://{}:{}",
        lan_ip().unwrap_or_else(|| "<this-machine-ip>".to_string()),
        options.vite_port
    );
    println!();
    println!("    LAN exposure warning: the dev server has no authentication.");
    println!("    Only run this on networks you trust.");
    println!("    Ctrl-C to stop both.");
    println!();
}

fn print_install_next_steps(prefix: &Path, lan_mode: bool) {
    println!();
    println!("Next steps:");
    if path_contains(prefix) {
        println!("  1. Start the gateway: swage gateway start");
    } else {
        println!("  1. {} is not on PATH yet.", prefix.display());
        println!(
            "     fish: set -U fish_user_paths {} $fish_user_paths",
            prefix.display()
        );
        println!(
            "     bash: echo 'export PATH=\"{}:$PATH\"' >> ~/.bashrc",
            prefix.display()
        );
        println!("  2. Start the gateway: swage gateway start");
    }

    if lan_mode {
        println!("  2. Open the UI:");
        println!("     Local: http://127.0.0.1:3000");
        println!(
            "     LAN:   http://{}:3000",
            lan_ip().unwrap_or_else(|| "<this-machine-ip>".to_string())
        );
        println!();
        println!("LAN exposure warning:");
        println!("  The gateway has no authentication. Only use --lan on networks you trust.");
    } else {
        println!("  2. Open http://127.0.0.1:3000 in a browser.");
    }
}

fn warn_if_vite_proxy_mismatch(gateway_port: &str) -> Result<()> {
    let config = fs::read_to_string("frontend/vite.config.ts")?;
    let Some(proxy_marker) = config.split("http://127.0.0.1:").nth(1) else {
        return Ok(());
    };
    let expected_port = proxy_marker
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if !expected_port.is_empty() && expected_port != gateway_port {
        eprintln!(
            "==> Note: vite.config.ts proxies /api to port {expected_port}, but the gateway will run on {gateway_port}."
        );
    }
    Ok(())
}

fn lan_ip() -> Option<String> {
    let hostname_output = Command::new("hostname").arg("-I").output().ok()?;
    if hostname_output.status.success() {
        let output = String::from_utf8_lossy(&hostname_output.stdout);
        if let Some(ip) = output.split_whitespace().next() {
            return Some(ip.to_string());
        }
    }
    None
}

fn path_contains(prefix: &Path) -> bool {
    env::var_os("PATH").is_some_and(|paths| env::split_paths(&paths).any(|path| path == prefix))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".into())
}

fn option_value(arg: &str) -> &str {
    arg.split_once('=').map_or("", |(_, value)| value)
}

fn print_help() {
    println!(
        "Usage: cargo run -p xtask -- <command>\n\nCommands:\n  setup\n  build-dev [--embed-frontend]\n  dev [--gateway-port PORT] [--vite-host HOST] [--vite-port PORT]\n  fmt\n  fmt-check\n  check\n  clippy\n  test\n  test-lib <package> [filter...]\n  test-integration <package> <test-target> [filter...]\n  ci\n  sweep\n  release [--force-frontend]\n  install [--prefix DIR] [--skip-frontend] [--lan]"
    );
}
