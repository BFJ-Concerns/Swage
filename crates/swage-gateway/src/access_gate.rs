//! The token gate on a gateway bound beyond loopback (C41, C69).
//!
//! Bound to `127.0.0.1` the gateway is reachable only from its own machine
//! and asks nothing of a request. Bound to a private network address it asks
//! every request from beyond loopback for its token — a bearer header, the
//! `X-Swage-Token` header, the `swage_token` cookie a paired browser carries,
//! or a `token` query parameter for a WebSocket a browser cannot add headers
//! to. A request from loopback is always admitted: the operator's own machine
//! is where pairing codes are issued and where the CLI runs.
//!
//! The token lives in one file under the data root, generated once when the
//! gateway first starts without one. A client gets it by redeeming a
//! one-time pairing code that only a loopback request can issue, and the
//! gateway sets it as a cookie on pairing so a browser remembers it without
//! the page keeping it. Relaxing the gate — `warn` admits and logs, `off`
//! admits — is install-wide configuration (`[server] access`), recorded like
//! any other security relaxation and shown as relaxed.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use swage_protocol::client::access::{
    AccessGateMode, AccessStatusResponse, PairClientRequest, PairClientResponse,
    PairingCodeResponse,
};
use swage_protocol::client::error::ErrorCode;

use crate::error::ApiError;
use crate::state::AppState;

/// The file under the data root that holds the gateway's token.
pub const TOKEN_FILE: &str = "gateway-token";
/// The cookie a paired browser carries.
pub const TOKEN_COOKIE: &str = "swage_token";
/// The header a paired CLI or desktop app sends.
pub const TOKEN_HEADER: &str = "x-swage-token";
/// How long an issued pairing code may wait to be redeemed.
pub const PAIRING_CODE_LIFETIME: Duration = Duration::minutes(10);
/// Routes every client must reach before it is paired: liveness, and pairing
/// itself. The frontend's own assets are outside `/api/` and never gated.
const UNGATED_PATHS: &[&str] = &["/api/health", "/api/access", "/api/pair"];

