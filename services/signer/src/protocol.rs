//! The wire protocol from [`docs/signing.md`](../../../docs/signing.md).
//!
//! These types are the contract. The shipped `RemoteSigner` implementations —
//! TypeScript (`packages/core/src/signer.ts`) and Rust (`sdk/rust/src/signer.rs`)
//! — must work against this service unmodified, so the field names here are
//! `camelCase` and are not negotiable.
//!
//! ```text
//! GET  /v1/public-key
//!   → 200 { "publicKey": "G..." }
//!
//! POST /v1/sign/transaction
//!   ← { "xdr": "...", "networkPassphrase": "..." }
//!   → 200 { "signedXdr": "..." }
//!
//! POST /v1/sign/auth-entry
//!   ← { "authEntryXdr": "...", "networkPassphrase": "...",
//!       "validUntilLedgerSeq": 123456 }
//!   → 200 { "signedAuthEntryXdr": "..." }
//! ```
//!
//! # Deliberate strictness
//!
//! Every request type is `#[serde(deny_unknown_fields)]`. A signing service
//! that silently ignores a field it does not recognise is one protocol
//! revision away from ignoring a field that constrains what it signs — imagine
//! a future `maxAmount` that an old build drops on the floor. Rejecting the
//! unknown field is the safe direction: the caller gets a `400` and finds out
//! immediately, rather than getting a signature that ignores their constraint.

use serde::{Deserialize, Serialize};

/// The response to `GET /v1/public-key`.
///
/// There is no request body and no identity parameter: the key returned is the
/// one bound to the **authenticated caller**. A caller cannot ask for someone
/// else's key, which is why there is nothing to ask with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKeyResponse {
    /// The Stellar address (`G…`) this service signs for on the caller's behalf.
    #[serde(rename = "publicKey")]
    pub public_key: String,
}

/// The body of `POST /v1/sign/transaction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignTransactionRequest {
    /// Base64 `TransactionEnvelope` XDR.
    pub xdr: String,
    /// The network the signature must be bound to.
    ///
    /// Verified against the key's allowed networks before anything is signed —
    /// `docs/signing.md` lists this under "service responsibilities", and it is
    /// what stops a testnet-scoped key signing something replayable on mainnet.
    #[serde(rename = "networkPassphrase")]
    pub network_passphrase: String,
}

/// The response to `POST /v1/sign/transaction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignTransactionResponse {
    /// Base64 `TransactionEnvelope` XDR with our signature appended.
    #[serde(rename = "signedXdr")]
    pub signed_xdr: String,
}

/// The body of `POST /v1/sign/auth-entry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignAuthEntryRequest {
    /// Base64 `SorobanAuthorizationEntry` XDR.
    #[serde(rename = "authEntryXdr")]
    pub auth_entry_xdr: String,
    /// The network the signature must be bound to.
    #[serde(rename = "networkPassphrase")]
    pub network_passphrase: String,
    /// Ledger after which the authorisation must no longer be valid.
    ///
    /// **Caller-supplied and therefore capped.** This is the single control on
    /// how long a leaked signed auth entry stays replayable on-chain, and
    /// `docs/signing.md` does not say to bound it — see
    /// `docs/signer-service-design.md`, gap #2. A request exceeding the
    /// policy's cap is refused rather than silently clamped: clamping would
    /// hand back a signature with a different validity than the caller asked
    /// for, and they would have no way to notice.
    #[serde(rename = "validUntilLedgerSeq")]
    pub valid_until_ledger_seq: u32,
}

/// The response to `POST /v1/sign/auth-entry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignAuthEntryResponse {
    /// Base64 `SorobanAuthorizationEntry` XDR with the signature filled in.
    #[serde(rename = "signedAuthEntryXdr")]
    pub signed_auth_entry_xdr: String,
}

/// The response to `GET /healthz` and `GET /readyz`.
///
/// Deliberately says nothing about identity, keys, or policy: these endpoints
/// are unauthenticated so a load balancer can reach them, which means anyone
/// can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// `"ok"` or `"degraded"`.
    pub status: &'static str,
}

/// The header a caller may set to correlate a request with an audit record.
///
/// Generated when absent, and echoed on every response including refusals —
/// so an operator handed a `SigningError` can find the exact audit entry.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Protocol version prefix. Present so a v2 can coexist rather than replace.
pub const API_PREFIX: &str = "/v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bodies_match_what_the_shipped_clients_send() {
        // Byte-for-byte the JSON in docs/signing.md. If this test needs
        // changing, the shipped SDKs need a release first.
        let body =
            r#"{"xdr":"AAAAAgAAAAB","networkPassphrase":"Test SDF Network ; September 2015"}"#;
        let parsed: SignTransactionRequest = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.xdr, "AAAAAgAAAAB");
        assert_eq!(
            parsed.network_passphrase,
            "Test SDF Network ; September 2015"
        );

        let body = r#"{"authEntryXdr":"AAAAAQ","networkPassphrase":"Test SDF Network ; September 2015","validUntilLedgerSeq":123456}"#;
        let parsed: SignAuthEntryRequest = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.auth_entry_xdr, "AAAAAQ");
        assert_eq!(parsed.valid_until_ledger_seq, 123_456);
    }

    #[test]
    fn response_bodies_use_the_field_names_the_clients_read() {
        let json = serde_json::to_value(PublicKeyResponse {
            public_key: "GABC".into(),
        })
        .unwrap();
        assert_eq!(json["publicKey"], "GABC");

        let json = serde_json::to_value(SignTransactionResponse {
            signed_xdr: "AAAA".into(),
        })
        .unwrap();
        assert_eq!(json["signedXdr"], "AAAA");

        let json = serde_json::to_value(SignAuthEntryResponse {
            signed_auth_entry_xdr: "AAAA".into(),
        })
        .unwrap();
        assert_eq!(json["signedAuthEntryXdr"], "AAAA");
    }

    #[test]
    fn an_unknown_field_is_rejected_rather_than_ignored() {
        // The safe direction: a future field that constrains what we sign must
        // not be silently dropped by an old build.
        let body = r#"{"xdr":"AAAA","networkPassphrase":"x","maxAmount":"100"}"#;
        assert!(serde_json::from_str::<SignTransactionRequest>(body).is_err());
    }

    #[test]
    fn a_missing_required_field_is_rejected() {
        assert!(serde_json::from_str::<SignTransactionRequest>(r#"{"xdr":"AAAA"}"#).is_err());
        assert!(serde_json::from_str::<SignAuthEntryRequest>(
            r#"{"authEntryXdr":"AAAA","networkPassphrase":"x"}"#
        )
        .is_err());
    }

    #[test]
    fn snake_case_is_not_accepted_for_camel_case_fields() {
        // Guards against someone "helpfully" adding an alias that would let a
        // non-conforming client work here and fail against another server.
        let body = r#"{"xdr":"AAAA","network_passphrase":"x"}"#;
        assert!(serde_json::from_str::<SignTransactionRequest>(body).is_err());
    }
}
