//! Deterministic fixed-point arithmetic for Stellar agent payment calculations.
//!
//! Rust port of `packages/core/src/math/fixed-point.ts`, with
//! `python/src/stellaragent/fixed_point.py` as the second reference. Every
//! function here is required to produce the byte-identical string its
//! TypeScript counterpart produces for the same input; `fixtures/determinism.json`
//! is generated from the TypeScript implementation and asserted against by all
//! three test suites.
//!
//! # Why a third implementation is worth having
//!
//! The TypeScript module exists because IEEE-754 doubles round differently on
//! x86 and ARM, so the same bid-score expression could pick different winners
//! on different machines. A port that reached for `f64` — or for a decimal
//! type whose precision is counted in significant digits rather than decimal
//! places — would reintroduce that divergence *between languages* instead of
//! between CPUs, which is strictly worse: it would reproduce on every machine.
//!
//! # Design rules
//!
//! - Never use `f64` for a monetary or score value. [`IntoDecimal`] is
//!   deliberately not implemented for floats, so the type system enforces
//!   this rather than a comment asking nicely. Pass a `&str`.
//! - Monetary amounts are strings at the API boundary.
//! - On-chain values are `i128` stroops. Use [`to_stroops`] / [`from_stroops`].
//! - [`stroop_scale`] is 10,000,000 (1 XLM = 10^7 stroops), matching Stellar.
//!
//! # Divergences from the TypeScript API, and why
//!
//! - **Floats are rejected at compile time.** The TS signature accepts
//!   `number`; here there is no `impl IntoDecimal for f64`. The Python SDK
//!   made the same call at runtime for the same reason.
//! - **Every fallible function returns [`Result`].** The TS module throws
//!   `RangeError` and Python raises `FixedPointError`; neither is a thing
//!   Rust callers should discover from a panic in a payment path.
//! - **`0x`/`0b`/`0o` literals are rejected.** `bignumber.js` accepts them;
//!   Python does not. See [`Decimal::parse`].
//!
//! ```
//! use stellaragent::math::fixed_point as fp;
//!
//! // Exact — no 0.30000000000000004.
//! assert_eq!(fp::add("0.1", "0.2")?.to_fixed(18), "0.300000000000000000");
//! // Division truncates toward zero at 18 places, never up.
//! assert_eq!(fp::div("2", "3")?.to_fixed(18), "0.666666666666666666");
//! # Ok::<(), stellaragent::math::FixedPointError>(())
//! ```

use num_bigint::BigInt;

use super::decimal::{Decimal, ParseDecimalError};

/// Decimal places retained by division — `DECIMAL_PLACES: 18` in the shared
/// `BigNumber.config`, and the single precision knob in this module.
pub const DECIMAL_PLACES: u32 = 18;

/// Stellar stroop denominator: 1 XLM = 10^7 stroops.
pub fn stroop_scale() -> Decimal {
    Decimal::from(10_000_000u32)
}

/// Basis-point denominator (100.00% = 10,000 bps).
pub fn bps_scale() -> Decimal {
    Decimal::from(10_000u32)
}

/// Invalid input, or an operation with no defined result.
///
/// Mirrors the `RangeError` the TypeScript module throws and the
/// `FixedPointError` the Python module raises, including the message text —
/// the three SDKs are meant to fail on the same inputs, recognisably.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FixedPointError {
    /// The value is not a finite decimal — a typo, an empty string, `NaN`, or
    /// `Infinity`.
    #[error("FixedPoint: value \"{value}\" is not a finite decimal")]
    NotFinite {
        /// The rejected input.
        value: String,
    },

    /// Division by zero, in any spelling: `0`, `0.0`, or `-0`.
    #[error("FixedPoint: division by zero")]
    DivisionByZero,

    /// More decimal places were requested than [`crate::math::MAX_SCALE`] allows.
    #[error("FixedPoint: places must be at most {max}, got {places}")]
    PlacesTooLarge {
        /// The requested number of decimal places.
        places: u32,
        /// The ceiling that was exceeded.
        max: u32,
    },

    /// Bid weights did not sum to exactly 1.
    ///
    /// Lives here rather than in [`crate::math::bid`] so all three SDKs raise
    /// the same error type for it, as Python's `FixedPointError` does.
    #[error("BidScorer: weights must sum to 1.0, got {sum}")]
    InvalidWeights {
        /// The offending sum, formatted to 4 places as the other SDKs format it.
        sum: String,
    },
}

