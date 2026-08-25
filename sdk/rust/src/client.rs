//! [`StellarAgent`] — the SDK's entry point.
//!
//! Rust port of `packages/core/src/index.ts`. Method names are `snake_case`;
//! everything else — arguments, semantics, validation order, error text — is
//! deliberately the same, so an agent written against one SDK reads the same
//! in the other.
//!
//! # The invocation pipeline
//!
//! Every contract call goes through one private path, `StellarAgent::invoke`:
//!
//! 1. **Build** — read the source account's sequence number from RPC and wrap
//!    the invocation in a transaction envelope.
//! 2. **Simulate** — ask the host what the call would do, what it would
//!    return, what it would cost, and which authorisations it needs.
//! 3. **Sign the authorisations** — each entry with `Address` credentials is
//!    signed separately from the envelope, bound to a nonce, an expiry ledger
//!    and one specific invocation tree.
//! 4. **Assemble** — attach the simulated footprint and resource fee. Skipping
//!    this is the classic way to get `txSorobanInvalid` back: the network will
//!    not run an invocation whose footprint it was not told about.
//! 5. **Sign and submit** the envelope.
//! 6. **Poll** until the transaction reaches a terminal status.
//!
//! Read-only calls stop after step 2. They cost nothing, need no signature,
//! and — importantly — need no funded account, which is why
//! [`StellarAgent::channel`] and friends work against an agent that has never
//! submitted anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use num_bigint::BigInt;
use sha2::{Digest, Sha256};
use stellar_xdr::curr::{
    Asset, ContractIdPreimage, HashIdPreimage, HashIdPreimageContractId, HostFunction,
    InvokeContractArgs, InvokeHostFunctionOp, Limits, Memo, Operation, OperationBody,
    Preconditions, ReadXdr, ScAddress, ScSymbol, ScVal, SequenceNumber, SorobanAuthorizationEntry,
    SorobanCredentials, TimeBounds, TimePoint, Transaction, TransactionEnvelope, TransactionExt,
    TransactionV1Envelope, WriteXdr,
};

use crate::contracts::{self, ContractAddresses, ContractKey};
use crate::error::{ErrorCode, Result, StellarAgentError};
use crate::math::{self, LedgerCloseEstimate, LedgerCloseSample};
use crate::rpc::{SorobanRpcClient, TransactionStatus};
use crate::scval;
use crate::signer::{KeypairSigner, SignAuthEntryOptions, SignTransactionOptions, Signer};
use crate::types::{
    AgentInfo, ChannelInfo, JobInfo, JobStatus, Network, NetworkConfig, OpenChannelParams,
    PayForApiParams, RateLimitConfig, RateLimitStatus, RequestWorkParams, SpendPeriod, SpendReport,
    TxResult,
};

/// Stellar's base fee, in stroops. The resource fee from simulation is added
/// on top of this for a Soroban invocation.
const BASE_FEE: u32 = 100;

/// How long a built transaction stays valid, in seconds.
const TRANSACTION_TIMEOUT_SECONDS: u64 = 30;

/// How far past the simulated ledger an authorisation signature stays valid.
///
/// Matches the TypeScript SDK's `simulation.latestLedger + 100`: long enough
/// to survive submission and inclusion (~8 minutes at 5s ledgers), short
/// enough that a leaked signed entry is not indefinitely replayable.
const AUTH_VALIDITY_LEDGERS: u32 = 100;

/// How many times to poll for a terminal transaction status, and how long to
/// wait between polls.
const POLL_ATTEMPTS: u32 = 30;
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The testnet funding service.
const FRIENDBOT_URL: &str = "https://friendbot.stellar.org";

/// Main SDK type for AI agent payment operations on Stellar.
///
/// Build one with [`StellarAgent::builder`].
///
/// ```no_run
/// use stellaragent::{StellarAgent, types::{Network, PayForApiParams}};
///
/// # async fn example() -> Result<(), stellaragent::StellarAgentError> {
/// let agent = StellarAgent::builder()
///     .network(Network::Testnet)
///     .secret_key("SB...")
///     .build()
///     .await?;
///
/// agent.pay_for_api(&PayForApiParams {
///     endpoint: "https://api.example.com/inference".into(),
///     amount: "0.001".into(),
///     asset: Some("USDC".into()),
///     ..Default::default()
/// })
/// .await?;
/// # Ok(())
/// # }
/// ```
pub struct StellarAgent {
    /// Where signing happens. For a [`crate::signer::RemoteSigner`] this holds
    /// a URL and a token — no key material — which is the entire point.
    signer: Arc<dyn Signer>,
    /// Resolved once at build time, so `address()` stays synchronous even when
    /// the signer derives it over the network.
    public_key: String,
    network: Network,
    network_config: NetworkConfig,
    contracts: ContractAddresses,
    asset_contracts: HashMap<String, String>,
    rpc: SorobanRpcClient,
    http: reqwest::Client,
    active_channel_id: Mutex<Option<u64>>,
}

impl std::fmt::Debug for StellarAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omits the signer: a `Debug` line lands in logs and
        // tracebacks, and nothing about the signing identity belongs there
        // beyond the public address.
        f.debug_struct("StellarAgent")
            .field("address", &self.public_key)
            .field("network", &self.network)
            .finish_non_exhaustive()
    }
}

/// Builder for [`StellarAgent`].
///
/// Supply **at most one** of [`StellarAgentBuilder::signer`] or
/// [`StellarAgentBuilder::secret_key`]. With neither, a fresh random keypair
/// is generated — useful for a testnet demo, never for anything else.
#[derive(Default)]
pub struct StellarAgentBuilder {
    network: Option<Network>,
    network_config: Option<NetworkConfig>,
    signer: Option<Arc<dyn Signer>>,
    secret_key: Option<String>,
    contract_overrides: Vec<(ContractKey, String)>,
    asset_contracts: HashMap<String, String>,
    allow_unconfigured_contracts: bool,
    fund_new_testnet_accounts: bool,
    rpc_timeout: Option<Duration>,
}

