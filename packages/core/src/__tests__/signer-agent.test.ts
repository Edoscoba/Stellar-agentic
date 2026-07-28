import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { Keypair, Networks } from '@stellar/stellar-sdk';

import { StellarAgent } from '../index.js';
import {
  KeypairSigner,
  RemoteSigner,
  SigningError,
  type Signer,
  type SignTransactionOptions,
  type SignAuthEntryOptions,
} from '../signer.js';
import { TEST_SECRET, TEST_PUBLIC, DEPLOYED_CONTRACTS } from './fixtures.js';

// ─── A mock remote signer ────────────────────────────────────────────────────

/**
 * A Signer that behaves exactly like a remote service: it produces real,
 * verifiable signatures, but the agent under test never holds the secret.
 *
 * The key is captured in a module-local closure here to model "the key lives
 * somewhere else". Nothing the agent can reach refers to it.
 */
function createMockRemoteSigner(secret = TEST_SECRET) {
  const keypair = Keypair.fromSecret(secret);
  const delegate = new KeypairSigner(keypair);

  const calls = {
    getPublicKey: 0,
    signTransaction: [] as { xdr: string; options: SignTransactionOptions }[],
    signAuthEntry: [] as { xdr: string; options: SignAuthEntryOptions }[],
  };

  const signer: Signer = {
    async getPublicKey() {
      calls.getPublicKey++;
      return keypair.publicKey();
    },
    async signTransaction(xdr, options) {
      calls.signTransaction.push({ xdr, options });
      return delegate.signTransaction(xdr, options);
    },
    async signAuthEntry(xdr, options) {
      calls.signAuthEntry.push({ xdr, options });
      return delegate.signAuthEntry(xdr, options);
    },
  };

  return { signer, calls, publicKey: keypair.publicKey() };
}

const createWithSigner = (signer: Signer, overrides = {}) =>
  StellarAgent.create({
    network: 'testnet',
    contracts: DEPLOYED_CONTRACTS,
    signer,
    ...overrides,
  });

const createWithSecret = (secretKey = TEST_SECRET, overrides = {}) =>
  StellarAgent.create({
    network: 'testnet',
    contracts: DEPLOYED_CONTRACTS,
    secretKey,
    ...overrides,
  });

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

// ─── The headline requirement ────────────────────────────────────────────────

