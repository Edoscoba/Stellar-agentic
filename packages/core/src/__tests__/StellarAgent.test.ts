import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { Keypair } from '@stellar/stellar-sdk';

import { StellarAgent } from '../index.js';

// A deterministic, well-formed test keypair, derived from an all-0x07 ed25519
// seed so the assertions below are reproducible. It holds nothing, is never
// funded on any real network, and must never be used outside these tests.
const TEST_SECRET = 'SADQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQP54X';
const TEST_PUBLIC = Keypair.fromSecret(TEST_SECRET).publicKey();

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
    const agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
    expect(agent.address).toBe(TEST_PUBLIC);
    expect(agent.address).toMatch(/^G[A-Z2-7]{55}$/);
  });

  it('exposes the same secret key it was given', async () => {
    const agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
    expect(agent.secretKey).toBe(TEST_SECRET);
  });

  it('generates a fresh keypair when no secret is supplied', async () => {
    stubFetch(() => new Response(null, { status: 200 }));
    const a = await StellarAgent.create({ network: 'testnet' });
    const b = await StellarAgent.create({ network: 'testnet' });
    expect(a.address).not.toBe(b.address);
    expect(a.address).toMatch(/^G[A-Z2-7]{55}$/);
    // The generated secret must round-trip to the same address.
    expect(Keypair.fromSecret(a.secretKey).publicKey()).toBe(a.address);
  });

  it('rejects a malformed secret key', async () => {
    await expect(
      StellarAgent.create({ network: 'testnet', secretKey: 'not-a-secret' }),
    ).rejects.toThrow();
  });

  it('rejects a public key passed where a secret is expected', async () => {
    await expect(
      StellarAgent.create({ network: 'testnet', secretKey: TEST_PUBLIC }),
    ).rejects.toThrow();
  });
});

