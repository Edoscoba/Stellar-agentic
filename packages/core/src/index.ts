// Key handling lives behind the Signer abstraction in ./signer.ts.
import {
  Address,
  Contract,
  TransactionBuilder,
  BASE_FEE,
  Operation,
  Asset,
  Horizon,
  SorobanRpc,
  nativeToScVal,
  scValToNative,
  xdr,
} from '@stellar/stellar-sdk';
import { fromStroops, toStroops } from './math/index.js';

// ─── Deterministic math (re-exported for consumers) ──────────────────────────
export * as math from './math/index.js';
export {
  // fixed-point primitives
  bn,
  add,
  sub,
  mul,
  div,
  pct,
  clamp,
  sumStrings,
  toStroops,
  fromStroops,
  fmt,
  toStr,
  gt, gte, lt, lte, eq,
  isZero,
  isPositive,
  STROOP_SCALE,
  BPS_SCALE,
  BigNumber,
  // bidding algorithm
  scoreBid,
  rankBids,
  selectBestBid,
  isWithinSpendLimit,
  remainingBudget,
  DEFAULT_BID_WEIGHTS,
  // bid attestation
  attestRankBids,
  verifyBidAttestation,
} from './math/index.js';
export type {
  AgentBid,
  BidWeights,
  ScoredBid,
} from './math/bid.js';
export type {
  BidAttestation,
  AttestRankBidsOptions,
  AttestedRanking,
  ScorerKeyRecord,
  ScorerKeyDirectory,
  VerifyBidAttestationOptions,
  BidAttestationVerification,
} from './math/attestation.js';

import type {
  StellarAgentConfig,
  Network,
  NetworkConfig,
  OpenChannelParams,
  PayForAPIParams,
  RequestWorkParams,
  RateLimitConfig,
  RateLimitStatus,
  AgentInfo,
  ChannelInfo,
  JobInfo,
  SpendReport,
  TxResult,
  ContractAddresses,
} from './types/index.js';

import { NETWORK_CONFIGS } from './types/index.js';
import { StellarAgentError } from './errors.js';
import type { StellarAgentErrorCode } from './errors.js';

export { StellarAgentError } from './errors.js';
export type { StellarAgentErrorCode } from './errors.js';

// Public type surface — previously only imported internally, never
// re-exported, so consumers (e.g. @stellaragent/react) had no way to
// import these from the package root at all.
export type {
  Network,
  NetworkConfig,
  SpendPeriod,
  SpendLimit,
  StellarAgentConfig,
  AgentInfo,
  OpenChannelParams,
  PayForAPIParams,
  ChannelInfo,
  SpendReport,
  JobStatus,
  RequestWorkParams,
  JobInfo,
  RateLimitConfig,
  RateLimitStatus,
  ContractAddresses,
  AgentEvent,
  TxResult,
} from './types/index.js';

// ─── Circuit Breaker (emergency pause) ────────────────────────────────────────
export {
  CircuitBreaker,
  asPublicAddress,
} from './circuitBreaker.js';
export type { CircuitBreakerOptions, PublicAddress } from './circuitBreaker.js';

// ─── Contract address resolution ─────────────────────────────────────────────
//
// Addresses used to be hard-coded here as obviously-fake placeholders. They
// now resolve from explicit config or environment variables, and an
// unconfigured network fails fast at create() time. See ./contracts.ts.

export {
  resolveContracts,
  assertDeployed,
  isDeployedAddress,
  envVarNames,
  ContractsNotDeployedError,
  UNCONFIGURED_CONTRACTS,
  CONTRACT_KEYS,
} from './contracts.js';
export type { ContractKey } from './contracts.js';

import { resolveContracts, assertDeployed } from './contracts.js';

// ─── Signing ─────────────────────────────────────────────────────────────────
//
// The agent signs through a Signer rather than an in-memory Keypair, so key
// material need never live in this process. See ./signer.ts.

export {
  KeypairSigner,
  RemoteSigner,
  SignerAdapter,
  SigningError,
  isSigner,
} from './signer.js';
export type {
  Signer,
  SignTransactionOptions,
  SignAuthEntryOptions,
  RemoteSignerOptions,
  Sep43Like,
} from './signer.js';

import { KeypairSigner, SigningError } from './signer.js';
import type { Signer } from './signer.js';

