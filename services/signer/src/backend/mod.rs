//! Where keys actually live.
//!
//! # The trait is small on purpose
//!
//! Both Stellar signing operations reduce to "Ed25519 over these 32 bytes"
//! (see [`crate::stellar`]), so a backend needs exactly three things: report a
//! public key, sign a payload, and say whether it is reachable.
//!
//! **There is no method that can return private key material**, and no
//! `export` / `unwrap` / `raw_key` escape hatch. `docs/signing.md` lists "never
//! export the key" as a service responsibility; making it a property of the
//! trait means it is enforced by the compiler rather than by a rule someone has
//! to remember. A backend that wanted to leak a key would have to change this
//! file, which is a reviewable event in a way that a new method on an
//! implementation is not.
//!
//! The fixed-size `[u8; 32]` payload is load-bearing too. A `&[u8]` would let a
//! caller-controlled length reach an HSM, and some PKCS#11 implementations
//! behave differently — or worse, pre-hash — on an unexpected input size.
//!
//! # Adapters
//!
//! - [`local`] — an encrypted file keystore. **Development and testnet only**,
//!   and it refuses to load without an explicit acknowledgement.
//! - [`aws_kms`] — AWS KMS, `ECC_NIST_EDWARDS25519` / `ED25519_SHA_512`.
//! - [`gcp_kms`] — GCP Cloud KMS, `EC_SIGN_ED25519`.
//!
//! [`conformance`] is the harness every adapter must pass.

pub mod aws_kms;
pub mod conformance;
pub mod gcp_kms;
pub mod local;
pub mod spki;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{RefusalReason, ServiceError};

/// Which key, in which backend.
///
/// Resolved from the caller's identity by [`crate::registry`]. A caller never
/// supplies one of these — that is the point of the registry being a one-way
/// lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyRef {
    /// Which backend holds it — matches [`SigningBackend::id`].
    pub backend: String,
    /// The backend's own identifier: a key ARN, a Cloud KMS resource name, or
    /// a keystore alias.
    pub key_id: String,
}

impl KeyRef {
    /// Build a reference.
    pub fn new(backend: impl Into<String>, key_id: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            key_id: key_id.into(),
        }
    }
}

impl std::fmt::Display for KeyRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.backend, self.key_id)
    }
}

/// What a backend can fail with.
///
/// The variants are what the *caller* needs to distinguish: a missing key is
/// a configuration error someone must fix, whereas an unavailable backend is
/// worth retrying. Everything else collapses to [`BackendError::Backend`] —
/// a signing service should not narrate its cloud provider's error taxonomy to
/// an agent.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// No key with that reference.
    #[error("no key {0} in this backend")]
    UnknownKey(KeyRef),

    /// The key exists but is not an Ed25519 signing key.
    ///
    /// Worth its own variant because it is the most likely misconfiguration:
    /// AWS KMS will happily hand back an ECDSA P-256 key, whose signatures are
    /// structurally valid and useless to Stellar.
    #[error("key {key} is not an Ed25519 signing key: {detail}")]
    WrongKeyType {
        /// The offending key.
        key: KeyRef,
        /// What it turned out to be.
        detail: String,
    },

    /// The backend could not be reached, or refused.
    #[error("backend unavailable: {0}")]
    Unavailable(String),

    /// The backend answered, but with something unusable.
    #[error("backend returned an unusable response: {0}")]
    Backend(String),
}

impl From<BackendError> for ServiceError {
    fn from(error: BackendError) -> Self {
        // Deliberately uniform on the wire. An agent has no use for the
        // difference between "the ARN is wrong" and "KMS is down", and a
        // prober should not learn our key layout from error text. The detail
        // goes to the log via `with_internal`.
        let reason = match &error {
            BackendError::UnknownKey(_) | BackendError::WrongKeyType { .. } => {
                RefusalReason::Internal
            }
            BackendError::Unavailable(_) | BackendError::Backend(_) => {
                RefusalReason::BackendUnavailable
            }
        };
        ServiceError::new(reason, "the signing backend is unavailable")
            .with_internal(error.to_string())
    }
}

/// Somewhere a private key lives and can be used without leaving.
#[async_trait]
pub trait SigningBackend: Send + Sync {
    /// A stable identifier, matched against [`KeyRef::backend`].
    fn id(&self) -> &str;

