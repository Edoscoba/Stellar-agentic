"""Unit tests for :mod:`stellaragent.fixed_point`.

Cross-language value parity is covered exhaustively by
``test_determinism.py`` against the shared fixtures. This file covers what
those fixtures cannot: Python-specific typing behaviour, the exception
contract, and the properties that are easier to state as a rule than as a
table of cases.
"""

from __future__ import annotations

from decimal import Context, Decimal, getcontext

import pytest

from stellaragent.fixed_point import (
    BPS_SCALE,
    DECIMAL_PLACES,
    STROOP_SCALE,
    FixedPointError,
    add,
    bn,
    clamp,
    div,
    eq,
    fmt,
    from_stroops,
    gt,
    gte,
    is_positive,
    is_zero,
    lt,
    lte,
    mul,
    pct,
    sub,
    sum_strings,
    to_str,
    to_stroops,
)

# ─── Constants ───────────────────────────────────────────────────────────────


def test_stroop_scale_matches_stellar() -> None:
    assert STROOP_SCALE == Decimal(10_000_000)


def test_bps_scale() -> None:
    assert BPS_SCALE == Decimal(10_000)


def test_decimal_places_matches_typescript_config() -> None:
    """Mirrors ``DECIMAL_PLACES: 18`` in fixed-point.ts's BigNumber.config."""
    assert DECIMAL_PLACES == 18


# ─── bn: accepted and rejected types ─────────────────────────────────────────


class TestBnTypes:
    def test_accepts_str(self) -> None:
        assert bn("1.25") == Decimal("1.25")

    def test_accepts_int(self) -> None:
        assert bn(42) == Decimal(42)

    def test_accepts_decimal(self) -> None:
        assert bn(Decimal("3.5")) == Decimal("3.5")

    def test_accepts_int_beyond_float_range(self) -> None:
        # Python ints are arbitrary precision; this must not lose the low bits.
        assert bn(2**63 - 1) == Decimal("9223372036854775807")

    def test_strips_surrounding_whitespace(self) -> None:
        assert bn("  1.5  ") == Decimal("1.5")

    @pytest.mark.parametrize("value", [0.1, 1.0, -3.5, float("inf"), float("nan")])
    def test_rejects_float(self, value: float) -> None:
        """The one intentional divergence from the TS API.

        ``Decimal(0.1)`` is ``0.1000000000000000055511151231257827`` — accepting
        a float would reintroduce exactly the cross-language divergence this
        module exists to prevent.
        """
        with pytest.raises(FixedPointError, match="refusing to convert float"):
            bn(value)

    def test_float_rejection_message_suggests_the_fix(self) -> None:
        with pytest.raises(FixedPointError, match="Pass a string, int, or Decimal"):
            bn(0.1)

    @pytest.mark.parametrize("value", [True, False])
    def test_rejects_bool(self, value: bool) -> None:
        # bool is an int subclass, so this would otherwise silently be 0 or 1.
        with pytest.raises(FixedPointError):
            bn(value)

    @pytest.mark.parametrize("value", ["abc", "", "   ", "1.2.3", "1,5", "--1"])
    def test_rejects_unparseable_strings(self, value: str) -> None:
        with pytest.raises(FixedPointError, match="not a finite decimal"):
            bn(value)

    @pytest.mark.parametrize("value", ["nan", "inf", "-inf", "Infinity", "NaN"])
    def test_rejects_non_finite_strings(self, value: str) -> None:
        with pytest.raises(FixedPointError, match="not a finite decimal"):
            bn(value)

    @pytest.mark.parametrize("value", [None, [], {}, object()])
    def test_rejects_unsupported_types(self, value: object) -> None:
        with pytest.raises(FixedPointError):
            bn(value)  # type: ignore[arg-type]

    def test_names_the_offending_value(self) -> None:
        with pytest.raises(FixedPointError, match='value "abc" is not a finite decimal'):
            bn("abc")

    def test_is_a_value_error(self) -> None:
        """Ordinary ``except ValueError`` handling must still work."""
        assert issubclass(FixedPointError, ValueError)
        with pytest.raises(ValueError):
            bn("abc")


class TestBnNegativeZero:
    """``BigNumber('-0')`` normalises to plain zero at parse time."""

    @pytest.mark.parametrize("value", ["-0", "-0.0", "-0.0000000"])
    def test_normalises_negative_zero(self, value: str) -> None:
        assert not bn(value).is_signed()
        assert fmt(value, 2) == "0.00"

    def test_but_truncation_still_preserves_sign(self) -> None:
        # Distinct from the above: the value is genuinely negative, and
        # ROUND_DOWN truncates toward zero while keeping the sign.
        assert fmt("-0.5", 0) == "-0"
        assert fmt("-0.0000001", 2) == "-0.00"


# ─── Isolation from the ambient decimal context ──────────────────────────────


