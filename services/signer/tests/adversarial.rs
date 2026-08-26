//! Attempts to get a signature the service should not give.
//!
//! Each test is written from the attacker's side: assume a caller that holds a
//! valid token (the realistic compromise — the agent process was owned) and is
//! now trying to turn it into more than policy intends.
//!
//! The point of a suite like this is not that any single case is clever. It is
//! that each one has a specific way of *almost* working, and the assertion
//! records which control stops it.

mod harness;

use harness::{harness, harness_with_clock, permissive_policy};
use std::sync::Arc;
use stellaragent_signer::ledger::{FixedClock, RatchetingClock};
use stellaragent_signer::policy::Policy;
use stellaragent_signer::testing::{self, PaymentSpec};

// ─── Credentials ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_credential_is_refused_and_reveals_nothing() {
    let harness = harness(permissive_policy());
    let reply = harness
        .send_with_token("GET", "/v1/public-key", None, None)
        .await;

    assert_eq!(reply.status, 401);
    assert_eq!(reply.reason(), "unauthenticated");
    // A prober must not learn whether any identity exists.
    assert!(
        !reply.error().contains(harness::SUBJECT),
        "{}",
        reply.error()
    );
}

#[tokio::test]
async fn a_wrong_token_is_indistinguishable_from_an_unknown_one() {
    // The message must not confirm that a guessed token was "close".
    let harness = harness(permissive_policy());

    let unknown = harness
        .send_with_token("GET", "/v1/public-key", None, Some("completely-made-up"))
        .await;
    let near_miss = harness
        .send_with_token(
            "GET",
            "/v1/public-key",
            None,
            Some("an-entirely-ordinary-test-toke"), // one character short
        )
        .await;

    assert_eq!(unknown.status, 401);
    assert_eq!(near_miss.status, 401);
    assert_eq!(unknown.error(), near_miss.error());
}

#[tokio::test]
async fn a_non_bearer_authorization_header_is_refused() {
    let harness = harness(permissive_policy());
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/public-key")
        .header("authorization", format!("Basic {}", harness::TOKEN))
        .body(axum::body::Body::empty())
        .unwrap();

    assert_eq!(harness.dispatch(request).await.status, 401);
}

#[tokio::test]
async fn every_rejected_credential_is_still_audited() {
    // An attacker probing tokens should leave a trail, not silence.
    let harness = harness(permissive_policy());
    for token in ["a", "b", "c"] {
        let _ = harness
            .send_with_token("GET", "/v1/public-key", None, Some(token))
            .await;
    }

    let records = harness.records();
    assert_eq!(records.len(), 3);
    for record in &records {
        assert!(
            record.subject.is_none(),
            "there is no identity to attribute"
        );
        assert!(record.token_id.is_none(), "and no token id to record");
    }
    stellaragent_signer::audit::verify_chain(&records).unwrap();
}

// ─── Envelope smuggling ──────────────────────────────────────────────────────

#[tokio::test]
async fn an_envelope_that_already_carries_a_signature_is_refused() {
    // Otherwise the service becomes a co-signer for a multisig flow nobody
    // configured, and an attacker gets a second signature onto an envelope
    // they already control.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::pre_signed_envelope())
        .await;

    assert_eq!(reply.status, 400);
    assert_eq!(reply.reason(), "malformed_envelope");
    assert!(reply.error().contains("co-sign"), "{}", reply.error());
}

#[tokio::test]
async fn a_fee_bump_wrapping_someone_elses_transaction_is_refused() {
    // Signing one pays for an inner transaction the service never inspected.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::fee_bump_envelope())
        .await;

    assert_eq!(reply.status, 400);
    assert!(reply.error().contains("fee-bump"), "{}", reply.error());
}

#[tokio::test]
async fn an_operation_that_overrides_the_source_account_is_refused() {
    // The source-account check happens on the transaction; without this an
    // operation inside it could act as an account nobody checked.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::operation_source_override_envelope())
        .await;

    assert_eq!(reply.status, 422);
    assert_eq!(reply.reason(), "uninspectable");
}

#[tokio::test]
async fn a_transaction_from_a_different_source_account_is_refused() {
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            source: testing::STRANGER.clone(),
            ..Default::default()
        }))
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply
        .violated_rules()
        .contains(&"source_account".to_string()));
}

#[tokio::test]
async fn a_classic_payment_operation_is_refused_rather_than_signed_blind() {
    // The bypass this closes: a raw Payment moves value with no contract
    // involved, so none of the contract-aware rules would even see it.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::classic_payment_envelope())
        .await;

    assert_eq!(reply.status, 422);
    assert_eq!(reply.reason(), "uninspectable");
}

#[tokio::test]
async fn uploading_contract_wasm_is_refused() {
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::upload_wasm_envelope())
        .await;
    assert_eq!(reply.status, 422);
}

// ─── Policy bypass ───────────────────────────────────────────────────────────

