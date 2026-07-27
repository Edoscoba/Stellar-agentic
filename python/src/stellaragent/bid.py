"""Deterministic agent bidding algorithm.

Python port of ``packages/core/src/math/bid.ts``. Every function here must
produce byte-identical strings to its TypeScript counterpart — see
``fixtures/determinism.json`` and the shared regression suite.

Scoring formula
---------------
Each bid is scored 0-100 as a weighted combination::

    score = (price_weight   * price_score)
          + (rep_weight     * reputation_score)
          + (latency_weight * latency_score)
          + (reliab_weight  * reliability_score)

with sub-scores normalised to [0, 100]:

- ``price_score``       = 100 * (1 - price / max_bid)      — lower is better
- ``reputation_score``  = reputation (already 0-100)
- ``latency_score``     = 100 * (1 - latency / max_latency) — lower is better
- ``reliability_score`` = success_rate * 100                — 0.0-1.0 input

Weights must sum to exactly 1.0. The check is exact equality rather than a
tolerance: an epsilon comparison would be sensitive to accumulated
representation error and defeat the whole point of the module.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass, field
from decimal import Decimal

from .fixed_point import (
    FixedPointError,
    Numeric,
    _format,
    _quantize,
    add,
    bn,
    clamp,
    div,
    mul,
    sub,
)

__all__ = [
    "AgentBid",
    "BidWeights",
    "ScoredBid",
    "ScoreBreakdown",
    "DEFAULT_BID_WEIGHTS",
    "score_bid",
    "rank_bids",
    "select_best_bid",
    "is_within_spend_limit",
    "remaining_budget",
]


# ─── Types ───────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class AgentBid:
    """A single agent's bid for an escrow job.

    All numeric fields are decimal strings, never floats — the same rule the
    TS interface documents.
    """

    #: Unique agent address.
    agent_address: str
    #: Price the agent will accept, e.g. ``"0.05"``.
    price: str
    #: Reputation score 0-100, from on-chain history.
    reputation: str
    #: Expected completion time in seconds. Lower is better.
    estimated_latency_seconds: str
    #: Lifetime success rate as a fraction 0-1, e.g. ``"0.97"``.
    success_rate: str


@dataclass(frozen=True)
class BidWeights:
    """Relative importance of each scoring dimension. Must sum to exactly 1."""

    price: str = "0.25"
    reputation: str = "0.25"
    latency: str = "0.25"
    reliability: str = "0.25"


@dataclass(frozen=True)
class ScoreBreakdown:
    """Individual sub-scores, for transparency. Each is a 4-decimal string."""

    price_score: str
    reputation_score: str
    latency_score: str
    reliability_score: str


@dataclass(frozen=True)
class ScoredBid:
    """A scored bid ready for ranking."""

    agent_address: str
    #: Final composite score 0-100, truncated to 4 decimal places.
    score: str
    breakdown: ScoreBreakdown = field(compare=True)


#: Equal split across all four dimensions.
DEFAULT_BID_WEIGHTS = BidWeights()


# ─── Core scoring ────────────────────────────────────────────────────────────


def score_bid(
    bid: AgentBid,
    max_bid: Numeric,
    max_latency: Numeric,
    weights: BidWeights = DEFAULT_BID_WEIGHTS,
) -> ScoredBid:
    """Compute a deterministic composite score for one bid.

    :param bid: the bid to score
    :param max_bid: highest price among competing bids (the normaliser)
    :param max_latency: highest latency among competing bids
    :param weights: dimension weights, which must sum to 1
    :raises FixedPointError: when the weights do not sum to exactly 1
    """
    weight_sum = add(add(add(weights.price, weights.reputation), weights.latency), weights.reliability)
    if weight_sum != bn("1"):
        raise FixedPointError(
            f"BidScorer: weights must sum to 1.0, got {_format(_quantize(weight_sum, 4))}"
        )

    # Price: 100 * (1 - price/max_bid). A zero normaliser means every bid is
    # free, so short-circuit to full marks rather than dividing by zero.
    if bn(max_bid).is_zero():
        price_score = Decimal(100)
    else:
        price_ratio = div(bid.price, max_bid)
        price_score = clamp(mul(sub("1", price_ratio), "100"), "0", "100")

    reputation_score = clamp(bn(bid.reputation), "0", "100")

    # Latency: 100 * (1 - latency/max_latency). Same zero-normaliser case.
    if bn(max_latency).is_zero():
        latency_score = Decimal(100)
    else:
        latency_ratio = div(bid.estimated_latency_seconds, max_latency)
        latency_score = clamp(mul(sub("1", latency_ratio), "100"), "0", "100")

    reliability_score = clamp(mul(bid.success_rate, "100"), "0", "100")

    composite = add(
        add(
            add(
                mul(price_score, weights.price),
                mul(reputation_score, weights.reputation),
            ),
            mul(latency_score, weights.latency),
        ),
        mul(reliability_score, weights.reliability),
    )

    return ScoredBid(
        agent_address=bid.agent_address,
        score=_fixed4(composite),
        breakdown=ScoreBreakdown(
            price_score=_fixed4(price_score),
            reputation_score=_fixed4(reputation_score),
            latency_score=_fixed4(latency_score),
            reliability_score=_fixed4(reliability_score),
        ),
    )


def rank_bids(
    bids: Sequence[AgentBid],
    weights: BidWeights = DEFAULT_BID_WEIGHTS,
) -> list[ScoredBid]:
    """Rank competing bids deterministically, best first.

    Normalises against the maximum price and latency across the whole set,
    scores every bid, then sorts descending by score with ties broken
    lexicographically on ``agent_address`` so the ordering is reproducible
    regardless of input order.
    """
    if not bids:
        return []

    # The normalisers are truncated to 7 decimal places *before* scoring, as
    # in the TS original — this is observable in the final digit.
    max_bid = _fixed7(max(bn(b.price) for b in bids))
    max_latency = _fixed7(max(bn(b.estimated_latency_seconds) for b in bids))

    scored = [score_bid(bid, max_bid, max_latency, weights) for bid in bids]

    # Sort by descending score, then ascending address. Python's sort is
    # stable and total, so this needs no explicit comparator.
    scored.sort(key=lambda s: (-bn(s.score), s.agent_address))
    return scored


def select_best_bid(
    bids: Sequence[AgentBid],
    weights: BidWeights = DEFAULT_BID_WEIGHTS,
) -> ScoredBid | None:
    """Select the single best bid, or ``None`` when the pool is empty."""
    ranked = rank_bids(bids, weights)
    return ranked[0] if ranked else None


# ─── Spend limits ────────────────────────────────────────────────────────────


def is_within_spend_limit(
    spent_this_period: Numeric,
    limit_per_period: Numeric,
    proposed_amount: Numeric,
) -> bool:
    """Whether a proposed payment stays within the spend limit.

    Replicates the on-chain ``PaymentChannel.pay`` guard so the SDK can
    pre-validate without a network round-trip. Inclusive at the limit.
    """
    return add(spent_this_period, proposed_amount) <= bn(limit_per_period)


def remaining_budget(spent_this_period: Numeric, limit_per_period: Numeric) -> str:
    """Remaining budget as an integer stroop string, never negative."""
    remaining = sub(limit_per_period, spent_this_period)
    return _format(_quantize(max(remaining, Decimal(0)), 0))


# ─── Internals ───────────────────────────────────────────────────────────────


def _fixed4(value: Decimal) -> str:
    """The TS ``.decimalPlaces(4, ROUND_DOWN).toFixed(4)``."""
    return _format(_quantize(value, 4))


def _fixed7(value: Decimal) -> str:
    """The TS ``.toFixed(7)`` applied to the normalisers."""
    return _format(_quantize(value, 7))
