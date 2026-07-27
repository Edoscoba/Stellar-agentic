import { describe, it, expect, vi } from 'vitest';
import {
  Account,
  Keypair,
  Networks,
  Operation,
  Asset,
  TransactionBuilder,
  BASE_FEE,
} from '@stellar/stellar-sdk';

import {
  KeypairSigner,
  RemoteSigner,
  SignerAdapter,
  SigningError,
  isSigner,
  type Signer,
} from '../signer.js';
import { TEST_SECRET, TEST_PUBLIC } from './fixtures.js';

const NETWORK = Networks.TESTNET;

/** Build a real, signable transaction envelope XDR. */
function unsignedTxXdr(source = TEST_PUBLIC): string {
  const account = new Account(source, '1');
  return new TransactionBuilder(account, { fee: BASE_FEE, networkPassphrase: NETWORK })
    .addOperation(
      Operation.payment({
        destination: 'GDVEU3DD4KOFECV66VIHWEZOYX4ZKR3WV27L464SIIPOU2IUI3JCZA57',
        asset: Asset.native(),
        amount: '1',
      }),
    )
    .setTimeout(30)
    .build()
    .toXDR();
}

function signatureCount(xdr: string): number {
  return TransactionBuilder.fromXDR(xdr, NETWORK).signatures.length;
}

// ─── KeypairSigner ───────────────────────────────────────────────────────────

describe('KeypairSigner', () => {
  it('reports the public key for its secret', async () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    expect(await signer.getPublicKey()).toBe(TEST_PUBLIC);
    expect(signer.publicKey()).toBe(TEST_PUBLIC);
  });

  it('generates a usable random signer', async () => {
    const signer = KeypairSigner.random();
    expect(await signer.getPublicKey()).toMatch(/^G[A-Z2-7]{55}$/);
  });

  it('rejects a public-key-only keypair', () => {
    const watchOnly = Keypair.fromPublicKey(TEST_PUBLIC);
    expect(() => new KeypairSigner(watchOnly)).toThrow(SigningError);
    expect(() => new KeypairSigner(watchOnly)).toThrow(/requires a keypair with a secret/);
  });

  it('rejects a malformed secret', () => {
    expect(() => KeypairSigner.fromSecret('nope')).toThrow();
  });

  it('actually signs a transaction', async () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    const xdr = unsignedTxXdr();
    expect(signatureCount(xdr)).toBe(0);

    const signed = await signer.signTransaction(xdr, { networkPassphrase: NETWORK });
    expect(signatureCount(signed)).toBe(1);
  });

  it('produces a signature that verifies against the public key', async () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    const signed = await signer.signTransaction(unsignedTxXdr(), { networkPassphrase: NETWORK });

    const tx = TransactionBuilder.fromXDR(signed, NETWORK);
    const keypair = Keypair.fromPublicKey(TEST_PUBLIC);
    expect(keypair.verify(tx.hash(), tx.signatures[0]!.signature())).toBe(true);
  });

  it('binds the signature to the network passphrase', async () => {
    // The same transaction signed for a different network must not verify
    // against this one — replay protection lives in the passphrase.
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    const signed = await signer.signTransaction(unsignedTxXdr(), { networkPassphrase: NETWORK });

    const asTestnet = TransactionBuilder.fromXDR(signed, Networks.TESTNET);
    const asPublic = TransactionBuilder.fromXDR(signed, Networks.PUBLIC);
    const keypair = Keypair.fromPublicKey(TEST_PUBLIC);

    expect(keypair.verify(asTestnet.hash(), asTestnet.signatures[0]!.signature())).toBe(true);
    expect(keypair.verify(asPublic.hash(), asPublic.signatures[0]!.signature())).toBe(false);
  });

  it('is deterministic — ed25519 signatures do not vary per call', async () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    const xdr = unsignedTxXdr();
    const a = await signer.signTransaction(xdr, { networkPassphrase: NETWORK });
    const b = await signer.signTransaction(xdr, { networkPassphrase: NETWORK });
    expect(a).toBe(b);
  });

  it('wraps a malformed XDR in a SigningError', async () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    await expect(signer.signTransaction('not-xdr', { networkPassphrase: NETWORK }))
      .rejects.toThrow(SigningError);
  });

  it('wraps a malformed auth entry in a SigningError', async () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    await expect(
      signer.signAuthEntry('not-xdr', { networkPassphrase: NETWORK, validUntilLedgerSeq: 100 }),
    ).rejects.toThrow(SigningError);
  });

  it('keeps the secret off the instance as an enumerable property', () => {
    // The key lives in a #private field, so it survives neither JSON
    // serialisation nor a shallow property walk by an error reporter.
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    expect(Object.keys(signer)).toEqual([]);
    expect(JSON.stringify(signer)).not.toContain(TEST_SECRET);
    expect(Object.getOwnPropertyNames(signer)).not.toContain('keypair');
  });

  it('exports the secret only through an explicit method', () => {
    const signer = KeypairSigner.fromSecret(TEST_SECRET);
    expect(signer.exportSecret()).toBe(TEST_SECRET);
    // Not reachable as a property — exporting key material has to be a
    // visible, greppable call.
    expect((signer as unknown as { secretKey?: string }).secretKey).toBeUndefined();
  });

  it('satisfies the Signer interface', () => {
    expect(isSigner(KeypairSigner.fromSecret(TEST_SECRET))).toBe(true);
  });
});

