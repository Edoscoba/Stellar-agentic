import { describe, it, expect, afterEach } from 'vitest';

import {
  CONTRACT_KEYS,
  UNCONFIGURED_CONTRACTS,
  isDeployedAddress,
  envVarNames,
  resolveContracts,
  assertDeployed,
  ContractsNotDeployedError,
} from '../contracts.js';
import type { ContractAddresses, Network } from '../types/index.js';

/**
 * Valid contract IDs, strkey-encoded from fixed 32-byte payloads so they
 * carry real checksums. Nothing is deployed at them — they exist purely to
 * exercise the "looks like a real deployment" path.
 */
export const DEPLOYED: ContractAddresses = {
  agentWalletFactory: 'CAAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQC526',
  paymentChannel: 'CABAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAFNSZ',
  escrow: 'CABQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGCK3',
  rateLimiter: 'CACAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAINCW',
  circuitBreaker: 'CACQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQLC2U',
};

const NETWORKS: Network[] = ['testnet', 'mainnet', 'local'];

/** Env vars set by a test, cleared afterwards. */
const touched = new Set<string>();
function setEnv(name: string, value: string) {
  touched.add(name);
  process.env[name] = value;
}
afterEach(() => {
  for (const name of touched) delete process.env[name];
  touched.clear();
});

// ─── isDeployedAddress ───────────────────────────────────────────────────────

describe('isDeployedAddress', () => {
  it('accepts a real contract ID', () => {
    expect(isDeployedAddress(DEPLOYED.paymentChannel)).toBe(true);
  });

  it('rejects every hard-coded testnet placeholder', () => {
    // These are what DEFAULT_CONTRACTS used to ship: 60-61 characters where a
    // contract ID is 56, and no valid checksum on any of them.
    for (const key of CONTRACT_KEYS) {
      expect(isDeployedAddress(UNCONFIGURED_CONTRACTS.testnet[key])).toBe(false);
    }
  });

  it('rejects the empty mainnet and local placeholders', () => {
    for (const network of ['mainnet', 'local'] as const) {
      for (const key of CONTRACT_KEYS) {
        expect(isDeployedAddress(UNCONFIGURED_CONTRACTS[network][key])).toBe(false);
      }
    }
  });

  it.each([
    ['empty string', ''],
    ['undefined', undefined],
    ['a G-address (account, not contract)', 'GDVEU3DD4KOFECV66VIHWEZOYX4ZKR3WV27L464SIIPOU2IUI3JCZA57'],
    ['lowercase', 'caaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqcaibaeaqc526'],
    ['arbitrary text', 'not-a-contract'],
    ['whitespace', '   '],
  ])('rejects %s', (_label, value) => {
    expect(isDeployedAddress(value as string | undefined)).toBe(false);
  });

  it('rejects a valid ID with a single-character typo', () => {
    // The whole reason for checksum validation over pattern matching: a
    // fat-fingered paste is otherwise indistinguishable from a real address
    // until an RPC call fails.
    const typo = `${DEPLOYED.paymentChannel.slice(0, -1)}A`;
    expect(typo).toHaveLength(56);
    expect(isDeployedAddress(typo)).toBe(false);
  });

  it('rejects a truncated ID', () => {
    expect(isDeployedAddress(DEPLOYED.paymentChannel.slice(0, 55))).toBe(false);
  });
});

// ─── envVarNames ─────────────────────────────────────────────────────────────

describe('envVarNames', () => {
  it('produces a network-scoped name and a global fallback', () => {
    expect(envVarNames('testnet', 'paymentChannel')).toEqual([
      'STELLARAGENT_TESTNET_PAYMENT_CHANNEL',
      'STELLARAGENT_PAYMENT_CHANNEL',
    ]);
  });

  it('splits camelCase into SCREAMING_SNAKE_CASE', () => {
    expect(envVarNames('local', 'agentWalletFactory')[0])
      .toBe('STELLARAGENT_LOCAL_AGENT_WALLET_FACTORY');
    expect(envVarNames('mainnet', 'circuitBreaker')[0])
      .toBe('STELLARAGENT_MAINNET_CIRCUIT_BREAKER');
  });

  it('leaves a single-word key alone', () => {
    expect(envVarNames('testnet', 'escrow')).toEqual([
      'STELLARAGENT_TESTNET_ESCROW',
      'STELLARAGENT_ESCROW',
    ]);
  });

  it('generates a unique pair for every contract on every network', () => {
    const all = NETWORKS.flatMap((n) => CONTRACT_KEYS.map((k) => envVarNames(n, k)[0]));
    expect(new Set(all).size).toBe(all.length);
  });
});

