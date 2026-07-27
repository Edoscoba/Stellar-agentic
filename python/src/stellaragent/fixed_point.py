"""Deterministic fixed-point arithmetic for Stellar agent payment calculations.

Python port of ``packages/core/src/math/fixed-point.ts``.

Why this module exists
----------------------
The TypeScript original exists because IEEE-754 doubles round differently on
x86 (SSE2) and ARM (NEON), so the same bid-score expression could produce
different results on different machines and break hash agreement. It solves
that by routing every calculation through ``bignumber.js``.

A Python port that reached for ``float`` — or for ``Decimal`` with mismatched
context settings — would reintroduce exactly the divergence the TS module was
written to prevent, only now *between languages* rather than between CPUs.
So this module is a semantic port, not a rewrite: every function here is
required to produce the byte-identical string the TS function produces for the
same input. ``fixtures/determinism.json`` is generated from the TS
implementation and asserted against by both test suites.

Matching ``bignumber.js`` semantics
-----------------------------------
The TS module configures::

    BigNumber.config({
      DECIMAL_PLACES: 18,
      ROUNDING_MODE: BigNumber.ROUND_DOWN,
      EXPONENTIAL_AT: [-18, 36],
    })

Three details make a naive translation wrong:

1. **``DECIMAL_PLACES`` applies to division only.** In ``bignumber.js``,
   ``+``, ``-`` and ``*`` are *exact* and unbounded; only ``dividedBy`` is
   rounded to 18 decimal places. Python's ``Context.prec``, by contrast,
   applies to every operation and counts *significant digits*, not decimal
   places. Setting ``prec = 18`` would silently corrupt large additions.
   This module therefore uses a deliberately large precision so add/sub/mul
   stay exact, and quantizes *only* division to 18 decimal places.

2. **``DECIMAL_PLACES`` is decimal places, ``prec`` is significant digits.**
   Division is handled with an explicit ``quantize`` to ``1e-18`` rather than
   by setting precision.

3. **Negative zero.** ``BigNumber('-0')`` normalises to ``0`` at parse time,
   so ``BigNumber('-0').toFixed(2) === '0.00'``. But truncating a negative
   value *preserves* the sign: ``BigNumber('-0.5').toFixed(0, ROUND_DOWN)``
   is ``'-0'``. ``Decimal('-0')`` keeps its sign, so :func:`bn` normalises
   zero at construction to reproduce the first behaviour, while ``quantize``
   naturally reproduces the second.

Design rules
------------
- Never use ``float`` for a monetary or score value. :func:`bn` rejects it
  outright — see the note on that function.
- Monetary amounts are strings at the API boundary.
- On-chain values are ``i128`` stroops (``int``). Use :func:`to_stroops` /
  :func:`from_stroops`.
- ``STROOP_SCALE`` is 10_000_000 (1 XLM = 10^7 stroops), matching Stellar.
"""

from __future__ import annotations

from decimal import ROUND_DOWN, Context, Decimal, InvalidOperation

__all__ = [
    "STROOP_SCALE",
    "BPS_SCALE",
    "DECIMAL_PLACES",
    "FixedPointError",
    "bn",
    "add",
    "sub",
    "mul",
    "div",
    "pct",
    "clamp",
    "sum_strings",
    "to_stroops",
    "from_stroops",
    "fmt",
    "to_str",
    "gt",
    "gte",
    "lt",
    "lte",
    "eq",
    "is_zero",
    "is_positive",
]

# ─── Configuration ───────────────────────────────────────────────────────────

#: Decimal places retained by division — ``DECIMAL_PLACES: 18`` in the TS config.
DECIMAL_PLACES = 18

#: Quantum used to truncate division results.
_QUANTUM = Decimal(1).scaleb(-DECIMAL_PLACES)

#: Working precision, in *significant digits*.
#:
#: Large enough that add/sub/mul never round, matching ``bignumber.js``'s
#: unbounded exact arithmetic across this SDK's domain: an ``i128`` is 39
#: digits, a product of two is 78, and 18 fractional digits on top of that is
#: ~96. 500 leaves a wide margin. A dedicated Context object is used rather
#: than ``getcontext()`` so a caller changing the thread-local context cannot
#: silently alter results here — which would be precisely the class of
#: non-determinism this module exists to prevent.
_PRECISION = 500

_CTX = Context(prec=_PRECISION, rounding=ROUND_DOWN)

# ─── Constants ───────────────────────────────────────────────────────────────

#: Stellar stroop denominator: 1 XLM = 10^7 stroops.
STROOP_SCALE = Decimal(10_000_000)

