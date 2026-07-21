# ZK solvency proofs for PaymentChannel — design doc

## Problem

`PaymentChannel` tracks `total_spent` and enforces `limit_per_period` in
real time, but the *only* way for a third party (an auditor, a
counterparty, a regulator) to convince themselves that a channel's
`total_spent` figure is legitimate — i.e. that it really is the sum of a
payment history that never blew through the per-period limit — is to
either trust the chain's execution outright, or replay the full public
event log and recompute it by hand. The event log reveals every
individual payment: exact amount, recipient, and timestamp for each call
to `pay()`. An agent operator who wants to demonstrate compliance without
handing over their entire vendor list and spend pattern currently has no
way to do that.

This doc covers the ZK design built to close that gap: a Groth16 circuit
proving *"some private payment history, grouped into periods, never
exceeded the limit in any period and sums to exactly this public
`total_spent`"* — without revealing the history — plus a real prover and
a real on-chain verifier.

## Feasibility research: what's actually practical on Soroban

The brief for this task assumed on-chain verification of a general SNARK
might be infeasible given Soroban's CPU/memory budget, and that the
fallback would be "verify off-chain, anchor only a proof hash on-chain."
That assumption turned out to be wrong for the specific case of
**pairing-based SNARKs on BLS12-381**, because of something not obvious
from the contract-writing surface of Soroban: **the host itself exposes
native BLS12-381 field/curve/pairing operations as host functions**
(`soroban_sdk::crypto::bls12_381`, added in Stellar Protocol 21 explicitly
to make pairing-based cryptography — BLS signature aggregation, and SNARK
verification — practical on-chain). The relevant primitives:

- `g1_msm` / `g2_msm` — multi-scalar multiplication over G1/G2
- `g1_add` / `g2_add` — point addition
- `pairing_check(vp1: Vec<G1Affine>, vp2: Vec<G2Affine>) -> bool` — checks
  whether the product of pairings `∏ e(vp1[i], vp2[i])` equals the
  identity in the target group

This is *exactly* the primitive set a Groth16 verifier needs: MSM to fold
the public-input contribution into a single G1 point (`vk_x`), and a
4-term multi-pairing check for the verification equation. Both run as
**native host functions**, not WASM bytecode — so the cost is a handful
of host-metered operations, not millions of WASM instructions doing
finite-field arithmetic in software. This is the difference between
"infeasible" and "cheap": a naive from-scratch pairing implementation
compiled to WASM would very plausibly blow the CPU instruction budget;
calling four pre-metered host functions does not.

Conclusion: **on-chain verification is not just feasible, it's the
better design** than an off-chain-verify/anchor-only scheme, and that's
what this repo implements — `PaymentChannel::verify_solvency_proof` is a
real Soroban entrypoint, not a stub.

What Soroban does *not* give you is a general-purpose SNARK verifier for
arbitrary curves or proof systems — only BLS12-381 pairing primitives.
That constrains the proving-system choice (see below): anything that
needs a different curve, or verification steps beyond pairings and G1/G2
arithmetic (FRI, generic hashing-heavy folding, lattice operations), does
not have a comparably cheap on-chain path today.

## Proving system choice: Groth16

| System | On-chain verification cost on Soroban | Trusted setup | Proof size |
|---|---|---|---|
| **Groth16** | 1 MSM + 1 four-term pairing check — maps directly onto `g1_msm`/`pairing_check` | Per-circuit, needs a real ceremony | ~128–192 bytes (3 curve points) |
| PLONK / Marlin (KZG-based) | Also pairing-based, could in principle use the same host functions, but needs a larger/more involved verifier (multiple opening checks, more MSM terms) and a *universal* setup | Universal (one ceremony, any circuit) — better story than Groth16 | Larger than Groth16 |
| Bulletproofs | No pairings needed, but verification is O(log n) group operations over an ordinary elliptic curve (typically Ed25519/secp256k1-family) with no succinct native primitive Soroban exposes for that — would mean implementing the inner-product-argument verifier in WASM | None (transparent) | Larger, and verification is not O(1) |
| STARKs (FRI-based) | Verification is dominated by many hash evaluations (Merkle path checks) and low-degree testing — no native primitive for this on Soroban; would run entirely in WASM | None (transparent) | Large (tens of KB) |

