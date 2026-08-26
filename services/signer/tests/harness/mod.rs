//! A service wired up for tests, driven through the real axum router.
//!
//! Requests go through routing, the body limit, the auth header parsing and the
//! error rendering — not straight into [`SignerService`]. A test that calls the
//! service struct directly proves nothing about whether the HTTP surface an
//! agent actually talks to behaves the same way.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use stellaragent_signer::audit::{AuditLog, MemorySink, Sink};
use stellaragent_signer::auth::{token_hash, Subject, TokenRecord, TokenStore};
use stellaragent_signer::backend::local::{LocalKeystore, BACKEND_ID};
use stellaragent_signer::backend::{BackendRegistry, KeyRef};
use stellaragent_signer::http::{router, AppState};
use stellaragent_signer::ledger::{LedgerClock, RatchetingClock};
use stellaragent_signer::metrics::Metrics;
use stellaragent_signer::policy::{Policy, PolicySet, RateLimitState};
use stellaragent_signer::registry::{KeyRegistry, PolicyName, Registration};
use stellaragent_signer::sign::SignerService;
use stellaragent_signer::testing;

/// The token the fixture agent presents.
pub const TOKEN: &str = "an-entirely-ordinary-test-token";

/// The identity it authenticates as.
pub const SUBJECT: &str = "agent-1";

/// A shared in-memory audit sink, so a test can read what was recorded.
pub struct SharedSink(pub Arc<MemorySink>);

impl Sink for SharedSink {
    fn append(&self, line: &str) -> std::io::Result<()> {
        self.0.append(line)
    }
}

/// A service, its router, and the pieces a test wants to inspect.
pub struct Harness {
    /// The router requests go through.
    pub app: Router,
    /// What was written to the audit log.
    pub audit: Arc<MemorySink>,
    /// The service, for direct inspection where a test needs it.
    pub service: Arc<SignerService>,
}

/// The permissive policy the fixtures are built against.
pub fn permissive_policy() -> Policy {
    Policy {
        networks: vec![testing::NETWORK_PASSPHRASE.into()],
        max_amount_stroops: Some("100000000".into()),
        max_transaction_stroops: Some("500000000".into()),
        recipients: vec![testing::RECIPIENT.clone()],
        contracts: vec![testing::CONTRACT.clone()],
        functions: vec!["pay".into(), "set_limits".into()],
        ..Policy::default()
    }
}

/// Build a harness with `policy`, an unexpired token, and a local keystore
/// holding the agent's key.
pub fn harness(policy: Policy) -> Harness {
    harness_with_clock(policy, Arc::new(RatchetingClock::new(1_000, 1_000)))
}

/// Build a harness with an explicit ledger clock.
pub fn harness_with_clock(policy: Policy, clock: Arc<dyn LedgerClock>) -> Harness {
    let keystore = LocalKeystore::single("k1", testing::AGENT_SEED);
    let backends = BackendRegistry::new(vec![Box::new(keystore)]);

    let mut entries = HashMap::new();
    entries.insert(
        Subject::new(SUBJECT),
        Registration {
            key: KeyRef::new(BACKEND_ID, "k1"),
            policy: PolicyName::new("default"),
        },
    );
    let registry = KeyRegistry::new(entries);

    let mut policies = HashMap::new();
    policies.insert(PolicyName::new("default"), policy);
    let policies = PolicySet::new(policies).expect("the fixture policy is valid");

    let tokens = TokenStore::new(vec![TokenRecord {
        id: "t1".into(),
        subject: Subject::new(SUBJECT),
        token_sha256: token_hash(TOKEN),
        label: "test".into(),
        expires_at: None,
        revoked_at: None,
    }])
    .expect("the fixture token store is valid");

    let audit = Arc::new(MemorySink::new());
    let service = Arc::new(SignerService {
        backends,
        registry,
        policies,
        tokens,
        audit: AuditLog::new(Box::new(SharedSink(Arc::clone(&audit)))),
        metrics: Arc::new(Metrics::new()),
        rate_limits: RateLimitState::new(),
        clock,
    });

    let app = router(
        AppState {
            service: Arc::clone(&service),
        },
        64 * 1024,
        std::time::Duration::from_secs(30),
    );

    Harness {
        app,
        audit,
        service,
    }
}

