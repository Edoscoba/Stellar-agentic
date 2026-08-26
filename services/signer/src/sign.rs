//! The one path a signing request takes.
//!
//! ```text
//! authenticate → resolve key + policy → decode → evaluate policy
//!                                                      │
//!                                 deny ────────────────┤
//!                                                      │ allow
//!                                      backend.sign(32-byte payload)
//!                                                      │
//!                                                 audit + respond
//! ```
//!
//! Deliberately one function per operation, long enough to read top to bottom.
//! A signing service is a thing people audit by reading it; splitting this
//! across six helpers would make each piece prettier and the whole thing
//! harder to be sure about.
//!
//! # Two ordering rules that matter
//!
//! **Audit before responding, on both branches.** A refusal that is not
//! recorded is indistinguishable from a request that never arrived, and a
//! signature that is not recorded defeats the point of the log. A sink failure
//! therefore fails the request.
//!
//! **Consume rate-limit budget only after signing.** Checking consumes
//! nothing, so a caller whose requests are all refused cannot exhaust the
//! legitimate agent's allowance.

use std::sync::Arc;

use crate::audit::{self, AuditLog, Operation, Outcome, PendingRecord, RequestSummary};
use crate::auth::{Subject, TokenRecord, TokenStore, UnixSeconds};
use crate::backend::BackendRegistry;
use crate::error::{RefusalReason, Result, ServiceError, Violation};
use crate::inspect::{self, InspectedCall};
use crate::ledger::LedgerClock;
use crate::metrics::Metrics;
use crate::policy::{
    self, AuthEntryContext, Decision, PolicySet, RateLimitState, TransactionContext,
};
use crate::protocol::{
    PublicKeyResponse, SignAuthEntryRequest, SignAuthEntryResponse, SignTransactionRequest,
    SignTransactionResponse,
};
use crate::registry::KeyRegistry;
use crate::stellar;

use stellar_xdr::curr::{
    DecoratedSignature, Limits, ReadXdr, Signature, SignatureHint, SorobanCredentials,
    TransactionEnvelope, WriteXdr,
};

/// Everything the service needs to answer a request.
pub struct SignerService {
    /// Where keys live.
    pub backends: BackendRegistry,
    /// Which identity uses which key.
    pub registry: KeyRegistry,
    /// What each key is allowed to sign.
    pub policies: PolicySet,
    /// Which credentials are accepted.
    pub tokens: TokenStore,
    /// The append-only record.
    pub audit: AuditLog,
    /// Counters.
    pub metrics: Arc<Metrics>,
    /// Rolling budgets.
    pub rate_limits: RateLimitState,
    /// The current-ledger estimate used to bound auth-entry validity.
    pub clock: Arc<dyn LedgerClock>,
}

impl std::fmt::Debug for SignerService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignerService")
            .field("backends", &self.backends.ids())
            .field("subjects", &self.registry.len())
            .field("policies", &self.policies.names())
            .finish_non_exhaustive()
    }
}

/// Who is calling, resolved from a credential.
#[derive(Debug, Clone)]
pub struct Caller {
    /// The identity.
    pub subject: Subject,
    /// Which credential was presented, by id.
    pub token_id: String,
}

impl From<&TokenRecord> for Caller {
    fn from(record: &TokenRecord) -> Self {
        Self {
            subject: record.subject.clone(),
            token_id: record.id.clone(),
        }
    }
}

impl SignerService {
    /// Resolve a bearer token to the identity it authenticates.
    pub fn authenticate(&self, token: &str, now: UnixSeconds) -> Result<Caller> {
        self.tokens.authenticate(token, now).map(Caller::from)
    }

