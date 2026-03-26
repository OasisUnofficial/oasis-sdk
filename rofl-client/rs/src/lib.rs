// rofl-client/rs/src/lib.rs
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::TcpStream,
    path::Path,
};

const DEFAULT_SOCKET: &str = "/run/rofl-appd.sock";
const DEFAULT_HTTP_PORT: u16 = 80;
const HTTP_SCHEME: &str = "http://";
const HTTPS_SCHEME: &str = "https://";
const LOCALHOST_HOST: &str = "localhost";

#[derive(Clone)]
pub struct RoflClient {
    transport: Transport,
}

#[derive(Clone)]
enum Transport {
    UnixSocket { socket_path: String },
    Http(HttpTransport),
}

#[derive(Clone)]
struct HttpTransport {
    connect_target: String,
    host_header: String,
    base_path: String,
}

impl RoflClient {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_socket_path(DEFAULT_SOCKET)
    }

    pub fn with_socket_path<P: AsRef<Path>>(
        socket_path: P,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let socket_path = socket_path.as_ref().to_string_lossy().to_string();
        if !Path::new(&socket_path).exists() {
            return Err(format!("Socket not found at: {socket_path}").into());
        }
        Ok(Self {
            transport: Transport::UnixSocket { socket_path },
        })
    }

    pub fn with_url(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        if let Some(url) = url.strip_prefix(HTTP_SCHEME) {
            let transport = HttpTransport::parse(url)?;
            return Ok(Self {
                transport: Transport::Http(transport),
            });
        }
        if url.starts_with(HTTPS_SCHEME) {
            return Err(
                "HTTPS transport is not supported by oasis-rofl-client; use http:// or a Unix socket path"
                    .into(),
            );
        }

        Self::with_socket_path(url)
    }

    async fn blocking<T, F>(&self, f: F) -> Result<T, Box<dyn std::error::Error>>
    where
        T: Send + 'static,
        F: FnOnce(Transport) -> std::io::Result<T> + Send + 'static,
    {
        let transport = self.transport.clone();
        tokio::task::spawn_blocking(move || f(transport))
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
    }

    // GET /rofl/v1/app/id
    pub async fn get_app_id(&self) -> Result<String, Box<dyn std::error::Error>> {
        self.blocking(|transport| {
            let body = transport.request("GET", "/rofl/v1/app/id", None, None)?;
            let s = String::from_utf8(body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(s.trim().to_string())
        })
        .await
    }

    // POST /rofl/v1/keys/generate
    pub async fn generate_key(
        &self,
        key_id: &str,
        kind: KeyKind,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let req = serde_json::to_vec(&KeyGenerationRequest {
            key_id: key_id.to_string(),
            kind: kind.to_string(),
        })?;
        self.blocking(move |transport| {
            let body = transport.request(
                "POST",
                "/rofl/v1/keys/generate",
                Some(&req),
                Some("application/json"),
            )?;
            let resp: KeyGenerationResponse = serde_json::from_slice(&body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(resp.key)
        })
        .await
    }

    // POST /rofl/v1/tx/sign-submit
    pub async fn sign_submit(
        &self,
        tx: Tx,
        encrypt: Option<bool>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let req = serde_json::to_vec(&SignSubmitRequest { tx, encrypt })?;
        self.blocking(move |transport| {
            let body = transport.request(
                "POST",
                "/rofl/v1/tx/sign-submit",
                Some(&req),
                Some("application/json"),
            )?;
            let resp: SignSubmitResponse = serde_json::from_slice(&body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(resp.data)
        })
        .await
    }

    // GET /rofl/v1/metadata
    pub async fn get_metadata(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, Box<dyn std::error::Error>> {
        self.blocking(|transport| {
            let body = transport.request("GET", "/rofl/v1/metadata", None, None)?;
            let resp: std::collections::HashMap<String, String> = serde_json::from_slice(&body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(resp)
        })
        .await
    }

    // POST /rofl/v1/metadata
    pub async fn set_metadata(
        &self,
        metadata: &std::collections::HashMap<String, String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let req = serde_json::to_vec(metadata)?;
        self.blocking(move |transport| {
            transport.request(
                "POST",
                "/rofl/v1/metadata",
                Some(&req),
                Some("application/json"),
            )?;
            Ok(())
        })
        .await
    }

    // POST /rofl/v1/query
    pub async fn query(
        &self,
        method: &str,
        args: &[u8],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let payload = serde_json::json!({
            "method": method,
            "args": hex::encode(args),
        });
        let req = serde_json::to_vec(&payload)?;
        self.blocking(move |transport| {
            let body = transport.request(
                "POST",
                "/rofl/v1/query",
                Some(&req),
                Some("application/json"),
            )?;
            let resp: serde_json::Value = serde_json::from_slice(&body)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            let data_hex = resp.get("data").and_then(|v| v.as_str()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing 'data' field")
            })?;
            let data = hex::decode(data_hex)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(data)
        })
        .await
    }

    /// Convenience helper for ETH-style call
    pub async fn sign_submit_eth(
        &self,
        gas_limit: u64,
        to: &str,
        value: &str,
        data_hex: &str,
        encrypt: Option<bool>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let eth = EthCall {
            gas_limit,
            to: to.to_string(),
            value: value.to_string(),
            data: data_hex.to_string(),
        };
        self.sign_submit(Tx::Eth(eth), encrypt).await
    }
}

impl Transport {
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        content_type: Option<&str>,
    ) -> std::io::Result<Vec<u8>> {
        match self {
            Self::UnixSocket { socket_path } => {
                unix_socket_request(socket_path, method, path, body, content_type)
            }
            Self::Http(http) => {
                let stream = TcpStream::connect(&http.connect_target)?;
                let request_path = http.request_path(path)?;
                http_request(
                    stream,
                    &http.host_header,
                    method,
                    &request_path,
                    body,
                    content_type,
                )
            }
        }
    }
}

impl HttpTransport {
    fn parse(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        if url.is_empty() {
            return Err("HTTP URL must include a host".into());
        }

        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let authority = &url[..authority_end];
        let suffix = &url[authority_end..];

        if authority.is_empty() {
            return Err("HTTP URL must include a host".into());
        }
        if authority.chars().any(char::is_whitespace) {
            return Err("HTTP URL must not contain whitespace in the authority".into());
        }
        if authority.contains('@') {
            return Err("HTTP URL must not include user info".into());
        }
        if suffix.starts_with('?') || suffix.starts_with('#') {
            return Err("HTTP URL must not include a query string or fragment".into());
        }
        if suffix.contains('?') || suffix.contains('#') {
            return Err("HTTP URL base path must not include a query string or fragment".into());
        }

        let connect_target = normalize_connect_target(authority)?;
        let base_path = normalize_base_path(suffix)?;

        Ok(Self {
            connect_target,
            host_header: authority.to_string(),
            base_path,
        })
    }

    fn request_path(&self, path: &str) -> std::io::Result<String> {
        if !path.starts_with('/') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ROFL endpoint path must start with '/'",
            ));
        }

        if self.base_path.is_empty() {
            return Ok(path.to_string());
        }

        Ok(format!("{}{}", self.base_path, path))
    }
}

