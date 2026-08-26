//! A Soroban RPC client: the transport half of the SDK.
//!
//! Soroban RPC is JSON-RPC 2.0 over HTTP. This module speaks exactly the five
//! methods the SDK needs — `getLatestLedger`, `getLedgerEntries`,
//! `simulateTransaction`, `sendTransaction`, `getTransaction` — and decodes
//! their base64 XDR payloads into [`stellar_xdr`] types, so callers upstream
//! never touch a base64 string.
//!
//! # Why account state comes from `getLedgerEntries` rather than Horizon
//!
//! Building a transaction needs the source account's sequence number. Horizon
//! exposes that directly, but requiring a Horizon endpoint would mean an agent
//! could not run against a bare RPC deployment — and the two can disagree
//! about the tip during a sync. [`SorobanRpcClient::account`] reads the
//! `AccountEntry` ledger entry from the same RPC server the transaction will
//! be submitted to, so the sequence number and the ledger it is valid against
//! come from one consistent view.
//!
//! # Error mapping
//!
//! Transport failures and JSON-RPC error objects both become
//! [`ErrorCode::NetworkError`]; a simulation that the host rejected becomes
//! [`ErrorCode::SimulationFailed`], with the contract's own panic text
//! classified through [`StellarAgentError::from_contract_message`] so a spend
//! limit surfaces as [`ErrorCode::SpendLimitExceeded`] rather than as a wall
//! of host-error text.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use stellar_xdr::curr::{
    AccountId, LedgerEntryData, LedgerKey, LedgerKeyAccount, Limits, ReadXdr, ScVal,
    SorobanAuthorizationEntry, SorobanTransactionData, TransactionMeta, WriteXdr,
};

use crate::error::{ErrorCode, Result, StellarAgentError};

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The current ledger, as reported by `getLatestLedger`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestLedger {
    /// The ledger's hash.
    pub id: String,
    /// The protocol version the network is running.
    pub protocol_version: u32,
    /// The ledger sequence number — the value every window calculation uses.
    pub sequence: u32,
}

/// The subset of an `AccountEntry` needed to build a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountState {
    /// The account's address.
    pub address: String,
    /// Its current sequence number. A transaction must use this **plus one**.
    pub sequence: i64,
    /// Native balance in stroops, for a quick funded/unfunded check.
    pub balance: i64,
}

/// One result of a simulated invocation.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// The value the invocation would return.
    pub return_value: ScVal,
    /// Authorisation entries the host says the invocation requires.
    ///
    /// Every entry with `Address` credentials must be signed before
    /// submission; entries with `SourceAccount` credentials are covered by the
    /// envelope signature.
    pub auth: Vec<SorobanAuthorizationEntry>,
}

/// A successful `simulateTransaction`.
#[derive(Debug, Clone)]
pub struct Simulation {
    /// The ledger the simulation ran against.
    pub latest_ledger: u32,
    /// The resource fee the host says this invocation needs, in stroops.
    pub min_resource_fee: i64,
    /// Footprint and resource limits to attach to the real transaction.
    pub transaction_data: SorobanTransactionData,
    /// The invocation's result. `None` for a transaction with no return value.
    pub result: Option<SimulationResult>,
}

/// What `sendTransaction` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendStatus {
    /// Accepted into the queue.
    Pending,
    /// Already seen — the same envelope was submitted before.
    Duplicate,
    /// Rejected for now; the caller may retry.
    TryAgainLater,
    /// Rejected outright.
    Error,
}

impl SendStatus {
    fn parse(value: &str) -> Self {
        match value {
            "PENDING" => Self::Pending,
            "DUPLICATE" => Self::Duplicate,
            "TRY_AGAIN_LATER" => Self::TryAgainLater,
            _ => Self::Error,
        }
    }

    /// Whether the transaction reached the queue and is worth polling for.
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Pending | Self::Duplicate)
    }
}

/// The response to `sendTransaction`.
#[derive(Debug, Clone)]
pub struct SendResponse {
    /// Whether the submission was accepted.
    pub status: SendStatus,
    /// The transaction hash, hex-encoded. Valid even for a rejection, so a
    /// caller can report *which* transaction failed.
    pub hash: String,
    /// Base64 `TransactionResult`, present when the submission was rejected.
    pub error_result_xdr: Option<String>,
    /// Diagnostic events, when the server produced any.
    pub diagnostic_events_xdr: Vec<String>,
}

