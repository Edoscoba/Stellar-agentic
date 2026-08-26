//! The refusal taxonomy, and how it reaches the wire.
//!
//! # Why refusals are a type rather than a string
//!
//! `docs/signing.md` specifies the error body as `{ "error": "<message>" }`
//! and nothing more, because that is all the shipped clients read — the
//! TypeScript `RemoteSigner` surfaces it verbatim in a `SigningError`, and the
//! Rust one does the same.
//!
//! That is fine for a human reading a log and useless for a fleet. "Signing
//! was refused" and "signing was refused because this agent has spent its
//! hourly budget" want different responses, and a service that only emits
//! prose forces every operator to write a regex over messages that were never
//! promised to be stable.
//!
//! So the body keeps `error` byte-for-byte as specified, and **adds** `reason`
//! and `violations`. Existing clients ignore unknown JSON fields, so this is
//! backwards compatible by construction; a dashboard gets something it can
//! aggregate. See `docs/signer-service-design.md`, gap #5.
//!
//! # Status codes
//!
//! - `400` — the request is malformed. The caller sent something that is not a
//!   valid request at all.
//! - `401` — the caller is not authenticated.
//! - `403` — **policy**. `docs/signing.md` reserves this for refusals the
//!   caller could in principle have avoided: ceiling exceeded, rate limited,
//!   token revoked.
//! - `422` — the request is well-formed but what it asks us to sign cannot be
//!   understood well enough to judge. Distinct from `403` on purpose: `403`
//!   means "we understood and said no", `422` means "we did not understand, so
//!   we will not sign".
//! - `500` / `503` — ours, not the caller's.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// Why a request was refused, in a form something other than a human can act on.
///
/// Serialises as `snake_case`, and the strings are part of the service's
/// public contract — renaming one is a breaking change for anything alerting
/// on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// No credential was presented, or it did not parse.
    Unauthenticated,
    /// The credential is real but no longer valid — expired or revoked.
    CredentialRejected,
    /// The caller authenticated but has no key registered.
    NoKeyForSubject,
    /// The request body did not match the protocol.
    MalformedRequest,
    /// The XDR did not decode, or decoded to something unusable.
    MalformedEnvelope,
    /// The envelope decoded, but into a shape this service will not reason
    /// about — an unknown operation, an unrecognised contract call.
    ///
    /// Deliberately distinct from [`RefusalReason::PolicyViolation`]: this is
    /// "we do not understand what you asked us to sign", which is a refusal on
    /// principle rather than a policy outcome.
    Uninspectable,
    /// One or more policy rules said no. `violations` carries which.
    PolicyViolation,
    /// The backing key store could not be reached, or refused.
    BackendUnavailable,
    /// The audit sink could not record the request.
    ///
    /// Fails the request on purpose: signing something we cannot record is
    /// exactly the case the audit log exists for.
    AuditUnavailable,
    /// Anything else. Never carries detail to the caller.
    Internal,
}

impl RefusalReason {
    /// The HTTP status this reason maps to.
    pub const fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated | Self::CredentialRejected => StatusCode::UNAUTHORIZED,
            Self::NoKeyForSubject | Self::PolicyViolation => StatusCode::FORBIDDEN,
            Self::MalformedRequest | Self::MalformedEnvelope => StatusCode::BAD_REQUEST,
            Self::Uninspectable => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BackendUnavailable | Self::AuditUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable string form, used in the response body and in metrics labels.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::CredentialRejected => "credential_rejected",
            Self::NoKeyForSubject => "no_key_for_subject",
            Self::MalformedRequest => "malformed_request",
            Self::MalformedEnvelope => "malformed_envelope",
            Self::Uninspectable => "uninspectable",
            Self::PolicyViolation => "policy_violation",
            Self::BackendUnavailable => "backend_unavailable",
            Self::AuditUnavailable => "audit_unavailable",
            Self::Internal => "internal",
        }
    }
}

impl std::fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One rule's objection, named so an operator can find it in `policy.toml`.
///
/// `rule` is an owned `String` rather than a `&'static str` because these are
/// written to the audit log and read back out of it by [`crate::audit::verify_chain`]
/// and by anything auditing the file. A borrowed type would make the record
/// serialise but not deserialise, which would leave the chain unverifiable by
/// anyone but the process that wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    /// The rule that objected — `amount_cap`, `recipient_allowlist`, …
    pub rule: String,
    /// What specifically was wrong, in terms an operator can act on.
    pub detail: String,
}

impl Violation {
    /// Build a violation for `rule`.
    pub fn new(rule: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            rule: rule.into(),
            detail: detail.into(),
        }
    }
}

/// A refusal, on its way to the caller.
#[derive(Debug)]
pub struct ServiceError {
    reason: RefusalReason,
    message: String,
    violations: Vec<Violation>,
    /// Detail we log but never send. Anything that would tell a prober about
    /// our internals, or about a key or token, belongs here.
    internal: Option<String>,
}

