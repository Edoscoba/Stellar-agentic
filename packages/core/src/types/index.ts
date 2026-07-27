// ─── Network ─────────────────────────────────────────────────────────────────

export type Network = 'mainnet' | 'testnet' | 'local';

export interface NetworkConfig {
  rpcUrl: string;
  networkPassphrase: string;
  horizonUrl: string;
}

export const NETWORK_CONFIGS: Record<Network, NetworkConfig> = {
  mainnet: {
    rpcUrl: 'https://soroban-rpc.stellar.org',
    networkPassphrase: 'Public Global Stellar Network ; September 2015',
    horizonUrl: 'https://horizon.stellar.org',
  },
  testnet: {
    rpcUrl: 'https://soroban-rpc.testnet.stellar.gateway.fm',
    networkPassphrase: 'Test SDF Network ; September 2015',
    horizonUrl: 'https://horizon-testnet.stellar.org',
  },
  local: {
    rpcUrl: 'http://localhost:8000/soroban/rpc',
    networkPassphrase: 'Standalone Network ; February 2017',
    horizonUrl: 'http://localhost:8000',
  },
};

// ─── Agent ───────────────────────────────────────────────────────────────────

export type SpendPeriod = 'per_ledger' | 'hourly' | 'daily';

export interface SpendLimit {
  /** Maximum amount per period */
  amount: string;
  /** Asset to limit (e.g. 'USDC') */
  asset: string;
  /** How often the limit resets */
  period: SpendPeriod;
}

export interface StellarAgentConfig {
  /** Stellar network to connect to */
  network: Network;
  /**
   * Where signing happens.
   *
   * Prefer this over `secretKey` for anything holding real funds: with a
   * `RemoteSigner` (or a hardware/wallet-backed one) the key never enters
   * this process, so a heap dump or a compromised transitive dependency
   * cannot yield it. Mutually exclusive with `secretKey`.
   *
   * Typed structurally rather than imported to keep `types/` free of runtime
   * imports; see `signer.ts` for the interface and its implementations.
   */
  signer?: {
    getPublicKey(): Promise<string>;
    signTransaction(xdr: string, options: { networkPassphrase: string }): Promise<string>;
    signAuthEntry(
      authEntryXdr: string,
      options: { networkPassphrase: string; validUntilLedgerSeq: number },
    ): Promise<string>;
  };
  /**
   * Private key for the agent wallet (keep secret!).
   *
   * Holding a raw secret in a long-lived process is a real risk for an agent
   * with funds — use `signer` instead where that matters. Mutually exclusive
   * with `signer`.
   */
  secretKey?: string;
  /** Spend limit enforced on-chain */
  spendLimit?: SpendLimit;
  /**
   * Contract addresses. Anything omitted falls back to the
   * `STELLARAGENT_<NETWORK>_<CONTRACT>` / `STELLARAGENT_<CONTRACT>`
   * environment variables, then to the network's unconfigured sentinel.
   */
  contracts?: Partial<ContractAddresses>;
  /**
   * Skip the deployed-contracts check in `StellarAgent.create()`.
   *
   * By default an agent refuses to be created against contract addresses
   * that are not real deployed contract IDs, so the failure names the actual
   * problem instead of surfacing as an opaque RPC error mid-payment. Set
   * this when you only need calls that touch no contract at all — currently
   * `getBalance()` — or in tests. Any contract call made on such an agent
   * will still fail.
   *
   * @default false
   */
  allowUnconfiguredContracts?: boolean;
}

export interface AgentInfo {
  id: bigint;
  address: string;
  name: string;
  owner: string;
  active: boolean;
  createdAt: number;
  totalOps: bigint;
}

// ─── Payment Channel ─────────────────────────────────────────────────────────

export interface OpenChannelParams {
  /**
   * Token to use for payments (defaults to USDC). This remains the
   * channel's single funding/settlement asset — `limitPerPeriod` is always
   * denominated in it, even for cross-asset payments made via
   * `payForAPI`'s `destAsset` (see `PayForAPIParams`). Cross-asset support
   * lets one channel pay recipients in other assets; it does not make the
   * channel itself multi-asset.
   */
  token?: string;
  /** Initial deposit amount (as string to avoid precision issues) */
  deposit: string;
  /** Max spend per period, denominated in `token` */
  limitPerPeriod: string;
  period: SpendPeriod;
}