impl From<ParseDecimalError> for FixedPointError {
    fn from(error: ParseDecimalError) -> Self {
        match error {
            ParseDecimalError::Syntax { input } | ParseDecimalError::NotFinite { input } => {
                Self::NotFinite { value: input }
            }
            ParseDecimalError::ScaleTooLarge { scale } => Self::PlacesTooLarge {
                places: scale.clamp(0, i64::from(u32::MAX)) as u32,
                max: super::MAX_SCALE,
            },
        }
    }
}

/// Anything this module will accept as a decimal value.
///
/// Implemented for strings, [`Decimal`], and every integer primitive.
/// **Not** implemented for `f32`/`f64`, and that omission is the point:
/// `0.1f64` is not `0.1`, so a float reaching a spend calculation would break
/// cross-language determinism in exactly the place this module exists to
/// protect. There is no escape hatch — convert deliberately, at the boundary,
/// and own the rounding you chose.
pub trait IntoDecimal {
    /// Convert, or explain why the value is not a usable decimal.
    fn into_decimal(self) -> Result<Decimal, FixedPointError>;
}

impl IntoDecimal for Decimal {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        Ok(self)
    }
}

impl IntoDecimal for &Decimal {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        Ok(self.clone())
    }
}

impl IntoDecimal for &str {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        Ok(Decimal::parse(self)?)
    }
}

impl IntoDecimal for &String {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        self.as_str().into_decimal()
    }
}

impl IntoDecimal for String {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        self.as_str().into_decimal()
    }
}

impl IntoDecimal for BigInt {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        Ok(Decimal::from_bigint(self))
    }
}

impl IntoDecimal for &BigInt {
    fn into_decimal(self) -> Result<Decimal, FixedPointError> {
        Ok(Decimal::from_bigint(self.clone()))
    }
}

macro_rules! into_decimal_for_integer {
    ($($ty:ty),* $(,)?) => {
        $(impl IntoDecimal for $ty {
            fn into_decimal(self) -> Result<Decimal, FixedPointError> {
                Ok(Decimal::from(self))
            }
        })*
    };
}

into_decimal_for_integer!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, isize, usize);

// ─── Core helper ─────────────────────────────────────────────────────────────

/// Coerce a value to a [`Decimal`], rejecting anything unusable.
///
/// The counterpart of the TypeScript `bn()`: validating at every entry point
/// stops a `NaN` propagating silently through a calculation and surfacing
/// three layers later as a nonsense spend limit.
///
/// ```
/// use stellaragent::math::fixed_point as fp;
///
/// assert_eq!(fp::bn("1.50")?.to_fixed(2), "1.50");
/// assert!(fp::bn("abc").is_err());
/// assert!(fp::bn("").is_err());
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn bn<V: IntoDecimal>(value: V) -> Result<Decimal, FixedPointError> {
    value.into_decimal()
}

// ─── Arithmetic ──────────────────────────────────────────────────────────────

/// Deterministic addition: `a + b`. Exact, at unbounded precision.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::add("0.1", "0.2")?.to_fixed(18), "0.300000000000000000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn add<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<Decimal, FixedPointError> {
    Ok(bn(a)?.add(&bn(b)?))
}

/// Deterministic subtraction: `a - b`. Exact.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::sub("0.1", "0.2")?.to_fixed(18), "-0.100000000000000000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn sub<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<Decimal, FixedPointError> {
    Ok(bn(a)?.sub(&bn(b)?))
}