// ─── resolveContracts ────────────────────────────────────────────────────────

describe('resolveContracts', () => {
  it('falls back to the unconfigured sentinels with no config at all', () => {
    expect(resolveContracts('testnet')).toEqual(UNCONFIGURED_CONTRACTS.testnet);
  });

  it('returns every contract key, always', () => {
    for (const network of NETWORKS) {
      expect(Object.keys(resolveContracts(network)).sort()).toEqual([...CONTRACT_KEYS].sort());
    }
  });

  it('prefers an explicit override over everything else', () => {
    setEnv('STELLARAGENT_TESTNET_PAYMENT_CHANNEL', DEPLOYED.escrow);
    const resolved = resolveContracts('testnet', { paymentChannel: DEPLOYED.paymentChannel });
    expect(resolved.paymentChannel).toBe(DEPLOYED.paymentChannel);
  });

  it('reads a network-scoped environment variable', () => {
    setEnv('STELLARAGENT_TESTNET_PAYMENT_CHANNEL', DEPLOYED.paymentChannel);
    expect(resolveContracts('testnet').paymentChannel).toBe(DEPLOYED.paymentChannel);
  });

  it('falls back to the unscoped environment variable', () => {
    setEnv('STELLARAGENT_ESCROW', DEPLOYED.escrow);
    expect(resolveContracts('testnet').escrow).toBe(DEPLOYED.escrow);
    expect(resolveContracts('local').escrow).toBe(DEPLOYED.escrow);
  });

  it('prefers the network-scoped variable over the unscoped one', () => {
    setEnv('STELLARAGENT_ESCROW', DEPLOYED.paymentChannel);
    setEnv('STELLARAGENT_TESTNET_ESCROW', DEPLOYED.escrow);
    expect(resolveContracts('testnet').escrow).toBe(DEPLOYED.escrow);
  });

  it('keeps two networks separate in the same process', () => {
    setEnv('STELLARAGENT_TESTNET_ESCROW', DEPLOYED.escrow);
    setEnv('STELLARAGENT_LOCAL_ESCROW', DEPLOYED.paymentChannel);
    expect(resolveContracts('testnet').escrow).toBe(DEPLOYED.escrow);
    expect(resolveContracts('local').escrow).toBe(DEPLOYED.paymentChannel);
  });

  it('ignores an empty environment variable', () => {
    setEnv('STELLARAGENT_TESTNET_ESCROW', '');
    expect(resolveContracts('testnet').escrow).toBe(UNCONFIGURED_CONTRACTS.testnet.escrow);
  });

  it('resolves each contract independently', () => {
    setEnv('STELLARAGENT_TESTNET_ESCROW', DEPLOYED.escrow);
    const resolved = resolveContracts('testnet');
    expect(resolved.escrow).toBe(DEPLOYED.escrow);
    expect(resolved.paymentChannel).toBe(UNCONFIGURED_CONTRACTS.testnet.paymentChannel);
  });

  it('never throws, even when nothing is configured', () => {
    // Reporting is `resolveContracts`' job; rejecting is `assertDeployed`'s.
    expect(() => resolveContracts('mainnet')).not.toThrow();
  });

  it('does not mutate the sentinel table', () => {
    const before = JSON.stringify(UNCONFIGURED_CONTRACTS);
    resolveContracts('testnet', { escrow: DEPLOYED.escrow });
    expect(JSON.stringify(UNCONFIGURED_CONTRACTS)).toBe(before);
  });
});

// ─── assertDeployed ──────────────────────────────────────────────────────────

