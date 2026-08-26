//! Contract address resolution.
//!
//! Rust port of `packages/core/src/contracts.ts` and
//! `python/src/stellaragent/contracts.py`, with the same precedence rules and
//! the same environment variable names — so a single deployment configures a
//! TypeScript agent, a Python agent and a Rust agent identically, from one
//! `.env` block.

use std::env;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, StellarAgentError};
use crate::types::Network;

/// The five contracts the SDK calls directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractAddresses {
    /// `AgentWalletFactory` — agent registration.
    pub agent_wallet_factory: String,
    /// `PaymentChannel` — funding, spend limits, payments.
    pub payment_channel: String,
    /// `Escrow` — agent-to-agent job escrow.
    pub escrow: String,
    /// `RateLimiter` — per-agent spend and transaction caps.
    pub rate_limiter: String,
    /// `CircuitBreaker` — emergency stop.
    pub circuit_breaker: String,
}

/// Which contract a lookup refers to.
///
/// An enum rather than a string key, so [`ContractAddresses::get`] cannot be
/// asked for a contract that does not exist and the environment-variable
/// names cannot drift from the field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContractKey {
    /// `AgentWalletFactory`.
    AgentWalletFactory,
    /// `PaymentChannel`.
    PaymentChannel,
    /// `Escrow`.
    Escrow,
    /// `RateLimiter`.
    RateLimiter,
    /// `CircuitBreaker`.
    CircuitBreaker,
}

impl ContractKey {
    /// Every contract, in deployment order — matching the TypeScript
    /// `CONTRACT_KEYS` and the Python tuple of the same name.
    pub const ALL: [ContractKey; 5] = [
        ContractKey::AgentWalletFactory,
        ContractKey::PaymentChannel,
        ContractKey::Escrow,
        ContractKey::RateLimiter,
        ContractKey::CircuitBreaker,
    ];

    /// The snake_case key, as used in JSON deployment files.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentWalletFactory => "agent_wallet_factory",
            Self::PaymentChannel => "payment_channel",
            Self::Escrow => "escrow",
            Self::RateLimiter => "rate_limiter",
            Self::CircuitBreaker => "circuit_breaker",
        }
    }

    /// Environment variables consulted for this contract, most specific first.
    ///
    /// ```
    /// use stellaragent::contracts::ContractKey;
    /// use stellaragent::types::Network;
    ///
    /// assert_eq!(
    ///     ContractKey::PaymentChannel.env_var_names(Network::Testnet),
    ///     (
    ///         "STELLARAGENT_TESTNET_PAYMENT_CHANNEL".to_string(),
    ///         "STELLARAGENT_PAYMENT_CHANNEL".to_string(),
    ///     )
    /// );
    /// ```
    ///
    /// The network-scoped form lets one process talk to more than one network.
    /// Identical to the names the other two SDKs read.
    pub fn env_var_names(self, network: Network) -> (String, String) {
        let suffix = self.as_str().to_ascii_uppercase();
        (
            format!(
                "STELLARAGENT_{}_{suffix}",
                network.as_str().to_ascii_uppercase()
            ),
            format!("STELLARAGENT_{suffix}"),
        )
    }
}

impl std::fmt::Display for ContractKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ContractAddresses {
    /// The address configured for one contract.
    pub fn get(&self, key: ContractKey) -> &str {
        match key {
            ContractKey::AgentWalletFactory => &self.agent_wallet_factory,
            ContractKey::PaymentChannel => &self.payment_channel,
            ContractKey::Escrow => &self.escrow,
            ContractKey::RateLimiter => &self.rate_limiter,
            ContractKey::CircuitBreaker => &self.circuit_breaker,
        }
    }

    fn set(&mut self, key: ContractKey, value: String) {
        match key {
            ContractKey::AgentWalletFactory => self.agent_wallet_factory = value,
            ContractKey::PaymentChannel => self.payment_channel = value,
            ContractKey::Escrow => self.escrow = value,
            ContractKey::RateLimiter => self.rate_limiter = value,
            ContractKey::CircuitBreaker => self.circuit_breaker = value,
        }
    }

    /// The per-network placeholders meaning "nothing deployed here yet".
    ///
    /// Byte-for-byte the values the TypeScript SDK once shipped as
    /// `DEFAULT_CONTRACTS`. They are not valid contract IDs — 60-61 characters
    /// where a Stellar contract ID is exactly 56, with no valid checksum — and
    /// [`assert_deployed`] rejects every one of them. They exist so a
    /// misconfigured agent fails with "not deployed" rather than with a
    /// missing-field panic.
    pub fn unconfigured(network: Network) -> Self {
        match network {
            Network::Testnet => Self {
                agent_wallet_factory:
                    "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
                payment_channel: "CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
                    .into(),
                escrow: "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".into(),
                rate_limiter: "CDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD"
                    .into(),
                circuit_breaker: "CEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"
                    .into(),
            },
            Network::Mainnet | Network::Local => Self {
                agent_wallet_factory: String::new(),
                payment_channel: String::new(),
                escrow: String::new(),
                rate_limiter: String::new(),
                circuit_breaker: String::new(),
            },
        }
    }
}

/// Whether a string is a real, deployable Stellar contract ID.
///
/// Uses strkey checksum validation rather than a pattern match against the
/// known placeholders, so it also catches truncated addresses, addresses
/// pasted from the wrong network, and single-character typos — all of which
/// would otherwise surface as an opaque RPC error from the middle of a
/// payment.
///
/// ```
/// use stellaragent::contracts::is_deployed_address;
///
/// assert!(!is_deployed_address(""));
/// assert!(!is_deployed_address(
///     "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
/// ));
/// ```
pub fn is_deployed_address(address: &str) -> bool {
    !address.is_empty() && stellar_strkey::Contract::from_string(address).is_ok()
}

