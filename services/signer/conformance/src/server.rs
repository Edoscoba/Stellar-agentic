//! Server conformance: point this at a running service.
//!
//! Every check quotes the clause of `docs/signing.md` it enforces, so a
//! failure tells an implementer what to change.
//!
//! # What it deliberately does not do
//!
//! It does not check that a *policy* is correct — policy is an operator's
//! choice, not the protocol's. It checks the protocol: paths, methods, status
//! codes, body shapes, and the authentication rules. A service that refuses
//! everything on policy grounds still conforms.

use crate::Report;

/// What the suite needs to talk to a service.
pub struct Target {
    /// Base URL, e.g. `https://signer.internal`.
    pub url: String,
    /// A token the service will accept.
    pub token: String,
    /// A token the service must reject — revoked, expired, or invented.
    pub invalid_token: String,
    /// An unsigned base64 transaction envelope the service's policy permits.
    ///
    /// Optional: without it the signing checks are skipped rather than
    /// guessed at, because a fixture that policy refuses would make a
    /// conforming service look broken.
    pub signable_envelope: Option<String>,
    /// The network passphrase that goes with it.
    pub network_passphrase: String,
}

impl Target {
    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.url.trim_end_matches('/'))
    }
}

/// Run every server check.
pub async fn run(target: &Target) -> Report {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("an HTTP client");

    let mut report = Report::default();

    check_public_key(&client, target, &mut report).await;
    check_authentication(&client, target, &mut report).await;
    check_malformed_bodies(&client, target, &mut report).await;
    check_health(&client, target, &mut report).await;
    check_signing(&client, target, &mut report).await;

    report
}

async fn check_public_key(client: &reqwest::Client, target: &Target, report: &mut Report) {
    let response = client
        .get(target.endpoint("/v1/public-key"))
        .bearer_auth(&target.token)
        .send()
        .await;

    let Ok(response) = response else {
        report.fail(
            "public-key/reachable",
            "GET {url}/v1/public-key must be reachable",
            format!("{:?}", response.err()),
        );
        return;
    };

    let status = response.status();
    report.check(
        "public-key/200",
        "GET /v1/public-key returns 200 with a valid credential",
        status == 200,
        format!("status {status}"),
    );

    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    let address = body.get("publicKey").and_then(|v| v.as_str());

    report.check(
        "public-key/shape",
        "the response body is { \"publicKey\": \"G...\" }",
        address.is_some(),
        format!("body was {body}"),
    );

    if let Some(address) = address {
        report.check(
            "public-key/strkey",
            "publicKey is a valid Stellar ed25519 public key",
            stellar_strkey::ed25519::PublicKey::from_string(address).is_ok(),
            format!("publicKey was {address:?}"),
        );
    }
}

async fn check_authentication(client: &reqwest::Client, target: &Target, report: &mut Report) {
    // No credential at all.
    let response = client.get(target.endpoint("/v1/public-key")).send().await;
    match response {
        Ok(response) => {
            let status = response.status();
            report.check(
                "auth/missing-credential",
                "a request with no bearer token is refused with 401",
                status == 401,
                format!("status {status}"),
            );
        }
        Err(error) => report.fail(
            "auth/missing-credential",
            "the service answers",
            error.to_string(),
        ),
    }

    // A credential the service should not accept.
    let response = client
        .get(target.endpoint("/v1/public-key"))
        .bearer_auth(&target.invalid_token)
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status();
            report.check(
                "auth/invalid-credential",
                "an unknown or revoked token is refused with 401",
                status == 401,
                format!("status {status}"),
            );

            let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
            report.check(
                "auth/error-body",
                "a non-2xx response carries { \"error\": \"<message>\" }",
                body.get("error").and_then(|v| v.as_str()).is_some(),
                format!("body was {body}"),
            );
        }
        Err(error) => report.fail(
            "auth/invalid-credential",
            "the service answers",
            error.to_string(),
        ),
    }
}

async fn check_malformed_bodies(client: &reqwest::Client, target: &Target, report: &mut Report) {
    let cases = [
        (
            "sign-transaction/empty-body",
            "/v1/sign/transaction",
            serde_json::json!({}),
        ),
        (
            "sign-transaction/missing-passphrase",
            "/v1/sign/transaction",
            serde_json::json!({ "xdr": "AAAA" }),
        ),
        (
            "sign-transaction/bad-xdr",
            "/v1/sign/transaction",
            serde_json::json!({ "xdr": "not-xdr", "networkPassphrase": target.network_passphrase }),
        ),
        (
            "sign-auth-entry/missing-valid-until",
            "/v1/sign/auth-entry",
            serde_json::json!({
                "authEntryXdr": "AAAA",
                "networkPassphrase": target.network_passphrase
            }),
        ),
    ];

    for (name, path, body) in cases {
        let response = client
            .post(target.endpoint(path))
            .bearer_auth(&target.token)
            .json(&body)
            .send()
            .await;

        match response {
            Ok(response) => {
                let status = response.status();
                report.check(
                    name,
                    "a request that is not a valid protocol message is refused with a 4xx",
                    status.is_client_error(),
                    format!("status {status}"),
                );

                let parsed: serde_json::Value =
                    response.json().await.unwrap_or(serde_json::Value::Null);
                report.check(
                    format!("{name}/error-body"),
                    "a non-2xx response carries { \"error\": \"<message>\" }",
                    parsed.get("error").and_then(|v| v.as_str()).is_some(),
                    format!("body was {parsed}"),
                );
            }
            Err(error) => report.fail(name, "the service answers", error.to_string()),
        }
    }
}