describe('StellarAgent.create({ signer })', () => {
  it('creates an agent without config.secretKey ever being passed', async () => {
    const { signer } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    expect(agent.address).toBe(TEST_PUBLIC);
  });

  it('derives the address through the signer, not from a local secret', async () => {
    const { signer, calls } = createMockRemoteSigner();
    await createWithSigner(signer);
    expect(calls.getPublicKey).toBe(1);
  });

  it('resolves the address once and caches it', async () => {
    // `address` stays a synchronous getter even though derivation is async.
    const { signer, calls } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    agent.address;
    agent.address;
    agent.address;
    expect(calls.getPublicKey).toBe(1);
  });

  it('reports that it holds no secret key', async () => {
    const { signer } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    expect(agent.holdsSecretKey).toBe(false);
  });

  it('throws rather than inventing a secret key to return', async () => {
    const { signer } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    expect(() => agent.secretKey).toThrow(SigningError);
    expect(() => agent.secretKey).toThrow(/no secret key to expose/);
  });

  it('rejects passing both a signer and a secretKey', async () => {
    const { signer } = createMockRemoteSigner();
    await expect(
      StellarAgent.create({
        network: 'testnet',
        contracts: DEPLOYED_CONTRACTS,
        signer,
        secretKey: TEST_SECRET,
      }),
    ).rejects.toThrow(/pass either `signer` or `secretKey`, not both/);
  });

  it('never friendbot-funds a signer-backed agent', async () => {
    // Funding is for freshly generated keypairs only; an external signer is
    // assumed to already have a funded account.
    const fetchSpy = vi.fn(async () => new Response(null, { status: 200 }));
    vi.stubGlobal('fetch', fetchSpy);
    const { signer } = createMockRemoteSigner();
    await createWithSigner(signer);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it('accepts any duck-typed Signer, not just the built-in classes', async () => {
    const keypair = Keypair.fromSecret(TEST_SECRET);
    const minimal: Signer = {
      getPublicKey: async () => keypair.publicKey(),
      signTransaction: async (xdr) => xdr,
      signAuthEntry: async (xdr) => xdr,
    };
    const agent = await createWithSigner(minimal);
    expect(agent.address).toBe(TEST_PUBLIC);
  });

  it('surfaces a signer that cannot report its address', async () => {
    const broken: Signer = {
      getPublicKey: async () => {
        throw new SigningError('signing service unreachable');
      },
      signTransaction: async (xdr) => xdr,
      signAuthEntry: async (xdr) => xdr,
    };
    await expect(createWithSigner(broken)).rejects.toThrow(/signing service unreachable/);
  });
});

// ─── The secret must not be reachable from the agent ─────────────────────────

/**
 * Walk an object graph — own properties, prototypes, arrays, Maps, Sets —
 * looking for a string. Reports the path so a failure is diagnosable.
 *
 * `#private` fields are genuinely unreachable this way, which is the point:
 * this asserts the secret is not sitting on any field an error reporter, a
 * structured logger, or a heap-walking exploit would traverse.
 */
function findString(root: unknown, needle: string, maxDepth = 8): string | null {
  const seen = new WeakSet<object>();

  function walk(value: unknown, path: string, depth: number): string | null {
    if (depth > maxDepth || value == null) return null;

    if (typeof value === 'string') return value.includes(needle) ? path : null;
    if (typeof value !== 'object' && typeof value !== 'function') return null;
    if (seen.has(value as object)) return null;
    seen.add(value as object);

    if (Array.isArray(value)) {
      for (let i = 0; i < value.length; i++) {
        const hit = walk(value[i], `${path}[${i}]`, depth + 1);
        if (hit) return hit;
      }
      return null;
    }
    if (value instanceof Map) {
      for (const [k, v] of value) {
        const hit = walk(v, `${path}.get(${String(k)})`, depth + 1);
        if (hit) return hit;
      }
      return null;
    }
    if (value instanceof Set) {
      for (const v of value) {
        const hit = walk(v, `${path}.<set>`, depth + 1);
        if (hit) return hit;
      }
      return null;
    }

    for (const key of Object.getOwnPropertyNames(value)) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      // Skip getters: invoking them can throw (agent.secretKey does exactly
      // that for a remote signer) and they hold no stored state anyway.
      if (!descriptor || descriptor.get) continue;
      const hit = walk(descriptor.value, `${path}.${key}`, depth + 1);
      if (hit) return hit;
    }
    return null;
  }

  return walk(root, '<agent>', 0);
}

describe('secret containment', () => {
  // The containment assertions below are only worth anything if the detector
  // actually detects. These pin that down first.
  describe('findString (the detector itself)', () => {
    it('finds a secret on a direct field', () => {
      expect(findString({ keypair: { _secretKey: TEST_SECRET } }, TEST_SECRET))
        .toBe('<agent>.keypair._secretKey');
    });

    it('finds a secret nested in an array', () => {
      expect(findString({ history: [{ signed: `used ${TEST_SECRET}` }] }, TEST_SECRET))
        .toBe('<agent>.history[0].signed');
    });

    it('finds a secret in a Map value', () => {
      expect(findString({ cache: new Map([['k', TEST_SECRET]]) }, TEST_SECRET))
        .toBe('<agent>.cache.get(k)');
    });

    it('finds a secret behind a class instance field', () => {
      class Holder {
        constructor(public secret: string) {}
      }
      expect(findString({ signer: new Holder(TEST_SECRET) }, TEST_SECRET))
        .toBe('<agent>.signer.secret');
    });

    it('returns null when the secret is genuinely absent', () => {
      expect(findString({ url: 'https://signer.internal', token: 'tok' }, TEST_SECRET)).toBeNull();
    });

    it('survives a circular graph', () => {
      const a: Record<string, unknown> = { name: 'a' };
      a.self = a;
      expect(findString(a, TEST_SECRET)).toBeNull();
    });

    it('does not invoke throwing getters', () => {
      const obj = {
        get boom(): string {
          throw new Error('should not be called');
        },
        safe: 'fine',
      };
      expect(() => findString(obj, TEST_SECRET)).not.toThrow();
    });
  });

  it('the mock remote signer produces real signatures despite hiding the key', async () => {
    // Establishes that the containment assertions below are meaningful: this
    // signer genuinely signs, it is not a no-op stand-in.
    const { signer } = createMockRemoteSigner();
    const { TransactionBuilder, Account, Operation, Asset, BASE_FEE } = await import(
      '@stellar/stellar-sdk'
    );
    const xdr = new TransactionBuilder(new Account(TEST_PUBLIC, '1'), {
      fee: BASE_FEE,
      networkPassphrase: Networks.TESTNET,
    })
      .addOperation(Operation.payment({ destination: TEST_PUBLIC, asset: Asset.native(), amount: '1' }))
      .setTimeout(30)
      .build()
      .toXDR();

    const signed = await signer.signTransaction(xdr, { networkPassphrase: Networks.TESTNET });
    const tx = TransactionBuilder.fromXDR(signed, Networks.TESTNET);
    expect(tx.signatures).toHaveLength(1);
    expect(Keypair.fromPublicKey(TEST_PUBLIC).verify(tx.hash(), tx.signatures[0]!.signature()))
      .toBe(true);
  });

  it('the raw secret appears nowhere in a signer-backed agent', async () => {
    const { signer } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    expect(findString(agent, TEST_SECRET)).toBeNull();
  });

  it('no S-prefixed secret-shaped string appears in a signer-backed agent', async () => {
    // Broader than an exact-match check: catches a secret arriving by some
    // other path than the one this test injected.
    const { signer } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    const path = findString(agent, 'S');
    if (path) {
      // 'S' matches plenty of harmless strings; assert none is a valid secret.
      const collected: string[] = [];
      JSON.stringify(agent, (_k, v) => {
        if (typeof v === 'string' && /^S[A-Z2-7]{55}$/.test(v)) collected.push(v);
        return v;
      });
      expect(collected).toEqual([]);
    }
  });

  it('the secret does not survive JSON serialisation of the agent', async () => {
    const { signer } = createMockRemoteSigner();
    const agent = await createWithSigner(signer);
    // Getters can throw, so serialise own data properties only.
    const snapshot = JSON.stringify(agent, (_key, value) => value);
    expect(snapshot ?? '').not.toContain(TEST_SECRET);
  });

  it('a RemoteSigner-backed agent holds only a url and a token', async () => {
    const signer = new RemoteSigner({
      url: 'https://signer.internal',
      token: 'tok_abc',
      fetch: (async () =>
        new Response(JSON.stringify({ publicKey: TEST_PUBLIC }), {
          status: 200,
        })) as unknown as typeof globalThis.fetch,
    });
    const agent = await createWithSigner(signer);
    expect(agent.address).toBe(TEST_PUBLIC);
    expect(findString(agent, TEST_SECRET)).toBeNull();
    expect(agent.holdsSecretKey).toBe(false);
  });

  it('by contrast, a keypair-backed agent can still export its secret', async () => {
    // The backward-compatible path is unchanged — but note that even here the
    // secret is not on a plain field, only behind an explicit accessor.
    const agent = await createWithSecret();
    expect(agent.holdsSecretKey).toBe(true);
    expect(agent.secretKey).toBe(TEST_SECRET);
    expect(findString(agent, TEST_SECRET)).toBeNull();
  });
});

// ─── Parity between the two paths ────────────────────────────────────────────

describe('signer path vs keypair path parity', () => {
  let viaSigner: StellarAgent;
  let viaSecret: StellarAgent;

  beforeEach(async () => {
    const { signer } = createMockRemoteSigner();
    viaSigner = await createWithSigner(signer);
    viaSecret = await createWithSecret();
    vi.spyOn(console, 'log').mockImplementation(() => {});
  });

  it('both report the same address', () => {
    expect(viaSigner.address).toBe(viaSecret.address);
  });

  it('both resolve the same contracts', () => {
    const contractsOf = (a: StellarAgent) =>
      (a as unknown as { contracts: unknown }).contracts;
    expect(contractsOf(viaSigner)).toEqual(contractsOf(viaSecret));
  });

  it('openChannel behaves identically on both', async () => {
    const params = { deposit: '10', limitPerPeriod: '1', period: 'hourly' } as const;
    const fromSigner = await viaSigner.openChannel(params).catch((e: Error) => e.message);
    const fromSecret = await viaSecret.openChannel(params).catch((e: Error) => e.message);
    expect(fromSigner).toBe(fromSecret);
  });

  it('payForAPI rejects with no channel identically on both', async () => {
    const params = { endpoint: 'https://api.example.com', amount: '0.001' };
    const fromSigner = await viaSigner.payForAPI(params).catch((e: Error) => e.message);
    const fromSecret = await viaSecret.payForAPI(params).catch((e: Error) => e.message);
    expect(fromSigner).toBe(fromSecret);
    expect(fromSigner).toMatch(/No active payment channel/);
  });

  it('payForAPI enforces the destAsset/minReceived pairing identically on both', async () => {
    const setChannel = (a: StellarAgent) => {
      (a as unknown as { activeChannelId?: bigint }).activeChannelId = 1n;
    };
    setChannel(viaSigner);
    setChannel(viaSecret);

    const params = { endpoint: 'https://api.example.com', amount: '0.001', destAsset: 'XLM' };
    const fromSigner = await viaSigner.payForAPI(params).catch((e: Error) => e.message);
    const fromSecret = await viaSecret.payForAPI(params).catch((e: Error) => e.message);
    expect(fromSigner).toBe(fromSecret);
    expect(fromSigner).toMatch(/destAsset and minReceived must be set together/);
  });

  it('the full escrow surface behaves identically on both', async () => {
    const probes: [string, (a: StellarAgent) => Promise<unknown>][] = [
      ['requestWork', (a) => a.requestWork({ workerAgent: TEST_PUBLIC, task: 't', escrowAmount: '1' })],
      ['acceptJob', (a) => a.acceptJob(1n)],
      ['submitResult', (a) => a.submitResult(1n, 'r')],
      ['releasePayment', (a) => a.releasePayment(1n)],
      ['getJob', (a) => a.getJob(1n)],
      ['getChannel', (a) => a.getChannel(1n)],
      ['getSpendReport', (a) => a.getSpendReport()],
      ['getRateLimitStatus', (a) => a.getRateLimitStatus()],
      ['checkRateLimit', (a) => a.checkRateLimit('1')],
    ];
    for (const [name, call] of probes) {
      const fromSigner = await call(viaSigner).catch((e: Error) => e.message);
      const fromSecret = await call(viaSecret).catch((e: Error) => e.message);
      expect(fromSigner, `${name} should behave identically`).toBe(fromSecret);
    }
  });
});

// ─── Read-only methods need no signer at all ─────────────────────────────────

describe('read-only methods require no signing', () => {
  /**
   * A Signer that fails loudly if anything asks it to sign. Address
   * derivation is allowed, since `create()` legitimately needs it.
   */
  function refuseToSign(): Signer {
    return {
      getPublicKey: async () => TEST_PUBLIC,
      signTransaction: async () => {
        throw new Error('signTransaction must not be called by a read-only method');
      },
      signAuthEntry: async () => {
        throw new Error('signAuthEntry must not be called by a read-only method');
      },
    };
  }

  function stubHorizon(agent: StellarAgent, balances: unknown[]) {
    (agent as unknown as { horizon: unknown }).horizon = {
      loadAccount: vi.fn(async () => ({ balances })),
    };
  }

  it('getBalance never signs', async () => {
    const agent = await createWithSigner(refuseToSign());
    stubHorizon(agent, [{ asset_type: 'native', balance: '42.0000000' }]);
    expect(await agent.getBalance()).toBe('42.0000000');
  });

  it('getBalance works with a signer that has no key at all', async () => {
    // Confirms the read path is genuinely independent of signing capability.
    const watchOnly: Signer = {
      getPublicKey: async () => TEST_PUBLIC,
      signTransaction: async () => {
        throw new SigningError('watch-only');
      },
      signAuthEntry: async () => {
        throw new SigningError('watch-only');
      },
    };
    const agent = await createWithSigner(watchOnly);
    stubHorizon(agent, [{ asset_type: 'native', balance: '7' }]);
    expect(await agent.getBalance()).toBe('7');
  });

  it('getBalance still degrades to "0" for a missing account', async () => {
    const agent = await createWithSigner(refuseToSign());
    (agent as unknown as { horizon: unknown }).horizon = {
      loadAccount: vi.fn(async () => {
        throw new Error('404');
      }),
    };
    expect(await agent.getBalance()).toBe('0');
  });

  it('address derivation needs no signing either', async () => {
    const agent = await createWithSigner(refuseToSign());
    expect(agent.address).toBe(TEST_PUBLIC);
  });
});
