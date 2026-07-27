"""Cross-language determinism suite.

This is the test that matters for the whole Python port. ``fixtures/
determinism.json`` is generated from the **TypeScript** implementation
(``pnpm fixtures:generate``); the same file is consumed by
``packages/core/src/math/__tests__/determinism-fixtures.test.ts``. If both
suites pass, the two implementations produce byte-identical strings for every
case in the file.

The TS module exists to stop x86 and ARM disagreeing about a bid score. A
Python port that quietly rounded differently would reintroduce exactly that
divergence for a mixed TS/Python agent ecosystem — only now the two halves
would disagree on every machine rather than some of them. Hence: string
equality, not numeric closeness. ``pytest.approx`` would defeat the point.
"""

from __future__ import annotations

import json
from decimal import Decimal
from pathlib import Path

import pytest

from stellaragent import bid as bid_mod
from stellaragent import fixed_point as fp
from stellaragent.bid import AgentBid, BidWeights
from stellaragent.fixed_point import FixedPointError, _format, _quantize

# ─── Fixture loading ─────────────────────────────────────────────────────────

FIXTURES_PATH = Path(__file__).resolve().parents[2] / "fixtures" / "determinism.json"


def _load() -> dict:
    if not FIXTURES_PATH.exists():
        raise AssertionError(
            f"Shared fixtures missing at {FIXTURES_PATH}.\n"
            "Generate them from the TypeScript implementation: pnpm fixtures:generate"
        )
    return json.loads(FIXTURES_PATH.read_text())


FIXTURES = _load()


def test_fixture_file_is_populated() -> None:
    """A silently empty fixture file would make every test below vacuous."""
    assert FIXTURES["version"] == 1
    assert len(FIXTURES["fixedPoint"]) > 300
    assert len(FIXTURES["bid"]["scoreBid"]) > 100
    assert len(FIXTURES["bid"]["rankBids"]) > 10
    assert len(FIXTURES["bid"]["spendLimit"]) > 10


# ─── Dispatch ────────────────────────────────────────────────────────────────

#: TypeScript name → Python callable. Explicit rather than derived from the
#: name, so a renamed or missing function fails loudly here.
FIXED_POINT_FNS = {
    "bn": fp.bn,
    "add": fp.add,
    "sub": fp.sub,
    "mul": fp.mul,
    "div": fp.div,
    "pct": fp.pct,
    "clamp": fp.clamp,
    "sumStrings": fp.sum_strings,
    "toStroops": fp.to_stroops,
    "fromStroops": lambda s, dp: fp.from_stroops(int(s), dp),
    "fmt": fp.fmt,
    "toStr": fp.to_str,
    "gt": fp.gt,
    "gte": fp.gte,
    "lt": fp.lt,
    "lte": fp.lte,
    "eq": fp.eq,
    "isZero": fp.is_zero,
    "isPositive": fp.is_positive,
}


def _canonical(result: object, kind: str) -> str:
    """Serialise a Python result the way the generator serialised the TS one."""
    if kind == "decimal":
        assert isinstance(result, Decimal), f"expected Decimal, got {type(result)}"
        return _format(_quantize(result, 18))
    if kind == "int":
        assert isinstance(result, int) and not isinstance(result, bool)
        return str(result)
    if kind == "bool":
        assert isinstance(result, bool)
        # TS `String(true)` is 'true'; Python `str(True)` is 'True'.
        return "true" if result else "false"
    return str(result)


def _weights(name: str) -> BidWeights:
    raw = FIXTURES["weightSets"][name]
    return BidWeights(
        price=raw["price"],
        reputation=raw["reputation"],
        latency=raw["latency"],
        reliability=raw["reliability"],
    )


def _bid(raw: dict) -> AgentBid:
    return AgentBid(
        agent_address=raw["agentAddress"],
        price=raw["price"],
        reputation=raw["reputation"],
        estimated_latency_seconds=raw["estimatedLatencySeconds"],
        success_rate=raw["successRate"],
    )


# ─── fixed-point parity ──────────────────────────────────────────────────────

_FP_VALUE_CASES = [c for c in FIXTURES["fixedPoint"] if not c.get("throws")]
_FP_THROW_CASES = [c for c in FIXTURES["fixedPoint"] if c.get("throws")]


@pytest.mark.parametrize("case", _FP_VALUE_CASES, ids=lambda c: c["id"])
def test_fixed_point_matches_typescript(case: dict) -> None:
    fn = FIXED_POINT_FNS[case["fn"]]
    args = [case["args"]] if case["fn"] == "sumStrings" else case["args"]

    actual = _canonical(fn(*args), case["kind"])

    assert actual == case["expect"], (
        f"{case['id']}\n"
        f"  TypeScript: {case['expect']}\n"
        f"  Python:     {actual}\n"
        "  The two implementations have diverged — this breaks the determinism "
        "guarantee for any mixed TS/Python agent ecosystem."
    )


