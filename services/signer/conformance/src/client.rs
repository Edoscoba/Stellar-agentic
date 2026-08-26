//! Client conformance: a reference server that grades the *client*.
//!
//! Phase 5 asks to "verify the TS `RemoteSigner` and the future Python one".
//! Those are clients, and a server-side suite pointed at them would prove
//! nothing — so this is the mirror image: a server that answers correctly and
//! records whether each request it received was well-formed.
//!
//! ```bash
//! signer-conformance client --port 8099
//! # then, in the client under test:
//! #   new RemoteSigner({ url: 'http://127.0.0.1:8099', token: 'conformance-token' })
//! # exercise getPublicKey / signTransaction / signAuthEntry, then:
//! # the server prints its report on SIGINT
//! ```
//!
//! # Why the HTTP is hand-rolled
//!
//! This has to observe things a framework normalises away — the exact header
//! casing a client sent, whether it set `content-type`, whether it used the
//! documented path. A router that helpfully accepts `/v1/sign/transaction/`
//! and `/V1/Sign/Transaction` would hide precisely the divergences this is
//! looking for.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::Report;

/// The token a client under test must present.
pub const EXPECTED_TOKEN: &str = "conformance-token";

/// The address the reference server signs for, derived from a fixed seed so a
/// client pinning `expectedPublicKey` has something stable to pin.
pub fn reference_address() -> String {
    let key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
    stellar_strkey::ed25519::PublicKey(key.verifying_key().to_bytes()).to_string()
}

/// A running reference server.
pub struct ReferenceServer {
    /// The port it bound.
    pub port: u16,
    findings: Arc<Mutex<Report>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl ReferenceServer {
    /// Bind on `port` (0 for any) and start serving.
    pub async fn start(port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let bound = listener.local_addr()?.port();

        let findings = Arc::new(Mutex::new(Report::default()));
        let seen = Arc::new(Mutex::new(Vec::new()));

        let server_findings = Arc::clone(&findings);
        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let findings = Arc::clone(&server_findings);
                let seen = Arc::clone(&server_seen);
                tokio::spawn(async move {
                    let _ = handle(socket, findings, seen).await;
                });
            }
        });

        Ok(Self {
            port,
            findings,
            seen,
        }
        .with_port(bound))
    }

    fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// The base URL to point a client at.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Which endpoints have been exercised.
    pub fn exercised(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }

    /// The report so far, plus coverage checks for endpoints never called.
    ///
    /// A client that never calls `signAuthEntry` has not been shown to work;
    /// the report says so rather than implying a clean pass.
    pub fn report(&self) -> Report {
        let mut report = Report::default();
        report.absorb(std::mem::take(
            &mut *self.findings.lock().expect("findings"),
        ));

        let exercised = self.exercised();
        for endpoint in [
            "/v1/public-key",
            "/v1/sign/transaction",
            "/v1/sign/auth-entry",
        ] {
            if exercised.iter().any(|seen| seen == endpoint) {
                report.pass(format!("coverage{endpoint}"));
            } else {
                report.skip(format!("coverage{endpoint} (the client never called it)"));
            }
        }
        report
    }
}

struct ParsedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl ParsedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

async fn handle(
    mut socket: TcpStream,
    findings: Arc<Mutex<Report>>,
    seen: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let Some(request) = read_request(&mut socket).await else {
        return Ok(());
    };

    {
        let mut report = findings.lock().expect("findings");
        grade(&request, &mut report);
    }
    seen.lock().expect("seen").push(request.path.clone());

    let (status, body) = respond(&request);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await
}