class TestContextIsolation:
    def test_ignores_a_caller_lowering_the_thread_context_precision(self) -> None:
        """A dedicated Context is used precisely so this cannot bite.

        If the module relied on ``getcontext()``, a caller setting prec=5
        anywhere in the process would silently corrupt every calculation here
        — which is the exact class of non-determinism the module prevents.
        """
        original = getcontext().prec
        try:
            getcontext().prec = 5
            assert (
                to_str(mul("123456789012345678901234567890", "2"), 0)
                == "246913578024691357802469135780"
            )
        finally:
            getcontext().prec = original

    def test_ignores_a_caller_changing_the_thread_rounding_mode(self) -> None:
        original = getcontext().rounding
        try:
            getcontext().rounding = "ROUND_HALF_UP"
            assert fmt("8.2399999", 2) == "8.23"
            assert fmt("8.235", 2) == "8.23"
        finally:
            getcontext().rounding = original

    def test_uses_its_own_context_object(self) -> None:
        from stellaragent.fixed_point import _CTX

        assert isinstance(_CTX, Context)
        assert _CTX.prec >= 100, "precision must stay high enough for exact add/mul"


# ─── Arithmetic properties ───────────────────────────────────────────────────


class TestArithmeticProperties:
    def test_addition_is_exact_where_floats_are_not(self) -> None:
        assert add("0.1", "0.2") == bn("0.3")
        assert 0.1 + 0.2 != 0.3  # the failure being avoided

    def test_multiplication_is_exact_where_floats_are_not(self) -> None:
        assert mul("0.1", "0.2") == bn("0.02")

    def test_addition_is_exact_for_very_large_operands(self) -> None:
        # bignumber.js add/mul are unbounded; a naive prec setting breaks here.
        big = "123456789012345678901234567890123456789012345678901234567890"
        assert to_str(add(big, "1"), 0) == big[:-1] + "1"

    def test_multiplication_is_exact_for_very_large_operands(self) -> None:
        assert to_str(mul("123456789012345678901234567890", "987654321098765432109876543210"), 0) == (
            "121932631137021795226185032733622923332237463801111263526900"
        )

    def test_addition_is_commutative(self) -> None:
        assert add("1.7", "2.3") == add("2.3", "1.7")

    def test_subtraction_is_the_inverse_of_addition(self) -> None:
        assert sub(add("1.234567", "7.654321"), "7.654321") == bn("1.234567")

    def test_operations_validate_both_operands(self) -> None:
        for op in (add, sub, mul, div):
            with pytest.raises(FixedPointError):
                op("1", "abc")
            with pytest.raises(FixedPointError):
                op("abc", "1")


class TestDivision:
    @pytest.mark.parametrize("divisor", ["0", "0.0", "-0", "0.0000000"])
    def test_rejects_every_spelling_of_zero(self, divisor: str) -> None:
        with pytest.raises(FixedPointError, match="division by zero"):
            div("1", divisor)

    def test_allows_a_zero_numerator(self) -> None:
        assert div("0", "5").is_zero()

    def test_truncates_at_eighteen_places(self) -> None:
        assert to_str(div("2", "3"), 18) == "0.666666666666666666"

    def test_quotients_below_1e18_truncate_to_zero(self) -> None:
        # A documented boundary, identical in both implementations.
        assert to_str(div("1", "1000000000000000000"), 18) == "0.000000000000000001"
        assert div("1", "10000000000000000000").is_zero()

    def test_truncates_negative_quotients_toward_zero(self) -> None:
        assert to_str(div("-2", "3"), 18) == "-0.666666666666666666"


# ─── pct / clamp / sum_strings ───────────────────────────────────────────────


class TestPct:
    def test_zero_total_returns_zero_rather_than_raising(self) -> None:
        # Deliberately unlike div(): a zero total is a normal progress-bar
        # state, not a programming error.
        assert pct("1", "0").is_zero()
        assert pct("0", "0").is_zero()

    def test_rejects_negative_decimal_places(self) -> None:
        with pytest.raises(FixedPointError, match="places must be >= 0"):
            pct("1", "2", -1)

    def test_does_not_clamp_above_one_hundred(self) -> None:
        assert to_str(pct("10", "5"), 4) == "200.0000"


class TestClamp:
    @pytest.mark.parametrize(
        ("value", "expected"), [("-3", "0"), ("5", "5"), ("99", "10"), ("0", "0"), ("10", "10")]
    )
    def test_boundaries(self, value: str, expected: str) -> None:
        assert to_str(clamp(value, "0", "10"), 0) == expected

    def test_inverted_range_resolves_to_max(self) -> None:
        # max() then min(), matching the TS operator order.
        assert to_str(clamp("7", "10", "0"), 0) == "0"


