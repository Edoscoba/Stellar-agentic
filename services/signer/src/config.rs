//! Configuration, and the checks that run before the service accepts traffic.
//!
//! # Everything is validated at startup
//!
//! A signing service that discovers a misconfiguration on its first payment
//! has already failed. Every ceiling is parsed, every policy referenced by the
//! registry is confirmed to exist, every key is resolved against its backend,
//! and every allowlist is checked for the empty-means-deny footgun — before
//! the listener is bound.
//!
//! # Policy lives in a file, not behind an API
//!
//! There is deliberately no endpoint that can change a policy. A change to a
//! spend ceiling should be a reviewed, diffable, git-tracked commit, and the
//! absence of a write path means a compromised caller cannot loosen its own
//! limits however far it gets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::{Subject, TokenRecord, TokenStore};
use crate::backend::KeyRef;
use crate::policy::{Policy, PolicySet};
use crate::registry::{KeyRegistry, PolicyName, Registration};

/// The whole configuration file.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where to listen and how to behave.
    #[serde(default)]
    pub server: ServerConfig,
    /// Which backends to start.
    #[serde(default)]
    pub backend: BackendConfig,
    /// Where audit records go.
    #[serde(default)]
    pub audit: AuditConfig,
    /// How the current-ledger estimate behaves.
    #[serde(default)]
    pub ledger: LedgerConfig,
    /// Credentials, by token id.
    #[serde(default)]
    pub tokens: Vec<TokenRecord>,
    /// Identity → key and policy.
    #[serde(default)]
    pub registry: HashMap<String, RegistrationConfig>,
    /// Named policies.
    #[serde(default)]
    pub policy: HashMap<String, Policy>,
}

/// Listener settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to bind.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Largest request body accepted, in bytes.
    ///
    /// An envelope is a few kilobytes; the cap exists so an unauthenticated
    /// caller cannot make the service allocate.
    #[serde(default = "default_body_limit")]
    pub max_body_bytes: usize,
    /// Per-request timeout, in seconds.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
}

fn default_bind() -> String {
    "127.0.0.1:8443".into()
}

fn default_body_limit() -> usize {
    64 * 1024
}

fn default_request_timeout() -> u64 {
    30
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            max_body_bytes: default_body_limit(),
            request_timeout_seconds: default_request_timeout(),
        }
    }
}

/// Which signing backends to start.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    /// The development keystore.
    #[serde(default)]
    pub local: Option<LocalBackendConfig>,
    /// AWS KMS.
    #[serde(default)]
    pub aws_kms: Option<AwsBackendConfig>,
}

/// The file-backed development keystore.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalBackendConfig {
    /// Path to the keystore file.
    pub path: PathBuf,
    /// Must be `true`. See [`crate::backend::local`].
    #[serde(default)]
    pub acknowledge_insecure: bool,
}

/// AWS KMS.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AwsBackendConfig {
    /// Region to talk to. Falls back to the ambient AWS configuration.
    #[serde(default)]
    pub region: Option<String>,
}

/// Where audit records go.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    /// `stdout` or `file`.
    #[serde(default)]
    pub sink: AuditSink,
    /// Path, when the sink is `file`.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            sink: AuditSink::Stdout,
            path: None,
        }
    }
}

/// Which audit sink to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSink {
    /// One JSON object per line on stdout — the right default in a container.
    #[default]
    Stdout,
    /// Append to a file, fsyncing each record.
    File,
}

/// How the ledger estimate behaves.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    /// Seed the estimate, e.g. from the last audit record after a restart.
    #[serde(default)]
    pub initial: u32,
    /// Largest single advance the estimate will accept. See [`crate::ledger`].
    #[serde(default = "default_max_advance")]
    pub max_advance: u32,
}

fn default_max_advance() -> u32 {
    1_000
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            initial: 0,
            max_advance: default_max_advance(),
        }
    }
}

/// One identity's registration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationConfig {
    /// Which backend holds the key.
    pub backend: String,
    /// The backend's identifier for it.
    pub key_id: String,
    /// Which policy applies.
    pub policy: String,
}

/// A configuration that failed a startup check.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