/// The gate as this gateway runs it.
#[derive(Debug)]
pub struct AccessGate {
    /// Whether the bound address reaches beyond loopback; `false` means the
    /// gate never applies, whatever the mode.
    beyond_loopback: bool,
    token: Option<String>,
    pairing_codes: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl AccessGate {
    /// A gate that never applies — the default a state is built with until
    /// startup reads the binding and the token.
    pub fn open() -> Self {
        Self {
            beyond_loopback: false,
            token: None,
            pairing_codes: Mutex::new(HashMap::new()),
        }
    }

    /// The gate for a gateway binding `host`, with the token read from the
    /// data root or generated there when absent.
    pub fn for_binding(config_root: &Path, host: &str) -> std::io::Result<Self> {
        let beyond_loopback = !host_is_loopback(host);
        let token = load_or_create_token(&config_root.join(TOKEN_FILE))?;
        Ok(Self {
            beyond_loopback,
            token: Some(token),
            pairing_codes: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_token(host: &str, token: &str) -> Self {
        Self {
            beyond_loopback: !host_is_loopback(host),
            token: Some(token.to_owned()),
            pairing_codes: Mutex::new(HashMap::new()),
        }
    }

    /// Whether the binding reaches beyond loopback at all.
    pub fn beyond_loopback(&self) -> bool {
        self.beyond_loopback
    }

    /// Whether requests from beyond loopback are asked for the token under
    /// `mode` — the active configuration's `[server] access`, read per
    /// request so a relaxation applies without a restart.
    pub fn gated(&self, mode: AccessGateMode) -> bool {
        self.beyond_loopback && mode != AccessGateMode::Off
    }

    /// Whether a request carrying `presented` is admitted.
    fn admits(&self, presented: Option<&str>) -> bool {
        match (&self.token, presented) {
            (Some(token), Some(presented)) => {
                constant_time_eq(token.as_bytes(), presented.as_bytes())
            }
            _ => false,
        }
    }

    /// Issue a one-time pairing code.
    pub fn issue_pairing_code(&self) -> PairingCodeResponse {
        let code = pairing_code();
        let expires_at = Utc::now() + PAIRING_CODE_LIFETIME;
        let mut codes = self.pairing_codes.lock().unwrap_or_else(|e| e.into_inner());
        codes.retain(|_, expiry| *expiry > Utc::now());
        codes.insert(code.clone(), expires_at);
        PairingCodeResponse {
            pairing_code: code,
            expires_at,
        }
    }

    /// Redeem a pairing code for the token; a code is good once.
    pub fn redeem_pairing_code(&self, code: &str) -> Result<String, ApiError> {
        let mut codes = self.pairing_codes.lock().unwrap_or_else(|e| e.into_inner());
        let expires_at = codes.remove(code.trim()).ok_or_else(|| {
            ApiError::validation("pairing code was not found or has already been used".into())
        })?;
        if expires_at < Utc::now() {
            return Err(ApiError::validation("pairing code has expired".into()));
        }
        self.token
            .clone()
            .ok_or_else(|| ApiError::internal("the gateway has no token to pair with".into()))
    }
}

/// Admit or refuse a request by the gate: loopback and the ungated routes
/// pass; otherwise the token must be presented, or the mode must be lenient.
pub async fn access_gate_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let gate = &state.access_gate;
    let mode = state.active_config().server.access;
    if !gate.gated(mode) || !request.uri().path().starts_with("/api/") {
        return next.run(request).await;
    }
    let path = request.uri().path();
    if UNGATED_PATHS.contains(&path) {
        return next.run(request).await;
    }
    if peer_is_loopback(&request) || gate.admits(presented_token(&request).as_deref()) {
        return next.run(request).await;
    }
    match mode {
        AccessGateMode::Token => ApiError::new(
            ErrorCode::Unauthorised,
            "this gateway is bound beyond loopback and admits paired clients only; redeem a pairing code at /api/pair",
        )
        .into_response(),
        AccessGateMode::Warn => {
            tracing::warn!(
                path,
                "request from beyond loopback carried no token; admitted under [server] access = \"warn\""
            );
            let mut response = next.run(request).await;
            response.headers_mut().insert(
                "x-swage-access",
                HeaderValue::from_static("unauthenticated"),
            );
            response
        }
        AccessGateMode::Off => next.run(request).await,
    }
}

/// Whether the gate applies and whether this request would be admitted.
pub fn access_status(state: &AppState, request: &Request<Body>) -> AccessStatusResponse {
    let gate = &state.access_gate;
    let mode = state.active_config().server.access;
    AccessStatusResponse {
        gated: gate.gated(mode),
        mode,
        admitted: !gate.gated(mode)
            || peer_is_loopback(request)
            || gate.admits(presented_token(request).as_deref()),
    }
}

/// Issue a pairing code — from loopback only, the gateway's own machine.
pub fn issue_pairing_code(
    state: &AppState,
    peer: Option<SocketAddr>,
) -> Result<PairingCodeResponse, ApiError> {
    if !peer.is_some_and(|addr| addr.ip().is_loopback()) {
        return Err(ApiError::forbidden(
            "pairing codes are issued from the gateway's own machine only",
        ));
    }
    Ok(state.access_gate.issue_pairing_code())
}

/// Redeem a pairing code: the token in the body and as the browser's cookie.
pub fn pair_client(state: &AppState, request: PairClientRequest) -> Result<Response, ApiError> {
    let token = state
        .access_gate
        .redeem_pairing_code(&request.pairing_code)?;
    let cookie = format!("{TOKEN_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict");
    let mut response = axum::Json(PairClientResponse { token }).into_response();
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|_| ApiError::internal("token is not header-safe".into()))?,
    );
    Ok(response)
}

fn peer_is_loopback(request: &Request<Body>) -> bool {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback())
}