// ─── RemoteSigner ────────────────────────────────────────────────────────────

/** Build a RemoteSigner over a scripted fetch. */
function remoteSigner(
  routes: Record<string, { status?: number; body: unknown }>,
  options: { expectedPublicKey?: string; timeoutMs?: number } = {},
) {
  const calls: { url: string; init?: RequestInit }[] = [];
  const fetchImpl = vi.fn(async (input: unknown, init?: unknown) => {
    const url = String(input);
    calls.push({ url, init: init as RequestInit });
    const path = new URL(url).pathname;
    const route = routes[path];
    if (!route) return new Response(JSON.stringify({ error: 'no route' }), { status: 404 });
    return new Response(JSON.stringify(route.body), {
      status: route.status ?? 200,
      headers: { 'content-type': 'application/json' },
    });
  });
  const signer = new RemoteSigner({
    url: 'https://signer.test',
    token: 'tok_123',
    fetch: fetchImpl as unknown as typeof globalThis.fetch,
    ...options,
  });
  return { signer, calls, fetchImpl };
}

describe('RemoteSigner — construction', () => {
  it('requires a url', () => {
    expect(() => new RemoteSigner({ url: '' })).toThrow(/requires a url/);
  });

  it('rejects an invalid expectedPublicKey up front', () => {
    expect(() => new RemoteSigner({ url: 'https://x', expectedPublicKey: 'nope' }))
      .toThrow(/not a valid Stellar address/);
  });

  it('strips a trailing slash from the url', async () => {
    const calls: string[] = [];
    const signer = new RemoteSigner({
      url: 'https://signer.test///',
      fetch: (async (input: unknown) => {
        calls.push(String(input));
        return new Response(JSON.stringify({ publicKey: TEST_PUBLIC }), { status: 200 });
      }) as unknown as typeof globalThis.fetch,
    });
    await signer.getPublicKey();
    expect(calls[0]).toBe('https://signer.test/v1/public-key');
  });

  it('satisfies the Signer interface', () => {
    expect(isSigner(new RemoteSigner({ url: 'https://x' }))).toBe(true);
  });
});