fn normalize_connect_target(authority: &str) -> Result<String, Box<dyn std::error::Error>> {
    if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return Err("HTTP URL contains an invalid IPv6 host".into());
        };
        if end == 1 {
            return Err("HTTP URL must include a host".into());
        }
        let suffix = &authority[end + 1..];
        return Ok(match suffix {
            "" => format!("{authority}:{DEFAULT_HTTP_PORT}"),
            _ if suffix.starts_with(':') => {
                validate_port(&suffix[1..])?;
                authority.to_string()
            }
            _ => return Err("HTTP URL contains an invalid IPv6 host".into()),
        });
    }

    let colon_count = authority.matches(':').count();
    match colon_count {
        0 => Ok(format!("{authority}:{DEFAULT_HTTP_PORT}")),
        1 => {
            let Some((host, port)) = authority.split_once(':') else {
                return Err("HTTP URL contains an invalid authority".into());
            };
            if host.is_empty() {
                return Err("HTTP URL must include a host".into());
            }
            validate_port(port)?;
            Ok(authority.to_string())
        }
        _ => Err("HTTP URL contains an invalid host; IPv6 addresses must be bracketed".into()),
    }
}

fn validate_port(port: &str) -> Result<(), Box<dyn std::error::Error>> {
    if port.is_empty() {
        return Err("HTTP URL must include a numeric port after ':'".into());
    }
    port.parse::<u16>()
        .map(|_| ())
        .map_err(|_| "HTTP URL contains an invalid port".into())
}

