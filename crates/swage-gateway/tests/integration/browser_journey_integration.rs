use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use swage_config::{BrowserScreenshotPolicy, BrowserToolConfig, ToolPermissionConfig};
use swage_gateway::browser::{BrowserConfig, BrowserHost};
use swage_tool::permission::{PermissionConfig, check_permission};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_browser_drives_fixture_and_persists_profile() {
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let fixture = FixtureServer::spawn().await;
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));

    let form_url = fixture.url("/login");
    let first = host
        .navigate("session-a", &form_url)
        .await
        .expect("navigate login page");
    assert!(first.output.contains("textbox \"Username\""));
    assert!(first.output.contains("link \"Sign in\""));

    let username = first
        .refs
        .values()
        .find(|entry| entry.name == "Username")
        .expect("username ref")
        .reference
        .clone();
    let password = first
        .refs
        .values()
        .find(|entry| entry.name == "Password")
        .expect("password ref")
        .reference
        .clone();
    let submit = first
        .refs
        .values()
        .find(|entry| entry.name == "Sign in")
        .expect("submit ref")
        .reference
        .clone();

    host.type_text("session-a", &username, "ben", true, false)
        .await
        .expect("type username");
    host.type_text("session-a", &password, "secret", true, false)
        .await
        .expect("type password");
    let (after_submit, audit) = host
        .click("session-a", &submit)
        .await
        .expect("click submit");
    assert!(after_submit.output.contains("heading \"Signed in\""));
    assert!(audit.navigated);
    assert!(fixture.saw_login.load(Ordering::SeqCst));

    host.shutdown().await;

    let relaunched = BrowserHost::new(browser_config(&binary, temp.path(), fixture.host()));
    let whoami = relaunched
        .navigate("session-b", &fixture.url("/whoami"))
        .await
        .expect("navigate whoami after restart");
    assert!(whoami.output.contains("Signed in as ben"));
    relaunched.shutdown().await;
}

#[test]
fn browser_permission_bundles_split_read_from_actions() {
    let defaults = ToolPermissionConfig::default();
    let read_only = PermissionConfig {
        allow: vec!["browsing".to_string()],
        deny: Vec::new(),
        bundles: defaults.bundles,
        require_approval: Vec::new(),
    };
    assert!(check_permission(&read_only, "browser_snapshot").is_ok());
    assert!(check_permission(&read_only, "browser_navigate").is_ok());
    assert!(check_permission(&read_only, "browser_click").is_err());
    assert!(check_permission(&read_only, "browser_type").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "hand-run live smoke against the public web; never run in CI"]
async fn live_smoke_public_site() {
    let Some(binary) = browser_binary_or_skip() else {
        return;
    };
    let temp = TempDir::new().expect("tempdir");
    let host = BrowserHost::new(browser_config(
        &binary,
        temp.path(),
        "example.com".to_string(),
    ));
    let snapshot = host
        .navigate("live-smoke", "https://example.com/")
        .await
        .expect("navigate example.com");
    println!("{}", snapshot.output);
    assert!(snapshot.output.contains("Example Domain"));
    host.shutdown().await;
}

fn browser_config(binary: &str, root: &std::path::Path, host: String) -> BrowserConfig {
    let tool_config = BrowserToolConfig {
        binary_path: Some(binary.to_string()),
        page_idle_secs: 30,
        process_idle_secs: 30,
        max_snapshot_bytes: 40_000,
        screenshots: BrowserScreenshotPolicy::Off,
        allow_hosts: vec![host],
    };
    BrowserConfig {
        binary_path: tool_config.binary_path,
        profile_root: root.join("profiles"),
        page_idle: std::time::Duration::from_secs(tool_config.page_idle_secs),
        process_idle: std::time::Duration::from_secs(tool_config.process_idle_secs),
        max_snapshot_bytes: tool_config.max_snapshot_bytes,
        screenshots: tool_config.screenshots,
        allow_hosts: tool_config.allow_hosts,
    }
}

fn browser_binary_or_skip() -> Option<String> {
    for candidate in [
        std::env::var("SWAGE_BROWSER_BIN").ok(),
        which("chromium"),
        which("google-chrome-stable"),
        which("chromium-browser"),
    ]
    .into_iter()
    .flatten()
    {
        return Some(candidate);
    }
    if std::env::var("SWAGE_BROWSER_TESTS").as_deref() == Ok("required") {
        panic!("SWAGE_BROWSER_TESTS=required but no Chromium binary was found");
    }
    eprintln!("skipping browser journey: no Chromium binary found");
    None
}

fn which(binary: &str) -> Option<String> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(binary))
            .find(|path| path.is_file())
            .map(|path| path.display().to_string())
    })
}

struct FixtureServer {
    addr: SocketAddr,
    shutdown: CancellationToken,
    saw_login: Arc<AtomicBool>,
}

impl FixtureServer {
    async fn spawn() -> Self {
        let saw_login = Arc::new(AtomicBool::new(false));
        let app = Router::new()
            .route("/login", get(login_page).post(login_post))
            .route("/set-cookie", get(set_cookie))
            .route("/signed-in", get(signed_in))
            .route("/whoami", get(whoami))
            .with_state(Arc::clone(&saw_login));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fixture");
        let addr = listener.local_addr().expect("fixture address");
        let shutdown = CancellationToken::new();
        let shutdown_for_task = shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown_for_task.cancelled().await;
                })
                .await
                .expect("fixture server");
        });
        Self {
            addr,
            shutdown,
            saw_login,
        }
    }

    fn host(&self) -> String {
        self.addr.ip().to_string()
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn login_page() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
        <title>Fixture Login</title>
        <h1>Sign in</h1>
        <form method="post" action="/login">
            <label>Username <input name="username" autocomplete="username"></label>
            <label>Password <input name="password" type="password" autocomplete="current-password"></label>
            <a href="/set-cookie">Sign in</a>
        </form>"#,
    )
}

async fn login_post(State(saw_login): State<Arc<AtomicBool>>) -> impl IntoResponse {
    saw_login.store(true, Ordering::SeqCst);
    (
        StatusCode::SEE_OTHER,
        [
            (
                "set-cookie",
                "swage_fixture_user=ben; Path=/; Max-Age=3600; SameSite=Lax",
            ),
            ("location", "/signed-in"),
        ],
        Html(""),
    )
}

async fn signed_in() -> Html<&'static str> {
    Html("<title>Signed in</title><h1>Signed in</h1>")
}

async fn set_cookie(State(saw_login): State<Arc<AtomicBool>>) -> impl IntoResponse {
    saw_login.store(true, Ordering::SeqCst);
    (
        StatusCode::SEE_OTHER,
        [
            (
                "set-cookie",
                "swage_fixture_user=ben; Path=/; Max-Age=3600; SameSite=Lax",
            ),
            ("location", "/signed-in"),
        ],
        Html(""),
    )
}

async fn whoami(headers: HeaderMap) -> impl IntoResponse {
    let cookie = headers
        .get("cookie")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if cookie.contains("swage_fixture_user=ben") {
        Html("<title>Whoami</title><h1>Signed in as ben</h1>").into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}
