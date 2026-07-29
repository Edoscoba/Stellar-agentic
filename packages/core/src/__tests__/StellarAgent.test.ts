import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { Keypair } from '@stellar/stellar-sdk';

import { StellarAgent } from '../index.js';
import type { StellarAgentConfig } from '../types/index.js';

import { TEST_SECRET, TEST_PUBLIC, DEPLOYED_CONTRACTS } from './fixtures.js';

/**
 * `StellarAgent.create()` now refuses to build an agent against contracts
 * that are not deployed, so every unit test has to supply a structurally
 * valid set. This helper keeps that noise out of the individual tests.
 */
function createAgent(config: Partial<StellarAgentConfig> = {}): Promise<StellarAgent> {
  return StellarAgent.create({
    network: 'testnet',
    contracts: DEPLOYED_CONTRACTS,
    ...config,
  } as StellarAgentConfig);
}

/**
 * Stub `fetch` so no test ever reaches the real friendbot.
 * The parameter is typed `unknown` rather than `RequestInfo` because the core
 * tsconfig deliberately omits the DOM lib.
 */
function stubFetch(impl: (url: string) => Promise<Response> | Response) {
  const spy = vi.fn((input: unknown) => Promise.resolve(impl(String(input))));
  vi.stubGlobal('fetch', spy);
  return spy;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

// ─── Construction & identity ─────────────────────────────────────────────────

describe('StellarAgent.create — identity', () => {
  it('derives the public address from a supplied secret key', async () => {
    const agent = await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
    expect(agent.address).toBe(TEST_PUBLIC);
    expect(agent.address).toMatch(/^G[A-Z2-7]{55}$/);
  });

  it('exposes the same secret key it was given', async () => {
    const agent = await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
    expect(agent.secretKey).toBe(TEST_SECRET);
  });

  it('generates a fresh keypair when no secret is supplied', async () => {
    stubFetch(() => new Response(null, { status: 200 }));
    const a = await createAgent({ network: 'testnet' });
    const b = await createAgent({ network: 'testnet' });
    expect(a.address).not.toBe(b.address);
    expect(a.address).toMatch(/^G[A-Z2-7]{55}$/);
    // The generated secret must round-trip to the same address.
    expect(Keypair.fromSecret(a.secretKey).publicKey()).toBe(a.address);
  });

  it('rejects a malformed secret key', async () => {
    await expect(
      createAgent({ network: 'testnet', secretKey: 'not-a-secret' }),
    ).rejects.toThrow();
  });

  it('rejects a public key passed where a secret is expected', async () => {
    await expect(
      createAgent({ network: 'testnet', secretKey: TEST_PUBLIC }),
    ).rejects.toThrow();
  });
});

describe('StellarAgent.fromSecret', () => {
  const contracts = { contracts: DEPLOYED_CONTRACTS };

  it('restores an agent at the same address', async () => {
    const agent = await StellarAgent.fromSecret(TEST_SECRET, 'testnet', contracts);
    expect(agent.address).toBe(TEST_PUBLIC);
  });

  it('defaults to testnet', async () => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    await StellarAgent.fromSecret(TEST_SECRET, undefined, contracts);
    // A restored agent is never friendbot-funded, even on testnet — funding
    // is reserved for freshly generated keypairs.
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('accepts an explicit network', async () => {
    const agent = await StellarAgent.fromSecret(TEST_SECRET, 'mainnet', contracts);
    expect(agent.address).toBe(TEST_PUBLIC);
  });

  it('forwards contracts through to create()', async () => {
    // Without this passthrough a restored agent could only ever reach
    // contracts resolved from the environment.
    const agent = await StellarAgent.fromSecret(TEST_SECRET, 'testnet', contracts);
    expect((agent as unknown as { contracts: Record<string, string> }).contracts)
      .toEqual(DEPLOYED_CONTRACTS);
  });

  it('forwards allowUnconfiguredContracts', async () => {
    await expect(
      StellarAgent.fromSecret(TEST_SECRET, 'mainnet', { allowUnconfiguredContracts: true }),
    ).resolves.toBeInstanceOf(StellarAgent);
  });

  it('fails fast when given neither contracts nor the escape hatch', async () => {
    await expect(StellarAgent.fromSecret(TEST_SECRET, 'mainnet'))
      .rejects.toThrow(/Contracts not deployed/);
  });
});

// ─── Friendbot funding ───────────────────────────────────────────────────────

describe('friendbot funding', () => {
  it('funds a fresh testnet keypair', async () => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    const agent = await createAgent({ network: 'testnet' });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(fetchSpy.mock.calls[0]![0]).toBe(
      `https://friendbot.stellar.org?addr=${agent.address}`,
    );
  });

  it('does not fund when a secret key is supplied', async () => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it.each(['mainnet', 'local'] as const)('does not fund on %s', async (network) => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    await createAgent({ network });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('does not reject when friendbot returns an error status', async () => {
    // An already-funded account is a normal, non-fatal outcome.
    stubFetch(() => new Response(null, { status: 400 }));
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await expect(createAgent({ network: 'testnet' })).resolves.toBeInstanceOf(StellarAgent);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('Friendbot funding failed'));
  });

  it('does not reject when friendbot is unreachable', async () => {
    stubFetch(() => {
      throw new Error('ENOTFOUND');
    });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await expect(createAgent({ network: 'testnet' })).resolves.toBeInstanceOf(StellarAgent);
    expect(warn).toHaveBeenCalledWith('Could not reach friendbot');
  });
});

// ─── Contract address resolution ─────────────────────────────────────────────

describe('contract address resolution', () => {
  /** Reach the private `contracts` field — there is no public accessor yet. */
  const contractsOf = (agent: StellarAgent) =>
    (agent as unknown as { contracts: Record<string, string> }).contracts;

  it('populates all five contract slots', async () => {
    const agent = await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
    expect(Object.keys(contractsOf(agent)).sort()).toEqual([
      'agentWalletFactory',
      'circuitBreaker',
      'escrow',
      'paymentChannel',
      'rateLimiter',
    ]);
  });

  it('lets an explicit override replace a single address', async () => {
    const custom = 'CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC';
    const agent = await createAgent({
      network: 'testnet',
      secretKey: TEST_SECRET,
      contracts: { ...DEPLOYED_CONTRACTS, paymentChannel: custom },
    });
    expect(contractsOf(agent).paymentChannel).toBe(custom);
    expect(contractsOf(agent).escrow).toBe(DEPLOYED_CONTRACTS.escrow);
  });

  it('does not share contract state between instances', async () => {
    const custom = 'CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC';
    const a = await createAgent({
      network: 'testnet',
      secretKey: TEST_SECRET,
      contracts: { ...DEPLOYED_CONTRACTS, escrow: custom },
    });
    const b = await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
    expect(contractsOf(a).escrow).toBe(custom);
    expect(contractsOf(b).escrow).toBe(DEPLOYED_CONTRACTS.escrow);
  });
});

// ─── Fast-fail on undeployed contracts ───────────────────────────────────────

describe('deployed-contracts check', () => {
  it('refuses to create an agent against the testnet placeholders', async () => {
    // The whole point: previously this succeeded and every later contract
    // call failed with an opaque RPC error instead.
    await expect(
      StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET }),
    ).rejects.toThrow(/Contracts not deployed for network "testnet"/);
  });

  it.each(['mainnet', 'local'] as const)(
    'refuses to create an agent on %s with no configuration',
    async (network) => {
      await expect(
        StellarAgent.create({ network, secretKey: TEST_SECRET }),
      ).rejects.toThrow(/Contracts not deployed/);
    },
  );

  it('points at the deployment runbook', async () => {
    await expect(
      StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET }),
    ).rejects.toThrow(/docs\/deployment\.md/);
  });

  it('rejects a partially configured set', async () => {
    await expect(
      StellarAgent.create({
        network: 'testnet',
        secretKey: TEST_SECRET,
        contracts: { ...DEPLOYED_CONTRACTS, escrow: '' },
      }),
    ).rejects.toThrow(/escrow/);
  });

  it('rejects an address with a single-character typo', async () => {
    await expect(
      StellarAgent.create({
        network: 'testnet',
        secretKey: TEST_SECRET,
        contracts: {
          ...DEPLOYED_CONTRACTS,
          escrow: `${DEPLOYED_CONTRACTS.escrow.slice(0, -1)}A`,
        },
      }),
    ).rejects.toThrow(/Contracts not deployed/);
  });

  it('succeeds with a fully deployed set', async () => {
    await expect(
      StellarAgent.create({
        network: 'testnet',
        secretKey: TEST_SECRET,
        contracts: DEPLOYED_CONTRACTS,
      }),
    ).resolves.toBeInstanceOf(StellarAgent);
  });

  it('resolves addresses from environment variables', async () => {
    const vars = {
      STELLARAGENT_TESTNET_AGENT_WALLET_FACTORY: DEPLOYED_CONTRACTS.agentWalletFactory,
      STELLARAGENT_TESTNET_PAYMENT_CHANNEL: DEPLOYED_CONTRACTS.paymentChannel,
      STELLARAGENT_TESTNET_ESCROW: DEPLOYED_CONTRACTS.escrow,
      STELLARAGENT_TESTNET_RATE_LIMITER: DEPLOYED_CONTRACTS.rateLimiter,
      STELLARAGENT_TESTNET_CIRCUIT_BREAKER: DEPLOYED_CONTRACTS.circuitBreaker,
    };
    Object.assign(process.env, vars);
    try {
      const agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
      expect((agent as unknown as { contracts: unknown }).contracts).toEqual(DEPLOYED_CONTRACTS);
    } finally {
      for (const name of Object.keys(vars)) delete process.env[name];
    }
  });

  it('can be bypassed for read-only use', async () => {
    // getBalance() touches no contract, so an unconfigured agent is still
    // useful for it — but only when the caller opts in explicitly.
    const agent = await StellarAgent.create({
      network: 'testnet',
      secretKey: TEST_SECRET,
      allowUnconfiguredContracts: true,
    });
    expect(agent.address).toBe(TEST_PUBLIC);
  });

  it('still fails contract calls when the check is bypassed', async () => {
    const agent = await StellarAgent.create({
      network: 'testnet',
      secretKey: TEST_SECRET,
      allowUnconfiguredContracts: true,
    });
    vi.spyOn(console, 'log').mockImplementation(() => {});
    await expect(
      agent.openChannel({ deposit: '10', limitPerPeriod: '1', period: 'hourly' }),
    ).rejects.toThrow();
  });
});