    /// `GET /v1/public-key`.
    ///
    /// Returns the address of the key bound to the *authenticated caller*.
    /// There is no parameter, so there is no way to ask about anyone else.
    pub async fn public_key(
        &self,
        caller: &Caller,
        request_id: &str,
        now: UnixSeconds,
    ) -> Result<PublicKeyResponse> {
        self.metrics.request(Operation::PublicKey);

        let outcome = self.resolve_public_key(caller).await;
        let (address, record_outcome) = match &outcome {
            Ok(address) => (
                Some(address.clone()),
                Outcome::Signed {
                    // No signature was produced; the digest names the key so
                    // the record is still tied to something specific.
                    payload_sha256: audit::digest(address),
                    signature: String::new(),
                },
            ),
            Err(error) => (None, refusal_outcome(error)),
        };

        let registration = self.registry.resolve(&caller.subject).ok();
        self.write_audit(PendingRecord {
            at: now,
            request_id: request_id.to_string(),
            subject: Some(caller.subject.clone()),
            token_id: Some(caller.token_id.clone()),
            key: registration.map(|r| r.key.to_string()),
            policy: registration.map(|r| r.policy.to_string()),
            operation: Operation::PublicKey,
            request: RequestSummary::default(),
            outcome: record_outcome,
        })?;

        let _ = address;
        outcome.map(|public_key| PublicKeyResponse { public_key })
    }

    async fn resolve_public_key(&self, caller: &Caller) -> Result<String> {
        let registration = self.registry.resolve(&caller.subject)?;
        let backend = self.backends.get(&registration.key).map_err(|error| {
            self.metrics.backend_error();
            ServiceError::from(error)
        })?;
        let raw = backend
            .public_key(&registration.key)
            .await
            .map_err(|error| {
                self.metrics.backend_error();
                ServiceError::from(error)
            })?;
        Ok(stellar::address_from_public_key(&raw))
    }

    /// `POST /v1/sign/transaction`.
    pub async fn sign_transaction(
        &self,
        caller: &Caller,
        request: &SignTransactionRequest,
        request_id: &str,
        now: UnixSeconds,
    ) -> Result<SignTransactionResponse> {
        self.metrics.request(Operation::SignTransaction);

        let registration = self.registry.resolve(&caller.subject)?;
        let policy = self.policy_for(registration)?;

        let mut summary = RequestSummary {
            network: Some(request.network_passphrase.clone()),
            envelope_sha256: Some(audit::digest(&request.xdr)),
            ..RequestSummary::default()
        };

        // ── Decode ──────────────────────────────────────────────────────────
        let decoded = inspect::inspect_transaction(&request.xdr, policy.unknown_calls.into());
        let (transaction, inspected) = match decoded {
            Ok(value) => value,
            Err(error) => {
                return Err(self.refuse(
                    caller,
                    registration,
                    Operation::SignTransaction,
                    summary,
                    error,
                    request_id,
                    now,
                )?)
            }
        };

        summary.source_account = Some(inspected.source_account.clone());
        summary.calls = inspected.calls.iter().map(render_call).collect();
        let total: i128 = inspected
            .calls
            .iter()
            .flat_map(InspectedCall::amounts)
            .fold(0i128, |acc, amount| acc.saturating_add(amount));
        summary.total_amount_stroops = Some(total.to_string());

        // ── Policy ──────────────────────────────────────────────────────────
        let signing_address = self.resolve_public_key(caller).await?;
        let mut decision = policy::evaluate_transaction(
            policy,
            &TransactionContext {
                transaction: &inspected,
                network_passphrase: &request.network_passphrase,
                signing_address: &signing_address,
                now,
            },
        );

        if let Some(violation) = self.rate_limits.check(policy, &caller.subject, total, now) {
            decision = merge(decision, violation);
        }

        if let Decision::Deny(violations) = decision {
            self.metrics
                .policy_violations(violations.iter().map(|v| v.rule.as_str()));
            return Err(self.refuse(
                caller,
                registration,
                Operation::SignTransaction,
                summary,
                ServiceError::policy(violations),
                request_id,
                now,
            )?);
        }

        // ── Sign ────────────────────────────────────────────────────────────
        let payload =
            stellar::transaction_signing_payload(&transaction, &request.network_passphrase)?;
        let backend = self.backends.get(&registration.key).map_err(|error| {
            self.metrics.backend_error();
            ServiceError::from(error)
        })?;
        let signature = backend
            .sign(&registration.key, &payload)
            .await
            .map_err(|error| {
                self.metrics.backend_error();
                ServiceError::from(error)
            })?;

        let public_key = stellar::public_key_from_address(&signing_address).ok_or_else(|| {
            ServiceError::new(RefusalReason::Internal, "the signing address is unusable")
        })?;

        let signed_xdr = attach_signature(&request.xdr, &public_key, &signature)?;

        // ── Record, then respond ────────────────────────────────────────────
        self.write_audit(PendingRecord {
            at: now,
            request_id: request_id.to_string(),
            subject: Some(caller.subject.clone()),
            token_id: Some(caller.token_id.clone()),
            key: Some(registration.key.to_string()),
            policy: Some(registration.policy.to_string()),
            operation: Operation::SignTransaction,
            request: summary,
            outcome: Outcome::Signed {
                payload_sha256: hex::encode(payload),
                signature: hex::encode(signature),
            },
        })?;

        // Only now does the request count against the budget.
        self.rate_limits.commit(policy, &caller.subject, total, now);
        self.metrics.signed(total);

        Ok(SignTransactionResponse { signed_xdr })
    }