@pytest.mark.parametrize("case", _FP_THROW_CASES, ids=lambda c: c["id"])
def test_fixed_point_rejects_what_typescript_rejects(case: dict) -> None:
    """Both implementations must fail on the same inputs, not just agree on values."""
    fn = FIXED_POINT_FNS[case["fn"]]
    args = [case["args"]] if case["fn"] == "sumStrings" else case["args"]
    with pytest.raises(FixedPointError):
        fn(*args)


# ─── scoreBid parity ─────────────────────────────────────────────────────────


@pytest.mark.parametrize("case", FIXTURES["bid"]["scoreBid"], ids=lambda c: c["id"])
def test_score_bid_matches_typescript(case: dict) -> None:
    scored = bid_mod.score_bid(
        _bid(case["bid"]),
        case["maxBid"],
        case["maxLatency"],
        _weights(case["weights"]),
    )
    expected = case["expect"]

    assert scored.score == expected["score"], (
        f"{case['id']} composite score\n"
        f"  TypeScript: {expected['score']}\n"
        f"  Python:     {scored.score}"
    )
    assert scored.breakdown.price_score == expected["breakdown"]["priceScore"]
    assert scored.breakdown.reputation_score == expected["breakdown"]["reputationScore"]
    assert scored.breakdown.latency_score == expected["breakdown"]["latencyScore"]
    assert scored.breakdown.reliability_score == expected["breakdown"]["reliabilityScore"]


# ─── rankBids parity ─────────────────────────────────────────────────────────


@pytest.mark.parametrize("case", FIXTURES["bid"]["rankBids"], ids=lambda c: c["id"])
def test_rank_bids_matches_typescript(case: dict) -> None:
    ranked = bid_mod.rank_bids([_bid(b) for b in case["bids"]], _weights(case["weights"]))

    actual = [{"agentAddress": r.agent_address, "score": r.score} for r in ranked]
    assert actual == case["expect"], (
        f"{case['id']}\n"
        f"  TypeScript: {case['expect']}\n"
        f"  Python:     {actual}\n"
        "  Ranking order or scores diverged — two agents scoring the same pool "
        "would pick different winners."
    )


@pytest.mark.parametrize("case", FIXTURES["bid"]["rankBids"], ids=lambda c: c["id"])
def test_ranking_is_order_independent(case: dict) -> None:
    """Reversing the pool must not change the outcome, in either language."""
    bids = [_bid(b) for b in case["bids"]]
    weights = _weights(case["weights"])
    forward = bid_mod.rank_bids(bids, weights)
    reverse = bid_mod.rank_bids(list(reversed(bids)), weights)
    assert [(r.agent_address, r.score) for r in forward] == [
        (r.agent_address, r.score) for r in reverse
    ]


@pytest.mark.parametrize("case", FIXTURES["bid"]["rankBids"], ids=lambda c: c["id"])
def test_select_best_bid_agrees_with_ranking(case: dict) -> None:
    bids = [_bid(b) for b in case["bids"]]
    weights = _weights(case["weights"])
    best = bid_mod.select_best_bid(bids, weights)

    if not case["expect"]:
        assert best is None
    else:
        assert best is not None
        assert best.agent_address == case["expect"][0]["agentAddress"]
        assert best.score == case["expect"][0]["score"]


# ─── Spend limits parity ─────────────────────────────────────────────────────


@pytest.mark.parametrize("case", FIXTURES["bid"]["spendLimit"], ids=lambda c: c["id"])
def test_spend_limit_matches_typescript(case: dict) -> None:
    within = bid_mod.is_within_spend_limit(case["spent"], case["limit"], case["amount"])
    remaining = bid_mod.remaining_budget(case["spent"], case["limit"])

    assert within is case["expect"]["withinLimit"], (
        f"{case['id']} isWithinSpendLimit\n"
        f"  TypeScript: {case['expect']['withinLimit']}\n"
        f"  Python:     {within}\n"
        "  A disagreement here means one implementation would allow a payment "
        "the other blocks."
    )
    assert remaining == case["expect"]["remaining"], (
        f"{case['id']} remainingBudget\n"
        f"  TypeScript: {case['expect']['remaining']}\n"
        f"  Python:     {remaining}"
    )


# ─── Invalid weights parity ──────────────────────────────────────────────────


@pytest.mark.parametrize(
    "case", FIXTURES["bid"]["invalidWeights"], ids=lambda c: c["id"]
)
def test_invalid_weights_rejected(case: dict) -> None:
    """Weight validation must reject exactly what TypeScript rejects."""
    weights = BidWeights(
        price=case["weights"]["price"],
        reputation=case["weights"]["reputation"],
        latency=case["weights"]["latency"],
        reliability=case["weights"]["reliability"],
    )
    sample = AgentBid(
        agent_address="GTEST",
        price="1",
        reputation="50",
        estimated_latency_seconds="10",
        success_rate="0.5",
    )
    with pytest.raises(FixedPointError, match="weights must sum to 1.0"):
        bid_mod.score_bid(sample, "10", "10", weights)
