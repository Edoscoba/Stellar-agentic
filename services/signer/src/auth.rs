//! Who is calling, and are they still allowed to.
//!
//! # Bearer tokens, hashed, compared in constant time
//!
//! A token is 256 bits from the OS random source. The service stores only
//! `SHA-256(token)` and compares with [`subtle`], so a timing oracle cannot
//! walk a token out byte by byte, and a leaked store does not yield usable
//! credentials.
//!
//! ## Why not Argon2
//!
//! This will come up in review, so: password hashing exists to make
//! *low-entropy* secrets expensive to guess. A 256-bit random token has nothing
//! to guess — an attacker who can brute-force it can brute-force the key.
//! Putting a memory-hard KDF on the hot path of every signing request buys no
//! security and adds two real problems: tens of milliseconds of latency per
//! payment, and a memory-hard workload an unauthenticated caller can trigger,
//! which is a denial-of-service surface pointed at the one service every
//! payment depends on.
//!
//! The properties that actually matter here are constant-time comparison, a
//! high-entropy secret, and never writing the token anywhere. All three are
//! cheap.
//!
//! # Rotation is overlap, not swap
//!
//! An identity may hold several unexpired tokens at once, so a new one is
//! issued and adopted before the old one is revoked — there is no window in
//! which an agent cannot sign. Revocation is checked per request rather than
//! cached, so it takes effect on the next request rather than at the next
//! restart.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{RefusalReason, Result, ServiceError};

/// Who a caller is. Maps to exactly one key via [`crate::registry`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Subject(pub String);

impl Subject {
    /// Build a subject.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The subject's name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Seconds since the Unix epoch. A plain integer rather than a date-time type:
/// the only arithmetic this service does on time is comparison, and a
/// dependency for that is not worth its supply chain.
pub type UnixSeconds = u64;

/// A registered credential.
///
/// The token itself is never stored — only its hash. `Debug` is derived and
/// safe precisely because there is nothing secret in here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Stable identifier, safe to log and to name in an audit record.
    pub id: String,
    /// Which identity this token authenticates.
    pub subject: Subject,
    /// Lowercase hex `SHA-256` of the token.
    pub token_sha256: String,
    /// A human label — which deployment, which operator issued it.
    #[serde(default)]
    pub label: String,
    /// When it stops being valid. `None` means no expiry, which the config
    /// loader warns about.
    #[serde(default)]
    pub expires_at: Option<UnixSeconds>,
    /// When it was revoked, if it has been.
    #[serde(default)]
    pub revoked_at: Option<UnixSeconds>,
}

impl TokenRecord {
    /// Whether this credential is usable at `now`.
    pub fn is_valid_at(&self, now: UnixSeconds) -> bool {
        if let Some(revoked) = self.revoked_at {
            if now >= revoked {
                return false;
            }
        }
        match self.expires_at {
            Some(expiry) => now < expiry,
            None => true,
        }
    }

    /// Why this credential is not usable, for the audit log. `None` when valid.
    pub fn invalid_reason(&self, now: UnixSeconds) -> Option<&'static str> {
        if self.revoked_at.is_some_and(|revoked| now >= revoked) {
            return Some("revoked");
        }
        if self.expires_at.is_some_and(|expiry| now >= expiry) {
            return Some("expired");
        }
        None
    }
}

/// The hex `SHA-256` of a token, as stored.
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Generate a fresh 256-bit token, returned as lowercase hex.
///
/// Only used by the `issue-token` admin path and by tests — the service never
/// mints a token in response to a request.
pub fn generate_token() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// The set of credentials the service will accept.
#[derive(Debug, Default)]
pub struct TokenStore {
    /// Keyed by token hash, so a lookup is O(1) and does not iterate secrets.
    by_hash: HashMap<String, TokenRecord>,
}

