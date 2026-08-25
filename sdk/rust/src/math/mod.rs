//! Deterministic math: the part of this SDK with a hard correctness requirement.
//!
//! Every function in [`fixed_point`] and [`bid`] must produce **byte-identical
//! strings** to its TypeScript and Python counterparts. That is not a stylistic
//! goal: `fixtures/determinism.json` is generated from the TypeScript
//! implementation and asserted against by all three suites, and the shared
//! `Determinism` CI job fails the build if any of them disagree.
//!
//! # Module map
//!
//! - [`decimal`] — the arbitrary-precision decimal type the rest is built on,
//!   with `bignumber.js`'s exact rounding and formatting rules.
//! - [`fixed_point`] — the arithmetic primitives: `add`, `div`, `pct`,
//!   `to_stroops`, and friends.
//! - [`bid`] — bid scoring and ranking, plus the spend-limit helpers.
//! - [`predict`] — off-chain prediction of whether a payment would be blocked.
//! - [`ledger_time`] — wall-clock estimates for ledger-based windows. The one
//!   module here that uses floating point, because what it produces is an
//!   estimate for display and never feeds a payment decision.
//!
//! ```
//! use stellaragent::math::{rank_bids, AgentBid, BidWeights};
//!
//! let bids = vec![AgentBid {
//!     agent_address: "GWORKER".into(),
//!     price: "0.05".into(),
//!     reputation: "88".into(),
//!     estimated_latency_seconds: "12".into(),
//!     success_rate: "0.97".into(),
//! }];
//! let ranked = rank_bids(&bids, &BidWeights::default())?;
//! // A lone bid normalises against itself, so its price and latency scores are
//! // both 0 — it is simultaneously the cheapest and the most expensive offer.
//! // Only reputation (88) and reliability (97) contribute: (88 + 97) / 4.
//! assert_eq!(ranked[0].score, "46.2500");
//! # Ok::<(), stellaragent::math::FixedPointError>(())
//! ```

pub mod bid;
pub mod decimal;
pub mod fixed_point;
pub mod ledger_time;
pub mod predict;

pub use decimal::{Decimal, ParseDecimalError, MAX_SCALE};
pub use fixed_point::{
    add, bn, bps_scale, clamp, div, eq, fmt, from_stroops, gt, gte, is_positive, is_zero, lt, lte,
    mul, pct, stroop_scale, sub, sum_strings, to_str, to_stroops, FixedPointError, IntoDecimal,
    DECIMAL_PLACES,
};

pub use bid::{
    is_within_spend_limit, rank_bids, remaining_budget, score_bid, select_best_bid, AgentBid,
    BidWeights, ScoreBreakdown, ScoredBid,
};

pub use predict::{
    is_window_expired, ledgers_per_channel_period, ledgers_remaining_in_window,
    predict_payment_outcome, BlockReason, ChannelSpendState, PaymentPrediction,
    PredictPaymentParams, RateLimitSpendState, RATE_LIMIT_LEDGERS_PER_DAY,
    RATE_LIMIT_LEDGERS_PER_HOUR,
};

pub use ledger_time::{
    estimate_ledger_close_seconds, estimate_seconds_remaining, ledger_close_estimate,
    LedgerCloseEstimate, LedgerCloseSample, FALLBACK_LEDGER_CLOSE_SECONDS,
};
