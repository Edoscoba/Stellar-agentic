import type {
  ChannelInfo,
  PayForAPIParams,
  RateLimitStatus,
  SpendReport,
  StellarAgent,
  TxResult,
} from '@stellaragent/core';

const DEMO_CHANNEL_ID = 1n;
const DEMO_LIMIT = 10; // human units of the channel's settlement asset

function delay<T>(value: T, ms: number): Promise<T> {
  return new Promise((resolve) => setTimeout(() => resolve(value), ms));
}

/**
 * A fully in-memory stand-in for `StellarAgent`, used by this example by
 * default. `@stellaragent/core`'s Soroban-facing query/mutation methods
 * (`getSpendReport`, `getChannel`, `getRateLimitStatus`, `payForAPI`, …)
 * are still stubs that throw `Not yet implemented` — see the companion
 * SDK issue — so pointing `StellarAgentProvider` at `network: 'local'`
 * without an injected agent will *not* return real data yet, even against
 * a running Soroban standalone network. This demo agent exists so the
 * hook layer (polling, loading/error states, optimistic updates) can
 * still be exercised end-to-end today. Swap it for a real
 * `StellarAgent.create(...)` call — no hook code changes needed — once
 * that companion work lands. See the README for details.
 */
export function createDemoAgent(): StellarAgent {
  const state = {
    spent: 2.5,
    txsThisHour: 3,
  };

  const agent: Pick<
    StellarAgent,
    'address' | 'getChannel' | 'getSpendReport' | 'getRateLimitStatus' | 'getJob' | 'payForAPI'
  > = {
    address: 'GDEMO7AGENTADDRESSEXAMPLE0000000000000000000000000000000',

    async getChannel(channelId: bigint): Promise<ChannelInfo> {
      await delay(null, 300);
      return {
        id: channelId,
        agent: agent.address,
        owner: 'GDEMO7OWNERADDRESSEXAMPLE00000000000000000000000000000',
        token: 'USDC',
        limitPerPeriod: BigInt(Math.round(DEMO_LIMIT * 1e7)),
        spentThisPeriod: BigInt(Math.round(state.spent * 1e7)),
        totalSpent: BigInt(Math.round(state.spent * 1e7)),
        active: true,
      };
    },

    async getSpendReport(): Promise<SpendReport> {
      await delay(null, 300);
      return {
        spentThisPeriod: state.spent.toFixed(7),
        remainingThisPeriod: Math.max(DEMO_LIMIT - state.spent, 0).toFixed(7),
        totalLifetime: state.spent.toFixed(7),
      };
    },

    async getRateLimitStatus(): Promise<RateLimitStatus> {
      await delay(null, 300);
      return {
        maxPerTx: '5',
        maxPerHour: '20',
        maxPerDay: '100',
        maxTxsPerHour: 50,
        spentThisHour: state.spent.toFixed(7),
        spentToday: state.spent.toFixed(7),
        txsThisHour: state.txsThisHour,
      };
    },

    async getJob() {
      throw new Error('This demo only exercises the payment-channel hooks, not escrow jobs.');
    },

    async payForAPI(params: PayForAPIParams): Promise<TxResult> {
      // Simulate realistic submit + confirm latency so the optimistic
      // update in the UI is actually visible before it reconciles.
      await delay(null, 900);

      const amount = Number(params.amount);
      if (!Number.isFinite(amount) || amount <= 0) {
        throw new Error('Invalid amount');
      }
      if (state.spent + amount > DEMO_LIMIT) {
        throw new Error('Demo spend limit exceeded — reload to reset the demo channel');
      }

      state.spent += amount;
      state.txsThisHour += 1;

      return {
        hash: `demo-${Date.now().toString(16)}`,
        success: true,
        ledger: Math.floor(Date.now() / 5000),
      };
    },
  };

  return agent as unknown as StellarAgent;
}

export { DEMO_CHANNEL_ID };
