//! Type definitions mirroring `packages/core/src/types/index.ts` and
//! `python/src/stellaragent/types.py`.
//!
//! Field names are `snake_case` per Rust convention; every type that crosses a
//! wire boundary carries `#[serde(rename_all = "camelCase")]` so the JSON
//! shape stays identical to the TypeScript SDK's. An agent written against one
//! SDK and an agent written against another must be able to exchange these
//! structures verbatim.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Which Stellar network an agent talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// Public network. Real money.
    Mainnet,
    /// The SDF test network, fundable from friendbot.
    Testnet,
    /// A standalone network on localhost — `stellar container` or quickstart.
    Local,
}

impl Network {
    /// The lowercase name, matching the TypeScript union's string values.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Testnet => "testnet",
            Self::Local => "local",
        }
    }

    /// Every network, in the order the SDK documents them.
    pub const ALL: [Network; 3] = [Network::Mainnet, Network::Testnet, Network::Local];

    /// RPC, Horizon and passphrase for this network.
    pub fn config(self) -> NetworkConfig {
        match self {
            Self::Mainnet => NetworkConfig {
                rpc_url: "https://soroban-rpc.stellar.org".into(),
                network_passphrase: "Public Global Stellar Network ; September 2015".into(),
                horizon_url: "https://horizon.stellar.org".into(),
            },
            Self::Testnet => NetworkConfig {
                rpc_url: "https://soroban-rpc.testnet.stellar.gateway.fm".into(),
                network_passphrase: "Test SDF Network ; September 2015".into(),
                horizon_url: "https://horizon-testnet.stellar.org".into(),
            },
            Self::Local => NetworkConfig {
                rpc_url: "http://localhost:8000/soroban/rpc".into(),
                network_passphrase: "Standalone Network ; February 2017".into(),
                horizon_url: "http://localhost:8000".into(),
            },
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Network {
    type Err = UnknownNetwork;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mainnet" => Ok(Self::Mainnet),
            "testnet" => Ok(Self::Testnet),
            "local" => Ok(Self::Local),
            other => Err(UnknownNetwork {
                value: other.to_string(),
            }),
        }
    }
}

/// A network name the SDK does not recognise.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Unknown network \"{value}\". Expected one of: local, mainnet, testnet")]
pub struct UnknownNetwork {
    /// The unrecognised name.
    pub value: String,
}

/// Endpoints and passphrase for one network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkConfig {
    /// Soroban RPC endpoint.
    pub rpc_url: String,
    /// The network passphrase every signature is domain-separated by.
    pub network_passphrase: String,
    /// Horizon endpoint, used for account and ledger queries.
    pub horizon_url: String,
}

/// How often a channel's spend limit resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendPeriod {
    /// Resets every ledger.
    PerLedger,
    /// Resets every 720 ledgers (~1 hour).
    Hourly,
    /// Resets every 17,280 ledgers (~1 day).
    Daily,
}

impl SpendPeriod {
    /// The snake_case name, matching the TypeScript union's string values.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PerLedger => "per_ledger",
            Self::Hourly => "hourly",
            Self::Daily => "daily",
        }
    }

    /// The Rust `contracttype` enum variant name this maps to on-chain.
    ///
    /// Soroban encodes a unit enum variant as a one-element symbol vector, so
    /// the SDK has to know the *contract's* spelling (`PerLedger`), not the
    /// SDK's (`per_ledger`).
    pub const fn contract_variant(self) -> &'static str {
        match self {
            Self::PerLedger => "PerLedger",
            Self::Hourly => "Hourly",
            Self::Daily => "Daily",
        }
    }

    /// Parse the contract's variant spelling back into a [`SpendPeriod`].
    pub fn from_contract_variant(variant: &str) -> Option<Self> {
        match variant {
            "PerLedger" | "per_ledger" => Some(Self::PerLedger),
            "Hourly" | "hourly" => Some(Self::Hourly),
            "Daily" | "daily" => Some(Self::Daily),
            _ => None,
        }
    }
}

impl fmt::Display for SpendPeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maximum spend per period, enforced on-chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendLimit {
    /// The ceiling, as a decimal string in `asset` units.
    pub amount: String,
    /// Asset code or token contract ID.
    pub asset: String,
    /// How often the ceiling resets.
    pub period: SpendPeriod,
}

/// An agent registered in `AgentWalletFactory`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// The factory's agent ID.
    pub id: u64,
    /// The agent's Stellar address.
    pub address: String,
    /// Human-readable label.
    pub name: String,
    /// The owner authorised to administer it.
    pub owner: String,
    /// Whether the agent is live.
    pub active: bool,
    /// Ledger the agent was registered at.
    pub created_at: u32,
    /// Lifetime operation count.
    pub total_ops: u64,
}

