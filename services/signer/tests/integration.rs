//! End-to-end: the definition-of-done cases, driven over the real HTTP surface.
//!
//! The three the epic names:
//!
//! 1. an agent holding no key completes a full payment through the service;
//! 2. a policy-violating request is rejected **and audited**;
//! 3. the audit chain verifies afterwards.

mod harness;

use harness::{envelope_signature_verifies, harness, permissive_policy, Harness};
use stellaragent_signer::audit::{verify_chain, Outcome};
use stellaragent_signer::policy::{Policy, RateLimit};
use stellaragent_signer::testing::{self, PaymentSpec};

#[tokio::test]
async fn an_agent_with_no_key_completes_a_payment_through_the_service() {
    // The definition of done. The agent never sees key material: it asks for
    // its address, sends an unsigned envelope, and gets a signed one back.
    let harness = harness(permissive_policy());

    let address = harness.public_key().await;
    assert_eq!(address.status, 200);
    let signing_address = address.body["publicKey"].as_str().unwrap().to_string();
    assert_eq!(signing_address, *testing::AGENT);

    let envelope = testing::payment_envelope(PaymentSpec::default());
    let signed = harness.sign_transaction(&envelope).await;
    assert_eq!(signed.status, 200, "{:?}", signed.body);

    let signed_xdr = signed.body["signedXdr"].as_str().unwrap();
    assert_ne!(
        signed_xdr, envelope,
        "the response should carry a signature"
    );
    assert!(
        envelope_signature_verifies(signed_xdr, &signing_address, testing::NETWORK_PASSPHRASE),
        "the returned envelope must carry a signature that verifies under the service's key"
    );
}

#[tokio::test]
async fn no_response_anywhere_contains_key_material() {
    // The property the whole epic exists for, asserted rather than assumed.
    let harness = harness(permissive_policy());
    let secret = stellar_strkey::ed25519::PrivateKey(testing::AGENT_SEED).to_string();

    let replies = vec![
        harness.public_key().await,
        harness
            .sign_transaction(&testing::payment_envelope(PaymentSpec::default()))
            .await,
        harness
            .sign_auth_entry(&testing::auth_entry_xdr(1, 1_000), 1_050)
            .await,
    ];

    for reply in replies {
        let rendered = reply.body.to_string();
        assert!(
            !rendered.contains(&secret),
            "a response leaked the seed: {rendered}"
        );
        assert!(
            !rendered.contains(&hex::encode(testing::AGENT_SEED)),
            "{rendered}"
        );
    }

    // ...and neither does the audit log.
    for line in harness.audit.lines() {
        assert!(
            !line.contains(&secret),
            "the audit log leaked the seed: {line}"
        );
    }
}

#[tokio::test]
async fn a_policy_violating_request_is_rejected_and_audited() {
    // The second definition-of-done case. Both halves matter: a refusal that
    // is not recorded is indistinguishable from a request that never arrived.
    let policy = Policy {
        max_amount_stroops: Some("1000".into()),
        ..permissive_policy()
    };
    let harness = harness(policy);

    let envelope = testing::payment_envelope(PaymentSpec {
        amount: 999_999_999,
        ..Default::default()
    });
    let reply = harness.sign_transaction(&envelope).await;

    assert_eq!(
        reply.status, 403,
        "policy refusals are 403 per docs/signing.md"
    );
    assert_eq!(reply.reason(), "policy_violation");
    // Both caps are breached and both are reported: an operator fixing this
    // policy should not have to discover the second problem after fixing the
    // first.
    assert_eq!(reply.violated_rules(), ["amount_cap", "transaction_cap"]);
    // The `error` field the SDK surfaces must still explain itself.
    assert!(reply.error().contains("amount_cap"), "{}", reply.error());
    assert!(
        reply.error().contains("transaction_cap"),
        "{}",
        reply.error()
    );

    let record = harness.last_record();
    match record.outcome {
        Outcome::Refused { reason, violations } => {
            assert_eq!(reason, "policy_violation");
            assert_eq!(violations.len(), 2, "the record keeps every objection");
            assert_eq!(violations[0].rule, "amount_cap");
        }
        other => panic!("expected a recorded refusal, got {other:?}"),
    }
    assert_eq!(record.subject.unwrap().as_str(), harness::SUBJECT);
    assert_eq!(record.request.total_amount_stroops.unwrap(), "999999999");
}

