# Off-chain payment vouchers for PaymentChannel — design doc

Status: **proposed**, for review before any contract code is written.
Area: `contracts/payment_channel` · Epic: signed off-chain vouchers with
unilateral settlement.

## Problem

Every `payForAPI` is an on-chain transaction: simulate, sign, submit, wait for
confirmation. Roughly five seconds and a fee. The README's pitch is an agent
paying $0.001 per inference call, and at that price it is the **round trip**,
not the fee, that makes the pitch untrue.

`PaymentChannel` already custodies a deposit and enforces a spend limit, which
is exactly the collateral a voucher scheme needs. This doc specifies the
scheme: what a voucher is, what it is signed over, how it settles, and what has
to be true for it to be safe.

---

## Before anything else: two bugs that block this

Reading `contracts/payment_channel/src/lib.rs` against this epic's definition of
done — *"the payout never exceeds the deposit under any voucher sequence"* —
turns up two pre-existing problems. Neither is caused by this work, and both
have to be fixed **by** it, because the invariant cannot be stated without them.

### 1. There is no per-channel collateral

`open_channel` takes a `deposit`, validates it, transfers it to the contract —
and never stores it. `Channel` has `limit_per_period`, `spent_this_period` and
`total_spent`, but no balance. So:

- every channel's deposit lands in one commingled contract-level pool;
- `total_spent` is bounded by nothing. `spent_this_period` resets each period,
  so an agent can spend `limit_per_period` per period indefinitely, drawing
  down a pool funded by *other owners' deposits*;
- "the payout never exceeds the deposit" is not currently true of the existing
  on-chain `pay` path, let alone a voucher path.

### 2. `close_channel` does not return the money

Its doc comment says "Owner closes a channel and reclaims unspent funds". It
sets `active = false`, emits an event, and transfers nothing. The deposit is
stranded in the contract permanently. No test covers this.

### What this design does about them

`Channel` gains `collateral` (tokens actually held for this channel) and
`paid_out`. `open_channel` and `top_up` credit it; `pay`, voucher settlement
and `close_channel` debit it. `close_channel` refunds the remainder.

**This is a behaviour change to the existing `pay` path**, which Phase 2 says to
keep working unchanged. The tension is real and I would rather name it than
quietly resolve it: `pay` will now fail when a channel's collateral is
exhausted, where today it succeeds by spending someone else's deposit. I read
"unchanged" as "no change to its interface or its spend-limit semantics", and
treat refusing to pay out money the channel does not have as a fix rather than
a regression. **If the thread disagrees, this needs settling before Phase 2** —
everything below assumes per-channel collateral exists.

---

## The voucher

A voucher is a signed statement of the form:

> *As of sequence `n`, channel `c` owes recipient `r` a **cumulative** total of
> `a`.*

Cumulative, not incremental. This is the single most important choice in the
design and it buys three things:

- the recipient stores exactly one voucher per channel, not a growing list;
- settlement is idempotent — paying out `a` minus what has already been paid
  needs no history;
- a lost or dropped voucher costs nothing, because the next one supersedes it.

```
Voucher {
  channel_id:        u64
  recipient:         Address
  sequence:          u64
  cumulative_amount: i128     // total owed since the channel opened
  signature:         [u8; 64] // Ed25519 over the preimage below
}
```

### Who signs

Not the agent's `Address`. Soroban's `ed25519_verify` needs a raw 32-byte
public key, and an `Address` does not yield one — it may be a contract, and
even for a `G…` account there is no host function to recover the key from the
address inside a contract.

So a voucher-enabled channel carries an explicit **`voucher_signer:
BytesN<32>`**, declared by the owner at `open_channel` time: the raw Ed25519
public key authorised to mint vouchers for this channel.

This is better than deriving it, not just easier. It means the voucher-signing
key can be a *different* key from the one that submits transactions — an agent
can sign vouchers with a key held in the signing service (`services/signer`)
while its on-chain identity is something else entirely, and rotating one does
not touch the other.

---

## The signing domain

The epic asks that a voucher not be replayable across channels or networks.
Four things have to be bound in, and the preimage binds all of them:

