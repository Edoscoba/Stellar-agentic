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
  resolveContracts,
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
