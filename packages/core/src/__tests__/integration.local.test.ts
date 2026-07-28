/**
 * Integration tests against a local Soroban standalone network.
 *
 * ## Why these are gated
 *
 * These tests need a running `stellar network start local` (Horizon on
 * :8000, Soroban RPC on :8000/soroban/rpc) *and* the four contracts deployed
 * and wired together. Neither is available in a bare `pnpm test`, so the
 * whole suite is skipped unless `STELLAR_LOCAL_INTEGRATION=1` is set.
 *
 * ```bash
 * stellar network start local
 * pnpm --filter @stellaragent/core exec tsx ../../scripts/deploy.ts --network local
 * STELLAR_LOCAL_INTEGRATION=1 pnpm --filter @stellaragent/core test
 * ```
 *
 * ## Current state
 *
 * The lifecycle assertions below are the acceptance criteria for the
 * companion "real Soroban invocation" work: `openChannel`, `payForAPI`,
 * `requestWork` and the query methods are still stubs that throw
 * "Not yet implemented". Until that lands, the lifecycle block is marked
 * `.todo` — it describes the contract the implementation must satisfy
 * without reporting a false pass. The connectivity block below it does run,
 * and is what proves the local-network plumbing works today.
 */

import { describe, it, expect, beforeAll } from 'vitest';

import { StellarAgent } from '../index.js';

const ENABLED = process.env.STELLAR_LOCAL_INTEGRATION === '1';
const describeLocal = ENABLED ? describe : describe.skip;

/** Same deterministic test seed used by the unit suite. */
const TEST_SECRET = 'SADQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQP54X';

describeLocal('local standalone network — connectivity', () => {
  let agent: StellarAgent;

  beforeAll(async () => {
    agent = await StellarAgent.create({ network: 'local', secretKey: TEST_SECRET });
  });

  it('constructs an agent against the plain-HTTP local Horizon', () => {
    expect(agent.address).toMatch(/^G[A-Z2-7]{55}$/);
  });

  it('reads a balance without any signing', async () => {
    // An unfunded account reads as '0' rather than throwing — that fallback is
    // what makes the SDK usable before friendbot/mint has run.
    const balance = await agent.getBalance();
    expect(balance).toMatch(/^\d+(\.\d+)?$/);
  });

  it('reaches Horizon at all', async () => {
    const res = await fetch('http://localhost:8000');
    expect(res.ok).toBe(true);
  });
});

describeLocal('local standalone network — full payment lifecycle', () => {
  // Unblocked by the companion "real Soroban invocation" issue. Each `todo`
  // is one acceptance criterion of that work.
  it.todo('opens a payment channel and returns a channel id');
  it.todo('reports the opened channel via getChannel()');
  it.todo('pays for an API call and returns a successful TxResult');
  it.todo('reflects the payment in getSpendReport()');
  it.todo('rejects a payment that would exceed the on-chain spend limit');
  it.todo('converts assets via pay_with_conversion when destAsset is set');
  it.todo('creates an escrow job via requestWork()');
  it.todo('runs accept → submit → release and pays out the worker');
  it.todo('enforces configured rate limits via setRateLimits/checkRateLimit');
});
