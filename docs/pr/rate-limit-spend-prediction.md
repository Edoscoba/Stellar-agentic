# [SDK] Predict spend/rate-limit outcomes before paying

## Problem

Nothing in the codebase let a consumer answer "will my next payment of
amount X be blocked by rate limits or spend limits, before I submit it and
pay a transaction fee to find out?" `isWithinSpendLimit`/`remainingBudget`
already existed in `packages/core/src/math/bid.ts` as pure functions, but
nothing wired live on-chain `RateLimit`/`Channel` state into them, and
nothing translated Stellar's ledger-sequence-based period windows
(`hour_window_start`, `day_window_start` in
`contracts/rate_limiter/src/lib.rs`, `period_start_ledger` in
`contracts/payment_channel/src/lib.rs`) into human wall-clock countdowns.

## Summary

- **`predictPaymentOutcome`** (`packages/core/src/math/predict.ts`) — a
  pure, environment-agnostic function combining a channel's spend-limit
  check and a rate limiter's per-tx/hourly/daily/tx-count checks into one
  `{ wouldBlock, reasons }` result. It replicates the *exact* on-chain
  logic, not an approximation of it:
  - Both `PaymentChannel::pay` and `RateLimiter::check` reset their
    rolling windows (`spent_this_period` / `hourly_spend` + `daily_spend`)
    **before** evaluating the proposed amount, once the current ledger has
    passed the window's expiry. `predictPaymentOutcome` takes
    `currentLedger` explicitly and performs the same reset-then-check
    sequence — otherwise a caller holding a stale `spentThisPeriod` would
    predict a block the chain itself no longer enforces.
  - Every limit comparison uses strict `>` (landing exactly on the limit
    is allowed) **except** the hourly transaction-count check, which uses
    `>=` (once `max_txs_per_hour` slots are used, the next one is
    refused). Getting this backwards is an off-by-one that either
    double-blocks a legitimate last payment or lets one through the chain
    would reject.
  - A deliberate faithfulness quirk: `RateLimiter::kill_agent` sets
    `RateLimit.active = false`, but `RateLimiter::check` never reads that
    field — only `is_active()` (a separate query) does. So a killed
    agent's `check()` call still evaluates normally on-chain today, and
    `predictPaymentOutcome` mirrors that exactly rather than "fixing" it,
    because its contract is "agrees with `RateLimiter.check`," not "agrees
    with what `check()` probably should do."
- **`ledgerTime.ts`** — converts a ledger-count window into an estimated
  number of wall-clock seconds using an **observed** average ledger close
  time, derived from a handful of recent ledgers fetched from Horizon
  (`fetchLedgerCloseEstimate`), rather than a hard-coded "5 seconds" (which
  drifts with real network conditions). Exposes both the ledger-count and
  estimated-seconds forms, and a `observed: boolean` flag distinguishing a
  real measurement from the fallback constant used when too few samples
  are available.
- **`StellarAgent.getRateLimitStatus(agentAddress = this.address)`** — now
  takes an optional agent address, since `RateLimiter.get_limits` is keyed
  by an arbitrary agent, not necessarily the signed-in agent's own (an
  owner monitoring several agents can query any of them read-only through
  one `StellarAgent`). Still a stub pending the companion "real Soroban
  invocation" work — this only widens its future signature.
- **`StellarAgent.getLedgerCloseEstimate()`** — a real, working method
  (unlike the still-stubbed contract queries), since it's a plain Horizon
  read with no contract/signing involved — the same category as the
  already-implemented `getBalance()`.
- **`useRateLimitStatus(agentAddress, options)`** in `@stellaragent/react`
  — polls `RateLimiter.get_limits` and (when `options.channelId` is given)
  `PaymentChannel.get_channel`, folds them through `predictPaymentOutcome`,
  and exposes `wouldBlock(amount)` / `predict(amount)` plus
  `hourWindow`/`dayWindow`/`channelPeriodWindow`, each with
  `ledgersRemaining` and `estimatedSecondsRemaining`. Distinguishes
  `rateLimitConfigured: false` (never configured — `RateLimiter.check`
  always allows) from `rateLimitKilled: true` (configured, then disabled
  via `kill_agent`) — two different states that were previously
  indistinguishable from `is_active()` alone.
- `ChannelInfo` gains `period`/`periodStartLedger`; `RateLimitStatus`
  gains `configured`/`active`/`hourWindowStartLedger`/`dayWindowStartLedger`
  — fields the contracts already track that the TS types previously
  omitted, needed to compute window resets at all.

## Why the "no rate limit configured" case needs `get_limits` to fail, not `is_active`

`RateLimiter.is_active(agent)` returns `true` in **two** different
situations: nothing has ever been configured for that agent, or something
has been configured and is still active. Those need different UI treatment
("unrestricted" vs. "restricted and currently fine"), so the SDK-side
implementation of `getRateLimitStatus` (still a stub, tracked as a TODO
inline) will need to attempt `get_limits` and treat its on-chain panic
(`"no rate limit for agent"`) as the `configured: false` signal, rather
than relying on `is_active` alone.

