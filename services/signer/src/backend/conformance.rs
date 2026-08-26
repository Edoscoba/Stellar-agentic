//! The harness every [`SigningBackend`] must pass.
//!
//! A backend is a piece of code that decides what a private key does. "It
//! compiled" is not evidence that it is correct, and the failure mode of a
//! subtly wrong one is a signature that is structurally valid, passes every
//! local test, and is rejected on-chain — or worse, is valid but authorises
//! something other than what was intended.
//!
//! This module is exported from the crate so a third-party backend can run the
//! same checks:
//!
//! ```no_run
//! # use stellaragent_signer::backend::{conformance, local::LocalKeystore, KeyRef};
//! # async fn example() {
//! let backend = LocalKeystore::single("k", [1u8; 32]);
//! conformance::assert_backend_conforms(&backend, &KeyRef::new("local", "k"))
//!     .await
//!     .expect("backend conforms");
//! # }
//! ```
//!
//! # The check that matters most
//!
//! [`assert_backends_agree`] takes two backends holding *the same key* and
//! asserts they produce byte-identical signatures for the same payload. Ed25519
//! is deterministic (RFC 8032), so this is a legitimate equality check rather
//! than a flaky one — and it is the only thing that catches a backend
//! configured for a pre-hashed variant. AWS KMS offers both `ED25519_SHA_512`
//! (raw message, correct here) and `ED25519_PH_SHA_512` (pre-hashed, wrong
//! here); the latter yields a perfectly well-formed signature over the wrong
//! message, which nothing but a cross-backend comparison or an on-chain
//! submission will reveal.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::{BackendError, KeyRef, SigningBackend};

/// A conformance failure, naming the property that did not hold.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConformanceFailure {
    /// The backend errored where it should have succeeded.
    #[error("{property}: backend returned an error: {detail}")]
    Errored {
        /// Which property was being checked.
        property: &'static str,
        /// The backend's error.
        detail: String,
    },

    /// A property did not hold.
    #[error("{property}: {detail}")]
    Failed {
        /// Which property was being checked.
        property: &'static str,
        /// What went wrong.
        detail: String,
    },
}

impl ConformanceFailure {
    fn failed(property: &'static str, detail: impl Into<String>) -> Self {
        Self::Failed {
            property,
            detail: detail.into(),
        }
    }

    fn errored(property: &'static str, error: BackendError) -> Self {
        Self::Errored {
            property,
            detail: error.to_string(),
        }
    }
}

/// The result of a conformance run.
pub type ConformanceResult = Result<(), ConformanceFailure>;

/// Every property a backend must satisfy, checked against one key it holds.
///
/// | Property | Why it matters |
/// |---|---|
/// | the public key is stable | a key that changes between calls would make signatures unverifiable |
/// | the public key is a valid Ed25519 point | a truncated or wrong-type key produces signatures that verify against nothing |
/// | signatures verify under it | the minimum bar |
/// | signing is deterministic | RFC 8032 requires it; a backend adding randomness is not Ed25519 |
/// | distinct payloads give distinct signatures | catches a backend that ignores its input |
/// | an unknown key errors | catches a backend with a default-key fallback |
/// | concurrent signing is safe | the service signs from many tasks at once |
pub async fn assert_backend_conforms<B: SigningBackend + ?Sized>(
    backend: &B,
    key: &KeyRef,
) -> ConformanceResult {
    // ── The public key is stable and well-formed ────────────────────────────
    let public = backend
        .public_key(key)
        .await
        .map_err(|error| ConformanceFailure::errored("public_key", error))?;

    let again = backend
        .public_key(key)
        .await
        .map_err(|error| ConformanceFailure::errored("public_key/stable", error))?;
    if public != again {
        return Err(ConformanceFailure::failed(
            "public_key/stable",
            "two calls returned different keys",
        ));
    }

    let verifying = VerifyingKey::from_bytes(&public).map_err(|error| {
        ConformanceFailure::failed(
            "public_key/valid",
            format!(
                "not a valid Ed25519 public key: {error}. A KMS key created with an \
                     ECDSA or RSA key spec produces this."
            ),
        )
    })?;

    // ── Signatures verify, and are deterministic ────────────────────────────
    let payload = [0x5au8; 32];
    let signature = backend
        .sign(key, &payload)
        .await
        .map_err(|error| ConformanceFailure::errored("sign", error))?;

    verifying
        .verify(&payload, &Signature::from_bytes(&signature))
        .map_err(|error| {
            ConformanceFailure::failed(
                "sign/verifies",
                format!(
                    "signature does not verify under the reported public key: {error}. \
                     A backend configured for a pre-hashed variant (AWS ED25519_PH_SHA_512) \
                     fails exactly here."
                ),
            )
        })?;

    let repeat = backend
        .sign(key, &payload)
        .await
        .map_err(|error| ConformanceFailure::errored("sign/deterministic", error))?;
    if repeat != signature {
        return Err(ConformanceFailure::failed(
            "sign/deterministic",
            "the same payload produced two different signatures; Ed25519 (RFC 8032) is \
             deterministic, so this backend is doing something else",
        ));
    }

    // ── Different payloads are actually different ───────────────────────────
    let other_payload = [0xa5u8; 32];
    let other = backend
        .sign(key, &other_payload)
        .await
        .map_err(|error| ConformanceFailure::errored("sign/distinct", error))?;
    if other == signature {
        return Err(ConformanceFailure::failed(
            "sign/distinct",
            "two different payloads produced the same signature; the backend is ignoring \
             its input",
        ));
    }
    verifying
        .verify(&other_payload, &Signature::from_bytes(&other))
        .map_err(|error| ConformanceFailure::failed("sign/distinct-verifies", error.to_string()))?;

    // ── An unknown key must not fall back to a default ──────────────────────
    let unknown = KeyRef::new(
        key.backend.clone(),
        format!("{}-definitely-not-a-real-key", key.key_id),
    );
    if backend.public_key(&unknown).await.is_ok() {
        return Err(ConformanceFailure::failed(
            "unknown_key",
            "an unknown key reference resolved; a backend must not fall back to a default key",
        ));
    }
    if backend.sign(&unknown, &payload).await.is_ok() {
        return Err(ConformanceFailure::failed(
            "unknown_key/sign",
            "signing succeeded for an unknown key reference",
        ));
    }

    // ── Health ──────────────────────────────────────────────────────────────
    backend
        .health()
        .await
        .map_err(|error| ConformanceFailure::errored("health", error))?;

    Ok(())
}

