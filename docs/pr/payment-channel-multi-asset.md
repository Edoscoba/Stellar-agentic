# [Advanced/Interop] Add multi-asset / path-payment support to PaymentChannel

Closes #262

## Summary

`PaymentChannel` hard-locked a channel to a single `token`, so an agent
funded in one asset (e.g. USDC) had no way to pay a recipient who only
accepts a different one (e.g. XLM) without opening a second channel —
fragmenting a single coherent spend limit across N independent ones.

This adds `PaymentChannel::pay_with_conversion`, which lets one channel
settle recipients in any asset while keeping `pay()` and the existing
single-asset behavior completely unchanged.

- `pay_with_conversion(agent, channel_id, recipient, amount, dest_token, min_received, memo) -> i128`
  — `amount` is debited in `channel.token` exactly like `pay()`'s
  `amount`; if `dest_token == channel.token` it behaves identically to
  `pay()` (no oracle/AMM involved at all). Otherwise it converts via a
  configured AMM and settles the recipient in `dest_token`.
- Two new contracts:
  - **`price_oracle`** — a trusted reference price feed, admin-published.
    Used as an independent slippage/fairness bound on the conversion, on
    top of the caller's own `min_received`. If a pair has no published
    price, the whole call panics and reverts — nothing transferred,
    nothing spent. Unpriced never means unlimited.
  - **`amm_swap`** — a minimal, admin-rate-quoted swap contract standing
    in for a full DEX integration (Soroswap/Comet in production). It
    executes the actual `channel.token -> dest_token` transfer and
    enforces `min_out` itself.
- `set_price_oracle` / `set_amm` wire both into `PaymentChannel`,
  mirroring `set_circuit_breaker`'s admin-rotation pattern.
- `packages/core`: `PayForAPIParams` gains `destAsset` / `minReceived`;
  `OpenChannelParams` docs clarify `token` stays the channel's one
  funding/settlement asset. `payForAPI` remains a stub (per the companion
  SDK issue) but now validates the new params are paired correctly.
- Fixed a pre-existing compile break in `payment_channel` (missing
  `Symbol` import needed by `require_not_paused`) — the crate did not
  build on `main` before this change.

Full design rationale — why normalization stays in `channel.token`, what
the price oracle actually protects against (AMM quote manipulation, not
the spend-limit arithmetic itself), and the trust assumption it
introduces — is in [`docs/price-oracle-design.md`](../price-oracle-design.md).

## Why normalization doesn't need a new unit of account

The spend limit (`limit_per_period` / `spent_this_period` / `total_spent`)
stays denominated in `channel.token`, exactly as today. `amount` in
`pay_with_conversion` is always in `channel.token` units regardless of
which asset the recipient ends up receiving, so the limit is normalized
by construction — no new reference unit, no risk of `pay()`'s existing
behavior changing.

What *does* need an independent, trusted input is the conversion itself:
an AMM's on-chain quote is manipulable, so `PaymentChannel` never trusts
"whatever the AMM says it paid out" on its own. `price_oracle` supplies a
fair-value bound that `min_received` must clear (in addition to the
AMM's own `min_out` enforcement) before any transfer happens.

## Trust assumption

`price_oracle` is a new external trust dependency this contract suite
didn't previously have — a single admin key publishing prices. This is
an explicit starting point, not an oversight; see the design doc for the
full justification and what should replace it (a decentralized/aggregated
feed, or a multi-admin quorum like `CircuitBreaker`'s) before this path
moves meaningful value in production.

## Testing

`cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D
warnings`, `cargo test --all`, and `cargo build --target
wasm32-unknown-unknown --release` all pass locally.

19 new tests across three crates:

- **`price_oracle`** (4): publish/read a price, identity pairs always
  price at 1:1, an unpublished pair panics rather than assuming a rate,
  admin gating.
- **`amm_swap`** (3): pays out at the configured rate, reverts below
  `min_out`, reverts with no rate configured.
- **`payment_channel`** (10 — spans `pay()` baseline, `pay_with_conversion`,
  and the acceptance-criteria scenario):
  - `pay_transfers_settlement_token_and_tracks_spend`,
    `pay_still_enforces_the_spend_limit` — `pay()` regression baseline.
  - `pay_with_conversion_same_asset_behaves_like_pay` — same-asset
    `pay_with_conversion`, on a channel that never configured an oracle or
    AMM, proving the same-asset path is fully independent of them (no
    regression).
  - `cross_asset_payment_within_slippage_updates_normalized_spend` —
    cross-asset payment within tolerance succeeds; spend counters move by
    the `channel.token` amount, not the (larger) `dest_token` amount
    received.
  - `cross_asset_payment_exceeding_slippage_tolerance_reverts` — a
    `min_received` more than `MAX_SLIPPAGE_BPS` below the oracle's fair
    value reverts before any transfer.
  - `cross_asset_payment_reverts_if_amm_cannot_clear_min_received` — the
    AMM's own `min_out` check reverts a trade the oracle bound alone would
    have allowed.
  - `price_feed_unavailable_fails_safely` — an unpriced pair panics; the
    test explicitly re-checks (via `catch_unwind`) that the recipient
    balance and channel spend counters are unchanged afterward — fails
    safe, not unpriced-and-unlimited.
  - `spend_limit_enforced_in_normalized_terms_across_same_and_cross_asset_payments`
    — **acceptance criteria**: a single channel pays through `pay()`,
    same-asset `pay_with_conversion`, and cross-asset
    `pay_with_conversion` in sequence, and the period limit is enforced
    against their combined total in `channel.token` terms throughout.
  - `set_price_oracle_is_admin_gated`, `set_amm_is_admin_gated`.

## Out of scope / follow-ups

- `payForAPI`'s actual Soroban invocation (both the existing `pay` path
  and the new `pay_with_conversion` path) is still a stub, blocked on the
  companion SDK issue — this PR only exposes the new params on the type.
- `amm_swap` is an admin-rate-quoted stand-in, not a constant-product
  pool; swapping in a real DEX aggregator (Soroswap/Comet) is a follow-up
  that doesn't require changing `PaymentChannel`'s trust model.
- `price_oracle`'s single-admin design should be hardened (decentralized
  feed or multi-admin quorum) before this path is used for
  production-value payments — see the design doc.

## Files changed

- `contracts/payment_channel/src/lib.rs` — `pay_with_conversion`,
  `set_price_oracle`, `set_amm`; fixes the missing `Symbol` import.
- `contracts/price_oracle/` — new crate.
- `contracts/amm_swap/` — new crate.
- `contracts/Cargo.toml`, `contracts/payment_channel/Cargo.toml` —
  workspace/dev-dependency wiring for the two new crates.
- `contracts/payment_channel/src/test.rs`, `contracts/price_oracle/src/test.rs`,
  `contracts/amm_swap/src/test.rs` — new tests.
- `packages/core/src/types/index.ts`, `packages/core/src/index.ts` —
  `PayForAPIParams`/`OpenChannelParams` updates.
- `docs/price-oracle-design.md` — design doc.