// ─── Network configuration ───────────────────────────────────────────────────

describe('network configuration', () => {
  const networkOf = (agent: StellarAgent) =>
    (agent as unknown as { networkConfig: { networkPassphrase: string; horizonUrl: string } })
      .networkConfig;

  it.each([
    ['testnet', 'Test SDF Network ; September 2015'],
    ['mainnet', 'Public Global Stellar Network ; September 2015'],
    ['local', 'Standalone Network ; February 2017'],
  ] as const)('selects the %s passphrase', async (network, passphrase) => {
    const agent = await createAgent({ network, secretKey: TEST_SECRET });
    expect(networkOf(agent).networkPassphrase).toBe(passphrase);
  });

  const horizonOf = (agent: StellarAgent) =>
    (agent as unknown as { horizon: { serverURL: { toString(): string } } }).horizon;

  it('constructs against the plain-HTTP local Horizon without throwing', async () => {
    // Horizon.Server rejects http:// unless allowHttp is set, so before the
    // loopback exemption this call threw "Cannot connect to insecure horizon
    // server" and the local network was unusable — including for the
    // standalone-network integration tests.
    const agent = await createAgent({ network: 'local', secretKey: TEST_SECRET });
    expect(horizonOf(agent).serverURL.toString()).toContain('localhost:8000');
  });

  it.each(['testnet', 'mainnet'] as const)(
    'uses an https Horizon endpoint on %s',
    async (network) => {
      const agent = await createAgent({ network, secretKey: TEST_SECRET });
      expect(networkOf(agent).horizonUrl.startsWith('https://')).toBe(true);
    },
  );

  it('refuses a non-loopback plain-HTTP Horizon endpoint', async () => {
    // The exemption is loopback-only: a plaintext LAN or public endpoint must
    // still fail loudly rather than transmit signed transactions in the clear.
    const { NETWORK_CONFIGS } = await import('../types/index.js');
    const original = NETWORK_CONFIGS.local.horizonUrl;
    NETWORK_CONFIGS.local.horizonUrl = 'http://horizon.example.com';
    try {
      await expect(
        createAgent({ network: 'local', secretKey: TEST_SECRET }),
      ).rejects.toThrow(/insecure horizon server/);
    } finally {
      NETWORK_CONFIGS.local.horizonUrl = original;
    }
  });
});

