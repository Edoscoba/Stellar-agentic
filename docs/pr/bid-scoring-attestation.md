# [SDK] Signed, off-chain-verifiable attestations for bid scoring

## Problem

`scoreBid`/`rankBids`/`selectBestBid` (`packages/core/src/math/bid.ts`) are
pure, deterministic functions, but they are **trusted, off-chain** ones:
whoever runs the scoring service can simply not run it, or run it and then
tell the counterparty about a different "winner" than the one it actually
computed. A worker that's handed only a bid set and a claimed result has no
way to check, after the fact, that the "winning bid" it was told about was
genuinely the top score.

The companion contracts-side "commit-reveal settlement" issue solves this
fully on-chain. This is the lighter-weight, off-chain-verifiable
alternative — useful before that lands, or for callers that don't want full
on-chain settlement overhead at all.

## Summary

- **`attestRankBids(bids, weights, scorerKeypair, options)`** runs
  `rankBids` as normal and signs a sha256 digest of the canonicalized
  `(bids, weights, result)` triple with the scorer's Stellar `Keypair`
  (raw ed25519 `sign()` over arbitrary bytes — not an XDR transaction, so
  this sits alongside the existing `Signer` abstraction rather than going
  through it). Returns `{ result, attestation }`, where `result` is
  identical to a plain `rankBids` call.
- **`verifyBidAttestation(bids, result, attestation, trustedKeys, options)`**
  is the standalone verifier — usable by any worker/observer with zero
  access to the scorer's process. Given only the bid set, the claimed
  result, the attestation, and a directory of which scorer keys it
  trusts, it:
  1. looks up the attestation's `keyEpoch` in the trusted-key directory
     and rejects an unknown epoch, a public-key mismatch, or an epoch used
     outside its validity window;
  2. rejects an expired attestation;
  3. verifies the ed25519 signature over the attestation header (this
     authenticates every field, including `digest` and `weights`);
  4. recomputes the digest from the given `bids`/`result` and checks it
     matches — catches tampering with either in transit;
  5. **recomputes `rankBids(bids, attestation.weights)` from scratch**,
     using the exported `bid.ts` functions, and checks it structurally
     matches `result` — this is the check that actually catches a scorer
     reporting a different bid than the one it truly computed, which is
     the core threat the issue describes.
- Both functions live in `packages/core/src/math/attestation.ts` and are
  re-exported from `@stellaragent/core`'s root and `math` barrels, next to
  `rankBids` itself.

## Key rotation

An attestation names the `keyEpoch` it was signed under rather than a bare
public key, and `verifyBidAttestation` takes a `ScorerKeyDirectory` —
`{ epoch, publicKey, validFrom?, validUntil? }[]` — instead of a single
hard-coded key:

- Retiring a key is a directory update (give its epoch a `validUntil`, or
  drop it once nothing signed under it needs to verify any more), not a
  breaking change. Attestations already issued before that cutoff keep
  verifying; new ones claiming that epoch after the cutoff don't — so a
  compromised key can be cut off going forward without invalidating
  history.
- `issuedAt`/`expiresAt` bound every attestation's own lifetime
  independently of key rotation (default TTL 300s), so a leaked
  `(bids, result, attestation)` tuple can't be replayed indefinitely as
  "proof" of a stale ranking.

## Why a digest check *and* a full recompute

The digest (`sha256` over canonicalized bids/weights/result, itself
covered by the signature) and the from-scratch `rankBids` recompute are
doing different jobs, and the issue's acceptance criteria needs both:

- The **digest** confirms the verifier's copy of `(bids, weights, result)`
  is byte-identical to what the scorer actually signed — it catches
  tampering with the data in transit between the scorer and the worker.
- The **recompute** confirms the attested result is the *true* top-ranked
  output for that bid set — it catches a scorer that computed the honest
  ranking internally but signed and reported a fabricated one instead.
  Digest-matching alone can't catch this: if the scorer signs its own lie,
  the digest of that lie matches itself perfectly.

Bids are sorted by `agentAddress` before digesting (mirroring
`rankBids`'s own documented input-order-independence), so two bid arrays
containing the same set in different orders digest identically — a
worker shouldn't have to reproduce the scorer's exact array order for the
attestation to verify. `result`'s order is *not* normalized, since that
order is the ranking itself.

## Testing

`pnpm --filter @stellaragent/core test`, `typecheck`, and `lint` all pass
(19 new tests; 1063 pre-existing tests in the package unaffected).

19 tests in `packages/core/src/math/__tests__/attestation.test.ts`:

- **`attestRankBids` — validation** (4) — rejects a public-key-only
  keypair, a negative/non-integer `keyEpoch`, a non-positive `ttlSeconds`,
  and propagates `rankBids`'s own weight-validation `RangeError`.
- **`attestRankBids` — output** (2) — `result` matches a plain `rankBids`
  call exactly; `issuedAt`/`expiresAt`/`keyEpoch`/`scorerPublicKey` are
  stamped correctly from an injected clock.
- **`verifyBidAttestation` — valid attestation** (3) — a genuine
  attestation verifies; verification is independent of the order bids are
  supplied to the verifier in; an otherwise-genuine attestation is
  rejected once its TTL has elapsed.
- **`verifyBidAttestation` — tampered output** (2) — a fully fabricated
  (reversed) result is rejected; a single mutated score is rejected even
  with array order preserved.
- **`verifyBidAttestation` — tampered input** (2) — an altered bid price
  is rejected; an injected extra bid is rejected.
- **`verifyBidAttestation` — untrusted keys** (3) — a signature from a
  key absent from the trusted directory is rejected; a forged attestation
  claiming a trusted epoch but signed by a different keypair is rejected
  (public-key mismatch, not a signature failure); a tampered signature
  byte string is rejected.
- **`verifyBidAttestation` — key rotation** (3) — a still-current epoch
  verifies alongside an already-retired one in the same directory; a
  *new* attestation issued under an already-retired epoch is rejected;
  an attestation issued *before* its key's retirement still verifies when
  checked well after that retirement.

## Out of scope / follow-ups

- On-chain commit-reveal settlement — the companion contracts-side issue;
  this is explicitly the lighter-weight alternative to it, not a
  replacement.
- Nothing in `StellarAgent` consumes this yet (no `requestWork`/escrow
  wiring) — those methods are still stubs pending the Soroban invocation
  work tracked elsewhere; this PR only adds the scoring/verification
  primitives themselves.
- The scorer's private key is assumed to be held wherever `attestRankBids`
  runs (e.g. behind a `RemoteSigner`-style boundary in a real deployment);
  this PR doesn't add a signing-service protocol for it — `scorerKeypair`
  is a plain `Keypair`, matching how the scoring service is expected to
  hold its own key.

## Files changed

- `packages/core/src/math/attestation.ts` — `attestRankBids`,
  `verifyBidAttestation`, canonicalization/digest helpers, and all
  associated types (`BidAttestation`, `ScorerKeyRecord`,
  `ScorerKeyDirectory`, etc.).
- `packages/core/src/math/__tests__/attestation.test.ts` — 19 tests.
- `packages/core/src/math/index.ts`, `packages/core/src/index.ts` —
  barrel + root re-exports.
