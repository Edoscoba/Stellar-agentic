"""Unit tests for :mod:`stellaragent.bid`.

Value parity with TypeScript is covered exhaustively by ``test_determinism.py``.
This file covers the Python-side API: dataclass behaviour, defaults, the
exception contract, and the structural properties of ranking.
"""

from __future__ import annotations

import dataclasses

import pytest

from stellaragent.bid import (
    DEFAULT_BID_WEIGHTS,
    AgentBid,
    BidWeights,
    ScoredBid,
    is_within_spend_limit,
    rank_bids,
    remaining_budget,
    score_bid,
    select_best_bid,
)
from stellaragent.fixed_point import FixedPointError


def make_bid(**overrides: str) -> AgentBid:
    base = {
        "agent_address": "GAAA",
        "price": "1",
        "reputation": "50",
        "estimated_latency_seconds": "10",
        "success_rate": "0.5",
    }
    base.update(overrides)
    return AgentBid(**base)  # type: ignore[arg-type]


# ─── Dataclass surface ───────────────────────────────────────────────────────


class TestTypes:
    def test_default_weights_are_an_equal_split(self) -> None:
        assert DEFAULT_BID_WEIGHTS == BidWeights("0.25", "0.25", "0.25", "0.25")

    def test_bid_weights_defaults_match_the_constant(self) -> None:
        assert BidWeights() == DEFAULT_BID_WEIGHTS

    def test_bids_are_frozen(self) -> None:
        # Immutability keeps a scoring pass from mutating its inputs.
        with pytest.raises(dataclasses.FrozenInstanceError):
            make_bid().price = "2"  # type: ignore[misc]

    def test_weights_are_frozen(self) -> None:
        with pytest.raises(dataclasses.FrozenInstanceError):
            DEFAULT_BID_WEIGHTS.price = "0.5"  # type: ignore[misc]

    def test_scored_bids_compare_by_value(self) -> None:
        a = score_bid(make_bid(), "10", "10")
        b = score_bid(make_bid(), "10", "10")
        assert a == b
        assert isinstance(a, ScoredBid)


# ─── Weight validation ───────────────────────────────────────────────────────


class TestWeightValidation:
    def test_accepts_the_defaults(self) -> None:
        assert score_bid(make_bid(), "10", "10").score

    @pytest.mark.parametrize(
        ("weights", "reported"),
        [
            (BidWeights("0.25", "0.25", "0.25", "0.20"), "0.9500"),
            (BidWeights("0.5", "0.25", "0.25", "0.25"), "1.2500"),
            (BidWeights("0", "0", "0", "0"), "0.0000"),
        ],
    )
    def test_rejects_sums_other_than_one(self, weights: BidWeights, reported: str) -> None:
        with pytest.raises(FixedPointError, match=f"weights must sum to 1.0, got {reported}"):
            score_bid(make_bid(), "10", "10", weights)

    def test_rejects_a_sum_off_by_one_unit_in_the_eighteenth_decimal(self) -> None:
        # Exact equality, not a tolerance: an epsilon check would be sensitive
        # to accumulated representation error and defeat the whole point.
        weights = BidWeights("0.250000000000000001", "0.25", "0.25", "0.25")
        with pytest.raises(FixedPointError):
            score_bid(make_bid(), "10", "10", weights)

    def test_rejects_a_non_numeric_weight(self) -> None:
        with pytest.raises(FixedPointError):
            score_bid(make_bid(), "10", "10", BidWeights("oops", "0.25", "0.25", "0.25"))

    def test_accepts_all_weight_on_one_dimension(self) -> None:
        scored = score_bid(
            make_bid(price="0", reputation="0", success_rate="0"),
            "10",
            "10",
            BidWeights("1", "0", "0", "0"),
        )
        assert scored.score == "100.0000"


# ─── Scoring shape ───────────────────────────────────────────────────────────


class TestScoreShape:
    def test_every_output_has_exactly_four_decimal_places(self) -> None:
        scored = score_bid(make_bid(), "10", "10")
        values = [
            scored.score,
            scored.breakdown.price_score,
            scored.breakdown.reputation_score,
            scored.breakdown.latency_score,
            scored.breakdown.reliability_score,
        ]
        for value in values:
            assert value.count(".") == 1
            assert len(value.split(".")[1]) == 4

    def test_carries_the_agent_address_through(self) -> None:
        assert score_bid(make_bid(agent_address="GWORKER"), "10", "10").agent_address == "GWORKER"

    def test_rejects_a_malformed_bid_field(self) -> None:
        with pytest.raises(FixedPointError):
            score_bid(make_bid(price="free"), "10", "10")

    def test_zero_normalisers_do_not_raise(self) -> None:
        # The short-circuit exists so a degenerate pool cannot divide by zero.
        assert score_bid(make_bid(), "0", "0").breakdown.price_score == "100.0000"

    def test_is_byte_identical_across_repeated_evaluation(self) -> None:
        bid = make_bid(
            price="0.0333333",
            reputation="77.7777777",
            estimated_latency_seconds="1.1111111",
            success_rate="0.9999999",
        )
        first = score_bid(bid, "0.7777777", "9.9999999")
        assert all(score_bid(bid, "0.7777777", "9.9999999") == first for _ in range(50))