describe('RemoteSigner — getPublicKey', () => {
  it('fetches the address from the service', async () => {
    const { signer, calls } = remoteSigner({
      '/v1/public-key': { body: { publicKey: TEST_PUBLIC } },
    });
    expect(await signer.getPublicKey()).toBe(TEST_PUBLIC);
    expect(calls[0]!.url).toBe('https://signer.test/v1/public-key');
    expect(calls[0]!.init?.method).toBe('GET');
  });

  it('sends the bearer token', async () => {
    const { signer, calls } = remoteSigner({
      '/v1/public-key': { body: { publicKey: TEST_PUBLIC } },
    });
    await signer.getPublicKey();
    const headers = calls[0]!.init?.headers as Record<string, string>;
    expect(headers.authorization).toBe('Bearer tok_123');
  });

  it('caches the address — an identity cannot change', async () => {
    const { signer, fetchImpl } = remoteSigner({
      '/v1/public-key': { body: { publicKey: TEST_PUBLIC } },
    });
    await signer.getPublicKey();
    await signer.getPublicKey();
    await signer.getPublicKey();
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('rejects a malformed address from the service', async () => {
    const { signer } = remoteSigner({ '/v1/public-key': { body: { publicKey: 'garbage' } } });
    await expect(signer.getPublicKey()).rejects.toThrow(/invalid public key/);
  });

  it('rejects a missing address', async () => {
    const { signer } = remoteSigner({ '/v1/public-key': { body: {} } });
    await expect(signer.getPublicKey()).rejects.toThrow(/invalid public key/);
  });

  it('refuses a service signing for an unexpected account', async () => {
    // Guards against a substituted or misrouted signing service quietly
    // signing as somebody else.
    const other = Keypair.random().publicKey();
    const { signer } = remoteSigner(
      { '/v1/public-key': { body: { publicKey: other } } },
      { expectedPublicKey: TEST_PUBLIC },
    );
    await expect(signer.getPublicKey()).rejects.toThrow(/Refusing to continue/);
  });

  it('accepts a service matching expectedPublicKey', async () => {
    const { signer } = remoteSigner(
      { '/v1/public-key': { body: { publicKey: TEST_PUBLIC } } },
      { expectedPublicKey: TEST_PUBLIC },
    );
    await expect(signer.getPublicKey()).resolves.toBe(TEST_PUBLIC);
  });
});

describe('RemoteSigner — signing', () => {
  it('posts the envelope and returns the signed XDR', async () => {
    const signed = 'AAAA-signed-envelope';
    const { signer, calls } = remoteSigner({
      '/v1/sign/transaction': { body: { signedXdr: signed } },
    });
    const result = await signer.signTransaction('AAAA-unsigned', { networkPassphrase: NETWORK });

    expect(result).toBe(signed);
    expect(calls[0]!.init?.method).toBe('POST');
    expect(JSON.parse(String(calls[0]!.init?.body))).toEqual({
      xdr: 'AAAA-unsigned',
      networkPassphrase: NETWORK,
    });
  });

  it('posts the auth entry with its validity ledger', async () => {
    const { signer, calls } = remoteSigner({
      '/v1/sign/auth-entry': { body: { signedAuthEntryXdr: 'AAAA-signed-entry' } },
    });
    const result = await signer.signAuthEntry('AAAA-entry', {
      networkPassphrase: NETWORK,
      validUntilLedgerSeq: 4242,
    });

    expect(result).toBe('AAAA-signed-entry');
    expect(JSON.parse(String(calls[0]!.init?.body))).toEqual({
      authEntryXdr: 'AAAA-entry',
      networkPassphrase: NETWORK,
      validUntilLedgerSeq: 4242,
    });
  });

  it('rejects an empty signedXdr', async () => {
    const { signer } = remoteSigner({ '/v1/sign/transaction': { body: { signedXdr: '' } } });
    await expect(signer.signTransaction('x', { networkPassphrase: NETWORK }))
      .rejects.toThrow(/returned no signedXdr/);
  });

  it('rejects a missing signedAuthEntryXdr', async () => {
    const { signer } = remoteSigner({ '/v1/sign/auth-entry': { body: {} } });
    await expect(
      signer.signAuthEntry('x', { networkPassphrase: NETWORK, validUntilLedgerSeq: 1 }),
    ).rejects.toThrow(/returned no signedAuthEntryXdr/);
  });

  it('never puts key material in a request', async () => {
    const { signer, calls } = remoteSigner({
      '/v1/sign/transaction': { body: { signedXdr: 'signed' } },
    });
    await signer.signTransaction('AAAA-unsigned', { networkPassphrase: NETWORK });
    const serialised = JSON.stringify(calls);
    expect(serialised).not.toContain(TEST_SECRET);
    expect(serialised).not.toMatch(/\bS[A-Z2-7]{55}\b/);
  });
});

describe('RemoteSigner — failures', () => {
  it('surfaces the service error text on a policy refusal', async () => {
    // A 403 from a spend-ceiling or rate-limit policy is the interesting
    // case; a bare status code would be useless.
    const { signer } = remoteSigner({
      '/v1/sign/transaction': { status: 403, body: { error: 'spend ceiling exceeded' } },
    });
    await expect(signer.signTransaction('x', { networkPassphrase: NETWORK }))
      .rejects.toThrow(/returned 403: spend ceiling exceeded/);
  });

  it('reports a bare status when the body is not JSON', async () => {
    const signer = new RemoteSigner({
      url: 'https://signer.test',
      fetch: (async () => new Response('<html>502</html>', { status: 502 })) as unknown as typeof globalThis.fetch,
    });
    await expect(signer.signTransaction('x', { networkPassphrase: NETWORK }))
      .rejects.toThrow(/returned 502/);
  });

  it('wraps a network failure', async () => {
    const signer = new RemoteSigner({
      url: 'https://signer.test',
      fetch: (async () => {
        throw new Error('ECONNREFUSED');
      }) as unknown as typeof globalThis.fetch,
    });
    await expect(signer.getPublicKey()).rejects.toThrow(SigningError);
    await expect(signer.getPublicKey()).rejects.toThrow(/failed/);
  });

  it('times out a hanging service', async () => {
    const signer = new RemoteSigner({
      url: 'https://signer.test',
      timeoutMs: 20,
      fetch: ((_url: unknown, init?: { signal?: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () => {
            const err = new Error('aborted');
            err.name = 'AbortError';
            reject(err);
          });
        })) as unknown as typeof globalThis.fetch,
    });
    await expect(signer.getPublicKey()).rejects.toThrow(/timed out after 20ms/);
  });

  it('rejects invalid JSON in a 200 response', async () => {
    const signer = new RemoteSigner({
      url: 'https://signer.test',
      fetch: (async () => new Response('not json', { status: 200 })) as unknown as typeof globalThis.fetch,
    });
    await expect(signer.getPublicKey()).rejects.toThrow(/invalid JSON/);
  });

  it('does not leak the bearer token in an error message', async () => {
    const { signer } = remoteSigner({
      '/v1/sign/transaction': { status: 500, body: { error: 'boom' } },
    });
    await expect(signer.signTransaction('x', { networkPassphrase: NETWORK }))
      .rejects.toThrow(expect.not.stringContaining('tok_123') as unknown as string);
  });
});

// ─── SignerAdapter ───────────────────────────────────────────────────────────

describe('SignerAdapter', () => {
  it('adapts a wallet returning { address }', async () => {
    const adapter = new SignerAdapter({
      getAddress: async () => ({ address: TEST_PUBLIC }),
      signTransaction: async () => ({ signedTxXdr: 'signed' }),
    });
    expect(await adapter.getPublicKey()).toBe(TEST_PUBLIC);
  });

  it('adapts a wallet returning a bare address string', async () => {
    const adapter = new SignerAdapter({
      getAddress: () => TEST_PUBLIC,
      signTransaction: async () => 'signed',
    });
    expect(await adapter.getPublicKey()).toBe(TEST_PUBLIC);
  });

  it('unwraps both signTransaction response shapes', async () => {
    const wrapped = new SignerAdapter({
      getAddress: () => TEST_PUBLIC,
      signTransaction: async () => ({ signedTxXdr: 'A' }),
    });
    const bare = new SignerAdapter({
      getAddress: () => TEST_PUBLIC,
      signTransaction: async () => 'A',
    });
    const opts = { networkPassphrase: NETWORK };
    expect(await wrapped.signTransaction('x', opts)).toBe('A');
    expect(await bare.signTransaction('x', opts)).toBe('A');
  });

  it('forwards the network passphrase to the wallet', async () => {
    const signTransaction = vi.fn(async () => 'signed');
    const adapter = new SignerAdapter({ getAddress: () => TEST_PUBLIC, signTransaction });
    await adapter.signTransaction('x', { networkPassphrase: NETWORK });
    expect(signTransaction).toHaveBeenCalledWith('x', { networkPassphrase: NETWORK });
  });

  it('rejects an invalid address from the wallet', async () => {
    const adapter = new SignerAdapter({
      getAddress: () => 'not-an-address',
      signTransaction: async () => 'signed',
    });
    await expect(adapter.getPublicKey()).rejects.toThrow(/invalid address/);
  });

  it('explains clearly when the wallet cannot sign auth entries', async () => {
    // A wallet without signAuthEntry cannot authorize Soroban invocations at
    // all, which is worth saying plainly rather than failing on undefined.
    const adapter = new SignerAdapter({
      getAddress: () => TEST_PUBLIC,
      signTransaction: async () => 'signed',
    });
    await expect(
      adapter.signAuthEntry('x', { networkPassphrase: NETWORK, validUntilLedgerSeq: 1 }),
    ).rejects.toThrow(/cannot sign Soroban authorization entries/);
  });

  it('uses signAuthEntry when the wallet provides it', async () => {
    const adapter = new SignerAdapter({
      getAddress: () => TEST_PUBLIC,
      signTransaction: async () => 'signed',
      signAuthEntry: async () => ({ signedAuthEntry: 'signed-entry' }),
    });
    expect(
      await adapter.signAuthEntry('x', { networkPassphrase: NETWORK, validUntilLedgerSeq: 1 }),
    ).toBe('signed-entry');
  });

  it('satisfies the Signer interface', () => {
    expect(isSigner(new SignerAdapter({
      getAddress: () => TEST_PUBLIC,
      signTransaction: async () => 'signed',
    }))).toBe(true);
  });
});

// ─── isSigner ────────────────────────────────────────────────────────────────

describe('isSigner', () => {
  it.each([
    ['null', null],
    ['undefined', undefined],
    ['a string', 'signer'],
    ['a number', 1],
    ['an empty object', {}],
    ['a partial signer', { getPublicKey: () => {}, signTransaction: () => {} }],
  ])('rejects %s', (_label, value) => {
    expect(isSigner(value)).toBe(false);
  });

  it('accepts any object with the three methods', () => {
    const duck: Signer = {
      getPublicKey: async () => TEST_PUBLIC,
      signTransaction: async () => 'x',
      signAuthEntry: async () => 'x',
    };
    expect(isSigner(duck)).toBe(true);
  });
});