impl Config {
    /// Read and parse a configuration file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| ConfigError(format!("could not read {}: {error}", path.display())))?;
        Self::parse(&raw)
    }

    /// Parse a configuration from TOML.
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        toml::from_str(raw).map_err(|error| ConfigError(format!("invalid configuration: {error}")))
    }

    /// Check everything that can be checked before serving traffic.
    ///
    /// Returns warnings — conditions that are legitimate but worth an operator
    /// seeing — and fails on anything that would make the service unsafe or
    /// unusable.
    pub fn validate(&self) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();

        if self.tokens.is_empty() {
            return Err(ConfigError(
                "no tokens are configured, so no caller could ever authenticate".into(),
            ));
        }
        if self.registry.is_empty() {
            return Err(ConfigError(
                "no identities are registered, so no caller could ever sign".into(),
            ));
        }
        if self.policy.is_empty() {
            return Err(ConfigError("no policies are defined".into()));
        }

        // Every policy parses and is internally consistent.
        for (name, policy) in &self.policy {
            policy.validate(name).map_err(ConfigError)?;
        }

        // Every registration names a policy that exists. Falling back to a
        // default here would be the one moment a typo becomes an unbounded
        // signing oracle.
        for (subject, registration) in &self.registry {
            if !self.policy.contains_key(&registration.policy) {
                return Err(ConfigError(format!(
                    "identity `{subject}` names policy `{}`, which is not defined",
                    registration.policy
                )));
            }
            if self
                .backend_ids()
                .iter()
                .all(|id| id != &registration.backend)
            {
                return Err(ConfigError(format!(
                    "identity `{subject}` names backend `{}`, which is not configured",
                    registration.backend
                )));
            }
        }

        // Every token names an identity that exists. A token for an unknown
        // subject authenticates and then fails at the registry — better to say
        // so now.
        for token in &self.tokens {
            if !self.registry.contains_key(token.subject.as_str()) {
                return Err(ConfigError(format!(
                    "token `{}` is for identity `{}`, which is not registered",
                    token.id, token.subject
                )));
            }
            if token.expires_at.is_none() && token.revoked_at.is_none() {
                warnings.push(format!(
                    "token `{}` has no expiry; a credential that never expires is one that can \
                     only be withdrawn by editing this file",
                    token.id
                ));
            }
        }

        // An identity with no token can never call; harmless, but almost
        // always a half-finished change.
        for subject in self.registry.keys() {
            if !self.tokens.iter().any(|t| t.subject.as_str() == subject) {
                warnings.push(format!(
                    "identity `{subject}` has no token, so nothing can authenticate as it"
                ));
            }
        }

        if self.backend_ids().is_empty() {
            return Err(ConfigError("no signing backend is configured".into()));
        }

        if let Some(local) = &self.backend.local {
            if !local.acknowledge_insecure {
                return Err(ConfigError(
                    "[backend.local] holds key material on disk and is for development only. Set \
                     `acknowledge_insecure = true` to use it, or configure aws_kms instead."
                        .into(),
                ));
            }
            warnings.push(
                "the LOCAL keystore is enabled: key material is on disk in plaintext. This is \
                 for development and testnet only."
                    .into(),
            );
        }

        if self.audit.sink == AuditSink::File && self.audit.path.is_none() {
            return Err(ConfigError(
                "[audit] sink = \"file\" requires a `path`".into(),
            ));
        }

        Ok(warnings)
    }

    /// The backends this configuration starts.
    pub fn backend_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if self.backend.local.is_some() {
            ids.push(crate::backend::local::BACKEND_ID.to_string());
        }
        if self.backend.aws_kms.is_some() {
            ids.push(crate::backend::aws_kms::BACKEND_ID.to_string());
        }
        ids
    }

    /// Build the credential store.
    pub fn token_store(&self) -> Result<TokenStore, ConfigError> {
        TokenStore::new(self.tokens.clone()).map_err(ConfigError)
    }

    /// Build the key registry.
    pub fn key_registry(&self) -> KeyRegistry {
        let entries = self
            .registry
            .iter()
            .map(|(subject, registration)| {
                (
                    Subject::new(subject),
                    Registration {
                        key: KeyRef::new(&registration.backend, &registration.key_id),
                        policy: PolicyName::new(&registration.policy),
                    },
                )
            })
            .collect();
        KeyRegistry::new(entries)
    }

    /// Build the policy set.
    pub fn policy_set(&self) -> Result<PolicySet, ConfigError> {
        let policies = self
            .policy
            .iter()
            .map(|(name, policy)| (PolicyName::new(name), policy.clone()))
            .collect();
        PolicySet::new(policies).map_err(ConfigError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[backend.local]
path = "keys.toml"
acknowledge_insecure = true

[[tokens]]
id = "t1"
subject = "agent-1"
token_sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
expires_at = 4102444800

[registry.agent-1]
backend = "local"
key_id = "k1"
policy = "default"

[policy.default]
networks = ["Test SDF Network ; September 2015"]
contracts = ["CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE"]
functions = ["pay"]
recipients = ["GA7QYNF7SOWQ3GLR2BGMZEHXAVIRZA4KVWLTJJFC7MGXUA74P7UJVSGZ"]
max_amount_stroops = "10000000"
"#;

    fn config() -> Config {
        Config::parse(MINIMAL).expect("the fixture parses")
    }

    #[test]
    fn a_minimal_configuration_validates() {
        let warnings = config().validate().unwrap();
        // The local keystore always warns.
        assert!(
            warnings.iter().any(|w| w.contains("LOCAL keystore")),
            "{warnings:?}"
        );
    }

    #[test]
    fn defaults_are_conservative() {
        let config = config();
        assert_eq!(
            config.server.bind, "127.0.0.1:8443",
            "must not bind 0.0.0.0 by default"
        );
        assert_eq!(config.audit.sink, AuditSink::Stdout);
        assert_eq!(config.ledger.max_advance, 1_000);
        assert!(config.server.max_body_bytes <= 64 * 1024);
    }

    #[test]
    fn an_unknown_field_is_rejected_rather_than_ignored() {
        // A misspelled `max_amount_stroopz` that was silently ignored would
        // leave a key with no cap and an operator who believes it has one.
        let raw = format!("{MINIMAL}\nmax_amount_stroopz = \"1\"\n");
        assert!(Config::parse(&raw).is_err());
    }

    #[test]
    fn a_registration_naming_an_undefined_policy_fails_closed() {
        let raw = MINIMAL.replace("policy = \"default\"", "policy = \"typo\"");
        let error = Config::parse(&raw).unwrap().validate().unwrap_err();
        assert!(error.0.contains("typo"), "{error}");
        assert!(error.0.contains("not defined"), "{error}");
    }

    #[test]
    fn a_registration_naming_an_unconfigured_backend_fails() {
        let raw = MINIMAL.replace("backend = \"local\"", "backend = \"gcp-kms\"");
        let error = Config::parse(&raw).unwrap().validate().unwrap_err();
        assert!(error.0.contains("gcp-kms"), "{error}");
    }

    #[test]
    fn a_token_for_an_unregistered_identity_fails_at_startup() {
        let raw = MINIMAL.replace("subject = \"agent-1\"", "subject = \"ghost\"");
        let error = Config::parse(&raw).unwrap().validate().unwrap_err();
        assert!(error.0.contains("ghost"), "{error}");
    }

    #[test]
    fn the_local_keystore_refuses_to_start_without_acknowledgement() {
        let raw = MINIMAL.replace(
            "acknowledge_insecure = true",
            "acknowledge_insecure = false",
        );
        let error = Config::parse(&raw).unwrap().validate().unwrap_err();
        assert!(error.0.contains("acknowledge_insecure"), "{error}");
    }

    #[test]
    fn a_policy_with_no_networks_fails_at_startup() {
        let raw = MINIMAL.replace(
            "networks = [\"Test SDF Network ; September 2015\"]",
            "networks = []",
        );
        let error = Config::parse(&raw).unwrap().validate().unwrap_err();
        assert!(error.0.contains("networks"), "{error}");
    }

    #[test]
    fn a_configuration_with_no_tokens_is_refused() {
        let mut config = config();
        config.tokens.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn a_token_with_no_expiry_warns_but_does_not_fail() {
        // Legitimate for a bootstrap credential, and worth saying out loud.
        let raw = MINIMAL.replace("expires_at = 4102444800", "");
        let warnings = Config::parse(&raw).unwrap().validate().unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("no expiry")),
            "{warnings:?}"
        );
    }

    #[test]
    fn an_identity_with_no_token_warns() {
        let raw = format!(
            "{MINIMAL}\n[registry.agent-2]\nbackend = \"local\"\nkey_id = \"k2\"\npolicy = \"default\"\n"
        );
        let warnings = Config::parse(&raw).unwrap().validate().unwrap();
        assert!(
            warnings.iter().any(|w| w.contains("agent-2")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_file_audit_sink_without_a_path_is_refused() {
        let raw = format!("{MINIMAL}\n[audit]\nsink = \"file\"\n");
        let error = Config::parse(&raw).unwrap().validate().unwrap_err();
        assert!(error.0.contains("path"), "{error}");
    }

    #[test]
    fn the_derived_stores_match_the_file() {
        let config = config();
        assert_eq!(config.token_store().unwrap().len(), 1);
        assert_eq!(config.key_registry().len(), 1);
        assert_eq!(config.policy_set().unwrap().names().len(), 1);
        assert_eq!(
            config
                .key_registry()
                .resolve(&Subject::new("agent-1"))
                .unwrap()
                .key,
            KeyRef::new("local", "k1")
        );
    }
}