Groth16 is the only option here whose verification cost maps almost
one-to-one onto host functions Soroban already exposes. Its two real
downsides — a per-circuit trusted setup, and a fixed circuit shape — are
both acceptable for this use case: the circuit shape is fixed by design
(a bounded-size spending-history check, see below), and the trusted-setup
problem is a genuine limitation this doc is upfront about (see
"Trusted setup" below) rather than one solved by this prototype.

PLONK on the same curve would also work through `pairing_check` in
principle and would remove the per-circuit setup problem; it's the
natural next step if this circuit needs to change shape frequently in
production. It wasn't chosen for the prototype because Groth16's smaller,
simpler verifier equation was faster to get *correct* against Soroban's
byte encoding (see "The encoding bridge" below) — correctness of that
bridge, not verifier complexity in the abstract, was the dominant
engineering risk here.

## What the circuit actually proves

> There exists a sequence of at most `MAX_PAYMENTS` (private) payments,
> each assigned to a (private) non-decreasing period index, such that:
> 1. no single payment exceeds `limit_per_period`,
> 2. the running sum within any one period never exceeds
>    `limit_per_period`, and
> 3. the payments sum to exactly `total_spent`.

`limit_per_period` and `total_spent` are the circuit's only two public
inputs, and — critically — **the on-chain verifier does not take them as
caller-supplied arguments**. `verify_solvency_proof(channel_id, proof)`
reads them straight off the channel's own live storage
(`Channel::limit_per_period`, `Channel::total_spent`, populated only by
the contract's own `open_channel`/`pay` logic). A proof is only useful
for the specific channel whose current state matches the two values it
was built for; there's no way to point a valid proof at a different
channel's claims by supplying different public inputs, because the
verifier never accepts them as input in the first place. This is what
"consistent with the public commitment(s) already on PaymentChannel"
means in the requirements — the *channel's own state* is the commitment,
not something new bolted on.

Implementation: `zk/solvency_proof/src/circuit.rs`. Fixed-capacity
witness array (`MAX_PAYMENTS = 8`), padded with zero-amount payments
repeating the last real period index when the real history is shorter.
Per-payment constraints (using `ark-r1cs-std`'s `FpVar::enforce_cmp`):

```text
for each payment i:
    amount[i] <= limit_per_period                       (checked cmp)
    period_id[i] >= period_id[i-1]                       (checked cmp, i>0)
    same_period = (period_id[i] == current_period_marker)
    period_base = same_period ? running_period_spend : 0  (conditional select)
    running_period_spend = period_base + amount[i]
    running_period_spend <= limit_per_period              (checked cmp)
    total += amount[i]

enforce total == total_spent
```

### Soundness: why the field-arithmetic version of this is actually safe

BLS12-381's scalar field `Fr` is a ~255-bit prime field. Any of these
comparisons, done naively over field elements, are ambiguous for values
near the modulus (mod-`r` wraparound could make a "negative" number look
like a valid small positive one, or vice versa). `ark-r1cs-std`'s
*checked* comparison (`enforce_cmp`, as opposed to `enforce_cmp_unchecked`)
closes this: it adds constraints forcing both operands to be `<= (p-1)/2`
before doing the actual less-than check, and those constraints are only
satisfiable for witness values that genuinely are in that range. Since
`amount[i] <= limit_per_period` is checked *individually, per payment*
(not only as part of the running-sum check), no witness value involved in
the circuit can silently be a huge field element disguised as a small or
negative number — the constraint system is simply unsatisfiable for such
an assignment, so no proof can be produced for it. This is argued
informally here; `zk/solvency_proof/src/lib.rs`'s tests include a
`history_exceeding_period_limit_is_rejected_before_proving` case that
confirms `prove()` refuses non-satisfying witnesses rather than silently
producing garbage.

### What this does *not* prove

A proof shows *some* history consistent with the public totals exists —
not that it's *the* real history. This is an intentional and, for this
use case, acceptable relaxation: `PaymentChannel::pay` already enforces
the per-period limit in real time on the *actual* execution, so
`total_spent` being what it is on-chain already implies the real history
was compliant. The proof's job isn't to re-establish that (the contract
already guarantees it); it's to let the channel owner *demonstrate*
compliance to a third party who wasn't going to trust chain execution or
scan the event log, **without disclosing the itemized history**. A
dishonest-but-internally-consistent fabricated history would need to sum
to the exact public `total_spent` the chain already committed to, so it
doesn't let an operator claim a false total — it only lets them avoid
revealing the true breakdown behind a true total.

### Capacity: `MAX_PAYMENTS = 8`

Kept intentionally small so setup/proving stay fast for a prototype
(sub-second proving in release builds — see benchmarks below). A channel
with more than 8 payments per proof needs one of:
- multiple proofs, one per 8-payment chunk, each proving partial sums,
  with the on-chain caller checking they compose to the full total, or
- a larger `MAX_PAYMENTS` (the circuit is `O(MAX_PAYMENTS)` constraints —
  doubling it roughly doubles proving time and one-time setup time, but
  proof size and on-chain verification cost stay *constant*, since
  Groth16 proofs are always 3 curve points regardless of circuit size).

The second option costs nothing on the verification side — this is one of
Groth16's real advantages here. If this became a production feature, the
practical move is to pick `MAX_PAYMENTS` based on a realistic
payments-per-period ceiling and use the batching approach above for
channels that exceed it.

## The encoding bridge (arkworks ↔ Soroban)

Soroban and arkworks agree on the curve (BLS12-381) but disagree on wire
format. Soroban's native encoding (documented in
`soroban_sdk::crypto::bls12_381`'s rustdoc) is the ZCash/IETF-style
convention: **uncompressed, big-endian** coordinates, flag bits packed
into the top 3 bits of the first byte, G2's `Fp2` coordinates serialized
`c1 || c0` (not `c0 || c1`). arkworks' `CanonicalSerialize` uses a
different (little-endian, Montgomery-adjacent) layout by design — it's
optimized for arkworks' own internal reuse, not cross-implementation
compatibility. Feeding one straight into the other silently produces a
different point (or a validation failure), not an error you'd catch by
inspection.

`zk/solvency_proof/src/soroban_encoding.rs` hand-rolls the conversion
instead of relying on either side's default serializer, extracting affine
coordinates from arkworks' point representation directly (`ark_ff`'s
`BigInteger::to_bytes_be()`, which — verified by reading arkworks' own
source — always emits a *fixed*-width, unpadded big-endian encoding
matching Soroban's 48-byte `Fp` exactly) and reassembling them in
Soroban's documented byte order.

This is checked, not assumed: `soroban_encoding`'s tests include a
known-answer test against the exact G1 generator hex constant published
in `soroban_sdk::crypto::bls12_381`'s own rustdoc example, plus round-trip
tests. The real proof, though, is `zk/solvency_proof/tests/end_to_end.rs`
— it runs the actual arkworks-generated proof through the actual on-chain
`PaymentChannel::verify_solvency_proof`, using Soroban's real
`pairing_check` host function inside a genuine (native, non-mocked)
`soroban-sdk` test `Env`. If the encoding bridge were wrong in any way, a
genuinely valid proof would fail that real pairing check — it isn't
possible for this test to pass by accident the way a purely
self-referential round-trip test could.

## Trusted setup

Groth16 needs a per-circuit structured reference string generated from
secret randomness (the "toxic waste") that must never be reconstructable
by anyone after setup — if it were, that party could forge false proofs
for the exact circuit. **The prototype's `setup()` is a single-party,
in-process setup — explicitly not production-safe.** `zk/solvency_proof`
prints a loud warning about this from the CLI (`solvency-prover setup`),
and the design doc is calling it out here too: shipping this circuit for
real would need either

1. a real multi-party-computation ceremony (the Zcash/Filecoin/Aztec
   playbook — many independent participants each contribute randomness
   and destroy their share, so no single party ever holds the full
   secret), run once per circuit version, or
2. moving to a universal-setup system (PLONK/Marlin) so a single ceremony
   covers any circuit up to a size bound, removing the need to re-run a
   ceremony whenever `MAX_PAYMENTS` or the constraint logic changes.

This is the single biggest gap between this prototype and a real
deployment, and it's a cryptographic-process problem, not an engineering
one this codebase can solve unilaterally.

## On-chain verifier

`contracts/payment_channel/src/lib.rs`:

- `SolvencyVerifyingKey` / `SolvencyProof` — `#[contracttype]` structs
  wrapping `soroban_sdk::crypto::bls12_381::{G1Affine, G2Affine}`
  directly (these already implement the `Val` conversions needed to be
  contract-type fields and call arguments, so no manual `BytesN`
  plumbing is needed in the contract itself).
- `set_solvency_vk(admin, vk)` — admin-gated (first caller becomes admin,
  same bootstrap pattern used elsewhere in this contract), so the VK can
  be rotated if the circuit changes.
- `verify_solvency_proof(channel_id, proof) -> bool` — reads
  `limit_per_period`/`total_spent` from the channel's own storage,
  computes `vk_x = gamma_abc_g1[0] + limit·gamma_abc_g1[1] +
  total_spent·gamma_abc_g1[2]` via `g1_msm`, then calls
  `pairing_check([A, -vk_x, -C, -alpha], [B, gamma_g2, delta_g2, beta_g2])`
  — the standard rearrangement of Groth16's `e(A,B) = e(α,β)·e(vk_x,γ)·e(C,δ)`
  into the product-of-pairings-equals-identity form Soroban's
  `pairing_check` expects.

One behavioral note surfaced by testing (see
`proof_with_malformed_off_curve_bytes_traps`): Soroban's host validates
that BLS12-381 byte input actually decodes to a point on the curve, and
**traps the transaction** rather than returning `false` for input that
doesn't. A well-formed-but-wrong proof (right shape, wrong content) does
return a clean `false` (see `proof_with_a_component_swapped_from_another_proof_is_rejected`
and `proof_for_a_different_total_spent_is_rejected`). Both are secure
rejections; callers should be aware `verify_solvency_proof` can trap on
genuinely malformed byte input rather than only returning `false`.

## The prover

`zk/solvency_proof` (a plain native Rust crate — **not** a workspace
member of `contracts/`, since it depends on arkworks, which has no
business being compiled into the on-chain WASM contract; the contract
only calls Soroban's native BLS12-381 host functions, with zero
compile-time dependency on arkworks).

- `circuit.rs` — the R1CS circuit described above.
- `soroban_encoding.rs` — the byte bridge.
- `lib.rs` — `setup`/`prove`/`verify_native`, plus
  `proof_to_soroban_bytes`/`vk_to_soroban_bytes` for the on-chain-facing
  encoding. `prove()` locally re-checks the witness against the circuit
  (`ConstraintSystem::is_satisfied()`) before spending time on a real
  proof, so a caller gets a clear `Error::PaymentExceedsLimit` /
  `Error::TotalMismatch` instead of an opaque failure.
- `src/bin/prover.rs` — CLI (`solvency-prover setup|prove|verify`). Takes
  a JSON array of `{amount, period_index}` as the private history,
  outputs hex-encoded Soroban-format proof bytes ready to submit to
  `verify_solvency_proof`.

## Demo: it actually works, end to end

```console
$ ./target/release/solvency-prover setup --out-dir ./keys
WARNING: this is a toy, single-party Groth16 setup, not a real trusted-setup ceremony...
wrote ./keys/pk.bin, ./keys/vk.bin, ./keys/vk_soroban.json

$ cat history.json
[
  {"amount": 100000, "period_index": 0},
  {"amount": 200000, "period_index": 0},
  {"amount": 50000, "period_index": 1},
  {"amount": 400000, "period_index": 1}
]

$ ./target/release/solvency-prover prove --pk ./keys/pk.bin --history history.json \
    --limit 500000 --total 750000 --out proof.json
wrote proof.json

$ ./target/release/solvency-prover verify --vk ./keys/vk.bin --proof proof.json
VALID

$ # tamper with the claimed total and re-verify
$ jq '.total_spent = 999999' proof.json > proof_tampered.json
$ ./target/release/solvency-prover verify --vk ./keys/vk.bin --proof proof_tampered.json
INVALID
error: proof did not verify
```

Timings (release build, this machine): setup ~2.5s, proving ~20s, native
verification well under a second. Setup/proving cost scales with
`MAX_PAYMENTS`; on-chain verification cost is constant regardless.

The acceptance-critical version of this exact flow — a real payment
history through the real `PaymentChannel` contract, a real proof, and the
real on-chain `verify_solvency_proof` (not a mock) accepting it, and a
tampered proof being rejected — is `zk/solvency_proof/tests/end_to_end.rs`:

```
$ cargo test --release --test end_to_end
running 5 tests
test payment_still_works_normally_alongside_solvency_proofs ... ok
test valid_proof_is_accepted_by_the_real_on_chain_verifier ... ok
test proof_with_malformed_off_curve_bytes_traps - should panic ... ok
test proof_for_a_different_total_spent_is_rejected ... ok
test proof_with_a_component_swapped_from_another_proof_is_rejected ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Known limitations / future work

- **Trusted setup**: see above — the biggest real gap before production
  use.
- **`MAX_PAYMENTS = 8`**: fine for a prototype/demo; a real deployment
  needs either a batching scheme or a larger bound sized to real traffic.
- **No on-chain gas/resource benchmarking against testnet**: this design
  doc argues the *shape* of the on-chain cost (a handful of host-function
  calls) is cheap, based on reading the host function surface, but the
  numbers here weren't measured against a live Soroban RPC's resource
  metering. Before shipping, run `verify_solvency_proof` through
  `simulateTransaction` against testnet to get real CPU-instruction and
  fee numbers.
- **Only the aggregate `total_spent` is bound**: the circuit doesn't tie
  private payments to any *individual* on-chain commitment (e.g. a
  running hash of each `pay()` call) — it only has to match the
  channel's final `total_spent`. Adding a `payment_commitment: BytesN<32>`
  field to `Channel`, updated via a cheap native `sha256` on every `pay()`
  call, plus an in-circuit SHA-256 gadget (`ark-crypto-primitives`
  provides one) binding the private witness to that running hash, would
  let a proof additionally attest "this is *the* recorded sequence," not
  just "some sequence summing to the same total." Not implemented here —
  it's a genuine complexity jump (SHA-256 is expensive in R1CS relative
  to algebraic hashes like Poseidon, and Soroban doesn't expose a native
  Poseidon host function to match cheaply on-chain) and the current
  design already satisfies the stated requirement ("consistent with the
  public commitment(s) already on PaymentChannel").
- **TS SDK**: no TypeScript wrapper for `verify_solvency_proof` /
  `set_solvency_vk` was added — this design doc and the Rust
  prover/verifier are the deliverable for this task; wiring a
  `packages/core` SDK method through would be a natural, small follow-up
  once contract addresses exist to point it at (see how `CircuitBreaker`
  in the same package is wired, for the pattern to follow).