/// Two backends holding the same key must produce byte-identical signatures.
///
/// This is the cross-backend equality Phase 2 asks for, and the only local
/// check that catches a pre-hashing misconfiguration. Run it with a local
/// keystore as the reference and a cloud backend as the subject.
pub async fn assert_backends_agree<A, B>(
    reference: &A,
    reference_key: &KeyRef,
    subject: &B,
    subject_key: &KeyRef,
) -> ConformanceResult
where
    A: SigningBackend + ?Sized,
    B: SigningBackend + ?Sized,
{
    let reference_public = reference
        .public_key(reference_key)
        .await
        .map_err(|error| ConformanceFailure::errored("agree/reference_public_key", error))?;
    let subject_public = subject
        .public_key(subject_key)
        .await
        .map_err(|error| ConformanceFailure::errored("agree/subject_public_key", error))?;

    if reference_public != subject_public {
        return Err(ConformanceFailure::failed(
            "agree/public_key",
            "the two backends do not hold the same key, so their signatures cannot be \
             compared. Point both at the same key material before running this.",
        ));
    }

    // A spread of payloads rather than one: an all-zero or all-one payload can
    // mask an off-by-one in a length or offset calculation.
    for payload in [
        [0u8; 32],
        [0xffu8; 32],
        [0x5au8; 32],
        *b"stellaragent conformance vector!",
    ] {
        let expected = reference
            .sign(reference_key, &payload)
            .await
            .map_err(|error| ConformanceFailure::errored("agree/reference_sign", error))?;
        let actual = subject
            .sign(subject_key, &payload)
            .await
            .map_err(|error| ConformanceFailure::errored("agree/subject_sign", error))?;

        if expected != actual {
            return Err(ConformanceFailure::failed(
                "agree/signature",
                format!(
                    "signatures differ for payload {}.\n  reference: {}\n  subject:   {}\n\
                     Ed25519 is deterministic, so this means the two backends are signing \
                     different messages — most likely the subject is pre-hashing (AWS \
                     ED25519_PH_SHA_512 rather than ED25519_SHA_512 with MessageType RAW).",
                    hex::encode(payload),
                    hex::encode(expected),
                    hex::encode(actual),
                ),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::local::{LocalKeystore, BACKEND_ID};
    use async_trait::async_trait;

    fn key() -> KeyRef {
        KeyRef::new(BACKEND_ID, "conformance")
    }

    #[tokio::test]
    async fn the_reference_backend_conforms() {
        let backend = LocalKeystore::single("conformance", [4u8; 32]);
        assert_backend_conforms(&backend, &key()).await.unwrap();
    }

    #[tokio::test]
    async fn two_keystores_holding_the_same_key_agree() {
        let a = LocalKeystore::single("conformance", [4u8; 32]);
        let b = LocalKeystore::single("other-alias", [4u8; 32]);
        assert_backends_agree(&a, &key(), &b, &KeyRef::new(BACKEND_ID, "other-alias"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn backends_holding_different_keys_are_reported_as_such() {
        let a = LocalKeystore::single("conformance", [4u8; 32]);
        let b = LocalKeystore::single("conformance", [5u8; 32]);
        let failure = assert_backends_agree(&a, &key(), &b, &key())
            .await
            .unwrap_err();
        assert!(matches!(
            failure,
            ConformanceFailure::Failed {
                property: "agree/public_key",
                ..
            }
        ));
    }

    // ── Backends that are wrong in each of the ways the harness exists to catch ──

    /// Signs `SHA-256(payload)` instead of `payload` — the pre-hashing bug.
    struct PreHashingBackend(ed25519_dalek::SigningKey);

    #[async_trait]
    impl SigningBackend for PreHashingBackend {
        fn id(&self) -> &str {
            "prehash"
        }
        async fn public_key(&self, _: &KeyRef) -> Result<[u8; 32], BackendError> {
            Ok(self.0.verifying_key().to_bytes())
        }
        async fn sign(&self, _: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError> {
            use ed25519_dalek::Signer;
            use sha2::{Digest, Sha256};
            let digest: [u8; 32] = Sha256::digest(payload).into();
            Ok(self.0.sign(&digest).to_bytes())
        }
        async fn health(&self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_pre_hashing_backend_is_caught() {
        // This is the headline case: the signature is structurally perfect and
        // over the wrong message. Only verification against the stated payload
        // reveals it.
        let backend = PreHashingBackend(ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]));
        let failure = assert_backend_conforms(&backend, &KeyRef::new("prehash", "k"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                failure,
                ConformanceFailure::Failed {
                    property: "sign/verifies",
                    ..
                }
            ),
            "{failure:?}"
        );
    }

    #[tokio::test]
    async fn a_pre_hashing_backend_is_also_caught_by_cross_backend_comparison() {
        let reference = LocalKeystore::single("conformance", [4u8; 32]);
        let subject = PreHashingBackend(ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]));
        let failure =
            assert_backends_agree(&reference, &key(), &subject, &KeyRef::new("prehash", "k"))
                .await
                .unwrap_err();
        let rendered = failure.to_string();
        assert!(rendered.contains("ED25519_PH_SHA_512"), "{rendered}");
    }

    /// Falls back to a default key for any reference — the "helpful" backend.
    struct DefaultingBackend(ed25519_dalek::SigningKey);

    #[async_trait]
    impl SigningBackend for DefaultingBackend {
        fn id(&self) -> &str {
            "defaulting"
        }
        async fn public_key(&self, _: &KeyRef) -> Result<[u8; 32], BackendError> {
            Ok(self.0.verifying_key().to_bytes())
        }
        async fn sign(&self, _: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError> {
            use ed25519_dalek::Signer;
            Ok(self.0.sign(payload).to_bytes())
        }
        async fn health(&self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_backend_that_signs_for_any_key_reference_is_caught() {
        // Signing under the wrong key on a typo'd reference is a way to move
        // the wrong agent's funds.
        let backend = DefaultingBackend(ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]));
        let failure = assert_backend_conforms(&backend, &KeyRef::new("defaulting", "k"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                failure,
                ConformanceFailure::Failed {
                    property: "unknown_key",
                    ..
                }
            ),
            "{failure:?}"
        );
    }

    /// Ignores the payload entirely.
    struct ConstantBackend(ed25519_dalek::SigningKey);

    #[async_trait]
    impl SigningBackend for ConstantBackend {
        fn id(&self) -> &str {
            "constant"
        }
        async fn public_key(&self, _: &KeyRef) -> Result<[u8; 32], BackendError> {
            Ok(self.0.verifying_key().to_bytes())
        }
        async fn sign(&self, key: &KeyRef, _: &[u8; 32]) -> Result<[u8; 64], BackendError> {
            use ed25519_dalek::Signer;
            if key.key_id.contains("definitely-not-a-real-key") {
                return Err(BackendError::UnknownKey(key.clone()));
            }
            Ok(self.0.sign(&[0u8; 32]).to_bytes())
        }
        async fn health(&self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_backend_that_ignores_its_payload_is_caught() {
        let backend = ConstantBackend(ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]));
        let failure = assert_backend_conforms(&backend, &KeyRef::new("constant", "k"))
            .await
            .unwrap_err();
        // It fails at the first verify, because the constant signature is over
        // a payload that is not the one we asked about.
        assert!(
            matches!(failure, ConformanceFailure::Failed { .. }),
            "{failure:?}"
        );
    }
}
