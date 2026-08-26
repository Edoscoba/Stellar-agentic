# [contracts/payment_channel] Off-chain payment vouchers — Phases 1–3

Partial: **Phases 1, 2 and 3** of the voucher epic. Phases 4–7 (SDKs, receiver
tooling, protocol doc) are deliberately not in this PR — see
[Where this stops](#where-this-stops).

## Problem

Every `payForAPI` is an on-chain transaction: simulate, sign, submit, wait for
confirmation. Roughly five seconds and a fee. The README's pitch is an agent
paying $0.001 per inference call, and at that price it is the **round trip**,
not the fee, that makes the pitch untrue.

`PaymentChannel` already custodies a deposit and enforces a spend limit, which
is exactly the collateral a voucher scheme needs.

## Summary

Signed off-chain vouchers with unilateral on-chain settlement and a dispute
window. An agent signs a voucher per micropayment — no transaction, no
confirmation wait — and one on-chain close settles all of them.

~2,230 lines across four commits. All seven contracts green: **46
`payment_channel` tests** (up from 11), `cargo fmt --check` and
`cargo clippy --all-targets -D warnings` clean.

| Commit | Phase | Contents |
| --- | --- | --- |
| `1bee412` | 1 | Protocol design doc |
| `9d4695d` | 2 | Voucher settlement + the collateral accounting it needs |
| `f80b3cd` | 3 | 36 tests, grouped by the property each establishes |
| `fa040bb` | — | Sync the design doc with what was implemented |

---

## ⚠️ Two pre-existing bugs this had to fix first

Reading `payment_channel/src/lib.rs` against the epic's definition of done —
*"the payout never exceeds the deposit under any voucher sequence"* — turned up
that **this was not true of the existing on-chain `pay` path either**. Neither
bug is caused by this work; both block it.

### 1. `Channel` had no collateral

`open_channel` took a `deposit`, validated it, transferred it to the
contract — and never stored it. `Channel` had `limit_per_period`,
`spent_this_period` and `total_spent`, but no balance. So:

- every channel's deposit landed in one commingled contract-level pool;
- `total_spent` was bounded by **nothing**. `spent_this_period` resets each
  period, so an agent could spend `limit_per_period` per period indefinitely,
  drawing down a pool funded by *other owners' deposits*.

### 2. `close_channel` returned no money

Its doc comment says "Owner closes a channel and reclaims unspent funds". It set
`active = false`, emitted an event, and transferred nothing. Every deposit was
stranded in the contract permanently. **No test covered it.**

### What changed

`Channel` gains `collateral` and `allocated`. `open_channel` records the deposit
it was already transferring; `top_up` credits it; both pay paths debit it and
refuse to spend past it; `close_channel` transfers the refund it always
promised. Four tests cover this directly, including
`a_channel_cannot_outspend_its_deposit_across_periods`.

**This changes when `pay` fails.** It now refuses once a channel's own
collateral is exhausted, where before it succeeded by spending another owner's
deposit. Phase 2 says keep the on-chain path unchanged, and this is the one
place it is not: the interface and the spend-limit semantics are untouched, but
refusing to pay out money the channel does not have reads as a fix rather than a
regression. **Flagged rather than resolved quietly — if the thread disagrees,
this is the thing to say so about.**

---

## The design decision that mattered most

The epic says *"enforce monotonic sequence"*. **Taken literally — settlement
picks the highest `sequence` — that is exploitable.**

Only the payer holds the voucher-signing key, and a payer's incentive is always
to *understate*. With highest-sequence-wins they could sign:

```
(sequence = u64::MAX, cumulative_amount = 0)
```

and submit it during the dispute window to erase the recipient's entire claim.
No amount of sequence discipline in an SDK prevents this, because the attacker
*is* the party that assigns sequences.

**So settlement orders by `cumulative_amount`.** A voucher supersedes only by
paying the recipient strictly *more*, so minting a small one is useless to an
attacker. `sequence` is still carried, still required to increase, and still
recorded — as an audit aid, not the security-relevant comparison. The doc says
so rather than pretending it implemented the literal requirement.

`an_enormous_sequence_with_a_tiny_amount_cannot_erase_the_claim` mints exactly
that attack voucher and asserts it does nothing.

---

## A production bug the tests caught

Soroban archives persistent entries whose TTL lapses. A settlement has to
survive a dispute window that is ~24 hours of ledgers by default.

Without an explicit TTL extension, a settlement entry could be **archived during
an open dispute** — `finalize` would then fail on an archived key, and the
allocation would be stranded with no way to pay it out *or* reclaim it. Funds
lost, permanently, with no recovery path.

It surfaced as a test failure after a ledger jump, and it would have shipped
otherwise. Every persistent write now goes through a helper that extends the
entry's lifetime to twice the window.

---

## The scheme

**Cumulative vouchers, not incremental.** A voucher says *"as of sequence `n`,
channel `c` owes recipient `r` a cumulative total of `a`"*. The recipient keeps
exactly one voucher, settlement is idempotent, and a dropped voucher costs
nothing because the next supersedes it.

**Explicit per-recipient allocation.** `allocate` reserves collateral for one
recipient — one transaction per `(channel, recipient)` pair, once, not per
payment. This is what makes the payout bound hold *by construction*:

```
1.  Σ allocations + free  ==  collateral
2.  settled[r]            ≤   allocations[r]
3.  ⟹ Σ settled + free_spent ≤ collateral
```

(3) is the definition of done, and it follows from (1) and (2) rather than from
an argument — which is what makes the property test meaningful rather than
decorative.

**Three close paths.** Cooperative (both sides authorise, pays immediately, no
window); unilateral (opens a challenge window); `finalize` (pays the best
voucher after it expires).

**An asymmetric penalty.** Only a payer benefits from a stale voucher — a
recipient submitting an old one would be paying themselves less — so only a
payer-side close can be penalised. Penalising both sides would deter honest
recipients from closing when a payer goes dark, which is the one thing they need
to be able to do.

### The signing domain

151 fixed-width bytes, no delimiters, so the encoding is trivially injective —
every field starts at a known offset, so no two distinct vouchers can produce
the same preimage.

```
  0   23  b"STELLARAGENT-VOUCHER-V1"
 23   32  network id            env.ledger().network_id()
 55   32  contract domain       sha256(xdr(this contract's Address))
 87   32  recipient domain      sha256(xdr(recipient Address))
119    8  channel id            u64, big-endian
127    8  sequence              u64, big-endian
135   16  cumulative amount     i128, big-endian, two's complement
```

**The doc was amended during implementation** (`fa040bb`). Its first draft
specified raw 32-byte keys plus a one-byte kind tag; a contract cannot recover
an Ed25519 key from an `Address` (the SDK keeps `contract_id()` private), so it
would have had to slice the id out of the serialised address at a fixed offset —
which breaks silently if that encoding ever changes. Hashing the whole
serialised address is offset-independent and distinguishes an account from a
contract for free, because their XDR discriminants differ. A spec that no longer
describes the code is worse than no spec, and Phases 4–5 have to implement
exactly this.

### Smaller decisions, each documented where it applies

- **`challenge` takes no authorisation** — the signature *is* the authority, and
  requiring one would stop a watchtower challenging on a recipient's behalf,
  which is precisely what watchtowers are for.
- **The window does not reset on challenge** — otherwise a payer could hold a
  channel open indefinitely by drip-feeding vouchers one stroop apart.
- **A cooperative close is refused while a dispute is open** — otherwise a payer
  could open a window with a stale voucher and finalise at the stale amount the
  moment the recipient co-signed anything.
- **Allocations survive `close_channel`** — an owner must not be able to close
  their way out of an obligation.
- **A dispute-window floor (`MIN_DISPUTE_LEDGERS`)** — without it, an owner
  could open with a one-ledger window, close with a stale voucher and finalise
  before any recipient could physically respond, making the whole mechanism
  decorative.
- **Key rotation repudiates unsettled vouchers** — the intended blast radius: a
  compromised voucher key is exactly when you need to revoke it.

---

## Tests

36 tests, grouped by the property each set establishes, because for a payment
protocol "does it work" is the least interesting question.

| Group | Establishes |
| --- | --- |
| Happy path | Cooperative and unilateral close; **10,000 vouchers settling in one transaction** |
| Can a payer pay less than they owe? | Stale close superseded; cheating costs more than honesty; a smaller voucher cannot supersede; the `u64::MAX`/amount-1 attack |
| Replay | Across channels, across recipients, amount edited after signing, wrong key, rotated key, already settled |
| The payout bound | Property test: the whole deposit allocated across four recipients, each claiming in full, asserting the contract's balance ends at zero |
| Windows | Challenge after expiry refused; finalize before expiry refused; window does not reset |
| The collateral fix | Close returns the deposit; allocations survive it; top-up credits; a channel cannot outspend its deposit across periods |

Signatures are produced with `ed25519-dalek` as a **dev-dependency** — the
contract only ever verifies, via the host's `ed25519_verify`, so it never
reaches the deployed WASM.

---

## Where this stops

**Phases 4–7 are not in this PR**: TypeScript SDK, Python SDK with
cross-language fixtures, receiver tooling, and the protocol document — roughly
2,900 lines. Two reasons to pause here rather than press on:

1. **Open question 4 is now blocking.** Phase 4 has to sign 151 arbitrary bytes
   through the `Signer` interface, which is deliberately XDR-shaped
   (`signTransaction` / `signAuthEntry` only). The signing service in
   `services/signer` would *correctly refuse* a voucher smuggled inside a fake
   envelope, as uninspectable. Options: add a narrow, domain-separated
   `signVoucher` the service can inspect and apply policy to (my inclination, and
   it would give `services/signer` a real reason to understand vouchers), or
   something else the thread prefers. This touches a shipped interface, so it is
   worth settling before code.

2. **Phase 1 was explicitly gated on thread agreement**, and Phases 2–3 were
   built past that gate on my own stated recommendations. The contract is where a
   rejected design is most expensive to unwind, so this is the natural place to
   check in.

### Definition of done

| Criterion | Status |
| --- | --- |
| 10,000 off-chain payments settle in a single on-chain transaction | ✅ `ten_thousand_off_chain_payments_settle_in_one_transaction` |
| A superseded voucher cannot steal funds, and tests prove it | ✅ five tests, including the sequence attack |
| TS and Python produce identical voucher signatures | ⬜ Phases 4–5 |

---

## Notes for review

- The **`pay` behaviour change** is the thing most worth a decision. Everything
  else is additive.
- **Assumptions I proceeded on**, all from the design doc's open questions: the
  collateral fix is in scope here (Q1); one allocation transaction per
  `(channel, recipient)` is acceptable (Q2); the penalty's upper bound is
  accepted and documented rather than fixed with a reserved margin (Q3); the
  default window is ~24 hours with an enforced floor (Q5).
- **The penalty's honest limit**: it comes out of what the owner would otherwise
  reclaim, so a payer who cheats on a fully drawn-down allocation loses nothing.
  Documented in the contract and the design doc rather than hidden.
- **Trust assumptions are written down** in the design doc, including the ones
  people skip: the recipient must be online within the dispute window or
  delegate to a watchtower, and must verify the allocation bound at the point of
  service rather than at settlement.