/// Parameters for opening a payment channel.
///
/// `limit_per_period` is always denominated in `token`, the channel's single
/// funding/settlement asset — even for cross-asset payments made via
/// [`PayForApiParams::dest_asset`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenChannelParams {
    /// Amount to fund the channel with, as a decimal string.
    pub deposit: String,
    /// Per-period spend ceiling, in `token` units.
    pub limit_per_period: String,
    /// How often the ceiling resets.
    pub period: SpendPeriod,
    /// Asset code or token contract ID. Defaults to `XLM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Parameters for paying for an API call.
///
/// Setting `dest_asset` routes through `PaymentChannel.pay_with_conversion`, so
/// the recipient is settled in a different asset than the channel holds. It
/// requires `min_received` — a slippage floor in `dest_asset` units — to be set
/// as well. The spend limit is still enforced in the channel's own settlement
/// asset either way.
///
/// `Default` is derived so the five optional fields can be elided with
/// `..Default::default()`; `endpoint` and `amount` are still required in
/// practice, and an empty `amount` is rejected by the contract's own
/// positive-amount guard rather than silently paying nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayForApiParams {
    /// The endpoint being paid for, recorded on-chain as a memo.
    pub endpoint: String,
    /// Amount to pay, as a decimal string in the channel's asset.
    pub amount: String,
    /// Asset code, for callers tracking which token a channel holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// Channel to draw from. Defaults to the agent's active channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<u64>,
    /// Who to pay. Defaults to the agent itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Settle the recipient in this asset instead. Requires `min_received`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_asset: Option<String>,
    /// Slippage floor in `dest_asset` units. Requires `dest_asset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_received: Option<String>,
}

/// A payment channel's on-chain state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    /// The channel ID.
    pub id: u64,
    /// The agent authorised to spend from it.
    pub agent: String,
    /// The owner who funded it.
    pub owner: String,
    /// Token contract ID of the settlement asset.
    pub token: String,
    /// Per-period ceiling, in stroops.
    pub limit_per_period: i128,
    /// How often the ceiling resets.
    pub period: SpendPeriod,
    /// Spent so far in the current period, in stroops.
    pub spent_this_period: i128,
    /// Ledger the current period started at.
    pub period_start_ledger: u32,
    /// Lifetime spend, in stroops.
    pub total_spent: i128,
    /// Whether the channel is open.
    pub active: bool,
}

/// Spend accounting for the current period, in human-readable units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendReport {
    /// Spent in the current period.
    pub spent_this_period: String,
    /// Remaining in the current period.
    pub remaining_this_period: String,
    /// Lifetime spend across all periods.
    pub total_lifetime: String,
}

/// Where an escrow job is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Posted, no worker assigned.
    Open,
    /// A worker accepted it.
    InProgress,
    /// A result was submitted, awaiting release.
    PendingRelease,
    /// Payment released to the worker.
    Completed,
    /// Escrow returned to the requester.
    Refunded,
    /// Under arbitration.
    Disputed,
}

impl JobStatus {
    /// The snake_case name, matching the TypeScript union's string values.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::PendingRelease => "pending_release",
            Self::Completed => "completed",
            Self::Refunded => "refunded",
            Self::Disputed => "disputed",
        }
    }

    /// Parse either the contract's `contracttype` variant spelling
    /// (`InProgress`) or the SDK's (`in_progress`).
    pub fn from_contract_variant(variant: &str) -> Option<Self> {
        match variant {
            "Open" | "open" => Some(Self::Open),
            "InProgress" | "in_progress" => Some(Self::InProgress),
            "PendingRelease" | "pending_release" => Some(Self::PendingRelease),
            "Completed" | "completed" => Some(Self::Completed),
            "Refunded" | "refunded" => Some(Self::Refunded),
            "Disputed" | "disputed" => Some(Self::Disputed),
            _ => None,
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parameters for delegating work to another agent through escrow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestWorkParams {
    /// The agent expected to do the work. Recorded for the caller's own
    /// bookkeeping — `Escrow.create_job` posts an open job that any worker may
    /// accept, so this does not restrict who can.
    pub worker_agent: String,
    /// What the worker is being asked to do.
    pub task: String,
    /// Amount to lock in escrow, as a decimal string.
    pub escrow_amount: String,
    /// Asset code or token contract ID. Defaults to `XLM`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// Ledgers from now until the deadline. Defaults to 720 (~1 hour).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ledgers: Option<u32>,
    /// Optional third party who can settle a dispute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arbiter: Option<String>,
}

/// An escrow job's on-chain state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    /// The job ID.
    pub id: u64,
    /// Who posted it.
    pub requester: String,
    /// Who accepted it, if anyone yet.
    pub worker: Option<String>,
    /// The dispute arbiter, if one was named.
    pub arbiter: Option<String>,
    /// Token contract ID the escrow is held in.
    pub token: String,
    /// Escrowed amount, in stroops.
    pub amount: i128,
    /// What the worker was asked to do.
    pub task_description: String,
    /// The submitted result, once there is one.
    pub result: Option<String>,
    /// Ledger after which the job expires.
    pub deadline_ledger: u32,
    /// Where the job is in its lifecycle.
    pub status: JobStatus,
    /// Ledger the job was created at.
    pub created_at: u32,
}

/// Rate limits to configure for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitConfig {
    /// Per-transaction ceiling, as a decimal string.
    pub max_per_tx: String,
    /// Rolling hourly ceiling.
    pub max_per_hour: String,
    /// Rolling daily ceiling.
    pub max_per_day: String,
    /// Rolling hourly transaction-count ceiling.
    pub max_txs_per_hour: u32,
}