#[tokio::test]
async fn the_audit_chain_verifies_across_a_mixed_run() {
    // Signatures and refusals interleaved, then the whole chain re-walked.
    let policy = Policy {
        max_amount_stroops: Some("50000000".into()),
        ..permissive_policy()
    };
    let harness = harness(policy);

    for amount in [1_000_000, 999_999_999, 2_000_000, 999_999_999, 3_000_000] {
        let envelope = testing::payment_envelope(PaymentSpec {
            amount,
            ..Default::default()
        });
        let _ = harness.sign_transaction(&envelope).await;
    }
    let _ = harness.public_key().await;

    let records = harness.records();
    assert_eq!(records.len(), 6);
    verify_chain(&records).expect("the chain must verify");

    let signed = records
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Signed { .. }))
        .count();
    let refused = records
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Refused { .. }))
        .count();
    assert_eq!(
        (signed, refused),
        (4, 2),
        "3 payments + public-key, 2 refusals"
    );
}

#[tokio::test]
async fn every_response_carries_the_request_id_including_refusals() {
    // An operator handed a SigningError needs to find the audit record, and
    // this header is the only thing the two have in common.
    let harness = harness(permissive_policy());

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/sign/transaction")
        .header("authorization", format!("Bearer {}", harness::TOKEN))
        .header("content-type", "application/json")
        .header("x-request-id", "trace-me-42")
        .body(axum::body::Body::from(
            serde_json::json!({
                "xdr": testing::payment_envelope(PaymentSpec::default()),
                "networkPassphrase": testing::NETWORK_PASSPHRASE,
            })
            .to_string(),
        ))
        .unwrap();

    let reply = harness.dispatch(request).await;
    assert_eq!(reply.request_id.as_deref(), Some("trace-me-42"));
    assert_eq!(harness.last_record().request_id, "trace-me-42");

    // ...and on a refusal.
    let refused = harness
        .sign_transaction_on("not-xdr", testing::NETWORK_PASSPHRASE)
        .await;
    assert!(refused.request_id.is_some());
}

#[tokio::test]
async fn an_auth_entry_is_signed_and_its_expiry_is_written_into_the_entry() {
    use stellar_xdr::curr::{Limits, ReadXdr, SorobanAuthorizationEntry, SorobanCredentials};

    let harness = harness(permissive_policy());
    let entry = testing::auth_entry_xdr(7, 1_000_000);

    let reply = harness.sign_auth_entry(&entry, 1_050).await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);

    let signed = SorobanAuthorizationEntry::from_xdr_base64(
        reply.body["signedAuthEntryXdr"].as_str().unwrap(),
        Limits::none(),
    )
    .unwrap();

    let SorobanCredentials::Address(credentials) = signed.credentials else {
        panic!("expected address credentials");
    };
    assert_eq!(
        credentials.signature_expiration_ledger, 1_050,
        "the expiry the caller asked for must be the one written into the entry"
    );
    assert_ne!(credentials.signature, stellar_xdr::curr::ScVal::Void);
    assert_eq!(credentials.nonce, 7, "the nonce must be untouched");
}

#[tokio::test]
async fn health_endpoints_need_no_credential_and_reveal_nothing() {
    let harness = harness(permissive_policy());

    for path in ["/healthz", "/readyz"] {
        let reply = harness.send_with_token("GET", path, None, None).await;
        assert_eq!(reply.status, 200, "{path}");
        assert_eq!(reply.body["status"], "ok");
        // Nothing about identities, keys, or policy.
        let rendered = reply.body.to_string();
        assert!(!rendered.contains(harness::SUBJECT), "{rendered}");
        assert!(!rendered.contains("local"), "{rendered}");
    }
}