/// Where a submitted transaction has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionStatus {
    /// Not yet visible to this RPC server.
    NotFound,
    /// Included and successful.
    Success,
    /// Included and failed.
    Failed,
}

/// The response to `getTransaction`.
#[derive(Debug, Clone)]
pub struct TransactionResponse {
    /// Terminal or not-yet status.
    pub status: TransactionStatus,
    /// The ledger it was included in, once known.
    pub ledger: Option<u32>,
    /// The invocation's return value, once successful.
    pub return_value: Option<ScVal>,
    /// Base64 `TransactionResult`, when the server supplied one.
    pub result_xdr: Option<String>,
}

/// A JSON-RPC client for one Soroban RPC endpoint.
#[derive(Debug)]
pub struct SorobanRpcClient {
    url: String,
    client: reqwest::Client,
    /// Monotonic JSON-RPC request ids, so a response can be matched to its
    /// request in a server log.
    next_id: AtomicU64,
}

impl SorobanRpcClient {
    /// Build a client for `url` with the default timeout.
    ///
    /// # Errors
    ///
    /// Rejects a URL that is not absolute, and — outside loopback — one that
    /// is not HTTPS. See [`is_loopback_url`]: a plaintext connection to a real
    /// network would expose submitted transactions, so it must fail loudly
    /// rather than silently transmitting in the clear.
    pub fn new(url: impl Into<String>) -> Result<Self> {
        Self::with_timeout(url, DEFAULT_TIMEOUT)
    }