/// Every check a client's request must satisfy.
fn grade(request: &ParsedRequest, report: &mut Report) {
    let path = request.path.as_str();

    // ── Authorization ───────────────────────────────────────────────────────
    match request.header("authorization") {
        Some(value) => {
            report.check(
                "client/bearer-scheme",
                "the token is sent as `Authorization: Bearer <token>`",
                value.to_ascii_lowercase().starts_with("bearer "),
                format!("Authorization was {value:?}"),
            );
            report.check(
                "client/token-value",
                "the configured token is sent verbatim",
                value.trim_start_matches(|c: char| c != ' ').trim() == EXPECTED_TOKEN,
                format!("Authorization was {value:?}"),
            );
        }
        None => report.fail(
            "client/bearer-scheme",
            "the token is sent as `Authorization: Bearer <token>`",
            "no Authorization header was sent".to_string(),
        ),
    }

    // ── Method and path ─────────────────────────────────────────────────────
    match path {
        "/v1/public-key" => report.check(
            "client/public-key-method",
            "GET {url}/v1/public-key",
            request.method == "GET",
            format!("method was {}", request.method),
        ),
        "/v1/sign/transaction" | "/v1/sign/auth-entry" => {
            report.check(
                format!("client/{}-method", path.trim_start_matches('/')),
                "POST is used for the signing endpoints",
                request.method == "POST",
                format!("method was {}", request.method),
            );
            report.check(
                format!("client/{}-content-type", path.trim_start_matches('/')),
                "a JSON body is sent with `content-type: application/json`",
                request
                    .header("content-type")
                    .is_some_and(|value| value.starts_with("application/json")),
                format!("content-type was {:?}", request.header("content-type")),
            );
        }
        other => report.fail(
            "client/path",
            "only /v1/public-key, /v1/sign/transaction and /v1/sign/auth-entry exist",
            format!("the client requested {other}"),
        ),
    }

    // ── Body shape ──────────────────────────────────────────────────────────
    if path == "/v1/sign/transaction" {
        let body: serde_json::Value =
            serde_json::from_str(&request.body).unwrap_or(serde_json::Value::Null);
        report.check(
            "client/sign-transaction-fields",
            "the body is { \"xdr\": ..., \"networkPassphrase\": ... }",
            body.get("xdr").and_then(|v| v.as_str()).is_some()
                && body
                    .get("networkPassphrase")
                    .and_then(|v| v.as_str())
                    .is_some(),
            format!("body was {}", request.body),
        );
    }

    if path == "/v1/sign/auth-entry" {
        let body: serde_json::Value =
            serde_json::from_str(&request.body).unwrap_or(serde_json::Value::Null);
        report.check(
            "client/sign-auth-entry-fields",
            "the body is { \"authEntryXdr\": ..., \"networkPassphrase\": ..., \
             \"validUntilLedgerSeq\": <number> }",
            body.get("authEntryXdr").and_then(|v| v.as_str()).is_some()
                && body
                    .get("networkPassphrase")
                    .and_then(|v| v.as_str())
                    .is_some()
                && body
                    .get("validUntilLedgerSeq")
                    .and_then(|v| v.as_u64())
                    .is_some(),
            format!("body was {}", request.body),
        );
        report.check(
            "client/valid-until-is-a-number",
            "validUntilLedgerSeq is a JSON number, not a string",
            body.get("validUntilLedgerSeq")
                .is_none_or(serde_json::Value::is_number),
            format!(
                "validUntilLedgerSeq was {:?}",
                body.get("validUntilLedgerSeq")
            ),
        );
    }
}

/// The canned responses. Deliberately valid, so a client under test gets far
/// enough to exercise every endpoint.
fn respond(request: &ParsedRequest) -> (&'static str, String) {
    match request.path.as_str() {
        "/v1/public-key" => (
            "200 OK",
            serde_json::json!({ "publicKey": reference_address() }).to_string(),
        ),
        "/v1/sign/transaction" => {
            let body: serde_json::Value =
                serde_json::from_str(&request.body).unwrap_or(serde_json::Value::Null);
            let echoed = body.get("xdr").and_then(|v| v.as_str()).unwrap_or("AAAA");
            (
                "200 OK",
                serde_json::json!({ "signedXdr": echoed }).to_string(),
            )
        }
        "/v1/sign/auth-entry" => {
            let body: serde_json::Value =
                serde_json::from_str(&request.body).unwrap_or(serde_json::Value::Null);
            let echoed = body
                .get("authEntryXdr")
                .and_then(|v| v.as_str())
                .unwrap_or("AAAA");
            (
                "200 OK",
                serde_json::json!({ "signedAuthEntryXdr": echoed }).to_string(),
            )
        }
        // A client must surface this text — docs/signing.md promises it lands
        // in the SigningError.
        _ => (
            "404 Not Found",
            serde_json::json!({ "error": "no such endpoint" }).to_string(),
        ),
    }
}

