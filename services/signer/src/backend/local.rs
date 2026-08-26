//! A file-backed keystore. **Development and testnet only.**
//!
//! # Why this exists at all
//!
//! The whole point of this service is that a key does not sit in a process's
//! memory. A local keystore is exactly the thing the epic exists to replace, so
//! shipping one deserves a justification:
//!
//! - the conformance suite ([`super::conformance`]) has to run somewhere, on
//!   every CI job, with no cloud credentials;
//! - a contributor should be able to run the service against a local network
//!   without an AWS account;
//! - "the same output bytes through each backend" is only checkable if there is
//!   a reference backend to check against.
//!
//! # The guard
//!
//! [`LocalKeystore::load`] refuses unless `acknowledge_insecure` is set, and
//! logs a warning on every startup. A service that quietly accepts a
//! file-backed key in production is precisely the failure mode this epic
//! exists to prevent, so it is made noisy and deliberate rather than merely
//! discouraged in a README.
//!
//! # What is and is not protected
//!
//! The keystore file holds raw seeds. It is not encrypted at rest here —
//! doing so would need a passphrase, which would need to come from somewhere,
//! which for a dev tool means an environment variable sitting next to the
//! file. That is ceremony, not security. The honest position is: this file is
//! as sensitive as the key, treat it accordingly, and do not use it for
//! anything that holds value. The [`super::aws_kms`] and [`super::gcp_kms`]
//! backends are the answer for that.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use ed25519_dalek::{Signer, SigningKey};
use serde::Deserialize;

use super::{BackendError, KeyRef, SigningBackend};

/// The backend id local keys are addressed under.
pub const BACKEND_ID: &str = "local";

/// The on-disk shape.
#[derive(Debug, Deserialize)]
struct KeystoreFile {
    #[serde(default)]
    keys: HashMap<String, KeystoreEntry>,
}

#[derive(Debug, Deserialize)]
struct KeystoreEntry {
    /// Stellar secret seed (`S…`).
    secret: String,
}

/// An in-memory keystore loaded from a TOML file.
///
/// `Debug` prints only key aliases: a derived implementation would put seeds
/// into any log line that formats the service's state.
pub struct LocalKeystore {
    keys: HashMap<String, SigningKey>,
}

impl std::fmt::Debug for LocalKeystore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut aliases: Vec<&String> = self.keys.keys().collect();
        aliases.sort();
        f.debug_struct("LocalKeystore")
            .field("aliases", &aliases)
            .finish_non_exhaustive()
    }
}

impl LocalKeystore {
    /// Load a keystore from `path`.
    ///
    /// # Errors
    ///
    /// Refuses outright unless `acknowledge_insecure` is `true`, which the
    /// config file must set explicitly.
    pub fn load(path: &Path, acknowledge_insecure: bool) -> Result<Self, BackendError> {
        if !acknowledge_insecure {
            return Err(BackendError::Unavailable(
                "the local keystore holds raw key material on disk and is for development only. \
                 Set `acknowledge_insecure = true` under [backend.local] to use it, or configure \
                 an aws-kms / gcp-kms backend instead."
                    .into(),
            ));
        }

        let raw = std::fs::read_to_string(path).map_err(|error| {
            BackendError::Unavailable(format!("could not read {}: {error}", path.display()))
        })?;
        let parsed: KeystoreFile = toml::from_str(&raw).map_err(|error| {
            BackendError::Backend(format!(
                "{} is not a valid keystore: {error}",
                path.display()
            ))
        })?;

        let mut keys = HashMap::new();
        for (alias, entry) in parsed.keys {
            let seed =
                stellar_strkey::ed25519::PrivateKey::from_string(&entry.secret).map_err(|_| {
                    // Never echo the secret, even when it is malformed — a
                    // near-miss seed in a log is still a seed.
                    BackendError::Backend(format!(
                        "key \"{alias}\" is not a valid Stellar secret seed (S…)"
                    ))
                })?;
            keys.insert(alias, SigningKey::from_bytes(&seed.0));
        }

        tracing::warn!(
            keys = keys.len(),
            path = %path.display(),
            "using the LOCAL keystore: key material is on disk in plaintext. \
             This is for development and testnet only."
        );

        Ok(Self { keys })
    }