/**
 * Whether a URL points at the local machine, and may therefore be spoken to
 * over plaintext HTTP. Anything else — including a LAN address — must use
 * TLS, so a misconfigured `horizonUrl` fails loudly instead of silently
 * transmitting signed transactions in the clear.
 */
function isLoopbackUrl(url: string): boolean {
  try {
    const { protocol, hostname } = new URL(url);
    if (protocol === 'https:') return false;
    return hostname === 'localhost' || hostname === '127.0.0.1' || hostname === '[::1]';
  } catch {
    return false;
  }
}

// ─── StellarAgent ─────────────────────────────────────────────────────────────

/**
 * Main SDK class for AI Agent payment operations on Stellar.
 *
 * @example
 * ```typescript
 * const agent = await StellarAgent.create({
 *   network: 'testnet',
 *   spendLimit: { amount: '10', asset: 'USDC', period: 'hourly' },
 * });
 *
 * await agent.payForAPI({
 *   endpoint: 'https://api.example.com/inference',
 *   amount: '0.001',
 *   asset: 'USDC',
 * });
 * ```
 */
export class StellarAgent {
  /**
   * Where signing happens. For a {@link RemoteSigner} this holds a URL and a
   * token — no key material — which is the entire point of the abstraction.
   */
  private signer: Signer;
  /**
   * Resolved once at `create()` time. Cached so `address` stays a synchronous
   * getter even when the signer derives it over the network.
   */
  private publicKey: string;
  private networkConfig: NetworkConfig;
  private contracts: ContractAddresses;
  private assetContracts: Record<string, string>;
  private horizon: Horizon.Server;
  private rpc: SorobanRpc.Server;
  private activeChannelId?: bigint;

  private constructor(
    signer: Signer,
    publicKey: string,
    networkConfig: NetworkConfig,
    contracts: ContractAddresses,
    assetContracts: Record<string, string>,
  ) {
    this.signer = signer;
    this.publicKey = publicKey;
    this.networkConfig = networkConfig;
    this.contracts = contracts;
    this.assetContracts = assetContracts;
    // `Horizon.Server` refuses plain-HTTP endpoints unless `allowHttp` is set,
    // which made the `local` network config (http://localhost:8000) throw
    // "Cannot connect to insecure horizon server" from the constructor. Allow
    // HTTP for loopback only — a plaintext connection to a real network would
    // expose submitted transactions, so this must not be blanket-enabled.
    this.horizon = new Horizon.Server(networkConfig.horizonUrl, {
      allowHttp: isLoopbackUrl(networkConfig.horizonUrl),
    });
    this.rpc = new SorobanRpc.Server(networkConfig.rpcUrl, {
      allowHttp: isLoopbackUrl(networkConfig.rpcUrl),
    });
  }

  // ── Factory Methods ──────────────────────────────────────────────────────

  /**
   * Create a new StellarAgent instance.
   *
   * Supply exactly one of:
   * - `signer` — any {@link Signer}. The secret never enters this process.
   * - `secretKey` — an in-memory keypair, wrapped in a {@link KeypairSigner}.
   * - neither — a fresh random keypair is generated.
   *
   * Contract addresses resolve from `config.contracts`, then from
   * `STELLARAGENT_*` environment variables, then from the per-network
   * unconfigured sentinels. If the result is not a set of real deployed
   * contract IDs this throws {@link ContractsNotDeployedError} immediately,
   * rather than letting an opaque RPC error surface later from the middle of
   * a payment. Pass `allowUnconfiguredContracts: true` to skip that check
   * when you only need read-only, contract-free calls such as
   * {@link StellarAgent.getBalance}.
   *
   * @example Remote signer — no key material in this process
   * ```typescript
   * const agent = await StellarAgent.create({
   *   network: 'testnet',
   *   signer: new RemoteSigner({ url: 'https://signer.internal', token: TOKEN }),
   * });
   * ```
   *
   * @throws {ContractsNotDeployedError} when contracts are not deployed
   */
  static async create(config: StellarAgentConfig): Promise<StellarAgent> {
    if (config.signer && config.secretKey) {
      throw new Error(
        'StellarAgent.create: pass either `signer` or `secretKey`, not both. ' +
          'Supplying a secret alongside a remote signer would defeat the point of the signer.',
      );
    }

    const generatedKeypair = !config.signer && !config.secretKey;
    const signer: Signer = config.signer
      ? config.signer
      : config.secretKey
        ? KeypairSigner.fromSecret(config.secretKey)
        : KeypairSigner.random();

    // Derived through the Signer interface, so a remote signer reports its
    // address without the secret ever being loaded here.
    const publicKey = await signer.getPublicKey();

    const networkConfig = NETWORK_CONFIGS[config.network];
    const contracts = resolveContracts(config.network, config.contracts);

    if (!config.allowUnconfiguredContracts) {
      assertDeployed(config.network, contracts);
    }

    const agent = new StellarAgent(
      signer,
      publicKey,
      networkConfig,
      contracts,
      config.assetContracts ?? {},
    );

    // Only a freshly generated keypair gets friendbot funding — a supplied
    // secret or an external signer is assumed to already have an account.
    if (config.network === 'testnet' && generatedKeypair) {
      await agent.fundFromFriendbot();
    }

    return agent;
  }

