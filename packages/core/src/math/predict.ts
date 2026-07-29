/**
 * Pre-flight prediction of whether a proposed payment would be blocked by
 * either a `PaymentChannel`'s per-period spend limit or a configured
 * `RateLimiter`, computed entirely off already-fetched on-chain state — no
 * RPC round trip, no transaction fee.
 *
 * This is a deliberately low-level, environment-agnostic function: it takes
 * plain state objects rather than live contract clients, so it's usable from
 * `@stellaragent/react`'s `useRateLimitStatus` hook, a future CLI dry-run
 * command, or a test — anything that already knows the relevant on-chain
 * state.
 *
 * ## Why this has to replicate the contracts' own logic, not just call them
 *
 * Both `PaymentChannel.pay` and `RateLimiter.check` reset their rolling
 * windows (`spent_this_period` / `hourly_spend` + `daily_spend`) *before*
 * evaluating the proposed amount, whenever the current ledger has moved past
 * the window's expiry — see `reset_windows_if_needed` in
 * `contracts/rate_limiter/src/lib.rs` and the inline reset in
 * `contracts/payment_channel/src/lib.rs`'s `pay`. A caller holding
 * yesterday's `spentThisPeriod` would otherwise predict a block that the
 * chain itself will not enforce (the window already rolled over). This
 * module takes `currentLedger` explicitly and performs the same
 * reset-then-check sequence.
 *
 * ## Boundary conditions matter
 *
 * Every one of the contracts' own limit checks uses strict `>` (a payment
 * that lands *exactly* on the limit is allowed) except the hourly
 * transaction-count check, which uses `>=` (once `max_txs_per_hour` slots are
 * used, the next one is refused). Getting these backwards is an off-by-one
 * that either double-blocks a legitimate last payment or lets one through
 * that the chain would reject — the source of truth for every comparison
 * below is cited inline against `contracts/rate_limiter/src/lib.rs` and
 * `contracts/payment_channel/src/lib.rs`.
 *
 * ## A deliberate faithfulness quirk: `active` does not gate `check()`
 *
 * `RateLimiter.kill_agent` sets `RateLimit.active = false`, but
 * `RateLimiter::check` never reads that field — only `is_active()` (a
 * separate query) does. So a killed agent's `check()` call still evaluates
 * (and can pass) the per-tx/hourly/daily/tx-count comparisons on-chain today.
 * This function mirrors that exactly, because its contract is "agrees with
 * `RateLimiter.check`", not "agrees with what `RateLimiter.check` probably
 * should do". `RateLimitSpendState.active` is exposed for callers that want
 * to surface a "killed" badge, but it does not participate in `wouldBlock`.
 *
 * @module predict
 */

import { bn, add } from './fixed-point.js';
import { isWithinSpendLimit } from './bid.js';
import type { SpendPeriod } from '../types/index.js';

// ─── Ledger-window constants ─────────────────────────────────────────────────

/**
 * Ledgers per channel period, mirroring `PaymentChannel::ledgers_per_period`
 * in `contracts/payment_channel/src/lib.rs` (~5s ledgers).
 */
export const LEDGERS_PER_CHANNEL_PERIOD: Record<SpendPeriod, number> = {
  per_ledger: 1,
  hourly: 720,
  daily: 17_280,
};

/**
 * `RateLimiter`'s hourly/daily windows are fixed cadences, independent of any
 * channel's own configurable period — mirroring the constants inside
 * `RateLimiter::reset_windows_if_needed` in
 * `contracts/rate_limiter/src/lib.rs`.
 */
export const RATE_LIMIT_LEDGERS_PER_HOUR = 720;
export const RATE_LIMIT_LEDGERS_PER_DAY = 17_280;

// ─── Input state ──────────────────────────────────────────────────────────────

/** The subset of `Channel` (contracts/payment_channel/src/lib.rs) needed to predict `pay`'s spend-limit check. */
export interface ChannelSpendState {
  active: boolean;
  limitPerPeriod: string;
  spentThisPeriod: string;
  periodStartLedger: number;
  period: SpendPeriod;
}

/** The subset of `RateLimit` (contracts/rate_limiter/src/lib.rs) needed to predict `check`. */
export interface RateLimitSpendState {
  /** `has_limit(agent)` on-chain — `false` means `check()` always returns `true`. */
  configured: boolean;
  /** `RateLimit.active` — see the module doc for why this does not gate `wouldBlock`. */
  active: boolean;
  maxPerTx: string;
  maxPerHour: string;
  maxPerDay: string;
  maxTxsPerHour: number;
  hourlySpend: string;
  dailySpend: string;
  hourlyTxCount: number;
  hourWindowStartLedger: number;
  dayWindowStartLedger: number;
}

export interface PredictPaymentOutcomeParams {
  /** Omit (or pass `null`) if the payment isn't going through a channel at all. */
  channelState?: ChannelSpendState | null;
  /** Omit (or pass `null`) if no `RateLimiter` applies to this agent/path. */
  rateLimitState?: RateLimitSpendState | null;
  /** Proposed payment amount, same unit as the channel/rate-limit state (stroops as a decimal string). */
  amount: string;
  /** Current ledger sequence — used to replicate the contracts' reset-before-check window semantics. */
  currentLedger: number;
}

