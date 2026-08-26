//! Pre-flight prediction of whether a payment would be blocked, computed
//! entirely off already-fetched on-chain state — no RPC round trip, no fee.
//!
//! Rust port of `packages/core/src/math/predict.ts`. Deliberately low-level
//! and environment-agnostic: it takes plain state structs rather than live
//! contract clients, so it is usable from an agent loop that already holds the
//! relevant state, from a CLI dry-run, or from a test.
//!
//! # Why this replicates the contracts' logic instead of just calling them
//!
//! Both `PaymentChannel::pay` and `RateLimiter::check` reset their rolling
//! windows (`spent_this_period` / `hourly_spend` + `daily_spend`) **before**
//! evaluating the proposed amount, whenever the current ledger has moved past
//! the window's expiry — see `reset_windows_if_needed` in
//! `contracts/rate_limiter/src/lib.rs` and the inline reset in
//! `contracts/payment_channel/src/lib.rs`. A caller holding yesterday's
//! `spent_this_period` would otherwise predict a block the chain will not
//! enforce, because the window already rolled over. This module takes
//! `current_ledger` explicitly and performs the same reset-then-check sequence.
//!
//! # Boundary conditions matter
//!
//! Every one of the contracts' limit checks uses strict `>` — a payment
//! landing *exactly* on the limit is allowed — **except** the hourly
//! transaction-count check, which uses `>=`: once `max_txs_per_hour` slots are
//! used, the next one is refused. Getting these backwards is an off-by-one
//! that either double-blocks a legitimate last payment or lets one through
//! that the chain would reject. Each comparison below cites its source line.
//!
//! # A deliberate faithfulness quirk: `active` does not gate `check()`
//!
//! `RateLimiter::kill_agent` sets `RateLimit.active = false`, but
//! `RateLimiter::check` never reads that field — only `is_active()`, a
//! separate query, does. So a killed agent's `check()` call still evaluates
//! (and can pass) the per-tx/hourly/daily/tx-count comparisons on-chain today.
//! This module mirrors that exactly, because its contract is "agrees with
//! `RateLimiter::check`", not "agrees with what `RateLimiter::check` probably
//! should do". [`RateLimitSpendState::active`] is exposed for callers that
//! want to surface a "killed" badge, but it does not participate in
//! [`PaymentPrediction::would_block`].

use serde::{Deserialize, Serialize};

use super::bid::is_within_spend_limit;
use super::fixed_point::{add, bn, FixedPointError};
use crate::types::SpendPeriod;

// ─── Ledger-window constants ─────────────────────────────────────────────────

/// `RateLimiter`'s hourly window, in ledgers — a fixed cadence independent of
/// any channel's own configurable period. Mirrors the constant inside
/// `RateLimiter::reset_windows_if_needed`.
pub const RATE_LIMIT_LEDGERS_PER_HOUR: u32 = 720;

/// `RateLimiter`'s daily window, in ledgers. See [`RATE_LIMIT_LEDGERS_PER_HOUR`].
pub const RATE_LIMIT_LEDGERS_PER_DAY: u32 = 17_280;

/// Ledgers per channel period, mirroring `PaymentChannel::ledgers_per_period`
/// in `contracts/payment_channel/src/lib.rs` (~5s ledgers).
pub const fn ledgers_per_channel_period(period: SpendPeriod) -> u32 {
    match period {
        SpendPeriod::PerLedger => 1,
        SpendPeriod::Hourly => 720,
        SpendPeriod::Daily => 17_280,
    }
}

// ─── Input state ─────────────────────────────────────────────────────────────

/// The subset of `Channel` needed to predict `pay`'s spend-limit check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSpendState {
    /// `Channel.active` — a closed channel rejects every payment.
    pub active: bool,
    /// The configured per-period limit, in stroops as a decimal string.
    pub limit_per_period: String,
    /// Already spent in the current period, in the same unit.
    pub spent_this_period: String,
    /// Ledger the current period started at.
    pub period_start_ledger: u32,
    /// Which cadence this channel's period follows.
    pub period: SpendPeriod,
}