  /**
   * Restore an agent from an existing secret key.
   *
   * `options` forwards the rest of {@link StellarAgentConfig} — notably
   * `contracts` and `allowUnconfiguredContracts`, without which a restored
   * agent could only ever target contracts resolved from the environment.
   */
  static async fromSecret(
    secretKey: string,
    network: Network = 'testnet',
    options: Omit<StellarAgentConfig, 'network' | 'secretKey'> = {},
  ): Promise<StellarAgent> {
    return StellarAgent.create({ ...options, network, secretKey });
  }

  // ── Identity ─────────────────────────────────────────────────────────────

  /**
   * The agent's Stellar public address.
   *
   * Resolved through the {@link Signer} at `create()` time, so this works
   * identically for a remote signer that never exposes its secret.
   */
  get address(): string {
    return this.publicKey;
  }

  /**
   * The agent's secret key.
   *
   * Only available when the agent was built from an in-memory keypair. With
   * any other {@link Signer} there is no secret in this process to return —
   * which is the point — so this throws rather than returning something
   * misleading.
   *
   * @deprecated Reading key material off a live agent is the pattern the
   * {@link Signer} abstraction exists to remove. Hold the secret yourself if
   * you need it, or use a {@link RemoteSigner} and stop having one.
   *
   * @throws {SigningError} when signing is not backed by a local keypair
   */
  get secretKey(): string {
    if (!(this.signer instanceof KeypairSigner)) {
      throw new SigningError(
        'This agent has no secret key to expose — it signs via ' +
          `${this.signer.constructor.name}, which holds the key elsewhere. ` +
          'That is the intended behaviour for a remote signer.',
      );
    }
    return this.signer.exportSecret();
  }

  /**
   * Whether this agent holds key material in-process.
   *
   * `false` for a remote or hardware signer. Useful for asserting a
   * production deployment is not running with an in-memory secret.
   */
  get holdsSecretKey(): boolean {
    return this.signer instanceof KeypairSigner;
  }

  /** Register this wallet in the configured AgentWalletFactory contract. */
  async createAgentWallet(name = 'StellarAgent'): Promise<bigint> {
    const result = await this.invokeContract(this.contracts.agentWalletFactory, 'create_agent', [
      this.addressVal(this.address),
      this.addressVal(this.address),
      xdr.ScVal.scvString(name),
    ]);
    return this.asBigInt(result.value);
  }

  /** Read and decode an agent registered in AgentWalletFactory. */
  async getAgent(agentId: bigint): Promise<AgentInfo> {
    const value = this.asRecord((await this.invokeContract(
      this.contracts.agentWalletFactory,
      'get_agent',
      [this.u64(agentId)],
      true,
    )).value);
    return {
      id: agentId,
      address: this.asString(value.address),
      name: this.asString(value.name),
      owner: this.asString(value.owner),
      active: value.active === true,
      createdAt: this.asNumber(value.created_at),
      totalOps: this.asBigInt(value.total_ops),
    };
  }

  // ── Payment Channel ──────────────────────────────────────────────────────

  /**
   * Open a payment channel for this agent.
   * Deposits tokens and sets a per-period spend limit.
   *
   * @returns The channel ID
   */
  async openChannel(params: OpenChannelParams): Promise<bigint> {
    const result = await this.invokeContract(this.contracts.paymentChannel, 'open_channel', [
      this.addressVal(this.address),
      this.addressVal(this.address),
      this.addressVal(this.resolveAssetContract(params.token ?? 'XLM')),
      this.i128(params.deposit),
      this.i128(params.limitPerPeriod),
      this.enumVal(this.spendPeriodVariant(params.period)),
    ]);
    const channelId = this.asBigInt(result.value);
    this.activeChannelId = channelId;
    return channelId;
  }