```
preimage (152 bytes, fixed width, no delimiters):

  offset  len  field
  ──────  ───  ─────────────────────────────────────────────────────────
       0   23  b"STELLARAGENT-VOUCHER-V1"   ASCII, no terminator
      23   32  network_id                   SHA-256(network passphrase)
      55   32  contract_id                  this PaymentChannel's raw ID
      87    8  channel_id                   u64, big-endian
      95    1  recipient_kind               0x00 = account, 0x01 = contract
      96   32  recipient                    raw Ed25519 key, or contract ID
     128    8  sequence                     u64, big-endian
     136   16  cumulative_amount            i128, big-endian, two's complement
  ──────  ───
            152

signature = Ed25519(voucher_signer_key, preimage)     // PureEdDSA, RFC 8032
```

Each field earns its place:

| Field | Attack it closes |
|---|---|
| version tag | a v2 voucher being verified by a v1 verifier, or vice versa |
| `network_id` | a testnet voucher settling on mainnet |
| `contract_id` | a voucher settling against a *different deployment* of this same contract on the same network |
| `channel_id` | a voucher for a cheap channel settling against a richer one |
| `recipient_kind` + `recipient` | redirecting a voucher to a different payee |
| `sequence` | ordering and audit — see the next section |
| `cumulative_amount` | the claim itself |

**Fixed-width fields with no delimiters and no length prefixes**, so the
encoding is trivially injective: every field starts at a known offset, so no
two distinct vouchers can produce the same bytes. A delimiter-or-length scheme
would need an argument about concatenation ambiguity; this needs none.

**Big-endian throughout**, because it is unambiguous to specify and every
language has it. Little-endian would work equally well and is a coin flip —
what matters is that all three implementations agree, which is what the
cross-language fixtures in Phase 5 exist to prove.

**Signed over the preimage directly, not over a hash of it.** Soroban's
`ed25519_verify` does PureEdDSA over the message it is given. Hashing first
would mean the contract, the TypeScript SDK and the Python SDK all have to
agree on a second construction for no benefit — 152 bytes is small enough to
sign directly.

---

## Monotonicity: order by amount, not by sequence

The epic says "enforce monotonic sequence". Taken literally — settlement picks
the voucher with the highest `sequence` — that is exploitable, and it is worth
being explicit about why.

**Only the payer holds the voucher-signing key**, and the payer's incentive is
always to *understate* what they owe. If the highest sequence won, the payer
could sign:

```
(sequence = u64::MAX, cumulative_amount = 0)
```

and submit it during the dispute window to erase the recipient's entire claim.
No amount of sequence discipline in the SDK prevents this, because the attacker
*is* the party that assigns sequences.

**So settlement orders by `cumulative_amount`.** A voucher supersedes the
current best only if it pays the recipient strictly more:

```
supersedes(new, best)  ⟺  new.cumulative_amount > best.cumulative_amount
```

A voucher that pays less can never win, so a malicious payer gains nothing from
minting one. The rule is monotone in exactly the quantity the recipient cares
about.

`sequence` is kept, must strictly increase per issued voucher, and is recorded
on settlement — it is genuinely useful for audit, for spotting a client that
has forked its own voucher history, and for the SDK's local store invariant. It
is simply not the security-relevant comparison, and this design does not
pretend otherwise.

---

## Making "payout never exceeds the deposit" provable

A channel can pay many recipients. If each recipient had an independent
cumulative counter with nothing tying them together, the payer could sign
vouchers to ten recipients each for the full collateral; the first to settle
would be paid and the rest would bounce. The invariant would hold only by
accident of ordering.

So collateral is **explicitly allocated**:

```
allocate(owner, channel_id, recipient, amount)      // one on-chain tx per payee
```

- `collateral` = tokens the contract holds for this channel.
- `allocations[recipient]` = collateral reserved for voucher settlement with
  that recipient.
- `free` = `collateral − Σ allocations`. The existing on-chain `pay` path
  spends from `free` only.

Invariants, all checkable on-chain at the point of change:

```
1.  Σ allocations + free  ==  collateral
2.  settled[r]            ≤   allocations[r]      for every recipient r
3.  ⟹ Σ settled + free_spent ≤ collateral
```

(3) is the definition of done, and it follows from (1) and (2) by construction
rather than by argument — which is what makes the Phase 3 property test
meaningful rather than decorative.

Allocation costs one on-chain transaction per `(channel, recipient)` pair, once
— not per payment. An agent talking to five API providers pays five allocation
transactions and then makes unlimited free micropayments to all five.

### The alternative I rejected

