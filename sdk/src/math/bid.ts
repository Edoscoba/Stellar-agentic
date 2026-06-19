/**
 * Deterministic agent bidding algorithm.
 *
 * ## Background
 *
 * When multiple agents compete for an escrow job the requester (or the
 * on-chain arbiter) needs a reproducible **bid score** so the same input
 * always produces the same winner, regardless of which CPU architecture
 * computed it.
 *
 * Historically this was done with native JS floats, which diverge between
 * x86 (SSE2) and ARM (M1/M2 NEON) due to intermediate-precision differences
 * in the FPU pipelines.  This file replaces every float operation with calls
 * to the `fixed-point` module, which uses `bignumber.js` pure-JS arithmetic.
 *
 * ## Scoring formula
 *
 * Each bid is scored 0–100 using a weighted combination of:
 *
 *   score = (priceWeight   × priceScore)
 *         + (repWeight     × repScore)
 *         + (latencyWeight × latencyScore)
 *         + (reliabWeight  × reliabScore)
 *
 * Where individual sub-scores are normalised to [0, 100]:
 *   - priceScore   = 100 × (1 − bid / maxBid)   — lower price is better
 *   - repScore     = reputation (0–100 input)
 *   - latencyScore = 100 × (1 − latency / maxLatency) — lower latency better
 *   - reliabScore  = successRate × 100           — 0.0–1.0 input
 *
 * All weights must sum to 1.0 (expressed as decimal strings).
 *
 * @module bid
 */

import BigNumber from 'bignumber.js';
import { bn, add, sub, mul, div, clamp } from './fixed-point.js';

// ─── Types ───────────────────────────────────────────────────────────────────

/** A single agent's bid for an escrow job */
export interface AgentBid {
  /** Unique agent address */
  agentAddress: string;
  /**
   * Price the agent is willing to accept for the job.
   * Must be a decimal string (e.g. "0.05") — never a JS number.
   */
  price: string;
  /**
   * Agent's reputation score 0–100 (integer or decimal string).
   * Sourced from on-chain historical data.
   */
  reputation: string;
  /**
   * Expected task completion time in seconds (decimal string).
   * Lower is better.
   */
  estimatedLatencySeconds: string;
  /**
   * Lifetime success rate as a decimal fraction 0–1 (e.g. "0.97").
   * Computed as successfulJobs / totalJobs.
   */
  successRate: string;
}

/** Weights controlling the relative importance of each dimension */
export interface BidWeights {
  /** Weight for price competitiveness (0–1, decimal string) */
  price: string;
  /** Weight for reputation (0–1, decimal string) */
  reputation: string;
  /** Weight for latency (0–1, decimal string) */
  latency: string;
  /** Weight for reliability / success rate (0–1, decimal string) */
  reliability: string;
}

/** Default weights — equal split across all four dimensions */
export const DEFAULT_BID_WEIGHTS: BidWeights = {
  price: '0.25',
  reputation: '0.25',
  latency: '0.25',
  reliability: '0.25',
};

/** A scored bid ready for ranking */
export interface ScoredBid {
  agentAddress: string;
  /** Final composite score 0–100, rounded down to 4 decimal places */
  score: string;
  /** Individual sub-scores for transparency */
  breakdown: {
    priceScore: string;
    reputationScore: string;
    latencyScore: string;
    reliabilityScore: string;
  };
}

// ─── Core scoring function ───────────────────────────────────────────────────

/**
 * Compute a deterministic composite score for a single bid.
 *
 * All intermediate values are `BigNumber` — no native JS floats are used.
 * The result is identical on x86, ARM, WASM, and any other platform where
 * `bignumber.js` runs.
 *
 * @param bid       - The agent's bid to score
 * @param maxBid    - The highest price among all competing bids (normaliser)
 * @param maxLatency - The highest latency among all competing bids (normaliser)
 * @param weights   - Dimension weights (must sum to 1)
 */
