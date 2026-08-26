//! What this key is allowed to sign.
//!
//! # Deny by default, and report everything
//!
//! Every rule runs; none short-circuits. An operator fixing a policy wants the
//! full list of objections, not to discover the second one after fixing the
//! first — the same choice `math::predict_payment_outcome` makes in the SDK,
//! for the same reason.
//!
//! An empty allowlist means **deny all**, not "allow all". A policy file with a
//! missing section should fail closed; the opposite convention turns a typo
//! into an unbounded signing oracle.
//!
//! # Rate limits are consumed on success, not on request
//!
//! A refused request must not burn budget. Otherwise anyone holding a stolen
//! token could exhaust an agent's hourly allowance with requests that were
//! never going to be signed — denying service to the legitimate agent without
//! ever passing policy. So [`RateLimitState::check`] only peeks, and
//! [`RateLimitState::commit`] is called after a signature is actually
//! produced.
//!
//! # Amounts are strings in the config file
//!
//! TOML integers are `i64`; Stellar amounts are `i128` stroops. A ceiling
//! above ~9.2×10^18 stroops would silently fail to parse or wrap. Strings are
//! parsed explicitly, and a malformed one fails at startup rather than at the
//! first payment.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::auth::{Subject, UnixSeconds};
use crate::error::Violation;
use crate::inspect::{InspectedAuthEntry, InspectedTransaction, UnknownCallPolicy};
use crate::registry::PolicyName;

/// The outcome of evaluating a policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing objected.
    Allow,
    /// One or more rules objected. Never empty.
    Deny(Vec<Violation>),
}

impl Decision {
    /// Whether this decision permits signing.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// The objections, empty when allowed.
    pub fn violations(&self) -> &[Violation] {
        match self {
            Self::Allow => &[],
            Self::Deny(violations) => violations,
        }
    }

    fn from(violations: Vec<Violation>) -> Self {
        if violations.is_empty() {
            Self::Allow
        } else {
            Self::Deny(violations)
        }
    }
}

/// A rolling budget.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    /// The window's width in seconds.
    pub window_seconds: u64,
    /// Most signing requests allowed in one window.
    pub max_requests: u32,
    /// Most total value, in stroops, allowed in one window.
    ///
    /// A string — see the module docs on TOML integer width.
    #[serde(default)]
    pub max_amount_stroops: Option<String>,
}

/// When signing is permitted.
///
/// `Default` is written out rather than derived: a derived one would give
/// `end_hour_utc: 0`, which disagrees with the serde default of 24 and would
/// make a programmatically-constructed window deny everything while a parsed
/// one allowed it. Two defaults for the same field is a bug waiting to be
/// written, so there is one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimeWindow {
    /// First permitted UTC hour, inclusive. Defaults to 0.
    #[serde(default)]
    pub start_hour_utc: u8,
    /// Last permitted UTC hour, exclusive. Defaults to 24.
    #[serde(default = "default_end_hour")]
    pub end_hour_utc: u8,
    /// Permitted days, as `mon`…`sun`. Empty means every day.
    #[serde(default)]
    pub days: Vec<String>,
    /// Reject anything before this Unix timestamp.
    #[serde(default)]
    pub not_before: Option<UnixSeconds>,
    /// Reject anything after this Unix timestamp.
    #[serde(default)]
    pub not_after: Option<UnixSeconds>,
}

fn default_end_hour() -> u8 {
    24
}

impl Default for TimeWindow {
    fn default() -> Self {
        Self {
            start_hour_utc: 0,
            end_hour_utc: default_end_hour(),
            days: Vec::new(),
            not_before: None,
            not_after: None,
        }
    }
}

