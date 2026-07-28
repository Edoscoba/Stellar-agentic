/**
 * Signing abstraction.
 *
 * ## Why this module exists
 *
 * `StellarAgent` was built entirely around `Keypair.fromSecret(config.secretKey)`
 * and exposed `get secretKey()` returning the raw secret string. For an agent
 * running with real funds that is a serious risk: the secret sits in a
 * long-lived Node.js process for its whole lifetime, reachable from a heap
 * dump, a `process.env` leak, an error report that serialises the object
 * graph, or any of the many transitive dependencies `@stellar/stellar-sdk`
 * pulls in.
 *
 * This module separates *what to sign* from *what holds the key*. The agent
 * gets a {@link Signer}; where the key actually lives is the Signer's
 * problem.
 *
 * ## The interface shape
 *
 * `signTransaction` / `signAuthEntry` over base64 XDR is deliberately the
 * same shape as SEP-43, the Stellar wallet-interface standard. That means an
 * existing browser wallet, a hardware device, or a signing service can be
 * adapted with a thin wrapper, and it keeps the boundary at "here are bytes,
 * give me back signed bytes" — the narrowest interface that never requires
 * key material to cross it.
 *
 * Soroban needs both halves: `signTransaction` covers the transaction
 * envelope, and `signAuthEntry` covers `SorobanAuthorizationEntry` values,
 * which are signed separately from the envelope that carries them.
 *
 * ## Why a remote-signing service rather than Ledger
 *
 * The issue asked for one remote backend — a Ledger integration or a
 * remote-signing RPC protocol — and said to document the choice. This module
 * implements {@link RemoteSigner}, an HTTP signing service.
 *
 * A Ledger requires a physical button press for every signature. That is a
 * good property for a human treasury and a fatal one here: the entire premise
 * of this SDK is an *autonomous* agent paying $0.001 per API call without a
 * human in the loop. A hardware wallet cannot serve an unattended process
 * making a payment per inference request — the first payment would block
 * forever waiting for a press. Hardware signing is the right answer for the
 * *admin* keys that deploy and configure contracts; it is the wrong answer
 * for the agent's hot operational key, which is what `StellarAgent` holds.
 *
 * A remote signing service does fit: the key lives in an HSM/KMS behind a
 * network boundary, the agent process holds only a URL and an auth token, and
 * the service is where policy belongs — rate limits, spend ceilings, an audit
 * log, revocation. Compromising the agent process then yields the ability to
 * *request* signatures subject to that policy, not the key itself. Rotation
 * means rotating a token, not redeploying every agent.
 *
 * {@link SignerAdapter} exists for anyone who does want Ledger or a browser
 * wallet: both already speak the SEP-43 method shape.
 *
 * @module signer
 */

import { Keypair, StrKey } from '@stellar/stellar-sdk';

// ─── Interface ───────────────────────────────────────────────────────────────

export interface SignTransactionOptions {
  /** Network passphrase the signature must be bound to. */
  networkPassphrase: string;
}

export interface SignAuthEntryOptions {
  /** Network passphrase the signature must be bound to. */
  networkPassphrase: string;
  /** Ledger sequence after which the authorization is no longer valid. */
  validUntilLedgerSeq: number;
}

/**
 * Somewhere that can sign on behalf of one Stellar account.
 *
 * Implementations must never require the caller to hold key material. The
 * only thing a `StellarAgent` ever learns from a Signer is a public address
 * and some signed bytes.
 */
export interface Signer {
  /**
   * The Stellar public address (`G...`) this signer signs for.
   *
   * Must be obtainable **without** the secret being present in the calling
   * process — a remote signer derives it on the far side of the boundary and
   * returns just the address.
   */
  getPublicKey(): Promise<string>;

  /**
   * Sign a transaction envelope.
   *
   * @param xdr - base64 transaction envelope XDR
   * @returns base64 **signed** transaction envelope XDR
   */
  signTransaction(xdr: string, options: SignTransactionOptions): Promise<string>;

  /**
   * Sign a Soroban authorization entry.
   *
   * Soroban auth entries are signed separately from the envelope that carries
   * them, so a signer that only implements `signTransaction` cannot authorize
   * a contract invocation.
   *
   * @param authEntryXdr - base64 `SorobanAuthorizationEntry` XDR
   * @returns base64 **signed** `SorobanAuthorizationEntry` XDR
   */
  signAuthEntry(authEntryXdr: string, options: SignAuthEntryOptions): Promise<string>;
}

