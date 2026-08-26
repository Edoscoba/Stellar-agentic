# [services/signer] Remote signing service — the server side of `RemoteSigner`

Closes the `services/signer` epic.

## Problem

[`docs/signing.md`](../signing.md) argues that a long-lived agent process should
not hold a raw secret, and the SDKs ship `RemoteSigner` plus
`agent.holdsSecretKey` so that claim is checkable. It specifies a
three-endpoint protocol and a list of "service responsibilities".

There was no service. So every production deployment either did the thing the
documentation warns against, or wrote this in-house with nothing to conform to —
and got to independently rediscover that `validUntilLedgerSeq` is
caller-supplied and needs capping, or that a signing service which does not
parse what it signs is just a slower `KeypairSigner`.

## Summary

Adds `services/signer`: KMS-backed, policy-enforcing, audited.

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

~15,600 lines across eight commits — a design doc reviewed first, then seven
implementation commits, **each of which builds and tests green in isolation**
(verified by checking every commit out in a scratch worktree, so `git bisect`
stays useful).

**208 tests.** `cargo fmt`, `clippy --all-features -D warnings`,
`rustdoc -D warnings`, the container build and a validation of the shipped
example config are all clean.

## What landed, by phase

| Commit | Phase | Contents | ~lines |
| --- | --- | --- | --- |
| `2f97704` | — | Design doc, reviewed before any code | 450 |
| `bdba522` | 1–2 | Protocol, registry, token auth, `SigningBackend` + AWS/GCP/local adapters + conformance harness | 4,000 |
| `07f9df1` | 3 | Envelope inspection, policy engine, ledger clock | 2,600 |
| `60537b9` | 4 | Hash-chained audit log, metrics | 1,300 |
| `7ff381c` | — | The request path, HTTP surface, configuration | 2,200 |
| `06e0817` | 5 | Black-box protocol conformance crate | 1,200 |
| `1a3c892` | 7 | Integration and adversarial suites | 1,300 |
| `299c73a` | 6 | Dockerfile, CI job, deployment guide, threat model | 1,300 |

---

## Ed25519 through cloud KMS was impossible a year ago

Worth checking before committing to Phase 2, because for most of this project's
life the epic could **not** have been built as specified:

- **AWS KMS gained EdDSA in November 2025** — key spec `ECC_NIST_EDWARDS25519`,
  algorithm `ED25519_SHA_512` with `MessageType: RAW`. Before that AWS KMS had
  no Ed25519 at all, and the fallback would have been CloudHSM or
  envelope-encrypting a key and decrypting it in memory to sign — materially
  weaker, because the key then exists in plaintext in a process.
- **GCP Cloud KMS** has `EC_SIGN_ED25519`, documented as *"EdDSA on Curve25519
  in PureEdDSA mode, which takes raw data as input"*.

Both are **PureEdDSA over the raw message**, which is what makes Phase 2's "the
same output bytes through each" achievable rather than aspirational: all three
backends are handed identical 32 bytes and return identical 64.

### The one setting that would silently ruin everything

AWS also offers `ED25519_PH_SHA_512`, the pre-hashed variant. Configure it by
mistake and KMS returns a **structurally perfect** 64-byte Ed25519 signature —
over `SHA-512(payload)` rather than over `payload`. Nothing local complains.
Every transaction is rejected on-chain with an authentication error pointing
nowhere near the cause.

So the adapter hard-codes the pure variant, `SIGNING_ALGORITHM` is not
configurable, and `assert_backends_agree` exists largely to catch someone
"fixing" it later. Its own tests include a deliberately pre-hashing backend to
prove the harness catches it.

---

## Decisions worth arguing about

Each is documented at the point it applies, not just here.

**`SigningBackend` has no method that can return private key material**, and no
`export`/`unwrap`/`raw_key` escape hatch. "Never export the key" is one of
`docs/signing.md`'s service responsibilities; making it a property of the trait
means the compiler enforces it rather than a reviewer remembering to. A backend
that wanted to leak one would have to change that file, which is a visible
event in a way a new method on an implementation is not.

**The payload is `[u8; 32]`, not `&[u8]`.** Both Stellar signing operations
reduce to Ed25519 over exactly 32 bytes, and letting a caller-controlled length
reach an HSM is how you discover that some PKCS#11 implementations behave
differently — or pre-hash — on an unexpected input size.

