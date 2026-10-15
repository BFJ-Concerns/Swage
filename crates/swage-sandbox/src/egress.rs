//! The recording egress proxy a sandboxed command reaches the network through.
//!
//! A sandboxed child runs in its own network namespace with nothing but
//! loopback, so it cannot reach the network at all. What it can reach is a
//! unix socket the host binds into the sandbox: the host end of that socket
//! is an HTTP proxy — `CONNECT host:port` for TLS and any other protocol a
//! client tunnels, absolute-form requests for plain HTTP — and the child end
//! is a small bridge listening on the sandbox's loopback, so every HTTP-aware
//! tool (git, cargo, npm, pip, curl, uv) finds a proxy at `HTTP_PROXY` and
//! `HTTPS_PROXY` and needs no configuration of its own. Nothing else leaves
//! the sandbox.
//!
//! Every connection is decided by an [`EgressPolicy`] and reported to an
//! [`EgressRecorder`] before any byte reaches the destination, so the record
//! lists every host a sandboxed command tried to reach and what was decided.
//! The proxy owns neither decision nor record: the gateway supplies both —
//! today an allow-list and the request judge, recorded as audit events.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use swage_core::SandboxError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::task::JoinHandle;

use crate::socket::SocketManager;

/// The environment variable carrying the proxy's unix socket path into the
/// sandboxed child.
pub const EGRESS_SOCKET_ENV: &str = "SWAGE_EGRESS_SOCKET";

/// The loopback port the child's bridge listens on. The sandbox's network
/// namespace is its own, so the port is free there by construction.
pub const EGRESS_BRIDGE_PORT: u16 = 3128;

/// The longest request head the proxy reads before answering 400.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// How long the proxy waits for the destination to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// One destination a sandboxed command asked to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRequest {
    pub host: String,
    pub port: u16,
}

/// What the policy decided about a destination, and on what basis — the
/// record keeps the basis beside the verdict, so an allow by the standing
/// list reads differently from an allow a judge gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressVerdict {
    Allowed { basis: String },
    Refused { reason: String },
}

impl EgressVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, EgressVerdict::Allowed { .. })
    }
}

/// Decides whether a sandboxed command may reach a destination.
#[async_trait]
pub trait EgressPolicy: Send + Sync {
    async fn decide(&self, request: &EgressRequest) -> EgressVerdict;
}

/// One connection attempt as the record keeps it: the destination, how the
/// client asked (`CONNECT`, or the method of a plain HTTP request) and what
/// was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressConnection {
    pub host: String,
    pub port: u16,
    pub method: String,
    pub verdict: EgressVerdict,
}

/// Receives every connection attempt, decided, before it proceeds.
#[async_trait]
pub trait EgressRecorder: Send + Sync {
    async fn record(&self, connection: EgressConnection);
}

/// The policy and recorder a sandbox's proxy runs with.
#[derive(Clone)]
pub struct EgressSetup {
    pub policy: Arc<dyn EgressPolicy>,
    pub recorder: Arc<dyn EgressRecorder>,
}

// ---------------------------------------------------------------------------
// Host side: the proxy
// ---------------------------------------------------------------------------

/// The host end of the egress route: a unix socket the sandbox is given, and
/// the task serving it. Dropping the proxy stops the task and removes the
/// socket.
pub struct EgressProxy {
    socket: SocketManager,
    accept_task: JoinHandle<()>,
}

impl EgressProxy {
    /// Bind the proxy's socket and start serving it.
    pub fn start(setup: EgressSetup) -> Result<Self, SandboxError> {
        let (socket, listener) = SocketManager::create("swage-egress")?;
        let accept_task = tokio::spawn(accept_loop(listener, setup));
        Ok(Self {
            socket,
            accept_task,
        })
    }