/// Everything one key is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Network passphrases this key will sign for. Empty denies all.
    ///
    /// `docs/signing.md` lists verifying the passphrase under "service
    /// responsibilities": it is what stops a testnet-scoped key producing a
    /// signature that is replayable on mainnet.
    #[serde(default)]
    pub networks: Vec<String>,

    /// Largest single amount, in stroops. Absent means no per-amount cap.
    #[serde(default)]
    pub max_amount_stroops: Option<String>,

    /// Largest total across one transaction, in stroops.
    ///
    /// Separate from the per-amount cap because a transaction can carry many
    /// operations: a hundred payments of one stroop under the cap each is
    /// still a hundred stroops leaving the account.
    #[serde(default)]
    pub max_transaction_stroops: Option<String>,

    /// Addresses that may receive value. Empty denies all.
    #[serde(default)]
    pub recipients: Vec<String>,

    /// Contracts that may be invoked. Empty denies all.
    #[serde(default)]
    pub contracts: Vec<String>,

    /// Functions that may be called. Empty denies all.
    #[serde(default)]
    pub functions: Vec<String>,

    /// Largest `validUntilLedgerSeq` offset an auth entry may request, in
    /// ledgers past the current one.
    ///
    /// The single control on how long a leaked auth-entry signature stays
    /// replayable. Defaults to 100 — about eight minutes at five-second
    /// ledgers, matching what the SDKs request.
    #[serde(default = "default_auth_validity")]
    pub max_auth_validity_ledgers: u32,

    /// Refuse a transaction with no upper time bound.
    ///
    /// An unbounded envelope stays submittable forever, so one that never made
    /// it into a ledger could be replayed much later. On by default.
    #[serde(default = "default_true")]
    pub require_bounded_expiry: bool,

    /// What to do with a contract call this service cannot decode.
    #[serde(default)]
    pub unknown_calls: UnknownCallSetting,

    /// The rolling budget, if any.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,

    /// When signing is permitted, if restricted.
    #[serde(default)]
    pub time_window: Option<TimeWindow>,
}

fn default_auth_validity() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

/// Config spelling of [`UnknownCallPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownCallSetting {
    /// Refuse to sign it. The default.
    #[default]
    Refuse,
    /// Decode it restrictively — see [`UnknownCallPolicy::Conservative`].
    Conservative,
}

impl From<UnknownCallSetting> for UnknownCallPolicy {
    fn from(setting: UnknownCallSetting) -> Self {
        match setting {
            UnknownCallSetting::Refuse => Self::Refuse,
            UnknownCallSetting::Conservative => Self::Conservative,
        }
    }
}

impl Default for Policy {
    /// The most restrictive policy that is still a policy: nothing allowed.
    fn default() -> Self {
        Self {
            networks: Vec::new(),
            max_amount_stroops: None,
            max_transaction_stroops: None,
            recipients: Vec::new(),
            contracts: Vec::new(),
            functions: Vec::new(),
            max_auth_validity_ledgers: default_auth_validity(),
            require_bounded_expiry: true,
            unknown_calls: UnknownCallSetting::Refuse,
            rate_limit: None,
            time_window: None,
        }
    }
}

impl Policy {
    /// Parse the amount ceilings, failing at load rather than at signing time.
    pub fn validate(&self, name: &str) -> Result<(), String> {
        for (field, value) in [
            ("max_amount_stroops", &self.max_amount_stroops),
            ("max_transaction_stroops", &self.max_transaction_stroops),
        ] {
            if let Some(raw) = value {
                raw.parse::<i128>().map_err(|_| {
                    format!("policy `{name}`: {field} = \"{raw}\" is not an integer")
                })?;
            }
        }
        if let Some(limit) = &self.rate_limit {
            if let Some(raw) = &limit.max_amount_stroops {
                raw.parse::<i128>().map_err(|_| {
                    format!("policy `{name}`: rate_limit.max_amount_stroops = \"{raw}\" is not an integer")
                })?;
            }
            if limit.window_seconds == 0 {
                return Err(format!(
                    "policy `{name}`: rate_limit.window_seconds must be positive"
                ));
            }
        }
        if let Some(window) = &self.time_window {
            if window.end_hour_utc > 24 || window.start_hour_utc > 23 {
                return Err(format!(
                    "policy `{name}`: time_window hours must be 0–23 (start) and 0–24 (end)"
                ));
            }
            for day in &window.days {
                if weekday_index(day).is_none() {
                    return Err(format!(
                        "policy `{name}`: time_window.days contains \"{day}\"; expected mon…sun"
                    ));
                }
            }
        }
        if self.networks.is_empty() {
            return Err(format!(
                "policy `{name}`: `networks` is empty, so every request would be refused. List \
                 the passphrases this key signs for."
            ));
        }
        Ok(())
    }

    fn amount_cap(&self) -> Option<i128> {
        self.max_amount_stroops.as_ref()?.parse().ok()
    }