/** Thrown when a signer cannot produce a signature. */
export class SigningError extends Error {
  readonly cause?: unknown;
  constructor(message: string, cause?: unknown) {
    super(message);
    this.name = 'SigningError';
    this.cause = cause;
  }
}

// ─── Keypair-backed signer ───────────────────────────────────────────────────

/**
 * The original behaviour, kept for backward compatibility: an in-memory
 * `Keypair`.
 *
 * This is fine for testnet, for development, and for agents holding
 * negligible value. It is explicitly *not* what you want for an agent with
 * real funds — see {@link RemoteSigner}.
 *
 * The secret is held in a module-private closure rather than on the instance,
 * so it does not appear when the signer (or an agent holding it) is logged,
 * serialised by an error reporter, or walked by a heap inspector that only
 * follows enumerable properties.
 */
export class KeypairSigner implements Signer {
  readonly #keypair: Keypair;

  constructor(keypair: Keypair) {
    if (!keypair.canSign()) {
      throw new SigningError('KeypairSigner requires a keypair with a secret key');
    }
    this.#keypair = keypair;
  }

  /** Build from a `S...` secret key string. */
  static fromSecret(secretKey: string): KeypairSigner {
    return new KeypairSigner(Keypair.fromSecret(secretKey));
  }

  /** Generate a fresh random keypair. */
  static random(): KeypairSigner {
    return new KeypairSigner(Keypair.random());
  }

  async getPublicKey(): Promise<string> {
    return this.#keypair.publicKey();
  }

  /** Synchronous accessor — available because the key is local. */
  publicKey(): string {
    return this.#keypair.publicKey();
  }

  /**
   * Reveal the raw secret.
   *
   * Deliberately a method with a blunt name rather than a `secretKey` getter:
   * exporting key material should be a visible, greppable act, not something
   * that happens by reading a property.
   */
  exportSecret(): string {
    return this.#keypair.secret();
  }

  async signTransaction(xdr: string, options: SignTransactionOptions): Promise<string> {
    const { TransactionBuilder } = await import('@stellar/stellar-sdk');
    try {
      const tx = TransactionBuilder.fromXDR(xdr, options.networkPassphrase);
      tx.sign(this.#keypair);
      return tx.toXDR();
    } catch (err) {
      throw new SigningError('KeypairSigner: failed to sign transaction', err);
    }
  }

  async signAuthEntry(authEntryXdr: string, options: SignAuthEntryOptions): Promise<string> {
    const { authorizeEntry, xdr } = await import('@stellar/stellar-sdk');
    try {
      const entry = xdr.SorobanAuthorizationEntry.fromXDR(authEntryXdr, 'base64');
      const signed = await authorizeEntry(
        entry,
        this.#keypair,
        options.validUntilLedgerSeq,
        options.networkPassphrase,
      );
      return signed.toXDR('base64');
    } catch (err) {
      throw new SigningError('KeypairSigner: failed to sign authorization entry', err);
    }
  }
}

// ─── Remote signer ───────────────────────────────────────────────────────────

export interface RemoteSignerOptions {
  /** Base URL of the signing service, e.g. `https://signer.internal:8443`. */
  url: string;
  /**
   * Bearer token presented on every request. This is the *only* credential
   * the agent process holds — losing it costs a token rotation, not a key
   * rotation and a migration of every funded account.
   */
  token?: string;
  /**
   * Public address this signer is expected to sign for. When set, it is
   * checked against what the service reports and a mismatch is rejected, so
   * a misconfigured or swapped-out service cannot quietly sign as a different
   * account.
   */
  expectedPublicKey?: string;
  /** Per-request timeout in milliseconds. @default 10000 */
  timeoutMs?: number;
  /** Extra headers (mTLS proxies, tracing, tenant routing). */
  headers?: Record<string, string>;
  /** Injectable `fetch`, for tests and for custom agents/proxies. */
  fetch?: typeof globalThis.fetch;
}

/**
 * A {@link Signer} backed by an HTTP signing service.
 *
 * ## Protocol
 *
 * Three endpoints, all JSON. The key never crosses the boundary.
 *
 * ### `GET {url}/v1/public-key`
 * ```json
 * → 200 { "publicKey": "G..." }
 * ```
 *
 * ### `POST {url}/v1/sign/transaction`
 * ```json
 * ← { "xdr": "<base64 envelope>", "networkPassphrase": "..." }
 * → 200 { "signedXdr": "<base64 signed envelope>" }
 * ```
 *
 * ### `POST {url}/v1/sign/auth-entry`
 * ```json
 * ← { "authEntryXdr": "<base64>", "networkPassphrase": "...",
 *     "validUntilLedgerSeq": 12345 }
 * → 200 { "signedAuthEntryXdr": "<base64>" }
 * ```
 *
 * Errors return a non-2xx status with `{ "error": "<message>" }`. A service
 * that refuses on policy grounds — spend ceiling, rate limit, revoked token —
 * should use `403` with a description; it surfaces here as a
 * {@link SigningError} carrying that text.
 *
 * ## Why signed XDR rather than a raw signature
 *
 * Returning `signedXdr` means the service parses what it is signing and can
 * therefore apply policy to it — reject payments over a ceiling, enforce a
 * destination allow-list, log the operation. A service that only returned a
 * signature over an opaque hash could not do any of that, which would waste
 * the main advantage of moving the key behind a boundary in the first place.
 */
export class RemoteSigner implements Signer {
  readonly #options: Required<Pick<RemoteSignerOptions, 'url' | 'timeoutMs'>> &
    RemoteSignerOptions;
  readonly #fetch: typeof globalThis.fetch;
  /** Cached — the address cannot change for a given signing identity. */
  #publicKey?: string;