export function scoreBid(
  bid: AgentBid,
  maxBid: string,
  maxLatency: string,
  weights: BidWeights = DEFAULT_BID_WEIGHTS,
): ScoredBid {
  // ── Validate weights sum to 1.0 ──────────────────────────────────────────
  const weightSum = add(add(add(weights.price, weights.reputation), weights.latency), weights.reliability);
  if (!weightSum.isEqualTo(bn('1'))) {
    throw new RangeError(
      `BidScorer: weights must sum to 1.0, got ${weightSum.toFixed(4)}`,
    );
  }

  // ── Price score: 100 × (1 − price/maxBid) ────────────────────────────────
  // Lower price → higher score.  If all bids are equal maxBid === price → 0.
  let priceScore: BigNumber;
  if (bn(maxBid).isZero()) {
    priceScore = new BigNumber(100);
  } else {
    const priceRatio = div(bid.price, maxBid);        // price / maxBid ∈ [0,1]
    priceScore = mul(sub('1', priceRatio), '100');     // 100 × (1 − ratio)
    priceScore = clamp(priceScore, '0', '100');
  }

  // ── Reputation score: already 0–100 ──────────────────────────────────────
  const reputationScore = clamp(bn(bid.reputation), '0', '100');

  // ── Latency score: 100 × (1 − latency/maxLatency) ────────────────────────
  // Lower latency → higher score.
  let latencyScore: BigNumber;
  if (bn(maxLatency).isZero()) {
    latencyScore = new BigNumber(100);
  } else {
    const latencyRatio = div(bid.estimatedLatencySeconds, maxLatency);
    latencyScore = mul(sub('1', latencyRatio), '100');
    latencyScore = clamp(latencyScore, '0', '100');
  }

  // ── Reliability score: successRate × 100 ─────────────────────────────────
  const reliabilityScore = clamp(mul(bid.successRate, '100'), '0', '100');

  // ── Composite weighted score ──────────────────────────────────────────────
  const compositeScore = add(
    add(
      add(
        mul(priceScore, weights.price),
        mul(reputationScore, weights.reputation),
      ),
      mul(latencyScore, weights.latency),
    ),
    mul(reliabilityScore, weights.reliability),
  );

  return {
    agentAddress: bid.agentAddress,
    score: compositeScore.decimalPlaces(4, BigNumber.ROUND_DOWN).toFixed(4),
    breakdown: {
      priceScore: priceScore.decimalPlaces(4, BigNumber.ROUND_DOWN).toFixed(4),
      reputationScore: reputationScore.decimalPlaces(4, BigNumber.ROUND_DOWN).toFixed(4),
      latencyScore: latencyScore.decimalPlaces(4, BigNumber.ROUND_DOWN).toFixed(4),
      reliabilityScore: reliabilityScore.decimalPlaces(4, BigNumber.ROUND_DOWN).toFixed(4),
    },
  };
}

/**
 * Rank a set of competing agent bids deterministically.
 *
 * Steps:
 * 1. Compute `maxBid` and `maxLatency` over the full bid set (normalisation).
 * 2. Score every bid using `scoreBid`.
 * 3. Sort descending by score (ties broken by `agentAddress` lexicographically
 *    so the ordering is always reproducible).
 *
 * @returns Bids sorted best-first with their scores attached.
 */
export function rankBids(
  bids: AgentBid[],
  weights: BidWeights = DEFAULT_BID_WEIGHTS,
): ScoredBid[] {
  if (bids.length === 0) return [];

  // Normalisation pass — find max price and max latency across the whole set
  const maxBid = bids
    .map((b) => bn(b.price))
    .reduce((m, v) => (v.isGreaterThan(m) ? v : m))
    .toFixed(7);

  const maxLatency = bids
    .map((b) => bn(b.estimatedLatencySeconds))
    .reduce((m, v) => (v.isGreaterThan(m) ? v : m))
    .toFixed(7);

  // Score every bid
  const scored: ScoredBid[] = bids.map((bid) =>
    scoreBid(bid, maxBid, maxLatency, weights),
  );

  // Sort: highest score first; stable tie-break on address
  scored.sort((a, b) => {
    const cmp = bn(b.score).comparedTo(bn(a.score));
    if (cmp !== 0) return cmp;
    return a.agentAddress < b.agentAddress ? -1 : 1;
  });

  return scored;
}

/**
 * Select the single best bid from a pool.
 * Returns `null` when the pool is empty.
 */
export function selectBestBid(
  bids: AgentBid[],
  weights: BidWeights = DEFAULT_BID_WEIGHTS,
): ScoredBid | null {
  const ranked = rankBids(bids, weights);
  return ranked.length > 0 ? ranked[0]! : null;
}

// ─── Spend-limit helpers (reused from payment_channel contract logic) ─────────

/**
 * Determine whether a proposed payment is within an agent's spend limit.
 *
 * Replicates the on-chain `PaymentChannel.pay` guard in TypeScript so the SDK
 * can pre-validate without a network round-trip.  Uses the same integer-safe
 * arithmetic as the contract (amounts in stroops).
 *
 * @param spentThisPeriod  - Already spent in current period (stroop string)
 * @param limitPerPeriod   - Configured period limit (stroop string)
 * @param proposedAmount   - Amount about to be spent (stroop string)
 */
export function isWithinSpendLimit(
  spentThisPeriod: string,
  limitPerPeriod: string,
  proposedAmount: string,
): boolean {
  const newTotal = add(spentThisPeriod, proposedAmount);
  return newTotal.isLessThanOrEqualTo(bn(limitPerPeriod));
}

/**
 * Compute remaining budget in a period.
 *
 * @returns Remaining as a stroop decimal string (never negative).
 */
export function remainingBudget(
  spentThisPeriod: string,
  limitPerPeriod: string,
): string {
  const remaining = sub(limitPerPeriod, spentThisPeriod);
  return BigNumber.maximum(remaining, new BigNumber(0)).toFixed(0);
}

// ─── Re-export ────────────────────────────────────────────────────────────────
export { fmt, pct, add, sub, mul, div, toStroops, fromStroops } from './fixed-point.js';