/// A response, decomposed for assertions.
pub struct Reply {
    /// The status code.
    pub status: StatusCode,
    /// The parsed JSON body, when there was one.
    pub body: serde_json::Value,
    /// The `x-request-id` header, if present.
    pub request_id: Option<String>,
}

impl Reply {
    /// The `error` field of a refusal body.
    pub fn error(&self) -> &str {
        self.body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    /// The `reason` field of a refusal body.
    pub fn reason(&self) -> &str {
        self.body
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    /// The rules named in `violations`.
    pub fn violated_rules(&self) -> Vec<String> {
        self.body
            .get("violations")
            .and_then(|v| v.as_array())
            .map(|items| {
                let mut rules: Vec<String> = items
                    .iter()
                    .filter_map(|item| item.get("rule").and_then(|r| r.as_str()))
                    .map(str::to_string)
                    .collect();
                rules.sort();
                rules.dedup();
                rules
            })
            .unwrap_or_default()
    }
}

impl Harness {
    /// Send a request with the fixture token.
    pub async fn send(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Reply {
        self.send_with_token(method, path, body, Some(TOKEN)).await
    }

    /// Send a request with an explicit credential (or none).
    pub async fn send_with_token(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        token: Option<&str>,
    ) -> Reply {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .expect("a valid request"),
            None => builder.body(Body::empty()).expect("a valid request"),
        };

        self.dispatch(request).await
    }

    /// Send a fully-constructed request.
    pub async fn dispatch(&self, request: Request<Body>) -> Reply {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers");

        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("a readable body")
            .to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

        Reply {
            status,
            body,
            request_id,
        }
    }

    /// Ask for the signing address.
    pub async fn public_key(&self) -> Reply {
        self.send("GET", "/v1/public-key", None).await
    }

    /// Ask for a transaction signature.
    pub async fn sign_transaction(&self, xdr: &str) -> Reply {
        self.sign_transaction_on(xdr, testing::NETWORK_PASSPHRASE)
            .await
    }

    /// Ask for a transaction signature on a named network.
    pub async fn sign_transaction_on(&self, xdr: &str, network: &str) -> Reply {
        self.send(
            "POST",
            "/v1/sign/transaction",
            Some(serde_json::json!({ "xdr": xdr, "networkPassphrase": network })),
        )
        .await
    }

    /// Ask for an auth-entry signature.
    pub async fn sign_auth_entry(&self, xdr: &str, valid_until: u32) -> Reply {
        self.send(
            "POST",
            "/v1/sign/auth-entry",
            Some(serde_json::json!({
                "authEntryXdr": xdr,
                "networkPassphrase": testing::NETWORK_PASSPHRASE,
                "validUntilLedgerSeq": valid_until,
            })),
        )
        .await
    }

    /// Every audit record written so far.
    pub fn records(&self) -> Vec<stellaragent_signer::audit::AuditRecord> {
        self.audit.records()
    }

    /// The most recent audit record.
    pub fn last_record(&self) -> stellaragent_signer::audit::AuditRecord {
        self.records().pop().expect("at least one audit record")
    }
}

/// Verify a signed envelope really carries a valid signature from `address`.
pub fn envelope_signature_verifies(signed_xdr: &str, address: &str, network: &str) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use stellar_xdr::curr::{Limits, ReadXdr, TransactionEnvelope};

    let Ok(TransactionEnvelope::Tx(envelope)) =
        TransactionEnvelope::from_xdr_base64(signed_xdr, Limits::none())
    else {
        return false;
    };
    let Some(public) = stellaragent_signer::stellar::public_key_from_address(address) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&public) else {
        return false;
    };
    let Ok(payload) =
        stellaragent_signer::stellar::transaction_signing_payload(&envelope.tx, network)
    else {
        return false;
    };

    envelope.signatures.iter().any(|decorated| {
        <[u8; 64]>::try_from(decorated.signature.0.as_slice())
            .map(|bytes| {
                verifying
                    .verify(&payload, &Signature::from_bytes(&bytes))
                    .is_ok()
            })
            .unwrap_or(false)
    })
}