#[tokio::test]
async fn splitting_a_large_payment_across_operations_still_hits_the_transaction_cap() {
    // The obvious way around a per-payment cap: many payments, each under it.
    let policy = Policy {
        max_amount_stroops: Some("10000000".into()),
        max_transaction_stroops: Some("25000000".into()),
        ..permissive_policy()
    };
    let harness = harness(policy);

    // Five payments of 10,000,000 — each exactly at the per-payment cap.
    let reply = harness
        .sign_transaction(&testing::multi_payment_envelope(5, 10_000_000))
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply
        .violated_rules()
        .contains(&"transaction_cap".to_string()));
}

#[tokio::test]
async fn an_unknown_contract_function_is_refused_by_default() {
    // A contract with a `drain()` the signer has never heard of must not be
    // signed just because the contract itself is allowlisted.
    let policy = Policy {
        functions: vec!["pay".into(), "drain_everything".into()],
        ..permissive_policy()
    };
    let harness = harness(policy);

    let reply = harness
        .sign_transaction(&testing::unknown_call_envelope(
            "drain_everything",
            999_999_999,
        ))
        .await;

    assert_eq!(reply.status, 422, "{:?}", reply.body);
    assert_eq!(reply.reason(), "uninspectable");
}

#[tokio::test]
async fn an_unknown_function_read_conservatively_is_still_capped() {
    // With `unknown_calls = conservative` the call is decoded, but every
    // integer becomes an amount — so the cap applies rather than being
    // bypassed.
    use stellaragent_signer::policy::UnknownCallSetting;

    let policy = Policy {
        unknown_calls: UnknownCallSetting::Conservative,
        functions: vec!["drain_everything".into()],
        max_amount_stroops: Some("1000".into()),
        ..permissive_policy()
    };
    let harness = harness(policy);

    let reply = harness
        .sign_transaction(&testing::unknown_call_envelope(
            "drain_everything",
            999_999_999,
        ))
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply.violated_rules().contains(&"amount_cap".to_string()));
}

#[tokio::test]
async fn paying_an_address_that_is_not_allowlisted_is_refused() {
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            recipient: testing::STRANGER.clone(),
            ..Default::default()
        }))
        .await;

    assert_eq!(reply.status, 403);
    assert_eq!(reply.violated_rules(), ["recipient_allowlist"]);
}

#[tokio::test]
async fn a_mainnet_signature_cannot_be_obtained_from_a_testnet_key() {
    // Domain separation is only worth anything if the service refuses to sign
    // for a network the key is not scoped to.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction_on(
            &testing::payment_envelope(PaymentSpec::default()),
            "Public Global Stellar Network ; September 2015",
        )
        .await;

    assert_eq!(reply.status, 403);
    assert_eq!(reply.violated_rules(), ["network"]);
}

#[tokio::test]
async fn an_unbounded_envelope_is_refused() {
    // A signature over an envelope with no upper time bound stays submittable
    // forever, so one that never landed could be replayed much later.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            max_time: 0,
            ..Default::default()
        }))
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply
        .violated_rules()
        .contains(&"bounded_expiry".to_string()));
}

// ─── Auth-entry replay window ────────────────────────────────────────────────

#[tokio::test]
async fn an_auth_entry_valid_for_a_decade_is_refused() {
    // The single control on how long a leaked auth-entry signature stays
    // replayable on-chain.
    let harness = harness_with_clock(permissive_policy(), Arc::new(FixedClock(1_000)));

    let reply = harness
        .sign_auth_entry(&testing::auth_entry_xdr(1, 1_000), 99_999_999)
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply
        .violated_rules()
        .contains(&"auth_validity".to_string()));
}

#[tokio::test]
async fn the_validity_cap_is_refused_rather_than_silently_clamped() {
    // Clamping would hand back a signature with a different validity than the
    // caller asked for, which they would have no way to notice.
    let harness = harness_with_clock(permissive_policy(), Arc::new(FixedClock(1_000)));

    let reply = harness
        .sign_auth_entry(&testing::auth_entry_xdr(1, 1_000), 5_000)
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply.body.get("signedAuthEntryXdr").is_none());
}

#[tokio::test]
async fn one_request_cannot_ratchet_the_ledger_estimate_far_forward() {
    // Without the bound, a single request claiming a distant ledger would
    // recalibrate the clock and buy an arbitrarily long-lived signature.
    let harness = harness_with_clock(
        permissive_policy(),
        Arc::new(RatchetingClock::new(1_000, 100)),
    );

    let reply = harness
        .sign_auth_entry(&testing::auth_entry_xdr(1, 1_000), 50_000_000)
        .await;

    assert_eq!(reply.status, 403);
    assert!(reply
        .violated_rules()
        .contains(&"auth_validity".to_string()));
    assert_eq!(
        harness.service.clock.current_ledger(),
        1_000,
        "a refused claim must not move the estimate"
    );
}

