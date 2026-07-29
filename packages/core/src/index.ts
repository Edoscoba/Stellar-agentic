// Keypair is no longer imported here: key handling moved behind the Signer
// abstraction in ./signer.ts. The rest are staged for the pending Soroban
// invocation work.
import {
  Networks,
  TransactionBuilder,
  BASE_FEE,
  Operation,
  Asset,
  Horizon,
} from '@stellar/stellar-sdk';

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
export { CircuitBreaker } from './circuitBreaker.js';
export type { CircuitBreakerOptions } from './circuitBreaker.js';

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
  private horizon: Horizon.Server;
  private activeChannelId?: bigint;

  private constructor(
    signer: Signer,
    publicKey: string,
    networkConfig: NetworkConfig,
    contracts: ContractAddresses,
  ) {
    this.signer = signer;
    this.publicKey = publicKey;
    this.networkConfig = networkConfig;
    this.contracts = contracts;
    // `Horizon.Server` refuses plain-HTTP endpoints unless `allowHttp` is set,
    // which made the `local` network config (http://localhost:8000) throw
    // "Cannot connect to insecure horizon server" from the constructor. Allow
    // HTTP for loopback only — a plaintext connection to a real network would
    // expose submitted transactions, so this must not be blanket-enabled.
    this.horizon = new Horizon.Server(networkConfig.horizonUrl, {
      allowHttp: isLoopbackUrl(networkConfig.horizonUrl),
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

    const agent = new StellarAgent(signer, publicKey, networkConfig, contracts);

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

  // ── Payment Channel ──────────────────────────────────────────────────────

  /**
   * Open a payment channel for this agent.
   * Deposits tokens and sets a per-period spend limit.
   *
   * @returns The channel ID
   */
  async openChannel(params: OpenChannelParams): Promise<bigint> {
    // TODO: Invoke AgentWalletFactory.create_agent + PaymentChannel.open_channel
    // via Soroban contract invocation
    console.log('Opening channel with params:', params);
    throw new Error('Not yet implemented — contract addresses needed. See CONTRIBUTING.md');
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
    if (!this.activeChannelId) {
      throw new Error('No active payment channel. Call openChannel() first.');
    }

    if ((params.destAsset !== undefined) !== (params.minReceived !== undefined)) {
      throw new Error('destAsset and minReceived must be set together');
    }

    // TODO: Invoke PaymentChannel.pay (or pay_with_conversion, when
    // params.destAsset is set) via Soroban — see companion SDK issue.
    console.log('Paying for API:', params);
    throw new Error('Not yet implemented — see contracts/payment_channel/src/lib.rs');
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
    // TODO: Invoke Escrow.create_job via Soroban
    console.log('Requesting work:', params);
    throw new Error('Not yet implemented — see contracts/escrow/src/lib.rs');
  }

  /**
   * Accept an open escrow job as a worker agent
   */
  async acceptJob(jobId: bigint): Promise<TxResult> {
    // TODO: Invoke Escrow.accept_job
    throw new Error('Not yet implemented');
  }

  /**
   * Submit work result for an escrow job
   */
  async submitResult(jobId: bigint, result: string): Promise<TxResult> {
    // TODO: Invoke Escrow.submit_result
    throw new Error('Not yet implemented');
  }

  /**
   * Release escrow payment to the worker after work is complete
   */
  async releasePayment(jobId: bigint): Promise<TxResult> {
    // TODO: Invoke Escrow.release
    throw new Error('Not yet implemented');
  }

  // ── Rate Limits ──────────────────────────────────────────────────────────

  /**
   * Configure rate limits for this agent on-chain.
   * Protects against runaway spending.
   */
  async setRateLimits(config: RateLimitConfig): Promise<TxResult> {
    // TODO: Invoke RateLimiter.set_limits
    throw new Error('Not yet implemented');
  }

  /**
   * Check if a payment would be blocked by rate limits (read-only)
   */
  async checkRateLimit(amount: string): Promise<boolean> {
    // TODO: Invoke RateLimiter.check (read-only call)
    throw new Error('Not yet implemented');
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
    // TODO: Query PaymentChannel.remaining_this_period
    throw new Error('Not yet implemented');
  }

  /**
   * Get info about a payment channel
   */
  async getChannel(channelId: bigint): Promise<ChannelInfo> {
    // TODO: Query PaymentChannel.get_channel
    throw new Error('Not yet implemented');
  }

  /**
   * Get info about a job
   */
  async getJob(jobId: bigint): Promise<JobInfo> {
    // TODO: Query Escrow.get_job
    throw new Error('Not yet implemented');
  }

  /**
   * Get current rate-limit usage alongside the configured limits
   */
  async getRateLimitStatus(): Promise<RateLimitStatus> {
    // TODO: Query RateLimiter.get_status
    throw new Error('Not yet implemented');
  }

  // ── Internals ────────────────────────────────────────────────────────────

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
