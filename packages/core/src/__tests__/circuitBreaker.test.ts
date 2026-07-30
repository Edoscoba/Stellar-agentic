import { describe, expect, it, vi } from 'vitest';
import {
  Account,
  Address,
  SorobanDataBuilder,
  SorobanRpc,
  xdr,
} from '@stellar/stellar-sdk';

import {
  CircuitBreaker,
  asPublicAddress,
} from '../circuitBreaker.js';
import { StellarAgentError } from '../errors.js';
import { KeypairSigner } from '../signer.js';
import type { Signer } from '../signer.js';
import { DEPLOYED_CONTRACTS, TEST_PUBLIC, TEST_SECRET } from './fixtures.js';

function addressAuthEntry(): xdr.SorobanAuthorizationEntry {
  const invokeArgs = new xdr.InvokeContractArgs({
    contractAddress: Address.fromString(DEPLOYED_CONTRACTS.circuitBreaker).toScAddress(),
    functionName: 'propose_pause',
    args: [Address.fromString(TEST_PUBLIC).toScVal()],
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

function mockRpc(overrides: Record<string, unknown> = {}) {
  return {
    getAccount: vi.fn(async () => new Account(TEST_PUBLIC, '1')),
    simulateTransaction: vi.fn(async () => simulation(xdr.ScVal.scvVoid(), [addressAuthEntry()])),
    sendTransaction: vi.fn(async () => ({ status: 'PENDING', hash: 'cb-tx-hash' })),
    getTransaction: vi.fn(async () => ({
      status: SorobanRpc.Api.GetTransactionStatus.SUCCESS,
      ledger: 101,
    })),
    ...overrides,
  };
}

function breakerWithRpc(rpc: Record<string, unknown>, signer?: Signer) {
  return new CircuitBreaker({
    rpcUrl: 'http://example.invalid',
    contractId: DEPLOYED_CONTRACTS.circuitBreaker,
    networkPassphrase: 'Test SDF Network ; September 2015',
    rpc: rpc as unknown as SorobanRpc.Server,
    signer,
  });
}

describe('CircuitBreaker', () => {
  it('rejects malformed contract IDs at construction', () => {
    expect(() => new CircuitBreaker({
      rpcUrl: 'http://example.invalid',
      contractId: 'not-a-contract',
    })).toThrow(StellarAgentError);
  });

  it('targets the configured contract ID, not a zero address', async () => {
    const rpc = mockRpc();
    const breaker = breakerWithRpc(rpc);

    await breaker.proposePause(TEST_SECRET);

    const simulatedTx = rpc.simulateTransaction.mock.calls[0]![0] as unknown as {
      operations: Array<{
        func: { invokeContract(): { contractAddress(): xdr.ScAddress; functionName(): Buffer } };
      }>;
    };
    const invoke = simulatedTx.operations[0].func.invokeContract();
    expect(Address.fromScAddress(invoke.contractAddress()).toString())
      .toBe(DEPLOYED_CONTRACTS.circuitBreaker);
    expect(invoke.functionName().toString()).toBe('propose_pause');
  });

  it('simulates, signs auth entries, submits, polls, and returns TxResult', async () => {
    const auth = addressAuthEntry();
    const rpc = mockRpc({
      simulateTransaction: vi.fn(async () => simulation(xdr.ScVal.scvVoid(), [auth])),
    });
    const breaker = breakerWithRpc(rpc);
    const authSpy = vi.spyOn(KeypairSigner.prototype, 'signAuthEntry').mockImplementation(
      async (entry: string) => entry,
    );

    await expect(breaker.proposePause(TEST_SECRET)).resolves.toEqual({
      hash: 'cb-tx-hash',
      success: true,
      ledger: 101,
    });

    expect(rpc.simulateTransaction).toHaveBeenCalledOnce();
    expect(authSpy).toHaveBeenCalledOnce();
    expect(rpc.sendTransaction).toHaveBeenCalledOnce();
    expect(rpc.getTransaction).toHaveBeenCalledWith('cb-tx-hash');
    authSpy.mockRestore();
  });

  it('uses a default instance signer when no argument is passed', async () => {
    const rpc = mockRpc();
    const signer = new KeypairSigner(
      (await import('@stellar/stellar-sdk')).Keypair.fromSecret(TEST_SECRET),
    );
    const breaker = breakerWithRpc(rpc, signer);

    await expect(breaker.executePause()).resolves.toMatchObject({ success: true });
    expect(rpc.getAccount).toHaveBeenCalledWith(TEST_PUBLIC);
  });

  it('uses simulation-only reads for isPaused and never submits', async () => {
    const rpc = mockRpc({
      simulateTransaction: vi.fn(async () => simulation(xdr.ScVal.scvBool(true))),
    });
    const breaker = breakerWithRpc(rpc);

    await expect(breaker.isPaused(TEST_PUBLIC)).resolves.toBe(true);
    expect(rpc.simulateTransaction).toHaveBeenCalledOnce();
    expect(rpc.sendTransaction).not.toHaveBeenCalled();
  });

  it('reads pause_quorum_count via simulation', async () => {
    const rpc = mockRpc({
      simulateTransaction: vi.fn(async (_tx: unknown) => {
        const tx = _tx as { operations: Array<{ func: { invokeContract(): { functionName(): Buffer } } }> };
        const method = tx.operations[0].func.invokeContract().functionName().toString();
        expect(method).toBe('pause_quorum_count');
        return simulation(xdr.ScVal.scvU32(3));
      }),
    });
    const breaker = breakerWithRpc(rpc);

    await expect(breaker.pauseQuorumCount(TEST_PUBLIC)).resolves.toBe(3);
  });

  it('maps contract panics to stable StellarAgentError codes', async () => {
    const rpc = mockRpc({
      simulateTransaction: vi.fn(async () => ({
        id: 'simulation',
        latestLedger: 1,
        events: [],
        _parsed: true,
        error: 'contract panic: not a trusted node',
      })),
    });
    const breaker = breakerWithRpc(rpc);

    const error = await breaker.proposePause(TEST_SECRET).catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('NOT_AUTHORIZED');
    expect(rpc.sendTransaction).not.toHaveBeenCalled();
  });

  it('throws when submission is rejected', async () => {
    const rpc = mockRpc({
      sendTransaction: vi.fn(async () => ({ status: 'ERROR' })),
    });
    const breaker = breakerWithRpc(rpc);

    const error = await breaker.executePause(TEST_SECRET).catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('SUBMISSION_FAILED');
    expect(rpc.getTransaction).not.toHaveBeenCalled();
  });

  it('rejects invalid secret keys before hitting the network', async () => {
    const rpc = mockRpc();
    const breaker = breakerWithRpc(rpc);

    const error = await breaker.proposePause('not-a-secret').catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('INVALID_ARGUMENT');
    expect(rpc.getAccount).not.toHaveBeenCalled();
  });

  it('wraps RPC transport failures as NETWORK_ERROR', async () => {
    const breaker = breakerWithRpc({
      getAccount: vi.fn(async () => { throw new Error('connection refused'); }),
    });

    const error = await breaker.isPaused(TEST_PUBLIC).catch((caught) => caught);
    expect(error).toBeInstanceOf(StellarAgentError);
    expect(error.code).toBe('NETWORK_ERROR');
  });
});

describe('asPublicAddress', () => {
  it('accepts valid public keys', () => {
    expect(asPublicAddress(TEST_PUBLIC)).toBe(TEST_PUBLIC);
  });

  it('rejects secret keys', () => {
    expect(() => asPublicAddress(TEST_SECRET)).toThrow(StellarAgentError);
    expect(() => asPublicAddress(TEST_SECRET)).toThrow(/Secret keys must not/);
  });

  it('rejects malformed values', () => {
    expect(() => asPublicAddress('not-an-address')).toThrow(/valid Stellar public address/);
  });
});