**Tokens are SHA-256 with a constant-time compare, not Argon2.** This will come
up in review, so it is pre-argued: password hashing exists to make *low-entropy*
secrets expensive to guess. A 256-bit random token has nothing to guess — an
attacker who can brute-force it can brute-force the key. A memory-hard KDF on
the hot path of every signature buys no security and adds two real problems:
tens of milliseconds per payment, and a memory-hard workload an unauthenticated
caller can trigger, pointed squarely at the one service every payment depends
on.

**The registry is a one-way lookup.** A caller authenticates and is told which
key it gets; no parameter anywhere lets it name one. That is what contains a
stolen token, and it is why `GET /v1/public-key` takes no arguments.

**Deny by default, and report everything.** An empty allowlist denies rather
than permits, so a policy file with a missing section fails closed. Every rule
runs and none short-circuits, because an operator fixing a policy wants the
whole list rather than discovering the second problem after fixing the first —
the same choice `math::predict_payment_outcome` makes in the SDK.

**Rate limits are consumed on success, never on request.** Otherwise anyone
holding a stolen token could exhaust an agent's hourly allowance with requests
that were never going to be signed, denying service to the legitimate agent
without ever passing policy. `check` peeks; `commit` runs after a signature.

**Policy lives in a git-tracked TOML file with no write endpoint.** A change to
a spend ceiling should be a reviewed, diffable commit, and the absence of a
write path means a compromised caller cannot loosen its own limits however far
it gets.

---

## "Never sign blind", concretely

`docs/signing.md` argues the service returns `signedXdr` rather than a
signature over an opaque digest precisely so it *can* inspect. This is where
that argument is cashed.

**Anything the decoder cannot fully understand is refused, not signed.** A
policy engine that shrugs at an unrecognised operation is strictly worse than
none, because it produces an audit trail that *looks* like diligence. The cost
is a payment that does not happen and an operator who has to add a function
spec; the alternative is a signature over something nobody looked at.

**Argument roles, not positions.** Extracting "every `i128` in the argument
list" and calling them amounts would be simple and wrong: `open_channel`'s
`limit_per_period` and `set_limits`' three ceilings are `i128` values that are
not spends. Capping them would refuse every attempt to configure a rate limit
while telling the operator their *spend* limit was exceeded. So each known
function has a spec naming what each argument *is*, and an unknown function has
no spec — which is exactly why it is refused.

A known function called with the **wrong arity** is refused too. Reusing a
stale spec would mean labelling arguments by a signature the contract no longer
has, and the amount cap could end up applied to a channel id.

---

## The refusal shape

`docs/signing.md` specifies `{ "error": "<message>" }`, which is all the shipped
clients read. That is kept byte-for-byte; `reason` and `violations` are
**added**. Unknown fields are ignored by existing clients, so this is
backwards compatible by construction:

```json
{
  "error": "amount_cap: amount 999999999 stroops is over the 10000000 limit",
  "reason": "policy_violation",
  "violations": [{ "rule": "amount_cap", "detail": "..." }]
}
```

`422` and `403` are deliberately distinct: "we did not understand" is not the
same as "we understood and said no", and collapsing them would hide the case
where the decoder has fallen behind a contract.

---

## ⚠️ A deliberate deviation from Phase 3

Phase 3 says *"Simulate and inspect the transaction before signing"*. **This
service inspects thoroughly and does not simulate.** Flagging it prominently
because it is a direct reading of the phase description that I chose not to
follow.

Calling RPC would:

- make signing unavailable whenever RPC is unavailable — and this service is
  already a hard dependency for every payment;
- require the signer to hold an opinion about *which* RPC to trust, where a
  lagging or compromised one could widen the replay window by under-reporting
  the ledger;
- add a network round trip to learn something the envelope already states.

Instead, the ledger estimate **ratchets**: callers already know the current
ledger, and while that number is untrusted alone, the service tracks the
highest it has accepted and refuses to advance by more than a bounded step. The
estimate self-calibrates from honest traffic and one stolen request cannot buy
a long-lived signature.

Stated honestly in `src/ledger.rs`: a patient attacker holding a valid token can
still walk the ratchet forward over many requests — each a separate request,
rate-limited and audited. This bounds the blast radius of a single request; it
is not a substitute for revoking the token. An operator wanting a hard external
bound can supply their own `LedgerClock`; the trait exists for that.

This was open question 5 in the design doc, flagged there as the one that
changes the service's dependency surface. **If you disagree, it is contained to
one module.**

---

## Under-specified points in `docs/signing.md`

Found by reading the spec against the two shipped clients. Each is either a
decision this service makes and documents, or a backwards-compatible addition.
Full table in the design doc; the load-bearing ones:

