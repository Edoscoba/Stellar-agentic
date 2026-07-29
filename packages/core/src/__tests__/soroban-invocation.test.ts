import { describe, expect, it, vi } from 'vitest';
import {
  Account,
  Address,
  SorobanDataBuilder,
  SorobanRpc,
  nativeToScVal,
  scValToNative,
  xdr,
} from '@stellar/stellar-sdk';

import { StellarAgent, StellarAgentError } from '../index.js';
import type { Signer } from '../signer.js';
import { TEST_PUBLIC, TEST_SECRET, DEPLOYED_CONTRACTS } from './fixtures.js';

function addressAuthEntry(): xdr.SorobanAuthorizationEntry {
  const invokeArgs = new xdr.InvokeContractArgs({
    contractAddress: Address.fromString(DEPLOYED_CONTRACTS.paymentChannel).toScAddress(),
    functionName: 'open_channel',
    args: [],
  });
  return new xdr.SorobanAuthorizationEntry({
    credentials: xdr.SorobanCredentials.sorobanCredentialsAddress(
      new xdr.SorobanAddressCredentials({
        address: Address.fromString(TEST_PUBLIC).toScAddress(),
        nonce: xdr.Int64.fromString('1'),
        signatureExpirationLedger: 0,
        signature: xdr.ScVal.scvVoid(),
      }),
    ),
    rootInvocation: new xdr.SorobanAuthorizedInvocation({
      function: xdr.SorobanAuthorizedFunction
        .sorobanAuthorizedFunctionTypeContractFn(invokeArgs),
      subInvocations: [],
    }),
  });
}

function simulation(retval: xdr.ScVal, auth: xdr.SorobanAuthorizationEntry[] = []) {
  return {
    id: 'simulation',
    latestLedger: 100,
    events: [],
    _parsed: true,
    transactionData: new SorobanDataBuilder(),
    minResourceFee: '0',
    cost: { cpuInsns: '0', memBytes: '0' },
    result: { auth, retval },
  };
}

async function agentWithRpc(rpc: Record<string, unknown>) {
  const agent = await StellarAgent.create({
    network: 'testnet',
    secretKey: TEST_SECRET,
    contracts: DEPLOYED_CONTRACTS,
  });
  (agent as unknown as { rpc: Record<string, unknown> }).rpc = rpc;
  return agent;
}