    /// The socket path to bind into the sandbox and name in its environment.
    pub fn socket_path(&self) -> &Path {
        self.socket.path()
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn accept_loop(listener: UnixListener, setup: EgressSetup) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let setup = setup.clone();
                tokio::spawn(async move {
                    if let Err(error) = serve(stream, setup).await {
                        tracing::debug!(%error, "egress connection ended with an error");
                    }
                });
            }
            Err(error) => {
                tracing::warn!(%error, "egress proxy accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// A parsed proxy request: where the client wants to go and, for a plain
/// HTTP request, the head to forward once the tunnel is open.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProxyRequest {
    pub(crate) method: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    /// `None` for `CONNECT` (the client speaks to the destination directly
    /// once told the tunnel is open); the rewritten head otherwise.
    pub(crate) forward_head: Option<Vec<u8>>,
}

/// What the proxy answers before any destination is involved.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    /// The head could not be read as a proxy request.
    BadRequest(String),
}

/// Parse the request head a proxy client sent.
///
/// `CONNECT host:port HTTP/1.1` names the destination in its target. A plain
/// request carries an absolute URI (`GET http://host:port/path HTTP/1.1`),
/// which is rewritten to the origin form the destination expects, with the
/// hop-by-hop `Proxy-Connection` header dropped. Anything else is refused as
/// a bad request: a proxy that guessed would send traffic somewhere the
/// record never named.
pub(crate) fn parse_request(head: &[u8]) -> Result<ProxyRequest, RequestError> {
    let text = std::str::from_utf8(head)
        .map_err(|_| RequestError::BadRequest("request head is not UTF-8".into()))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| RequestError::BadRequest("empty request".into()))?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(RequestError::BadRequest(format!(
            "malformed request line: {request_line:?}"
        )));
    };
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(RequestError::BadRequest(format!(
            "malformed request line: {request_line:?}"
        )));
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(target, None)?;
        return Ok(ProxyRequest {
            method: "CONNECT".into(),
            host,
            port,
            forward_head: None,
        });
    }

    let Some(rest) = target.strip_prefix("http://") else {
        return Err(RequestError::BadRequest(format!(
            "a proxy request needs an absolute http:// target or CONNECT, got {target:?}"
        )));
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = split_authority(authority, Some(80))?;
    let mut forwarded = format!("{method} {path} {version}\r\n");
    for line in lines {
        if line.is_empty() {
            break;
        }
        let name = line.split(':').next().unwrap_or_default().trim();
        if name.eq_ignore_ascii_case("proxy-connection") {
            continue;
        }
        forwarded.push_str(line);
        forwarded.push_str("\r\n");
    }
    forwarded.push_str("\r\n");
    Ok(ProxyRequest {
        method: method.to_owned(),
        host,
        port,
        forward_head: Some(forwarded.into_bytes()),
    })
}

/// `host:port`, `[v6]:port`, or a bare host with `default_port`.
fn split_authority(
    authority: &str,
    default_port: Option<u16>,
) -> Result<(String, u16), RequestError> {
    let bad = || RequestError::BadRequest(format!("malformed authority: {authority:?}"));
    if authority.is_empty() {
        return Err(bad());
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(bad)?;
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => return Err(bad()),
        };
        (host.to_owned(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host.to_owned(), Some(port)),
            Some(_) => return Err(bad()),
            None => (authority.to_owned(), None),
        }
    };
    if host.is_empty() {
        return Err(bad());
    }
    let port = match (port, default_port) {
        (Some(port), _) => port.parse::<u16>().map_err(|_| bad())?,
        (None, Some(default)) => default,
        (None, None) => return Err(bad()),
    };
    Ok((host, port))
}