impl ServiceError {
    /// Build a refusal.
    pub fn new(reason: RefusalReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
            violations: Vec::new(),
            internal: None,
        }
    }

    /// A policy refusal carrying every rule that objected.
    ///
    /// The `error` string lists them so a human reading the SDK's
    /// `SigningError` sees the reasons without needing the structured fields.
    pub fn policy(violations: Vec<Violation>) -> Self {
        let summary = violations
            .iter()
            .map(|v| format!("{}: {}", v.rule, v.detail))
            .collect::<Vec<_>>()
            .join("; ");
        Self {
            reason: RefusalReason::PolicyViolation,
            message: if summary.is_empty() {
                "refused by policy".to_string()
            } else {
                summary
            },
            violations,
            internal: None,
        }
    }

    /// Attach detail that is logged but never returned.
    #[must_use]
    pub fn with_internal(mut self, detail: impl Into<String>) -> Self {
        self.internal = Some(detail.into());
        self
    }

    /// Why this was refused.
    pub fn reason(&self) -> RefusalReason {
        self.reason
    }

    /// The message that reaches the caller.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The rules that objected, if this was a policy refusal.
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Detail for the log only.
    pub fn internal_detail(&self) -> Option<&str> {
        self.internal.as_deref()
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.reason, self.message)
    }
}

impl std::error::Error for ServiceError {}

/// The wire shape of a refusal.
///
/// `error` is the field `docs/signing.md` specifies and the only one the
/// shipped clients read. The rest is additive.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Human-readable, surfaced verbatim by the SDK's `SigningError`.
    pub error: String,
    /// Machine-readable refusal reason.
    pub reason: &'static str,
    /// Which policy rules objected. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<Violation>,
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        // An internal error must not narrate itself to the caller: the message
        // could name a key id, a path, or a backend failure mode. It is
        // already on its way to the log via the caller of this function.
        let error = if self.reason == RefusalReason::Internal {
            "internal error".to_string()
        } else {
            self.message.clone()
        };

        let body = ErrorBody {
            error,
            reason: self.reason.as_str(),
            violations: self.violations,
        };

        (self.reason.status(), axum::Json(body)).into_response()
    }
}

/// The service's `Result`.
pub type Result<T> = std::result::Result<T, ServiceError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_refusals_are_403_as_the_protocol_specifies() {
        // docs/signing.md: "Use 403 for policy refusals — ceiling exceeded,
        // rate limited, token revoked."
        assert_eq!(
            RefusalReason::PolicyViolation.status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn not_understanding_a_request_is_distinct_from_refusing_it() {
        // 422 vs 403 is the difference between "we did not understand" and
        // "we understood and said no". Collapsing them would hide the case
        // where the decoder has fallen behind a contract.
        assert_eq!(
            RefusalReason::Uninspectable.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_ne!(
            RefusalReason::Uninspectable.status(),
            RefusalReason::PolicyViolation.status()
        );
    }

    #[test]
    fn a_policy_refusal_summarises_every_violation_in_the_error_string() {
        // The SDK only reads `error`, so a human debugging through the SDK has
        // to be able to see all the reasons there.
        let error = ServiceError::policy(vec![
            Violation::new("amount_cap", "1000 stroops over the 500 limit"),
            Violation::new("rate_limit", "12 of 10 requests this hour"),
        ]);
        assert!(error.message().contains("amount_cap"));
        assert!(error.message().contains("rate_limit"));
        assert_eq!(error.violations().len(), 2);
    }

    #[test]
    fn an_empty_policy_refusal_still_says_something() {
        let error = ServiceError::policy(Vec::new());
        assert_eq!(error.message(), "refused by policy");
    }

    #[test]
    fn internal_errors_do_not_narrate_themselves_to_the_caller() {
        let error = ServiceError::new(RefusalReason::Internal, "kms key arn:aws:... exploded")
            .with_internal("full stack detail");
        let body = serde_json::to_string(&ErrorBody {
            error: "internal error".into(),
            reason: error.reason().as_str(),
            violations: Vec::new(),
        })
        .unwrap();
        assert!(!body.contains("arn:aws"), "{body}");
    }

    #[test]
    fn every_reason_has_a_distinct_stable_string() {
        let reasons = [
            RefusalReason::Unauthenticated,
            RefusalReason::CredentialRejected,
            RefusalReason::NoKeyForSubject,
            RefusalReason::MalformedRequest,
            RefusalReason::MalformedEnvelope,
            RefusalReason::Uninspectable,
            RefusalReason::PolicyViolation,
            RefusalReason::BackendUnavailable,
            RefusalReason::AuditUnavailable,
            RefusalReason::Internal,
        ];
        let mut seen = std::collections::HashSet::new();
        for reason in reasons {
            assert!(seen.insert(reason.as_str()), "duplicate: {reason}");
        }
    }

    #[test]
    fn the_error_body_omits_violations_when_there_are_none() {
        // A non-policy refusal should not carry an empty array that a consumer
        // has to special-case.
        let body = serde_json::to_value(ErrorBody {
            error: "nope".into(),
            reason: RefusalReason::Unauthenticated.as_str(),
            violations: Vec::new(),
        })
        .unwrap();
        assert!(body.get("violations").is_none(), "{body}");
        assert_eq!(body["error"], "nope");
        assert_eq!(body["reason"], "unauthenticated");
    }
}