#[tokio::test]
async fn a_rate_limited_agent_is_refused_once_its_budget_is_spent() {
    let policy = Policy {
        rate_limit: Some(RateLimit {
            window_seconds: 3_600,
            max_requests: 2,
            max_amount_stroops: None,
        }),
        ..permissive_policy()
    };
    let harness = harness(policy);
    let envelope = testing::payment_envelope(PaymentSpec::default());

    assert_eq!(harness.sign_transaction(&envelope).await.status, 200);
    assert_eq!(harness.sign_transaction(&envelope).await.status, 200);

    let refused = harness.sign_transaction(&envelope).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.violated_rules(), ["rate_limit"]);
}

#[tokio::test]
async fn a_refused_request_does_not_consume_the_agents_budget() {
    // The property that stops a stolen token denying service to the real
    // agent: only signatures count against the allowance.
    let policy = Policy {
        max_amount_stroops: Some("50000000".into()),
        rate_limit: Some(RateLimit {
            window_seconds: 3_600,
            max_requests: 2,
            max_amount_stroops: None,
        }),
        ..permissive_policy()
    };
    let harness = harness(policy);

    let over_cap = testing::payment_envelope(PaymentSpec {
        amount: 999_999_999,
        ..Default::default()
    });
    for _ in 0..10 {
        assert_eq!(harness.sign_transaction(&over_cap).await.status, 403);
    }

    // The legitimate agent still has its whole allowance.
    let ok = testing::payment_envelope(PaymentSpec::default());
    assert_eq!(harness.sign_transaction(&ok).await.status, 200);
    assert_eq!(harness.sign_transaction(&ok).await.status, 200);
    assert_eq!(harness.sign_transaction(&ok).await.status, 403);
}

#[tokio::test]
async fn the_audit_record_names_what_was_signed_in_terms_an_operator_can_read() {
    let harness = harness(permissive_policy());
    let envelope = testing::payment_envelope(PaymentSpec {
        amount: 12_345_678,
        ..Default::default()
    });

    assert_eq!(harness.sign_transaction(&envelope).await.status, 200);

    let record = harness.last_record();
    assert_eq!(record.request.total_amount_stroops.unwrap(), "12345678");
    let call = &record.request.calls[0];
    assert!(call.contains(".pay("), "{call}");
    assert!(call.contains(&*testing::RECIPIENT), "{call}");
    assert!(call.contains("12345678"), "{call}");
    assert_eq!(record.key.unwrap(), "local:k1");
    assert_eq!(record.policy.unwrap(), "default");
}

#[tokio::test]
async fn metrics_reflect_what_happened() {
    let policy = Policy {
        max_amount_stroops: Some("1000".into()),
        ..permissive_policy()
    };
    let harness = harness(policy);

    let _ = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            amount: 500,
            ..Default::default()
        }))
        .await;
    let _ = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            amount: 999_999,
            ..Default::default()
        }))
        .await;

    let rendered = harness.service.metrics.render();
    assert!(
        rendered.contains("signer_requests_total{operation=\"sign_transaction\"} 2"),
        "{rendered}"
    );
    assert!(rendered.contains("signer_signatures_total 1"), "{rendered}");
    assert!(
        rendered.contains("signer_signed_stroops_total 500"),
        "{rendered}"
    );
    assert!(
        rendered.contains("signer_refusals_total{reason=\"policy_violation\"} 1"),
        "{rendered}"
    );
    assert!(
        rendered.contains("signer_policy_violations_total{rule=\"amount_cap\"} 1"),
        "{rendered}"
    );
}