    fn transaction_cap(&self) -> Option<i128> {
        self.max_transaction_stroops.as_ref()?.parse().ok()
    }
}

/// The policies loaded from the policy file.
#[derive(Debug, Default)]
pub struct PolicySet {
    policies: HashMap<PolicyName, Policy>,
}

impl PolicySet {
    /// Build a set, validating every policy.
    pub fn new(policies: HashMap<PolicyName, Policy>) -> Result<Self, String> {
        for (name, policy) in &policies {
            policy.validate(name.as_str())?;
        }
        Ok(Self { policies })
    }

    /// Look up a policy by name.
    pub fn get(&self, name: &PolicyName) -> Option<&Policy> {
        self.policies.get(name)
    }

    /// Every policy name, sorted.
    pub fn names(&self) -> Vec<&PolicyName> {
        let mut names: Vec<&PolicyName> = self.policies.keys().collect();
        names.sort();
        names
    }
}

/// What a transaction request is evaluated against.
pub struct TransactionContext<'a> {
    /// The decoded transaction.
    pub transaction: &'a InspectedTransaction,
    /// The passphrase the caller asked us to sign for.
    pub network_passphrase: &'a str,
    /// The address this key signs for.
    pub signing_address: &'a str,
    /// Wall clock, for time windows.
    pub now: UnixSeconds,
}

/// What an auth-entry request is evaluated against.
pub struct AuthEntryContext<'a> {
    /// The decoded entry.
    pub entry: &'a InspectedAuthEntry,
    /// The passphrase the caller asked us to sign for.
    pub network_passphrase: &'a str,
    /// The address this key signs for.
    pub signing_address: &'a str,
    /// The validity the caller requested.
    pub requested_valid_until: u32,
    /// The current ledger, as configured or observed.
    pub current_ledger: u32,
    /// Wall clock, for time windows.
    pub now: UnixSeconds,
}

/// Evaluate a transaction against a policy.
pub fn evaluate_transaction(policy: &Policy, ctx: &TransactionContext<'_>) -> Decision {
    let mut violations = Vec::new();

    check_network(policy, ctx.network_passphrase, &mut violations);
    check_time_window(policy, ctx.now, &mut violations);

    // The transaction must be submitted from the account we sign for. A
    // signature over someone else's transaction is useless at best, and a
    // probe at worst.
    if ctx.transaction.source_account != ctx.signing_address {
        violations.push(Violation::new(
            "source_account",
            format!(
                "the transaction is submitted from {}, but this key signs for {}",
                ctx.transaction.source_account, ctx.signing_address
            ),
        ));
    }

    if policy.require_bounded_expiry && ctx.transaction.max_time.is_none() {
        violations.push(Violation::new(
            "bounded_expiry",
            "the transaction has no upper time bound, so a signature over it would stay \
             submittable indefinitely",
        ));
    }

    let mut transaction_total: i128 = 0;
    for call in &ctx.transaction.calls {
        check_contract(policy, &call.contract, &mut violations);
        check_function(policy, &call.function, &mut violations);

        for recipient in call.recipients() {
            check_recipient(policy, recipient, &mut violations);
        }

        for amount in call.amounts() {
            transaction_total = transaction_total.saturating_add(amount);
            check_amount(policy, amount, &mut violations);
        }
    }

    if let Some(cap) = policy.transaction_cap() {
        if transaction_total > cap {
            violations.push(Violation::new(
                "transaction_cap",
                format!(
                    "the transaction moves {transaction_total} stroops in total, over the \
                     {cap} limit"
                ),
            ));
        }
    }

    Decision::from(violations)
}