export interface PayForAPIParams {
  /** API endpoint being paid for (stored in memo) */
  endpoint: string;
  /** Amount to pay, denominated in the channel's settlement asset */
  amount: string;
  /** Asset to pay with (must match the channel's settlement asset) */
  asset?: string;
  /** Channel ID to use (uses default if not specified) */
  channelId?: bigint;
  /**
   * Asset the recipient should actually receive, if different from the
   * channel's settlement asset (`asset`) — e.g. a channel funded in USDC
   * paying a provider that only accepts XLM. When set, this routes through
   * `PaymentChannel.pay_with_conversion` instead of `pay`, converting via
   * the channel contract's configured price oracle + AMM. The spend limit
   * is still enforced in the channel's settlement asset regardless of
   * `destAsset`. Requires `minReceived` to also be set.
   */
  destAsset?: string;
  /**
   * Minimum amount of `destAsset` the recipient must receive (slippage
   * floor), as a string in `destAsset` units. Required when `destAsset` is
   * set. The contract additionally enforces its own oracle-derived
   * fairness bound on top of this — see
   * `contracts/payment_channel/src/lib.rs`'s `pay_with_conversion` for the
   * full slippage/price-oracle design.
   */
  minReceived?: string;
}

export interface ChannelInfo {
  id: bigint;
  agent: string;
  owner: string;
  token: string;
  limitPerPeriod: bigint;
  spentThisPeriod: bigint;
  totalSpent: bigint;
  active: boolean;
}

export interface SpendReport {
  spentThisPeriod: string;
  remainingThisPeriod: string;
  totalLifetime: string;
}

// ─── Escrow / Jobs ───────────────────────────────────────────────────────────

export type JobStatus =
  | 'open'
  | 'in_progress'
  | 'pending_release'
  | 'completed'
  | 'refunded'
  | 'disputed';

export interface RequestWorkParams {
  /** Address of the worker agent */
  workerAgent: string;
  /** Task description or IPFS hash */
  task: string;
  /** Amount to lock in escrow */
  escrowAmount: string;
  /** Asset to pay with */
  asset?: string;
  /** Deadline in ledgers from now */
  deadlineLedgers?: number;
  /** Optional arbiter address for disputes */
  arbiter?: string;
}

export interface JobInfo {
  id: bigint;
  requester: string;
  worker: string | null;
  arbiter: string | null;
  token: string;
  amount: bigint;
  taskDescription: string;
  result: string | null;
  deadlineLedger: number;
  status: JobStatus;
  createdAt: number;
}

// ─── Rate Limiter ────────────────────────────────────────────────────────────

export interface RateLimitConfig {
  maxPerTx: string;
  maxPerHour: string;
  maxPerDay: string;
  maxTxsPerHour: number;
}

/** Current rate-limit usage alongside the configured limits, for `RateLimiter`. */
export interface RateLimitStatus extends RateLimitConfig {
  /** Amount spent in the current rolling hour */
  spentThisHour: string;
  /** Amount spent in the current rolling day */
  spentToday: string;
  /** Transaction count in the current rolling hour */
  txsThisHour: number;
}

// ─── Contracts ───────────────────────────────────────────────────────────────

export interface ContractAddresses {
  agentWalletFactory: string;
  paymentChannel: string;
  escrow: string;
  rateLimiter: string;
  circuitBreaker: string;
}

// ─── Events ──────────────────────────────────────────────────────────────────

export interface AgentEvent {
  type: 'payment' | 'job_created' | 'job_completed' | 'rate_limit_hit' | 'agent_killed';
  agentId: bigint;
  timestamp: number;
  data: Record<string, unknown>;
}

// ─── Results ─────────────────────────────────────────────────────────────────

export interface TxResult {
  /** Transaction hash */
  hash: string;
  /** Whether the transaction succeeded */
  success: boolean;
  /** Ledger number it was confirmed in */
  ledger?: number;
}
