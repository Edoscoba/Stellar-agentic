import {
  Keypair,
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
} from './math/index.js';
export type {
  AgentBid,
  BidWeights,
  ScoredBid,
} from './math/bid.js';

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

// ─── Default Testnet Contract Addresses ──────────────────────────────────────
// TODO: Update these after deploying contracts to testnet

const DEFAULT_CONTRACTS: Record<Network, ContractAddresses> = {
  testnet: {
    agentWalletFactory: 'CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
    paymentChannel: 'CBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB',
    escrow: 'CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC',
    rateLimiter: 'CDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD',
    circuitBreaker: 'CEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE',
  },
  mainnet: {
    agentWalletFactory: '',
    paymentChannel: '',
    escrow: '',
    rateLimiter: '',
    circuitBreaker: '',
  },
  local: {
    agentWalletFactory: '',
    paymentChannel: '',
    escrow: '',
    rateLimiter: '',
    circuitBreaker: '',
  },
};

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
  private keypair: Keypair;
  private networkConfig: NetworkConfig;
  private contracts: ContractAddresses;
  private horizon: Horizon.Server;
  private activeChannelId?: bigint;

  private constructor(
    keypair: Keypair,
    networkConfig: NetworkConfig,
    contracts: ContractAddresses,
  ) {
    this.keypair = keypair;
    this.networkConfig = networkConfig;
    this.contracts = contracts;
    this.horizon = new Horizon.Server(networkConfig.horizonUrl);
  }

  // ── Factory Methods ──────────────────────────────────────────────────────

  /**
   * Create a new StellarAgent instance.
   * If no secretKey is provided, generates a fresh keypair.
   */
  static async create(config: StellarAgentConfig): Promise<StellarAgent> {
    const keypair = config.secretKey
      ? Keypair.fromSecret(config.secretKey)
      : Keypair.random();

    const networkConfig = NETWORK_CONFIGS[config.network];
    const contracts = {
      ...DEFAULT_CONTRACTS[config.network],
      ...config.contracts,
    };

    const agent = new StellarAgent(keypair, networkConfig, contracts);

    // If testnet and fresh keypair, fund from friendbot
    if (config.network === 'testnet' && !config.secretKey) {
      await agent.fundFromFriendbot();
    }

    return agent;
  }

  /**
   * Restore an agent from an existing secret key.
   */
  static async fromSecret(
    secretKey: string,
    network: Network = 'testnet',
  ): Promise<StellarAgent> {
    return StellarAgent.create({ network, secretKey });
  }

  // ── Identity ─────────────────────────────────────────────────────────────

  /** The agent's Stellar public address */
  get address(): string {
    return this.keypair.publicKey();
  }

  /** The agent's secret key — keep this safe! */
  get secretKey(): string {
    return this.keypair.secret();
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