// ─── Malformed input ─────────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_xdr_is_refused_without_panicking() {
    let harness = harness(permissive_policy());

    for xdr in [
        "",
        "not base64 at all !!!",
        "AAAA",
        "////////",
        &"A".repeat(10_000),
    ] {
        let reply = harness.sign_transaction(xdr).await;
        assert_eq!(reply.status, 400, "xdr = {xdr:.40}");
        assert_eq!(reply.reason(), "malformed_envelope");
    }
}

#[tokio::test]
async fn a_body_that_is_not_the_protocol_is_refused_with_the_protocols_error_shape() {
    let harness = harness(permissive_policy());

    let cases = [
        serde_json::json!({}),
        serde_json::json!({ "xdr": "AAAA" }),
        serde_json::json!({ "xdr": 42, "networkPassphrase": "n" }),
        // An unknown field must not be silently ignored — a future field that
        // constrains what we sign must not be dropped by an old build.
        serde_json::json!({ "xdr": "AAAA", "networkPassphrase": "n", "maxAmount": "1" }),
    ];

    for body in cases {
        let reply = harness
            .send("POST", "/v1/sign/transaction", Some(body.clone()))
            .await;
        assert_eq!(reply.status, 400, "{body}");
        assert_eq!(reply.reason(), "malformed_request");
        assert!(
            !reply.error().is_empty(),
            "the `error` field is the protocol's contract"
        );
    }
}

#[tokio::test]
async fn an_auth_entry_using_source_account_credentials_is_refused() {
    // It needs no separate signature; asking for one is either confusion or a
    // probe.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_auth_entry(&testing::source_account_auth_entry_xdr(), 1_050)
        .await;

    assert_eq!(reply.status, 400);
    assert_eq!(reply.reason(), "malformed_envelope");
}

#[tokio::test]
async fn a_caller_controlled_endpoint_cannot_forge_an_audit_log_line() {
    // The endpoint field is arbitrary caller input and lands in a JSON log.
    let harness = harness(permissive_policy());
    let nasty = "https://x/\n{\"seq\":999,\"outcome\":\"signed\"}\n\u{1b}[31m";

    let _ = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            endpoint: nasty.into(),
            ..Default::default()
        }))
        .await;

    let lines = harness.audit.lines();
    assert_eq!(
        lines.len(),
        1,
        "one request must produce exactly one log line"
    );
    assert!(!lines[0].contains('\n'));
    assert!(!lines[0].contains('\u{1b}'));
    // ...and the line is still valid JSON.
    serde_json::from_str::<serde_json::Value>(&lines[0]).expect("the log line stays parseable");
}

#[tokio::test]
async fn a_large_endpoint_is_truncated_rather_than_logged_whole() {
    // Small enough to pass the body limit, large enough that logging it whole
    // would let a caller write an unbounded audit line.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            endpoint: "a".repeat(8_000),
            ..Default::default()
        }))
        .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);

    let line = &harness.audit.lines()[0];
    assert!(
        line.len() < 2_000,
        "a caller must not be able to write an unbounded log line: {} bytes",
        line.len()
    );
    assert!(
        line.contains('…'),
        "the truncation should be visible in the record"
    );
}

#[tokio::test]
async fn an_oversized_body_is_rejected_before_it_is_parsed() {
    // The body limit exists so an unauthenticated caller cannot make the
    // service allocate. It sits in front of the handler, so nothing is
    // decoded and nothing is audited.
    let harness = harness(permissive_policy());
    let reply = harness
        .sign_transaction(&testing::payment_envelope(PaymentSpec {
            endpoint: "a".repeat(200_000),
            ..Default::default()
        }))
        .await;

    assert!(
        reply.status.is_client_error(),
        "expected the body limit to reject this, got {}",
        reply.status
    );
    assert!(harness.audit.lines().is_empty());
}

#[tokio::test]
async fn an_unknown_endpoint_returns_the_protocols_error_shape() {
    let harness = harness(permissive_policy());
    let reply = harness.send("GET", "/v1/../etc/passwd", None).await;
    assert!(reply.status.is_client_error(), "{:?}", reply.status);
}

#[tokio::test]
async fn concurrent_requests_produce_a_chain_with_no_gaps() {
    // The audit chain is only meaningful if concurrent signing cannot
    // interleave two records onto the same predecessor.
    let harness = Arc::new(harness(permissive_policy()));
    let envelope = testing::payment_envelope(PaymentSpec::default());

    let mut tasks = Vec::new();
    for _ in 0..16 {
        let harness = Arc::clone(&harness);
        let envelope = envelope.clone();
        tasks.push(tokio::spawn(async move {
            harness.sign_transaction(&envelope).await.status
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), 200);
    }

    let mut records = harness.records();
    records.sort_by_key(|record| record.seq);
    assert_eq!(records.len(), 16);
    stellaragent_signer::audit::verify_chain(&records).expect("the chain must verify");
}
