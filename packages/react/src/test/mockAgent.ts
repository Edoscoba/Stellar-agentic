import { vi } from 'vitest';
import { StellarAgent } from '@stellaragent/core';
import type {
  ChannelInfo,
  JobInfo,
  PayForAPIParams,
  RateLimitStatus,
  SpendReport,
  TxResult,
} from '@stellaragent/core';

export interface MockAgentOverrides {
  getSpendReport?: () => Promise<SpendReport>;
  getChannel?: (channelId: bigint) => Promise<ChannelInfo>;
  getJob?: (jobId: bigint) => Promise<JobInfo>;
  getRateLimitStatus?: () => Promise<RateLimitStatus>;
  payForAPI?: (params: PayForAPIParams) => Promise<TxResult>;
}

function unmocked(name: string) {
  return () => Promise.reject(new Error(`${name} not mocked for this test`));
}

/**
 * A minimal stand-in for `StellarAgent` covering the methods the hooks in
 * this package call. `StellarAgent` has private fields, so a plain object
 * can't structurally satisfy its type — the cast is the standard escape
 * hatch for mocking a class in tests.
 */
export function createMockAgent(overrides: MockAgentOverrides = {}): StellarAgent {
  const mock = {
    address: 'GMOCKAGENTADDRESSXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX',
    getSpendReport: vi.fn(overrides.getSpendReport ?? unmocked('getSpendReport')),
    getChannel: vi.fn(overrides.getChannel ?? unmocked('getChannel')),
    getJob: vi.fn(overrides.getJob ?? unmocked('getJob')),
    getRateLimitStatus: vi.fn(overrides.getRateLimitStatus ?? unmocked('getRateLimitStatus')),
    payForAPI: vi.fn(overrides.payForAPI ?? unmocked('payForAPI')),
  };
  return mock as unknown as StellarAgent;
}
