//! Deterministic agent bidding algorithm.
//!
//! Rust port of `packages/core/src/math/bid.ts`. Every function here must
//! produce byte-identical strings to its TypeScript counterpart — see
//! `fixtures/determinism.json` and the shared regression suite in
//! `tests/determinism.rs`.
//!
//! # Background
//!
//! When several agents compete for an escrow job, the requester (or an
//! on-chain arbiter) needs a reproducible bid score so the same inputs always
//! produce the same winner, whichever machine and whichever SDK computed it.
//! A scorer that disagreed with its counterparty about the winner would be
//! indistinguishable from one that cheated.
//!
//! # Scoring formula
//!
//! Each bid is scored 0–100 as a weighted combination:
//!
//! ```text
//! score = price_weight   * price_score
//!       + rep_weight     * reputation_score
//!       + latency_weight * latency_score
//!       + reliab_weight  * reliability_score
//! ```
//!
//! with sub-scores normalised to `[0, 100]`:
//!
//! - `price_score`       = `100 * (1 - price / max_bid)` — lower is better
//! - `reputation_score`  = `reputation` (already 0–100)
//! - `latency_score`     = `100 * (1 - latency / max_latency)` — lower is better
//! - `reliability_score` = `success_rate * 100` — a 0–1 fraction
//!
//! Weights must sum to **exactly** 1. The check is exact equality rather than
//! a tolerance: an epsilon comparison would be sensitive to accumulated
//! representation error and defeat the whole point of the module.

use serde::{Deserialize, Serialize};

use super::decimal::Decimal;
use super::fixed_point::{add, bn, clamp, div, mul, sub, FixedPointError, IntoDecimal};

// ─── Types ───────────────────────────────────────────────────────────────────

/// A single agent's bid for an escrow job.
///
/// Every numeric field is a decimal string, never a float — the same rule the
/// TypeScript interface documents and the Python dataclass enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBid {
    /// Unique agent address.
    pub agent_address: String,
    /// Price the agent will accept, e.g. `"0.05"`.
    pub price: String,
    /// Reputation score 0–100, from on-chain history.
    pub reputation: String,
    /// Expected completion time in seconds. Lower is better.
    pub estimated_latency_seconds: String,
    /// Lifetime success rate as a fraction 0–1, e.g. `"0.97"`.
    pub success_rate: String,
}

/// Relative importance of each scoring dimension. Must sum to exactly 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BidWeights {
    /// Weight for price competitiveness.
    pub price: String,
    /// Weight for reputation.
    pub reputation: String,
    /// Weight for latency.
    pub latency: String,
    /// Weight for reliability / success rate.
    pub reliability: String,
}

impl BidWeights {
    /// An equal split across all four dimensions — the SDK-wide default.
    ///
    /// ```
    /// use stellaragent::math::BidWeights;
    /// assert_eq!(BidWeights::default().price, "0.25");
    /// ```
    pub fn equal() -> Self {
        Self {
            price: "0.25".to_string(),
            reputation: "0.25".to_string(),
            latency: "0.25".to_string(),
            reliability: "0.25".to_string(),
        }
    }
}

impl Default for BidWeights {
    fn default() -> Self {
        Self::equal()
    }
}

/// Individual sub-scores, for transparency. Each is a 4-decimal string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreBreakdown {
    /// `100 * (1 - price / max_bid)`, clamped to `[0, 100]`.
    pub price_score: String,
    /// The bid's reputation, clamped to `[0, 100]`.
    pub reputation_score: String,
    /// `100 * (1 - latency / max_latency)`, clamped to `[0, 100]`.
    pub latency_score: String,
    /// `success_rate * 100`, clamped to `[0, 100]`.
    pub reliability_score: String,
}

/// A scored bid, ready for ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredBid {
    /// The address of the agent that submitted the bid.
    pub agent_address: String,
    /// Final composite score 0–100, truncated to 4 decimal places.
    pub score: String,
    /// The four sub-scores the composite was built from.
    pub breakdown: ScoreBreakdown,
}

// ─── Core scoring ────────────────────────────────────────────────────────────