/// The token a request presents, by any of the routes a client has.
fn presented_token(request: &Request<Body>) -> Option<String> {
    let headers = request.headers();
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(bearer) = value.strip_prefix("Bearer ")
    {
        return Some(bearer.trim().to_owned());
    }
    if let Some(value) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) {
        return Some(value.trim().to_owned());
    }
    if let Some(cookies) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for cookie in cookies.split(';') {
            if let Some((name, value)) = cookie.trim().split_once('=')
                && name == TOKEN_COOKIE
            {
                return Some(value.trim().to_owned());
            }
        }
    }
    request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            pair.split_once('=')
                .filter(|(name, _)| *name == "token")
                .map(|(_, value)| value.to_owned())
        })
    })
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn load_or_create_token(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(existing) if !existing.trim().is_empty() => return Ok(existing.trim().to_owned()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token = random_hex(32);
    write_private(path, &token)?;
    Ok(token)
}

#[cfg(unix)]
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    std::io::Write::write_all(&mut file, body.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    std::fs::write(path, body)
}

fn random_hex(bytes: usize) -> String {
    let mut rng = rand::rng();
    (0..bytes)
        .map(|_| format!("{:02x}", rng.random::<u8>()))
        .collect()
}

/// A pairing code a person can read out: eight digits in two groups.
fn pairing_code() -> String {
    let mut rng = rand::rng();
    let number: u32 = rng.random_range(0..100_000_000);
    format!("{:04}-{:04}", number / 10_000, number % 10_000)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Where the token file lives for a data root.
pub fn token_path(config_root: &Path) -> PathBuf {
    config_root.join(TOKEN_FILE)
}

pub type SharedAccessGate = Arc<AccessGate>;

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str, headers: &[(&str, &str)], peer: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        request
    }

    #[test]
    fn a_loopback_binding_is_never_gated_and_a_lan_binding_is() {
        assert!(!AccessGate::with_token("127.0.0.1", "t").gated(AccessGateMode::Token));
        assert!(!AccessGate::with_token("localhost", "t").gated(AccessGateMode::Token));
        assert!(!AccessGate::with_token("::1", "t").gated(AccessGateMode::Token));
        assert!(AccessGate::with_token("0.0.0.0", "t").gated(AccessGateMode::Token));
        assert!(AccessGate::with_token("192.168.1.20", "t").gated(AccessGateMode::Warn));
        assert!(!AccessGate::with_token("0.0.0.0", "t").gated(AccessGateMode::Off));
    }

    #[test]
    fn the_token_is_read_from_every_route_a_client_has() {
        let bearer = request("/api/topics", &[("authorization", "Bearer abc")], None);
        assert_eq!(presented_token(&bearer).as_deref(), Some("abc"));
        let header = request("/api/topics", &[("x-swage-token", "def")], None);
        assert_eq!(presented_token(&header).as_deref(), Some("def"));
        let cookie = request(
            "/api/topics",
            &[("cookie", "theme=dark; swage_token=ghi")],
            None,
        );
        assert_eq!(presented_token(&cookie).as_deref(), Some("ghi"));
        let query = request("/api/conversations/x/ws?token=jkl", &[], None);
        assert_eq!(presented_token(&query).as_deref(), Some("jkl"));
        assert_eq!(presented_token(&request("/api/topics", &[], None)), None);
    }

    #[test]
    fn a_gate_admits_only_its_own_token() {
        let gate = AccessGate::with_token("0.0.0.0", "secret-token");
        assert!(gate.admits(Some("secret-token")));
        assert!(!gate.admits(Some("secret-tokeN")));
        assert!(!gate.admits(Some("")));
        assert!(!gate.admits(None));
        assert!(!AccessGate::open().admits(Some("anything")));
    }

    #[test]
    fn a_pairing_code_is_redeemed_once_for_the_token() {
        let gate = AccessGate::with_token("0.0.0.0", "secret-token");
        let issued = gate.issue_pairing_code();
        assert_eq!(issued.pairing_code.len(), 9, "{}", issued.pairing_code);
        assert_eq!(
            gate.redeem_pairing_code(&issued.pairing_code).unwrap(),
            "secret-token"
        );
        let again = gate.redeem_pairing_code(&issued.pairing_code).unwrap_err();
        assert!(again.message().contains("already been used"));
        let unknown = gate.redeem_pairing_code("0000-0000").unwrap_err();
        assert!(unknown.message().contains("not found"));
    }

    #[test]
    fn the_token_file_is_created_once_and_read_back() {
        let root = tempfile::tempdir().unwrap();
        let first = AccessGate::for_binding(root.path(), "0.0.0.0").unwrap();
        let second = AccessGate::for_binding(root.path(), "0.0.0.0").unwrap();
        assert_eq!(first.token, second.token);
        assert_eq!(first.token.as_ref().unwrap().len(), 64);
        let written = std::fs::read_to_string(root.path().join(TOKEN_FILE)).unwrap();
        assert_eq!(written.trim(), first.token.as_deref().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.path().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