/// The subset of `RateLimit` needed to predict `check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSpendState {
    /// `has_limit(agent)` on-chain — `false` means `check()` always returns true.
    pub configured: bool,
    /// `RateLimit.active` — see the module docs for why this does not gate the prediction.
    pub active: bool,
    /// Per-transaction ceiling.
    pub max_per_tx: String,
    /// Rolling hourly spend ceiling.
    pub max_per_hour: String,
    /// Rolling daily spend ceiling.
    pub max_per_day: String,
    /// Rolling hourly transaction-count ceiling.
    pub max_txs_per_hour: u32,
    /// Spend recorded in the current hourly window.
    pub hourly_spend: String,
    /// Spend recorded in the current daily window.
    pub daily_spend: String,
    /// Transactions recorded in the current hourly window.
    pub hourly_tx_count: u32,
    /// Ledger the current hourly window started at.
    pub hour_window_start_ledger: u32,
    /// Ledger the current daily window started at.
    pub day_window_start_ledger: u32,
}

/// Everything [`predict_payment_outcome`] needs.
///
/// Leave `channel_state` as `None` when the payment does not go through a
/// channel at all, and `rate_limit_state` as `None` when no `RateLimiter`
/// applies to this agent.
#[derive(Debug, Clone, Default)]
pub struct PredictPaymentParams<'a> {
    /// Channel state, or `None` to skip the channel check entirely.
    pub channel_state: Option<&'a ChannelSpendState>,
    /// Rate-limit state, or `None` to skip the rate-limit checks entirely.
    pub rate_limit_state: Option<&'a RateLimitSpendState>,
    /// Proposed amount, in the same unit as the state above (stroops, as a decimal string).
    pub amount: &'a str,
    /// Current ledger sequence — replicates the contracts' reset-before-check semantics.
    pub current_ledger: u32,
}

/// Every distinct reason a payment can be predicted to fail, each tied to one
/// specific on-chain check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// The amount is not strictly positive.
    InvalidAmount,
    /// The channel is closed.
    ChannelInactive,
    /// `spent_this_period + amount` would exceed the channel's limit.
    ChannelSpendLimit,
    /// The amount alone exceeds `max_per_tx`.
    RateLimitPerTx,
    /// The rolling hourly spend would be exceeded.
    RateLimitHourly,
    /// The rolling daily spend would be exceeded.
    RateLimitDaily,
    /// The hourly transaction-count cap is already used up.
    RateLimitTxCount,
}

impl BlockReason {
    /// The stable string form, identical to the TypeScript `BlockReason` union.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidAmount => "invalid_amount",
            Self::ChannelInactive => "channel_inactive",
            Self::ChannelSpendLimit => "channel_spend_limit",
            Self::RateLimitPerTx => "rate_limit_per_tx",
            Self::RateLimitHourly => "rate_limit_hourly",
            Self::RateLimitDaily => "rate_limit_daily",
            Self::RateLimitTxCount => "rate_limit_tx_count",
        }
    }
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The outcome of a prediction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPrediction {
    /// `true` if any reason fired — the on-chain call is predicted to fail.
    pub would_block: bool,
    /// Every check that would fail, most upstream first. Empty when `would_block` is false.
    pub reasons: Vec<BlockReason>,
}

// ─── Window-reset helpers ────────────────────────────────────────────────────

/// Whether a rolling window that started at `window_start_ledger` has expired.
///
/// ```
/// use stellaragent::math::is_window_expired;
///
/// assert!(!is_window_expired(1000, 720, 1719)); // one ledger short
/// assert!(is_window_expired(1000, 720, 1720));  // exactly at the boundary
/// ```
pub const fn is_window_expired(
    window_start_ledger: u32,
    ledgers_per_window: u32,
    current_ledger: u32,
) -> bool {
    current_ledger >= window_start_ledger.saturating_add(ledgers_per_window)
}