/// Compute a deterministic composite score for one bid.
///
/// `max_bid` and `max_latency` are the normalisers — the highest price and
/// latency across the competing set. [`rank_bids`] derives them for you;
/// call this directly only when scoring against a normaliser you already have.
///
/// # Errors
///
/// [`FixedPointError::InvalidWeights`] when the weights do not sum to exactly
/// 1, and [`FixedPointError::NotFinite`] for any unparseable field.
///
/// ```
/// use stellaragent::math::{score_bid, AgentBid, BidWeights};
///
/// let bid = AgentBid {
///     agent_address: "GPERFECT".into(),
///     price: "0".into(),
///     reputation: "100".into(),
///     estimated_latency_seconds: "0".into(),
///     success_rate: "1".into(),
/// };
/// let scored = score_bid(&bid, "10", "10", &BidWeights::default())?;
/// assert_eq!(scored.score, "100.0000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn score_bid<M: IntoDecimal, L: IntoDecimal>(
    bid: &AgentBid,
    max_bid: M,
    max_latency: L,
    weights: &BidWeights,
) -> Result<ScoredBid, FixedPointError> {
    let weight_sum = add(
        add(add(&weights.price, &weights.reputation)?, &weights.latency)?,
        &weights.reliability,
    )?;
    if weight_sum != bn("1")? {
        return Err(FixedPointError::InvalidWeights {
            sum: weight_sum.to_fixed(4),
        });
    }

    let max_bid = bn(max_bid)?;
    let max_latency = bn(max_latency)?;
    let hundred = Decimal::from(100u8);

    // Price: 100 * (1 - price/max_bid). A zero normaliser means every bid in
    // the set is free, so short-circuit to full marks rather than dividing by
    // zero — the same branch the other two SDKs take.
    let price_score = if max_bid.is_zero() {
        hundred.clone()
    } else {
        let ratio = div(&bid.price, &max_bid)?;
        clamp(mul(sub("1", ratio)?, "100")?, "0", "100")?
    };

    let reputation_score = clamp(bn(&bid.reputation)?, "0", "100")?;

    // Latency: 100 * (1 - latency/max_latency). Same zero-normaliser case.
    let latency_score = if max_latency.is_zero() {
        hundred
    } else {
        let ratio = div(&bid.estimated_latency_seconds, &max_latency)?;
        clamp(mul(sub("1", ratio)?, "100")?, "0", "100")?
    };

    let reliability_score = clamp(mul(&bid.success_rate, "100")?, "0", "100")?;

    let composite = add(
        add(
            add(
                mul(&price_score, &weights.price)?,
                mul(&reputation_score, &weights.reputation)?,
            )?,
            mul(&latency_score, &weights.latency)?,
        )?,
        mul(&reliability_score, &weights.reliability)?,
    )?;

    Ok(ScoredBid {
        agent_address: bid.agent_address.clone(),
        score: fixed4(&composite),
        breakdown: ScoreBreakdown {
            price_score: fixed4(&price_score),
            reputation_score: fixed4(&reputation_score),
            latency_score: fixed4(&latency_score),
            reliability_score: fixed4(&reliability_score),
        },
    })
}