    /// The raw Ed25519 public key for `key`.
    ///
    /// Implementations should treat this as cacheable — it cannot change for a
    /// given key — but must not cache a *failure*.
    async fn public_key(&self, key: &KeyRef) -> Result<[u8; 32], BackendError>;

    /// Sign 32 bytes with PureEdDSA Ed25519.
    ///
    /// `payload` is already the final message: `SHA-256` of a transaction
    /// signature payload or of an authorisation preimage. Implementations must
    /// **not** hash it again — a backend configured for a pre-hashed variant
    /// (AWS's `ED25519_PH_SHA_512`) produces a structurally valid signature
    /// that Stellar rejects, and [`conformance`] exists partly to catch that.
    async fn sign(&self, key: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError>;

    /// Whether the backend is reachable. Used by `/readyz`.
    async fn health(&self) -> Result<(), BackendError>;
}

/// A set of backends, keyed by [`SigningBackend::id`].
pub struct BackendRegistry {
    backends: Vec<Box<dyn SigningBackend>>,
}

impl BackendRegistry {
    /// Build a registry from the configured backends.
    pub fn new(backends: Vec<Box<dyn SigningBackend>>) -> Self {
        Self { backends }
    }

    /// Look up the backend a key reference names.
    pub fn get(&self, key: &KeyRef) -> Result<&dyn SigningBackend, BackendError> {
        self.backends
            .iter()
            .find(|backend| backend.id() == key.backend)
            .map(|backend| backend.as_ref())
            .ok_or_else(|| BackendError::UnknownKey(key.clone()))
    }

    /// Every configured backend's id.
    pub fn ids(&self) -> Vec<&str> {
        self.backends.iter().map(|backend| backend.id()).collect()
    }

    /// Whether every backend is reachable.
    pub async fn health(&self) -> Result<(), BackendError> {
        for backend in &self.backends {
            backend.health().await?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for BackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendRegistry")
            .field("backends", &self.ids())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RefusalReason;

    struct Stub(&'static str);

    #[async_trait]
    impl SigningBackend for Stub {
        fn id(&self) -> &str {
            self.0
        }
        async fn public_key(&self, _: &KeyRef) -> Result<[u8; 32], BackendError> {
            Ok([1u8; 32])
        }
        async fn sign(&self, _: &KeyRef, _: &[u8; 32]) -> Result<[u8; 64], BackendError> {
            Ok([2u8; 64])
        }
        async fn health(&self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[test]
    fn a_registry_resolves_by_backend_id() {
        let registry = BackendRegistry::new(vec![Box::new(Stub("local")), Box::new(Stub("aws"))]);
        assert!(registry.get(&KeyRef::new("aws", "arn:...")).is_ok());
        assert!(registry.get(&KeyRef::new("gcp", "projects/...")).is_err());
        assert_eq!(registry.ids(), ["local", "aws"]);
    }

    #[test]
    fn backend_failures_do_not_narrate_themselves_to_the_caller() {
        // A prober must not learn our key layout from an error body.
        let error: ServiceError = BackendError::UnknownKey(KeyRef::new("aws", "arn:secret")).into();
        assert_eq!(error.message(), "the signing backend is unavailable");
        assert!(!error.message().contains("arn:secret"));
        // ...but an operator reading the log gets the detail.
        assert!(error.internal_detail().unwrap().contains("arn:secret"));
    }

    #[test]
    fn a_misconfigured_key_is_our_problem_not_the_callers() {
        // An ECDSA key where an Ed25519 one was expected is a deployment bug;
        // reporting it as 503 would invite an agent to retry forever.
        let error: ServiceError = BackendError::WrongKeyType {
            key: KeyRef::new("aws", "arn:..."),
            detail: "ECC_NIST_P256".into(),
        }
        .into();
        assert_eq!(error.reason(), RefusalReason::Internal);

        let error: ServiceError = BackendError::Unavailable("timeout".into()).into();
        assert_eq!(error.reason(), RefusalReason::BackendUnavailable);
    }

    #[test]
    fn key_refs_render_readably_for_logs() {
        assert_eq!(
            KeyRef::new("aws", "arn:aws:kms:...").to_string(),
            "aws:arn:aws:kms:..."
        );
    }
}