impl TokenStore {
    /// Build a store from records.
    ///
    /// # Errors
    ///
    /// Rejects two records sharing a token hash. That can only mean the same
    /// token was registered for two identities, which would make "who called"
    /// ambiguous — and the audit log's whole value is that it is not.
    pub fn new(records: Vec<TokenRecord>) -> std::result::Result<Self, String> {
        let mut by_hash = HashMap::new();
        for record in records {
            let hash = record.token_sha256.to_ascii_lowercase();
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "token {} has a token_sha256 that is not a 64-character hex digest",
                    record.id
                ));
            }
            if let Some(existing) = by_hash.insert(hash, record.clone()) {
                return Err(format!(
                    "tokens {} and {} share a hash, so the caller's identity would be ambiguous",
                    existing.id, record.id
                ));
            }
        }
        Ok(Self { by_hash })
    }

    /// Resolve a presented token to the identity it authenticates.
    ///
    /// # Errors
    ///
    /// [`RefusalReason::Unauthenticated`] for a token we do not know, and
    /// [`RefusalReason::CredentialRejected`] for one we know but will not
    /// accept. The distinction is deliberate and safe: an attacker holding a
    /// revoked token already knows it was real.
    pub fn authenticate(&self, presented: &str, now: UnixSeconds) -> Result<&TokenRecord> {
        let hash = token_hash(presented);

        // The map lookup is not constant-time with respect to the *hash*, which
        // is fine — the hash is derived from the token by a one-way function,
        // so timing on it reveals nothing about the token. The constant-time
        // comparison below is what protects the token itself against a store
        // that ever gains a linear-scan path.
        let record = self.by_hash.get(&hash).ok_or_else(|| {
            ServiceError::new(
                RefusalReason::Unauthenticated,
                "unknown or missing credential",
            )
        })?;

        if record
            .token_sha256
            .as_bytes()
            .ct_eq(hash.as_bytes())
            .unwrap_u8()
            != 1
        {
            return Err(ServiceError::new(
                RefusalReason::Unauthenticated,
                "unknown or missing credential",
            ));
        }

        match record.invalid_reason(now) {
            None => Ok(record),
            Some(reason) => Err(ServiceError::new(
                RefusalReason::CredentialRejected,
                format!("credential {reason}"),
            )
            .with_internal(format!("token {} is {reason}", record.id))),
        }
    }

    /// Every token registered for `subject`, valid or not.
    pub fn tokens_for(&self, subject: &Subject) -> Vec<&TokenRecord> {
        let mut tokens: Vec<&TokenRecord> = self
            .by_hash
            .values()
            .filter(|record| &record.subject == subject)
            .collect();
        tokens.sort_by(|a, b| a.id.cmp(&b.id));
        tokens
    }

    /// How many credentials are registered.
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    /// Whether the store is empty — a service with no credentials can serve
    /// nobody, which the config loader treats as an error.
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