#: Basis-point denominator (100.00% = 10 000 bps).
BPS_SCALE = Decimal(10_000)

# Accepted input types. `float` is deliberately absent — see `bn`.
# A runtime alias, so this uses PEP 604 syntax available from Python 3.10,
# which is this package's declared floor.
Numeric = str | int | Decimal


class FixedPointError(ValueError):
    """Raised for invalid input or an undefined operation.

    Subclasses :class:`ValueError` so ordinary ``except ValueError`` handling
    still works. Mirrors the ``RangeError`` the TS module raises.
    """


# ─── Core helpers ────────────────────────────────────────────────────────────


def bn(value: Numeric) -> Decimal:
    """Coerce a value to :class:`~decimal.Decimal`, rejecting anything unusable.

    Mirrors the TS ``bn()``: validating at every entry point stops ``NaN`` and
    ``Infinity`` propagating silently through a calculation.

    ``float`` is rejected rather than converted. The TS signature accepts
    ``number``, but a Python ``float`` cannot round-trip a decimal string —
    ``Decimal(0.1)`` is ``0.1000000000000000055511151231257827``, not
    ``Decimal('0.1')`` — so accepting one would reintroduce cross-language
    divergence in the exact place this module exists to prevent it. Pass a
    string instead. This is the one intentional divergence from the TS API.

    :raises FixedPointError: on a float, a non-finite value, or an
        unparseable string.
    """
    if isinstance(value, bool):
        # bool is an int subclass; almost certainly a mistake here.
        raise FixedPointError(f'FixedPoint: value "{value}" is not a finite decimal')
    if isinstance(value, float):
        raise FixedPointError(
            f"FixedPoint: refusing to convert float {value!r} — floats cannot "
            "represent decimal values exactly and would break cross-platform "
            "determinism. Pass a string, int, or Decimal instead."
        )
    if isinstance(value, Decimal):
        result = value
    elif isinstance(value, int):
        result = Decimal(value)
    elif isinstance(value, str):
        try:
            result = Decimal(value.strip()) if value.strip() else Decimal("nan")
        except InvalidOperation:
            raise FixedPointError(
                f'FixedPoint: value "{value}" is not a finite decimal'
            ) from None
    else:
        raise FixedPointError(f'FixedPoint: value "{value}" is not a finite decimal')

    if not result.is_finite():
        raise FixedPointError(f'FixedPoint: value "{value}" is not a finite decimal')

    # `BigNumber('-0')` parses to plain zero, so `-0` never survives
    # construction on the TS side. Normalise here to match.
    if result.is_zero():
        return Decimal(0)
    return result


# ─── Arithmetic ──────────────────────────────────────────────────────────────


def add(a: Numeric, b: Numeric) -> Decimal:
    """Deterministic addition: ``a + b``. Exact, like ``bignumber.js``."""
    return _CTX.add(bn(a), bn(b))


def sub(a: Numeric, b: Numeric) -> Decimal:
    """Deterministic subtraction: ``a - b``. Exact."""
    return _CTX.subtract(bn(a), bn(b))


def mul(a: Numeric, b: Numeric) -> Decimal:
    """Deterministic multiplication: ``a * b``. Exact."""
    return _CTX.multiply(bn(a), bn(b))


def div(a: Numeric, b: Numeric) -> Decimal:
    """Deterministic division: ``a / b``.

    Truncated (``ROUND_DOWN``) to 18 decimal places, so the result can never
    exceed the true value — the safe direction for a spend-limit comparison.

    A quotient smaller than ``1e-18`` truncates to zero, exactly as it does in
    the TS module. Do not use this to compare quantities below that.

    :raises FixedPointError: when ``b`` is zero.
    """
    divisor = bn(b)
    if divisor.is_zero():
        raise FixedPointError("FixedPoint: division by zero")
    return _quantize(_CTX.divide(bn(a), divisor), DECIMAL_PLACES)


def pct(value: Numeric, total: Numeric, decimal_places: int = 4) -> Decimal:
    """Percentage of ``value`` out of ``total``, truncated to ``decimal_places``.

    Returns exactly zero when ``total`` is zero — unlike :func:`div`, which
    raises. A zero total is a normal state for a progress bar (nothing
    budgeted yet), not a programming error.

    The operation order matters and is copied from the TS original: divide
    (truncating at 18 places) *then* multiply by 100 *then* truncate to
    ``decimal_places``. Reordering changes the last digit.

    >>> str(pct("1.45", "5.00"))
    '29.0000'
    """
    t = bn(total)
    if t.is_zero():
        return _quantize(Decimal(0), decimal_places)
    quotient = _quantize(_CTX.divide(bn(value), t), DECIMAL_PLACES)
    return _quantize(_CTX.multiply(quotient, Decimal(100)), decimal_places)