    /// Build an in-memory keystore directly, for tests and the conformance suite.
    pub fn from_keys(keys: HashMap<String, SigningKey>) -> Self {
        Self { keys }
    }

    /// A keystore holding one deterministic key, for tests.
    pub fn single(alias: &str, seed: [u8; 32]) -> Self {
        let mut keys = HashMap::new();
        keys.insert(alias.to_string(), SigningKey::from_bytes(&seed));
        Self { keys }
    }

    fn key(&self, key: &KeyRef) -> Result<&SigningKey, BackendError> {
        self.keys
            .get(&key.key_id)
            .ok_or_else(|| BackendError::UnknownKey(key.clone()))
    }

    /// The aliases this keystore holds.
    pub fn aliases(&self) -> Vec<&str> {
        self.keys.keys().map(String::as_str).collect()
    }
}

#[async_trait]
impl SigningBackend for LocalKeystore {
    fn id(&self) -> &str {
        BACKEND_ID
    }

    async fn public_key(&self, key: &KeyRef) -> Result<[u8; 32], BackendError> {
        Ok(self.key(key)?.verifying_key().to_bytes())
    }

    async fn sign(&self, key: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError> {
        // PureEdDSA over the raw 32 bytes — the same thing the cloud backends
        // are configured to do, which is what makes cross-backend byte
        // equality checkable.
        Ok(self.key(key)?.sign(payload).to_bytes())
    }

    async fn health(&self) -> Result<(), BackendError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn keystore() -> LocalKeystore {
        LocalKeystore::single("agent-1", [9u8; 32])
    }

    #[tokio::test]
    async fn signs_and_the_signature_verifies_under_the_reported_key() {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let store = keystore();
        let key = KeyRef::new(BACKEND_ID, "agent-1");
        let payload = [3u8; 32];

        let public = store.public_key(&key).await.unwrap();
        let signature = store.sign(&key, &payload).await.unwrap();

        let verifying = VerifyingKey::from_bytes(&public).unwrap();
        assert!(verifying
            .verify(&payload, &Signature::from_bytes(&signature))
            .is_ok());
    }

    #[tokio::test]
    async fn an_unknown_alias_is_an_error_not_a_default_key() {
        let store = keystore();
        let missing = KeyRef::new(BACKEND_ID, "nope");
        assert!(matches!(
            store.public_key(&missing).await,
            Err(BackendError::UnknownKey(_))
        ));
        assert!(matches!(
            store.sign(&missing, &[0u8; 32]).await,
            Err(BackendError::UnknownKey(_))
        ));
    }

    #[test]
    fn loading_without_acknowledgement_is_refused_with_an_actionable_message() {
        let error = LocalKeystore::load(Path::new("/nonexistent"), false).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("acknowledge_insecure"), "{message}");
        assert!(message.contains("development only"), "{message}");
    }

    #[test]
    fn a_valid_keystore_file_loads() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[keys.agent-1]
secret = "SAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYPSBF5K"
"#
        )
        .unwrap();

        let store = LocalKeystore::load(file.path(), true).unwrap();
        assert_eq!(store.aliases(), ["agent-1"]);
    }

    #[test]
    fn a_malformed_seed_is_reported_without_echoing_it() {
        // A near-miss seed in a log is still a seed.
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[keys.agent-1]
secret = "SNOTAVALIDSEEDATALL"
"#
        )
        .unwrap();

        let error = LocalKeystore::load(file.path(), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("agent-1"), "{error}");
        assert!(!error.contains("SNOTAVALIDSEED"), "{error}");
    }

    #[test]
    fn debug_output_never_contains_key_material() {
        let rendered = format!("{:?}", keystore());
        assert!(rendered.contains("agent-1"));
        // The seed is [9u8; 32]; nothing resembling it should appear.
        assert!(
            !rendered.contains('9') || !rendered.contains("SigningKey"),
            "{rendered}"
        );
        assert!(!rendered.to_lowercase().contains("secret"), "{rendered}");
    }
}