/// Read up to and including the blank line ending the request head. Bytes
/// read past it belong to the body and are returned for forwarding.
async fn read_head(stream: &mut UnixStream) -> Result<(Vec<u8>, Vec<u8>), RequestError> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = find_head_end(&buffer) {
            let body = buffer.split_off(end);
            return Ok((buffer, body));
        }
        if buffer.len() >= MAX_HEAD_BYTES {
            return Err(RequestError::BadRequest(
                "request head exceeds 16 KiB".into(),
            ));
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| RequestError::BadRequest(format!("read failed: {error}")))?;
        if read == 0 {
            return Err(RequestError::BadRequest(
                "connection closed before the request head ended".into(),
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

async fn respond(stream: &mut UnixStream, status: &str, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

async fn serve(mut stream: UnixStream, setup: EgressSetup) -> std::io::Result<()> {
    let (head, body) = match read_head(&mut stream).await {
        Ok(parts) => parts,
        Err(RequestError::BadRequest(reason)) => {
            return respond(&mut stream, "400 Bad Request", &reason).await;
        }
    };
    let request = match parse_request(&head) {
        Ok(request) => request,
        Err(RequestError::BadRequest(reason)) => {
            return respond(&mut stream, "400 Bad Request", &reason).await;
        }
    };

    let verdict = setup
        .policy
        .decide(&EgressRequest {
            host: request.host.clone(),
            port: request.port,
        })
        .await;
    setup
        .recorder
        .record(EgressConnection {
            host: request.host.clone(),
            port: request.port,
            method: request.method.clone(),
            verdict: verdict.clone(),
        })
        .await;
    if let EgressVerdict::Refused { reason } = verdict {
        return respond(
            &mut stream,
            "403 Forbidden",
            &format!(
                "swage: {}:{} is not a destination this session may reach: {reason}",
                request.host, request.port
            ),
        )
        .await;
    }

    let upstream = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((request.host.as_str(), request.port)),
    )
    .await
    {
        Ok(Ok(upstream)) => upstream,
        Ok(Err(error)) => {
            return respond(
                &mut stream,
                "502 Bad Gateway",
                &format!(
                    "swage: could not connect to {}:{}: {error}",
                    request.host, request.port
                ),
            )
            .await;
        }
        Err(_) => {
            return respond(
                &mut stream,
                "504 Gateway Timeout",
                &format!(
                    "swage: connecting to {}:{} took longer than {}s",
                    request.host,
                    request.port,
                    CONNECT_TIMEOUT.as_secs()
                ),
            )
            .await;
        }
    };
    let mut upstream = upstream;

    match request.forward_head {
        None => {
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }
        Some(forward_head) => {
            upstream.write_all(&forward_head).await?;
        }
    }
    if !body.is_empty() {
        upstream.write_all(&body).await?;
    }
    tokio::io::copy_bidirectional(&mut stream, &mut upstream)
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// Child side: the bridge
// ---------------------------------------------------------------------------

/// The proxy address the child's tools are given, once the bridge listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressBridge {
    pub address: std::net::SocketAddr,
}

impl EgressBridge {
    /// The variables that point every HTTP-aware tool at the bridge. Both
    /// cases are set because tools disagree on which they read; loopback is
    /// excluded so a tool talking to a server the command itself started is
    /// not sent through the proxy.
    pub fn environment(&self) -> Vec<(&'static str, String)> {
        let url = format!("http://{}", self.address);
        vec![
            ("HTTP_PROXY", url.clone()),
            ("HTTPS_PROXY", url.clone()),
            ("http_proxy", url.clone()),
            ("https_proxy", url),
            ("NO_PROXY", "127.0.0.1,localhost".into()),
            ("no_proxy", "127.0.0.1,localhost".into()),
        ]
    }
}

/// Start the child's bridge when the host gave it an egress socket: listen on
/// the sandbox's loopback and carry each accepted connection to the socket.
///
/// The listener is bound synchronously so the caller can publish the address
/// in the process environment before any task runs. `None` when no socket
/// was given — a sandbox without egress — or when the bridge cannot start,
/// which is logged: the command then finds no proxy and its connections fail
/// as they would in a sandbox with no network.
pub fn start_egress_bridge() -> Option<EgressBridge> {
    let socket = PathBuf::from(std::env::var_os(EGRESS_SOCKET_ENV)?);
    let listener = std::net::TcpListener::bind(("127.0.0.1", EGRESS_BRIDGE_PORT))
        .or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0)))
        .and_then(|listener| {
            listener.set_nonblocking(true)?;
            Ok(listener)
        });
    let listener = match listener {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "egress bridge could not listen on loopback; the sandbox has no network route");
            return None;
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            tracing::warn!(%error, "egress bridge address unavailable");
            return None;
        }
    };
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, "egress bridge listener could not join the runtime");
            return None;
        }
    };
    tokio::spawn(bridge_loop(listener, socket));
    Some(EgressBridge { address })
}