def clamp(value: Numeric, minimum: Numeric, maximum: Numeric) -> Decimal:
    """Clamp ``value`` to ``[minimum, maximum]``.

    Applies ``max`` then ``min``, matching the TS
    ``BigNumber.minimum(BigNumber.maximum(v, min), max)``. With an inverted
    range (``min > max``) the maximum therefore wins.
    """
    return min(max(bn(value), bn(minimum)), bn(maximum))


def sum_strings(values: list[Numeric] | tuple[Numeric, ...]) -> Decimal:
    """Sum a sequence of decimal values deterministically. Empty sums to zero."""
    total = Decimal(0)
    for value in values:
        total = _CTX.add(total, bn(value))
    return total


# ─── Stroop conversions ──────────────────────────────────────────────────────


def to_stroops(amount: Numeric) -> int:
    """Convert a human-readable amount to Stellar stroops as an ``int``.

    Truncates sub-stroop fractions rather than rounding: rounding up here
    would let an agent spend one stroop more than its limit allows.

    Python's ``int`` is arbitrary-precision, so this is the natural
    counterpart to the TS ``bigint`` return and is ``i128``-safe.

    >>> to_stroops("1.5000001")
    15000001
    """
    scaled = _CTX.multiply(bn(amount), STROOP_SCALE)
    return int(scaled.to_integral_value(rounding=ROUND_DOWN))


def from_stroops(stroops: int, decimal_places: int = 7) -> str:
    """Convert on-chain stroops to a decimal string with ``decimal_places``.

    >>> from_stroops(15000001, 7)
    '1.5000001'
    """
    if isinstance(stroops, bool) or not isinstance(stroops, int):
        raise FixedPointError(f"FixedPoint: stroops must be an int, got {stroops!r}")
    quotient = _CTX.divide(Decimal(stroops), STROOP_SCALE)
    return _format(_quantize(quotient, decimal_places))


# ─── Formatting ──────────────────────────────────────────────────────────────


def fmt(value: Numeric, places: int = 2) -> str:
    """Format for display, truncating (never rounding up) to ``places``.

    >>> fmt("8.2399999", 2)
    '8.23'
    """
    return _format(_quantize(bn(value), places))


def to_str(value: Numeric, places: int = 7) -> str:
    """Stringify for storage or wire transmission, never in scientific notation."""
    return _format(_quantize(bn(value), places))


# ─── Comparisons ─────────────────────────────────────────────────────────────


def gt(a: Numeric, b: Numeric) -> bool:
    """``a > b``"""
    return bn(a) > bn(b)


def gte(a: Numeric, b: Numeric) -> bool:
    """``a >= b``"""
    return bn(a) >= bn(b)


def lt(a: Numeric, b: Numeric) -> bool:
    """``a < b``"""
    return bn(a) < bn(b)


def lte(a: Numeric, b: Numeric) -> bool:
    """``a <= b``"""
    return bn(a) <= bn(b)


def eq(a: Numeric, b: Numeric) -> bool:
    """``a == b`` by numeric value, so ``'1.0'`` equals ``'1'``."""
    return bn(a) == bn(b)


def is_zero(a: Numeric) -> bool:
    """Whether ``a`` is zero, in any spelling."""
    return bn(a).is_zero()


def is_positive(a: Numeric) -> bool:
    """Whether ``a`` is strictly greater than zero. Zero is not positive."""
    value = bn(a)
    return value > 0 and not value.is_zero()


# ─── Internals ───────────────────────────────────────────────────────────────


def _quantize(value: Decimal, places: int) -> Decimal:
    """Truncate to ``places`` decimal places — the TS ``toFixed(n, ROUND_DOWN)``.

    ``quantize`` raises when the result would need more digits than the
    context allows, so the exponent range is widened for the call rather than
    letting a legitimately large value fail.
    """
    if places < 0:
        raise FixedPointError(f"FixedPoint: places must be >= 0, got {places}")
    quantum = _QUANTUM if places == DECIMAL_PLACES else Decimal(1).scaleb(-places)
    try:
        return value.quantize(quantum, rounding=ROUND_DOWN, context=_CTX)
    except InvalidOperation:
        raise FixedPointError(
            f"FixedPoint: cannot represent {value} to {places} decimal places "
            f"within {_PRECISION} significant digits"
        ) from None


def _format(value: Decimal) -> str:
    """Render without scientific notation, matching ``toFixed``'s output."""
    return format(value, "f")