    /// Build a client with an explicit per-request timeout.
    pub fn with_timeout(url: impl Into<String>, timeout: Duration) -> Result<Self> {
        let url = url.into();
        let parsed = url::Url::parse(&url).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                format!("Soroban RPC url \"{url}\" is not a valid URL: {error}"),
            )
        })?;

        if parsed.scheme() != "https" && !is_loopback_url(&url) {
            return Err(StellarAgentError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "Refusing to talk to {url} over plaintext HTTP. Signed transactions would \
                     cross the network in the clear. Use https, or a loopback address for local \
                     development."
                ),
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| {
                StellarAgentError::new(ErrorCode::NetworkError, "could not build an HTTP client")
                    .with_source(error)
            })?;

        Ok(Self {
            url,
            client,
            next_id: AtomicU64::new(1),
        })
    }

    /// The endpoint this client talks to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The current ledger sequence and protocol version.
    pub async fn latest_ledger(&self) -> Result<LatestLedger> {
        let value = self.request("getLatestLedger", json!({})).await?;
        serde_json::from_value(value).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                "getLatestLedger returned an unexpected shape",
            )
            .with_source(error)
        })
    }

    /// Read an account's sequence number and native balance.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidArgument`] when the account does not exist on this
    /// network — which for a freshly generated keypair means "not funded yet",
    /// and is worth saying so rather than surfacing an empty entry list.
    pub async fn account(&self, address: &str) -> Result<AccountState> {
        let account_id: AccountId = address.parse().map_err(|_| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                format!("Not a valid Stellar account address: {address}"),
            )
        })?;

        let key = LedgerKey::Account(LedgerKeyAccount { account_id })
            .to_xdr_base64(Limits::none())
            .map_err(|error| {
                StellarAgentError::new(ErrorCode::InvalidArgument, "could not encode a ledger key")
                    .with_source(error)
            })?;

        let value = self
            .request("getLedgerEntries", json!({ "keys": [key] }))
            .await?;

        let entries = value
            .get("entries")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();

        let entry = entries.first().ok_or_else(|| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "Account {address} does not exist on this network. A newly generated \
                     account must be funded before it can submit transactions."
                ),
            )
        })?;

        // Field name has moved between RPC releases; accept both rather than
        // failing on a server that is one version ahead or behind.
        let encoded = entry
            .get("xdr")
            .or_else(|| entry.get("dataXdr"))
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                StellarAgentError::new(
                    ErrorCode::NetworkError,
                    "getLedgerEntries returned an entry with no xdr field",
                )
            })?;

        let data = LedgerEntryData::from_xdr_base64(encoded, Limits::none()).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                "could not decode the account entry",
            )
            .with_source(error)
        })?;

        match data {
            LedgerEntryData::Account(account) => Ok(AccountState {
                address: address.to_string(),
                sequence: account.seq_num.0,
                balance: account.balance,
            }),
            other => Err(StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("expected an account ledger entry, got {}", other.name()),
            )),
        }
    }

    /// Simulate a transaction envelope.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::SimulationFailed`] when the host rejected the invocation,
    /// with the contract's panic message classified onto a specific code where
    /// one applies. A `restorePreamble` in the response is also an error here:
    /// the invocation touches archived ledger entries and cannot proceed until
    /// they are restored, which is a distinct situation from a plain failure
    /// and says so.
    pub async fn simulate_transaction(&self, envelope_xdr: &str) -> Result<Simulation> {
        let value = self
            .request(
                "simulateTransaction",
                json!({ "transaction": envelope_xdr }),
            )
            .await?;

        if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
            return Err(StellarAgentError::from_contract_message(
                ErrorCode::SimulationFailed,
                format!("simulation failed: {error}"),
            ));
        }
        if value.get("restorePreamble").is_some_and(|p| !p.is_null()) {
            return Err(StellarAgentError::new(
                ErrorCode::SimulationFailed,
                "this invocation reads ledger entries that have been archived; they must be \
                 restored before it can run",
            ));
        }

        let latest_ledger = value
            .get("latestLedger")
            .and_then(|l| l.as_u64())
            .unwrap_or_default() as u32;

        // `minResourceFee` is a stringified integer, because it can exceed
        // what JSON numbers represent exactly.
        let min_resource_fee = value
            .get("minResourceFee")
            .and_then(|f| f.as_str())
            .and_then(|f| f.parse::<i64>().ok())
            .unwrap_or_default();

        let transaction_data = value
            .get("transactionData")
            .and_then(|d| d.as_str())
            .ok_or_else(|| {
                StellarAgentError::new(
                    ErrorCode::SimulationFailed,
                    "simulation returned no transactionData, so the transaction has no footprint \
                     to submit with",
                )
            })
            .and_then(|encoded| {
                SorobanTransactionData::from_xdr_base64(encoded, Limits::none()).map_err(|error| {
                    StellarAgentError::new(
                        ErrorCode::NetworkError,
                        "could not decode the simulation's transactionData",
                    )
                    .with_source(error)
                })
            })?;

        let result = match value.get("results").and_then(|r| r.as_array()) {
            Some(results) if !results.is_empty() => {
                let first = &results[0];
                let return_value = match first.get("xdr").and_then(|x| x.as_str()) {
                    Some(encoded) => decode_scval(encoded)?,
                    None => ScVal::Void,
                };
                let auth = first
                    .get("auth")
                    .and_then(|a| a.as_array())
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|e| e.as_str())
                            .map(decode_auth_entry)
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                Some(SimulationResult { return_value, auth })
            }
            _ => None,
        };

        Ok(Simulation {
            latest_ledger,
            min_resource_fee,
            transaction_data,
            result,
        })
    }

    /// Submit a signed transaction envelope.
    ///
    /// Returns as soon as the server has queued (or rejected) it — use
    /// [`SorobanRpcClient::poll_transaction`] to wait for a terminal status.
    pub async fn send_transaction(&self, envelope_xdr: &str) -> Result<SendResponse> {
        let value = self
            .request("sendTransaction", json!({ "transaction": envelope_xdr }))
            .await?;

        Ok(SendResponse {
            status: SendStatus::parse(value.get("status").and_then(|s| s.as_str()).unwrap_or("")),
            hash: value
                .get("hash")
                .and_then(|h| h.as_str())
                .unwrap_or_default()
                .to_string(),
            error_result_xdr: value
                .get("errorResultXdr")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            diagnostic_events_xdr: value
                .get("diagnosticEventsXdr")
                .and_then(|e| e.as_array())
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|e| e.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Look up a submitted transaction by hash.
    pub async fn get_transaction(&self, hash: &str) -> Result<TransactionResponse> {
        let value = self
            .request("getTransaction", json!({ "hash": hash }))
            .await?;

        let status = match value.get("status").and_then(|s| s.as_str()).unwrap_or("") {
            "SUCCESS" => TransactionStatus::Success,
            "FAILED" => TransactionStatus::Failed,
            _ => TransactionStatus::NotFound,
        };

        // Newer servers hand back `returnValue` directly; older ones only
        // include it inside `resultMetaXdr`. Prefer the cheap path and fall
        // back rather than requiring one particular RPC version.
        let return_value = match value.get("returnValue").and_then(|v| v.as_str()) {
            Some(encoded) => Some(decode_scval(encoded)?),
            None => value
                .get("resultMetaXdr")
                .and_then(|m| m.as_str())
                .map(return_value_from_meta)
                .transpose()?
                .flatten(),
        };

        Ok(TransactionResponse {
            status,
            ledger: value
                .get("ledger")
                .and_then(|l| l.as_u64())
                .map(|l| l as u32),
            return_value,
            result_xdr: value
                .get("resultXdr")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        })
    }

    /// Poll `getTransaction` until it reaches a terminal status.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::TransactionTimeout`] after `attempts` polls without a
    /// terminal status. That is not the same as failure — the transaction may
    /// still land — so the error carries the hash, and the code is retryable.
    pub async fn poll_transaction(
        &self,
        hash: &str,
        attempts: u32,
        interval: Duration,
    ) -> Result<TransactionResponse> {
        for attempt in 0..attempts {
            let response = self.get_transaction(hash).await?;
            if response.status != TransactionStatus::NotFound {
                return Ok(response);
            }
            // Sleep between polls but not after the last one — an extra wait
            // before reporting a timeout helps nobody.
            if attempt + 1 < attempts {
                tokio::time::sleep(interval).await;
            }
        }

        Err(StellarAgentError::new(
            ErrorCode::TransactionTimeout,
            format!(
                "transaction did not reach a terminal status within {attempts} polls; it may \
                     still be included"
            ),
        )
        .with_transaction_hash(hash))
    }

    /// Issue one JSON-RPC call and return its `result`.
    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let response = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                StellarAgentError::new(
                    ErrorCode::NetworkError,
                    format!(
                        "{method} failed while communicating with Soroban RPC at {}",
                        self.url
                    ),
                )
                .with_source(error)
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("{method}: could not read the response body"),
            )
            .with_source(error)
        })?;

        if !status.is_success() {
            return Err(StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("{method}: Soroban RPC responded {status}: {text}"),
            ));
        }

        let envelope: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("{method}: Soroban RPC returned a body that is not JSON: {text}"),
            )
            .with_source(error)
        })?;

        if let Some(error) = envelope.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            return Err(StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("{method}: Soroban RPC error {code}: {message}"),
            ));
        }

        envelope.get("result").cloned().ok_or_else(|| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("{method}: Soroban RPC response had neither a result nor an error"),
            )
        })
    }
}