/// Rank competing bids deterministically, best first.
///
/// Normalises against the maximum price and latency across the whole set,
/// scores every bid, then sorts descending by score with ties broken
/// lexicographically on `agent_address` — so the ordering is reproducible
/// regardless of the order the bids arrived in.
///
/// ```
/// use stellaragent::math::{rank_bids, AgentBid, BidWeights};
///
/// let bids = vec![
///     AgentBid {
///         agent_address: "GCHEAP".into(),
///         price: "1".into(),
///         reputation: "50".into(),
///         estimated_latency_seconds: "10".into(),
///         success_rate: "0.9".into(),
///     },
///     AgentBid {
///         agent_address: "GPRICEY".into(),
///         price: "9".into(),
///         reputation: "50".into(),
///         estimated_latency_seconds: "10".into(),
///         success_rate: "0.9".into(),
///     },
/// ];
/// let ranked = rank_bids(&bids, &BidWeights::default())?;
/// assert_eq!(ranked[0].agent_address, "GCHEAP");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn rank_bids(
    bids: &[AgentBid],
    weights: &BidWeights,
) -> Result<Vec<ScoredBid>, FixedPointError> {
    if bids.is_empty() {
        return Ok(Vec::new());
    }

    // The normalisers are truncated to 7 decimal places *before* scoring, as
    // in the TypeScript original. This is observable in the final digit of
    // the score, so it is not a formatting detail that can be skipped.
    let max_bid = max_field(bids, |b| &b.price)?;
    let max_latency = max_field(bids, |b| &b.estimated_latency_seconds)?;

    let mut scored = bids
        .iter()
        .map(|bid| score_bid(bid, &max_bid, &max_latency, weights))
        .collect::<Result<Vec<_>, _>>()?;

    // Descending score, then ascending address. Parsing each score once up
    // front keeps this O(n log n) parses rather than O(n log n) *comparisons*
    // each re-parsing two decimal strings.
    let mut keyed = scored
        .drain(..)
        .map(|s| Ok((bn(&s.score)?, s)))
        .collect::<Result<Vec<_>, FixedPointError>>()?;
    keyed.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| a.agent_address.cmp(&b.agent_address))
    });

    Ok(keyed.into_iter().map(|(_, s)| s).collect())
}

/// Select the single best bid, or `None` when the pool is empty.
pub fn select_best_bid(
    bids: &[AgentBid],
    weights: &BidWeights,
) -> Result<Option<ScoredBid>, FixedPointError> {
    Ok(rank_bids(bids, weights)?.into_iter().next())
}

// ─── Spend limits ────────────────────────────────────────────────────────────

/// Whether a proposed payment stays within the spend limit.
///
/// Replicates the on-chain `PaymentChannel.pay` guard so the SDK can
/// pre-validate without a network round trip. **Inclusive at the limit**: the
/// contract's own check is `spent + amount > limit`, so a payment landing
/// exactly on the limit is allowed. Getting that boundary backwards would
/// block a legitimate final payment on every channel.
///
/// ```
/// use stellaragent::math::is_within_spend_limit;
///
/// assert!(is_within_spend_limit("9500", "10000", "500")?);  // exactly on the limit
/// assert!(!is_within_spend_limit("9500", "10000", "501")?);
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn is_within_spend_limit<S: IntoDecimal, L: IntoDecimal, P: IntoDecimal>(
    spent_this_period: S,
    limit_per_period: L,
    proposed_amount: P,
) -> Result<bool, FixedPointError> {
    Ok(add(spent_this_period, proposed_amount)? <= bn(limit_per_period)?)
}

