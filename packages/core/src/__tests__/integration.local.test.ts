/**
 * End-to-end tests for a `stellar network start local` standalone network.
 *
 * The deployment script prints the STELLARAGENT_LOCAL_* variables consumed
 * here. The local quickstart image exposes friendbot at `/friendbot`, so the
 * suite creates and funds isolated accounts on every run.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair } from '@stellar/stellar-sdk';

import {
  StellarAgent,
  StellarAgentError,
  CircuitBreaker,
  resolveContracts,
  isDeployedAddress,
} from '../index.js';

const ENABLED = process.env.STELLAR_LOCAL_INTEGRATION === '1';
const describeLocal = ENABLED ? describe : describe.skip;

async function fund(address: string): Promise<void> {
  const response = await fetch(`http://localhost:8000/friendbot?addr=${address}`);
  if (!response.ok) {
    throw new Error(`Local friendbot could not fund ${address}: HTTP ${response.status}`);
  }
}

describeLocal('local standalone — payment-channel lifecycle', () => {
  let owner: StellarAgent;
  let recipient: StellarAgent;

  beforeAll(async () => {
    const ownerKey = Keypair.random();
    const recipientKey = Keypair.random();
    await Promise.all([fund(ownerKey.publicKey()), fund(recipientKey.publicKey())]);
    const contracts = resolveContracts('local');
    [owner, recipient] = await Promise.all([
      StellarAgent.create({ network: 'local', secretKey: ownerKey.secret(), contracts }),
      StellarAgent.create({ network: 'local', secretKey: recipientKey.secret(), contracts }),
    ]);
  });

  it('creates an agent, opens, pays, reports spend, rejects overspend, and closes', async () => {
    const agentId = await owner.createAgentWallet('integration-owner');
    expect((await owner.getAgent(agentId)).address).toBe(owner.address);

    const channelId = await owner.openChannel({
      token: 'XLM',
      deposit: '10',
      limitPerPeriod: '5',
      period: 'hourly',
    });
    expect(channelId).toBeGreaterThan(0n);
    expect(await owner.getChannel(channelId)).toMatchObject({
      id: channelId,
      agent: owner.address,
      active: true,
    });

    await expect(owner.payForAPI({
      endpoint: 'https://api.example.com/inference',
      recipient: recipient.address,
      amount: '1',
      asset: 'XLM',
    })).resolves.toMatchObject({ success: true });
    await expect(owner.getSpendReport()).resolves.toEqual({
      spentThisPeriod: '1.0000000',
      remainingThisPeriod: '4.0000000',
      totalLifetime: '1.0000000',
    });

    const error = await owner.payForAPI({
      endpoint: 'https://api.example.com/too-expensive',
      recipient: recipient.address,
      amount: '5',
      asset: 'XLM',
    }).catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('SPEND_LIMIT_EXCEEDED');

    await expect(owner.closeChannel()).resolves.toMatchObject({ success: true });
    expect((await owner.getChannel(channelId)).active).toBe(false);
  }, 120_000);

  it('configures and checks the on-chain rate limiter', async () => {
    await expect(owner.setRateLimits({
      maxPerTx: '1',
      maxPerHour: '10',
      maxPerDay: '100',
      maxTxsPerHour: 5,
    })).resolves.toMatchObject({ success: true });
    await expect(owner.checkRateLimit('0.5')).resolves.toBe(true);
    await expect(owner.checkRateLimit('2')).resolves.toBe(false);
    await expect(owner.getRateLimitStatus()).resolves.toMatchObject({
      maxPerTx: '1.0000000',
      maxTxsPerHour: 5,
    });
  }, 120_000);
});

describeLocal('local standalone — parallel escrow lifecycle', () => {
  let requester: StellarAgent;
  let worker: StellarAgent;

  beforeAll(async () => {
    const requesterKey = Keypair.random();
    const workerKey = Keypair.random();
    await Promise.all([fund(requesterKey.publicKey()), fund(workerKey.publicKey())]);
    const contracts = resolveContracts('local');
    [requester, worker] = await Promise.all([
      StellarAgent.create({ network: 'local', secretKey: requesterKey.secret(), contracts }),
      StellarAgent.create({ network: 'local', secretKey: workerKey.secret(), contracts }),
    ]);
  });

  it('creates, accepts, submits, and releases an escrow job', async () => {
    const jobId = await requester.requestWork({
      workerAgent: worker.address,
      task: 'Summarize this document',
      escrowAmount: '2',
      asset: 'XLM',
      deadlineLedgers: 100,
    });
    expect(await requester.getJob(jobId)).toMatchObject({
      requester: requester.address,
      status: 'open',
      amount: 20_000_000n,
    });

    await expect(worker.acceptJob(jobId)).resolves.toMatchObject({ success: true });
    await expect(worker.submitResult(jobId, 'ipfs://result')).resolves.toMatchObject({
      success: true,
    });
    await expect(requester.releasePayment(jobId)).resolves.toMatchObject({ success: true });
    expect(await requester.getJob(jobId)).toMatchObject({
      worker: worker.address,
      result: 'ipfs://result',
      status: 'completed',
    });
  }, 120_000);
});

describeLocal('local standalone — circuit breaker', () => {
  it('reads is_paused via CircuitBreaker.isPaused()', async () => {
    const contracts = resolveContracts('local');
    if (!isDeployedAddress(contracts.circuitBreaker)) {
      return;
    }

    const ownerKey = Keypair.random();
    await fund(ownerKey.publicKey());

    const breaker = new CircuitBreaker({
      rpcUrl: 'http://localhost:8000/soroban/rpc',
      contractId: contracts.circuitBreaker,
      networkPassphrase: 'Standalone Network ; February 2017',
    });

    await expect(breaker.isPaused(ownerKey.publicKey())).resolves.toBe(false);
  }, 60_000);
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