class TestSumStrings:
    def test_empty_is_zero(self) -> None:
        assert sum_strings([]).is_zero()

    def test_accepts_a_tuple(self) -> None:
        assert to_str(sum_strings(("1", "2")), 0) == "3"

    def test_is_order_independent(self) -> None:
        values = ["0.1", "0.02", "3", "0.000004"]
        assert sum_strings(values) == sum_strings(list(reversed(values)))

    def test_no_drift_over_many_terms(self) -> None:
        assert sum_strings(["0.1"] * 10) == bn("1")

    def test_rejects_an_invalid_element(self) -> None:
        with pytest.raises(FixedPointError):
            sum_strings(["1", "abc"])


# ─── Stroops ─────────────────────────────────────────────────────────────────


class TestStroops:
    def test_returns_a_python_int_not_a_float(self) -> None:
        result = to_stroops("1.5")
        assert isinstance(result, int)
        assert not isinstance(result, bool)

    def test_truncates_sub_stroop_fractions(self) -> None:
        # Rounding up would let an agent exceed its limit by a stroop.
        assert to_stroops("1.50000019") == 15000001
        assert to_stroops("0.00000009") == 0

    def test_handles_i128_magnitudes(self) -> None:
        assert to_stroops("1000000000000") == 10_000_000_000_000_000_000

    def test_from_stroops_rejects_a_non_int(self) -> None:
        with pytest.raises(FixedPointError, match="must be an int"):
            from_stroops("15000001")  # type: ignore[arg-type]

    def test_from_stroops_rejects_a_bool(self) -> None:
        with pytest.raises(FixedPointError, match="must be an int"):
            from_stroops(True)  # type: ignore[arg-type]

    @pytest.mark.parametrize(
        "amount",
        ["0", "1", "0.0000001", "1.5000001", "123456.7891234", "999999999.9999999", "-42.0000001"],
    )
    def test_round_trip(self, amount: str) -> None:
        assert from_stroops(to_stroops(amount), 7) == to_str(bn(amount), 7)

    def test_exhaustive_round_trip_over_a_small_range(self) -> None:
        for i in range(1000):
            assert to_stroops(from_stroops(i, 7)) == i

    def test_never_emits_scientific_notation(self) -> None:
        assert from_stroops(1, 7) == "0.0000001"
        assert "E" not in from_stroops(10**30, 7).upper()


# ─── Formatting ──────────────────────────────────────────────────────────────


class TestFormatting:
    def test_fmt_truncates_rather_than_rounding(self) -> None:
        assert fmt("8.2399999", 2) == "8.23"
        assert fmt("8.235", 2) == "8.23"
        assert fmt("1.5", 0) == "1"

    def test_fmt_pads_to_the_requested_precision(self) -> None:
        assert fmt("1.2", 4) == "1.2000"
        assert fmt("7", 2) == "7.00"

    def test_fmt_defaults_to_two_places(self) -> None:
        assert fmt("1.239") == "1.23"

    def test_to_str_defaults_to_stroop_precision(self) -> None:
        assert to_str("1.5") == "1.5000000"

    @pytest.mark.parametrize("places", [-1, -10])
    def test_rejects_negative_places(self, places: int) -> None:
        with pytest.raises(FixedPointError, match="places must be >= 0"):
            fmt("1", places)

    def test_never_emits_scientific_notation(self) -> None:
        assert to_str("0.0000000000000000001", 25) == "0.0000000000000000001000000"
        assert "E" not in to_str("1e30", 0).upper()


# ─── Comparisons ─────────────────────────────────────────────────────────────


class TestComparisons:
    def test_all_five_operators(self) -> None:
        assert gt("2", "1") and not gt("1", "1")
        assert gte("1", "1") and not gte("0", "1")
        assert lt("1", "2") and not lt("1", "1")
        assert lte("1", "1") and not lte("2", "1")
        assert eq("1.0", "1") and not eq("1", "2")

    def test_compare_at_full_precision(self) -> None:
        a, b = "1.00000000000000000001", "1.00000000000000000002"
        assert not eq(a, b)
        assert lt(a, b)

    def test_return_real_bools(self) -> None:
        # The parity suite serialises these as 'true'/'false'.
        for result in (gt("2", "1"), is_zero("0"), is_positive("1")):
            assert isinstance(result, bool)

    @pytest.mark.parametrize("value", ["0", "0.0", "-0", "0.0000000"])
    def test_is_zero_recognises_every_spelling(self, value: str) -> None:
        assert is_zero(value)

    @pytest.mark.parametrize(("value", "expected"), [("1", True), ("0", False), ("-0", False), ("-1", False)])
    def test_is_positive_excludes_zero(self, value: str, expected: bool) -> None:
        assert is_positive(value) is expected


# ─── Determinism ─────────────────────────────────────────────────────────────


def test_repeated_evaluation_is_byte_identical() -> None:
    def run() -> str:
        return to_str(div(mul(add("0.1", "0.2"), "7.7777777"), "3"), 18)

    first = run()
    assert first == "0.777777770000000000"
    assert all(run() == first for _ in range(100))