fn normalize_base_path(path: &str) -> Result<String, Box<dyn std::error::Error>> {
    if path.is_empty() || path == "/" {
        return Ok(String::new());
    }
    if !path.starts_with('/') {
        return Err("HTTP URL base path must start with '/'".into());
    }

    Ok(path.trim_end_matches('/').to_string())
}

#[cfg(unix)]
fn unix_socket_request(
    socket_path: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> std::io::Result<Vec<u8>> {
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket_path)?;
    http_request(stream, LOCALHOST_HOST, method, path, body, content_type)
}

#[cfg(not(unix))]
fn unix_socket_request(
    _socket_path: &str,
    _method: &str,
    _path: &str,
    _body: Option<&[u8]>,
    _content_type: Option<&str>,
) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::other(
        "Unix domain socket transport is not supported on this platform; use HTTP transport instead",
    ))
}

// Blocking HTTP/1.1 request over any synchronous stream.
fn http_request<S: Read + Write>(
    mut stream: S,
    host: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> std::io::Result<Vec<u8>> {
    use std::io::{Error, ErrorKind};

    let mut req = Vec::new();
    req.extend_from_slice(format!("{method} {path} HTTP/1.1\r\n").as_bytes());
    req.extend_from_slice(format!("Host: {host}\r\n").as_bytes());
    req.extend_from_slice(b"Connection: close\r\n");
    if let Some(ct) = content_type {
        req.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
    }
    if let Some(b) = body {
        req.extend_from_slice(format!("Content-Length: {}\r\n", b.len()).as_bytes());
    }
    req.extend_from_slice(b"\r\n");
    if let Some(b) = body {
        req.extend_from_slice(b);
    }

    stream.write_all(&req)?;
    stream.flush()?;

    let mut resp = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        resp.extend_from_slice(&buf[..n]);
    }

    let header_end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "Invalid HTTP response: no header/body delimiter",
            )
        })?;
    let (head, body_bytes) = resp.split_at(header_end + 4);

    let mut lines = head.split(|&b| b == b'\n');
    let status_line = lines
        .next()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "Invalid HTTP response: empty"))?;
    let status_str = String::from_utf8(status_line.to_vec())
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    let code: u16 = status_str
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "Invalid HTTP status line"))?
        .parse()
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;

    if !(200..300).contains(&code) {
        let msg = String::from_utf8_lossy(body_bytes).to_string();
        return Err(Error::other(format!("HTTP {code} error: {msg}")));
    }

    Ok(body_bytes.to_vec())
}

// See https://github.com/oasisprotocol/oasis-sdk/blob/1ae8882b05d10a44398e52b5b8c56ab2086f81b3/rofl-appd/src/services/kms.rs#L59-L74
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyKind {
    Raw256,
    Raw384,
    Ed25519,
    Secp256k1,
}

impl std::fmt::Display for KeyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyKind::Raw256 => write!(f, "raw-256"),
            KeyKind::Raw384 => write!(f, "raw-384"),
            KeyKind::Ed25519 => write!(f, "ed25519"),
            KeyKind::Secp256k1 => write!(f, "secp256k1"),
        }
    }
}