// ─── payForAPI validation ────────────────────────────────────────────────────

describe('payForAPI — validation guards', () => {
  /**
   * `activeChannelId` is private and only ever set by `openChannel()`, which
   * is still a stub. Setting it directly is the only way to reach the
   * argument-validation branch underneath the channel guard.
   */
  function withActiveChannel(agent: StellarAgent): StellarAgent {
    (agent as unknown as { activeChannelId?: bigint }).activeChannelId = 1n;
    return agent;
  }

  let agent: StellarAgent;
  beforeEach(async () => {
    agent = await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
  });

  it('refuses to pay with no open channel', async () => {
    await expect(
      agent.payForAPI({ endpoint: 'https://api.example.com', amount: '0.001' }),
    ).rejects.toThrow('No active payment channel. Call openChannel() first.');
  });

  it('checks the channel before validating arguments', async () => {
    // Even a malformed cross-asset request reports the missing channel first.
    await expect(
      agent.payForAPI({ endpoint: 'https://api.example.com', amount: '0.001', destAsset: 'XLM' }),
    ).rejects.toThrow('No active payment channel');
  });

  it('rejects destAsset without minReceived', async () => {
    withActiveChannel(agent);
    await expect(
      agent.payForAPI({
        endpoint: 'https://api.example.com',
        amount: '0.001',
        asset: 'USDC',
        destAsset: 'XLM',
      }),
    ).rejects.toThrow('destAsset and minReceived must be set together');
  });

  it('rejects minReceived without destAsset', async () => {
    withActiveChannel(agent);
    await expect(
      agent.payForAPI({
        endpoint: 'https://api.example.com',
        amount: '0.001',
        asset: 'USDC',
        minReceived: '0.009',
      }),
    ).rejects.toThrow('destAsset and minReceived must be set together');
  });

  it('routes a complete cross-asset request through pay_with_conversion', async () => {
    withActiveChannel(agent);
    const invoke = vi.spyOn(
      agent as unknown as { invokeContract: (...args: unknown[]) => Promise<unknown> },
      'invokeContract',
    ).mockResolvedValue({ value: 90_000n, tx: { hash: 'abc', success: true } });
    await expect(agent.payForAPI({
      endpoint: 'https://api.example.com',
      amount: '0.001',
      asset: 'USDC',
      destAsset: 'XLM',
      minReceived: '0.009',
    })).resolves.toEqual({ hash: 'abc', success: true });
    expect(invoke).toHaveBeenCalledWith(
      DEPLOYED_CONTRACTS.paymentChannel,
      'pay_with_conversion',
      expect.any(Array),
    );
  });

  it('routes a same-asset request through pay', async () => {
    withActiveChannel(agent);
    const invoke = vi.spyOn(
      agent as unknown as { invokeContract: (...args: unknown[]) => Promise<unknown> },
      'invokeContract',
    ).mockResolvedValue({ value: undefined, tx: { hash: 'def', success: true } });
    await expect(agent.payForAPI({
      endpoint: 'https://api.example.com',
      amount: '0.001',
      asset: 'USDC',
    })).resolves.toEqual({ hash: 'def', success: true });
    expect(invoke).toHaveBeenCalledWith(
      DEPLOYED_CONTRACTS.paymentChannel,
      'pay',
      expect.any(Array),
    );
  });
});