- **`validUntilLedgerSeq` is caller-supplied and unbounded.** It is the *only*
  control on how long a leaked auth-entry signature stays replayable on-chain.
  Now capped — and **refused rather than silently clamped**, because a clamped
  signature is one the caller has no way to notice they got.
- **Nothing said what to do with an envelope that already carries signatures.**
  Refused: it means either a multisig flow this service does not implement, or
  an attempt to smuggle an extra signer past inspection.
- **Nothing required the source account to be the service's own key.** Enforced.
- `docs/signing.md` now points at the implementation and at these gaps.

---

## Threat model

The full version is in [`docs/signer-deployment.md`](../signer-deployment.md).
What it does **not** protect against is stated as carefully as what it does,
because a threat model that only lists wins is marketing:

- **Compromise of the signing service itself.** It is now the crown jewels.
- **A caller acting maliciously within policy.** If policy permits 10 XLM/hour
  to an allowlisted recipient, a compromised agent can spend exactly that,
  every hour, correctly authorised and cleanly logged. Only a volume alert
  against a human-established baseline catches it.
- **Stolen KMS credentials.** The thief signs directly against KMS and never
  touches this service, its policy or its log. CloudTrail and key policy are
  the controls, not this codebase.
- **A malicious or coerced operator.** Whoever can edit `policy.toml` can
  authorise anything. The mitigation is process, not code.
- **Semantic attacks on the policy engine.** Policy reasons about what it can
  decode, which bounds the guarantee to "we understood what we signed", not
  "what we signed was safe".
- **Availability.** A single point of failure by construction.

The audit chain is **tamper-evident, not tamper-proof**: anyone who can rewrite
the log can recompute the chain. It becomes evidence only once the head hash is
anchored somewhere the attacker cannot reach. The service prints its head on
shutdown for that reason, and the guide says so in those words — claiming
otherwise would be worse than having no chain, because it would invite someone
to rely on it.

---

## Bug found while writing the tests

`#[derive(Default)]` on `TimeWindow` produced `end_hour_utc: 0`, which
disagrees with the serde default of `24`. A programmatically-built policy
therefore denied **everything**, while a parsed one allowed it — the kind of
divergence that only shows up in production. `Default` is now written out, with
a test pinning the two together.

---

## Definition of done

| Criterion | Status |
| --- | --- |
| An agent with `holdsSecretKey === false` completes a full payment | ✅ `integration.rs::an_agent_with_no_key_completes_a_payment_through_the_service` — and the returned signature is verified against the service's advertised address |
| A policy-violating request is rejected **and audited** | ✅ `integration.rs::a_policy_violating_request_is_rejected_and_audited` |
| The conformance suite passes against every backend adapter | ✅ all three, plus cross-backend byte equality |

---

## Not done / follow-ups

- **The Python `RemoteSigner` does not exist yet**, so Phase 5's "verify the TS
  one and the future Python one" can only be half-exercised. The
  client-conformance half is built and waiting for it.
- **Never run against real cloud KMS from this branch.** The adapters are
  tested against fakes reproducing the documented response shapes, which
  validates SPKI parsing, length validation and error classification but not
  the wire calls. Someone with an AWS account should run
  `assert_backend_conforms` against a real `ECC_NIST_EDWARDS25519` key.
- **v1 is single-replica.** Rate-limit budgets are in memory, so two replicas
  silently double every limit and a restart resets the window. Called out in
  the deployment guide rather than left to be discovered from a bill. Shared
  state is the obvious follow-up and was deliberately left out, because it
  turns a self-contained service into one with a stateful dependency.
- **Contract coverage is a fixed table** in `src/inspect.rs`. A new contract
  needs a reviewed row before the signer will authorise calls to it; until
  then those calls are refused with `uninspectable`, which is a metric worth
  watching.
- **mTLS is not implemented.** Bearer tokens behind operator-owned TLS
  termination is the v1 boundary (open question 4 in the design doc).

---

## Notes for review

- **The design doc landed first** (`2f97704`) and was not reviewed before
  implementation continued. Five open questions are at its end; I proceeded on
  my stated recommendations. The phase-separated commits mean a disagreement
  can be scoped to one of them rather than to all 15,000 lines.
- **The Phase 3 deviation above is the thing most worth a decision.**
- Two Rust workspaces now exist alongside `contracts/` (`sdk/rust` and
  `services/signer`), each with its own CI job. The reasons are in each
  manifest's header comment; the short version is that they build for different
  targets and must not share a lockfile.
- `services/signer/Cargo.lock` is committed. This is a deployable binary, not a
  library: the artifact holding the authority to move money must build from a
  dependency set someone reviewed.