#[derive(Debug, Serialize)]
struct KeyGenerationRequest {
    key_id: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct KeyGenerationResponse {
    key: String,
}

// -------------------- sign-submit types --------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum Tx {
    #[serde(rename = "eth")]
    Eth(EthCall),
    #[serde(rename = "std")]
    Std(String), // CBOR-serialized hex-encoded Transaction
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EthCall {
    pub gas_limit: u64,
    pub to: String,
    pub value: String,
    pub data: String, // hex string without 0x prefix
}

#[derive(Debug, Serialize)]
struct SignSubmitRequest {
    pub tx: Tx,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypt: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SignSubmitResponse {
    pub data: String, // CBOR-serialized hex-encoded call result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    #[cfg(unix)]
    use tempfile::TempDir;

    #[cfg(unix)]
    fn setup_mock_uds_server(responses: Vec<(String, String)>) -> (TempDir, String) {
        let temp_dir = TempDir::new().unwrap();
        let socket_path = temp_dir.path().join("test.sock");
        let socket_path_str = socket_path.to_string_lossy().to_string();

        let listener = UnixListener::bind(&socket_path).unwrap();

        thread::spawn(move || serve_requests(listener, responses));

        thread::sleep(std::time::Duration::from_millis(100));

        (temp_dir, socket_path_str)
    }

    fn setup_mock_tcp_server(responses: Vec<(String, String)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        thread::spawn(move || serve_requests(listener, responses));

        thread::sleep(std::time::Duration::from_millis(100));

        format!("http://{address}")
    }

    fn serve_requests<L>(listener: L, responses: Vec<(String, String)>)
    where
        L: AcceptOnce,
    {
        for (expected_path, response) in responses {
            if let Some(mut stream) = listener.accept_one() {
                let mut buf = vec![0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..n]);
                assert!(request.contains(&expected_path));

                let http_response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                    response.len(),
                    response
                );
                stream.write_all(http_response.as_bytes()).unwrap();
            }
        }
    }

    trait AcceptOnce {
        type Stream: Read + Write;

        fn accept_one(&self) -> Option<Self::Stream>;
    }

    #[cfg(unix)]
    impl AcceptOnce for UnixListener {
        type Stream = std::os::unix::net::UnixStream;

        fn accept_one(&self) -> Option<Self::Stream> {
            self.accept().ok().map(|(stream, _)| stream)
        }
    }

    impl AcceptOnce for TcpListener {
        type Stream = std::net::TcpStream;