// ─── getBalance ──────────────────────────────────────────────────────────────

describe('getBalance', () => {
  let agent: StellarAgent;
  beforeEach(async () => {
    agent = await createAgent({ network: 'testnet', secretKey: TEST_SECRET });
  });

  /** Replace the Horizon server with a controllable stub. */
  function stubHorizon(loadAccount: (address: string) => Promise<unknown>) {
    (agent as unknown as { horizon: unknown }).horizon = { loadAccount: vi.fn(loadAccount) };
  }

  it('returns the native XLM balance', async () => {
    stubHorizon(async () => ({
      balances: [
        { asset_type: 'credit_alphanum4', asset_code: 'USDC', balance: '250.0000000' },
        { asset_type: 'native', balance: '1234.5670000' },
      ],
    }));
    expect(await agent.getBalance()).toBe('1234.5670000');
  });

  it('returns "0" when the account holds no native balance entry', async () => {
    stubHorizon(async () => ({
      balances: [{ asset_type: 'credit_alphanum4', asset_code: 'USDC', balance: '250.0000000' }],
    }));
    expect(await agent.getBalance()).toBe('0');
  });

  it('returns "0" for an account with no balances at all', async () => {
    stubHorizon(async () => ({ balances: [] }));
    expect(await agent.getBalance()).toBe('0');
  });

  it('returns "0" rather than throwing when the account does not exist', async () => {
    stubHorizon(async () => {
      throw new Error('Request failed with status code 404');
    });
    expect(await agent.getBalance()).toBe('0');
  });

  it('queries Horizon for its own address', async () => {
    const spy = vi.fn(async () => ({ balances: [{ asset_type: 'native', balance: '1' }] }));
    stubHorizon(spy);
    await agent.getBalance();
    expect(spy).toHaveBeenCalledWith(TEST_PUBLIC);
  });

  it('needs no signing — it is a read-only Horizon query', async () => {
    // Guards the property that balance reads never touch key material, which
    // the remote-signer work depends on.
    const secret = vi.spyOn(
      Object.getPrototypeOf(agent) as object,
      'secretKey' as never,
      'get',
    );
    stubHorizon(async () => ({ balances: [{ asset_type: 'native', balance: '5' }] }));
    await agent.getBalance();
    expect(secret).not.toHaveBeenCalled();
  });
});