async fn bridge_loop(listener: tokio::net::TcpListener, socket: PathBuf) {
    loop {
        let Ok((mut client, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let socket = socket.clone();
        tokio::spawn(async move {
            match UnixStream::connect(&socket).await {
                Ok(mut proxy) => {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut proxy).await;
                }
                Err(error) => {
                    tracing::warn!(%error, "egress bridge could not reach the host proxy");
                    let _ = client.shutdown().await;
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpListener;

    use super::*;

    struct AllowHosts(Vec<&'static str>);

    #[async_trait]
    impl EgressPolicy for AllowHosts {
        async fn decide(&self, request: &EgressRequest) -> EgressVerdict {
            if self.0.contains(&request.host.as_str()) {
                EgressVerdict::Allowed {
                    basis: "test list".into(),
                }
            } else {
                EgressVerdict::Refused {
                    reason: "not on the list".into(),
                }
            }
        }
    }

    #[derive(Default)]
    struct Recorded(Mutex<Vec<EgressConnection>>);

    #[async_trait]
    impl EgressRecorder for Recorded {
        async fn record(&self, connection: EgressConnection) {
            self.0.lock().unwrap().push(connection);
        }
    }

    fn setup(hosts: Vec<&'static str>) -> (EgressSetup, Arc<Recorded>) {
        let recorder = Arc::new(Recorded::default());
        (
            EgressSetup {
                policy: Arc::new(AllowHosts(hosts)),
                recorder: recorder.clone(),
            },
            recorder,
        )
    }

    /// A destination that answers one request with its request line, so a
    /// test can see what the proxy forwarded.
    async fn echo_server() -> (std::net::SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let body = format!("saw {}", line.trim_end());
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let mut stream = reader.into_inner();
                    stream.write_all(response.as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                });
            }
        });
        (address, task)
    }

    async fn send(proxy: &EgressProxy, request: &str) -> String {
        let mut stream = UnixStream::connect(proxy.socket_path()).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[test]
    fn connect_names_the_destination_and_forwards_nothing() {
        let request =
            parse_request(b"CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\n\r\n")
                .unwrap();
        assert_eq!(
            request,
            ProxyRequest {
                method: "CONNECT".into(),
                host: "github.com".into(),
                port: 443,
                forward_head: None,
            }
        );
        assert!(parse_request(b"CONNECT github.com HTTP/1.1\r\n\r\n").is_err());
        assert_eq!(
            parse_request(b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n")
                .unwrap()
                .host,
            "::1"
        );
    }

    #[test]
    fn a_plain_request_is_rewritten_to_origin_form_without_proxy_headers() {
        let request = parse_request(
            b"GET http://example.org/index.html HTTP/1.1\r\nHost: example.org\r\nProxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n",
        )
        .unwrap();
        assert_eq!((request.host.as_str(), request.port), ("example.org", 80));
        assert_eq!(
            request.forward_head.as_deref().unwrap(),
            b"GET /index.html HTTP/1.1\r\nHost: example.org\r\nAccept: */*\r\n\r\n"
        );
        assert_eq!(
            parse_request(b"GET http://example.org:8080 HTTP/1.1\r\n\r\n")
                .unwrap()
                .port,
            8080
        );
    }

    #[test]
    fn anything_but_connect_or_an_absolute_target_is_a_bad_request() {
        assert!(parse_request(b"GET /relative HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"GET https://example.org/ HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"nonsense\r\n\r\n").is_err());
        assert!(parse_request(b"\xff\xfe\r\n\r\n").is_err());
    }

    #[tokio::test]
    async fn an_allowed_connect_opens_a_tunnel_and_is_recorded() {
        let (address, _server) = echo_server().await;
        let (setup, recorded) = setup(vec!["127.0.0.1"]);
        let proxy = EgressProxy::start(setup).unwrap();

        let mut stream = UnixStream::connect(proxy.socket_path()).await.unwrap();
        stream
            .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", address.port()).as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line, "HTTP/1.1 200 Connection Established\r\n");
        reader.read_line(&mut line).await.unwrap();
        // Through the tunnel the client speaks to the destination itself.
        let mut stream = reader.into_inner();
        stream
            .write_all(b"GET /tunnelled HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(
            response.ends_with("saw GET /tunnelled HTTP/1.1"),
            "{response}"
        );

        let records = recorded.0.lock().unwrap();
        assert_eq!(
            *records,
            vec![EgressConnection {
                host: "127.0.0.1".into(),
                port: address.port(),
                method: "CONNECT".into(),
                verdict: EgressVerdict::Allowed {
                    basis: "test list".into()
                },
            }]
        );
    }

    #[tokio::test]
    async fn a_plain_request_is_forwarded_in_origin_form() {
        let (address, _server) = echo_server().await;
        let (setup, _) = setup(vec!["127.0.0.1"]);
        let proxy = EgressProxy::start(setup).unwrap();
        let response = send(
            &proxy,
            &format!(
                "GET http://127.0.0.1:{}/plain HTTP/1.1\r\nHost: x\r\n\r\n",
                address.port()
            ),
        )
        .await;
        assert!(response.ends_with("saw GET /plain HTTP/1.1"), "{response}");
    }

    #[tokio::test]
    async fn a_refused_destination_gets_403_and_is_recorded_before_any_connection() {
        let (setup, recorded) = setup(vec![]);
        let proxy = EgressProxy::start(setup).unwrap();
        let response = send(&proxy, "CONNECT example.invalid:443 HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");
        assert!(response.contains("example.invalid:443"), "{response}");
        assert!(response.contains("not on the list"), "{response}");
        let records = recorded.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].verdict,
            EgressVerdict::Refused {
                reason: "not on the list".into()
            }
        );
    }

    #[tokio::test]
    async fn an_unreachable_allowed_destination_is_502_and_a_bad_head_is_400() {
        // A listener that is closed at once leaves a port nothing accepts on.
        let closed = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let (setup, _) = setup(vec!["127.0.0.1"]);
        let proxy = EgressProxy::start(setup).unwrap();
        let response = send(
            &proxy,
            &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"),
        )
        .await;
        assert!(
            response.starts_with("HTTP/1.1 502 Bad Gateway"),
            "{response}"
        );
        let response = send(&proxy, "GET /nowhere HTTP/1.1\r\n\r\n").await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "{response}"
        );
    }

    #[test]
    fn the_bridge_environment_points_every_tool_at_loopback_and_spares_loopback_itself() {
        let bridge = EgressBridge {
            address: "127.0.0.1:3128".parse().unwrap(),
        };
        let environment = bridge.environment();
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert!(
                environment
                    .iter()
                    .any(|(key, value)| *key == name && value == "http://127.0.0.1:3128"),
                "{name} must name the bridge"
            );
        }
        assert!(
            environment
                .iter()
                .any(|(key, value)| *key == "NO_PROXY" && value.contains("127.0.0.1"))
        );
    }
}