    /// `POST /v1/sign/auth-entry`.
    pub async fn sign_auth_entry(
        &self,
        caller: &Caller,
        request: &SignAuthEntryRequest,
        request_id: &str,
        now: UnixSeconds,
    ) -> Result<SignAuthEntryResponse> {
        self.metrics.request(Operation::SignAuthEntry);

        let registration = self.registry.resolve(&caller.subject)?;
        let policy = self.policy_for(registration)?;

        let mut summary = RequestSummary {
            network: Some(request.network_passphrase.clone()),
            envelope_sha256: Some(audit::digest(&request.auth_entry_xdr)),
            ..RequestSummary::default()
        };

        let decoded =
            inspect::inspect_auth_entry(&request.auth_entry_xdr, policy.unknown_calls.into());
        let (entry, inspected) = match decoded {
            Ok(value) => value,
            Err(error) => {
                return Err(self.refuse(
                    caller,
                    registration,
                    Operation::SignAuthEntry,
                    summary,
                    error,
                    request_id,
                    now,
                )?)
            }
        };

        summary.source_account = Some(inspected.address.clone());
        summary.calls = inspected.call.iter().map(render_call).collect();
        let total: i128 = inspected
            .call
            .as_ref()
            .map(|call| {
                call.amounts()
                    .iter()
                    .fold(0i128, |a, b| a.saturating_add(*b))
            })
            .unwrap_or_default();
        summary.total_amount_stroops = Some(total.to_string());

        // The ratchet: fold the caller's claimed ledger into the estimate,
        // refusing a jump that would let one request buy a long-lived
        // signature. See `crate::ledger`.
        let requested = request.valid_until_ledger_seq;
        let claimed_now = requested.saturating_sub(policy.max_auth_validity_ledgers);
        let mut decision = match self.clock.observe(claimed_now) {
            Ok(()) => Decision::Allow,
            Err(ceiling) => Decision::Deny(vec![Violation::new(
                "auth_validity",
                format!(
                    "validUntilLedgerSeq {requested} implies a current ledger beyond the \
                     permitted {ceiling}; this service advances its ledger estimate in bounded \
                     steps"
                ),
            )]),
        };

        let signing_address = self.resolve_public_key(caller).await?;
        let policy_decision = policy::evaluate_auth_entry(
            policy,
            &AuthEntryContext {
                entry: &inspected,
                network_passphrase: &request.network_passphrase,
                signing_address: &signing_address,
                requested_valid_until: requested,
                current_ledger: self.clock.current_ledger(),
                now,
            },
        );
        for violation in policy_decision.violations() {
            decision = merge(decision, violation.clone());
        }

        if let Some(violation) = self.rate_limits.check(policy, &caller.subject, total, now) {
            decision = merge(decision, violation);
        }

        if let Decision::Deny(violations) = decision {
            self.metrics
                .policy_violations(violations.iter().map(|v| v.rule.as_str()));
            return Err(self.refuse(
                caller,
                registration,
                Operation::SignAuthEntry,
                summary,
                ServiceError::policy(violations),
                request_id,
                now,
            )?);
        }

        let payload =
            stellar::auth_entry_signing_payload(&entry, &request.network_passphrase, requested)?;
        let backend = self.backends.get(&registration.key).map_err(|error| {
            self.metrics.backend_error();
            ServiceError::from(error)
        })?;
        let signature = backend
            .sign(&registration.key, &payload)
            .await
            .map_err(|error| {
                self.metrics.backend_error();
                ServiceError::from(error)
            })?;

        let public_key = stellar::public_key_from_address(&signing_address).ok_or_else(|| {
            ServiceError::new(RefusalReason::Internal, "the signing address is unusable")
        })?;

        let mut signed = entry;
        let SorobanCredentials::Address(credentials) = &mut signed.credentials else {
            return Err(ServiceError::new(
                RefusalReason::Internal,
                "the entry lost its address credentials between decode and signing",
            ));
        };
        credentials.signature_expiration_ledger = requested;
        credentials.signature = stellar::account_signature_scval(&public_key, &signature)?;

        let signed_auth_entry_xdr = signed.to_xdr_base64(Limits::none()).map_err(|error| {
            ServiceError::new(
                RefusalReason::Internal,
                "could not re-encode the signed authorization entry",
            )
            .with_internal(error.to_string())
        })?;

        self.write_audit(PendingRecord {
            at: now,
            request_id: request_id.to_string(),
            subject: Some(caller.subject.clone()),
            token_id: Some(caller.token_id.clone()),
            key: Some(registration.key.to_string()),
            policy: Some(registration.policy.to_string()),
            operation: Operation::SignAuthEntry,
            request: summary,
            outcome: Outcome::Signed {
                payload_sha256: hex::encode(payload),
                signature: hex::encode(signature),
            },
        })?;

        self.rate_limits.commit(policy, &caller.subject, total, now);
        self.metrics.signed(total);

        Ok(SignAuthEntryResponse {
            signed_auth_entry_xdr,
        })
    }