## Testing

`pnpm --filter @stellaragent/core --filter @stellaragent/react typecheck`,
`lint`, and `test` all pass (network access for a full `pnpm install` was
unavailable in this environment — see below).

- **35 boundary-exact unit tests** (`predict.test.ts`) covering: under all
  limits, over per-tx/hourly/daily/tx-count limits, exactly-at-boundary for
  every check (including the tx-count `>=` vs the amount checks' `>`),
  window-reset behavior for both the channel period and the rate limiter's
  hour/day windows, the killed-agent-still-evaluates-normally case, and
  combined channel + rate-limiter predictions reporting every independent
  failure reason.
- **2,500 fuzzed scenarios** across two tests, cross-checked against an
  independent second transcription of `RateLimiter::check` (`shadowCheck`
  in the same file, written directly from the Rust source using plain
  `bigint` arithmetic rather than reusing any of `predict.ts`'s own
  helpers) — one pass with fixed (non-expiring) windows, one pass fuzzing
  window-expiry combinations.
- **14 tests** for `ledgerTime.ts` — average-close-time derivation
  (evenly spaced, weighted by ledger count rather than per-pair, unsorted
  input, duplicate/invalid samples skipped, all-unusable fallback), and
  `fetchLedgerCloseEstimate` against a mocked `fetch` (happy path, trailing
  slash handling, single-sample fallback, non-OK status, empty page).
- **7 new/updated hook tests** for `useRateLimitStatus` — disabled until
  ready, loads with/without a channel, queries `getRateLimitStatus` with
  the given address, unconfigured-vs-killed distinction, combined
  channel + rate-limit `wouldBlock`/`predict`, and both windows' estimated
  forms.
- A **fuzz test against a real local Soroban network**
  (`integration.local.test.ts`, gated behind
  `STELLAR_LOCAL_INTEGRATION=1`, consistent with the existing suite in that
  file) that deploys against `RateLimiter.check` directly via Soroban RPC
  (build → simulate → sign → submit → poll, the same pattern
  `circuitBreaker.ts` already uses) for 25 randomized
  (recorded-history, proposed-amount) scenarios and asserts
  `predictPaymentOutcome` agrees with the on-chain result every time. I
  could not run this myself — no `stellar` CLI or local network in this
  environment — so it's unverified by me directly; it typechecks cleanly
  and follows the exact structure of the connectivity tests already
  passing in that file.

## Scope decision: `StellarAgent`'s contract-query methods stay stubs

`getRateLimitStatus`/`getChannel` are still `Not yet implemented` — real
Soroban invocation for those is explicitly tracked as a separate,
already-in-flight companion issue (see the `it.todo` block in
`integration.local.test.ts`), and this PR doesn't take that on. What
changed here is scoped to: the prediction math itself (fully real, fully
tested), the wall-clock estimation (fully real — it's a plain Horizon
read), and widening the stub signatures/types so the hook and a future CLI
dry-run command have the right shape to call once that companion work
lands.

## Out of scope / follow-ups

- Wiring `getRateLimitStatus`/`getChannel` to real Soroban calls — blocked
  on the companion SDK issue.
- A CLI dry-run command reusing `predictPaymentOutcome` — mentioned as a
  future consumer in the design, not built here.
- Rate limiter enforcement isn't currently wired into `PaymentChannel`
  on-chain at all (they're independent contracts); `predictPaymentOutcome`
  checks both because a real payment can be gated by either mechanism
  today, but that's a pre-existing contract-level fact, not something this
  PR changes.

## Files changed

- `packages/core/src/math/predict.ts`, `packages/core/src/math/__tests__/predict.test.ts` — the predictor + its tests.
- `packages/core/src/ledgerTime.ts`, `packages/core/src/__tests__/ledgerTime.test.ts` — wall-clock estimation.
- `packages/core/src/index.ts` — `getRateLimitStatus(agentAddress)`, `getLedgerCloseEstimate()`, new exports.
- `packages/core/src/math/index.ts` — barrel export for `predict.ts`.
- `packages/core/src/types/index.ts` — `ChannelInfo`/`RateLimitStatus` new fields.
- `packages/core/src/__tests__/StellarAgent.test.ts` — tests for the two new/changed `StellarAgent` methods.
- `packages/core/src/__tests__/integration.local.test.ts` — the gated live-network fuzz test.
- `packages/react/src/hooks/useRateLimitStatus.ts` — the rewritten hook.
- `packages/react/src/__tests__/useRateLimitStatus.test.tsx`, `packages/react/src/__tests__/useChannel.test.tsx`, `packages/react/src/test/mockAgent.ts` — updated/new tests and mock support.
- `packages/react/src/index.ts` — new hook types re-exported.
- `packages/react/example/src/demoAgent.ts`, `packages/react/example/src/App.tsx` — example app updated for the new types/hook signature.
