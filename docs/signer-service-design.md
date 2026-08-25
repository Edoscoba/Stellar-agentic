# Remote signing service — design doc

Status: **proposed**, for review before implementation.
Area: `services/signer` · Epic: server side of the `RemoteSigner` protocol.

## Problem

[`docs/signing.md`](signing.md) argues that a long-lived agent process should
not hold a raw secret, and the SDK ships `RemoteSigner` plus
`agent.holdsSecretKey` so that claim is checkable. It specifies a three-endpoint
protocol and a list of "service responsibilities".

There is no service. So every production deployment either does the thing the
documentation warns against, or writes this service in-house with nothing to
conform to — and gets to independently rediscover that the caller-supplied
`validUntilLedgerSeq` needs capping, or that a signing service which does not
parse what it signs is just a slower `KeypairSigner`.

This doc proposes the service, and — more importantly — argues the handful of
decisions that are expensive to change later.

## Scope

**In:** the protocol in `docs/signing.md`, a key registry, caller
authentication with rotation and revocation, Ed25519 signing through AWS KMS /
GCP KMS / a local dev keystore, a policy engine, a tamper-evident audit log,
metrics, two conformance suites, a container image and a threat model.

**Out:** key *provisioning* (creating KMS keys is Terraform's job, not the
service's), a UI, multi-tenant billing, and anything that would require the
service to hold funds.

---

## Decision 1: Rust

The alternative was TypeScript, which fits the monorepo better — pnpm/turbo are
already wired, and Phase 5 has to drive the TypeScript `RemoteSigner` anyway.
It was a close call and the tooling argument is real.

Rust wins on the thing that actually distinguishes this component: it is the
only process in the system that can move money, and it spends its whole life
parsing attacker-influenced binary input. A signing service's job is to decode
XDR supplied by a caller that may already be compromised, and then decide
whether to sign it. That is a parser sitting directly in front of a signing
oracle. Memory-unsafe or dynamically-typed handling of that input is a poor
trade for build-system convenience.

Secondary reasons:

- `stellar-xdr` gives exhaustive `match` over envelope and operation variants,
  so "a transaction shape we have never seen" is a compile error to handle
  rather than a runtime `undefined` that falls through to signing.
- `sdk/rust` already established the pattern for a second Cargo workspace in
  this repo, and the CI job to build one.
- The deployable artifact is a static binary in a distroless image, with no
  runtime to patch and no `node_modules` in the supply chain of a process
  holding signing authority.

Phase 5's conformance suite drives the TypeScript client over HTTP, so it does
not care what the server is written in — see [Conformance](#conformance-two-suites-not-one).

### Not depending on the `stellaragent` SDK crate

Tempting: the SDK already has `ScVal` accessors for exactly the decoding the
policy engine needs. Rejected for two reasons.

The dependency direction is backwards — a service does not depend on its
clients' SDK — and the practical cost is real: `stellaragent` pulls `reqwest`
and a TLS stack, and this process should not inherit an HTTP *client* and its
transitive supply chain to reuse fifty lines of accessor code.

The service depends on `stellar-xdr`, `stellar-strkey` and `ed25519-dalek`
directly. The ~50 lines of `ScVal` accessors are duplicated, deliberately, and
pinned by shared test vectors rather than by a shared crate.

---

## Decision 2: what the protocol actually needs to say

`docs/signing.md` is the normative spec and the wire format does not change —
the shipped TypeScript and Rust `RemoteSigner`s must work against this service
unmodified. But implementing it surfaced seven under-specified points. Each is
either a decision this service makes and documents, or a backwards-compatible
addition.

| # | Gap in `docs/signing.md` | Proposed resolution |
|---|---|---|
| 1 | `/v1/public-key` takes no identity parameter | It returns the key bound to the **authenticated caller**. A caller can never ask for someone else's key, which is why there is no parameter. To be stated explicitly in the spec. |
| 2 | `validUntilLedgerSeq` is caller-supplied and unbounded | The service **caps** it at `current_ledger + policy.max_auth_validity_ledgers` (default 100, matching the SDK's own choice). A caller asking for more is refused, not silently clamped — silent clamping produces a signature the caller did not expect. |
| 3 | No idempotency or request identity | Accept an optional `X-Request-Id` header; generate one otherwise. It appears in the audit record and in the response, so a caller can correlate a refusal with a log line. |
| 4 | Nothing says what to do with an envelope that already carries signatures | Refuse. The service co-signs nothing: a partially-signed envelope arriving here means either a multisig flow this service does not implement, or an attempt to smuggle an extra signer. |
| 5 | Errors are `{ "error": "<message>" }` only | Keep that field exactly (the SDK surfaces it verbatim), and **add** `reason` and `violations` alongside. Existing clients ignore unknown fields; a policy dashboard gets machine-readable output. Backwards compatible by construction. |
| 6 | No statement that the source account must be the service's own key | Enforce it. A transaction whose `source_account` is not the address this key signs for is refused — the signature would be useless at best and is a probe at worst. |
| 7 | No health/readiness endpoints | Add `/healthz` (liveness, no auth) and `/readyz` (backend reachable, no auth). Neither reveals key material or identity. |

Point 5 is the one worth arguing in review: it is the difference between a
refusal a human reads in a log and a refusal a fleet can alert on.

---

## Architecture

```
services/signer/
  Cargo.toml                 # own workspace, like sdk/rust
  config.example.toml
  Dockerfile
  src/
    main.rs                  # config → wiring → serve
    config.rs                # typed config, validated at startup
    protocol.rs              # the wire types from docs/signing.md
    error.rs                 # RefusalReason → (status, body)
    http/                    # routes + middleware (auth, request-id, tracing)
    auth/                    # token hashing, rotation, revocation
    registry.rs              # caller identity → KeyRef + PolicyId
    backend/                 # SigningBackend trait + local/aws/gcp + conformance
    inspect/                 # decode tx and auth entry into a reviewable summary
    policy/                  # Policy, Decision, Violation, rules/
    audit/                   # hash-chained append-only records + sinks
    metrics.rs
    sign.rs                  # the one orchestration path
  conformance/               # Phase 5: separate crate, black-box, --url/--token
  tests/
```

Every signing request follows exactly one path, and it is short enough to read
in one sitting:

```
authenticate  →  resolve key + policy  →  decode  →  evaluate policy
                                                          │
                                     deny ────────────────┤
                                                          │ allow
                                          backend.sign(32-byte payload)
                                                          │
                                                     audit + respond
```

The audit write happens on **both** branches, before the response is sent. A
refusal that is not recorded is indistinguishable from a request that never
arrived.

---

## Decision 3: the backend interface

Everything both Stellar signing operations need reduces to the same primitive.
A transaction signature is Ed25519 over `SHA-256(TransactionSignaturePayload)`;
an auth-entry signature is Ed25519 over
`SHA-256(HashIdPreimage::SorobanAuthorization)`. Both are 32 bytes in, 64 bytes
out. So the backend trait is deliberately tiny:

```rust
#[async_trait]
pub trait SigningBackend: Send + Sync {
    fn id(&self) -> &str;
    async fn public_key(&self, key: &KeyRef) -> Result<[u8; 32], BackendError>;
    async fn sign(&self, key: &KeyRef, payload: &[u8; 32]) -> Result<[u8; 64], BackendError>;
    async fn health(&self) -> Result<(), BackendError>;
}
```

**There is no method that can return private key material, and no
`export`/`unwrap`/`raw` escape hatch.** "Never return key material" is
enforced by the shape of the trait rather than by a rule someone has to
remember. A backend that could export a key would have to change this file,
which is a reviewable event.

The fixed-size `[u8; 32]` / `[u8; 64]` are load-bearing too: a `&[u8]` payload
would let a caller-controlled length reach a backend, and some HSM APIs behave
differently on unexpected input sizes.

### Ed25519 through cloud KMS is possible — but only recently

This was worth verifying before committing to Phase 2, because for most of this
project's life it was **not** possible:

- **AWS KMS** gained EdDSA in **November 2025** — key spec
  `ECC_NIST_EDWARDS25519`, algorithm `ED25519_SHA_512` with `MessageType: RAW`.
  Before that, AWS KMS had no Ed25519 at all and this epic could not have been
  built as specified; the fallback would have been CloudHSM or envelope-
  encrypting a key and decrypting it in memory, which is a materially weaker
  design.
- **GCP Cloud KMS** has `EC_SIGN_ED25519`, documented as *"EdDSA on Curve25519
  in PureEdDSA mode, which takes raw data as input"*.

Both are **PureEdDSA over the raw message** — no pre-hashing inside the KMS.
That is what makes Phase 2's "the same output bytes" requirement achievable
rather than aspirational: we hand all three backends the identical 32 bytes and
get the identical 64 back. `ED25519_PH_SHA_512` (AWS's pre-hashed variant, with
`MessageType: DIGEST`) would produce a *different, invalid* signature for
Stellar. The adapter must pin the pure variant, and the conformance suite
asserts cross-backend byte equality precisely to catch someone "fixing" this
later.

### The local keystore is dev-only, and says so at startup

The local backend keeps an encrypted keystore on disk for development and
testnet. It refuses to start unless the config explicitly sets
`backend.local.acknowledge_insecure = true`, and it logs a warning on every
startup. A service that quietly accepts a file-backed key in production is the
failure this whole epic exists to prevent.

---

## Decision 4: identity, tokens, rotation

Callers present a bearer token. Tokens are 256-bit random values, stored as
`SHA-256(token)` and compared in constant time via `subtle`.

**Not Argon2**, deliberately, and this will come up in review: password hashing
exists to make *low-entropy* secrets expensive to brute-force. A 256-bit random
token has nothing to brute-force, and putting Argon2 on the hot path of every
signing request buys no security while adding tens of milliseconds and a memory-
hard workload that is itself a DoS surface. The relevant properties here are
constant-time comparison and never logging the token, both of which we do.

A token record carries: `id`, `subject` (the agent identity), `created_at`,
`expires_at`, `revoked_at`, `label`.

- **Rotation** is overlap, not swap: an identity may hold several unexpired
  tokens, so a new one is issued and adopted before the old one is revoked, with
  no window in which the agent cannot sign.
- **Revocation** is checked per request, not cached, so it takes effect on the
  next request rather than at the next restart.

The key registry maps `subject → (KeyRef, PolicyId)`. It is a one-way lookup on
purpose: a caller names no key, so a compromised caller cannot pivot to another
agent's key by asking nicely.

---

## Decision 5: policy, and "never sign blind"

The policy engine is the reason this service returns `signedXdr` rather than a
signature over an opaque digest — `docs/signing.md` already argues that, and
this is the part that cashes the argument.

### Decoding first

`inspect/` turns an envelope into a reviewable summary before any rule runs:
envelope version, source account, sequence, time bounds, and per operation the
contract address, function name, and decoded arguments. For the payment path it
extracts amounts (`i128` in stroops) and recipients (`Address`) from the known
`payment_channel` / `escrow` / `rate_limiter` signatures.

Anything it cannot fully decode is **refused, not signed**. That is the whole
point: a policy engine that shrugs at an unrecognised operation and signs it is
strictly worse than no policy engine, because it produces an audit trail that
looks like diligence.

### Evaluation model

Deny by default. Every rule runs, and **all** violations are collected rather
than short-circuiting on the first — the same choice `math::predict_payment_outcome`
makes in the SDK, and for the same reason: an operator fixing a policy wants
the full list, not to discover the second problem after fixing the first.

Rules, per key:

| Rule | Refuses when |
|---|---|
| `network` | the `networkPassphrase` is not one this key signs for |
| `amount_cap` | any single payment exceeds `max_amount_stroops` |
| `recipient_allowlist` | a recipient is not on the list (empty list = deny all) |
| `contract_allowlist` | the contract or function invoked is not permitted |
| `rate_limit` | request count or cumulative amount exceeds the window budget |
| `time_window` | the request falls outside permitted hours, or the token has expired |
| `auth_validity` | requested `validUntilLedgerSeq` exceeds the cap (gap #2) |

Policy lives in TOML, loaded at startup and on `SIGHUP`. Config-as-file rather
than an API is a deliberate constraint: a policy change should be a reviewed,
diffable, git-tracked event, and there should be no endpoint that can loosen a
spend ceiling.

### On replay

Worth being precise, because "replay protection" is easy to claim and easy to
get wrong.

A replayed *transaction* signing request is not itself dangerous: the envelope
carries a sequence number, so the network applies it at most once. The real
exposures are (a) a stolen token being used to request *new* signatures — which
is what policy and revocation address, not replay detection — and (b) an
**auth entry** whose `signature_expiration_ledger` is far in the future, which
stays replayable on-chain for as long as it is valid. Gap #2's cap is the
control that matters.

We additionally refuse to re-sign a byte-identical envelope within a short
window. This is defence in depth, not a security boundary: it makes the audit
log unambiguous and turns a retry storm into a visible refusal.

---

## Decision 6: audit log

Append-only JSONL. Every record carries `prev_hash`, and
`hash = SHA-256(canonical_json(record without hash) ‖ prev_hash)`, so any
edit or deletion breaks the chain from that point on.

A record holds: timestamp, request id, subject, key ref, operation, the decoded
summary, the decision, any violations, the envelope hash, and — when signing
happened — the signature. It never holds the token, and it structurally cannot
hold key material.

**What this does not give you:** a hash chain is tamper-*evident*, not
tamper-*proof*. Anyone who can rewrite the log can recompute the whole chain.
It only becomes evidence once the head hash is anchored somewhere the attacker
cannot reach — shipped to a separate account's log sink, or periodically
published. The deployment guide will say this in those words rather than
implying the chain alone is sufficient.

Sinks are pluggable (stdout JSONL, file). A failed audit write **fails the
request**: signing something we cannot record is exactly the case the log
exists for.

---

## Conformance: two suites, not one

Phase 2 and Phase 5 both say "conformance suite" and they are different things.
Conflating them is how one of them ends up not existing.

**Backend conformance (Phase 2)** — an in-process Rust harness,
`assert_backend_conforms(&backend)`, that any adapter must pass: the public key
is stable across calls; a signature verifies under that public key; the *same*
payload yields byte-identical output across backends; error taxonomy is
respected; concurrent signing is safe. It is exported from the crate so a
third-party backend can run it.

**Protocol conformance (Phase 5)** — a separate black-box crate driving HTTP
against `--url`, so it can verify *any* implementation of the protocol in any
language. It has two halves, and Phase 5's wording ("verify the TS RemoteSigner
and the future Python one") is really about the second:

- *Server conformance*: point it at a running service; it asserts endpoint
  shapes, status codes, the `{ error }` body on refusal, and the auth
  behaviour.
- *Client conformance*: a reference server that asserts the **client's**
  requests are well-formed — correct paths, `Authorization` header, JSON field
  names, `expectedPublicKey` enforcement — and returns canned responses. This
  is what the TypeScript and Python `RemoteSigner`s get run against.

---

## Threat model

### What this protects against

- **Agent process compromise or memory dump.** The agent holds a URL and a
  token. There is no key to find.
- **Secret leakage through the usual channels** — `process.env`, a heap dump,
  an error reporter serialising the object graph, a log line.
- **Unbounded spend by a compromised agent.** A stolen token buys the ability
  to *request* signatures subject to policy — capped per transaction, per
  window, and per recipient — not the key.
- **Credential rotation without fund migration.** Rotating a token is a config
  change; rotating a key would mean moving funds off every account.
- **Absence of an audit trail.** Every request, granted or refused, is
  recorded before the response is sent.

### What it does not protect against

Stated plainly, because a threat model that only lists wins is marketing.

- **Compromise of the signing service itself.** It is now the crown jewels. An
  attacker with code execution here signs whatever policy allows, and can
  rewrite the audit log unless the head hash is anchored externally.
- **A caller acting maliciously within policy.** If policy permits 10 XLM/hour
  to an allowlisted recipient, a compromised agent can spend exactly that,
  every hour, and every request will be correctly authorised and cleanly
  logged. Policy tightness is an operator decision the service cannot make.
- **Compromise of the KMS credentials.** If the service's IAM role is stolen,
  the thief signs directly against KMS and never touches this service, its
  policy, or its log. KMS-side key policy and CloudTrail are the controls
  there, not this codebase.
- **A malicious or coerced operator.** Whoever can change `policy.toml` and
  restart the service can authorise anything. The mitigation is process —
  code review on the policy file, separation of duties — not code.
- **Transport misconfiguration.** The service refuses plaintext outside
  loopback, but if TLS terminates at a proxy the operator controls, the segment
  behind it is theirs to secure.
- **Semantic attacks on the policy engine.** Policy reasons about what it can
  decode. A contract call shaped so its real effect is not visible in the
  arguments we inspect would pass. This is why unrecognised operations are
  refused rather than allowed — the failure mode is availability, not silent
  authorisation — but it bounds the guarantee to "we understood what we
  signed", not "what we signed was safe".
- **Availability.** This service becomes a hard dependency for every payment.
  It is a single point of failure by construction, and the rate-limit state is
  in-memory (see below).

### Known limitation: horizontal scaling

Rate-limit budgets live in memory. Two replicas means two independent budgets,
so a `10/hour` limit silently becomes `20/hour`, and a restart resets the
window. **v1 is single-replica**, and the deployment guide will say so rather
than leaving an operator to discover it from a bill.

Shared state (Redis, or a small persistent store) is the obvious follow-up. It
is deliberately out of scope here because it turns a self-contained service
into one with a stateful dependency, and that trade deserves its own discussion
rather than being smuggled in.

---

## Phase plan

Stacked, each green on its own, in the order the issue asks for.

| Phase | Contents | Est. |
|---|---|---|
| 1 | Protocol, key registry, token auth with rotation/revocation, HTTP surface | ~1,200 |
| 2 | `SigningBackend` + local/AWS/GCP adapters + backend conformance harness | ~900 |
| 3 | Decoding, policy rules, evaluation model, machine-readable refusals | ~1,000 |
| 4 | Hash-chained audit log, sinks, metrics, alerting guidance | ~700 |
| 5 | Protocol conformance crate (server half and client half) | ~700 |
| 6 | Dockerfile, health checks, deployment guide, this threat model | ~600 |
| 7 | Integration and adversarial tests across backends and policies | ~1,100 |

---

## Open questions for review

These are the ones where I would rather have your answer than my assumption.

1. **Is `403 + { error, reason, violations }` the right refusal shape?** It
   keeps `error` verbatim for the shipped clients, but it does put policy
   detail on the wire. An alternative is a generic refusal with the detail
   available only in the audit log — safer against a caller probing the policy
   boundary, worse for an operator debugging a legitimate failure.
2. **Should the local keystore exist at all?** It makes development and the
   conformance suite pleasant, and it is a file with a key in it. The guard is
   an explicit `acknowledge_insecure` flag; the alternative is to make the dev
   path a KMS emulator instead.
3. **Single-replica for v1** — acceptable, or should shared rate-limit state be
   in scope from the start?
4. **Do we want mTLS in Phase 1**, or is bearer-token auth behind an operator's
   own TLS termination the right v1 boundary?
5. **Should the service verify the transaction against live network state**
   (simulate against RPC before signing)? Phase 3 says "simulate and inspect".
   Full simulation means the signer needs RPC access and an opinion about which
   RPC to trust, and it couples signing availability to RPC availability. My
   inclination is to *inspect* thoroughly and *not* simulate, and to argue that
   in review — but it is a direct reading of the phase description, so I want it
   settled before building.

Question 5 is the one I would most like resolved first; it changes the
service's dependency surface.