  /** Close a payment channel and return its remaining token balance. */
  async closeChannel(channelId = this.activeChannelId): Promise<TxResult> {
    if (channelId === undefined) {
      throw new StellarAgentError(
        'NO_ACTIVE_CHANNEL',
        'No active payment channel. Call openChannel() first.',
      );
    }
    const tx = (await this.invokeContract(this.contracts.paymentChannel, 'close_channel', [
      this.addressVal(this.address),
      this.u64(channelId),
    ])).tx;
    if (this.activeChannelId === channelId) this.activeChannelId = undefined;
    return tx;
  }

  /**
   * Pay for an API call. Deducts from the active payment channel.
   * Respects on-chain spend limits automatically.
   *
   * If `destAsset` differs from the channel's settlement asset, this
   * settles the recipient in `destAsset` instead — e.g. a channel funded
   * in USDC paying a provider that only accepts XLM — by invoking
   * `PaymentChannel.pay_with_conversion` rather than `pay`. The spend
   * limit is still enforced in the channel's settlement asset either way.
   *
   * @example
   * ```typescript
   * await agent.payForAPI({
   *   endpoint: 'https://api.openai.com/v1/chat',
   *   amount: '0.001',
   *   asset: 'USDC',
   * });
   *
   * // Channel funded in USDC, provider only accepts XLM:
   * await agent.payForAPI({
   *   endpoint: 'https://api.example.com/inference',
   *   amount: '0.001',
   *   asset: 'USDC',
   *   destAsset: 'XLM',
   *   minReceived: '0.009', // slippage floor, in XLM
   * });
   * ```
   */
  async payForAPI(params: PayForAPIParams): Promise<TxResult> {
    const channelId = params.channelId ?? this.activeChannelId;
    if (channelId === undefined) {
      throw new StellarAgentError(
        'NO_ACTIVE_CHANNEL',
        'No active payment channel. Call openChannel() first.',
      );
    }

    if ((params.destAsset !== undefined) !== (params.minReceived !== undefined)) {
      throw new StellarAgentError(
        'INVALID_ARGUMENT',
        'destAsset and minReceived must be set together',
      );
    }

    const common = [
      this.addressVal(this.address),
      this.u64(channelId),
      this.addressVal(params.recipient ?? this.address),
      this.i128(params.amount),
    ];
    const args = params.destAsset === undefined
      ? [...common, this.bytesVal(params.endpoint)]
      : [
          ...common,
          this.addressVal(this.resolveAssetContract(params.destAsset)),
          this.i128(params.minReceived!),
          this.bytesVal(params.endpoint),
        ];
    return (await this.invokeContract(
      this.contracts.paymentChannel,
      params.destAsset === undefined ? 'pay' : 'pay_with_conversion',
      args,
    )).tx;
  }

  // ── Agent-to-Agent Escrow ────────────────────────────────────────────────

  /**
   * Create an escrow job delegating work to another agent.
   * Locks payment until the work is delivered and released.
   *
   * @example
   * ```typescript
   * const job = await agent.requestWork({
   *   workerAgent: 'G...WORKER_ADDRESS',
   *   task: 'Summarize this document: ipfs://Qm...',
   *   escrowAmount: '0.05',
   *   asset: 'USDC',
   * });
   * ```
   */
  async requestWork(params: RequestWorkParams): Promise<bigint> {
    const latest = await this.getLatestLedger();
    const deadlineOffset = params.deadlineLedgers ?? 720;
    if (!Number.isInteger(deadlineOffset) || deadlineOffset <= 0) {
      throw new StellarAgentError(
        'INVALID_ARGUMENT',
        'deadlineLedgers must be a positive integer',
      );
    }
    const deadline = latest + deadlineOffset;
    if (deadline > 0xffff_ffff) {
      throw new StellarAgentError('INVALID_ARGUMENT', 'deadline ledger exceeds u32 range');
    }
    const result = await this.invokeContract(this.contracts.escrow, 'create_job', [
      this.addressVal(this.address),
      this.addressVal(this.resolveAssetContract(params.asset ?? 'XLM')),
      this.i128(params.escrowAmount),
      this.bytesVal(params.task),
      this.u32(deadline),
      params.arbiter ? this.addressVal(params.arbiter) : xdr.ScVal.scvVoid(),
    ]);
    return this.asBigInt(result.value);
  }