One recipient per channel makes the invariant trivial and is what a
Lightning-style unidirectional channel does. It is also operationally
miserable: an agent using five providers needs five channels, five deposits and
five separate spend limits, and the existing `Channel` type would have to grow
a `recipient` field that the on-chain `pay` path does not want. Allocation
keeps one channel, one deposit, one spend limit, and buys the same proof.

---

## Closing

Three paths. All of them settle to the same place: the recipient is paid
`cumulative_amount`, the owner reclaims the rest of the allocation.

### Cooperative — no delay

```
close_cooperative(channel_id, voucher, recipient_auth, owner_auth)
```

Both sides authorise the same final voucher. Nothing is in dispute, so nothing
waits: pay out immediately and release the allocation. This is the path that
should be taken 99% of the time, and it costs one transaction.

### Unilateral — a dispute window opens

```
close_unilateral(channel_id, voucher)      // anyone holding a valid voucher
```

Verifies the signature and the allocation bound, records the voucher as the
current best, and opens a window of `dispute_ledgers`. During the window:

```
challenge(channel_id, voucher)             // a strictly larger voucher wins
```

Each successful challenge replaces the best voucher. The window does **not**
reset on challenge — otherwise a payer could keep it open indefinitely by
drip-feeding vouchers.

After the window:

```
finalize(channel_id, recipient)            // anyone may call
```

Pays the best voucher and releases the remainder.

### Why a window at all

Because a unilateral closer might be the payer, submitting a stale voucher to
pay less than they owe. The window is the recipient's opportunity to say
otherwise. Its length is the central trade-off:

| Shorter window | Longer window |
|---|---|
| capital freed sooner | more time for the recipient to notice and challenge |
| recipient must be online promptly | collateral locked up longer |

**Proposed default: 17,280 ledgers (~24 hours), configurable per channel at
open time**, with a floor the contract enforces so a channel cannot be created
with a window too short to challenge. A recipient that cannot guarantee being
online within the window should delegate to a watchtower — the same assumption
every payment channel makes, stated here rather than assumed.

---

## The penalty, and its honest limit

If a payer closes with a stale voucher and the recipient challenges
successfully, the payer should end up worse off than if they had been honest.
Otherwise cheating is free: try it, and if caught, pay what you owed anyway.

```
understatement = final_cumulative − submitted_cumulative
penalty        = min(understatement, allocation − final_cumulative)
recipient gets = final_cumulative + penalty
```

The penalty is asymmetric on purpose. **Only the payer benefits from a stale
voucher** — a recipient submitting an old voucher would be paying themselves
less — so only a payer-side close can be penalised. Penalising both sides
symmetrically would deter honest recipients from closing unilaterally, which is
the one thing they need to be able to do when a payer goes dark.

**The limit worth stating plainly:** the penalty comes out of what the owner
would otherwise reclaim, so it is bounded by `allocation − final_cumulative`.
When the recipient is owed the whole allocation there is nothing left to
penalise with, and a payer who cheats in that situation loses nothing beyond
the attempt. Reserving a dedicated penalty margin on top of the allocation
would fix it, at the cost of locking up capital that is not owed to anyone.
**This is open question 3 below** — I lean toward accepting the limit and
documenting it, because the case only arises when the channel is fully drawn
down, which is also when the recipient has the least left to lose.

---

## On-chain state

Per channel, added to `Channel`:

```
collateral:       i128        // tokens held for this channel
paid_out:         i128        // lifetime, across pay() and vouchers
voucher_signer:   BytesN<32>  // the Ed25519 key allowed to mint vouchers
dispute_ledgers:  u32         // this channel's window length
```

Per `(channel, recipient)`:

```
Allocation {
  amount:       i128    // reserved collateral
  settled:      i128    // paid out so far
}

Settlement {                 // present only while a close is in flight
  best_sequence:    u64
  best_cumulative:  i128
  opened_at_ledger: u32
  closed_by_payer:  bool     // whether a penalty can apply
}
```

Stored under composite keys rather than in a `Map` on the instance: the
existing `channels` map is loaded and re-serialised in full on every `pay`,
which is a cost that grows with the number of channels and a pattern this work
should not extend. Phase 2 will use `DataKey::Allocation(channel_id, recipient)`
in persistent storage.

---

## Worked example: 10,000 payments, one settlement

An agent paying $0.0001 per inference call to one provider:

| | On-chain today | With vouchers |
|---|---|---|
| Transactions | 10,000 | 3 (open, allocate, close) |
| Latency per payment | ~5 s | signature only, sub-millisecond |
| Fees | 10,000 × base fee | 3 × base fee |
| Recipient's storage | — | one voucher |

