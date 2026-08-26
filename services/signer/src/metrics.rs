//! Counters, and what to alert on.
//!
//! Prometheus text format, hand-rolled. A metrics library would be more
//! ergonomic and would add a dependency plus a global registry to a process
//! whose dependency list is part of its security argument; there are eight
//! counters here and they fit in a struct of atomics.
//!
//! # What matters is the *shape* of the numbers
//!
//! A signing service's interesting signal is not throughput. It is:
//!
//! - **refusals by reason** — a rising `policy_violation` count means either an
//!   agent is misbehaving or a policy is too tight, and the two are
//!   distinguishable by which rule fires;
//! - **`unauthenticated` climbing** — someone is guessing tokens;
//! - **`uninspectable` climbing** — a contract has changed shape and this
//!   service's decoder has fallen behind, so legitimate payments are failing;
//! - **volume against a baseline** — a compromised agent operating *within*
//!   policy is invisible to every rule and shows up only as an unusual rate.
//!
//! The last one is the reason this module exists at all. Policy catches
//! requests that break a rule; nothing catches a caller that is quietly
//! spending its whole allowance every hour except a human noticing the number
//! changed.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::audit::Operation;
use crate::error::RefusalReason;

/// Everything the service counts.
#[derive(Debug, Default)]
pub struct Metrics {
    requests_public_key: AtomicU64,
    requests_sign_transaction: AtomicU64,
    requests_sign_auth_entry: AtomicU64,
    signatures_issued: AtomicU64,
    refusals: [AtomicU64; REFUSAL_REASONS.len()],
    policy_rule_violations: std::sync::Mutex<std::collections::BTreeMap<String, u64>>,
    signed_stroops: AtomicU64,
    backend_errors: AtomicU64,
}