async fn read_request(socket: &mut TcpStream) -> Option<ParsedRequest> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.split('?').next()?.to_string();

    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);

    while buffer.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    let body = String::from_utf8_lossy(
        &buffer[header_end..(header_end + content_length).min(buffer.len())],
    )
    .to_string();

    Some(ParsedRequest {
        method,
        path,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, body: &str, auth: Option<&str>) -> ParsedRequest {
        let mut headers = vec![("content-type".into(), "application/json".into())];
        if let Some(auth) = auth {
            headers.push(("authorization".into(), auth.into()));
        }
        ParsedRequest {
            method: method.into(),
            path: path.into(),
            headers,
            body: body.into(),
        }
    }

    #[test]
    fn a_well_formed_client_request_passes_every_check() {
        let mut report = Report::default();
        grade(
            &request(
                "POST",
                "/v1/sign/transaction",
                r#"{"xdr":"AAAA","networkPassphrase":"Test SDF Network ; September 2015"}"#,
                Some(&format!("Bearer {EXPECTED_TOKEN}")),
            ),
            &mut report,
        );
        assert!(report.is_conformant(), "{report}");
    }

    #[test]
    fn a_client_that_forgets_the_bearer_scheme_is_caught() {
        let mut report = Report::default();
        grade(
            &request("GET", "/v1/public-key", "", Some(EXPECTED_TOKEN)),
            &mut report,
        );
        assert!(!report.is_conformant());
        assert!(report
            .failed
            .iter()
            .any(|f| f.check == "client/bearer-scheme"));
    }

    #[test]
    fn a_client_sending_no_credential_is_caught() {
        let mut report = Report::default();
        grade(&request("GET", "/v1/public-key", "", None), &mut report);
        assert!(report
            .failed
            .iter()
            .any(|f| f.check == "client/bearer-scheme"));
    }

    #[test]
    fn snake_case_body_fields_are_caught() {
        // The protocol is camelCase; a client sending snake_case would work
        // against a lenient server and fail against a strict one.
        let mut report = Report::default();
        grade(
            &request(
                "POST",
                "/v1/sign/transaction",
                r#"{"xdr":"AAAA","network_passphrase":"n"}"#,
                Some(&format!("Bearer {EXPECTED_TOKEN}")),
            ),
            &mut report,
        );
        assert!(report
            .failed
            .iter()
            .any(|f| f.check == "client/sign-transaction-fields"));
    }

    #[test]
    fn a_stringified_valid_until_is_caught() {
        // JSON has numbers; sending "123456" is the kind of thing that works
        // until it meets a strictly-typed server.
        let mut report = Report::default();
        grade(
            &request(
                "POST",
                "/v1/sign/auth-entry",
                r#"{"authEntryXdr":"AAAA","networkPassphrase":"n","validUntilLedgerSeq":"123"}"#,
                Some(&format!("Bearer {EXPECTED_TOKEN}")),
            ),
            &mut report,
        );
        assert!(report
            .failed
            .iter()
            .any(|f| f.check == "client/valid-until-is-a-number"));
    }

    #[test]
    fn an_unknown_path_is_caught() {
        let mut report = Report::default();
        grade(
            &request(
                "GET",
                "/v2/public-key",
                "",
                Some(&format!("Bearer {EXPECTED_TOKEN}")),
            ),
            &mut report,
        );
        assert!(report.failed.iter().any(|f| f.check == "client/path"));
    }

    #[test]
    fn the_reference_address_is_a_valid_stellar_key() {
        assert!(stellar_strkey::ed25519::PublicKey::from_string(&reference_address()).is_ok());
    }

    #[tokio::test]
    async fn the_reference_server_answers_and_reports_coverage() {
        let server = ReferenceServer::start(0).await.unwrap();

        let client = reqwest::Client::new();
        let response = client
            .get(format!("{}/v1/public-key", server.url()))
            .bearer_auth(EXPECTED_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["publicKey"], reference_address());

        let report = server.report();
        assert!(report.is_conformant(), "{report}");
        // The endpoints the client never touched are reported as untested
        // rather than silently passing.
        assert!(report
            .skipped
            .iter()
            .any(|check| check.contains("/v1/sign/transaction")));
    }

    #[tokio::test]
    async fn the_reference_server_grades_a_malformed_client() {
        let server = ReferenceServer::start(0).await.unwrap();

        // No Authorization header at all.
        let _ = reqwest::Client::new()
            .get(format!("{}/v1/public-key", server.url()))
            .send()
            .await
            .unwrap();

        let report = server.report();
        assert!(!report.is_conformant(), "{report}");
    }
}