/** Every distinct reason `predictPaymentOutcome` can cite for a block, each tied to one specific on-chain check. */
export type BlockReason =
  | 'invalid_amount'
  | 'channel_inactive'
  | 'channel_spend_limit'
  | 'rate_limit_per_tx'
  | 'rate_limit_hourly'
  | 'rate_limit_daily'
  | 'rate_limit_tx_count';

export interface PaymentPrediction {
  /** `true` if any reason fired — i.e. the on-chain call(s) are predicted to fail. */
  wouldBlock: boolean;
  /** Every check that would fail, most upstream first. Empty when `wouldBlock` is `false`. */
  reasons: BlockReason[];
}

// ─── Window-reset helpers (shared logic, exported for the React hook) ───────

/** Whether a rolling window that started at `windowStartLedger` has expired by `currentLedger`. */
export function isWindowExpired(
  windowStartLedger: number,
  ledgersPerWindow: number,
  currentLedger: number,
): boolean {
  return currentLedger >= windowStartLedger + ledgersPerWindow;
}

/**
 * Ledgers remaining until a rolling window resets, floored at 0 (an expired
 * window has 0 remaining, not a negative count).
 */
export function ledgersRemainingInWindow(
  windowStartLedger: number,
  ledgersPerWindow: number,
  currentLedger: number,
): number {
  return Math.max(0, windowStartLedger + ledgersPerWindow - currentLedger);
}

// ─── The predictor ────────────────────────────────────────────────────────────

/**
 * Predict whether a proposed `amount` would be blocked by a channel's spend
 * limit and/or a configured rate limiter, without an RPC round trip.
 *
 * Pass `channelState: null`/`undefined` and/or `rateLimitState:
 * null`/`undefined` to skip either check (e.g. a payment with no channel, or
 * an agent with no `RateLimiter` configured at all).
 */
export function predictPaymentOutcome({
  channelState,
  rateLimitState,
  amount,
  currentLedger,
}: PredictPaymentOutcomeParams): PaymentPrediction {
  const reasons: BlockReason[] = [];
  const amt = bn(amount);

  // Mirrors `PaymentChannel::pay`'s `if amount <= 0 { panic!(...) }` /
  // `RateLimiter`'s implicit assumption of a positive amount.
  if (!amt.isGreaterThan(0)) {
    reasons.push('invalid_amount');
  }

  if (channelState) {
    // `pay`: `if !channel.active { panic!("channel is closed"); }`
    if (!channelState.active) {
      reasons.push('channel_inactive');
    } else {
      const ledgersPerPeriod = LEDGERS_PER_CHANNEL_PERIOD[channelState.period];
      const expired = isWindowExpired(
        channelState.periodStartLedger,
        ledgersPerPeriod,
        currentLedger,
      );
      // `pay` zeroes `spent_this_period` before checking, once the period
      // has rolled over.
      const effectiveSpent = expired ? '0' : channelState.spentThisPeriod;
      // `pay`: `if channel.spent_this_period + amount > channel.limit_per_period { panic!(...) }`
      // `isWithinSpendLimit` uses `<=`, the exact negation of that `>`.
      if (!isWithinSpendLimit(effectiveSpent, channelState.limitPerPeriod, amount)) {
        reasons.push('channel_spend_limit');
      }
    }
  }

  if (rateLimitState && rateLimitState.configured) {
    // See the module doc: `check()` does not gate on `active` — intentionally
    // not checked here either, to stay faithful to on-chain behavior.

    // `check`: `if amount > limit.max_per_tx { return false; }`
    if (amt.isGreaterThan(bn(rateLimitState.maxPerTx))) {
      reasons.push('rate_limit_per_tx');
    }

    const hourExpired = isWindowExpired(
      rateLimitState.hourWindowStartLedger,
      RATE_LIMIT_LEDGERS_PER_HOUR,
      currentLedger,
    );
    const dayExpired = isWindowExpired(
      rateLimitState.dayWindowStartLedger,
      RATE_LIMIT_LEDGERS_PER_DAY,
      currentLedger,
    );

    // `check` zeroes `hourly_spend`/`hourly_tx_count` and/or `daily_spend`
    // before checking, exactly like `reset_windows_if_needed`.
    const effectiveHourlySpend = hourExpired ? bn('0') : bn(rateLimitState.hourlySpend);
    const effectiveDailySpend = dayExpired ? bn('0') : bn(rateLimitState.dailySpend);
    const effectiveHourlyTxCount = hourExpired ? 0 : rateLimitState.hourlyTxCount;

    // `check`: `if limit.hourly_spend + amount > limit.max_per_hour { return false; }`
    if (add(effectiveHourlySpend, amt).isGreaterThan(bn(rateLimitState.maxPerHour))) {
      reasons.push('rate_limit_hourly');
    }
    // `check`: `if limit.daily_spend + amount > limit.max_per_day { return false; }`
    if (add(effectiveDailySpend, amt).isGreaterThan(bn(rateLimitState.maxPerDay))) {
      reasons.push('rate_limit_daily');
    }
    // `check`: `if limit.hourly_tx_count >= limit.max_txs_per_hour { return false; }`
    // Note `>=`, unlike every amount comparison above — the boundary case
    // (count already equal to the cap) blocks, it does not allow one more.
    if (effectiveHourlyTxCount >= rateLimitState.maxTxsPerHour) {
      reasons.push('rate_limit_tx_count');
    }
  }

  return { wouldBlock: reasons.length > 0, reasons };
}