  /**
   * Accept an open escrow job as a worker agent
   */
  async acceptJob(jobId: bigint): Promise<TxResult> {
    return (await this.invokeContract(this.contracts.escrow, 'accept_job', [
      this.addressVal(this.address),
      this.u64(jobId),
    ])).tx;
  }

  /**
   * Submit work result for an escrow job
   */
  async submitResult(jobId: bigint, result: string): Promise<TxResult> {
    return (await this.invokeContract(this.contracts.escrow, 'submit_result', [
      this.addressVal(this.address),
      this.u64(jobId),
      this.bytesVal(result),
    ])).tx;
  }

  /**
   * Release escrow payment to the worker after work is complete
   */
  async releasePayment(jobId: bigint): Promise<TxResult> {
    return (await this.invokeContract(this.contracts.escrow, 'release', [
      this.addressVal(this.address),
      this.u64(jobId),
    ])).tx;
  }

  // ── Rate Limits ──────────────────────────────────────────────────────────

  /**
   * Configure rate limits for this agent on-chain.
   * Protects against runaway spending.
   */
  async setRateLimits(config: RateLimitConfig): Promise<TxResult> {
    return (await this.invokeContract(this.contracts.rateLimiter, 'set_limits', [
      this.addressVal(this.address),
      this.addressVal(this.address),
      this.i128(config.maxPerTx),
      this.i128(config.maxPerHour),
      this.i128(config.maxPerDay),
      this.u32(config.maxTxsPerHour),
    ])).tx;
  }

  /**
   * Check if a payment would be blocked by rate limits (read-only)
   */
  async checkRateLimit(amount: string): Promise<boolean> {
    const result = await this.invokeContract(this.contracts.rateLimiter, 'check', [
      this.addressVal(this.address),
      this.i128(amount),
    ], true);
    return result.value === true;
  }

  // ── Queries ──────────────────────────────────────────────────────────────

  /**
   * Get current XLM balance
   */
  async getBalance(): Promise<string> {
    try {
      const account = await this.horizon.loadAccount(this.address);
      const xlmBalance = account.balances.find(
        (b) => b.asset_type === 'native',
      );
      return xlmBalance?.balance ?? '0';
    } catch {
      return '0';
    }
  }

  /**
   * Get spend report for the current period
   */
  async getSpendReport(): Promise<SpendReport> {
    if (this.activeChannelId === undefined) {
      throw new StellarAgentError(
        'NO_ACTIVE_CHANNEL',
        'No active payment channel. Call openChannel() first.',
      );
    }
    const channel = await this.getChannel(this.activeChannelId);
    const remaining = await this.invokeContract(
      this.contracts.paymentChannel,
      'remaining_this_period',
      [this.u64(this.activeChannelId)],
      true,
    );
    return {
      spentThisPeriod: fromStroops(channel.spentThisPeriod),
      remainingThisPeriod: fromStroops(this.asBigInt(remaining.value)),
      totalLifetime: fromStroops(channel.totalSpent),
    };
  }

  /**
   * Get info about a payment channel
   */
  async getChannel(channelId: bigint): Promise<ChannelInfo> {
    const value = this.asRecord((await this.invokeContract(
      this.contracts.paymentChannel,
      'get_channel',
      [this.u64(channelId)],
      true,
    )).value);
    return {
      id: channelId,
      agent: this.asString(value.agent),
      owner: this.asString(value.owner),
      token: this.asString(value.token),
      limitPerPeriod: this.asBigInt(value.limit_per_period),
      spentThisPeriod: this.asBigInt(value.spent_this_period),
      totalSpent: this.asBigInt(value.total_spent),
      active: value.active === true,
    };
  }

  /**
   * Get info about a job
   */
  async getJob(jobId: bigint): Promise<JobInfo> {
    const value = this.asRecord((await this.invokeContract(
      this.contracts.escrow,
      'get_job',
      [this.u64(jobId)],
      true,
    )).value);
    return {
      id: jobId,
      requester: this.asString(value.requester),
      worker: this.optionalString(value.worker),
      arbiter: this.optionalString(value.arbiter),
      token: this.asString(value.token),
      amount: this.asBigInt(value.amount),
      taskDescription: this.decodeBytes(value.task_description),
      result: value.result == null ? null : this.decodeBytes(value.result),
      deadlineLedger: this.asNumber(value.deadline_ledger),
      status: this.jobStatus(value.status),
      createdAt: this.asNumber(value.created_at),
    };
  }