impl StellarAgentBuilder {
    /// Which network to talk to. Defaults to [`Network::Testnet`].
    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = Some(network);
        self
    }

    /// Override the endpoints and passphrase the network name would supply.
    ///
    /// For a standalone network on a non-default port, or an RPC provider that
    /// is not the public one.
    #[must_use]
    pub fn network_config(mut self, config: NetworkConfig) -> Self {
        self.network_config = Some(config);
        self
    }

    /// Sign through an arbitrary [`Signer`] — the secret never enters this
    /// process.
    #[must_use]
    pub fn signer(mut self, signer: Arc<dyn Signer>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Sign with an in-memory keypair restored from a Stellar secret seed.
    #[must_use]
    pub fn secret_key(mut self, secret: impl Into<String>) -> Self {
        self.secret_key = Some(secret.into());
        self
    }

    /// Pin one contract's address, overriding the environment.
    #[must_use]
    pub fn contract(mut self, key: ContractKey, address: impl Into<String>) -> Self {
        self.contract_overrides.push((key, address.into()));
        self
    }

    /// Map an asset code to its token contract ID, so amounts can be given as
    /// `"USDC"` rather than a `C…` address.
    #[must_use]
    pub fn asset_contract(mut self, code: impl Into<String>, address: impl Into<String>) -> Self {
        self.asset_contracts.insert(code.into(), address.into());
        self
    }

    /// Skip the deployed-contracts check.
    ///
    /// Pass this when you only need contract-free calls such as
    /// [`StellarAgent::balance`]; otherwise leave it off, so a misconfigured
    /// deployment fails at build time with an actionable message rather than
    /// from the middle of a payment.
    #[must_use]
    pub fn allow_unconfigured_contracts(mut self, allow: bool) -> Self {
        self.allow_unconfigured_contracts = allow;
        self
    }

    /// Fund a *freshly generated* keypair from friendbot on testnet.
    ///
    /// Off by default, and deliberately narrower than the TypeScript SDK's
    /// implicit behaviour: funding is a network side effect, and one that
    /// happens without being asked for is a surprise in a library. Only ever
    /// applies when no secret and no signer were supplied.
    #[must_use]
    pub fn fund_new_testnet_accounts(mut self, fund: bool) -> Self {
        self.fund_new_testnet_accounts = fund;
        self
    }

    /// Override the RPC per-request timeout.
    #[must_use]
    pub fn rpc_timeout(mut self, timeout: Duration) -> Self {
        self.rpc_timeout = Some(timeout);
        self
    }

    /// Resolve everything and build the agent.
    ///
    /// # Errors
    ///
    /// - [`ErrorCode::InvalidArgument`] when both a signer and a secret key
    ///   were supplied. Handing a secret to an agent that also has a remote
    ///   signer defeats the point of the signer, so it is refused rather than
    ///   silently preferring one.
    /// - [`ErrorCode::InvalidArgument`] when the resolved contracts are not
    ///   real deployed contract IDs and `allow_unconfigured_contracts` is off.
    pub async fn build(self) -> Result<StellarAgent> {
        if self.signer.is_some() && self.secret_key.is_some() {
            return Err(StellarAgentError::new(
                ErrorCode::InvalidArgument,
                "StellarAgent: pass either a signer or a secret_key, not both. Supplying a \
                 secret alongside a remote signer would defeat the point of the signer.",
            ));
        }

        let network = self.network.unwrap_or(Network::Testnet);
        let network_config = self.network_config.unwrap_or_else(|| network.config());

        let generated = self.signer.is_none() && self.secret_key.is_none();
        let signer: Arc<dyn Signer> = match (self.signer, self.secret_key) {
            (Some(signer), _) => signer,
            (None, Some(secret)) => Arc::new(KeypairSigner::from_secret(&secret)?),
            (None, None) => Arc::new(KeypairSigner::random()),
        };

        // Derived through the Signer interface, so a remote signer reports its
        // address without the secret ever being loaded here.
        let public_key = signer.public_key().await?;

        let contracts = contracts::resolve_contracts(network, &self.contract_overrides);
        if !self.allow_unconfigured_contracts {
            contracts::assert_deployed(network, &contracts)?;
        }

        let rpc = match self.rpc_timeout {
            Some(timeout) => SorobanRpcClient::with_timeout(&network_config.rpc_url, timeout)?,
            None => SorobanRpcClient::new(&network_config.rpc_url)?,
        };

        let http = reqwest::Client::builder()
            .timeout(self.rpc_timeout.unwrap_or(Duration::from_secs(30)))
            .build()
            .map_err(|error| {
                StellarAgentError::new(ErrorCode::NetworkError, "could not build an HTTP client")
                    .with_source(error)
            })?;

        let agent = StellarAgent {
            signer,
            public_key,
            network,
            network_config,
            contracts,
            asset_contracts: self.asset_contracts,
            rpc,
            http,
            active_channel_id: Mutex::new(None),
        };

        if generated && network == Network::Testnet && self.fund_new_testnet_accounts {
            agent.fund_from_friendbot().await;
        }

        Ok(agent)
    }
}

impl StellarAgent {
    /// Start building an agent.
    pub fn builder() -> StellarAgentBuilder {
        StellarAgentBuilder::default()
    }

    // ── Identity ─────────────────────────────────────────────────────────────

    /// The agent's Stellar public address.
    pub fn address(&self) -> &str {
        &self.public_key
    }

    /// Which network this agent talks to.
    pub fn network(&self) -> Network {
        self.network
    }

    /// RPC, Horizon and passphrase for the selected network.
    pub fn network_config(&self) -> &NetworkConfig {
        &self.network_config
    }

    /// The resolved contract addresses this agent will call.
    pub fn contracts(&self) -> &ContractAddresses {
        &self.contracts
    }

    /// The RPC client, for callers that need a method this SDK does not wrap.
    pub fn rpc(&self) -> &SorobanRpcClient {
        &self.rpc
    }