/// Resolve contract addresses for a network.
///
/// Precedence, highest first: `overrides`, the network-scoped environment
/// variable, the unscoped environment variable, then the unconfigured
/// sentinel. Never fails — use [`assert_deployed`] to reject the result.
///
/// `overrides` is a slice of `(key, address)` pairs rather than a map so the
/// common case (one or two overrides) needs no allocation from the caller.
pub fn resolve_contracts(
    network: Network,
    overrides: &[(ContractKey, String)],
) -> ContractAddresses {
    let mut resolved = ContractAddresses::unconfigured(network);

    for key in ContractKey::ALL {
        if let Some((_, address)) = overrides.iter().find(|(k, _)| *k == key) {
            if !address.is_empty() {
                resolved.set(key, address.clone());
                continue;
            }
        }
        let (scoped, unscoped) = key.env_var_names(network);
        if let Some(value) = env::var(&scoped).ok().filter(|v| !v.is_empty()) {
            resolved.set(key, value);
        } else if let Some(value) = env::var(&unscoped).ok().filter(|v| !v.is_empty()) {
            resolved.set(key, value);
        }
    }

    resolved
}

/// Fail unless every contract address is a real deployed contract ID.
///
/// Called from [`crate::StellarAgentBuilder::build`] so the failure names the actual
/// problem — and how to fix it — instead of surfacing later as a confusing RPC
/// error from the middle of a payment.
pub fn assert_deployed(
    network: Network,
    contracts: &ContractAddresses,
) -> crate::error::Result<()> {
    let missing: Vec<ContractKey> = ContractKey::ALL
        .into_iter()
        .filter(|key| !is_deployed_address(contracts.get(*key)))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let names: Vec<&str> = missing.iter().map(|k| k.as_str()).collect();
    let env_lines: Vec<String> = missing
        .iter()
        .map(|key| format!("       {}=C...", key.env_var_names(network).0))
        .collect();

    let message = format!(
        "Contracts not deployed for network \"{network}\" — see docs/deployment.md\n\
         \n\
         Unconfigured or invalid: {}\n\
         \n\
         Fix this by either:\n\
         \x20 1. Deploying them:  pnpm deploy:contracts --network {network}\n\
         \x20    (writes deployments/{network}.json and prints an .env block)\n\
         \x20 2. Passing known addresses explicitly:\n\
         \x20      StellarAgent::builder().contract(ContractKey::PaymentChannel, \"C...\")\n\
         \x20 3. Setting environment variables:\n\
         {}",
        names.join(", "),
        env_lines.join("\n"),
    );

    Err(StellarAgentError::new(ErrorCode::InvalidArgument, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real, checksum-valid contract ID, so the deployed/undeployed
    // distinction is exercised against something strkey actually accepts.
    const VALID: &str = "CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE";

    #[test]
    fn placeholders_are_not_valid_contract_ids() {
        let unconfigured = ContractAddresses::unconfigured(Network::Testnet);
        for key in ContractKey::ALL {
            assert!(
                !is_deployed_address(unconfigured.get(key)),
                "{key} placeholder should not validate"
            );
        }
    }

    #[test]
    fn strkey_validation_catches_a_single_character_typo() {
        assert!(is_deployed_address(VALID));
        let mut typo = VALID.to_string();
        typo.replace_range(10..11, "X");
        assert!(!is_deployed_address(&typo), "checksum should reject {typo}");
    }

    #[test]
    fn env_var_names_match_the_other_sdks() {
        assert_eq!(
            ContractKey::Escrow.env_var_names(Network::Local),
            (
                "STELLARAGENT_LOCAL_ESCROW".to_string(),
                "STELLARAGENT_ESCROW".to_string()
            )
        );
    }

    #[test]
    fn overrides_win_over_everything_else() {
        let resolved = resolve_contracts(
            Network::Testnet,
            &[(ContractKey::PaymentChannel, VALID.to_string())],
        );
        assert_eq!(resolved.payment_channel, VALID);
        // Untouched keys keep the sentinel.
        assert_eq!(
            resolved.escrow,
            ContractAddresses::unconfigured(Network::Testnet).escrow
        );
    }

    #[test]
    fn an_empty_override_falls_through_rather_than_blanking_the_address() {
        let resolved = resolve_contracts(
            Network::Testnet,
            &[(ContractKey::PaymentChannel, String::new())],
        );
        assert_eq!(
            resolved.payment_channel,
            ContractAddresses::unconfigured(Network::Testnet).payment_channel
        );
    }

    #[test]
    fn assert_deployed_names_every_missing_contract_and_its_env_var() {
        let error = assert_deployed(
            Network::Testnet,
            &ContractAddresses::unconfigured(Network::Testnet),
        )
        .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        let message = error.to_string();
        for key in ContractKey::ALL {
            assert!(message.contains(key.as_str()), "{message}");
            assert!(
                message.contains(&key.env_var_names(Network::Testnet).0),
                "{message}"
            );
        }
    }

    #[test]
    fn a_fully_deployed_set_passes() {
        let deployed = ContractAddresses {
            agent_wallet_factory: VALID.into(),
            payment_channel: VALID.into(),
            escrow: VALID.into(),
            rate_limiter: VALID.into(),
            circuit_breaker: VALID.into(),
        };
        assert!(assert_deployed(Network::Testnet, &deployed).is_ok());
    }
}
