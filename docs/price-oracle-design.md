# Multi-asset payments for PaymentChannel — design doc

## Problem

`PaymentChannel` hard-locks a channel to a single `token`, chosen once at
`open_channel` and unchangeable afterwards; every `pay()` call moves that
exact asset. In practice, the asset an agent's channel is funded in and
the asset a given recipient actually wants are two independent choices —
an agent's channel might be funded in USDC while a specific API provider
only accepts XLM. Today the only workaround is opening one channel per
asset, which fragments a single coherent spend limit across N independent
ones with no shared accounting.

This doc covers the fix: `PaymentChannel::pay_with_conversion`, plus two
small new contracts (`price_oracle`, `amm_swap`) it depends on.

## What stays the same, and why

`Channel.token` keeps its existing meaning exactly: the one asset the
channel is ever funded in (`open_channel` / `top_up`), and the unit
`limit_per_period` / `spent_this_period` / `total_spent` are denominated
in. `pay()` is untouched — same code path, same behavior, zero risk of
regression for existing integrations.

This is a deliberate, load-bearing choice: it means the spend limit is
**already normalized** the moment a payment is initiated, because
`amount` in `pay_with_conversion` is always expressed in `channel.token`
units, exactly like `pay()`'s `amount` — regardless of which asset the
recipient ends up receiving. There is no need to invent a second unit of
account (e.g. "USD equivalent") for the ledger itself; that would add a
conversion step to the *accounting*, not just the *transfer*, and would
be one more thing that could drift from what was actually funded.

## Where a price oracle actually earns its keep

If the spend limit is already normalized without an oracle, why does this
design need one at all? Because **the conversion itself is the
untrusted step**, not the accounting. `pay_with_conversion` calls out to
a configured AMM (`set_amm`) to actually swap `channel.token` into the
recipient's `dest_token`. An AMM's on-chain quote is manipulable — a pool
can be temporarily imbalanced within the same transaction/block. If this
contract simply trusted "whatever the AMM says it paid out" as the
measure of a fair trade, a manipulated pool could report a technically
successful swap while paying out `dest_token` far below (or, more
dangerously framed the other way, requiring far more `channel.token`
than) the real market rate — degrading the payment's value without ever
tripping the spend limit, since the limit only sees `amount` of
`channel.token`, which is fixed by the caller regardless of the AMM's
behavior.

The actual role of `PriceOracle` here is a **slippage backstop, not a
spend-limit input**: it supplies an independent reference price for
`channel.token -> dest_token`, and the caller's `min_received` must clear
`MAX_SLIPPAGE_BPS` (5%, `payment_channel::MAX_SLIPPAGE_BPS`) off that
reference *before* any transfer happens — on top of whatever `min_received`
the caller asked for. So there are two independent floors a trade must
clear:

1. the caller's own `min_received` (standard AMM-interaction hygiene —
   the caller states the worst rate they'll accept), and
2. the oracle-derived fair-value floor (protects against a caller/client
   that set `min_received` too low, and against a mispriced/manipulated
   AMM quote that would otherwise clear (1) anyway).

If the oracle has no price for the pair — or `set_price_oracle` /
`set_amm` was never configured — `pay_with_conversion` panics and the
entire call reverts: nothing transferred, `spent_this_period` untouched.
**Unpriced must never mean unlimited.** This is enforced by construction,
not by a fallback branch: the price lookup happens before any token
transfer, and a Soroban panic unwinds the whole invocation.

## Trust assumption this introduces

`PriceOracle` is a new external trust dependency the rest of this
contract suite doesn't otherwise have. `PaymentChannel`, `Escrow`, and
`RateLimiter` are all self-contained modulo `CircuitBreaker`, and
`CircuitBreaker` itself requires a 5-of-N quorum of trusted nodes to
change anything security-relevant — not a single key. `PriceOracle`, as
built here, is a **single admin key** publishing prices via `set_price`.

This is an explicit, documented starting point, not an oversight:

- It keeps the on-chain surface small enough to audit in one sitting
  (~100 lines), which matters more at this stage than the marginal safety
  a quorum would add.
- `PaymentChannel` never treats the oracle price as the sole determinant
  of how much value moves — it's a bound on top of an already-executed
  AMM swap, not a price the AMM is instructed to fill at. A stale or
  slightly-off price degrades the slippage guarantee; it does not by
  itself let value leave the channel beyond `amount` of `channel.token`.
- It is **not** sufficient for production use moving meaningful value: a
  compromised or careless oracle admin can publish a bad price and
  degrade exactly the protection described above. Before mainnet this
  should be replaced with, or layered under, a decentralized/aggregated
  feed (e.g. [Reflector](https://reflector.network) on Stellar) or a
  multi-admin quorum mirroring `CircuitBreaker`'s trusted-node model.

## The AMM side

`amm_swap` is intentionally not a constant-product pool — it holds
admin-funded reserves and quotes admin-set fixed rates. That keeps the
reference implementation small and its behavior deterministic to test.
Its funding model mirrors the self-authorized-transfer pattern `pay()`
already uses: `PaymentChannel` pushes `channel.token` to the AMM's own
balance first (it's the direct caller of that transfer, so no separate
auth entry is needed — same as `pay()` paying a recipient directly), then
calls `execute_swap`, which pays `dest_token` out of its own reserves. A
production deployment should swap this contract out for a real DEX
aggregator (Soroswap, Comet, …) implementing the same `execute_swap`
shape, or adapt the call site to that aggregator's native interface —
`PaymentChannel` doesn't need to change its trust model to do that, since
it never trusted the AMM's quote unilaterally in the first place.

## Testing

- `contracts/price_oracle/src/test.rs` — the oracle in isolation: publish
  and read a price, identity pairs always price at 1:1, an unpublished
  pair panics rather than assuming a rate, admin gating.
- `contracts/amm_swap/src/test.rs` — the swap contract in isolation: pays
  out at the configured rate, reverts below `min_out`, reverts with no
  rate configured.
- `contracts/payment_channel/src/test.rs` — the integration:
  - `pay_transfers_settlement_token_and_tracks_spend` /
    `pay_still_enforces_the_spend_limit` — baseline `pay()` regression
    coverage.
  - `pay_with_conversion_same_asset_behaves_like_pay` — same-asset
    `pay_with_conversion`, on a channel that never configured an oracle
    or AMM at all, to prove the same-asset path is fully independent of
    them.
  - `cross_asset_payment_within_slippage_updates_normalized_spend` —
    cross-asset payment succeeds and the spend counters move by the
    `channel.token` amount, not the (larger) `dest_token` amount received.
  - `cross_asset_payment_exceeding_slippage_tolerance_reverts` — a
    `min_received` more than `MAX_SLIPPAGE_BPS` below the oracle's fair
    value reverts before any transfer.
  - `cross_asset_payment_reverts_if_amm_cannot_clear_min_received` — the
    AMM's own `min_out` check reverts a trade the oracle bound alone
    would have allowed.
  - `price_feed_unavailable_fails_safely` — an unpriced pair panics, and
    the test explicitly re-checks (via `catch_unwind`) that the recipient
    balance and the channel's spend counters are unchanged afterward.
  - `spend_limit_enforced_in_normalized_terms_across_same_and_cross_asset_payments`
    — the acceptance-criteria scenario: a single channel pays through
    `pay()`, same-asset `pay_with_conversion`, and cross-asset
    `pay_with_conversion` in sequence, and the period limit is enforced
    against their combined total in `channel.token` terms throughout.