/// Pull a bearer token out of an `Authorization` header value.
///
/// Case-insensitive on the scheme, as RFC 7235 requires.
pub fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, value) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = value.trim();
    if token.is_empty() {
        return None;
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: UnixSeconds = 1_700_000_000;

    fn record(id: &str, token: &str) -> TokenRecord {
        TokenRecord {
            id: id.into(),
            subject: Subject::new("agent-1"),
            token_sha256: token_hash(token),
            label: "test".into(),
            expires_at: None,
            revoked_at: None,
        }
    }

    fn store(records: Vec<TokenRecord>) -> TokenStore {
        TokenStore::new(records).expect("valid store")
    }

    #[test]
    fn a_valid_token_resolves_to_its_subject() {
        let store = store(vec![record("t1", "secret-token")]);
        let authenticated = store.authenticate("secret-token", NOW).unwrap();
        assert_eq!(authenticated.subject, Subject::new("agent-1"));
        assert_eq!(authenticated.id, "t1");
    }

    #[test]
    fn an_unknown_token_is_unauthenticated() {
        let store = store(vec![record("t1", "secret-token")]);
        let error = store.authenticate("wrong", NOW).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::Unauthenticated);
        // The message must not confirm anything about what a valid token
        // looks like.
        assert_eq!(error.message(), "unknown or missing credential");
    }

    #[test]
    fn a_revoked_token_stops_working_immediately() {
        let mut revoked = record("t1", "secret-token");
        revoked.revoked_at = Some(NOW - 1);
        let store = store(vec![revoked]);

        let error = store.authenticate("secret-token", NOW).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::CredentialRejected);
        assert!(error.message().contains("revoked"));
    }

    #[test]
    fn revocation_is_not_retroactive_before_its_timestamp() {
        // A token revoked at T is valid at T-1: the audit log should not
        // disagree with itself about requests that happened before revocation.
        let mut revoked = record("t1", "secret-token");
        revoked.revoked_at = Some(NOW);
        let store = store(vec![revoked]);

        assert!(store.authenticate("secret-token", NOW - 1).is_ok());
        assert!(store.authenticate("secret-token", NOW).is_err());
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let mut expired = record("t1", "secret-token");
        expired.expires_at = Some(NOW);
        let store = store(vec![expired]);

        assert!(store.authenticate("secret-token", NOW - 1).is_ok());
        let error = store.authenticate("secret-token", NOW).unwrap_err();
        assert!(error.message().contains("expired"));
    }

    #[test]
    fn rotation_is_an_overlap_with_no_gap() {
        // The property that makes rotation safe: both tokens work at once, so
        // a fleet can adopt the new one before the old is revoked.
        let old = record("old", "old-token");
        let mut new = record("new", "new-token");
        new.expires_at = Some(NOW + 86_400);
        let overlapping = store(vec![old.clone(), new]);

        assert_eq!(
            overlapping.authenticate("old-token", NOW).unwrap().id,
            "old"
        );
        assert_eq!(
            overlapping.authenticate("new-token", NOW).unwrap().id,
            "new"
        );

        // Now retire the old one; the new one keeps working.
        let mut retired = old;
        retired.revoked_at = Some(NOW);
        let rotated = store(vec![retired, record("new", "new-token")]);
        assert!(rotated.authenticate("old-token", NOW).is_err());
        assert!(rotated.authenticate("new-token", NOW).is_ok());
    }

    #[test]
    fn two_tokens_sharing_a_hash_are_refused_at_load() {
        // Ambiguous identity would make the audit log useless.
        let mut duplicate = record("t2", "secret-token");
        duplicate.subject = Subject::new("agent-2");
        let error = TokenStore::new(vec![record("t1", "secret-token"), duplicate]).unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn a_malformed_hash_is_refused_at_load() {
        // Catches a config that pasted the token itself into token_sha256 — an
        // easy mistake that would otherwise store a plaintext credential.
        let bad = TokenRecord {
            token_sha256: "not-a-hash".into(),
            ..record("t1", "x")
        };
        let error = TokenStore::new(vec![bad]).unwrap_err();
        assert!(error.contains("64-character hex"), "{error}");
    }

    #[test]
    fn the_store_never_holds_the_token_itself() {
        let store = store(vec![record("t1", "secret-token")]);
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("secret-token"), "{rendered}");
        assert!(rendered.contains(&token_hash("secret-token")));
    }

    #[test]
    fn tokens_for_lists_an_identitys_credentials_in_a_stable_order() {
        let mut second = record("t2", "another");
        second.subject = Subject::new("agent-1");
        let store = store(vec![second, record("t1", "secret-token")]);
        let ids: Vec<&str> = store
            .tokens_for(&Subject::new("agent-1"))
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, ["t1", "t2"]);
        assert!(store.tokens_for(&Subject::new("nobody")).is_empty());
    }

    #[test]
    fn bearer_parsing_follows_rfc7235() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), Some("abc"));
        assert_eq!(bearer_token("BEARER abc"), Some("abc"));
        assert_eq!(bearer_token("Bearer   abc  "), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("Bearer"), None);
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token(""), None);
    }

    #[test]
    fn generated_tokens_are_256_bits_of_hex_and_do_not_repeat() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn hashing_is_the_plain_sha256_an_operator_can_reproduce() {
        // An operator issuing a token must be able to compute the hash with
        // `sha256sum`, so this must not be a bespoke construction.
        assert_eq!(
            token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