/// Evaluate an auth-entry request against a policy.
pub fn evaluate_auth_entry(policy: &Policy, ctx: &AuthEntryContext<'_>) -> Decision {
    let mut violations = Vec::new();

    check_network(policy, ctx.network_passphrase, &mut violations);
    check_time_window(policy, ctx.now, &mut violations);

    if ctx.entry.address != ctx.signing_address {
        violations.push(Violation::new(
            "auth_address",
            format!(
                "the entry authorises {}, but this key signs for {}",
                ctx.entry.address, ctx.signing_address
            ),
        ));
    }

    // The cap that bounds how long a leaked signature stays replayable.
    // Refused rather than clamped: silently handing back a shorter validity
    // than the caller asked for would be a signature they cannot reason about.
    let ceiling = ctx
        .current_ledger
        .saturating_add(policy.max_auth_validity_ledgers);
    if ctx.requested_valid_until > ceiling {
        violations.push(Violation::new(
            "auth_validity",
            format!(
                "validUntilLedgerSeq {} is beyond the permitted {} ledgers past the current \
                 ledger {} (ceiling {ceiling})",
                ctx.requested_valid_until, policy.max_auth_validity_ledgers, ctx.current_ledger
            ),
        ));
    }

    if let Some(call) = &ctx.entry.call {
        check_contract(policy, &call.contract, &mut violations);
        check_function(policy, &call.function, &mut violations);
        for recipient in call.recipients() {
            check_recipient(policy, recipient, &mut violations);
        }
        for amount in call.amounts() {
            check_amount(policy, amount, &mut violations);
        }
    }

    Decision::from(violations)
}

fn check_network(policy: &Policy, passphrase: &str, violations: &mut Vec<Violation>) {
    if !policy.networks.iter().any(|allowed| allowed == passphrase) {
        violations.push(Violation::new(
            "network",
            format!("this key does not sign for the network {passphrase:?}"),
        ));
    }
}

fn check_contract(policy: &Policy, contract: &str, violations: &mut Vec<Violation>) {
    if !policy.contracts.iter().any(|allowed| allowed == contract) {
        violations.push(Violation::new(
            "contract_allowlist",
            format!("contract {contract} is not on this key's allowlist"),
        ));
    }
}

fn check_function(policy: &Policy, function: &str, violations: &mut Vec<Violation>) {
    if !policy.functions.iter().any(|allowed| allowed == function) {
        violations.push(Violation::new(
            "function_allowlist",
            format!("function `{function}` is not on this key's allowlist"),
        ));
    }
}

fn check_recipient(policy: &Policy, recipient: &str, violations: &mut Vec<Violation>) {
    if !policy.recipients.iter().any(|allowed| allowed == recipient) {
        violations.push(Violation::new(
            "recipient_allowlist",
            format!("{recipient} is not on this key's recipient allowlist"),
        ));
    }
}

fn check_amount(policy: &Policy, amount: i128, violations: &mut Vec<Violation>) {
    // A negative amount is not a small payment; it is a sign the argument was
    // not what we thought, and no contract in this system takes one.
    if amount < 0 {
        violations.push(Violation::new(
            "amount_cap",
            format!("amount {amount} is negative"),
        ));
        return;
    }
    if let Some(cap) = policy.amount_cap() {
        if amount > cap {
            violations.push(Violation::new(
                "amount_cap",
                format!("amount {amount} stroops is over the {cap} limit"),
            ));
        }
    }
}

fn check_time_window(policy: &Policy, now: UnixSeconds, violations: &mut Vec<Violation>) {
    let Some(window) = &policy.time_window else {
        return;
    };

    if window.not_before.is_some_and(|start| now < start) {
        violations.push(Violation::new(
            "time_window",
            "this key is not yet permitted to sign",
        ));
    }
    if window.not_after.is_some_and(|end| now > end) {
        violations.push(Violation::new(
            "time_window",
            "this key's signing period has ended",
        ));
    }

    let seconds_into_day = now % 86_400;
    let hour = (seconds_into_day / 3_600) as u8;
    if hour < window.start_hour_utc || hour >= window.end_hour_utc {
        violations.push(Violation::new(
            "time_window",
            format!(
                "the current hour ({hour:02}:00 UTC) is outside the permitted {:02}:00–{:02}:00",
                window.start_hour_utc, window.end_hour_utc
            ),
        ));
    }

    if !window.days.is_empty() {
        // 1970-01-01 was a Thursday, index 3 with Monday at 0.
        let day_index = ((now / 86_400 + 3) % 7) as u8;
        let permitted = window
            .days
            .iter()
            .filter_map(|day| weekday_index(day))
            .any(|index| index == day_index);
        if !permitted {
            violations.push(Violation::new(
                "time_window",
                format!("{} is not a permitted signing day", weekday_name(day_index)),
            ));
        }
    }
}