/// Ledgers remaining until a rolling window resets, floored at 0.
///
/// An expired window has 0 remaining, never a negative count — which is why
/// this returns `u32` rather than a signed type that would invite a caller to
/// render "-40 ledgers remaining".
///
/// ```
/// use stellaragent::math::ledgers_remaining_in_window;
///
/// assert_eq!(ledgers_remaining_in_window(1000, 720, 1700), 20);
/// assert_eq!(ledgers_remaining_in_window(1000, 720, 9999), 0);
/// ```
pub const fn ledgers_remaining_in_window(
    window_start_ledger: u32,
    ledgers_per_window: u32,
    current_ledger: u32,
) -> u32 {
    window_start_ledger
        .saturating_add(ledgers_per_window)
        .saturating_sub(current_ledger)
}

// ─── The predictor ───────────────────────────────────────────────────────────

/// Predict whether a proposed amount would be blocked by a channel's spend
/// limit and/or a configured rate limiter, without an RPC round trip.
///
/// ```
/// use stellaragent::math::{
///     predict_payment_outcome, BlockReason, ChannelSpendState, PredictPaymentParams,
/// };
/// use stellaragent::types::SpendPeriod;
///
/// let channel = ChannelSpendState {
///     active: true,
///     limit_per_period: "10000".into(),
///     spent_this_period: "9900".into(),
///     period_start_ledger: 1000,
///     period: SpendPeriod::Hourly,
/// };
///
/// // Inside the window, 200 would overrun the remaining 100.
/// let blocked = predict_payment_outcome(PredictPaymentParams {
///     channel_state: Some(&channel),
///     rate_limit_state: None,
///     amount: "200",
///     current_ledger: 1500,
/// })?;
/// assert_eq!(blocked.reasons, vec![BlockReason::ChannelSpendLimit]);
///
/// // Past the window, the contract zeroes `spent_this_period` first — so does this.
/// let allowed = predict_payment_outcome(PredictPaymentParams {
///     channel_state: Some(&channel),
///     rate_limit_state: None,
///     amount: "200",
///     current_ledger: 1720,
/// })?;
/// assert!(!allowed.would_block);
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn predict_payment_outcome(
    params: PredictPaymentParams<'_>,
) -> Result<PaymentPrediction, FixedPointError> {
    let PredictPaymentParams {
        channel_state,
        rate_limit_state,
        amount,
        current_ledger,
    } = params;

    let mut reasons = Vec::new();
    let amount_value = bn(amount)?;

    // Mirrors `PaymentChannel::pay`'s `if amount <= 0 { panic!(...) }`.
    if !amount_value.is_positive() {
        reasons.push(BlockReason::InvalidAmount);
    }

    if let Some(channel) = channel_state {
        // `pay`: `if !channel.active { panic!("channel is closed"); }`
        if !channel.active {
            reasons.push(BlockReason::ChannelInactive);
        } else {
            let expired = is_window_expired(
                channel.period_start_ledger,
                ledgers_per_channel_period(channel.period),
                current_ledger,
            );
            // `pay` zeroes `spent_this_period` before checking, once the
            // period has rolled over.
            let effective_spent = if expired {
                "0"
            } else {
                channel.spent_this_period.as_str()
            };
            // `pay`: `if channel.spent_this_period + amount > channel.limit_per_period { panic!(...) }`
            // `is_within_spend_limit` uses `<=`, the exact negation of that `>`.
            if !is_within_spend_limit(effective_spent, &channel.limit_per_period, &amount_value)? {
                reasons.push(BlockReason::ChannelSpendLimit);
            }
        }
    }

    if let Some(limit) = rate_limit_state.filter(|state| state.configured) {
        // See the module docs: `check()` does not gate on `active` —
        // intentionally not checked here either, to stay faithful.

        // `check`: `if amount > limit.max_per_tx { return false; }`
        if amount_value > bn(&limit.max_per_tx)? {
            reasons.push(BlockReason::RateLimitPerTx);
        }

        let hour_expired = is_window_expired(
            limit.hour_window_start_ledger,
            RATE_LIMIT_LEDGERS_PER_HOUR,
            current_ledger,
        );
        let day_expired = is_window_expired(
            limit.day_window_start_ledger,
            RATE_LIMIT_LEDGERS_PER_DAY,
            current_ledger,
        );

        // `check` zeroes `hourly_spend`/`hourly_tx_count` and/or `daily_spend`
        // before checking, exactly like `reset_windows_if_needed`.
        let effective_hourly = if hour_expired {
            "0"
        } else {
            limit.hourly_spend.as_str()
        };
        let effective_daily = if day_expired {
            "0"
        } else {
            limit.daily_spend.as_str()
        };
        let effective_tx_count = if hour_expired {
            0
        } else {
            limit.hourly_tx_count
        };

        // `check`: `if limit.hourly_spend + amount > limit.max_per_hour { return false; }`
        if add(effective_hourly, &amount_value)? > bn(&limit.max_per_hour)? {
            reasons.push(BlockReason::RateLimitHourly);
        }
        // `check`: `if limit.daily_spend + amount > limit.max_per_day { return false; }`
        if add(effective_daily, &amount_value)? > bn(&limit.max_per_day)? {
            reasons.push(BlockReason::RateLimitDaily);
        }
        // `check`: `if limit.hourly_tx_count >= limit.max_txs_per_hour { return false; }`
        // Note `>=`, unlike every amount comparison above — the boundary case
        // (count already equal to the cap) blocks, it does not allow one more.
        if effective_tx_count >= limit.max_txs_per_hour {
            reasons.push(BlockReason::RateLimitTxCount);
        }
    }

    Ok(PaymentPrediction {
        would_block: !reasons.is_empty(),
        reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> ChannelSpendState {
        ChannelSpendState {
            active: true,
            limit_per_period: "10000".into(),
            spent_this_period: "9900".into(),
            period_start_ledger: 1000,
            period: SpendPeriod::Hourly,
        }
    }

    fn rate_limit() -> RateLimitSpendState {
        RateLimitSpendState {
            configured: true,
            active: true,
            max_per_tx: "1000".into(),
            max_per_hour: "5000".into(),
            max_per_day: "20000".into(),
            max_txs_per_hour: 10,
            hourly_spend: "4900".into(),
            daily_spend: "19000".into(),
            hourly_tx_count: 9,
            hour_window_start_ledger: 1000,
            day_window_start_ledger: 1000,
        }
    }

    fn predict(
        channel_state: Option<&ChannelSpendState>,
        rate_limit_state: Option<&RateLimitSpendState>,
        amount: &str,
        current_ledger: u32,
    ) -> PaymentPrediction {
        predict_payment_outcome(PredictPaymentParams {
            channel_state,
            rate_limit_state,
            amount,
            current_ledger,
        })
        .expect("valid inputs")
    }

    #[test]
    fn a_non_positive_amount_is_rejected_before_anything_else() {
        for amount in ["0", "-1", "-0.0001"] {
            let prediction = predict(None, None, amount, 1000);
            assert_eq!(prediction.reasons[0], BlockReason::InvalidAmount);
        }
    }

    #[test]
    fn no_state_at_all_means_nothing_to_block_on() {
        assert!(!predict(None, None, "1000000", 1).would_block);
    }

    #[test]
    fn a_closed_channel_blocks_without_evaluating_the_limit() {
        let mut closed = channel();
        closed.active = false;
        let prediction = predict(Some(&closed), None, "1", 1000);
        assert_eq!(prediction.reasons, vec![BlockReason::ChannelInactive]);
    }

    #[test]
    fn the_channel_limit_boundary_is_inclusive() {
        // 9900 spent of 10000: exactly 100 is allowed, 101 is not.
        assert!(!predict(Some(&channel()), None, "100", 1500).would_block);
        assert_eq!(
            predict(Some(&channel()), None, "101", 1500).reasons,
            vec![BlockReason::ChannelSpendLimit]
        );
    }

    #[test]
    fn an_expired_channel_period_is_reset_before_the_check() {
        // The chain zeroes `spent_this_period` on rollover, so predicting a
        // block here would be predicting one the chain will not enforce.
        assert!(!predict(Some(&channel()), None, "9999", 1720).would_block);
        assert!(predict(Some(&channel()), None, "9999", 1719).would_block);
    }

    #[test]
    fn per_ledger_periods_roll_over_every_single_ledger() {
        let mut per_ledger = channel();
        per_ledger.period = SpendPeriod::PerLedger;
        assert!(!predict(Some(&per_ledger), None, "9999", 1001).would_block);
    }

    #[test]
    fn an_unconfigured_rate_limit_never_blocks() {
        let mut unconfigured = rate_limit();
        unconfigured.configured = false;
        assert!(!predict(None, Some(&unconfigured), "999999", 1000).would_block);
    }

    #[test]
    fn a_killed_agent_still_evaluates_the_comparisons() {
        // Faithful to `RateLimiter::check`, which never reads `active`.
        let mut killed = rate_limit();
        killed.active = false;
        assert!(!predict(None, Some(&killed), "100", 1000).would_block);
    }

    #[test]
    fn every_rate_limit_dimension_reports_separately() {
        let prediction = predict(None, Some(&rate_limit()), "1001", 1000);
        assert_eq!(
            prediction.reasons,
            vec![
                BlockReason::RateLimitPerTx,
                BlockReason::RateLimitHourly,
                BlockReason::RateLimitDaily,
            ]
        );
    }

    #[test]
    fn the_tx_count_cap_blocks_at_equality_not_above_it() {
        let mut at_cap = rate_limit();
        at_cap.hourly_tx_count = 10;
        at_cap.hourly_spend = "0".into();
        at_cap.daily_spend = "0".into();
        assert_eq!(
            predict(None, Some(&at_cap), "1", 1000).reasons,
            vec![BlockReason::RateLimitTxCount]
        );

        at_cap.hourly_tx_count = 9;
        assert!(!predict(None, Some(&at_cap), "1", 1000).would_block);
    }

    #[test]
    fn expired_rate_limit_windows_reset_independently_of_each_other() {
        let mut limit = rate_limit();
        limit.daily_spend = "19500".into();

        // Inside both windows: the hourly cap (4900 + 900 > 5000) and the
        // daily cap (19500 + 900 > 20000) both bite.
        assert_eq!(
            predict(None, Some(&limit), "900", 1500).reasons,
            vec![BlockReason::RateLimitHourly, BlockReason::RateLimitDaily]
        );

        // Past the hour boundary (1000 + 720) but well inside the day
        // (1000 + 17280): the hourly spend and tx count reset, the daily
        // spend does not. Resetting both together would wrongly allow this.
        assert_eq!(
            predict(None, Some(&limit), "900", 1720).reasons,
            vec![BlockReason::RateLimitDaily]
        );
    }

    #[test]
    fn channel_and_rate_limit_reasons_accumulate() {
        let prediction = predict(Some(&channel()), Some(&rate_limit()), "5000", 1500);
        assert!(prediction.would_block);
        assert!(prediction.reasons.contains(&BlockReason::ChannelSpendLimit));
        assert!(prediction.reasons.contains(&BlockReason::RateLimitPerTx));
    }

    #[test]
    fn window_helpers_saturate_instead_of_wrapping() {
        assert!(is_window_expired(u32::MAX, 720, u32::MAX));
        assert_eq!(ledgers_remaining_in_window(0, 720, u32::MAX), 0);
        assert_eq!(ledgers_remaining_in_window(1000, 720, 1700), 20);
    }
}