describe('shared Soroban invocation pipeline', () => {
  it('simulates, signs auth and envelope, submits, polls, and tracks the channel', async () => {
    const auth = addressAuthEntry();
    const rpc = {
      getAccount: vi.fn(async () => new Account(TEST_PUBLIC, '1')),
      simulateTransaction: vi.fn(async (_transaction: unknown) => simulation(
        nativeToScVal(7n, { type: 'u64' }),
        [auth],
      )),
      sendTransaction: vi.fn(async () => ({ status: 'PENDING', hash: 'tx-hash' })),
      getTransaction: vi.fn(async () => ({
        status: SorobanRpc.Api.GetTransactionStatus.SUCCESS,
        ledger: 123,
        returnValue: nativeToScVal(7n, { type: 'u64' }),
      })),
    };
    const agent = await agentWithRpc(rpc);
    const signer = (agent as unknown as { signer: Signer }).signer;
    const authSpy = vi.spyOn(signer, 'signAuthEntry').mockImplementation(
      async (entry: string) => entry,
    );
    const txSpy = vi.spyOn(signer, 'signTransaction');

    await expect(agent.openChannel({
      deposit: '10',
      limitPerPeriod: '1',
      period: 'hourly',
    })).resolves.toBe(7n);

    expect(rpc.simulateTransaction).toHaveBeenCalledOnce();
    const simulatedTx = rpc.simulateTransaction.mock.calls[0]![0] as {
      operations: Array<{
        func: { invokeContract(): { args(): xdr.ScVal[] } };
      }>;
    };
    const invokeArgs = simulatedTx.operations[0].func.invokeContract().args();
    expect(invokeArgs.map((arg: xdr.ScVal) => arg.switch().name)).toEqual([
      'scvAddress', 'scvAddress', 'scvAddress', 'scvI128', 'scvI128', 'scvVec',
    ]);
    expect(scValToNative(invokeArgs[3])).toBe(100_000_000n);
    expect(scValToNative(invokeArgs[4])).toBe(10_000_000n);
    expect(scValToNative(invokeArgs[5])).toEqual(['Hourly']);
    expect(authSpy).toHaveBeenCalledWith(auth.toXDR('base64'), {
      networkPassphrase: 'Test SDF Network ; September 2015',
      validUntilLedgerSeq: 200,
    });
    expect(txSpy).toHaveBeenCalledOnce();
    expect(rpc.sendTransaction).toHaveBeenCalledOnce();
    expect(rpc.getTransaction).toHaveBeenCalledWith('tx-hash');
    expect((agent as unknown as { activeChannelId?: bigint }).activeChannelId).toBe(7n);
  });

  it('uses simulation-only execution for reads and decodes the result', async () => {
    const rpc = {
      getAccount: vi.fn(async () => new Account(TEST_PUBLIC, '1')),
      simulateTransaction: vi.fn(async () => simulation(xdr.ScVal.scvBool(false))),
      sendTransaction: vi.fn(),
      getTransaction: vi.fn(),
    };
    const agent = await agentWithRpc(rpc);

    await expect(agent.checkRateLimit('1.25')).resolves.toBe(false);
    expect(rpc.simulateTransaction).toHaveBeenCalledOnce();
    expect(rpc.sendTransaction).not.toHaveBeenCalled();
  });

  it('maps contract panics to a stable machine-readable code', async () => {
    const rpc = {
      getAccount: vi.fn(async () => new Account(TEST_PUBLIC, '1')),
      simulateTransaction: vi.fn(async () => ({
        id: 'simulation',
        latestLedger: 100,
        events: [],
        _parsed: true,
        error: 'contract panic: spend limit exceeded for this period',
      })),
    };
    const agent = await agentWithRpc(rpc);
    (agent as unknown as { activeChannelId?: bigint }).activeChannelId = 1n;

    const error = await agent.payForAPI({
      endpoint: 'https://api.example.com',
      amount: '2',
    }).catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('SPEND_LIMIT_EXCEEDED');
  });

  it('wraps RPC transport failures without misclassifying them as contract panics', async () => {
    const rpcError = new Error('connection refused');
    const agent = await agentWithRpc({
      getAccount: vi.fn(async () => { throw rpcError; }),
    });

    const error = await agent.getChannel(1n).catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('NETWORK_ERROR');
    expect(error.cause).toBe(rpcError);
  });

  it('decodes AgentInfo, Channel, Job, and RateLimit contract structs', async () => {
    const agent = await agentWithRpc({});
    vi.spyOn(agent as unknown as {
      invokeContract(...args: unknown[]): Promise<unknown>;
    }, 'invokeContract').mockImplementation(
      async (...args: unknown[]) => {
        const method = String(args[1]);
        const values: Record<string, unknown> = {
          get_agent: {
            address: TEST_PUBLIC,
            name: 'worker',
            owner: TEST_PUBLIC,
            active: true,
            created_at: 9,
            total_ops: 3n,
          },
          get_channel: {
            agent: TEST_PUBLIC,
            owner: TEST_PUBLIC,
            token: DEPLOYED_CONTRACTS.paymentChannel,
            limit_per_period: 50n,
            spent_this_period: 10n,
            total_spent: 20n,
            active: true,
          },
          get_job: {
            requester: TEST_PUBLIC,
            worker: TEST_PUBLIC,
            arbiter: null,
            token: DEPLOYED_CONTRACTS.escrow,
            amount: 25n,
            task_description: Buffer.from('task'),
            result: Buffer.from('done'),
            deadline_ledger: 99,
            status: ['PendingRelease'],
            created_at: 8,
          },
          get_limits: {
            max_per_tx: 10_000_000n,
            max_per_hour: 20_000_000n,
            max_per_day: 30_000_000n,
            max_txs_per_hour: 4,
            hourly_spend: 5_000_000n,
            daily_spend: 6_000_000n,
            hourly_tx_count: 2,
          },
        };
        return { value: values[method], tx: { hash: '', success: true } };
      },
    );

    await expect(agent.getAgent(2n)).resolves.toMatchObject({
      id: 2n, name: 'worker', totalOps: 3n,
    });
    await expect(agent.getChannel(3n)).resolves.toMatchObject({
      id: 3n, limitPerPeriod: 50n, totalSpent: 20n,
    });
    await expect(agent.getJob(4n)).resolves.toMatchObject({
      id: 4n, taskDescription: 'task', result: 'done', status: 'pending_release',
    });
    await expect(agent.getRateLimitStatus()).resolves.toEqual({
      maxPerTx: '1.0000000',
      maxPerHour: '2.0000000',
      maxPerDay: '3.0000000',
      maxTxsPerHour: 4,
      spentThisHour: '0.5000000',
      spentToday: '0.6000000',
      txsThisHour: 2,
    });
  });
});