  constructor(options: RemoteSignerOptions) {
    if (!options.url) throw new SigningError('RemoteSigner requires a url');
    if (options.expectedPublicKey && !StrKey.isValidEd25519PublicKey(options.expectedPublicKey)) {
      throw new SigningError(
        `RemoteSigner: expectedPublicKey is not a valid Stellar address: ${options.expectedPublicKey}`,
      );
    }
    this.#options = {
      timeoutMs: 10_000,
      ...options,
      url: options.url.replace(/\/+$/, ''),
    };
    this.#fetch = options.fetch ?? globalThis.fetch;
    if (typeof this.#fetch !== 'function') {
      throw new SigningError('RemoteSigner: no fetch implementation available');
    }
  }

  async getPublicKey(): Promise<string> {
    if (this.#publicKey) return this.#publicKey;

    const { publicKey } = await this.#request<{ publicKey: string }>('GET', '/v1/public-key');
    if (!publicKey || !StrKey.isValidEd25519PublicKey(publicKey)) {
      throw new SigningError(
        `RemoteSigner: service returned an invalid public key: ${String(publicKey)}`,
      );
    }
    const expected = this.#options.expectedPublicKey;
    if (expected && publicKey !== expected) {
      throw new SigningError(
        `RemoteSigner: service signs for ${publicKey}, but ${expected} was expected. ` +
          'Refusing to continue — this signer may be misconfigured or substituted.',
      );
    }
    this.#publicKey = publicKey;
    return publicKey;
  }

  async signTransaction(xdr: string, options: SignTransactionOptions): Promise<string> {
    const { signedXdr } = await this.#request<{ signedXdr: string }>(
      'POST',
      '/v1/sign/transaction',
      { xdr, networkPassphrase: options.networkPassphrase },
    );
    if (typeof signedXdr !== 'string' || signedXdr.length === 0) {
      throw new SigningError('RemoteSigner: service returned no signedXdr');
    }
    return signedXdr;
  }