/// Whether a URL points at the local machine, and may therefore be spoken to
/// over plaintext HTTP.
///
/// Anything else — **including a LAN address** — must use TLS, so a
/// misconfigured endpoint fails loudly instead of silently transmitting signed
/// transactions in the clear. The same rule the TypeScript SDK applies.
///
/// ```
/// use stellaragent::rpc::is_loopback_url;
///
/// assert!(is_loopback_url("http://localhost:8000/soroban/rpc"));
/// assert!(is_loopback_url("http://127.0.0.1:8000"));
/// assert!(!is_loopback_url("http://192.168.1.10:8000"));
/// assert!(!is_loopback_url("http://rpc.example.com"));
/// ```
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() == "https" {
        return false;
    }
    matches!(
        parsed.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    )
}

fn decode_scval(encoded: &str) -> Result<ScVal> {
    ScVal::from_xdr_base64(encoded, Limits::none()).map_err(|error| {
        StellarAgentError::new(
            ErrorCode::NetworkError,
            "Soroban RPC returned a value that is not valid ScVal XDR",
        )
        .with_source(error)
    })
}

fn decode_auth_entry(encoded: &str) -> Result<SorobanAuthorizationEntry> {
    SorobanAuthorizationEntry::from_xdr_base64(encoded, Limits::none()).map_err(|error| {
        StellarAgentError::new(
            ErrorCode::NetworkError,
            "Soroban RPC returned an authorization entry that is not valid XDR",
        )
        .with_source(error)
    })
}