/// Every refusal reason, in the order the counters are laid out.
const REFUSAL_REASONS: [RefusalReason; 10] = [
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

impl Metrics {
    /// Fresh counters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Count an incoming request.
    pub fn request(&self, operation: Operation) {
        let counter = match operation {
            Operation::PublicKey => &self.requests_public_key,
            Operation::SignTransaction => &self.requests_sign_transaction,
            Operation::SignAuthEntry => &self.requests_sign_auth_entry,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Count a signature, and the value it authorised.
    ///
    /// `stroops` is saturating and approximate: it exists to spot a change in
    /// volume, not to be an accounting ledger. The audit log is the ledger.
    pub fn signed(&self, stroops: i128) {
        self.signatures_issued.fetch_add(1, Ordering::Relaxed);
        let clamped = u64::try_from(stroops.max(0)).unwrap_or(u64::MAX);
        self.signed_stroops.fetch_add(clamped, Ordering::Relaxed);
    }

    /// Count a refusal.
    pub fn refused(&self, reason: RefusalReason) {
        if let Some(index) = REFUSAL_REASONS.iter().position(|r| *r == reason) {
            self.refusals[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Count each policy rule that objected.
    ///
    /// Per-rule rather than per-request: "twelve refusals" tells an operator
    /// nothing, "twelve `recipient_allowlist` refusals" tells them an agent is
    /// trying to pay somewhere new.
    pub fn policy_violations<'a>(&self, rules: impl IntoIterator<Item = &'a str>) {
        let mut counts = self.policy_rule_violations.lock().expect("metrics");
        for rule in rules {
            *counts.entry(rule.to_string()).or_insert(0) += 1;
        }
    }

    /// Count a backend failure.
    pub fn backend_error(&self) {
        self.backend_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Render in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(2048);

        out.push_str("# HELP signer_requests_total Signing requests received.\n");
        out.push_str("# TYPE signer_requests_total counter\n");
        for (operation, counter) in [
            ("public_key", &self.requests_public_key),
            ("sign_transaction", &self.requests_sign_transaction),
            ("sign_auth_entry", &self.requests_sign_auth_entry),
        ] {
            out.push_str(&format!(
                "signer_requests_total{{operation=\"{operation}\"}} {}\n",
                counter.load(Ordering::Relaxed)
            ));
        }

        out.push_str("# HELP signer_signatures_total Signatures issued.\n");
        out.push_str("# TYPE signer_signatures_total counter\n");
        out.push_str(&format!(
            "signer_signatures_total {}\n",
            self.signatures_issued.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP signer_signed_stroops_total Value authorised, in stroops.\n");
        out.push_str("# TYPE signer_signed_stroops_total counter\n");
        out.push_str(&format!(
            "signer_signed_stroops_total {}\n",
            self.signed_stroops.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP signer_refusals_total Requests refused, by reason.\n");
        out.push_str("# TYPE signer_refusals_total counter\n");
        for (reason, counter) in REFUSAL_REASONS.iter().zip(&self.refusals) {
            out.push_str(&format!(
                "signer_refusals_total{{reason=\"{}\"}} {}\n",
                reason.as_str(),
                counter.load(Ordering::Relaxed)
            ));
        }

        out.push_str("# HELP signer_policy_violations_total Policy objections, by rule.\n");
        out.push_str("# TYPE signer_policy_violations_total counter\n");
        for (rule, count) in self.policy_rule_violations.lock().expect("metrics").iter() {
            out.push_str(&format!(
                "signer_policy_violations_total{{rule=\"{rule}\"}} {count}\n"
            ));
        }

        out.push_str("# HELP signer_backend_errors_total Signing backend failures.\n");
        out.push_str("# TYPE signer_backend_errors_total counter\n");
        out.push_str(&format!(
            "signer_backend_errors_total {}\n",
            self.backend_errors.load(Ordering::Relaxed)
        ));

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_counted_per_operation() {
        let metrics = Metrics::new();
        metrics.request(Operation::SignTransaction);
        metrics.request(Operation::SignTransaction);
        metrics.request(Operation::PublicKey);

        let rendered = metrics.render();
        assert!(rendered.contains("signer_requests_total{operation=\"sign_transaction\"} 2"));
        assert!(rendered.contains("signer_requests_total{operation=\"public_key\"} 1"));
        assert!(rendered.contains("signer_requests_total{operation=\"sign_auth_entry\"} 0"));
    }

    #[test]
    fn every_refusal_reason_is_exposed_even_at_zero() {
        // A counter that only appears once it fires cannot be alerted on
        // before the first incident.
        let rendered = Metrics::new().render();
        for reason in REFUSAL_REASONS {
            assert!(
                rendered.contains(&format!("reason=\"{}\"", reason.as_str())),
                "{} is missing from:\n{rendered}",
                reason.as_str()
            );
        }
    }

    #[test]
    fn policy_violations_are_counted_per_rule() {
        // "Twelve refusals" tells an operator nothing; "twelve
        // recipient_allowlist refusals" tells them what changed.
        let metrics = Metrics::new();
        metrics.policy_violations(["amount_cap", "recipient_allowlist"]);
        metrics.policy_violations(["amount_cap"]);

        let rendered = metrics.render();
        assert!(rendered.contains("signer_policy_violations_total{rule=\"amount_cap\"} 2"));
        assert!(rendered.contains("signer_policy_violations_total{rule=\"recipient_allowlist\"} 1"));
    }

    #[test]
    fn signed_value_is_tracked_for_volume_alerting() {
        // The only signal that catches a compromised agent operating within
        // policy.
        let metrics = Metrics::new();
        metrics.signed(10_000_000);
        metrics.signed(5_000_000);

        let rendered = metrics.render();
        assert!(rendered.contains("signer_signatures_total 2"));
        assert!(rendered.contains("signer_signed_stroops_total 15000000"));
    }

    #[test]
    fn an_absurd_amount_saturates_rather_than_wrapping() {
        // A wrapped counter would read as a *drop* in volume, which is the
        // opposite of the alert anyone wants.
        let metrics = Metrics::new();
        metrics.signed(i128::MAX);
        assert!(metrics
            .render()
            .contains(&format!("signer_signed_stroops_total {}", u64::MAX)));
    }

    #[test]
    fn a_negative_amount_does_not_reduce_the_counter() {
        let metrics = Metrics::new();
        metrics.signed(100);
        metrics.signed(-1_000);
        assert!(metrics.render().contains("signer_signed_stroops_total 100"));
    }

    #[test]
    fn the_output_parses_as_prometheus_exposition_format() {
        let metrics = Metrics::new();
        metrics.request(Operation::SignTransaction);
        metrics.refused(RefusalReason::PolicyViolation);
        metrics.policy_violations(["amount_cap"]);
        metrics.backend_error();

        for line in metrics.render().lines() {
            if line.starts_with('#') {
                assert!(
                    line.starts_with("# HELP ") || line.starts_with("# TYPE "),
                    "unexpected comment: {line}"
                );
                continue;
            }
            let (name, value) = line.rsplit_once(' ').expect("metric line has a value");
            assert!(!name.is_empty(), "{line}");
            assert!(value.parse::<u64>().is_ok(), "{line}");
        }
    }

    #[test]
    fn every_declared_metric_has_help_and_type_lines() {
        let rendered = Metrics::new().render();
        let names: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("# HELP "))
            .map(|line| line.split_whitespace().nth(2).unwrap())
            .collect();
        assert!(!names.is_empty());
        for name in names {
            assert!(
                rendered.contains(&format!("# TYPE {name} counter")),
                "{name}"
            );
        }
    }
}