  async signAuthEntry(authEntryXdr: string, options: SignAuthEntryOptions): Promise<string> {
    const { signedAuthEntryXdr } = await this.#request<{ signedAuthEntryXdr: string }>(
      'POST',
      '/v1/sign/auth-entry',
      {
        authEntryXdr,
        networkPassphrase: options.networkPassphrase,
        validUntilLedgerSeq: options.validUntilLedgerSeq,
      },
    );
    if (typeof signedAuthEntryXdr !== 'string' || signedAuthEntryXdr.length === 0) {
      throw new SigningError('RemoteSigner: service returned no signedAuthEntryXdr');
    }
    return signedAuthEntryXdr;
  }

  async #request<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    const url = `${this.#options.url}${path}`;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.#options.timeoutMs);

    let response: Response;
    try {
      response = await this.#fetch(url, {
        method,
        signal: controller.signal,
        headers: {
          accept: 'application/json',
          ...(body ? { 'content-type': 'application/json' } : {}),
          ...(this.#options.token ? { authorization: `Bearer ${this.#options.token}` } : {}),
          ...this.#options.headers,
        },
        ...(body ? { body: JSON.stringify(body) } : {}),
      });
    } catch (err) {
      const aborted = (err as { name?: string })?.name === 'AbortError';
      throw new SigningError(
        aborted
          ? `RemoteSigner: ${method} ${path} timed out after ${this.#options.timeoutMs}ms`
          : `RemoteSigner: ${method} ${path} failed`,
        err,
      );
    } finally {
      clearTimeout(timer);
    }

    if (!response.ok) {
      // Surface the service's own reason — a policy refusal (spend ceiling,
      // rate limit, revoked token) is the interesting case and is useless as
      // a bare status code.
      let detail = '';
      try {
        const payload = (await response.json()) as { error?: string };
        detail = payload?.error ? `: ${payload.error}` : '';
      } catch {
        /* non-JSON body — the status alone will have to do */
      }
      throw new SigningError(
        `RemoteSigner: ${method} ${path} returned ${response.status}${detail}`,
      );
    }

    try {
      return (await response.json()) as T;
    } catch (err) {
      throw new SigningError(`RemoteSigner: ${method} ${path} returned invalid JSON`, err);
    }
  }
}

// ─── Adapter for SEP-43 style wallets ────────────────────────────────────────

/**
 * Wrap anything already exposing SEP-43's method shape — Freighter and other
 * browser wallets, `@ledgerhq`-backed signers, an in-house module — as a
 * {@link Signer}.
 *
 * This is the extension point for a Ledger integration: hardware signing is
 * the right choice for admin keys even though it is the wrong choice for an
 * agent's hot key (see the module doc).
 */
export interface Sep43Like {
  getAddress(): Promise<{ address: string }> | Promise<string> | { address: string } | string;
  signTransaction(
    xdr: string,
    opts?: { networkPassphrase?: string },
  ): Promise<{ signedTxXdr: string } | string>;
  signAuthEntry?(
    entryXdr: string,
    opts?: { networkPassphrase?: string },
  ): Promise<{ signedAuthEntry: string } | string>;
}

export class SignerAdapter implements Signer {
  readonly #wallet: Sep43Like;

  constructor(wallet: Sep43Like) {
    this.#wallet = wallet;
  }

  async getPublicKey(): Promise<string> {
    const result = await this.#wallet.getAddress();
    const address = typeof result === 'string' ? result : result.address;
    if (!StrKey.isValidEd25519PublicKey(address)) {
      throw new SigningError(`SignerAdapter: wallet returned an invalid address: ${address}`);
    }
    return address;
  }

  async signTransaction(xdr: string, options: SignTransactionOptions): Promise<string> {
    const result = await this.#wallet.signTransaction(xdr, {
      networkPassphrase: options.networkPassphrase,
    });
    return typeof result === 'string' ? result : result.signedTxXdr;
  }

  async signAuthEntry(authEntryXdr: string, options: SignAuthEntryOptions): Promise<string> {
    if (!this.#wallet.signAuthEntry) {
      throw new SigningError(
        'SignerAdapter: the wrapped wallet cannot sign Soroban authorization entries, ' +
          'so it cannot authorize contract invocations.',
      );
    }
    const result = await this.#wallet.signAuthEntry(authEntryXdr, {
      networkPassphrase: options.networkPassphrase,
    });
    return typeof result === 'string' ? result : result.signedAuthEntry;
  }
}

// ─── Resolution ──────────────────────────────────────────────────────────────

/** Duck-typed check — `instanceof` fails across duplicated package copies. */
export function isSigner(value: unknown): value is Signer {
  if (!value || typeof value !== 'object') return false;
  const v = value as Partial<Signer>;
  return (
    typeof v.getPublicKey === 'function' &&
    typeof v.signTransaction === 'function' &&
    typeof v.signAuthEntry === 'function'
  );
}