# ─── Ranking ─────────────────────────────────────────────────────────────────


class TestRanking:
    def test_empty_pool_ranks_to_nothing(self) -> None:
        assert rank_bids([]) == []

    def test_empty_pool_selects_none(self) -> None:
        assert select_best_bid([]) is None

    def test_ties_break_lexicographically_by_address(self) -> None:
        identical = {
            "price": "1",
            "reputation": "50",
            "estimated_latency_seconds": "10",
            "success_rate": "0.5",
        }
        pool = [make_bid(agent_address=a, **identical) for a in ("GDELTA", "GALPHA", "GCHARLIE", "GBRAVO")]
        assert [r.agent_address for r in rank_bids(pool)] == ["GALPHA", "GBRAVO", "GCHARLIE", "GDELTA"]

    def test_scores_are_monotonically_non_increasing(self) -> None:
        from decimal import Decimal

        pool = [
            make_bid(agent_address=f"G{i:04d}", price=str(i % 13), reputation=str(i % 101))
            for i in range(50)
        ]
        scores = [Decimal(r.score) for r in rank_bids(pool)]
        assert all(a >= b for a, b in zip(scores, scores[1:], strict=False))

    def test_does_not_mutate_the_input(self) -> None:
        pool = [make_bid(agent_address="GZ", price="10"), make_bid(agent_address="GA", price="1")]
        snapshot = list(pool)
        rank_bids(pool)
        assert pool == snapshot

    def test_accepts_any_sequence(self) -> None:
        pool = (make_bid(agent_address="GA"), make_bid(agent_address="GB"))
        assert len(rank_bids(pool)) == 2

    def test_select_best_agrees_with_ranking(self) -> None:
        pool = [
            make_bid(agent_address="GA", price="3", reputation="80"),
            make_bid(agent_address="GB", price="2", reputation="85"),
            make_bid(agent_address="GC", price="4", reputation="95"),
        ]
        assert select_best_bid(pool) == rank_bids(pool)[0]

    def test_propagates_weight_validation_errors(self) -> None:
        with pytest.raises(FixedPointError):
            rank_bids([make_bid()], BidWeights("1", "1", "1", "1"))


# ─── Spend limits ────────────────────────────────────────────────────────────


class TestSpendLimits:
    def test_is_inclusive_at_the_limit(self) -> None:
        assert is_within_spend_limit("9500", "10000", "500") is True
        assert is_within_spend_limit("9500", "10000", "501") is False

    def test_returns_real_bools(self) -> None:
        assert isinstance(is_within_spend_limit("1", "2", "1"), bool)

    def test_exact_at_i128_magnitudes(self) -> None:
        limit = "170141183460469231731687303715884105727"
        spent = "170141183460469231731687303715884105726"
        assert is_within_spend_limit(spent, limit, "1") is True
        assert is_within_spend_limit(spent, limit, "2") is False

    def test_remaining_never_goes_negative(self) -> None:
        assert remaining_budget("15000", "10000") == "0"

    def test_remaining_is_an_integer_string(self) -> None:
        assert "." not in remaining_budget("0", "10000")

    def test_remaining_truncates_a_fractional_headroom(self) -> None:
        # Truncating downward is the safe direction for a spend guard.
        assert remaining_budget("0.1", "1") == "0"
        assert remaining_budget("0.5", "2") == "1"

    def test_remaining_agrees_with_the_guard_at_the_boundary(self) -> None:
        spent, limit = "7500", "10000"
        remaining = remaining_budget(spent, limit)
        assert is_within_spend_limit(spent, limit, remaining) is True
        assert is_within_spend_limit(spent, limit, str(int(remaining) + 1)) is False

    def test_validates_inputs(self) -> None:
        with pytest.raises(FixedPointError):
            is_within_spend_limit("abc", "1", "1")
        with pytest.raises(FixedPointError):
            remaining_budget("1", "abc")