The agent signs voucher `n` with `cumulative_amount = n × 100` stroops and
hands it over with the request. The provider verifies the signature, checks
`cumulative_amount ≤ allocation`, serves the request, and keeps only the latest
voucher. At the end of the day either side closes.

---

## Trust assumptions

Stated because a payment channel is a protocol, and a protocol that does not
say what it assumes is not specified.

1. **The recipient must be online within the dispute window**, or must delegate
   to a watchtower. A recipient that is offline for longer than the window can
   be settled against with a stale voucher.
2. **The recipient must verify every voucher on receipt** — signature, channel,
   recipient, and `cumulative_amount ≤ allocation`. A voucher exceeding the
   allocation is worthless, and the receiver tooling in Phase 6 must refuse it
   at the point of service rather than at settlement.
3. **A voucher is a claim, not a guarantee of service.** The payer can stop
   paying at any point; the recipient's protection is that they can stop
   serving.
4. **The collateral is only as good as the token.** A channel funded in a token
   whose issuer can claw back or freeze is a channel whose vouchers can become
   unredeemable.
5. **No routing.** Direct channels only. Multi-hop is out of scope and is not a
   small addition.

### What this does not protect against

- **A payer who never signs another voucher.** The recipient keeps what they
  hold; they cannot compel further payment. This is a feature.
- **A recipient who refuses service after being paid.** Vouchers are
  pay-then-serve or serve-then-pay by agreement; neither side is protected from
  the other's last defection, only from losing everything.
- **Griefing by forced closure.** Anyone holding a valid voucher can open a
  window and lock the allocation for its duration. Bounded and non-profitable,
  but real.
- **A compromised voucher-signing key.** It can mint vouchers up to the
  allocation. That bound is the whole protection, which is why allocations
  should be sized to what an agent is expected to spend, not to the full
  deposit.

---

## Phase plan

| Phase | Contents | Est. |
|---|---|---|
| 1 | **This document** | ~500 |
| 2 | Contract: collateral accounting, allocation, the three close paths, penalty | ~1,400 |
| 3 | Contract tests, including the property test for invariant (3) | ~1,200 |
| 4 | TypeScript SDK: creation, signing via the existing `Signer`, store, `payOffChain()`, `settle()` | ~900 |
| 5 | Python SDK + cross-language fixtures | ~700 |
| 6 | Receiver verifier and reference middleware | ~700 |
| 7 | Protocol document and the worked example | ~600 |

Phase 4 signs through the existing `Signer` interface. That works unchanged for
`KeypairSigner`, but `RemoteSigner` currently exposes only `signTransaction`
and `signAuthEntry` — neither of which is "sign these 152 arbitrary bytes". See
open question 4.

---

## Open questions

Genuinely open. I would rather have answers than assumptions.

1. **Is the collateral fix in scope here, or its own PR first?** Phase 2 says
   keep `pay` working unchanged, and this changes when `pay` fails. My
   inclination is to land the collateral accounting as the first commit of
   Phase 2 with its own tests, since vouchers cannot be made safe without it —
   but it could equally be a separate PR that this one builds on.

2. **Is one allocation transaction per `(channel, recipient)` acceptable?** It
   is what makes the payout bound provable. The alternative — unallocated
   vouchers, first-to-settle wins — is cheaper and gives the recipient no way
   to know whether their voucher is backed.

3. **Accept the penalty's upper bound, or reserve a margin?** As specified, a
   payer who cheats on a fully drawn-down allocation loses nothing. A reserved
   margin fixes it and locks up capital nobody is owed.

4. **How should the SDK sign 152 arbitrary bytes?** The `Signer` interface is
   deliberately XDR-shaped. Options: add a `signVoucher`/`signRaw` method to
   `Signer` (touches a shipped interface, and a "sign arbitrary bytes" method
   is a sharp tool to hand a remote signer); or wrap the preimage in a
   throwaway envelope so `signTransaction` can carry it (ugly, and the signing
   service would refuse it — correctly — as uninspectable). I lean toward a
   narrow, domain-separated `signVoucher` that the signing service can inspect
   and apply policy to, which would also give `services/signer` a real reason
   to understand vouchers.

5. **Default dispute window of ~24 hours** — right for an agent workload, or
   too long to lock capital an agent needs to recycle?

Question 4 is the one that reaches furthest outside this contract, so it is
worth settling first.