#[tokio::test]
async fn a_signature_is_bound_to_the_network_it_was_requested_for() {
    // Domain separation, end to end: a signature produced for testnet must not
    // verify against mainnet's payload.
    let harness = harness(permissive_policy());
    let envelope = testing::payment_envelope(PaymentSpec::default());

    let reply = harness.sign_transaction(&envelope).await;
    let signed_xdr = reply.body["signedXdr"].as_str().unwrap();

    assert!(envelope_signature_verifies(
        signed_xdr,
        &testing::AGENT,
        testing::NETWORK_PASSPHRASE
    ));
    assert!(
        !envelope_signature_verifies(
            signed_xdr,
            &testing::AGENT,
            "Public Global Stellar Network ; September 2015"
        ),
        "a testnet signature must not verify as a mainnet one"
    );
}

#[tokio::test]
async fn a_second_signature_request_for_the_same_envelope_is_independent() {
    // Ed25519 is deterministic, so the same envelope yields the same
    // signature. Worth pinning: a backend adding randomness would break
    // cross-backend agreement, and a caller retrying after a timeout must not
    // get a different envelope.
    let harness = harness(permissive_policy());
    let envelope = testing::payment_envelope(PaymentSpec::default());

    let first = harness.sign_transaction(&envelope).await;
    let second = harness.sign_transaction(&envelope).await;

    assert_eq!(first.body["signedXdr"], second.body["signedXdr"]);
    assert_eq!(harness.records().len(), 2, "both attempts are recorded");
}

#[tokio::test]
async fn a_service_whose_audit_sink_fails_refuses_to_sign() {
    // Signing something we cannot record is the case the log exists for.
    use std::collections::HashMap;
    use std::sync::Arc;
    use stellaragent_signer::audit::{AuditLog, FailingSink};
    use stellaragent_signer::auth::{token_hash, Subject, TokenRecord, TokenStore};
    use stellaragent_signer::backend::local::{LocalKeystore, BACKEND_ID};
    use stellaragent_signer::backend::{BackendRegistry, KeyRef};
    use stellaragent_signer::http::{router, AppState};
    use stellaragent_signer::ledger::RatchetingClock;
    use stellaragent_signer::metrics::Metrics;
    use stellaragent_signer::policy::{PolicySet, RateLimitState};
    use stellaragent_signer::registry::{KeyRegistry, PolicyName, Registration};
    use stellaragent_signer::sign::SignerService;

    let mut entries = HashMap::new();
    entries.insert(
        Subject::new(harness::SUBJECT),
        Registration {
            key: KeyRef::new(BACKEND_ID, "k1"),
            policy: PolicyName::new("default"),
        },
    );
    let mut policies = HashMap::new();
    policies.insert(PolicyName::new("default"), permissive_policy());

    let service = Arc::new(SignerService {
        backends: BackendRegistry::new(vec![Box::new(LocalKeystore::single(
            "k1",
            testing::AGENT_SEED,
        ))]),
        registry: KeyRegistry::new(entries),
        policies: PolicySet::new(policies).unwrap(),
        tokens: TokenStore::new(vec![TokenRecord {
            id: "t1".into(),
            subject: Subject::new(harness::SUBJECT),
            token_sha256: token_hash(harness::TOKEN),
            label: String::new(),
            expires_at: None,
            revoked_at: None,
        }])
        .unwrap(),
        audit: AuditLog::new(Box::new(FailingSink)),
        metrics: Arc::new(Metrics::new()),
        rate_limits: RateLimitState::new(),
        clock: Arc::new(RatchetingClock::new(1_000, 1_000)),
    });

    let broken = Harness {
        app: router(
            AppState {
                service: Arc::clone(&service),
            },
            64 * 1024,
            std::time::Duration::from_secs(30),
        ),
        audit: Arc::new(stellaragent_signer::audit::MemorySink::new()),
        service,
    };

    let reply = broken
        .sign_transaction(&testing::payment_envelope(PaymentSpec::default()))
        .await;

    assert_eq!(reply.status, 503);
    assert_eq!(reply.reason(), "audit_unavailable");
}
