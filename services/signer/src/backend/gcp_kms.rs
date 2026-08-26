//! GCP Cloud KMS, via `EC_SIGN_ED25519` keys.
//!
//! Cloud KMS documents `EC_SIGN_ED25519` as *"EdDSA on the Curve25519 in
//! PureEdDSA mode, which takes raw data as input"* — which is exactly what
//! Stellar needs, and means this adapter, the AWS one, and the local keystore
//! all produce identical bytes for identical input.
//!
//! # Two differences from AWS worth knowing
//!
//! 1. **The public key comes back PEM-armoured**, not raw DER, so
//!    [`spki::ed25519_from_pem`] does the unwrapping.
//! 2. **`AsymmetricSign` takes `data`, not `digest`**, for Ed25519 keys.
//!    Passing the digest field for a PureEdDSA key is rejected by the API
//!    rather than silently signing the wrong thing — GCP is less dangerous
//!    than AWS here, where the pre-hashed variant is a valid configuration
//!    that fails silently.
//!
//! # CRC32C
//!
//! Cloud KMS returns `verified_data_crc32c` and a `signature_crc32c` so a
//! caller can detect corruption in transit. This adapter surfaces the fields
//! it is given through [`AsymmetricSignOutput`] and verifies the server
//! confirmed our request checksum; see [`GcpKmsBackend::sign`].

use async_trait::async_trait;

use super::spki;
use super::{BackendError, KeyRef, SigningBackend};

/// The backend id GCP keys are addressed under.
pub const BACKEND_ID: &str = "gcp-kms";

/// The only algorithm this adapter will use.
pub const SIGNING_ALGORITHM: &str = "EC_SIGN_ED25519";

/// The narrow slice of the Cloud KMS API this adapter needs.
#[async_trait]
pub trait GcpKmsApi: Send + Sync {
    /// `GetPublicKey`, returning a PEM-armoured `SubjectPublicKeyInfo`.
    async fn get_public_key(&self, name: &str) -> Result<GetPublicKeyOutput, String>;

    /// `AsymmetricSign` over raw data.
    async fn asymmetric_sign(
        &self,
        name: &str,
        data: &[u8],
    ) -> Result<AsymmetricSignOutput, String>;
}

/// What `GetPublicKey` gives back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetPublicKeyOutput {
    /// PEM-armoured `SubjectPublicKeyInfo`.
    pub pem: String,
    /// The algorithm Cloud KMS reports, e.g. `EC_SIGN_ED25519`.
    pub algorithm: String,
}

/// What `AsymmetricSign` gives back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsymmetricSignOutput {
    /// The raw signature bytes.
    pub signature: Vec<u8>,
    /// Whether Cloud KMS confirmed the CRC32C of the data we sent.
    ///
    /// `false` means the request was corrupted in transit and the signature is
    /// over something other than what we meant — a case worth failing on
    /// rather than shrugging at.
    pub verified_data_crc32c: bool,
}

/// A Cloud KMS-backed signer.
pub struct GcpKmsBackend<C: GcpKmsApi> {
    client: C,
}

impl<C: GcpKmsApi> std::fmt::Debug for GcpKmsBackend<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcpKmsBackend").finish_non_exhaustive()
    }
}

