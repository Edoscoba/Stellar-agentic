//! AWS KMS, via `ECC_NIST_EDWARDS25519` keys.
//!
//! # This was impossible until November 2025
//!
//! AWS KMS had no Ed25519 support at all until it added EdDSA. Before that, a
//! Stellar signing service on AWS had to use CloudHSM, or envelope-encrypt a
//! key and decrypt it in memory to sign — materially weaker, because the key
//! exists in plaintext in a process for the duration.
//!
//! The relevant key spec is `ECC_NIST_EDWARDS25519`, and the signing algorithm
//! must be **`ED25519_SHA_512` with `MessageType: RAW`**.
//!
//! # The one setting that will silently ruin everything
//!
//! AWS also offers `ED25519_PH_SHA_512`, the pre-hashed variant, used with
//! `MessageType: DIGEST`. Configure that by mistake and KMS returns a
//! structurally perfect 64-byte Ed25519 signature — over `SHA-512(payload)`
//! rather than over `payload`. Nothing local complains. Every transaction is
//! rejected on-chain with an authentication error that points nowhere near the
//! real cause.
//!
//! So this adapter hard-codes the pure variant, [`SIGNING_ALGORITHM`] is not
//! configurable, and [`super::conformance::assert_backends_agree`] exists in
//! large part to catch someone "fixing" it later.
//!
//! # Why the API is behind a trait
//!
//! [`AwsKmsApi`] is a two-method view of KMS. The adapter's logic — SPKI
//! parsing, length validation, error mapping — is the part that can be wrong,
//! and it is testable against a fake without an AWS account, on every CI run.
//! The real `aws-sdk-kms` client is wired in behind the `aws-kms` feature, so
//! the default build does not carry it.

use async_trait::async_trait;

use super::spki;
use super::{BackendError, KeyRef, SigningBackend};

/// The backend id AWS keys are addressed under.
pub const BACKEND_ID: &str = "aws-kms";

/// The only signing algorithm this adapter will use.
///
/// Not configurable, deliberately — see the module docs.
pub const SIGNING_ALGORITHM: &str = "ED25519_SHA_512";

/// The message type that goes with it: the raw message, not a digest.
pub const MESSAGE_TYPE: &str = "RAW";

/// The narrow slice of the KMS API this adapter needs.
///
/// Modelled on the wire shapes rather than on `aws-sdk-kms`'s types, so a fake
/// can reproduce them exactly and the adapter never sees an SDK type it might
/// accidentally depend on the behaviour of.
#[async_trait]
pub trait AwsKmsApi: Send + Sync {
    /// `GetPublicKey`, returning DER-encoded `SubjectPublicKeyInfo`.
    async fn get_public_key(&self, key_id: &str) -> Result<GetPublicKeyOutput, String>;

    /// `Sign` with [`SIGNING_ALGORITHM`] and [`MESSAGE_TYPE`].
    async fn sign(&self, key_id: &str, message: &[u8]) -> Result<Vec<u8>, String>;
}

/// What `GetPublicKey` gives back, in the parts that matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetPublicKeyOutput {
    /// DER-encoded `SubjectPublicKeyInfo`.
    pub public_key_der: Vec<u8>,
    /// The key spec AWS reports, e.g. `ECC_NIST_EDWARDS25519`.
    pub key_spec: String,
}

/// An AWS KMS-backed signer.
pub struct AwsKmsBackend<C: AwsKmsApi> {
    client: C,
}

impl<C: AwsKmsApi> std::fmt::Debug for AwsKmsBackend<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsKmsBackend").finish_non_exhaustive()
    }
}