async fn check_health(client: &reqwest::Client, target: &Target, report: &mut Report) {
    // Health endpoints are an addition to docs/signing.md rather than part of
    // it, so a service without them is not non-conformant — the check is
    // recorded as skipped rather than failed.
    for path in ["/healthz", "/readyz"] {
        match client.get(target.endpoint(path)).send().await {
            Ok(response) if response.status() == 404 => {
                report.skip(format!("health{path}"));
            }
            Ok(response) => {
                let status = response.status();
                report.check(
                    format!("health{path}"),
                    "a health endpoint, if present, answers without a credential",
                    status.is_success() || status == 503,
                    format!("status {status}"),
                );
            }
            Err(error) => report.fail(
                format!("health{path}"),
                "the service answers",
                error.to_string(),
            ),
        }
    }
}

async fn check_signing(client: &reqwest::Client, target: &Target, report: &mut Report) {
    let Some(envelope) = &target.signable_envelope else {
        // Skipped rather than guessed: an envelope the service's policy
        // refuses would make a conforming service look broken.
        report.skip("sign-transaction/success (no --envelope supplied)");
        report.skip("sign-transaction/signature-verifies (no --envelope supplied)");
        return;
    };

    let response = client
        .post(target.endpoint("/v1/sign/transaction"))
        .bearer_auth(&target.token)
        .json(&serde_json::json!({
            "xdr": envelope,
            "networkPassphrase": target.network_passphrase,
        }))
        .send()
        .await;

    let Ok(response) = response else {
        report.fail(
            "sign-transaction/success",
            "POST /v1/sign/transaction is reachable",
            format!("{:?}", response.err()),
        );
        return;
    };

    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);

    if status != 200 {
        report.fail(
            "sign-transaction/success",
            "a policy-permitted envelope is signed and returns 200",
            format!("status {status}, body {body}"),
        );
        report.skip("sign-transaction/signature-verifies");
        return;
    }
    report.pass("sign-transaction/success");

    let signed = body.get("signedXdr").and_then(|v| v.as_str());
    report.check(
        "sign-transaction/shape",
        "the response body is { \"signedXdr\": \"<base64>\" }",
        signed.is_some(),
        format!("body was {body}"),
    );

    let Some(signed) = signed else {
        report.skip("sign-transaction/signature-verifies");
        return;
    };

    report.check(
        "sign-transaction/signature-verifies",
        "the returned envelope carries a signature valid for the advertised public key",
        signature_verifies(client, target, signed).await,
        "the signature did not verify against the service's own publicKey".to_string(),
    );
}

async fn signature_verifies(client: &reqwest::Client, target: &Target, signed_xdr: &str) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use sha2::{Digest, Sha256};
    use stellar_xdr::curr::{
        Hash, Limits, ReadXdr, TransactionEnvelope, TransactionSignaturePayload,
        TransactionSignaturePayloadTaggedTransaction, WriteXdr,
    };

    let Ok(response) = client
        .get(target.endpoint("/v1/public-key"))
        .bearer_auth(&target.token)
        .send()
        .await
    else {
        return false;
    };
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    let Some(address) = body.get("publicKey").and_then(|v| v.as_str()) else {
        return false;
    };
    let Ok(public) = stellar_strkey::ed25519::PublicKey::from_string(address) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&public.0) else {
        return false;
    };

    let Ok(TransactionEnvelope::Tx(envelope)) =
        TransactionEnvelope::from_xdr_base64(signed_xdr, Limits::none())
    else {
        return false;
    };

    let network_id: [u8; 32] = Sha256::digest(target.network_passphrase.as_bytes()).into();
    let payload = TransactionSignaturePayload {
        network_id: Hash(network_id),
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(envelope.tx.clone()),
    };
    let Ok(encoded) = payload.to_xdr(Limits::none()) else {
        return false;
    };
    let digest: [u8; 32] = Sha256::digest(encoded).into();

    envelope.signatures.iter().any(|decorated| {
        <[u8; 64]>::try_from(decorated.signature.0.as_slice())
            .map(|bytes| {
                verifying
                    .verify(&digest, &Signature::from_bytes(&bytes))
                    .is_ok()
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_conformant_only_when_nothing_failed() {
        let mut report = Report::default();
        report.pass("a");
        assert!(report.is_conformant());
        report.skip("b");
        assert!(report.is_conformant(), "a skipped check is not a failure");
        report.fail("c", "required", "actual");
        assert!(!report.is_conformant());
    }

    #[test]
    fn a_failure_renders_the_requirement_and_what_happened() {
        // An implementer reading a failure should not have to read this source.
        let failure = crate::Failure {
            check: "public-key/shape".into(),
            requirement: "the response body is { \"publicKey\": \"G...\" }".into(),
            actual: "body was null".into(),
        };
        let rendered = failure.to_string();
        assert!(rendered.contains("public-key/shape"));
        assert!(rendered.contains("required:"));
        assert!(rendered.contains("actual:"));
    }

    #[test]
    fn endpoints_do_not_double_up_slashes() {
        let target = Target {
            url: "https://signer.internal/".into(),
            token: "t".into(),
            invalid_token: "x".into(),
            signable_envelope: None,
            network_passphrase: "n".into(),
        };
        assert_eq!(
            target.endpoint("/v1/public-key"),
            "https://signer.internal/v1/public-key"
        );
    }
}