/// Deterministic multiplication: `a * b`. Exact — the product of two 39-digit
/// `i128`s keeps all 78 digits.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::mul("0.1", "0.2")?.to_fixed(18), "0.020000000000000000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn mul<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<Decimal, FixedPointError> {
    Ok(bn(a)?.mul(&bn(b)?))
}

/// Deterministic division: `a / b`, truncated toward zero at
/// [`DECIMAL_PLACES`] places.
///
/// Truncation is the safe direction for a spend-limit comparison: the result
/// can never exceed the true quotient, so a rounding artefact can only ever
/// under-report what an agent is about to spend, never over-authorise it.
///
/// A quotient smaller than `1e-18` truncates to zero, exactly as in the other
/// two SDKs. Do not use this to compare quantities below that.
///
/// # Errors
///
/// [`FixedPointError::DivisionByZero`] when `b` is zero in any spelling —
/// `0`, `0.0` or `-0`.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::div("1", "3")?.to_fixed(18), "0.333333333333333333");
/// assert!(fp::div("1", "0").is_err());
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn div<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<Decimal, FixedPointError> {
    let divisor = bn(b)?;
    bn(a)?
        .div_truncate(&divisor, DECIMAL_PLACES)
        .ok_or(FixedPointError::DivisionByZero)
}

/// Percentage of `value` out of `total`, truncated to `decimal_places`.
///
/// Returns exactly zero when `total` is zero — unlike [`div`], which errors. A
/// zero total is a normal state for a progress bar (nothing budgeted yet), not
/// a programming error.
///
/// The operation order is load-bearing and copied from the TypeScript
/// original: divide (truncating at 18 places) **then** multiply by 100
/// **then** truncate to `decimal_places`. Reordering changes the last digit,
/// and the fixtures would catch it.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::pct("1.45", "5.00", 4)?.to_fixed(4), "29.0000");
/// assert_eq!(fp::pct("2", "3", 2)?.to_fixed(2), "66.66");
/// assert_eq!(fp::pct("1", "0", 4)?.to_fixed(4), "0.0000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn pct<V: IntoDecimal, T: IntoDecimal>(
    value: V,
    total: T,
    decimal_places: u32,
) -> Result<Decimal, FixedPointError> {
    check_places(decimal_places)?;
    let total = bn(total)?;
    if total.is_zero() {
        return Ok(Decimal::zero().truncate(decimal_places));
    }
    let quotient = bn(value)?
        .div_truncate(&total, DECIMAL_PLACES)
        .ok_or(FixedPointError::DivisionByZero)?;
    Ok(quotient.mul(&Decimal::from(100u8)).truncate(decimal_places))
}

/// Clamp `value` to `[minimum, maximum]`.
///
/// Applies `max` then `min`, matching the TypeScript
/// `BigNumber.minimum(BigNumber.maximum(v, min), max)`. With an inverted range
/// (`minimum > maximum`) the *maximum* therefore wins — a quirk shared by all
/// three SDKs, and pinned by a fixture.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::clamp("99", "0", "10")?.to_fixed(0), "10");
/// assert_eq!(fp::clamp("-3", "0", "10")?.to_fixed(0), "0");
/// assert_eq!(fp::clamp("7", "10", "0")?.to_fixed(0), "0"); // inverted range
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn clamp<V: IntoDecimal, L: IntoDecimal, H: IntoDecimal>(
    value: V,
    minimum: L,
    maximum: H,
) -> Result<Decimal, FixedPointError> {
    let value = bn(value)?;
    let minimum = bn(minimum)?;
    let maximum = bn(maximum)?;
    let lifted = if value > minimum { value } else { minimum };
    Ok(if lifted < maximum { lifted } else { maximum })
}

/// Sum a sequence of decimal values deterministically. An empty sum is zero.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// let tenths = ["0.1"; 10];
/// assert_eq!(fp::sum_strings(tenths)?.to_fixed(18), "1.000000000000000000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn sum_strings<I>(values: I) -> Result<Decimal, FixedPointError>
where
    I: IntoIterator,
    I::Item: IntoDecimal,
{
    let mut total = Decimal::zero();
    for value in values {
        total = total.add(&bn(value)?);
    }
    Ok(total)
}