impl<C: GcpKmsApi> GcpKmsBackend<C> {
    /// Wrap a Cloud KMS client.
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C: GcpKmsApi> SigningBackend for GcpKmsBackend<C> {
    fn id(&self) -> &str {
        BACKEND_ID
    }

    async fn public_key(&self, key: &KeyRef) -> Result<[u8; 32], BackendError> {
        let output = self
            .client
            .get_public_key(&key.key_id)
            .await
            .map_err(|error| classify(key, error))?;

        if output.algorithm != SIGNING_ALGORITHM {
            return Err(BackendError::WrongKeyType {
                key: key.clone(),
                detail: format!(
                    "Cloud KMS reports algorithm {}, but Stellar needs {SIGNING_ALGORITHM}",
                    output.algorithm
                ),
            });
        }

        spki::ed25519_from_pem(&output.pem).map_err(|error| BackendError::WrongKeyType {
            key: key.clone(),
            detail: error.to_string(),
        })
    }

    async fn sign(&self, key: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError> {
        let output = self
            .client
            .asymmetric_sign(&key.key_id, payload)
            .await
            .map_err(|error| classify(key, error))?;

        // If Cloud KMS could not confirm our data's checksum, it may have
        // signed corrupted bytes. A signature over the wrong message is worse
        // than no signature, so this fails rather than warns.
        if !output.verified_data_crc32c {
            return Err(BackendError::Backend(format!(
                "Cloud KMS did not verify the CRC32C of the data sent for {}; the request may \
                 have been corrupted in transit and the signature may be over the wrong bytes",
                key.key_id
            )));
        }

        output.signature.as_slice().try_into().map_err(|_| {
            BackendError::Backend(format!(
                "Cloud KMS returned a {}-byte signature; an Ed25519 signature is 64 bytes. \
                 Check that {} uses the {SIGNING_ALGORITHM} algorithm.",
                output.signature.len(),
                key.key_id
            ))
        })
    }

    async fn health(&self) -> Result<(), BackendError> {
        // Same reasoning as the AWS adapter: `/readyz` is unauthenticated and
        // frequent, and turning it into a billed API call is a bad trade.
        Ok(())
    }
}

fn classify(key: &KeyRef, error: String) -> BackendError {
    let lower = error.to_ascii_lowercase();
    if lower.contains("not_found") || lower.contains("notfound") || lower.contains("not found") {
        BackendError::UnknownKey(key.clone())
    } else if lower.contains("permission_denied") || lower.contains("permission denied") {
        BackendError::Backend(format!(
            "Cloud KMS denied access to {}: {error}",
            key.key_id
        ))
    } else {
        BackendError::Unavailable(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::conformance;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    struct FakeKms {
        key: SigningKey,
        algorithm: String,
        verified_crc: bool,
    }

    impl FakeKms {
        fn correct() -> Self {
            Self {
                key: SigningKey::from_bytes(&[4u8; 32]),
                algorithm: SIGNING_ALGORITHM.into(),
                verified_crc: true,
            }
        }

        fn pem(&self) -> String {
            let mut der = vec![
                0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
            ];
            der.extend_from_slice(&self.key.verifying_key().to_bytes());
            let body = base64::engine::general_purpose::STANDARD.encode(der);
            format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n")
        }
    }

    #[async_trait]
    impl GcpKmsApi for FakeKms {
        async fn get_public_key(&self, name: &str) -> Result<GetPublicKeyOutput, String> {
            if name.contains("missing") || name.contains("not-a-real-key") {
                return Err("NOT_FOUND: CryptoKeyVersion not found".into());
            }
            Ok(GetPublicKeyOutput {
                pem: self.pem(),
                algorithm: self.algorithm.clone(),
            })
        }

        async fn asymmetric_sign(
            &self,
            name: &str,
            data: &[u8],
        ) -> Result<AsymmetricSignOutput, String> {
            if name.contains("missing") || name.contains("not-a-real-key") {
                return Err("NOT_FOUND: CryptoKeyVersion not found".into());
            }
            Ok(AsymmetricSignOutput {
                signature: self.key.sign(data).to_bytes().to_vec(),
                verified_data_crc32c: self.verified_crc,
            })
        }
    }

    fn key() -> KeyRef {
        KeyRef::new(
            BACKEND_ID,
            "projects/p/locations/l/keyRings/r/cryptoKeys/k/cryptoKeyVersions/1",
        )
    }

    #[tokio::test]
    async fn a_correctly_configured_gcp_backend_conforms() {
        let backend = GcpKmsBackend::new(FakeKms::correct());
        conformance::assert_backend_conforms(&backend, &key())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn all_three_backends_produce_identical_bytes() {
        // Phase 2's actual requirement, checked across the whole set.
        let local = crate::backend::local::LocalKeystore::single("k", [4u8; 32]);
        let gcp = GcpKmsBackend::new(FakeKms::correct());
        let aws = crate::backend::aws_kms::AwsKmsBackend::new(
            crate::backend::aws_kms::tests_support::fake_kms([4u8; 32]),
        );

        let local_key = KeyRef::new("local", "k");
        conformance::assert_backends_agree(&local, &local_key, &gcp, &key())
            .await
            .unwrap();
        conformance::assert_backends_agree(
            &local,
            &local_key,
            &aws,
            &KeyRef::new(crate::backend::aws_kms::BACKEND_ID, "arn:aws:kms:::key/k"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_p256_key_is_rejected_by_its_reported_algorithm() {
        let backend = GcpKmsBackend::new(FakeKms {
            algorithm: "EC_SIGN_P256_SHA256".into(),
            ..FakeKms::correct()
        });
        let error = backend.public_key(&key()).await.unwrap_err().to_string();
        assert!(error.contains("EC_SIGN_P256_SHA256"), "{error}");
        assert!(error.contains(SIGNING_ALGORITHM), "{error}");
    }

    #[tokio::test]
    async fn an_unverified_checksum_fails_rather_than_warns() {
        // A signature over corrupted bytes is worse than no signature.
        let backend = GcpKmsBackend::new(FakeKms {
            verified_crc: false,
            ..FakeKms::correct()
        });
        let error = backend
            .sign(&key(), &[0u8; 32])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("CRC32C"), "{error}");
    }

    #[tokio::test]
    async fn a_missing_key_version_maps_to_unknown_key() {
        let backend = GcpKmsBackend::new(FakeKms::correct());
        let error = backend
            .public_key(&KeyRef::new(BACKEND_ID, "projects/p/missing"))
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::UnknownKey(_)), "{error:?}");
    }

    #[test]
    fn permission_denied_is_distinguished_from_a_missing_key() {
        let denied = classify(&key(), "PERMISSION_DENIED: caller lacks kms.signer".into());
        assert!(matches!(denied, BackendError::Backend(_)), "{denied:?}");
        let missing = classify(&key(), "NOT_FOUND".into());
        assert!(
            matches!(missing, BackendError::UnknownKey(_)),
            "{missing:?}"
        );
    }
}