/// Pull the invocation's return value out of a transaction's result meta.
///
/// Handles both the v3 meta protocol 20-22 emit and the v4 meta protocol 23
/// introduced, so the client is not pinned to one network version.
fn return_value_from_meta(encoded: &str) -> Result<Option<ScVal>> {
    let meta = TransactionMeta::from_xdr_base64(encoded, Limits::none()).map_err(|error| {
        StellarAgentError::new(
            ErrorCode::NetworkError,
            "could not decode the transaction's result meta",
        )
        .with_source(error)
    })?;

    Ok(match meta {
        TransactionMeta::V3(v3) => v3.soroban_meta.map(|soroban| soroban.return_value),
        TransactionMeta::V4(v4) => v4.soroban_meta.and_then(|soroban| soroban.return_value),
        // A classic (non-Soroban) transaction has no invocation result. That
        // is not an error — there is simply nothing to return.
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_http_is_refused_outside_loopback() {
        let error = SorobanRpcClient::new("http://rpc.example.com").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("plaintext"), "{}", error.message());

        // A LAN address is not loopback, however private it feels.
        assert!(SorobanRpcClient::new("http://192.168.1.10:8000").is_err());
    }

    #[test]
    fn loopback_and_https_endpoints_are_accepted() {
        assert!(SorobanRpcClient::new("http://localhost:8000/soroban/rpc").is_ok());
        assert!(SorobanRpcClient::new("http://127.0.0.1:8000").is_ok());
        assert!(SorobanRpcClient::new("https://soroban-rpc.stellar.org").is_ok());
    }

    #[test]
    fn a_url_that_is_not_a_url_is_rejected_at_construction() {
        let error = SorobanRpcClient::new("localhost:8000").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn send_status_parsing_treats_unknown_values_as_errors() {
        assert_eq!(SendStatus::parse("PENDING"), SendStatus::Pending);
        assert_eq!(SendStatus::parse("DUPLICATE"), SendStatus::Duplicate);
        assert_eq!(
            SendStatus::parse("TRY_AGAIN_LATER"),
            SendStatus::TryAgainLater
        );
        assert_eq!(SendStatus::parse("ERROR"), SendStatus::Error);
        assert_eq!(SendStatus::parse("something new"), SendStatus::Error);

        assert!(SendStatus::Pending.is_accepted());
        assert!(SendStatus::Duplicate.is_accepted());
        assert!(!SendStatus::TryAgainLater.is_accepted());
    }

    #[test]
    fn a_classic_transactions_meta_yields_no_return_value_rather_than_an_error() {
        use stellar_xdr::curr::{ExtensionPoint, TransactionMetaV3};

        let meta = TransactionMeta::V3(TransactionMetaV3 {
            ext: ExtensionPoint::V0,
            tx_changes_before: Default::default(),
            operations: Default::default(),
            tx_changes_after: Default::default(),
            soroban_meta: None,
        });
        let encoded = meta.to_xdr_base64(Limits::none()).unwrap();
        assert_eq!(return_value_from_meta(&encoded).unwrap(), None);
    }

    #[test]
    fn a_soroban_return_value_is_read_out_of_v3_meta() {
        use stellar_xdr::curr::{
            ExtensionPoint, SorobanTransactionMeta, SorobanTransactionMetaExt, TransactionMetaV3,
        };

        let meta = TransactionMeta::V3(TransactionMetaV3 {
            ext: ExtensionPoint::V0,
            tx_changes_before: Default::default(),
            operations: Default::default(),
            tx_changes_after: Default::default(),
            soroban_meta: Some(SorobanTransactionMeta {
                ext: SorobanTransactionMetaExt::V0,
                events: Default::default(),
                return_value: ScVal::U64(42),
                diagnostic_events: Default::default(),
            }),
        });
        let encoded = meta.to_xdr_base64(Limits::none()).unwrap();
        assert_eq!(
            return_value_from_meta(&encoded).unwrap(),
            Some(ScVal::U64(42))
        );
    }

    #[test]
    fn malformed_meta_is_reported_rather_than_silently_dropped() {
        assert!(return_value_from_meta("not xdr").is_err());
    }
}