// ─── Stroop conversions ──────────────────────────────────────────────────────

/// Convert a human-readable amount to Stellar stroops.
///
/// Truncates sub-stroop fractions rather than rounding: rounding up here would
/// let an agent spend one stroop more than its limit allows, on every payment.
///
/// Returns a [`BigInt`] rather than an `i128` because the conversion itself
/// has no reason to overflow — range-check against `i128` at the point you
/// actually build the `ScVal`, where a failure can name the argument.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::to_stroops("1.5000001")?.to_string(), "15000001");
/// assert_eq!(fp::to_stroops("1.50000019")?.to_string(), "15000001"); // truncated
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn to_stroops<V: IntoDecimal>(amount: V) -> Result<BigInt, FixedPointError> {
    Ok(bn(amount)?.mul(&stroop_scale()).to_integer_truncated())
}

/// Convert on-chain stroops to a decimal string with `decimal_places` places.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// # use num_bigint::BigInt;
/// assert_eq!(fp::from_stroops(&BigInt::from(15_000_001), 7)?, "1.5000001");
/// assert_eq!(fp::from_stroops(&BigInt::from(15_000_001), 2)?, "1.50");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn from_stroops(stroops: &BigInt, decimal_places: u32) -> Result<String, FixedPointError> {
    check_places(decimal_places)?;
    let quotient = Decimal::from_bigint(stroops.clone())
        .div_truncate(&stroop_scale(), DECIMAL_PLACES)
        .ok_or(FixedPointError::DivisionByZero)?;
    Ok(quotient.to_fixed(decimal_places))
}

// ─── Formatting ──────────────────────────────────────────────────────────────

/// Format for display, truncating (never rounding up) to `places`.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::fmt("8.2399999", 2)?, "8.23");
/// assert_eq!(fp::fmt("-8.239", 2)?, "-8.23");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn fmt<V: IntoDecimal>(value: V, places: u32) -> Result<String, FixedPointError> {
    check_places(places)?;
    Ok(bn(value)?.to_fixed(places))
}

/// Stringify for storage or wire transmission, never in scientific notation.
///
/// Identical to [`fmt`] but with a default of 7 places at the call sites that
/// use it — kept as a separate name so the three SDKs' surfaces line up.
///
/// ```
/// # use stellaragent::math::fixed_point as fp;
/// assert_eq!(fp::to_str("1.5", 7)?, "1.5000000");
/// # Ok::<(), stellaragent::math::FixedPointError>(())
/// ```
pub fn to_str<V: IntoDecimal>(value: V, places: u32) -> Result<String, FixedPointError> {
    fmt(value, places)
}

// ─── Comparisons ─────────────────────────────────────────────────────────────

/// `a > b`
pub fn gt<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<bool, FixedPointError> {
    Ok(bn(a)? > bn(b)?)
}

/// `a >= b`
pub fn gte<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<bool, FixedPointError> {
    Ok(bn(a)? >= bn(b)?)
}

/// `a < b`
pub fn lt<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<bool, FixedPointError> {
    Ok(bn(a)? < bn(b)?)
}

/// `a <= b`
pub fn lte<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<bool, FixedPointError> {
    Ok(bn(a)? <= bn(b)?)
}

/// `a == b` by numeric value, so `"1.0"` equals `"1"`.
pub fn eq<A: IntoDecimal, B: IntoDecimal>(a: A, b: B) -> Result<bool, FixedPointError> {
    Ok(bn(a)? == bn(b)?)
}

/// Whether `a` is zero, in any spelling — `0`, `0.00`, `-0`, `0e10`.
pub fn is_zero<A: IntoDecimal>(a: A) -> Result<bool, FixedPointError> {
    Ok(bn(a)?.is_zero())
}