  /**
   * Get current rate-limit usage alongside the configured limits.
   *
   * `RateLimiter.get_limits` is keyed by an arbitrary agent address, not
   * necessarily this agent's own — an owner monitoring several agents can
   * query any of them read-only through one signed-in `StellarAgent`.
   * Defaults to {@link StellarAgent.address} (checking this agent's own
   * limits) when omitted.
   */
  async getRateLimitStatus(): Promise<RateLimitStatus> {
    const value = this.asRecord((await this.invokeContract(
      this.contracts.rateLimiter,
      'get_limits',
      [this.addressVal(this.address)],
      true,
    )).value);
    return {
      maxPerTx: fromStroops(this.asBigInt(value.max_per_tx)),
      maxPerHour: fromStroops(this.asBigInt(value.max_per_hour)),
      maxPerDay: fromStroops(this.asBigInt(value.max_per_day)),
      maxTxsPerHour: this.asNumber(value.max_txs_per_hour),
      spentThisHour: fromStroops(this.asBigInt(value.hourly_spend)),
      spentToday: fromStroops(this.asBigInt(value.daily_spend)),
      txsThisHour: this.asNumber(value.hourly_tx_count),
    };
  }

  /**
   * Derive the current ledger sequence and an *estimated* average ledger
   * close time from a handful of recently observed ledgers via Horizon.
   *
   * Ledgers close roughly every 5 seconds, but that figure drifts with
   * network conditions rather than being contractually fixed — so this
   * measures it from real recent closes instead of assuming a constant. Used
   * to convert a `RateLimiter`/`PaymentChannel` ledger-count window (e.g.
   * "720 ledgers until the hourly window resets") into a human wall-clock
   * estimate. See `ledgerTime.ts` for the derivation and its caveats.
   */
  async getLedgerCloseEstimate(): Promise<LedgerCloseEstimate> {
    return fetchLedgerCloseEstimate(this.networkConfig.horizonUrl);
  }

  // ── Internals ────────────────────────────────────────────────────────────