        fn accept_one(&self) -> Option<Self::Stream> {
            self.accept().ok().map(|(stream, _)| stream)
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_get_app_id() {
        let (_temp_dir, socket_path) = setup_mock_uds_server(vec![(
            "/rofl/v1/app/id".to_string(),
            "oasis1qr677rv0dcnh7ys4yanlynysvnjtk9gnsyhvm5wj".to_string(),
        )]);

        let client = RoflClient::with_socket_path(&socket_path).unwrap();
        let app_id = client.get_app_id().await.unwrap();

        assert_eq!(app_id, "oasis1qr677rv0dcnh7ys4yanlynysvnjtk9gnsyhvm5wj");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_generate_key() {
        let response = r#"{"key":"0x123456789abcdef"}"#;
        let (_temp_dir, socket_path) = setup_mock_uds_server(vec![(
            "/rofl/v1/keys/generate".to_string(),
            response.to_string(),
        )]);

        let client = RoflClient::with_socket_path(&socket_path).unwrap();
        let key = client
            .generate_key("test-key-id", KeyKind::Secp256k1)
            .await
            .unwrap();

        assert_eq!(key, "0x123456789abcdef");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_get_metadata() {
        let response = r#"{"key1":"value1","key2":"value2"}"#;
        let (_temp_dir, socket_path) = setup_mock_uds_server(vec![(
            "/rofl/v1/metadata".to_string(),
            response.to_string(),
        )]);

        let client = RoflClient::with_socket_path(&socket_path).unwrap();
        let metadata = client.get_metadata().await.unwrap();

        assert_eq!(metadata.get("key1").unwrap(), "value1");
        assert_eq!(metadata.get("key2").unwrap(), "value2");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_set_metadata() {
        let (_temp_dir, socket_path) =
            setup_mock_uds_server(vec![("/rofl/v1/metadata".to_string(), "".to_string())]);

        let client = RoflClient::with_socket_path(&socket_path).unwrap();
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("new_key".to_string(), "new_value".to_string());

        client.set_metadata(&metadata).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_query() {
        let response = r#"{"data":"48656c6c6f"}"#;
        let (_temp_dir, socket_path) =
            setup_mock_uds_server(vec![("/rofl/v1/query".to_string(), response.to_string())]);

        let client = RoflClient::with_socket_path(&socket_path).unwrap();
        let args = b"\xa1\x64test\x65value";
        let result = client.query("test.Method", args).await.unwrap();

        assert_eq!(result, b"Hello");
    }

    #[tokio::test]
    async fn test_http_transport_get_app_id() {
        let url = setup_mock_tcp_server(vec![(
            "/rofl/v1/app/id".to_string(),
            "oasis1qr677rv0dcnh7ys4yanlynysvnjtk9gnsyhvm5wj".to_string(),
        )]);

        let client = RoflClient::with_url(&url).unwrap();
        let app_id = client.get_app_id().await.unwrap();

        assert_eq!(app_id, "oasis1qr677rv0dcnh7ys4yanlynysvnjtk9gnsyhvm5wj");
    }

    #[tokio::test]
    async fn test_http_transport_preserves_base_path() {
        let base_url = setup_mock_tcp_server(vec![(
            "/prefix/rofl/v1/keys/generate".to_string(),
            r#"{"key":"0x123456789abcdef"}"#.to_string(),
        )]);

        let client = RoflClient::with_url(&format!("{base_url}/prefix/")).unwrap();
        let key = client
            .generate_key("test-key-id", KeyKind::Secp256k1)
            .await
            .unwrap();

        assert_eq!(key, "0x123456789abcdef");
    }

    #[tokio::test]
    async fn test_http_transport_uses_default_port() {
        let transport = HttpTransport::parse("example.com").unwrap();
        assert_eq!(transport.connect_target, "example.com:80");
        assert_eq!(transport.host_header, "example.com");
        assert_eq!(transport.base_path, "");
    }

    #[tokio::test]
    async fn test_with_url_rejects_https() {
        let err = match RoflClient::with_url("https://localhost:8549") {
            Ok(_) => panic!("expected https URL to be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("HTTPS transport is not supported"));
    }

    #[tokio::test]
    async fn test_with_url_rejects_query_and_fragment() {
        let err = match RoflClient::with_url("http://localhost:8549/prefix?test=true") {
            Ok(_) => panic!("expected URL with query string to be rejected"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("must not include a query string or fragment"));

        let err = match RoflClient::with_url("http://localhost:8549#fragment") {
            Ok(_) => panic!("expected URL with fragment to be rejected"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("must not include a query string or fragment"));
    }

    #[tokio::test]
    async fn test_http_url_without_host_is_rejected() {
        let err = match RoflClient::with_url("http://") {
            Ok(_) => panic!("expected URL without host to be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("must include a host"));
    }

    #[tokio::test]
    async fn test_with_url_rejects_invalid_authority() {
        let err = match RoflClient::with_url("http://localhost:") {
            Ok(_) => panic!("expected URL with empty port to be rejected"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("must include a numeric port after ':'"));

        let err = match RoflClient::with_url("http://:8549") {
            Ok(_) => panic!("expected URL with empty host to be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("must include a host"));

        let err = match RoflClient::with_url("http://localhost:abc") {
            Ok(_) => panic!("expected URL with non-numeric port to be rejected"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("contains an invalid port"));
    }

    #[tokio::test]
    async fn test_bad_socket_path() {
        let result = RoflClient::with_socket_path("/non/existent/socket.sock");
        assert!(result.is_err());
    }
}
