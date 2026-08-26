# stellaragent-signer

> The server side of the `RemoteSigner` protocol in
> [`docs/signing.md`](../../docs/signing.md).
> KMS-backed, policy-enforcing, audited.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.82%2B-orange)](https://www.rust-lang.org)

---

## Why this exists

`docs/signing.md` argues that a long-lived agent process should not hold a raw
secret, and the SDKs ship `RemoteSigner` plus `agent.holdsSecretKey` so that
claim is checkable. It specifies a three-endpoint protocol and a list of
"service responsibilities".

There was no service. So every production deployment either did the thing the
documentation warns against, or wrote this in-house with nothing to conform to —
and got to independently rediscover that `validUntilLedgerSeq` is caller-supplied
and needs capping, or that a signing service which does not parse what it signs
is just a slower `KeypairSigner`.

```
agent process                    signing service              AWS / GCP KMS
─────────────                    ───────────────              ─────────────
holds a URL + token   ──────▶    authenticate
                                 resolve key + policy
                                 decode the envelope
                                 evaluate policy
                                 audit  ──────────▶ append-only, hash-chained
                      ◀──────    signed XDR        ◀────────  sign(32 bytes)
```

The agent never holds a key. What it holds is the ability to *request*
signatures, subject to policy.

---

## Quick start

```bash
# 1. A credential
cargo run -- issue-token

# 2. A config — start from the example
cp config.example.toml config.toml
$EDITOR config.toml

# 3. Check it before anything binds a port
cargo run -- check --config config.toml

# 4. Serve
cargo run -- serve --config config.toml
```

Then point an agent at it:

```typescript
const agent = await StellarAgent.create({
  network: 'testnet',
  signer: new RemoteSigner({
    url: 'http://127.0.0.1:8443',
    token: process.env.SIGNER_TOKEN,
    expectedPublicKey: process.env.AGENT_ADDRESS,
  }),
});

agent.holdsSecretKey;  // false
```

Full runbook, including KMS key creation and the threat model:
[`docs/signer-deployment.md`](../../docs/signer-deployment.md).

---

## What it does that a naive signer does not

**It reads what it signs.** Every envelope is decoded and every contract call
matched against a known signature. `docs/signing.md` argues the service returns
`signedXdr` rather than a signature over an opaque digest precisely so it *can*
inspect; this is where that argument is cashed.

Anything it cannot fully decode is **refused, not signed**. A policy engine that
shrugs at an unrecognised operation is strictly worse than none, because it
produces an audit trail that looks like diligence.

**It reasons about argument roles, not positions.** Treating every `i128` as an
amount would be simple and wrong: `open_channel`'s `limit_per_period` and
`set_limits`' three ceilings are `i128` values that are not spends. Capping them
would refuse legitimate configuration calls while telling the operator their
*spend* limit was exceeded.

**It reports every violation, not the first.** An operator fixing a policy wants
the whole list.

**It caps `validUntilLedgerSeq`.** Caller-supplied and unbounded in the spec,
and the single control on how long a leaked auth-entry signature stays
replayable on-chain. Refused rather than silently clamped — a clamped signature
is one the caller cannot reason about.

---

## The refusal shape

`docs/signing.md` specifies `{ "error": "<message>" }`, which is all the shipped
clients read. That is kept byte-for-byte, and `reason` and `violations` are
**added** — unknown fields are ignored by existing clients, so this is
backwards compatible by construction:

```json
{
  "error": "amount_cap: amount 999999999 stroops is over the 10000000 limit",
  "reason": "policy_violation",
  "violations": [
    { "rule": "amount_cap", "detail": "amount 999999999 stroops is over the 10000000 limit" }
  ]
}
```

A human reading a `SigningError` gets the first field; a dashboard gets the rest.

| Status | Meaning |
|---|---|
| `401` | not authenticated, or the credential is expired/revoked |
| `403` | policy said no — the status `docs/signing.md` reserves for refusals |
| `400` | not a valid protocol message, or not valid XDR |
| `422` | well-formed, but we could not understand it well enough to judge |
| `503` | our problem: the backend or the audit sink is unavailable |

`422` and `403` are deliberately distinct: "we did not understand" is not the
same as "we understood and said no", and collapsing them would hide the case
where the decoder has fallen behind a contract.

---

## Backends

| Backend | Key spec | Use for |
|---|---|---|
| `aws-kms` | `ECC_NIST_EDWARDS25519` / `ED25519_SHA_512` | production |
| `gcp-kms` | `EC_SIGN_ED25519` | production |
| `local` | a file of seeds | development only, and it says so on every startup |

Cloud KMS support for Ed25519 is newer than most people expect — **AWS KMS only
gained EdDSA in November 2025**. Before that, a Stellar signing service on AWS
had to use CloudHSM or envelope-encrypt a key and decrypt it in memory, which is
materially weaker.

Both clouds expose **PureEdDSA over the raw message**, which is what makes "the
same output bytes through each backend" achievable rather than aspirational.

Adding a backend means implementing three methods and passing the harness:

```rust
use stellaragent_signer::backend::conformance;

conformance::assert_backend_conforms(&my_backend, &key).await?;
// ...and, against a reference holding the same key:
conformance::assert_backends_agree(&local, &local_key, &my_backend, &key).await?;
```

The second one is the important one. Ed25519 is deterministic, so byte-identical
output is a legitimate equality check — and it is the only local check that
catches a backend configured for a pre-hashed variant, which produces a
perfectly well-formed signature over the wrong message.

---

## Testing

```bash
cargo test --workspace                    # unit, integration, adversarial
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

- `tests/integration.rs` — the definition-of-done cases, driven through the
  real axum router rather than by calling the service struct.
- `tests/adversarial.rs` — attempts to get a signature the service should not
  give: envelope smuggling, cap splitting, ledger ratcheting, log injection.
  Each test is written from the attacker's side and records which control stops
  it.

---

## Layout

| Path | What lives there |
|---|---|
| `src/protocol.rs` | the wire types, exactly as `docs/signing.md` specifies |
| `src/inspect.rs` | decoding an envelope into something policy can reason about |
| `src/policy.rs` | the rules, and the deny-by-default evaluation model |
| `src/backend/` | the `SigningBackend` trait, adapters, and the conformance harness |
| `src/audit.rs` | the hash-chained append-only log |
| `src/auth.rs` | token hashing, rotation, revocation |
| `src/ledger.rs` | the ratcheting current-ledger estimate |
| `src/sign.rs` | the one path a request takes |
| `conformance/` | black-box protocol conformance, for any implementation |

---

## Design notes

Argued in [`docs/signer-service-design.md`](../../docs/signer-service-design.md);
the ones most likely to come up in review:

- **Rust, not TypeScript.** Close call. This is the one process that can move
  money and it spends its life parsing attacker-influenced XDR — a parser in
  front of a signing oracle.
- **No dependency on the `stellaragent` SDK crate.** The direction is backwards,
  and it would drag an HTTP client and a TLS stack into the process holding
  signing authority.
- **SHA-256 + constant-time compare for tokens, not Argon2.** Password hashing
  exists for low-entropy secrets. A 256-bit random token has nothing to
  brute-force, and a memory-hard KDF on the hot path is a DoS surface pointed at
  the service every payment depends on.
- **No method on `SigningBackend` can return key material.** "Never export the
  key" is enforced by the shape of the trait rather than by a rule someone has
  to remember.

---

## License

MIT — see [LICENSE](../../LICENSE).