  /**
   * Build and simulate every contract call. Mutations additionally sign the
   * simulated auth entries, assemble the footprint/resources, sign and submit
   * the envelope, and wait for a terminal transaction status.
   */
  private async invokeContract(
    contractId: string,
    method: string,
    args: xdr.ScVal[],
    readOnly = false,
  ): Promise<{ value: unknown; tx: TxResult }> {
    try {
      const account = await this.rpc.getAccount(this.address);
      const operation = new Contract(contractId).call(method, ...args);
      const transaction = new TransactionBuilder(account, {
        fee: BASE_FEE,
        networkPassphrase: this.networkConfig.networkPassphrase,
      })
        .addOperation(operation)
        .setTimeout(30)
        .build();

      const simulation = await this.rpc.simulateTransaction(transaction);
      if (SorobanRpc.Api.isSimulationError(simulation)) {
        throw this.contractError(
          'SIMULATION_FAILED',
          `${method} simulation failed: ${simulation.error}`,
        );
      }
      if (SorobanRpc.Api.isSimulationRestore(simulation)) {
        throw new StellarAgentError(
          'SIMULATION_FAILED',
          `${method} requires restoring expired ledger entries before invocation`,
        );
      }

      if (readOnly) {
        return {
          value: simulation.result?.retval
            ? scValToNative(simulation.result.retval)
            : undefined,
          tx: { hash: '', success: true },
        };
      }

      const validUntilLedgerSeq = simulation.latestLedger + 100;
      const auth = await Promise.all((simulation.result?.auth ?? []).map(async (entry) => {
        if (entry.credentials().switch().name !== 'sorobanCredentialsAddress') {
          return entry;
        }
        const signedXdr = await this.signer.signAuthEntry(entry.toXDR('base64'), {
          networkPassphrase: this.networkConfig.networkPassphrase,
          validUntilLedgerSeq,
        });
        return xdr.SorobanAuthorizationEntry.fromXDR(signedXdr, 'base64');
      }));

      const hostFunction = operation.body().invokeHostFunctionOp().hostFunction();
      const authorizedOperation = Operation.invokeHostFunction({ func: hostFunction, auth });
      const authorizedTransaction = TransactionBuilder.cloneFrom(transaction)
        .clearOperations()
        .addOperation(authorizedOperation)
        .build();
      const assembled = SorobanRpc.assembleTransaction(
        authorizedTransaction,
        simulation,
      ).build();
      const signedXdr = await this.signer.signTransaction(assembled.toXDR(), {
        networkPassphrase: this.networkConfig.networkPassphrase,
      });
      const signed = TransactionBuilder.fromXDR(
        signedXdr,
        this.networkConfig.networkPassphrase,
      );

      const submitted = await this.rpc.sendTransaction(signed);
      if (submitted.status !== 'PENDING' && submitted.status !== 'DUPLICATE') {
        const diagnostics = this.diagnosticText(submitted.diagnosticEvents);
        throw this.contractError(
          'SUBMISSION_FAILED',
          `${method} submission failed (${submitted.status}): ${
            diagnostics || submitted.errorResult?.toXDR('base64') || 'unknown error'
          }`,
        );
      }

      for (let attempt = 0; attempt < 30; attempt++) {
        const confirmed = await this.rpc.getTransaction(submitted.hash);
        if (confirmed.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) {
          return {
            value: confirmed.returnValue ? scValToNative(confirmed.returnValue) : undefined,
            tx: { hash: submitted.hash, success: true, ledger: confirmed.ledger },
          };
        }
        if (confirmed.status === SorobanRpc.Api.GetTransactionStatus.FAILED) {
          const diagnostics = this.diagnosticText(confirmed.diagnosticEventsXdr);
          throw this.contractError(
            'TRANSACTION_FAILED',
            `${method} transaction failed${diagnostics ? `: ${diagnostics}` : ''}`,
            submitted.hash,
          );
        }
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
      throw new StellarAgentError(
        'TRANSACTION_TIMEOUT',
        `${method} transaction did not complete in time`,
        { transactionHash: submitted.hash },
      );
    } catch (error) {
      if (error instanceof StellarAgentError || error instanceof SigningError) throw error;
      throw new StellarAgentError(
        'NETWORK_ERROR',
        `${method} failed while communicating with Soroban RPC: ${this.errorMessage(error)}`,
        { cause: error },
      );
    }
  }

  private contractError(
    fallback: StellarAgentErrorCode,
    message: string,
    transactionHash?: string,
  ): StellarAgentError {
    const mappings: Array<[RegExp, StellarAgentErrorCode]> = [
      [/spend limit exceeded/i, 'SPEND_LIMIT_EXCEEDED'],
      [/channel not found/i, 'CHANNEL_NOT_FOUND'],
      [/channel is closed/i, 'CHANNEL_CLOSED'],
      [/job not found/i, 'JOB_NOT_FOUND'],
      [/job is not open/i, 'JOB_NOT_OPEN'],
      [/job has expired/i, 'JOB_EXPIRED'],
      [/not (?:the )?(?:authorized|assigned)|not authorized/i, 'NOT_AUTHORIZED'],
      [/no rate limit|limit not found/i, 'RATE_LIMIT_NOT_FOUND'],
      [/(?:amount|deposit|limit).*(?:positive|invalid)|deadline must/i, 'INVALID_ARGUMENT'],
    ];
    const code = mappings.find(([pattern]) => pattern.test(message))?.[1] ?? fallback;
    return new StellarAgentError(code, message, { transactionHash });
  }

  private resolveAssetContract(asset: string): string {
    if (asset === 'XLM') return Asset.native().contractId(this.networkConfig.networkPassphrase);
    const resolved = this.assetContracts[asset] ?? asset;
    try {
      Address.fromString(resolved);
      if (!resolved.startsWith('C')) throw new Error('not a contract');
      return resolved;
    } catch {
      throw new StellarAgentError(
        'INVALID_ARGUMENT',
        `Unknown asset "${asset}". Pass its C... token contract ID or configure assetContracts.${asset}.`,
      );
    }
  }

  private addressVal(value: string): xdr.ScVal {
    try {
      return Address.fromString(value).toScVal();
    } catch (error) {
      throw new StellarAgentError('INVALID_ARGUMENT', `Invalid Stellar address: ${value}`, {
        cause: error,
      });
    }
  }

  private i128(value: string): xdr.ScVal {
    try {
      return nativeToScVal(toStroops(value), { type: 'i128' });
    } catch (error) {
      throw new StellarAgentError('INVALID_ARGUMENT', `Invalid amount: ${value}`, { cause: error });
    }
  }

  private u64(value: bigint): xdr.ScVal {
    if (value < 0n || value > 0xffff_ffff_ffff_ffffn) {
      throw new StellarAgentError('INVALID_ARGUMENT', `Value is outside u64 range: ${value}`);
    }
    return nativeToScVal(value, { type: 'u64' });
  }

  private u32(value: number): xdr.ScVal {
    if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
      throw new StellarAgentError('INVALID_ARGUMENT', `Value is outside u32 range: ${value}`);
    }
    return nativeToScVal(value, { type: 'u32' });
  }

