//! Which key an identity may use, and under which policy.
//!
//! # The lookup only goes one way
//!
//! A caller authenticates as a [`Subject`] and the registry answers with a
//! [`KeyRef`] and a policy name. There is no endpoint, parameter or header
//! that lets a caller *name* a key — which is why the protocol's
//! `GET /v1/public-key` takes no arguments.
//!
//! That is the property that contains a stolen token: it buys the ability to
//! request signatures from one key, subject to one policy, and no ability to
//! pivot to another agent's key by asking for it.
//!
//! # One key per subject
//!
//! Deliberately not a set. An agent that needs two keys is two identities with
//! two tokens and two policies, which keeps "who signed this" answerable from
//! the audit log without a second field. Modelling it as a set would make the
//! common case ambiguous to save configuration in a rare one.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::auth::Subject;
use crate::backend::KeyRef;
use crate::error::{RefusalReason, Result, ServiceError};

/// The name of a policy in the policy file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PolicyName(pub String);

impl PolicyName {
    /// Build a policy name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PolicyName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a subject is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// The key this identity signs with.
    pub key: KeyRef,
    /// The policy evaluated for every request from this identity.
    pub policy: PolicyName,
}

/// Subject → key and policy.
#[derive(Debug, Default)]
pub struct KeyRegistry {
    entries: HashMap<Subject, Registration>,
}

impl KeyRegistry {
    /// Build a registry.
    pub fn new(entries: HashMap<Subject, Registration>) -> Self {
        Self { entries }
    }

    /// Resolve what `subject` may do.
    ///
    /// # Errors
    ///
    /// [`RefusalReason::NoKeyForSubject`] when the identity authenticated but
    /// has no key. That is a configuration gap — a token was issued and the
    /// registry entry was never added — and it is worth its own reason so it
    /// does not read as an authentication failure in the audit log.
    pub fn resolve(&self, subject: &Subject) -> Result<&Registration> {
        self.entries.get(subject).ok_or_else(|| {
            ServiceError::new(
                RefusalReason::NoKeyForSubject,
                "no signing key is registered for this identity",
            )
            .with_internal(format!(
                "subject {subject} authenticated but has no registry entry"
            ))
        })
    }

    /// Every registered subject, sorted — for startup logging and `/readyz`.
    pub fn subjects(&self) -> Vec<&Subject> {
        let mut subjects: Vec<&Subject> = self.entries.keys().collect();
        subjects.sort();
        subjects
    }

    /// Every distinct key this registry can reach.
    ///
    /// Used at startup to resolve public keys once and to fail fast on a key
    /// the backend does not hold.
    pub fn keys(&self) -> Vec<&KeyRef> {
        let mut keys: Vec<&KeyRef> = self.entries.values().map(|entry| &entry.key).collect();
        keys.sort_by_key(|key| (key.backend.clone(), key.key_id.clone()));
        keys.dedup();
        keys
    }

    /// How many identities are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether anything is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Subjects sharing a key, if any.
    ///
    /// Not an error — two agents deliberately sharing one funded account is a
    /// legitimate topology — but the startup path warns, because it makes
    /// "which agent spent this" unanswerable from the chain alone and shifts
    /// the burden entirely onto the audit log.
    pub fn shared_keys(&self) -> Vec<(&KeyRef, Vec<&Subject>)> {
        let mut by_key: HashMap<&KeyRef, Vec<&Subject>> = HashMap::new();
        for (subject, registration) in &self.entries {
            by_key.entry(&registration.key).or_default().push(subject);
        }
        let mut shared: Vec<(&KeyRef, Vec<&Subject>)> = by_key
            .into_iter()
            .filter(|(_, subjects)| subjects.len() > 1)
            .map(|(key, mut subjects)| {
                subjects.sort();
                (key, subjects)
            })
            .collect();
        shared.sort_by_key(|(key, _)| (key.backend.clone(), key.key_id.clone()));
        shared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> KeyRegistry {
        let mut entries = HashMap::new();
        entries.insert(
            Subject::new("agent-1"),
            Registration {
                key: KeyRef::new("local", "k1"),
                policy: PolicyName::new("default"),
            },
        );
        entries.insert(
            Subject::new("agent-2"),
            Registration {
                key: KeyRef::new("aws-kms", "arn:..."),
                policy: PolicyName::new("strict"),
            },
        );
        KeyRegistry::new(entries)
    }

    #[test]
    fn a_registered_subject_resolves_to_its_key_and_policy() {
        let registry = registry();
        let registration = registry.resolve(&Subject::new("agent-1")).unwrap();
        assert_eq!(registration.key, KeyRef::new("local", "k1"));
        assert_eq!(registration.policy, PolicyName::new("default"));
    }

    #[test]
    fn an_unregistered_subject_is_its_own_refusal_reason() {
        // Distinct from an auth failure: the token was real, the config is
        // incomplete, and the audit log should say so.
        let error = registry().resolve(&Subject::new("agent-3")).unwrap_err();
        assert_eq!(error.reason(), RefusalReason::NoKeyForSubject);
        assert!(error.internal_detail().unwrap().contains("agent-3"));
    }

    #[test]
    fn there_is_no_way_to_ask_for_another_subjects_key() {
        // Encoded as a type-level property: `resolve` takes a Subject, which
        // only ever comes from an authenticated token. If this test needs
        // changing, so does the security model.
        let registry = registry();
        let one = registry
            .resolve(&Subject::new("agent-1"))
            .unwrap()
            .key
            .clone();
        let two = registry
            .resolve(&Subject::new("agent-2"))
            .unwrap()
            .key
            .clone();
        assert_ne!(one, two);
    }

    #[test]
    fn keys_are_deduplicated_for_startup_resolution() {
        let mut entries = HashMap::new();
        for subject in ["a", "b", "c"] {
            entries.insert(
                Subject::new(subject),
                Registration {
                    key: KeyRef::new("local", "shared"),
                    policy: PolicyName::new("default"),
                },
            );
        }
        assert_eq!(KeyRegistry::new(entries).keys().len(), 1);
    }

    #[test]
    fn a_shared_key_is_detected_so_startup_can_warn() {
        // Legitimate but worth flagging: two agents on one account makes "who
        // spent this" unanswerable from the chain alone.
        let mut entries = HashMap::new();
        for subject in ["agent-a", "agent-b"] {
            entries.insert(
                Subject::new(subject),
                Registration {
                    key: KeyRef::new("local", "shared"),
                    policy: PolicyName::new("default"),
                },
            );
        }
        entries.insert(
            Subject::new("agent-c"),
            Registration {
                key: KeyRef::new("local", "own"),
                policy: PolicyName::new("default"),
            },
        );

        let registry = KeyRegistry::new(entries);
        let shared = registry.shared_keys();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].0, &KeyRef::new("local", "shared"));
        assert_eq!(
            shared[0].1,
            vec![&Subject::new("agent-a"), &Subject::new("agent-b")]
        );
    }

    #[test]
    fn subjects_are_listed_in_a_stable_order() {
        assert_eq!(
            registry().subjects(),
            vec![&Subject::new("agent-1"), &Subject::new("agent-2")]
        );
    }
}
