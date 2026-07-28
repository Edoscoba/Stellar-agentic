# Signing and key custody

`StellarAgent` used to be built entirely around
`Keypair.fromSecret(config.secretKey)`, with a `get secretKey()` that returned
the raw secret string. For an agent running with real funds that is a serious
risk: the secret sits in a long-lived Node.js process for its whole lifetime,
reachable from a heap dump, a `process.env` leak, an error report that
serialises the object graph, or any of the many transitive dependencies
`@stellar/stellar-sdk` pulls in.

The agent now signs through a **`Signer`**. Where the key lives is the
Signer's problem, not the agent's.

---

## Contents

- [The interface](#the-interface)
- [Choosing a backend](#choosing-a-backend)
- [Why a signing service and not Ledger](#why-a-signing-service-and-not-ledger)
- [`KeypairSigner`](#keypairsigner)
- [`RemoteSigner`](#remotesigner)
- [The remote signing protocol](#the-remote-signing-protocol)
- [`SignerAdapter`](#signeradapter)
- [Writing your own](#writing-your-own)
- [What changed](#what-changed)

---

## The interface

```typescript
interface Signer {
  getPublicKey(): Promise<string>;
  signTransaction(xdr: string, options: SignTransactionOptions): Promise<string>;
  signAuthEntry(authEntryXdr: string, options: SignAuthEntryOptions): Promise<string>;
}
```

Three things, all speaking base64 XDR. Key material never crosses this
boundary in either direction.

The shape is deliberately the same as **SEP-43**, the Stellar wallet-interface
standard, so a browser wallet, a hardware device, or an in-house signing
module can be adapted with a thin wrapper rather than a rewrite.

Soroban needs both signing methods. `signTransaction` covers the transaction
envelope; `signAuthEntry` covers `SorobanAuthorizationEntry` values, which are
signed *separately* from the envelope carrying them. A signer implementing
only the first cannot authorize a contract invocation — `SignerAdapter` says
so explicitly rather than failing on `undefined`.

---

## Choosing a backend

| Backend | Key lives | Use for |
|---------|-----------|---------|
| `KeypairSigner` | this process | dev, testnet, negligible value |
| `RemoteSigner` | an HSM/KMS behind HTTP | **production agents with funds** |
| `SignerAdapter` | a wallet or hardware device | admin/treasury keys, browser apps |

---

## Why a signing service and not Ledger

The issue offered two options — a Ledger hardware-wallet integration, or a
remote-signing RPC protocol — and asked for the choice to be justified. This
package implements the **remote signing service**.

**A Ledger requires a physical button press for every signature.** That is an
excellent property for a human-controlled treasury and a fatal one here. The
entire premise of this SDK is an autonomous agent paying $0.001 per API call
with no human in the loop; the README's own example pays per inference
request. A hardware wallet cannot serve an unattended process at that
cadence — the first payment would block forever waiting for a press that
nobody is there to give.

**A signing service fits the actual threat model.** The risk being addressed
is "the agent process is compromised, or its memory is dumped". Moving the key
behind a network boundary means:

- the agent process holds a URL and a token, not a key
- policy lives at the boundary — spend ceilings, rate limits, destination
  allow-lists, an audit log — and is enforced even when the caller is
  compromised
- compromising the agent yields the ability to *request* signatures subject to
  that policy, not the key itself
- rotation means rotating a token, not migrating funds off every account
- one hardened service can serve a fleet of agents

Hardware signing is still the right answer for the keys that deploy and
configure contracts — those are used rarely, by humans, and a button press is
a feature. That is what `SignerAdapter` is for; `@ledgerhq`'s Stellar app
already speaks the SEP-43 shape. It is the wrong answer for an agent's hot
operational key, which is what `StellarAgent` holds.

---

## `KeypairSigner`

The original behaviour, preserved. Nothing about the existing API changed:

```typescript
const agent = await StellarAgent.create({
  network: 'testnet',
  secretKey: process.env.AGENT_SECRET,
});
```

The secret is held in a `#private` field rather than an ordinary property, so
it does not appear in `JSON.stringify`, in `Object.keys`, or in an error
reporter walking enumerable properties. Reading it back requires an explicit
call:

```typescript
KeypairSigner.fromSecret(secret).exportSecret();
```

Deliberately a blunt method name rather than a `secretKey` getter: exporting
key material should be a visible, greppable act, not something that happens by
reading a property.

---

## `RemoteSigner`

```typescript
import { StellarAgent, RemoteSigner } from '@stellaragent/core';

const agent = await StellarAgent.create({
  network: 'testnet',
  signer: new RemoteSigner({
    url: 'https://signer.internal',
    token: process.env.SIGNER_TOKEN,
    // Refuse to run if the service signs for anyone else.
    expectedPublicKey: process.env.AGENT_ADDRESS,
  }),
});

agent.address;         // 'G...' — derived remotely, no secret here
agent.holdsSecretKey;  // false
agent.secretKey;       // throws SigningError
```

| Option | Default | Purpose |
|--------|---------|---------|
| `url` | *(required)* | Base URL of the service |
| `token` | — | Bearer token, the only credential the agent holds |
| `expectedPublicKey` | — | Pin the identity; a mismatch is rejected |
| `timeoutMs` | `10000` | Per-request timeout |
| `headers` | — | mTLS proxies, tracing, tenant routing |
| `fetch` | `globalThis.fetch` | Injectable, for tests and custom agents |

`expectedPublicKey` is worth setting. Without it, a misconfigured or
substituted service can quietly sign as a different account and the agent will
never notice.

---

## The remote signing protocol

Three JSON endpoints.

### `GET {url}/v1/public-key`

```json
→ 200 { "publicKey": "GDVEU3DD..." }
```

### `POST {url}/v1/sign/transaction`

```json
← { "xdr": "AAAAAgAAAAB...", "networkPassphrase": "Test SDF Network ; September 2015" }
→ 200 { "signedXdr": "AAAAAgAAAAB..." }
```

### `POST {url}/v1/sign/auth-entry`

```json
← { "authEntryXdr": "AAAAAQ...",
    "networkPassphrase": "Test SDF Network ; September 2015",
    "validUntilLedgerSeq": 123456 }
→ 200 { "signedAuthEntryXdr": "AAAAAQ..." }
```

### Errors

Non-2xx with `{ "error": "<message>" }`. The text is surfaced in the
`SigningError`, so a policy refusal explains itself:

```
SigningError: RemoteSigner: POST /v1/sign/transaction returned 403: spend ceiling exceeded
```

Use `403` for policy refusals — ceiling exceeded, rate limited, token revoked.

### Why signed XDR rather than a raw signature

An HSM-style service that signed an opaque hash would be simpler, but it could
not inspect what it was signing — and inspection is most of the value of
moving the key behind a boundary. Returning `signedXdr` means the service
parses the transaction and can refuse payments over a ceiling, enforce a
destination allow-list, and write a meaningful audit log. A signature over an
opaque digest can do none of that.

### Service responsibilities

The service is trusted with the key, so it — not the agent — is where these
belong:

- authenticate the caller (bearer token, mTLS, workload identity)
- verify the network passphrase matches what it is willing to sign for
- apply spend and rate policy per calling identity
- log every signature request, granted or refused
- keep the key in an HSM/KMS and never export it

---

## `SignerAdapter`

Wraps anything already speaking SEP-43 — Freighter and other browser wallets,
an `@ledgerhq`-backed signer, an in-house module:

```typescript
import { SignerAdapter } from '@stellaragent/core';

const agent = await StellarAgent.create({
  network: 'mainnet',
  signer: new SignerAdapter(freighterApi),
});
```

This is the extension point for hardware signing.

---

## Writing your own

Any object with the three methods works — `isSigner` is a duck-typed check, so
`instanceof` across duplicated package copies is not a problem:

```typescript
import type { Signer } from '@stellaragent/core';

const signer: Signer = {
  getPublicKey: async () => 'G...',
  signTransaction: async (xdr, { networkPassphrase }) => signSomehow(xdr, networkPassphrase),
  signAuthEntry: async (xdr, { networkPassphrase, validUntilLedgerSeq }) =>
    signEntrySomehow(xdr, networkPassphrase, validUntilLedgerSeq),
};

const agent = await StellarAgent.create({ network: 'testnet', signer });
```

---

## What changed

| Before | Now |
|--------|-----|
| `create({ secretKey })` only | `create({ signer })` or `create({ secretKey })` |
| secret on `this.keypair` | signing behind a `Signer`; secret in a `#private` field |
| `agent.secretKey` returns the secret | returns it for keypair signers, **throws** for others |
| — | `agent.holdsSecretKey` |
| `fromSecret(secret, network)` | `fromSecret(secret, network, options)` |

`secretKey` and `create({ secretKey })` still work exactly as before, so
nothing needs migrating. Passing both `signer` and `secretKey` throws —
supplying a secret alongside a remote signer would defeat the point.

`agent.secretKey` is marked `@deprecated`: reading key material off a live
agent is the pattern this abstraction exists to remove.

### Read-only methods

`getBalance()` is a Horizon query and touches no contract and no signer. There
are tests asserting it works with a signer that throws on every signing call,
so the read path stays genuinely independent of signing capability.