/// Whether `a` is strictly greater than zero. Zero is not positive.
pub fn is_positive<A: IntoDecimal>(a: A) -> Result<bool, FixedPointError> {
    Ok(bn(a)?.is_positive())
}

// ─── Internals ───────────────────────────────────────────────────────────────

fn check_places(places: u32) -> Result<(), FixedPointError> {
    if places > super::MAX_SCALE {
        return Err(FixedPointError::PlacesTooLarge {
            places,
            max: super::MAX_SCALE,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bn_rejects_the_same_inputs_the_other_sdks_reject() {
        for input in ["abc", "", "1.2.3", "NaN", "Infinity"] {
            assert!(bn(input).is_err(), "{input:?} should be rejected");
        }
    }

    #[test]
    fn division_by_zero_is_an_error_in_every_spelling() {
        for divisor in ["0", "0.0", "-0", "0.000", "0e10"] {
            assert_eq!(
                div("1", divisor).unwrap_err(),
                FixedPointError::DivisionByZero,
                "dividing by {divisor:?}"
            );
        }
    }

    #[test]
    fn pct_short_circuits_a_zero_total_rather_than_erroring() {
        assert_eq!(pct("1", "0", 4).unwrap().to_fixed(4), "0.0000");
        assert_eq!(pct("0", "0", 8).unwrap().to_fixed(8), "0.00000000");
    }

    #[test]
    fn pct_order_of_operations_matches_the_typescript_original() {
        // divide (18 places) → times 100 → truncate. Doing it the other way
        // around gives 66.6667 / 66.67, both wrong.
        assert_eq!(pct("2", "3", 4).unwrap().to_fixed(4), "66.6666");
        assert_eq!(pct("1", "7", 8).unwrap().to_fixed(8), "14.28571428");
    }

    #[test]
    fn clamp_lets_the_maximum_win_an_inverted_range() {
        assert_eq!(clamp("7", "10", "0").unwrap().to_fixed(0), "0");
        assert_eq!(clamp("7", "3", "3").unwrap().to_fixed(0), "3");
    }

    #[test]
    fn stroops_round_trip_and_truncate_toward_zero() {
        assert_eq!(to_stroops("0.0000001").unwrap().to_string(), "1");
        assert_eq!(to_stroops("0.00000001").unwrap().to_string(), "0");
        assert_eq!(to_stroops("-8.239").unwrap().to_string(), "-82390000");
        assert_eq!(
            from_stroops(&to_stroops("123456.7891234").unwrap(), 7).unwrap(),
            "123456.7891234"
        );
    }

    #[test]
    fn from_stroops_keeps_the_sign_of_a_value_that_truncates_to_zero() {
        // -1 stroop is -0.0000001 XLM; asked for 2 places it is still a debit,
        // and reporting it as "0.00" would lose that.
        assert_eq!(from_stroops(&BigInt::from(-1), 2).unwrap(), "-0.00");
        assert_eq!(from_stroops(&BigInt::from(-1), 7).unwrap(), "-0.0000001");
    }

    #[test]
    fn comparisons_ignore_trailing_zeroes() {
        assert!(eq("1.0", "1").unwrap());
        assert!(gte("1", "1").unwrap());
        assert!(!gt("1", "1").unwrap());
        assert!(lte("1", "1").unwrap());
        assert!(!lt("1", "1").unwrap());
        assert!(is_zero("-0").unwrap());
        assert!(!is_positive("0").unwrap());
        assert!(is_positive("0.0000000000000000001").unwrap());
    }

    #[test]
    fn sum_is_exact_across_a_long_run_of_tenths() {
        let values = vec!["0.1"; 10];
        assert_eq!(
            sum_strings(values).unwrap().to_fixed(18),
            "1.000000000000000000"
        );
    }

    #[test]
    fn places_beyond_the_ceiling_are_rejected_not_allocated() {
        assert!(matches!(
            fmt("1", super::super::MAX_SCALE + 1),
            Err(FixedPointError::PlacesTooLarge { .. })
        ));
    }
}