    /// The channel [`StellarAgent::pay_for_api`] draws from by default.
    ///
    /// Set by [`StellarAgent::open_channel`], cleared by
    /// [`StellarAgent::close_channel`].
    pub fn active_channel_id(&self) -> Option<u64> {
        *self.active_channel_id.lock().expect("active channel lock")
    }

    /// Point the agent at an existing channel without reopening one.
    pub fn set_active_channel_id(&self, channel_id: Option<u64>) {
        *self.active_channel_id.lock().expect("active channel lock") = channel_id;
    }

    // ── Agent registry ───────────────────────────────────────────────────────

    /// Register this agent in `AgentWalletFactory`, returning its agent ID.
    pub async fn create_agent_wallet(&self, name: &str) -> Result<u64> {
        let (value, _) = self
            .invoke(
                ContractKey::AgentWalletFactory,
                "create_agent",
                vec![
                    scval::address(self.address())?,
                    scval::address(self.address())?,
                    scval::string(name)?,
                ],
                false,
            )
            .await?;
        scval::as_u64(&value)
    }

    /// Read a registered agent.
    pub async fn agent(&self, agent_id: u64) -> Result<AgentInfo> {
        let (value, _) = self
            .invoke(
                ContractKey::AgentWalletFactory,
                "get_agent",
                vec![scval::u64_value(agent_id)],
                true,
            )
            .await?;

        Ok(AgentInfo {
            id: agent_id,
            address: scval::as_address_string(scval::field(&value, "address")?)?,
            name: scval::as_utf8(scval::field(&value, "name")?)?,
            owner: scval::as_address_string(scval::field(&value, "owner")?)?,
            active: scval::as_bool(scval::field(&value, "active")?)?,
            created_at: scval::as_u32(scval::field(&value, "created_at")?)?,
            total_ops: scval::as_u64(scval::field(&value, "total_ops")?)?,
        })
    }

    // ── Payment channel ──────────────────────────────────────────────────────

    /// Open a payment channel, deposit into it, and set its spend limit.
    ///
    /// The new channel becomes this agent's active channel, so a subsequent
    /// [`StellarAgent::pay_for_api`] needs no `channel_id`.
    pub async fn open_channel(&self, params: &OpenChannelParams) -> Result<u64> {
        let token = self.resolve_asset_contract(params.token.as_deref().unwrap_or("XLM"))?;
        let (value, _) = self
            .invoke(
                ContractKey::PaymentChannel,
                "open_channel",
                vec![
                    scval::address(self.address())?,
                    scval::address(self.address())?,
                    scval::address(&token)?,
                    scval::amount(&params.deposit)?,
                    scval::amount(&params.limit_per_period)?,
                    scval::enum_variant(params.period.contract_variant())?,
                ],
                false,
            )
            .await?;

        let channel_id = scval::as_u64(&value)?;
        self.set_active_channel_id(Some(channel_id));
        Ok(channel_id)
    }

    /// Close a channel and return its remaining balance to the owner.
    ///
    /// Defaults to the active channel.
    pub async fn close_channel(&self, channel_id: Option<u64>) -> Result<TxResult> {
        let channel_id = self.require_channel(channel_id)?;
        let (_, tx) = self
            .invoke(
                ContractKey::PaymentChannel,
                "close_channel",
                vec![
                    scval::address(self.address())?,
                    scval::u64_value(channel_id),
                ],
                false,
            )
            .await?;

        if self.active_channel_id() == Some(channel_id) {
            self.set_active_channel_id(None);
        }
        Ok(tx)
    }

    /// Pay for an API call, deducting from a payment channel.
    ///
    /// On-chain spend limits are enforced automatically — this call fails with
    /// [`ErrorCode::SpendLimitExceeded`] rather than overspending.
    ///
    /// If `dest_asset` differs from the channel's settlement asset, this
    /// settles the recipient in `dest_asset` instead — a channel funded in
    /// USDC paying a provider that only accepts XLM — by invoking
    /// `pay_with_conversion` rather than `pay`. The spend limit is still
    /// enforced in the channel's own settlement asset either way, and
    /// `min_received` is the slippage floor in `dest_asset` units.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidArgument`] when exactly one of `dest_asset` and
    /// `min_received` is set. A conversion without a floor would accept any
    /// rate at all, and a floor without a conversion has nothing to bound.
    pub async fn pay_for_api(&self, params: &PayForApiParams) -> Result<TxResult> {
        let channel_id = self.require_channel(params.channel_id)?;

        if params.dest_asset.is_some() != params.min_received.is_some() {
            return Err(StellarAgentError::new(
                ErrorCode::InvalidArgument,
                "dest_asset and min_received must be set together",
            ));
        }

        let mut args = vec![
            scval::address(self.address())?,
            scval::u64_value(channel_id),
            scval::address(params.recipient.as_deref().unwrap_or(self.address()))?,
            scval::amount(&params.amount)?,
        ];

        let method = match (&params.dest_asset, &params.min_received) {
            (Some(dest_asset), Some(min_received)) => {
                args.push(scval::address(&self.resolve_asset_contract(dest_asset)?)?);
                args.push(scval::amount(min_received)?);
                "pay_with_conversion"
            }
            _ => "pay",
        };
        args.push(scval::bytes_from_str(&params.endpoint)?);

        let (_, tx) = self
            .invoke(ContractKey::PaymentChannel, method, args, false)
            .await?;
        Ok(tx)
    }

    // ── Agent-to-agent escrow ────────────────────────────────────────────────