/// Remaining budget as an integer stroop string, never negative.
///
/// ```
/// use stellaragent::math::remaining_budget;
///
/// assert_eq!(remaining_budget("1000", "10000")?, "9000");
/// assert_eq!(remaining_budget("99999", "10000")?, "0"); // clamped, not negative
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn remaining_budget<S: IntoDecimal, L: IntoDecimal>(
    spent_this_period: S,
    limit_per_period: L,
) -> Result<String, FixedPointError> {
    let remaining = sub(limit_per_period, spent_this_period)?;
    let zero = Decimal::zero();
    Ok(if remaining > zero { remaining } else { zero }.to_fixed(0))
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// The TypeScript `.decimalPlaces(4, ROUND_DOWN).toFixed(4)`.
///
/// Truncating *then* formatting, not `to_fixed(4)` directly: the two differ
/// for a negative value that truncates away to zero, and this is the order the
/// TypeScript original uses.
fn fixed4(value: &Decimal) -> String {
    value.truncate(4).to_fixed(4)
}

/// The largest value of one field across the set, truncated to 7 places —
/// the TypeScript `.toFixed(7)` applied to a normaliser.
fn max_field<F>(bids: &[AgentBid], field: F) -> Result<Decimal, FixedPointError>
where
    F: Fn(&AgentBid) -> &String,
{
    let mut max: Option<Decimal> = None;
    for bid in bids {
        let value = bn(field(bid))?;
        max = Some(match max {
            Some(current) if current >= value => current,
            _ => value,
        });
    }
    // `bids` is non-empty at every call site, so the fold always produced one.
    let max = max.unwrap_or_else(Decimal::zero);
    bn(max.to_fixed(7).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(address: &str, price: &str, reputation: &str, latency: &str, success: &str) -> AgentBid {
        AgentBid {
            agent_address: address.to_string(),
            price: price.to_string(),
            reputation: reputation.to_string(),
            estimated_latency_seconds: latency.to_string(),
            success_rate: success.to_string(),
        }
    }

    #[test]
    fn weights_must_sum_to_exactly_one() {
        let weights = BidWeights {
            price: "0.25".into(),
            reputation: "0.25".into(),
            latency: "0.25".into(),
            reliability: "0.20".into(),
        };
        let error = score_bid(&bid("G", "1", "50", "10", "0.5"), "10", "10", &weights).unwrap_err();
        assert!(matches!(error, FixedPointError::InvalidWeights { .. }));
        assert!(error.to_string().contains("weights must sum to 1.0"));
    }

    #[test]
    fn a_zero_normaliser_awards_full_marks_instead_of_dividing_by_zero() {
        let scored = score_bid(
            &bid("G", "0", "100", "0", "1"),
            "0",
            "0",
            &BidWeights::default(),
        )
        .unwrap();
        assert_eq!(scored.breakdown.price_score, "100.0000");
        assert_eq!(scored.breakdown.latency_score, "100.0000");
        assert_eq!(scored.score, "100.0000");
    }

    #[test]
    fn sub_scores_are_clamped_to_the_zero_hundred_range() {
        let scored = score_bid(
            &bid("G", "1", "999", "1", "5"),
            "10",
            "10",
            &BidWeights::default(),
        )
        .unwrap();
        assert_eq!(scored.breakdown.reputation_score, "100.0000");
        assert_eq!(scored.breakdown.reliability_score, "100.0000");
    }

    #[test]
    fn ranking_is_independent_of_input_order() {
        let bids = vec![
            bid("GAAA", "1", "50", "10", "0.9"),
            bid("GBBB", "5", "90", "2", "0.99"),
            bid("GCCC", "3", "70", "6", "0.95"),
        ];
        let forward = rank_bids(&bids, &BidWeights::default()).unwrap();
        let mut reversed = bids.clone();
        reversed.reverse();
        let backward = rank_bids(&reversed, &BidWeights::default()).unwrap();
        assert_eq!(forward, backward);
    }

    #[test]
    fn ties_break_lexicographically_on_address() {
        // Identical bids apart from the address: every score is equal, so the
        // ordering has to come from somewhere reproducible.
        let bids = vec![
            bid("GZZZ", "1", "50", "10", "0.9"),
            bid("GAAA", "1", "50", "10", "0.9"),
            bid("GMMM", "1", "50", "10", "0.9"),
        ];
        let ranked = rank_bids(&bids, &BidWeights::default()).unwrap();
        let addresses: Vec<_> = ranked.iter().map(|r| r.agent_address.as_str()).collect();
        assert_eq!(addresses, ["GAAA", "GMMM", "GZZZ"]);
    }

    #[test]
    fn an_empty_pool_ranks_to_nothing_rather_than_erroring() {
        assert!(rank_bids(&[], &BidWeights::default()).unwrap().is_empty());
        assert!(select_best_bid(&[], &BidWeights::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn best_bid_agrees_with_the_head_of_the_ranking() {
        let bids = vec![
            bid("GAAA", "9", "10", "90", "0.1"),
            bid("GBBB", "1", "99", "1", "0.99"),
        ];
        let weights = BidWeights::default();
        let best = select_best_bid(&bids, &weights).unwrap().unwrap();
        assert_eq!(best, rank_bids(&bids, &weights).unwrap()[0]);
        assert_eq!(best.agent_address, "GBBB");
    }

    #[test]
    fn spend_limit_boundary_is_inclusive() {
        assert!(is_within_spend_limit("0", "100", "100").unwrap());
        assert!(!is_within_spend_limit("0", "100", "101").unwrap());
        assert_eq!(remaining_budget("100", "100").unwrap(), "0");
        assert_eq!(remaining_budget("200", "100").unwrap(), "0");
    }
}