    /// Record a request that authentication itself rejected.
    ///
    /// Called from the HTTP layer, which is the only place that knows a
    /// request arrived at all when there is no caller to attribute it to.
    pub fn record_unauthenticated(
        &self,
        operation: Operation,
        request_id: &str,
        error: &ServiceError,
        now: UnixSeconds,
    ) {
        self.metrics.refused(error.reason());
        // A failure to record here cannot fail the request — the request is
        // already being refused. It is logged loudly instead.
        if let Err(sink_error) = self.audit.record(PendingRecord {
            at: now,
            request_id: request_id.to_string(),
            subject: None,
            token_id: None,
            key: None,
            policy: None,
            operation,
            request: RequestSummary::default(),
            outcome: refusal_outcome(error),
        }) {
            tracing::error!(%sink_error, "could not record an unauthenticated request");
        }
    }

    fn policy_for(&self, registration: &crate::registry::Registration) -> Result<&policy::Policy> {
        self.policies.get(&registration.policy).ok_or_else(|| {
            // A registry naming a policy the file does not define must fail
            // closed. Falling back to a default would be the one moment a
            // typo becomes an unbounded signing oracle.
            ServiceError::new(
                RefusalReason::Internal,
                "this identity's policy is not configured",
            )
            .with_internal(format!("policy `{}` is not defined", registration.policy))
        })
    }