describe('StellarAgent.fromSecret', () => {
  it('restores an agent at the same address', async () => {
    const agent = await StellarAgent.fromSecret(TEST_SECRET);
    expect(agent.address).toBe(TEST_PUBLIC);
  });

  it('defaults to testnet', async () => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    await StellarAgent.fromSecret(TEST_SECRET);
    // A restored agent is never friendbot-funded, even on testnet — funding
    // is reserved for freshly generated keypairs.
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('accepts an explicit network', async () => {
    const agent = await StellarAgent.fromSecret(TEST_SECRET, 'mainnet');
    expect(agent.address).toBe(TEST_PUBLIC);
  });
});

// ─── Friendbot funding ───────────────────────────────────────────────────────

describe('friendbot funding', () => {
  it('funds a fresh testnet keypair', async () => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    const agent = await StellarAgent.create({ network: 'testnet' });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(fetchSpy.mock.calls[0]![0]).toBe(
      `https://friendbot.stellar.org?addr=${agent.address}`,
    );
  });

  it('does not fund when a secret key is supplied', async () => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it.each(['mainnet', 'local'] as const)('does not fund on %s', async (network) => {
    const fetchSpy = stubFetch(() => new Response(null, { status: 200 }));
    await StellarAgent.create({ network });
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('does not reject when friendbot returns an error status', async () => {
    // An already-funded account is a normal, non-fatal outcome.
    stubFetch(() => new Response(null, { status: 400 }));
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await expect(StellarAgent.create({ network: 'testnet' })).resolves.toBeInstanceOf(StellarAgent);
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('Friendbot funding failed'));
  });

  it('does not reject when friendbot is unreachable', async () => {
    stubFetch(() => {
      throw new Error('ENOTFOUND');
    });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await expect(StellarAgent.create({ network: 'testnet' })).resolves.toBeInstanceOf(StellarAgent);
    expect(warn).toHaveBeenCalledWith('Could not reach friendbot');
  });
});

// ─── Contract address resolution ─────────────────────────────────────────────

describe('contract address resolution', () => {
  /** Reach the private `contracts` field — there is no public accessor yet. */
  const contractsOf = (agent: StellarAgent) =>
    (agent as unknown as { contracts: Record<string, string> }).contracts;

  it('populates all five contract slots from the network defaults', async () => {
    const agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
    expect(Object.keys(contractsOf(agent)).sort()).toEqual([
      'agentWalletFactory',
      'circuitBreaker',
      'escrow',
      'paymentChannel',
      'rateLimiter',
    ]);
  });

  it('lets an explicit override replace a single default', async () => {
    const custom = 'CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC';
    const agent = await StellarAgent.create({
      network: 'testnet',
      secretKey: TEST_SECRET,
      contracts: { paymentChannel: custom },
    });
    expect(contractsOf(agent).paymentChannel).toBe(custom);
    // The other four keep their defaults.
    expect(contractsOf(agent).escrow).not.toBe(custom);
  });

  it('lets overrides replace every slot', async () => {
    const all = {
      agentWalletFactory: 'C1',
      paymentChannel: 'C2',
      escrow: 'C3',
      rateLimiter: 'C4',
      circuitBreaker: 'C5',
    };
    const agent = await StellarAgent.create({
      network: 'testnet',
      secretKey: TEST_SECRET,
      contracts: all,
    });
    expect(contractsOf(agent)).toEqual(all);
  });

  it('does not share contract state between instances', async () => {
    const a = await StellarAgent.create({
      network: 'testnet',
      secretKey: TEST_SECRET,
      contracts: { escrow: 'CUSTOM_A' },
    });
    const b = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
    expect(contractsOf(a).escrow).toBe('CUSTOM_A');
    expect(contractsOf(b).escrow).not.toBe('CUSTOM_A');
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
    const agent = await StellarAgent.create({ network, secretKey: TEST_SECRET });
    expect(networkOf(agent).networkPassphrase).toBe(passphrase);
  });

  const horizonOf = (agent: StellarAgent) =>
    (agent as unknown as { horizon: { serverURL: { toString(): string } } }).horizon;

  it('constructs against the plain-HTTP local Horizon without throwing', async () => {
    // Horizon.Server rejects http:// unless allowHttp is set, so before the
    // loopback exemption this call threw "Cannot connect to insecure horizon
    // server" and the local network was unusable — including for the
    // standalone-network integration tests.
    const agent = await StellarAgent.create({ network: 'local', secretKey: TEST_SECRET });
    expect(horizonOf(agent).serverURL.toString()).toContain('localhost:8000');
  });

  it.each(['testnet', 'mainnet'] as const)(
    'uses an https Horizon endpoint on %s',
    async (network) => {
      const agent = await StellarAgent.create({ network, secretKey: TEST_SECRET });
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
        StellarAgent.create({ network: 'local', secretKey: TEST_SECRET }),
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
    agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
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

  it('accepts both together, falling through to the unimplemented invocation', async () => {
    withActiveChannel(agent);
    vi.spyOn(console, 'log').mockImplementation(() => {});
    await expect(
      agent.payForAPI({
        endpoint: 'https://api.example.com',
        amount: '0.001',
        asset: 'USDC',
        destAsset: 'XLM',
        minReceived: '0.009',
      }),
    ).rejects.toThrow('Not yet implemented');
  });

  it('accepts neither, falling through to the unimplemented invocation', async () => {
    withActiveChannel(agent);
    vi.spyOn(console, 'log').mockImplementation(() => {});
    await expect(
      agent.payForAPI({ endpoint: 'https://api.example.com', amount: '0.001', asset: 'USDC' }),
    ).rejects.toThrow('Not yet implemented');
  });
});

// ─── getBalance ──────────────────────────────────────────────────────────────

describe('getBalance', () => {
  let agent: StellarAgent;
  beforeEach(async () => {
    agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
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

// ─── Unimplemented surface ───────────────────────────────────────────────────

describe('unimplemented contract methods', () => {
  let agent: StellarAgent;
  beforeEach(async () => {
    agent = await StellarAgent.create({ network: 'testnet', secretKey: TEST_SECRET });
    vi.spyOn(console, 'log').mockImplementation(() => {});
  });

  // These all await real Soroban invocation. The assertions pin the current
  // contract — a clear throw, never a silent no-op or an undefined return —
  // so that implementing them is a deliberate, test-visible change.
  it('openChannel throws a descriptive error', async () => {
    await expect(
      agent.openChannel({ deposit: '10', limitPerPeriod: '1', period: 'hourly' }),
    ).rejects.toThrow(/Not yet implemented/);
  });

  it('requestWork throws a descriptive error', async () => {
    await expect(
      agent.requestWork({ workerAgent: TEST_PUBLIC, task: 'summarise', escrowAmount: '0.05' }),
    ).rejects.toThrow(/Not yet implemented/);
  });

  it.each([
    ['acceptJob', () => agent.acceptJob(1n)],
    ['submitResult', () => agent.submitResult(1n, 'done')],
    ['releasePayment', () => agent.releasePayment(1n)],
    ['setRateLimits', () =>
      agent.setRateLimits({ maxPerTx: '1', maxPerHour: '10', maxPerDay: '100', maxTxsPerHour: 5 })],
    ['checkRateLimit', () => agent.checkRateLimit('1')],
    ['getSpendReport', () => agent.getSpendReport()],
    ['getChannel', () => agent.getChannel(1n)],
    ['getJob', () => agent.getJob(1n)],
    ['getRateLimitStatus', () => agent.getRateLimitStatus()],
  ])('%s rejects rather than returning undefined', async (_name, call) => {
    await expect(call()).rejects.toThrow(/Not yet implemented/);
  });
});
