use std::sync::{Arc, Mutex};
use swage_client::{http::GatewayClient, websocket::GatewayWebSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn pairing_survives_a_new_client_and_authenticates_http_without_redeeming_twice() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pairings.json");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            while !buffer.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let size = stream.read(&mut chunk).await.unwrap();
                assert_ne!(size, 0, "request ended before its headers");
                buffer.extend_from_slice(&chunk[..size]);
            }
            let request = String::from_utf8_lossy(&buffer).to_string();
            let authenticated = request
                .to_ascii_lowercase()
                .contains("x-swage-token: remembered-token\r\n");
            let body = if request.starts_with("POST /api/pair ") {
                r#"{"token":"remembered-token"}"#.to_string()
            } else {
                format!(r#"{{"gated":true,"mode":"token","admitted":{authenticated}}}"#)
            };
            captured.lock().unwrap().push(request);
            stream.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut client = GatewayClient::with_saved_pairing(&address, &path).unwrap();
    assert!(!client.access_status().await.unwrap().admitted);
    client.pair_and_remember("1234-5678", &path).await.unwrap();
    assert!(
        client.access_status().await.unwrap().admitted,
        "paired request must carry the token"
    );
    let restored = GatewayClient::with_saved_pairing(&address, &path).unwrap();
    assert!(
        restored.access_status().await.unwrap().admitted,
        "new process client must read its token file"
    );
    assert!(!format!("{restored:?}").contains("remembered-token"));
    assert!(
        GatewayClient::with_saved_pairing("http://other.example", &path)
            .unwrap()
            .token()
            .is_none()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    server.await.unwrap();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("POST /api/pair "))
            .count(),
        1
    );
}

#[tokio::test]
async fn native_websocket_presents_the_token_in_a_header_without_a_query() {
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback's error response type"
    )]
    fn require_token(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        assert_eq!(
            request
                .headers()
                .get("X-Swage-Token")
                .map(|value| value.to_str().unwrap()),
            Some("upgrade-token")
        );
        assert!(request.uri().query().is_none());
        Ok(response)
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_hdr_async(stream, require_token)
            .await
            .unwrap()
    });
    let result =
        GatewayWebSocket::connect_with_token(&address, uuid::Uuid::new_v4(), Some("upgrade-token"))
            .await;
    server.await.unwrap();
    assert!(result.is_ok(), "authenticated upgrade must connect");
}
