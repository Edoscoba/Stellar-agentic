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
  /** Private key for the agent wallet (keep secret!) */
  secretKey?: string;
  /** Spend limit enforced on-chain */
  spendLimit?: SpendLimit;
  /** Contract addresses (optional — uses defaults for network) */
  contracts?: Partial<ContractAddresses>;
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
  /** Token to use for payments (defaults to USDC) */
  token?: string;
  /** Initial deposit amount (as string to avoid precision issues) */
  deposit: string;
  /** Max spend per period */
  limitPerPeriod: string;
  period: SpendPeriod;
}

export interface PayForAPIParams {
  /** API endpoint being paid for (stored in memo) */
  endpoint: string;
  /** Amount to pay */
  amount: string;
  /** Asset to pay with */
  asset?: string;
  /** Channel ID to use (uses default if not specified) */
  channelId?: bigint;
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

// ─── Contracts ───────────────────────────────────────────────────────────────

export interface ContractAddresses {
  agentWalletFactory: string;
  paymentChannel: string;
  escrow: string;
  rateLimiter: string;
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