    /// Record a refusal and return it.
    #[allow(
        clippy::too_many_arguments,
        reason = "every one of these is a distinct field of the audit record; bundling them \
                  into a struct would add a type whose only purpose is to satisfy a lint, and \
                  make the call sites harder to read against the record they produce"
    )]
    fn refuse(
        &self,
        caller: &Caller,
        registration: &crate::registry::Registration,
        operation: Operation,
        summary: RequestSummary,
        error: ServiceError,
        request_id: &str,
        now: UnixSeconds,
    ) -> Result<ServiceError> {
        self.metrics.refused(error.reason());
        self.write_audit(PendingRecord {
            at: now,
            request_id: request_id.to_string(),
            subject: Some(caller.subject.clone()),
            token_id: Some(caller.token_id.clone()),
            key: Some(registration.key.to_string()),
            policy: Some(registration.policy.to_string()),
            operation,
            request: summary,
            outcome: refusal_outcome(&error),
        })?;
        Ok(error)
    }

    fn write_audit(&self, pending: PendingRecord) -> Result<()> {
        self.audit.record(pending).map(|_| ()).map_err(|error| {
            tracing::error!(%error, "audit sink failed; refusing the request");
            ServiceError::new(
                RefusalReason::AuditUnavailable,
                "the request could not be recorded, so it was not served",
            )
            .with_internal(error.to_string())
        })
    }
}

fn refusal_outcome(error: &ServiceError) -> Outcome {
    Outcome::Refused {
        reason: error.reason().as_str().to_string(),
        violations: error.violations().to_vec(),
    }
}

fn merge(decision: Decision, violation: Violation) -> Decision {
    let mut violations = decision.violations().to_vec();
    violations.push(violation);
    Decision::Deny(violations)
}

fn render_call(call: &InspectedCall) -> String {
    let args: Vec<String> = call.args.iter().map(|arg| arg.value.to_string()).collect();
    let marker = if call.conservative {
        " [unspecced]"
    } else {
        ""
    };
    format!(
        "{}.{}({}){marker}",
        call.contract,
        call.function,
        args.join(", ")
    )
}

/// Put our signature onto the envelope the caller sent.
///
/// Re-decoded from the caller's XDR rather than re-encoded from the parsed
/// `Transaction`: the bytes we hand back must be the bytes we signed, and a
/// re-encode that normalised anything — a field order, an optional — would
/// produce an envelope whose hash no longer matches the signature.
fn attach_signature(
    envelope_xdr: &str,
    public_key: &[u8; 32],
    signature: &[u8; 64],
) -> Result<String> {
    let mut envelope =
        TransactionEnvelope::from_xdr_base64(envelope_xdr, Limits::none()).map_err(|error| {
            ServiceError::new(
                RefusalReason::Internal,
                "the envelope stopped decoding between inspection and signing",
            )
            .with_internal(error.to_string())
        })?;

    let decorated = DecoratedSignature {
        hint: SignatureHint(stellar::signature_hint(public_key)),
        signature: Signature(signature.to_vec().try_into().map_err(|_| {
            ServiceError::new(RefusalReason::Internal, "the signature was not 64 bytes")
        })?),
    };

    match &mut envelope {
        TransactionEnvelope::Tx(inner) => {
            let mut signatures = inner.signatures.to_vec();
            signatures.push(decorated);
            inner.signatures = signatures.try_into().map_err(|_| {
                ServiceError::new(
                    RefusalReason::MalformedEnvelope,
                    "the envelope cannot carry another signature",
                )
            })?;
        }
        _ => {
            return Err(ServiceError::new(
                RefusalReason::Internal,
                "only v1 envelopes reach the signing step",
            ))
        }
    }

    envelope.to_xdr_base64(Limits::none()).map_err(|error| {
        ServiceError::new(
            RefusalReason::Internal,
            "could not re-encode the signed envelope",
        )
        .with_internal(error.to_string())
    })
}
