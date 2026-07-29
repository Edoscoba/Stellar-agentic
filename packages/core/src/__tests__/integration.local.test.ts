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
import {
  Address,
  Contract,
  Keypair,
  SorobanRpc,
  TransactionBuilder,
  BASE_FEE,
  nativeToScVal,
  scValToNative,
} from '@stellar/stellar-sdk';

import { StellarAgent } from '../index.js';
import { resolveContracts } from '../contracts.js';
import { predictPaymentOutcome, type RateLimitSpendState } from '../math/predict.js';

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

// ─── predictPaymentOutcome vs on-chain RateLimiter.check — fuzz ──────────────
//
// `predictPaymentOutcome` (math/predict.ts) is a TypeScript re-implementation
// of `RateLimiter::check`'s logic, so its whole reason for existing is to
// agree with the real contract call it stands in for. This suite drives both
// the real `RateLimiter` contract and `predictPaymentOutcome` with the same
// fuzzed (state, amount) pairs and asserts every single one agrees —
// checking there are no false positives (predicting a block the chain would
// actually allow) or false negatives (predicting a pass the chain would
// actually reject).
//
// This calls the deployed `RateLimiter` contract directly via Soroban RPC
// (build → simulate → sign → submit → poll), the same low-level pattern
// `circuitBreaker.ts` uses, rather than going through `StellarAgent` — the
// SDK's own `setRateLimits`/`checkRateLimit` are still stubs (see the
// lifecycle block above), and this suite needs to work today regardless of
// when that lands.
describeLocal('predictPaymentOutcome vs on-chain RateLimiter.check — fuzz', () => {
  const rpcServer = new SorobanRpc.Server('http://localhost:8000/soroban/rpc', {
    allowHttp: true,
  });
  const NETWORK_PASSPHRASE = 'Standalone Network ; February 2017';

  let rateLimiterContract: Contract;
  let owner: Keypair;
  let agentKeypair: Keypair;

  /** Fund a fresh keypair on the local standalone network via its bundled friendbot. */
  async function fund(publicKey: string): Promise<void> {
    const res = await fetch(`http://localhost:8000/friendbot?addr=${publicKey}`);
    if (!res.ok) {
      throw new Error(`local friendbot funding failed for ${publicKey}: ${res.status}`);
    }
  }

  async function invoke(
    signer: Keypair,
    method: string,
    args: Parameters<Contract['call']>[1][],
  ): Promise<unknown> {
    const account = await rpcServer.getAccount(signer.publicKey());
    const tx = new TransactionBuilder(account, {
      fee: BASE_FEE,
      networkPassphrase: NETWORK_PASSPHRASE,
    })
      .addOperation(rateLimiterContract.call(method, ...args))
      .setTimeout(30)
      .build();

    const simulated = await rpcServer.simulateTransaction(tx);
    if (SorobanRpc.Api.isSimulationError(simulated)) {
      throw new Error(`${method} simulation failed: ${simulated.error}`);
    }

    const prepared = SorobanRpc.assembleTransaction(tx, simulated).build();
    prepared.sign(signer);
    const sent = await rpcServer.sendTransaction(prepared);
    if (sent.status === 'ERROR') {
      throw new Error(`${method} submission failed: ${JSON.stringify(sent.errorResult)}`);
    }

    for (let attempt = 0; attempt < 15; attempt++) {
      const result = await rpcServer.getTransaction(sent.hash);
      if (result.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) {
        return result.returnValue ? scValToNative(result.returnValue) : undefined;
      }
      if (result.status === SorobanRpc.Api.GetTransactionStatus.FAILED) {
        throw new Error(`${method} transaction failed: ${JSON.stringify(result)}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 1000));
    }
    throw new Error(`${method} did not complete in time`);
  }

  /** Read-only `check` call — no signing, matching `circuitBreaker.ts`'s `isPaused` pattern. */
  async function check(agent: string, amount: bigint): Promise<boolean> {
    const account = await rpcServer.getAccount(owner.publicKey());
    const tx = new TransactionBuilder(account, {
      fee: BASE_FEE,
      networkPassphrase: NETWORK_PASSPHRASE,
    })
      .addOperation(
        rateLimiterContract.call(
          'check',
          new Address(agent).toScVal(),
          nativeToScVal(amount, { type: 'i128' }),
        ),
      )
      .setTimeout(30)
      .build();

    const simulated = await rpcServer.simulateTransaction(tx);
    if (SorobanRpc.Api.isSimulationError(simulated)) {
      throw new Error(`check simulation failed: ${simulated.error}`);
    }
    return simulated.result?.retval ? scValToNative(simulated.result.retval) === true : false;
  }

  async function getLimits(agent: string): Promise<RateLimitSpendState> {
    const account = await rpcServer.getAccount(owner.publicKey());
    const tx = new TransactionBuilder(account, {
      fee: BASE_FEE,
      networkPassphrase: NETWORK_PASSPHRASE,
    })
      .addOperation(rateLimiterContract.call('get_limits', new Address(agent).toScVal()))
      .setTimeout(30)
      .build();

    const simulated = await rpcServer.simulateTransaction(tx);
    if (SorobanRpc.Api.isSimulationError(simulated)) {
      throw new Error(`get_limits simulation failed: ${simulated.error}`);
    }
    const raw = scValToNative(simulated.result!.retval) as {
      active: boolean;
      max_per_tx: bigint;
      max_per_hour: bigint;
      max_per_day: bigint;
      max_txs_per_hour: number;
      hourly_spend: bigint;
      daily_spend: bigint;
      hourly_tx_count: number;
      hour_window_start: number;
      day_window_start: number;
    };
    return {
      configured: true,
      active: raw.active,
      maxPerTx: raw.max_per_tx.toString(),
      maxPerHour: raw.max_per_hour.toString(),
      maxPerDay: raw.max_per_day.toString(),
      maxTxsPerHour: raw.max_txs_per_hour,
      hourlySpend: raw.hourly_spend.toString(),
      dailySpend: raw.daily_spend.toString(),
      hourlyTxCount: raw.hourly_tx_count,
      hourWindowStartLedger: raw.hour_window_start,
      dayWindowStartLedger: raw.day_window_start,
    };
  }

  beforeAll(async () => {
    const contracts = resolveContracts('local');
    rateLimiterContract = new Contract(contracts.rateLimiter);

    owner = Keypair.random();
    agentKeypair = Keypair.random();
    await fund(owner.publicKey());
    await fund(agentKeypair.publicKey());

    // A single fixed configuration; the fuzz varies (amount, and — via
    // record_payment — accumulated spend/tx-count) around it rather than
    // varying the limits themselves, which keeps the on-chain setup to one
    // agent for the whole suite.
    await invoke(owner, 'set_limits', [
      new Address(owner.publicKey()).toScVal(),
      new Address(agentKeypair.publicKey()).toScVal(),
      nativeToScVal(1_000n, { type: 'i128' }), // max_per_tx
      nativeToScVal(5_000n, { type: 'i128' }), // max_per_hour
      nativeToScVal(20_000n, { type: 'i128' }), // max_per_day
      nativeToScVal(6, { type: 'u32' }), // max_txs_per_hour
    ]);
  }, 60_000);

  it('agrees with RateLimiter.check across a fuzzed set of (recorded-history, proposed-amount) scenarios', async () => {
    // Deterministic PRNG so a failure is reproducible without needing to
    // capture random state.
    let seed = 42;
    const rand = () => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed / 0x7fffffff;
    };
    const randomAmount = (max: number) => BigInt(Math.floor(rand() * max));

    for (let scenario = 0; scenario < 25; scenario++) {
      // Occasionally record a real payment first, so `hourly_spend` /
      // `daily_spend` / `hourly_tx_count` accumulate real on-chain state
      // rather than every scenario starting from zero — otherwise the fuzz
      // would only ever exercise the per-tx boundary.
      if (rand() < 0.5) {
        const recordedAmount = randomAmount(400); // stays comfortably under max_per_tx
        try {
          await invoke(owner, 'record_payment', [
            new Address(owner.publicKey()).toScVal(),
            new Address(agentKeypair.publicKey()).toScVal(),
            nativeToScVal(recordedAmount, { type: 'i128' }),
          ]);
        } catch {
          // The hourly tx-count cap (6) may already be exhausted for this
          // run — that's fine, the fuzz below still has real state to test
          // predictions against.
        }
      }

      const proposedAmount = randomAmount(1_500); // spans both sides of max_per_tx (1000)
      const [onChainResult, limits, ledger] = await Promise.all([
        check(agentKeypair.publicKey(), proposedAmount),
        getLimits(agentKeypair.publicKey()),
        rpcServer.getLatestLedger(),
      ]);

      const prediction = predictPaymentOutcome({
        rateLimitState: limits,
        amount: proposedAmount.toString(),
        currentLedger: ledger.sequence,
      });

      expect(prediction.wouldBlock).toBe(!onChainResult);
    }
  }, 120_000);
});