describe('assertDeployed', () => {
  it('passes for a fully deployed set', () => {
    expect(() => assertDeployed('testnet', DEPLOYED)).not.toThrow();
  });

  it('throws ContractsNotDeployedError for the testnet placeholders', () => {
    expect(() => assertDeployed('testnet', UNCONFIGURED_CONTRACTS.testnet))
      .toThrow(ContractsNotDeployedError);
  });

  it('throws for the empty mainnet placeholders', () => {
    expect(() => assertDeployed('mainnet', UNCONFIGURED_CONTRACTS.mainnet))
      .toThrow(ContractsNotDeployedError);
  });

  it('throws when even one contract is unconfigured', () => {
    const partial = { ...DEPLOYED, rateLimiter: '' };
    expect(() => assertDeployed('local', partial)).toThrow(ContractsNotDeployedError);
  });

  it('names every missing contract, not just the first', () => {
    const partial = { ...DEPLOYED, escrow: '', rateLimiter: '' };
    try {
      assertDeployed('local', partial);
      expect.unreachable('should have thrown');
    } catch (err) {
      expect(err).toBeInstanceOf(ContractsNotDeployedError);
      expect((err as ContractsNotDeployedError).missing).toEqual(['escrow', 'rateLimiter']);
    }
  });

  it('reports missing contracts in CONTRACT_KEYS order', () => {
    const none = Object.fromEntries(
      CONTRACT_KEYS.map((k) => [k, '']),
    ) as unknown as ContractAddresses;
    try {
      assertDeployed('local', none);
      expect.unreachable('should have thrown');
    } catch (err) {
      expect((err as ContractsNotDeployedError).missing).toEqual([...CONTRACT_KEYS]);
    }
  });

  it('carries the network on the error', () => {
    try {
      assertDeployed('mainnet', UNCONFIGURED_CONTRACTS.mainnet);
      expect.unreachable('should have thrown');
    } catch (err) {
      expect((err as ContractsNotDeployedError).network).toBe('mainnet');
    }
  });

  describe('error message', () => {
    const messageFor = (network: Network, contracts: ContractAddresses) => {
      try {
        assertDeployed(network, contracts);
        return expect.unreachable('should have thrown') as never;
      } catch (err) {
        return (err as Error).message;
      }
    };

    it('points at the deployment docs, as the issue requires', () => {
      expect(messageFor('testnet', UNCONFIGURED_CONTRACTS.testnet))
        .toContain('Contracts not deployed');
      expect(messageFor('testnet', UNCONFIGURED_CONTRACTS.testnet))
        .toContain('docs/deployment.md');
    });

    it('names the network', () => {
      expect(messageFor('mainnet', UNCONFIGURED_CONTRACTS.mainnet)).toContain('"mainnet"');
    });

    it('suggests the deploy command for that network', () => {
      expect(messageFor('local', UNCONFIGURED_CONTRACTS.local))
        .toContain('pnpm deploy:contracts --network local');
    });

    it('lists the exact environment variables to set', () => {
      const message = messageFor('testnet', { ...DEPLOYED, escrow: '' });
      expect(message).toContain('STELLARAGENT_TESTNET_ESCROW=');
      // Only the missing one — not a wall of every variable.
      expect(message).not.toContain('STELLARAGENT_TESTNET_PAYMENT_CHANNEL=');
    });

    it('has a name that survives being logged', () => {
      const err = new ContractsNotDeployedError('testnet', ['escrow']);
      expect(err.name).toBe('ContractsNotDeployedError');
      expect(String(err)).toContain('ContractsNotDeployedError');
    });

    it('is a real Error subclass', () => {
      const err = new ContractsNotDeployedError('testnet', ['escrow']);
      expect(err).toBeInstanceOf(Error);
      expect(err.stack).toBeTruthy();
    });
  });
});

// ─── Regression guard ────────────────────────────────────────────────────────

describe('the placeholders that used to ship as defaults', () => {
  it('are not valid contract IDs — which is why calls against them failed', () => {
    const placeholders = Object.values(UNCONFIGURED_CONTRACTS.testnet);
    expect(placeholders).toHaveLength(5);
    // A real contract ID is exactly 56 characters. These are 60-61.
    for (const p of placeholders) {
      expect(p.length).not.toBe(56);
      expect(isDeployedAddress(p)).toBe(false);
    }
  });
});