impl<C: AwsKmsApi> AwsKmsBackend<C> {
    /// Wrap a KMS client.
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C: AwsKmsApi> SigningBackend for AwsKmsBackend<C> {
    fn id(&self) -> &str {
        BACKEND_ID
    }

    async fn public_key(&self, key: &KeyRef) -> Result<[u8; 32], BackendError> {
        let output = self
            .client
            .get_public_key(&key.key_id)
            .await
            .map_err(|error| classify(key, error))?;

        // Check the reported spec first: it gives a better message than "the
        // OID does not match", and it is the field an operator will recognise
        // from the console.
        if output.key_spec != "ECC_NIST_EDWARDS25519" {
            return Err(BackendError::WrongKeyType {
                key: key.clone(),
                detail: format!(
                    "KMS reports key spec {}, but Stellar needs ECC_NIST_EDWARDS25519",
                    output.key_spec
                ),
            });
        }

        spki::ed25519_from_der(&output.public_key_der).map_err(|error| BackendError::WrongKeyType {
            key: key.clone(),
            detail: error.to_string(),
        })
    }

    async fn sign(&self, key: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError> {
        let signature = self
            .client
            .sign(&key.key_id, payload)
            .await
            .map_err(|error| classify(key, error))?;

        // A wrong-length signature means the key was not Ed25519 after all —
        // an ECDSA signature is DER and variable-length. Catching it here
        // turns a confusing on-chain rejection into a clear local error.
        signature.as_slice().try_into().map_err(|_| {
            BackendError::Backend(format!(
                "KMS returned a {}-byte signature; an Ed25519 signature is 64 bytes. \
                 Check that {} was created with the ECC_NIST_EDWARDS25519 key spec.",
                signature.len(),
                key.key_id
            ))
        })
    }

    async fn health(&self) -> Result<(), BackendError> {
        // Deliberately not a KMS call. `/readyz` is unauthenticated and hit
        // constantly by load balancers; turning it into a paid API call per
        // probe is a way to be surprised by a bill and to rate-limit yourself
        // out of KMS. Reachability is proven by the first real signing request,
        // and the metrics in `crate::metrics` are what should alert.
        Ok(())
    }
}

/// Map a client error string onto the taxonomy.
///
/// String matching, because the alternative is depending on `aws-sdk-kms`'s
/// error enums here and losing the ability to test without it. The distinction
/// that matters is retryable vs not.
fn classify(key: &KeyRef, error: String) -> BackendError {
    let lower = error.to_ascii_lowercase();
    if lower.contains("notfound") || lower.contains("not found") {
        BackendError::UnknownKey(key.clone())
    } else if lower.contains("accessdenied") || lower.contains("not authorized") {
        // Not `UnknownKey`: an IAM misconfiguration is a different fix from a
        // wrong ARN, and conflating them costs an operator an hour.
        BackendError::Backend(format!("KMS denied access to {}: {error}", key.key_id))
    } else {
        BackendError::Unavailable(error)
    }
}

// ─── The real client, behind the feature flag ────────────────────────────────

#[cfg(feature = "aws-kms")]
mod real {
    use super::*;
    use aws_sdk_kms::primitives::Blob;
    use aws_sdk_kms::types::{MessageType, SigningAlgorithmSpec};

    #[async_trait]
    impl AwsKmsApi for aws_sdk_kms::Client {
        async fn get_public_key(&self, key_id: &str) -> Result<GetPublicKeyOutput, String> {
            let output = self
                .get_public_key()
                .key_id(key_id)
                .send()
                .await
                .map_err(|error| format!("{error:?}"))?;

            Ok(GetPublicKeyOutput {
                public_key_der: output
                    .public_key()
                    .map(|blob| blob.as_ref().to_vec())
                    .unwrap_or_default(),
                key_spec: output
                    .key_spec()
                    .map(|spec| spec.as_str().to_string())
                    .unwrap_or_default(),
            })
        }

        async fn sign(&self, key_id: &str, message: &[u8]) -> Result<Vec<u8>, String> {
            let output = self
                .sign()
                .key_id(key_id)
                .message(Blob::new(message.to_vec()))
                // Pinned, not configurable. See the module docs.
                .message_type(MessageType::Raw)
                .signing_algorithm(SigningAlgorithmSpec::from(SIGNING_ALGORITHM))
                .send()
                .await
                .map_err(|error| format!("{error:?}"))?;

            Ok(output
                .signature()
                .map(|blob| blob.as_ref().to_vec())
                .unwrap_or_default())
        }
    }
}

/// A fake KMS reproducing the documented response shapes.
///
/// Lives outside `mod tests` so the GCP adapter's cross-backend agreement test
/// can build an AWS backend too — checking that *all three* backends produce
/// identical bytes is more valuable than checking them pairwise in isolation.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// A fake reproducing KMS's documented response shapes.
    pub(crate) struct FakeKms {
        pub(crate) key: SigningKey,
        pub(crate) key_spec: String,
        /// Emulate the pre-hashed algorithm, to prove the harness catches it.
        pub(crate) pre_hash: bool,
    }

    /// A correctly configured fake holding `seed`.
    pub(crate) fn fake_kms(seed: [u8; 32]) -> FakeKms {
        FakeKms {
            key: SigningKey::from_bytes(&seed),
            key_spec: "ECC_NIST_EDWARDS25519".into(),
            pre_hash: false,
        }
    }

    impl FakeKms {
        pub(crate) fn correct() -> Self {
            fake_kms([4u8; 32])
        }
    }

    #[async_trait]
    impl AwsKmsApi for FakeKms {
        async fn get_public_key(&self, key_id: &str) -> Result<GetPublicKeyOutput, String> {
            if key_id.contains("missing") || key_id.contains("not-a-real-key") {
                return Err("NotFoundException: Key 'arn:...' does not exist".into());
            }
            let mut der = vec![
                0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
            ];
            der.extend_from_slice(&self.key.verifying_key().to_bytes());
            Ok(GetPublicKeyOutput {
                public_key_der: der,
                key_spec: self.key_spec.clone(),
            })
        }

        async fn sign(&self, key_id: &str, message: &[u8]) -> Result<Vec<u8>, String> {
            if key_id.contains("missing") || key_id.contains("not-a-real-key") {
                return Err("NotFoundException: Key 'arn:...' does not exist".into());
            }
            if self.pre_hash {
                use sha2::{Digest, Sha512};
                let digest = Sha512::digest(message);
                return Ok(self.key.sign(&digest).to_bytes().to_vec());
            }
            Ok(self.key.sign(message).to_bytes().to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::FakeKms;
    use super::*;
    use crate::backend::conformance;

    fn key() -> KeyRef {
        KeyRef::new(BACKEND_ID, "arn:aws:kms:eu-west-1:1:key/abc")
    }

    #[tokio::test]
    async fn a_correctly_configured_aws_backend_conforms() {
        let backend = AwsKmsBackend::new(FakeKms::correct());
        conformance::assert_backend_conforms(&backend, &key())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn it_agrees_byte_for_byte_with_a_local_keystore_holding_the_same_key() {
        // The Phase 2 requirement: the same output bytes through each backend.
        let reference = crate::backend::local::LocalKeystore::single("k", [4u8; 32]);
        let subject = AwsKmsBackend::new(FakeKms::correct());
        conformance::assert_backends_agree(
            &reference,
            &KeyRef::new("local", "k"),
            &subject,
            &key(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_pre_hashed_algorithm_is_caught_by_conformance() {
        // ED25519_PH_SHA_512 instead of ED25519_SHA_512: a perfect signature
        // over the wrong message.
        let backend = AwsKmsBackend::new(FakeKms {
            pre_hash: true,
            ..FakeKms::correct()
        });
        assert!(conformance::assert_backend_conforms(&backend, &key())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_ecdsa_key_is_rejected_by_its_reported_spec() {
        // The most likely misconfiguration when standing this up.
        let backend = AwsKmsBackend::new(FakeKms {
            key_spec: "ECC_NIST_P256".into(),
            ..FakeKms::correct()
        });
        let error = backend.public_key(&key()).await.unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("ECC_NIST_P256"), "{rendered}");
        assert!(rendered.contains("ECC_NIST_EDWARDS25519"), "{rendered}");
    }

    #[tokio::test]
    async fn a_missing_key_maps_to_unknown_key_not_unavailable() {
        // A wrong ARN is a config error to fix, not something to retry.
        let backend = AwsKmsBackend::new(FakeKms::correct());
        let error = backend
            .public_key(&KeyRef::new(BACKEND_ID, "arn:missing"))
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::UnknownKey(_)), "{error:?}");
    }

    #[test]
    fn access_denied_is_distinguished_from_a_wrong_arn() {
        // Different fixes: one is IAM, the other is config.
        let denied = classify(&key(), "AccessDeniedException: not authorized".into());
        assert!(matches!(denied, BackendError::Backend(_)), "{denied:?}");

        let missing = classify(&key(), "NotFoundException".into());
        assert!(
            matches!(missing, BackendError::UnknownKey(_)),
            "{missing:?}"
        );

        let down = classify(&key(), "dispatch failure: timeout".into());
        assert!(matches!(down, BackendError::Unavailable(_)), "{down:?}");
    }

    #[tokio::test]
    async fn a_wrong_length_signature_is_reported_with_what_to_check() {
        struct ShortSignature;

        #[async_trait]
        impl AwsKmsApi for ShortSignature {
            async fn get_public_key(&self, _: &str) -> Result<GetPublicKeyOutput, String> {
                unreachable!()
            }
            async fn sign(&self, _: &str, _: &[u8]) -> Result<Vec<u8>, String> {
                // An ECDSA signature is DER and variable-length.
                Ok(vec![0u8; 71])
            }
        }

        let backend = AwsKmsBackend::new(ShortSignature);
        let error = backend
            .sign(&key(), &[0u8; 32])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("71-byte"), "{error}");
        assert!(error.contains("ECC_NIST_EDWARDS25519"), "{error}");
    }

    #[tokio::test]
    async fn health_does_not_call_kms() {
        // A paid API call per load-balancer probe is a way to be surprised by
        // a bill and to rate-limit yourself out of KMS.
        struct ExplodingKms;

        #[async_trait]
        impl AwsKmsApi for ExplodingKms {
            async fn get_public_key(&self, _: &str) -> Result<GetPublicKeyOutput, String> {
                panic!("health must not call GetPublicKey");
            }
            async fn sign(&self, _: &str, _: &[u8]) -> Result<Vec<u8>, String> {
                panic!("health must not call Sign");
            }
        }

        AwsKmsBackend::new(ExplodingKms).health().await.unwrap();
    }
}