fn weekday_index(day: &str) -> Option<u8> {
    match day.to_ascii_lowercase().as_str() {
        "mon" | "monday" => Some(0),
        "tue" | "tuesday" => Some(1),
        "wed" | "wednesday" => Some(2),
        "thu" | "thursday" => Some(3),
        "fri" | "friday" => Some(4),
        "sat" | "saturday" => Some(5),
        "sun" | "sunday" => Some(6),
        _ => None,
    }
}

fn weekday_name(index: u8) -> &'static str {
    ["mon", "tue", "wed", "thu", "fri", "sat", "sun"][(index % 7) as usize]
}

// ─── Rate limiting ───────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
struct Window {
    started_at: UnixSeconds,
    requests: u32,
    amount: i128,
}

/// Rolling budgets, per subject.
///
/// # In memory, and therefore single-replica
///
/// State lives in this process. Two replicas means two independent budgets, so
/// a `10/hour` limit silently becomes `20/hour`, and a restart resets the
/// window. This is a real limitation, documented in
/// `docs/signer-service-design.md` and in the deployment guide — v1 is
/// single-replica. Shared state is the obvious follow-up and deliberately out
/// of scope, because it turns a self-contained service into one with a
/// stateful dependency.
#[derive(Debug, Default)]
pub struct RateLimitState {
    windows: Mutex<HashMap<Subject, Window>>,
}

impl RateLimitState {
    /// A fresh, empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `subject` may spend `amount` now — **without** consuming budget.
    ///
    /// Peeking rather than consuming is what stops a stolen token denying
    /// service: a request that policy refuses must not use up the legitimate
    /// agent's allowance.
    pub fn check(
        &self,
        policy: &Policy,
        subject: &Subject,
        amount: i128,
        now: UnixSeconds,
    ) -> Option<Violation> {
        let limit = policy.rate_limit.as_ref()?;
        let windows = self.windows.lock().expect("rate limit state");
        let window = current_window(&windows, subject, limit.window_seconds, now);

        if window.requests >= limit.max_requests {
            return Some(Violation::new(
                "rate_limit",
                format!(
                    "{} of {} signing requests used in this {}s window",
                    window.requests, limit.max_requests, limit.window_seconds
                ),
            ));
        }

        if let Some(cap) = limit
            .max_amount_stroops
            .as_ref()
            .and_then(|raw| raw.parse::<i128>().ok())
        {
            let projected = window.amount.saturating_add(amount);
            if projected > cap {
                return Some(Violation::new(
                    "rate_limit",
                    format!(
                        "{projected} stroops would exceed the {cap} allowed in this {}s window",
                        limit.window_seconds
                    ),
                ));
            }
        }

        None
    }

    /// Record a request that was actually signed.
    pub fn commit(&self, policy: &Policy, subject: &Subject, amount: i128, now: UnixSeconds) {
        let Some(limit) = policy.rate_limit.as_ref() else {
            return;
        };
        let mut windows = self.windows.lock().expect("rate limit state");
        let window = windows.entry(subject.clone()).or_default();

        if now.saturating_sub(window.started_at) >= limit.window_seconds {
            *window = Window {
                started_at: now,
                requests: 0,
                amount: 0,
            };
        }
        window.requests = window.requests.saturating_add(1);
        window.amount = window.amount.saturating_add(amount);
    }
}