    /// Create an escrow job, locking payment until the work is delivered.
    ///
    /// The deadline is expressed in ledgers from *now*, and resolved against
    /// the current ledger at call time — an absolute deadline computed by the
    /// caller would go stale between building and submitting.
    pub async fn request_work(&self, params: &RequestWorkParams) -> Result<u64> {
        let deadline_offset = params.deadline_ledgers.unwrap_or(720);
        if deadline_offset == 0 {
            return Err(StellarAgentError::new(
                ErrorCode::InvalidArgument,
                "deadline_ledgers must be a positive integer",
            ));
        }

        let latest = self.rpc.latest_ledger().await?.sequence;
        let deadline = latest.checked_add(deadline_offset).ok_or_else(|| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                "deadline ledger exceeds the u32 range",
            )
        })?;

        let token = self.resolve_asset_contract(params.asset.as_deref().unwrap_or("XLM"))?;
        let arbiter = match &params.arbiter {
            Some(address) => scval::address(address)?,
            None => scval::void(),
        };

        let (value, _) = self
            .invoke(
                ContractKey::Escrow,
                "create_job",
                vec![
                    scval::address(self.address())?,
                    scval::address(&token)?,
                    scval::amount(&params.escrow_amount)?,
                    scval::bytes_from_str(&params.task)?,
                    scval::u32_value(deadline),
                    arbiter,
                ],
                false,
            )
            .await?;
        scval::as_u64(&value)
    }

    /// Accept an open escrow job as a worker agent.
    pub async fn accept_job(&self, job_id: u64) -> Result<TxResult> {
        self.escrow_action("accept_job", job_id, None).await
    }

    /// Submit a work result for an escrow job.
    pub async fn submit_result(&self, job_id: u64, result: &str) -> Result<TxResult> {
        self.escrow_action(
            "submit_result",
            job_id,
            Some(scval::bytes_from_str(result)?),
        )
        .await
    }

    /// Release escrow payment to the worker.
    pub async fn release_payment(&self, job_id: u64) -> Result<TxResult> {
        self.escrow_action("release", job_id, None).await
    }

    /// Read an escrow job.
    pub async fn job(&self, job_id: u64) -> Result<JobInfo> {
        let (value, _) = self
            .invoke(
                ContractKey::Escrow,
                "get_job",
                vec![scval::u64_value(job_id)],
                true,
            )
            .await?;

        let status_raw = scval::as_enum_variant(scval::field(&value, "status")?)?;
        let status = JobStatus::from_contract_variant(&status_raw).ok_or_else(|| {
            StellarAgentError::new(
                ErrorCode::ContractError,
                format!("Unknown job status: {status_raw}"),
            )
        })?;

        let result_field = scval::field(&value, "result")?;
        let result = match scval::as_option(result_field) {
            Some(value) => Some(scval::as_utf8(value)?),
            None => None,
        };

        Ok(JobInfo {
            id: job_id,
            requester: scval::as_address_string(scval::field(&value, "requester")?)?,
            worker: scval::as_optional_address(scval::field(&value, "worker")?)?,
            arbiter: scval::as_optional_address(scval::field(&value, "arbiter")?)?,
            token: scval::as_address_string(scval::field(&value, "token")?)?,
            amount: scval::as_i128(scval::field(&value, "amount")?)?,
            task_description: scval::as_utf8(scval::field(&value, "task_description")?)?,
            result,
            deadline_ledger: scval::as_u32(scval::field(&value, "deadline_ledger")?)?,
            status,
            created_at: scval::as_u32(scval::field(&value, "created_at")?)?,
        })
    }

    // ── Rate limits ──────────────────────────────────────────────────────────

    /// Configure on-chain rate limits for this agent.
    pub async fn set_rate_limits(&self, config: &RateLimitConfig) -> Result<TxResult> {
        let (_, tx) = self
            .invoke(
                ContractKey::RateLimiter,
                "set_limits",
                vec![
                    scval::address(self.address())?,
                    scval::address(self.address())?,
                    scval::amount(&config.max_per_tx)?,
                    scval::amount(&config.max_per_hour)?,
                    scval::amount(&config.max_per_day)?,
                    scval::u32_value(config.max_txs_per_hour),
                ],
                false,
            )
            .await?;
        Ok(tx)
    }

    /// Whether a payment would pass the on-chain rate limits (read-only).
    ///
    /// This is a simulation, so it costs nothing and needs no signature — but
    /// it is still a network round trip. For a prediction computed from state
    /// you already hold, use [`crate::math::predict_payment_outcome`].
    pub async fn check_rate_limit(&self, amount: &str) -> Result<bool> {
        let (value, _) = self
            .invoke(
                ContractKey::RateLimiter,
                "check",
                vec![scval::address(self.address())?, scval::amount(amount)?],
                true,
            )
            .await?;
        scval::as_bool(&value)
    }

    /// Current rate-limit usage alongside the configured limits.
    ///
    /// `get_limits` is keyed by an arbitrary agent address, not necessarily
    /// this agent's own, so an owner monitoring several agents can query any
    /// of them read-only through one signed-in [`StellarAgent`]. Defaults to
    /// this agent's own address.
    ///
    /// An agent that was never configured reports
    /// [`RateLimitStatus::unconfigured`] rather than erroring: `get_limits`
    /// panics on-chain in that state, and that panic is the *only* way to
    /// derive `configured: false`, since `is_active` returns `true` both for
    /// an unconfigured agent and for a configured live one.
    pub async fn rate_limit_status(&self, agent_address: Option<&str>) -> Result<RateLimitStatus> {
        let address = agent_address.unwrap_or(self.address());
        let invocation = self
            .invoke(
                ContractKey::RateLimiter,
                "get_limits",
                vec![scval::address(address)?],
                true,
            )
            .await;

        let value = match invocation {
            Ok((value, _)) => value,
            Err(error) if error.code() == ErrorCode::RateLimitNotFound => {
                return Ok(RateLimitStatus::unconfigured())
            }
            Err(error) => return Err(error),
        };

        let stroops = |name: &str| -> Result<String> {
            Ok(math::from_stroops(
                &BigInt::from(scval::as_i128(scval::field(&value, name)?)?),
                7,
            )?)
        };

        Ok(RateLimitStatus {
            configured: true,
            active: scval::as_bool(scval::field(&value, "active")?)?,
            max_per_tx: stroops("max_per_tx")?,
            max_per_hour: stroops("max_per_hour")?,
            max_per_day: stroops("max_per_day")?,
            max_txs_per_hour: scval::as_u32(scval::field(&value, "max_txs_per_hour")?)?,
            spent_this_hour: stroops("hourly_spend")?,
            spent_today: stroops("daily_spend")?,
            txs_this_hour: scval::as_u32(scval::field(&value, "hourly_tx_count")?)?,
            hour_window_start_ledger: scval::as_u32(scval::field(&value, "hour_window_start")?)?,
            day_window_start_ledger: scval::as_u32(scval::field(&value, "day_window_start")?)?,
        })
    }

    // ── Queries ──────────────────────────────────────────────────────────────

    /// The agent's native XLM balance, or `"0"` for an account that does not
    /// exist yet.
    ///
    /// An unfunded account is a normal state for a freshly generated agent,
    /// not an error worth propagating.
    pub async fn balance(&self) -> Result<String> {
        match self.rpc.account(self.address()).await {
            Ok(account) => Ok(math::from_stroops(&BigInt::from(account.balance), 7)?),
            Err(error) if error.code() == ErrorCode::InvalidArgument => Ok("0".to_string()),
            Err(error) => Err(error),
        }
    }

    /// Read a payment channel.
    pub async fn channel(&self, channel_id: u64) -> Result<ChannelInfo> {
        let (value, _) = self
            .invoke(
                ContractKey::PaymentChannel,
                "get_channel",
                vec![scval::u64_value(channel_id)],
                true,
            )
            .await?;

        let period_raw = scval::as_enum_variant(scval::field(&value, "period")?)?;
        let period = SpendPeriod::from_contract_variant(&period_raw).ok_or_else(|| {
            StellarAgentError::new(
                ErrorCode::ContractError,
                format!("Unknown spend period: {period_raw}"),
            )
        })?;

        Ok(ChannelInfo {
            id: channel_id,
            agent: scval::as_address_string(scval::field(&value, "agent")?)?,
            owner: scval::as_address_string(scval::field(&value, "owner")?)?,
            token: scval::as_address_string(scval::field(&value, "token")?)?,
            limit_per_period: scval::as_i128(scval::field(&value, "limit_per_period")?)?,
            period,
            spent_this_period: scval::as_i128(scval::field(&value, "spent_this_period")?)?,
            period_start_ledger: scval::as_u32(scval::field(&value, "period_start_ledger")?)?,
            total_spent: scval::as_i128(scval::field(&value, "total_spent")?)?,
            active: scval::as_bool(scval::field(&value, "active")?)?,
        })
    }

    /// Spend accounting for the active channel's current period.
    pub async fn spend_report(&self) -> Result<SpendReport> {
        let channel_id = self.require_channel(None)?;
        let channel = self.channel(channel_id).await?;
        let (remaining, _) = self
            .invoke(
                ContractKey::PaymentChannel,
                "remaining_this_period",
                vec![scval::u64_value(channel_id)],
                true,
            )
            .await?;

        Ok(SpendReport {
            spent_this_period: math::from_stroops(&BigInt::from(channel.spent_this_period), 7)?,
            remaining_this_period: math::from_stroops(
                &BigInt::from(scval::as_i128(&remaining)?),
                7,
            )?,
            total_lifetime: math::from_stroops(&BigInt::from(channel.total_spent), 7)?,
        })
    }

    /// Derive the current ledger sequence and an *estimated* average ledger
    /// close time from recently observed ledgers via Horizon.
    ///
    /// Ledgers close roughly every 5 seconds, but that figure drifts with
    /// network conditions rather than being contractually fixed — so this
    /// measures it from real recent closes instead of assuming a constant.
    /// Used to turn a rate-limit or channel ledger-count window into a human
    /// wall-clock estimate. See [`crate::math::ledger_time`] for the
    /// derivation and its caveats.
    pub async fn ledger_close_estimate(&self) -> Result<LedgerCloseEstimate> {
        let base = self.network_config.horizon_url.trim_end_matches('/');
        let url = format!("{base}/ledgers?order=desc&limit=20");

        let response = self.http.get(&url).send().await.map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("could not reach Horizon at {url}"),
            )
            .with_source(error)
        })?;

        if !response.status().is_success() {
            return Err(StellarAgentError::new(
                ErrorCode::NetworkError,
                format!("Horizon request failed with status {}", response.status()),
            ));
        }

        let page: serde_json::Value = response.json().await.map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NetworkError,
                "Horizon returned a body that is not JSON",
            )
            .with_source(error)
        })?;

        let samples: Vec<LedgerCloseSample> = page
            .get("_embedded")
            .and_then(|e| e.get("records"))
            .and_then(|r| r.as_array())
            .map(|records| {
                records
                    .iter()
                    .filter_map(|record| {
                        Some(LedgerCloseSample {
                            sequence: u32::try_from(record.get("sequence")?.as_u64()?).ok()?,
                            closed_at: record.get("closed_at")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        math::ledger_close_estimate(&samples).ok_or_else(|| {
            StellarAgentError::new(ErrorCode::NetworkError, "Horizon returned no ledgers")
        })
    }

    // ── Internals ────────────────────────────────────────────────────────────

    /// Build, simulate, and — for a mutation — sign, submit and poll one
    /// contract call.
    ///
    /// Returns the invocation's return value and, for a mutation, the
    /// resulting [`TxResult`]. A read-only call returns a placeholder
    /// `TxResult` with an empty hash: nothing was submitted, so there is no
    /// transaction to name.
    async fn invoke(
        &self,
        contract: ContractKey,
        method: &str,
        args: Vec<ScVal>,
        read_only: bool,
    ) -> Result<(ScVal, TxResult)> {
        let contract_id = self.contracts.get(contract);
        let operation = self.invoke_operation(contract_id, method, args, Vec::new())?;
        let account = self.rpc.account(self.address()).await?;

        let transaction = self.build_transaction(&account, operation.clone(), BASE_FEE, None)?;
        let envelope = envelope_of(transaction.clone());
        let simulation = self
            .rpc
            .simulate_transaction(&encode_envelope(&envelope)?)
            .await
            .map_err(|error| prefix_method(error, method))?;

        let result = simulation.result.clone();
        let return_value = result
            .as_ref()
            .map(|r| r.return_value.clone())
            .unwrap_or(ScVal::Void);

        if read_only {
            return Ok((
                return_value,
                TxResult {
                    hash: String::new(),
                    success: true,
                    ledger: None,
                },
            ));
        }

        // Sign the authorisation entries the host asked for. `SourceAccount`
        // entries are covered by the envelope signature and pass through
        // unchanged.
        let valid_until = simulation
            .latest_ledger
            .saturating_add(AUTH_VALIDITY_LEDGERS);
        let mut signed_auth = Vec::new();
        for entry in result.map(|r| r.auth).unwrap_or_default() {
            signed_auth.push(self.sign_auth_entry(entry, valid_until).await?);
        }

        // Reassemble with the signed auth, the simulated footprint, and a fee
        // that covers the resource cost. Omitting the footprint is the classic
        // route to a `txSorobanInvalid` from the network.
        let authorized =
            self.invoke_operation(contract_id, method, extract_args(&operation)?, signed_auth)?;
        let fee =
            u32::try_from(i64::from(BASE_FEE) + simulation.min_resource_fee).unwrap_or(u32::MAX);
        let assembled =
            self.build_transaction(&account, authorized, fee, Some(simulation.transaction_data))?;

        let signed_xdr = self
            .signer
            .sign_transaction(
                &encode_envelope(&envelope_of(assembled))?,
                &SignTransactionOptions {
                    network_passphrase: self.network_config.network_passphrase.clone(),
                },
            )
            .await?;

        let submitted = self.rpc.send_transaction(&signed_xdr).await?;
        if !submitted.status.is_accepted() {
            let detail = submitted
                .error_result_xdr
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(StellarAgentError::from_contract_message(
                ErrorCode::SubmissionFailed,
                format!(
                    "{method} submission failed ({:?}): {detail}",
                    submitted.status
                ),
            )
            .with_transaction_hash(&submitted.hash));
        }

        let confirmed = self
            .rpc
            .poll_transaction(&submitted.hash, POLL_ATTEMPTS, POLL_INTERVAL)
            .await
            .map_err(|error| prefix_method(error, method))?;

        match confirmed.status {
            TransactionStatus::Success => Ok((
                confirmed.return_value.unwrap_or(return_value),
                TxResult {
                    hash: submitted.hash,
                    success: true,
                    ledger: confirmed.ledger,
                },
            )),
            _ => Err(StellarAgentError::from_contract_message(
                ErrorCode::TransactionFailed,
                format!(
                    "{method} transaction failed{}",
                    confirmed
                        .result_xdr
                        .map(|xdr| format!(": {xdr}"))
                        .unwrap_or_default()
                ),
            )
            .with_transaction_hash(&submitted.hash)),
        }
    }

    fn invoke_operation(
        &self,
        contract_id: &str,
        method: &str,
        args: Vec<ScVal>,
        auth: Vec<SorobanAuthorizationEntry>,
    ) -> Result<Operation> {
        let contract_address: ScAddress = contract_id.parse().map_err(|_| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                format!("Contract address is not a valid contract ID: {contract_id}"),
            )
        })?;

        let function_name = ScSymbol(method.try_into().map_err(|_| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                format!("Contract method name \"{method}\" is longer than 32 characters"),
            )
        })?);

        Ok(Operation {
            source_account: None,
            body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
                host_function: HostFunction::InvokeContract(InvokeContractArgs {
                    contract_address,
                    function_name,
                    args: args.try_into().map_err(|_| {
                        StellarAgentError::new(
                            ErrorCode::InvalidArgument,
                            format!("too many arguments for {method}"),
                        )
                    })?,
                }),
                auth: auth.try_into().map_err(|_| {
                    StellarAgentError::new(
                        ErrorCode::InvalidArgument,
                        "too many authorization entries for one operation",
                    )
                })?,
            }),
        })
    }

    fn build_transaction(
        &self,
        account: &crate::rpc::AccountState,
        operation: Operation,
        fee: u32,
        soroban_data: Option<stellar_xdr::curr::SorobanTransactionData>,
    ) -> Result<Transaction> {
        // A transaction consumes the account's *next* sequence number.
        let seq_num = SequenceNumber(account.sequence.saturating_add(1));

        let max_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() + TRANSACTION_TIMEOUT_SECONDS)
            .unwrap_or(0);

        Ok(Transaction {
            source_account: self.address().parse().map_err(|_| {
                StellarAgentError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "Agent address is not a valid Stellar account: {}",
                        self.address()
                    ),
                )
            })?,
            fee,
            seq_num,
            // Time bounds, not `Preconditions::None`: an envelope with no
            // upper bound stays submittable forever, so a signed transaction
            // that never made it into a ledger could be replayed much later.
            cond: Preconditions::Time(TimeBounds {
                min_time: TimePoint(0),
                max_time: TimePoint(max_time),
            }),
            memo: Memo::None,
            operations: vec![operation].try_into().map_err(|_| {
                StellarAgentError::new(
                    ErrorCode::InvalidArgument,
                    "could not build the operation list",
                )
            })?,
            ext: match soroban_data {
                Some(data) => TransactionExt::V1(data),
                None => TransactionExt::V0,
            },
        })
    }

    async fn sign_auth_entry(
        &self,
        entry: SorobanAuthorizationEntry,
        valid_until_ledger_seq: u32,
    ) -> Result<SorobanAuthorizationEntry> {
        if matches!(entry.credentials, SorobanCredentials::SourceAccount) {
            return Ok(entry);
        }

        let encoded = entry.to_xdr_base64(Limits::none()).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                "could not encode an authorization entry for signing",
            )
            .with_source(error)
        })?;

        let signed = self
            .signer
            .sign_auth_entry(
                &encoded,
                &SignAuthEntryOptions {
                    network_passphrase: self.network_config.network_passphrase.clone(),
                    valid_until_ledger_seq,
                },
            )
            .await?;

        SorobanAuthorizationEntry::from_xdr_base64(&signed, Limits::none()).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::NotAuthorized,
                "the signer returned an authorization entry that is not valid XDR",
            )
            .with_source(error)
        })
    }

    async fn escrow_action(
        &self,
        method: &str,
        job_id: u64,
        extra: Option<ScVal>,
    ) -> Result<TxResult> {
        let mut args = vec![scval::address(self.address())?, scval::u64_value(job_id)];
        if let Some(extra) = extra {
            args.push(extra);
        }
        let (_, tx) = self
            .invoke(ContractKey::Escrow, method, args, false)
            .await?;
        Ok(tx)
    }

    fn require_channel(&self, channel_id: Option<u64>) -> Result<u64> {
        channel_id
            .or_else(|| self.active_channel_id())
            .ok_or_else(|| {
                StellarAgentError::new(
                    ErrorCode::NoActiveChannel,
                    "No active payment channel. Call open_channel() first.",
                )
            })
    }

    /// Map an asset code to a token contract ID.
    ///
    /// `XLM` resolves to the native asset's Stellar Asset Contract, whose ID
    /// is *derived* from the network passphrase rather than looked up — so it
    /// is correct on any network, including a standalone one, without
    /// configuration.
    fn resolve_asset_contract(&self, asset: &str) -> Result<String> {
        if asset == "XLM" {
            return self.native_asset_contract_id();
        }
        let resolved = self
            .asset_contracts
            .get(asset)
            .map(String::as_str)
            .unwrap_or(asset);

        if stellar_strkey::Contract::from_string(resolved).is_ok() {
            return Ok(resolved.to_string());
        }
        Err(StellarAgentError::new(
            ErrorCode::InvalidArgument,
            format!(
                "Unknown asset \"{asset}\". Pass its C… token contract ID, or register it with \
                 StellarAgent::builder().asset_contract(\"{asset}\", \"C…\")."
            ),
        ))
    }

    /// The native asset's Stellar Asset Contract ID for this network.
    fn native_asset_contract_id(&self) -> Result<String> {
        let network_id: [u8; 32] =
            Sha256::digest(self.network_config.network_passphrase.as_bytes()).into();

        let preimage = HashIdPreimage::ContractId(HashIdPreimageContractId {
            network_id: stellar_xdr::curr::Hash(network_id),
            contract_id_preimage: ContractIdPreimage::Asset(Asset::Native),
        });
        let encoded = preimage.to_xdr(Limits::none()).map_err(|error| {
            StellarAgentError::new(
                ErrorCode::InvalidArgument,
                "could not derive the native asset contract ID",
            )
            .with_source(error)
        })?;
        let id: [u8; 32] = Sha256::digest(encoded).into();
        Ok(stellar_strkey::Contract(id).to_string())
    }

    /// Best-effort testnet funding. An already-funded account is not an error,
    /// and neither is an unreachable friendbot — the caller finds out either
    /// way on their first transaction.
    async fn fund_from_friendbot(&self) {
        let _ = self
            .http
            .get(format!("{FRIENDBOT_URL}?addr={}", self.address()))
            .send()
            .await;
    }
}