/// Current rate-limit usage alongside the configured limits.
///
/// When `configured` is `false`, `RateLimiter.set_limits` was never called for
/// this agent: `RateLimiter.check` then returns `true` unconditionally, so the
/// limits are not merely zero — they do not apply at all. Every other field is
/// a placeholder in that state and must not be read on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitStatus {
    /// Whether any limits are configured for this agent.
    pub configured: bool,
    /// `RateLimit.active`. Note that `check` does not read this — see
    /// [`crate::math::predict`].
    pub active: bool,
    /// Per-transaction ceiling.
    pub max_per_tx: String,
    /// Rolling hourly ceiling.
    pub max_per_hour: String,
    /// Rolling daily ceiling.
    pub max_per_day: String,
    /// Rolling hourly transaction-count ceiling.
    pub max_txs_per_hour: u32,
    /// Spent in the current hourly window.
    pub spent_this_hour: String,
    /// Spent in the current daily window.
    pub spent_today: String,
    /// Transactions in the current hourly window.
    pub txs_this_hour: u32,
    /// Ledger the hourly window started at.
    pub hour_window_start_ledger: u32,
    /// Ledger the daily window started at.
    pub day_window_start_ledger: u32,
}

impl RateLimitStatus {
    /// What [`crate::StellarAgent::rate_limit_status`] reports for an agent
    /// `set_limits` was never called for.
    pub fn unconfigured() -> Self {
        Self {
            configured: false,
            active: true,
            max_per_tx: "0".into(),
            max_per_hour: "0".into(),
            max_per_day: "0".into(),
            max_txs_per_hour: 0,
            spent_this_hour: "0".into(),
            spent_today: "0".into(),
            txs_this_hour: 0,
            hour_window_start_ledger: 0,
            day_window_start_ledger: 0,
        }
    }
}

/// The outcome of a submitted transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxResult {
    /// The transaction hash, hex-encoded.
    pub hash: String,
    /// Whether it reached a successful terminal status.
    pub success: bool,
    /// Ledger it was included in, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_names_round_trip() {
        for network in Network::ALL {
            assert_eq!(network.as_str().parse::<Network>().unwrap(), network);
        }
        assert!("mainet".parse::<Network>().is_err());
    }

    #[test]
    fn every_network_has_a_passphrase_and_endpoints() {
        for network in Network::ALL {
            let config = network.config();
            assert!(!config.network_passphrase.is_empty());
            assert!(config.rpc_url.starts_with("http"));
            assert!(config.horizon_url.starts_with("http"));
        }
    }

    #[test]
    fn spend_period_maps_both_spellings() {
        for period in [
            SpendPeriod::PerLedger,
            SpendPeriod::Hourly,
            SpendPeriod::Daily,
        ] {
            assert_eq!(
                SpendPeriod::from_contract_variant(period.contract_variant()),
                Some(period)
            );
            assert_eq!(
                SpendPeriod::from_contract_variant(period.as_str()),
                Some(period)
            );
        }
        assert_eq!(SpendPeriod::from_contract_variant("Weekly"), None);
    }

    #[test]
    fn job_status_maps_both_spellings() {
        for (variant, expected) in [
            ("InProgress", JobStatus::InProgress),
            ("in_progress", JobStatus::InProgress),
            ("PendingRelease", JobStatus::PendingRelease),
            ("disputed", JobStatus::Disputed),
        ] {
            assert_eq!(JobStatus::from_contract_variant(variant), Some(expected));
        }
        assert_eq!(JobStatus::from_contract_variant("Cancelled"), None);
    }

    #[test]
    fn json_field_names_match_the_typescript_sdk() {
        let params = OpenChannelParams {
            deposit: "10".into(),
            limit_per_period: "1".into(),
            period: SpendPeriod::Hourly,
            token: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"limitPerPeriod\""), "{json}");
        assert!(
            !json.contains("\"token\""),
            "an absent token is omitted: {json}"
        );
    }
}