fn current_window(
    windows: &HashMap<Subject, Window>,
    subject: &Subject,
    window_seconds: u64,
    now: UnixSeconds,
) -> Window {
    match windows.get(subject) {
        Some(window) if now.saturating_sub(window.started_at) < window_seconds => window.clone(),
        _ => Window {
            started_at: now,
            requests: 0,
            amount: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::{inspect_transaction, UnknownCallPolicy};
    use crate::testing;

    fn permissive() -> Policy {
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

    fn decide(policy: &Policy, xdr: &str) -> Decision {
        let (_, inspected) = inspect_transaction(xdr, UnknownCallPolicy::Refuse).unwrap();
        evaluate_transaction(
            policy,
            &TransactionContext {
                transaction: &inspected,
                network_passphrase: testing::NETWORK_PASSPHRASE,
                signing_address: &testing::AGENT,
                now: 1_700_000_000,
            },
        )
    }

    fn rules(decision: &Decision) -> Vec<&str> {
        let mut rules: Vec<&str> = decision
            .violations()
            .iter()
            .map(|v| v.rule.as_str())
            .collect();
        rules.sort();
        rules.dedup();
        rules
    }

    #[test]
    fn a_compliant_payment_is_allowed() {
        let decision = decide(
            &permissive(),
            &testing::payment_envelope(Default::default()),
        );
        assert!(decision.is_allowed(), "{decision:?}");
    }

    #[test]
    fn every_violated_rule_is_reported_not_just_the_first() {
        // An operator fixing a policy wants the whole list.
        let policy = Policy {
            max_amount_stroops: Some("1".into()),
            recipients: vec![], // deny all
            ..permissive()
        };
        let decision = decide(&policy, &testing::payment_envelope(Default::default()));
        assert_eq!(rules(&decision), ["amount_cap", "recipient_allowlist"]);
    }

    #[test]
    fn an_empty_allowlist_denies_rather_than_permits() {
        // The fail-closed convention. A policy file with a missing section
        // must not become an unbounded signing oracle.
        let policy = Policy {
            recipients: vec![],
            contracts: vec![],
            functions: vec![],
            ..permissive()
        };
        let decision = decide(&policy, &testing::payment_envelope(Default::default()));
        assert_eq!(
            rules(&decision),
            [
                "contract_allowlist",
                "function_allowlist",
                "recipient_allowlist"
            ]
        );
    }

    #[test]
    fn the_wrong_network_is_refused() {
        let (_, inspected) = inspect_transaction(
            &testing::payment_envelope(Default::default()),
            UnknownCallPolicy::Refuse,
        )
        .unwrap();
        let decision = evaluate_transaction(
            &permissive(),
            &TransactionContext {
                transaction: &inspected,
                network_passphrase: "Public Global Stellar Network ; September 2015",
                signing_address: &testing::AGENT,
                now: 1_700_000_000,
            },
        );
        assert_eq!(rules(&decision), ["network"]);
    }

    #[test]
    fn a_transaction_from_another_account_is_refused() {
        let decision = decide(
            &permissive(),
            &testing::payment_envelope(testing::PaymentSpec {
                source: testing::STRANGER.clone(),
                ..Default::default()
            }),
        );
        assert!(rules(&decision).contains(&"source_account"));
    }

    #[test]
    fn an_unbounded_envelope_is_refused_by_default() {
        let decision = decide(
            &permissive(),
            &testing::payment_envelope(testing::PaymentSpec {
                max_time: 0,
                ..Default::default()
            }),
        );
        assert!(rules(&decision).contains(&"bounded_expiry"));
    }

    #[test]
    fn many_small_payments_can_still_breach_the_transaction_cap() {
        // The reason a per-amount cap is not enough on its own: a hundred
        // payments each under the cap is still a hundred payments.
        let policy = Policy {
            max_amount_stroops: Some("10000000".into()),
            max_transaction_stroops: Some("15000000".into()),
            ..permissive()
        };
        let decision = decide(&policy, &testing::multi_payment_envelope(5, 10_000_000));
        assert!(
            rules(&decision).contains(&"transaction_cap"),
            "{decision:?}"
        );
    }

    #[test]
    fn a_configured_ceiling_does_not_trip_the_spend_cap() {
        // set_limits carries three i128 ceilings that are not spends.
        let policy = Policy {
            max_amount_stroops: Some("1".into()),
            ..permissive()
        };
        let decision = decide(&policy, &testing::set_limits_envelope(50_000_000_000));
        assert!(decision.is_allowed(), "{decision:?}");
    }

    #[test]
    fn a_negative_amount_is_refused_regardless_of_the_cap() {
        let policy = Policy {
            max_amount_stroops: None, // no cap at all
            ..permissive()
        };
        let decision = decide(
            &policy,
            &testing::payment_envelope(testing::PaymentSpec {
                amount: -1,
                ..Default::default()
            }),
        );
        assert!(rules(&decision).contains(&"amount_cap"), "{decision:?}");
    }

    // ── Auth entries ────────────────────────────────────────────────────────

    fn auth_decision(policy: &Policy, requested: u32, current: u32) -> Decision {
        let (_, inspected) = crate::inspect::inspect_auth_entry(
            &testing::auth_entry_xdr(1, 1_000_000),
            UnknownCallPolicy::Refuse,
        )
        .unwrap();
        evaluate_auth_entry(
            policy,
            &AuthEntryContext {
                entry: &inspected,
                network_passphrase: testing::NETWORK_PASSPHRASE,
                signing_address: &testing::AGENT,
                requested_valid_until: requested,
                current_ledger: current,
                now: 1_700_000_000,
            },
        )
    }

    #[test]
    fn an_auth_entry_within_the_validity_cap_is_allowed() {
        assert!(auth_decision(&permissive(), 1_100, 1_000).is_allowed());
    }

    #[test]
    fn an_over_long_auth_validity_is_refused_not_clamped() {
        // The one control on how long a leaked signature stays replayable.
        // Refusing rather than clamping means the caller knows what they got.
        let decision = auth_decision(&permissive(), 100_000, 1_000);
        assert_eq!(rules(&decision), ["auth_validity"]);
        assert!(decision.violations()[0].detail.contains("100000"));
    }

    // ── Time windows ────────────────────────────────────────────────────────

    fn at(hour: u64) -> UnixSeconds {
        // 2023-11-14 was a Tuesday; 1_699_920_000 is midnight UTC that day.
        1_699_920_000 + hour * 3_600
    }

    fn window_decision(window: TimeWindow, now: UnixSeconds) -> Decision {
        let policy = Policy {
            time_window: Some(window),
            ..permissive()
        };
        let (_, inspected) = inspect_transaction(
            &testing::payment_envelope(Default::default()),
            UnknownCallPolicy::Refuse,
        )
        .unwrap();
        evaluate_transaction(
            &policy,
            &TransactionContext {
                transaction: &inspected,
                network_passphrase: testing::NETWORK_PASSPHRASE,
                signing_address: &testing::AGENT,
                now,
            },
        )
    }

    #[test]
    fn signing_outside_permitted_hours_is_refused() {
        let window = TimeWindow {
            start_hour_utc: 9,
            end_hour_utc: 17,
            ..Default::default()
        };
        assert!(window_decision(window.clone(), at(12)).is_allowed());
        assert!(!window_decision(window.clone(), at(8)).is_allowed());
        assert!(
            !window_decision(window.clone(), at(17)).is_allowed(),
            "end is exclusive"
        );
        assert!(window_decision(window, at(16)).is_allowed());
    }

    #[test]
    fn signing_on_a_disallowed_day_is_refused() {
        let weekdays = TimeWindow {
            days: vec![
                "mon".into(),
                "tue".into(),
                "wed".into(),
                "thu".into(),
                "fri".into(),
            ],
            ..Default::default()
        };
        // at(12) is a Tuesday.
        assert!(window_decision(weekdays.clone(), at(12)).is_allowed());
        // ...and five days later is a Sunday.
        assert!(!window_decision(weekdays, at(12) + 5 * 86_400).is_allowed());
    }

    #[test]
    fn an_absolute_validity_period_is_enforced() {
        let window = TimeWindow {
            not_before: Some(at(10)),
            not_after: Some(at(14)),
            ..Default::default()
        };
        assert!(!window_decision(window.clone(), at(9)).is_allowed());
        assert!(window_decision(window.clone(), at(12)).is_allowed());
        assert!(!window_decision(window, at(15)).is_allowed());
    }

    // ── Rate limits ─────────────────────────────────────────────────────────

    fn rate_limited() -> Policy {
        Policy {
            rate_limit: Some(RateLimit {
                window_seconds: 3_600,
                max_requests: 3,
                max_amount_stroops: Some("100".into()),
            }),
            ..permissive()
        }
    }

    #[test]
    fn a_request_budget_is_enforced_across_a_window() {
        let policy = rate_limited();
        let state = RateLimitState::new();
        let subject = Subject::new("agent-1");
        let now = 1_700_000_000;

        for _ in 0..3 {
            assert!(state.check(&policy, &subject, 1, now).is_none());
            state.commit(&policy, &subject, 1, now);
        }
        let violation = state.check(&policy, &subject, 1, now).unwrap();
        assert_eq!(violation.rule, "rate_limit");
        assert!(violation.detail.contains("3 of 3"), "{}", violation.detail);
    }

    #[test]
    fn a_refused_request_does_not_burn_budget() {
        // The property that stops a stolen token denying service: only
        // requests that were actually signed consume the allowance.
        let policy = rate_limited();
        let state = RateLimitState::new();
        let subject = Subject::new("agent-1");
        let now = 1_700_000_000;

        for _ in 0..50 {
            assert!(
                state.check(&policy, &subject, 1, now).is_none(),
                "checking must not consume"
            );
        }
        // The legitimate agent still has its full allowance.
        state.commit(&policy, &subject, 1, now);
        assert!(state.check(&policy, &subject, 1, now).is_none());
    }

    #[test]
    fn a_cumulative_amount_budget_is_enforced() {
        let policy = rate_limited();
        let state = RateLimitState::new();
        let subject = Subject::new("agent-1");
        let now = 1_700_000_000;

        state.commit(&policy, &subject, 60, now);
        assert!(state.check(&policy, &subject, 40, now).is_none());
        let violation = state.check(&policy, &subject, 41, now).unwrap();
        assert!(violation.detail.contains("101"), "{}", violation.detail);
    }

    #[test]
    fn the_window_rolls_over() {
        let policy = rate_limited();
        let state = RateLimitState::new();
        let subject = Subject::new("agent-1");
        let now = 1_700_000_000;

        for _ in 0..3 {
            state.commit(&policy, &subject, 1, now);
        }
        assert!(state.check(&policy, &subject, 1, now).is_some());
        assert!(state.check(&policy, &subject, 1, now + 3_600).is_none());
    }

    #[test]
    fn budgets_are_per_subject() {
        let policy = rate_limited();
        let state = RateLimitState::new();
        let now = 1_700_000_000;

        for _ in 0..3 {
            state.commit(&policy, &Subject::new("noisy"), 1, now);
        }
        assert!(state
            .check(&policy, &Subject::new("noisy"), 1, now)
            .is_some());
        assert!(state
            .check(&policy, &Subject::new("quiet"), 1, now)
            .is_none());
    }

    // ── Validation ──────────────────────────────────────────────────────────

    #[test]
    fn a_policy_with_no_networks_is_refused_at_load() {
        // It would refuse every request; better to say so at startup.
        let policy = Policy::default();
        let error = policy.validate("empty").unwrap_err();
        assert!(error.contains("networks"), "{error}");
    }

    #[test]
    fn an_unparseable_ceiling_fails_at_load_not_at_the_first_payment() {
        let policy = Policy {
            max_amount_stroops: Some("ten million".into()),
            ..permissive()
        };
        let error = policy.validate("bad").unwrap_err();
        assert!(error.contains("max_amount_stroops"), "{error}");
    }

    #[test]
    fn ceilings_beyond_i64_are_accepted_because_they_are_strings() {
        // The reason amounts are strings: i128 stroops do not fit in a TOML
        // integer, and silently wrapping a ceiling would be catastrophic.
        let huge = (i64::MAX as i128 * 10).to_string();
        let policy = Policy {
            max_amount_stroops: Some(huge.clone()),
            ..permissive()
        };
        assert!(policy.validate("huge").is_ok());
        assert_eq!(policy.amount_cap(), Some(huge.parse().unwrap()));
    }

    #[test]
    fn a_bad_weekday_is_refused_at_load() {
        let policy = Policy {
            time_window: Some(TimeWindow {
                days: vec!["funday".into()],
                ..Default::default()
            }),
            ..permissive()
        };
        let error = policy.validate("days").unwrap_err();
        assert!(error.contains("funday"), "{error}");
    }

    #[test]
    fn the_derived_and_parsed_defaults_for_a_time_window_agree() {
        // A `#[derive(Default)]` here would give end_hour_utc = 0 and silently
        // deny every request, while a policy file with an empty
        // [time_window] section allowed them. Two defaults for one field is
        // exactly the kind of divergence that only shows up in production.
        let parsed: TimeWindow = toml::from_str("").unwrap();
        assert_eq!(parsed, TimeWindow::default());
        assert_eq!(TimeWindow::default().end_hour_utc, 24);
    }

    #[test]
    fn an_empty_time_window_permits_every_hour() {
        assert!(window_decision(TimeWindow::default(), at(0)).is_allowed());
        assert!(window_decision(TimeWindow::default(), at(23)).is_allowed());
    }

    #[test]
    fn a_zero_length_rate_limit_window_is_refused() {
        let policy = Policy {
            rate_limit: Some(RateLimit {
                window_seconds: 0,
                max_requests: 1,
                max_amount_stroops: None,
            }),
            ..permissive()
        };
        assert!(policy.validate("zero").is_err());
    }
}