  private bytesVal(value: string): xdr.ScVal {
    return xdr.ScVal.scvBytes(Buffer.from(value, 'utf8'));
  }

  /** Rust contracttype unit enums encode as a one-element symbol vector. */
  private enumVal(variant: string): xdr.ScVal {
    return xdr.ScVal.scvVec([xdr.ScVal.scvSymbol(variant)]);
  }

  private spendPeriodVariant(period: OpenChannelParams['period']): string {
    return { per_ledger: 'PerLedger', hourly: 'Hourly', daily: 'Daily' }[period];
  }

  private async getLatestLedger(): Promise<number> {
    try {
      return (await this.rpc.getLatestLedger()).sequence;
    } catch (error) {
      throw new StellarAgentError('NETWORK_ERROR', 'Unable to read the latest ledger', {
        cause: error,
      });
    }
  }

  private asRecord(value: unknown): Record<string, unknown> {
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      if (value instanceof Map) return Object.fromEntries(value);
      return value as Record<string, unknown>;
    }
    throw new StellarAgentError('CONTRACT_ERROR', 'Contract returned a malformed struct');
  }

  private asBigInt(value: unknown): bigint {
    if (typeof value === 'bigint') return value;
    if (typeof value === 'number' && Number.isSafeInteger(value)) return BigInt(value);
    if (value && typeof value === 'object' && 'value' in value) {
      return this.asBigInt((value as { value: unknown }).value);
    }
    throw new StellarAgentError('CONTRACT_ERROR', 'Contract returned a malformed integer');
  }

  private asNumber(value: unknown): number {
    const number = typeof value === 'bigint' ? Number(value) : value;
    if (typeof number !== 'number' || !Number.isSafeInteger(number)) {
      throw new StellarAgentError('CONTRACT_ERROR', 'Contract returned a malformed u32');
    }
    return number;
  }

  private asString(value: unknown): string {
    if (typeof value === 'string') return value;
    if (value && typeof value === 'object' && 'toString' in value) return String(value);
    throw new StellarAgentError('CONTRACT_ERROR', 'Contract returned a malformed address');
  }

  private optionalString(value: unknown): string | null {
    return value == null ? null : this.asString(value);
  }

  private decodeBytes(value: unknown): string {
    if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
      return Buffer.from(value).toString('utf8');
    }
    throw new StellarAgentError('CONTRACT_ERROR', 'Contract returned malformed bytes');
  }

  private jobStatus(value: unknown): JobInfo['status'] {
    const raw = Array.isArray(value) ? value[0] : value;
    const status = String(raw).replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase();
    const valid: JobInfo['status'][] = [
      'open', 'in_progress', 'pending_release', 'completed', 'refunded', 'disputed',
    ];
    if (!valid.includes(status as JobInfo['status'])) {
      throw new StellarAgentError('CONTRACT_ERROR', `Unknown job status: ${String(raw)}`);
    }
    return status as JobInfo['status'];
  }

  private errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  private diagnosticText(events: xdr.DiagnosticEvent[] | undefined): string {
    if (!events?.length) return '';
    try {
      return events.map((diagnostic) => {
        const event = diagnostic.event();
        return JSON.stringify({
          topics: event.body().v0().topics().map((topic) => scValToNative(topic)),
          data: scValToNative(event.body().v0().data()),
        }, (_key, value) => typeof value === 'bigint' ? value.toString() : value);
      }).join('; ');
    } catch {
      return events.map((event) => event.toXDR('base64')).join('; ');
    }
  }

  private async fundFromFriendbot(): Promise<void> {
    try {
      const response = await fetch(
        `https://friendbot.stellar.org?addr=${this.address}`,
      );
      if (!response.ok) {
        console.warn('Friendbot funding failed — account may already exist');
      }
    } catch {
      console.warn('Could not reach friendbot');
    }
  }
}