fn envelope_of(transaction: Transaction) -> TransactionEnvelope {
    TransactionEnvelope::Tx(TransactionV1Envelope {
        tx: transaction,
        signatures: Default::default(),
    })
}

fn encode_envelope(envelope: &TransactionEnvelope) -> Result<String> {
    envelope.to_xdr_base64(Limits::none()).map_err(|error| {
        StellarAgentError::new(
            ErrorCode::InvalidArgument,
            "could not encode the transaction envelope",
        )
        .with_source(error)
    })
}

/// Recover the invocation arguments from an operation, so the authorised
/// rebuild carries exactly the arguments that were simulated.
fn extract_args(operation: &Operation) -> Result<Vec<ScVal>> {
    match &operation.body {
        OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function: HostFunction::InvokeContract(args),
            ..
        }) => Ok(args.args.to_vec()),
        _ => Err(StellarAgentError::new(
            ErrorCode::InvalidArgument,
            "expected a contract invocation operation",
        )),
    }
}

/// Prefix an error's message with the method that produced it.
///
/// The RPC layer does not know which contract call it is serving, and
/// "simulation failed" without a method name is the kind of error that costs
/// an hour.
fn prefix_method(error: StellarAgentError, method: &str) -> StellarAgentError {
    let mut prefixed =
        StellarAgentError::new(error.code(), format!("{method}: {}", error.message()));
    if let Some(hash) = error.transaction_hash() {
        prefixed = prefixed.with_transaction_hash(hash);
    }
    prefixed
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "SAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYPSBF5K";
    const CONTRACT: &str = "CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE";

    async fn agent() -> StellarAgent {
        StellarAgent::builder()
            .network(Network::Local)
            .secret_key(SECRET)
            .allow_unconfigured_contracts(true)
            .build()
            .await
            .expect("a local agent needs no network to build")
    }

    #[tokio::test]
    async fn a_signer_and_a_secret_key_together_are_refused() {
        let error = StellarAgent::builder()
            .network(Network::Local)
            .secret_key(SECRET)
            .signer(Arc::new(KeypairSigner::from_secret(SECRET).unwrap()))
            .allow_unconfigured_contracts(true)
            .build()
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("not both"), "{}", error.message());
    }

    #[tokio::test]
    async fn undeployed_contracts_fail_at_build_time_by_default() {
        let error = StellarAgent::builder()
            .network(Network::Testnet)
            .secret_key(SECRET)
            .build()
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(
            error.message().contains("not deployed"),
            "{}",
            error.message()
        );
    }

    #[tokio::test]
    async fn the_address_comes_from_the_signer() {
        let agent = agent().await;
        let expected = KeypairSigner::from_secret(SECRET).unwrap().address();
        assert_eq!(agent.address(), expected);
    }

    #[tokio::test]
    async fn debug_output_names_the_address_but_not_the_signer() {
        let agent = agent().await;
        let rendered = format!("{agent:?}");
        assert!(rendered.contains(agent.address()));
        assert!(!rendered.contains(SECRET));
    }

    #[tokio::test]
    async fn operations_needing_a_channel_fail_before_any_network_call() {
        let agent = agent().await;
        let error = agent.close_channel(None).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::NoActiveChannel);

        let error = agent.spend_report().await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::NoActiveChannel);
    }

    #[tokio::test]
    async fn the_active_channel_can_be_set_and_cleared() {
        let agent = agent().await;
        assert_eq!(agent.active_channel_id(), None);
        agent.set_active_channel_id(Some(7));
        assert_eq!(agent.active_channel_id(), Some(7));
        agent.set_active_channel_id(None);
        assert_eq!(agent.active_channel_id(), None);
    }

    #[tokio::test]
    async fn a_conversion_without_a_slippage_floor_is_refused() {
        let agent = agent().await;
        agent.set_active_channel_id(Some(1));

        let error = agent
            .pay_for_api(&PayForApiParams {
                endpoint: "https://api.example.com".into(),
                amount: "1".into(),
                asset: None,
                channel_id: None,
                recipient: None,
                dest_asset: Some("XLM".into()),
                min_received: None,
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("must be set together"));
    }

    #[tokio::test]
    async fn a_slippage_floor_without_a_conversion_is_refused_too() {
        let agent = agent().await;
        agent.set_active_channel_id(Some(1));

        let error = agent
            .pay_for_api(&PayForApiParams {
                endpoint: "https://api.example.com".into(),
                amount: "1".into(),
                asset: None,
                channel_id: None,
                recipient: None,
                dest_asset: None,
                min_received: Some("0.9".into()),
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn a_zero_deadline_is_refused_before_the_ledger_lookup() {
        let agent = agent().await;
        let error = agent
            .request_work(&RequestWorkParams {
                worker_agent: "GWORKER".into(),
                task: "summarise".into(),
                escrow_amount: "1".into(),
                asset: None,
                deadline_ledgers: Some(0),
                arbiter: None,
            })
            .await
            .unwrap_err();

        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("positive"));
    }

    #[tokio::test]
    async fn asset_resolution_prefers_a_registered_contract_over_a_bare_code() {
        let agent = StellarAgent::builder()
            .network(Network::Local)
            .secret_key(SECRET)
            .asset_contract("USDC", CONTRACT)
            .allow_unconfigured_contracts(true)
            .build()
            .await
            .unwrap();

        assert_eq!(agent.resolve_asset_contract("USDC").unwrap(), CONTRACT);
        // A raw contract ID passes through unchanged.
        assert_eq!(agent.resolve_asset_contract(CONTRACT).unwrap(), CONTRACT);
        // An unregistered code is a configuration error, and says how to fix it.
        let error = agent.resolve_asset_contract("EURC").unwrap_err();
        assert!(
            error.message().contains("asset_contract"),
            "{}",
            error.message()
        );
    }

    #[tokio::test]
    async fn the_native_asset_contract_is_derived_per_network() {
        let local = agent().await;
        let testnet = StellarAgent::builder()
            .network(Network::Testnet)
            .secret_key(SECRET)
            .allow_unconfigured_contracts(true)
            .build()
            .await
            .unwrap();

        let local_native = local.resolve_asset_contract("XLM").unwrap();
        let testnet_native = testnet.resolve_asset_contract("XLM").unwrap();

        assert!(local_native.starts_with('C'));
        assert!(stellar_strkey::Contract::from_string(&local_native).is_ok());
        // Different passphrase, different SAC — deriving it from a hard-coded
        // constant would silently target the wrong network's contract.
        assert_ne!(local_native, testnet_native);
    }

    #[tokio::test]
    async fn the_testnet_native_asset_contract_matches_the_published_id() {
        // A fixed vector: if the derivation is ever rewritten, this catches a
        // change in the preimage rather than a change in taste.
        let testnet = StellarAgent::builder()
            .network(Network::Testnet)
            .secret_key(SECRET)
            .allow_unconfigured_contracts(true)
            .build()
            .await
            .unwrap();

        assert_eq!(
            testnet.resolve_asset_contract("XLM").unwrap(),
            "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC"
        );
    }

    #[tokio::test]
    async fn a_built_transaction_carries_the_next_sequence_number_and_time_bounds() {
        let agent = agent().await;
        let account = crate::rpc::AccountState {
            address: agent.address().to_string(),
            sequence: 41,
            balance: 100_000_000,
        };
        let operation = agent
            .invoke_operation(CONTRACT, "pay", vec![scval::u32_value(1)], Vec::new())
            .unwrap();
        let transaction = agent
            .build_transaction(&account, operation, BASE_FEE, None)
            .unwrap();

        assert_eq!(transaction.seq_num.0, 42);
        assert_eq!(transaction.fee, BASE_FEE);
        match transaction.cond {
            Preconditions::Time(bounds) => {
                assert_eq!(bounds.min_time.0, 0);
                assert!(
                    bounds.max_time.0 > 0,
                    "an unbounded envelope stays replayable"
                );
            }
            other => panic!("expected time bounds, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_over_long_method_name_is_rejected_with_the_name() {
        let agent = agent().await;
        let error = agent
            .invoke_operation(CONTRACT, &"m".repeat(33), Vec::new(), Vec::new())
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("32"));
    }

    #[tokio::test]
    async fn arguments_survive_the_rebuild_that_attaches_signed_auth() {
        let agent = agent().await;
        let args = vec![scval::u64_value(9), scval::bool_value(true)];
        let operation = agent
            .invoke_operation(CONTRACT, "pay", args.clone(), Vec::new())
            .unwrap();
        assert_eq!(extract_args(&operation).unwrap(), args);
    }

    #[test]
    fn method_prefixing_preserves_the_code_and_the_transaction_hash() {
        let error = StellarAgentError::new(ErrorCode::TransactionFailed, "reverted")
            .with_transaction_hash("deadbeef");
        let prefixed = prefix_method(error, "pay");
        assert_eq!(prefixed.code(), ErrorCode::TransactionFailed);
        assert_eq!(prefixed.message(), "pay: reverted");
        assert_eq!(prefixed.transaction_hash(), Some("deadbeef"));
    }
}
