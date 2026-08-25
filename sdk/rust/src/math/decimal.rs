//! Arbitrary-precision decimal arithmetic, semantically matching `bignumber.js`.
//!
//! # Why this type exists rather than `f64` or a fixed-scale decimal
//!
//! The TypeScript SDK routes every monetary and score calculation through
//! `bignumber.js` because IEEE-754 doubles round differently on x86 (SSE2) and
//! ARM (NEON), which is enough to make two agents disagree about which bid won.
//! `packages/core/src/math/fixed-point.ts` documents that history. The Python
//! SDK re-derived the same guarantee on top of `decimal.Decimal`.
//!
//! Rust has no `bignumber.js` equivalent whose rounding rules match by
//! construction, so this module implements the arithmetic directly:
//!
//! ```text
//! value = unscaled / 10^scale        (unscaled: BigInt, scale: u32)
//! ```
//!
//! Addition, subtraction and multiplication are **exact and unbounded** — the
//! scale of a product is the sum of the operands' scales and no digits are ever
//! discarded. Only division rounds, and only in one direction ([`Decimal::div_truncate`]).
//! That is precisely `bignumber.js`'s contract: `plus`/`minus`/`times` are
//! exact, and `DECIMAL_PLACES` bounds `dividedBy` alone.
//!
//! # The three rules that make a naive port wrong
//!
//! 1. **`DECIMAL_PLACES` counts decimal places, not significant digits.**
//!    A crate whose precision is expressed in significant digits (as
//!    `rust_decimal` or a `Context.prec` would be) truncates
//!    `123456789012345678901234567890 / 987654321098765432109876543210`
//!    somewhere other than the 18th decimal place and diverges on the last
//!    digit. Here the truncation point is always a decimal place.
//!
//! 2. **Rounding is `ROUND_DOWN` — toward zero, never half-even.** Rounding a
//!    spend calculation up by one stroop is the difference between a payment
//!    the chain accepts and one it rejects. `BigInt`'s own division truncates
//!    toward zero, which is the same direction, so the two agree for negative
//!    operands as well as positive ones.
//!
//! 3. **Formatting a negative value that truncates away to zero keeps its
//!    sign; a value that *is* zero does not.** `bignumber.js` decides the sign
//!    of `toFixed`'s output from the coefficient *before* rounding:
//!    `BigNumber('-0.00001').toFixed(4, ROUND_DOWN)` is `'-0.0000'`, but
//!    `BigNumber('-0.00001').decimalPlaces(4, ROUND_DOWN).toFixed(4)` — which
//!    rounds first, producing a value that is genuinely zero — is `'0.0000'`,
//!    and `BigNumber('-0').toFixed(2)` is `'0.00'`. [`Decimal::to_fixed`] and
//!    [`Decimal::truncate`] reproduce that split exactly: `to_fixed` takes the
//!    sign from the receiver, `truncate` returns a value that has genuinely
//!    lost it.
//!
//! # What is deliberately absent
//!
//! There is no `From<f64>`. Accepting a float would reintroduce, at the API
//! boundary, exactly the divergence this module exists to prevent —
//! `0.1f64` is not `0.1`. The Python SDK made the same call for the same
//! reason. Pass a string.

use std::cmp::Ordering;
use std::fmt;
use std::ops::Neg;

use num_bigint::{BigInt, Sign};
use num_traits::{One, Zero};

/// Largest scale (decimal places) this type will construct.
///
/// Not a precision limit — the *integer* part is unbounded — but a guard on
/// the fractional part, so a pathological input like `1e-4000000000` cannot
/// make a rescale allocate gigabytes of zeroes. Comfortably above anything
/// this SDK needs: the widest real value is an `i128` of stroops (39 digits)
/// against the 18 decimal places division retains.
pub const MAX_SCALE: u32 = 10_000;

/// Failure modes of [`Decimal::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDecimalError {
    /// The string is not a decimal literal at all.
    Syntax {
        /// The offending input, echoed back for the error message.
        input: String,
    },
    /// `NaN`, `Infinity` or `-Infinity`.
    ///
    /// Split out from [`ParseDecimalError::Syntax`] because `bignumber.js`
    /// *parses* these successfully and only fails the subsequent
    /// `isFinite()` check — the distinction matters when reproducing which
    /// inputs the TypeScript SDK rejects and why.
    NotFinite {
        /// The offending input, echoed back for the error message.
        input: String,
    },
    /// More fractional digits than [`MAX_SCALE`].
    ScaleTooLarge {
        /// The scale that was asked for.
        scale: i64,
    },
}

impl fmt::Display for ParseDecimalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { input } => {
                write!(f, "\"{input}\" is not a decimal literal")
            }
            Self::NotFinite { input } => {
                write!(f, "\"{input}\" is not a finite decimal")
            }
            Self::ScaleTooLarge { scale } => write!(
                f,
                "scale {scale} exceeds the maximum of {MAX_SCALE} decimal places"
            ),
        }
    }
}

impl std::error::Error for ParseDecimalError {}

/// An exact decimal number: `unscaled / 10^scale`.
///
/// Cheap to clone only in the sense that `BigInt` is — clone it freely for
/// small values, pass by reference in hot loops.
///
/// ```
/// use stellaragent::math::Decimal;
///
/// let a = Decimal::parse("0.1").unwrap();
/// let b = Decimal::parse("0.2").unwrap();
/// // Exact: no 0.30000000000000004 anywhere.
/// assert_eq!(a.add(&b).to_fixed(18), "0.300000000000000000");
/// ```
#[derive(Debug, Clone)]
pub struct Decimal {
    unscaled: BigInt,
    scale: u32,
}

impl Decimal {
    /// The additive identity, at scale 0.
    pub fn zero() -> Self {
        Self {
            unscaled: BigInt::zero(),
            scale: 0,
        }
    }

    /// The multiplicative identity, at scale 0.
    pub fn one() -> Self {
        Self {
            unscaled: BigInt::one(),
            scale: 0,
        }
    }

    /// Build directly from an unscaled integer and a scale.
    ///
    /// `from_parts(15_000_001, 7)` is `1.5000001`.
    ///
    /// ```
    /// use stellaragent::math::Decimal;
    /// assert_eq!(Decimal::from_parts(15_000_001.into(), 7).to_fixed(7), "1.5000001");
    /// ```
    pub fn from_parts(unscaled: BigInt, scale: u32) -> Self {
        Self { unscaled, scale }
    }

    /// The integer `value`, at scale 0.
    pub fn from_bigint(value: BigInt) -> Self {
        Self {
            unscaled: value,
            scale: 0,
        }
    }

    /// The unscaled integer: `self * 10^self.scale()`.
    pub fn unscaled(&self) -> &BigInt {
        &self.unscaled
    }

    /// How many decimal places this value carries. Not normalised: `1.00`
    /// keeps scale 2 and compares equal to `1`.
    pub fn scale(&self) -> u32 {
        self.scale
    }

    /// Whether this is exactly zero, at any scale and either sign.
    pub fn is_zero(&self) -> bool {
        self.unscaled.is_zero()
    }

    /// Whether this is strictly less than zero. Negative zero is not negative.
    pub fn is_negative(&self) -> bool {
        self.unscaled.sign() == Sign::Minus
    }

    /// Whether this is strictly greater than zero. Zero is not positive.
    pub fn is_positive(&self) -> bool {
        self.unscaled.sign() == Sign::Plus
    }

    /// Parse a decimal literal.
    ///
    /// Accepts surrounding whitespace, an optional sign, an optional decimal
    /// point with digits on either or both sides, and an optional `e`/`E`
    /// exponent — the same shapes `BigNumber` and Python's `Decimal` both
    /// take, which is what the shared fixtures exercise.
    ///
    /// Rejects `NaN`/`Infinity` (as [`ParseDecimalError::NotFinite`]), and
    /// rejects the `0x`/`0b`/`0o` base prefixes that `bignumber.js` alone
    /// accepts. A hex literal reaching a monetary parser is a bug, not a
    /// value; the Python SDK rejects them too, so this is the behaviour two
    /// of the three SDKs already share.
    ///
    /// ```
    /// use stellaragent::math::Decimal;
    ///
    /// assert_eq!(Decimal::parse(" 1e5 ").unwrap().to_fixed(0), "100000");
    /// assert_eq!(Decimal::parse(".5").unwrap().to_fixed(2), "0.50");
    /// assert_eq!(Decimal::parse("-0").unwrap().to_fixed(2), "0.00");
    /// assert!(Decimal::parse("0x1f").is_err());
    /// ```
    pub fn parse(input: &str) -> Result<Self, ParseDecimalError> {
        let trimmed = input.trim();
        let syntax = || ParseDecimalError::Syntax {
            input: input.to_string(),
        };

        if trimmed.is_empty() {
            return Err(syntax());
        }

        // `bignumber.js` parses these to NaN/Infinity and `bn()` then rejects
        // them on `isFinite()`. Reporting the same distinction keeps the
        // error text aligned with the other two SDKs.
        let unsigned_probe = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
        if unsigned_probe.eq_ignore_ascii_case("nan")
            || unsigned_probe.eq_ignore_ascii_case("inf")
            || unsigned_probe.eq_ignore_ascii_case("infinity")
        {
            return Err(ParseDecimalError::NotFinite {
                input: input.to_string(),
            });
        }

        let mut rest = trimmed;
        let mut negative = false;
        if let Some(stripped) = rest.strip_prefix('-') {
            negative = true;
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix('+') {
            rest = stripped;
        }

        // Split off the exponent before touching the mantissa, so `1e5` and
        // `1.5E-3` take the same path as a plain literal.
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(index) => {
                let (m, e) = rest.split_at(index);
                let digits = &e[1..];
                (m, parse_exponent(digits).ok_or_else(syntax)?)
            }
            None => (rest, 0i64),
        };

        let (int_part, frac_part) = match mantissa.find('.') {
            Some(index) => (&mantissa[..index], &mantissa[index + 1..]),
            None => (mantissa, ""),
        };

        // `.5` and `5.` are both legal; `.` alone is not.
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(syntax());
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(syntax());
        }

        let digits: String = format!("{int_part}{frac_part}");
        let mut unscaled = BigInt::parse_bytes(digits.as_bytes(), 10).ok_or_else(syntax)?;
        if negative {
            unscaled = -unscaled;
        }

        // Scale from the literal, then shifted by the exponent. A positive
        // exponent that outruns the fractional digits multiplies instead of
        // producing a negative scale, so `scale` stays unsigned and
        // formatting never has to reason about implicit trailing zeroes.
        let scale = i64::try_from(frac_part.len())
            .map_err(|_| ParseDecimalError::ScaleTooLarge { scale: i64::MAX })?
            - exponent;

        if scale > i64::from(MAX_SCALE) {
            return Err(ParseDecimalError::ScaleTooLarge { scale });
        }

        if scale < 0 {
            let shift =
                u32::try_from(-scale).map_err(|_| ParseDecimalError::ScaleTooLarge { scale })?;
            // A `1e900000000` style input would be an unbounded allocation
            // even though its *scale* is zero, so bound the shift too.
            if shift > MAX_SCALE {
                return Err(ParseDecimalError::ScaleTooLarge { scale });
            }
            Ok(Self {
                unscaled: unscaled * pow10(shift),
                scale: 0,
            })
        } else {
            Ok(Self {
                unscaled,
                scale: scale as u32,
            })
        }
    }

    /// Return an equal value carrying at least `target` decimal places.
    ///
    /// Never loses information: a request to *reduce* the scale is ignored
    /// (use [`Decimal::truncate`] to actually drop digits).
    pub fn rescale(&self, target: u32) -> Self {
        if target <= self.scale {
            return self.clone();
        }
        Self {
            unscaled: &self.unscaled * pow10(target - self.scale),
            scale: target,
        }
    }

    /// Exact addition. The result's scale is the larger of the two operands'.
    pub fn add(&self, other: &Self) -> Self {
        let scale = self.scale.max(other.scale);
        Self {
            unscaled: self.rescale(scale).unscaled + other.rescale(scale).unscaled,
            scale,
        }
    }

    /// Exact subtraction. The result's scale is the larger of the two operands'.
    pub fn sub(&self, other: &Self) -> Self {
        let scale = self.scale.max(other.scale);
        Self {
            unscaled: self.rescale(scale).unscaled - other.rescale(scale).unscaled,
            scale,
        }
    }

    /// Exact multiplication. The result's scale is the sum of the operands' —
    /// no digits are discarded, matching `bignumber.js`'s unbounded `times`.
    pub fn mul(&self, other: &Self) -> Self {
        Self {
            unscaled: &self.unscaled * &other.unscaled,
            scale: self.scale.saturating_add(other.scale).min(MAX_SCALE),
        }
    }

    /// Divide, truncating toward zero at `places` decimal places.
    ///
    /// This is the only lossy operation in the type, and the only one whose
    /// result depends on a precision setting — `bignumber.js` bounds
    /// `dividedBy` by `DECIMAL_PLACES` and leaves every other operation exact.
    ///
    /// Returns `None` when `divisor` is zero. Callers in
    /// [`crate::math::fixed_point`] turn that into a typed error rather than
    /// letting a division by zero surface as a panic.
    ///
    /// ```
    /// use stellaragent::math::Decimal;
    ///
    /// let a = Decimal::parse("1").unwrap();
    /// let b = Decimal::parse("3").unwrap();
    /// assert_eq!(a.div_truncate(&b, 18).unwrap().to_fixed(18), "0.333333333333333333");
    /// ```
    pub fn div_truncate(&self, divisor: &Self, places: u32) -> Option<Self> {
        if divisor.is_zero() {
            return None;
        }
        let places = places.min(MAX_SCALE);

        // (a / 10^sa) / (b / 10^sb) truncated at `places` places
        //   = (a * 10^(places + sb)) / (b * 10^sa), integer-divided.
        // BigInt division truncates toward zero, which is ROUND_DOWN for both
        // signs — the same direction bignumber.js and Python's Decimal use.
        let numerator = &self.unscaled * pow10(places + divisor.scale);
        let denominator = &divisor.unscaled * pow10(self.scale);
        Some(Self {
            unscaled: numerator / denominator,
            scale: places,
        })
    }

    /// Drop digits beyond `places`, truncating toward zero.
    ///
    /// The equivalent of `BigNumber.decimalPlaces(places, ROUND_DOWN)`. A
    /// negative value that truncates away entirely becomes a genuine zero and
    /// **loses its sign** — see the module docs for why that differs from
    /// [`Decimal::to_fixed`].
    ///
    /// ```
    /// use stellaragent::math::Decimal;
    ///
    /// let v = Decimal::parse("-0.00001").unwrap();
    /// assert_eq!(v.truncate(4).to_fixed(4), "0.0000"); // truncate first: sign gone
    /// assert_eq!(v.to_fixed(4), "-0.0000");            // format directly: sign kept
    /// ```
    pub fn truncate(&self, places: u32) -> Self {
        if places >= self.scale {
            return self.rescale(places);
        }
        Self {
            unscaled: &self.unscaled / pow10(self.scale - places),
            scale: places,
        }
    }

    /// Round toward zero to an integer, as `BigNumber.integerValue(ROUND_DOWN)`.
    pub fn to_integer_truncated(&self) -> BigInt {
        self.truncate(0).unscaled
    }

    /// Format with exactly `places` decimal places, truncating toward zero.
    ///
    /// The equivalent of `BigNumber.toFixed(places, ROUND_DOWN)`, including
    /// its sign rule: the `-` is emitted whenever **the receiver** is
    /// negative, even if every digit that survives truncation is a zero.
    /// Never uses scientific notation, at any magnitude.
    ///
    /// ```
    /// use stellaragent::math::Decimal;
    ///
    /// let v = Decimal::parse("-8.239").unwrap();
    /// assert_eq!(v.to_fixed(0), "-8");
    /// assert_eq!(v.to_fixed(2), "-8.23"); // toward zero, not -8.24
    /// assert_eq!(v.to_fixed(7), "-8.2390000");
    /// ```
    pub fn to_fixed(&self, places: u32) -> String {
        let places = places.min(MAX_SCALE);
        let truncated = self.truncate(places).rescale(places);
        let digits = truncated.unscaled.magnitude().to_str_radix(10);
        let sign = if self.is_negative() { "-" } else { "" };

        if places == 0 {
            return format!("{sign}{digits}");
        }

        let places = places as usize;
        let padded = if digits.len() <= places {
            format!("{}{}", "0".repeat(places + 1 - digits.len()), digits)
        } else {
            digits
        };
        let split = padded.len() - places;
        format!("{sign}{}.{}", &padded[..split], &padded[split..])
    }

    /// Format at this value's own scale, without scientific notation.
    ///
    /// Round-trips through [`Decimal::parse`] for any value this type can hold.
    pub fn to_plain_string(&self) -> String {
        self.to_fixed(self.scale)
    }
}

impl Neg for Decimal {
    type Output = Decimal;
    fn neg(self) -> Decimal {
        Decimal {
            unscaled: -self.unscaled,
            scale: self.scale,
        }
    }
}

impl PartialEq for Decimal {
    /// Numeric equality: `1.00` equals `1`, and `-0` equals `0`.
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare signs first: a scale alignment between, say, 10^0 and
        // 10^9000 is a large allocation, and is pure waste when the operands
        // already disagree about which side of zero they are on.
        let by_sign = self.unscaled.sign().cmp(&other.unscaled.sign());
        if by_sign != Ordering::Equal {
            return by_sign;
        }
        let scale = self.scale.max(other.scale);
        self.rescale(scale)
            .unscaled
            .cmp(&other.rescale(scale).unscaled)
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_plain_string())
    }
}

impl From<BigInt> for Decimal {
    fn from(value: BigInt) -> Self {
        Self::from_bigint(value)
    }
}

macro_rules! decimal_from_integer {
    ($($ty:ty),* $(,)?) => {
        $(impl From<$ty> for Decimal {
            fn from(value: $ty) -> Self {
                Self::from_bigint(BigInt::from(value))
            }
        })*
    };
}

decimal_from_integer!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, isize, usize);

/// `10^n`, the one allocation every scale change needs.
fn pow10(n: u32) -> BigInt {
    BigInt::from(10u8).pow(n)
}

/// Parse an exponent's digits, rejecting anything that would overflow the
/// scale arithmetic rather than wrapping into a wrong-but-plausible value.
fn parse_exponent(digits: &str) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    let (negative, magnitude) = match digits.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, digits.strip_prefix('+').unwrap_or(digits)),
    };
    if magnitude.is_empty() || !magnitude.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Saturate rather than overflow: the caller's MAX_SCALE check rejects
    // anything this large anyway, and it would reject it for the right reason.
    let value: i64 = magnitude.parse().unwrap_or(i64::MAX / 2);
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).expect("valid decimal literal")
    }

    #[test]
    fn parses_the_shapes_bignumber_accepts() {
        assert_eq!(d("1").to_fixed(0), "1");
        assert_eq!(d(" 1 ").to_fixed(0), "1");
        assert_eq!(d("+1.5").to_fixed(2), "1.50");
        assert_eq!(d(".5").to_fixed(2), "0.50");
        assert_eq!(d("5.").to_fixed(2), "5.00");
        assert_eq!(d("1e5").to_fixed(0), "100000");
        assert_eq!(d("1E+5").to_fixed(0), "100000");
        assert_eq!(d("1.5e-3").to_fixed(4), "0.0015");
        assert_eq!(d("-0").to_fixed(2), "0.00");
    }

    #[test]
    fn rejects_what_is_not_a_decimal_literal() {
        for input in [
            "", " ", "abc", "1.2.3", ".", "-", "1e", "1e+", "1_000", "0x1f",
        ] {
            assert!(
                matches!(Decimal::parse(input), Err(ParseDecimalError::Syntax { .. })),
                "{input:?} should be a syntax error"
            );
        }
    }

    #[test]
    fn reports_non_finite_input_distinctly_from_a_typo() {
        for input in ["NaN", "Infinity", "-Infinity", "inf"] {
            assert!(
                matches!(
                    Decimal::parse(input),
                    Err(ParseDecimalError::NotFinite { .. })
                ),
                "{input:?} should be reported as non-finite"
            );
        }
    }

    #[test]
    fn rejects_an_unrepresentable_scale_instead_of_allocating() {
        assert!(matches!(
            Decimal::parse("1e-999999"),
            Err(ParseDecimalError::ScaleTooLarge { .. })
        ));
        assert!(matches!(
            Decimal::parse("1e999999"),
            Err(ParseDecimalError::ScaleTooLarge { .. })
        ));
    }

    #[test]
    fn add_sub_mul_are_exact() {
        assert_eq!(d("0.1").add(&d("0.2")).to_fixed(18), "0.300000000000000000");
        assert_eq!(
            d("0.1").sub(&d("0.2")).to_fixed(18),
            "-0.100000000000000000"
        );
        assert_eq!(d("0.1").mul(&d("0.2")).to_fixed(18), "0.020000000000000000");
        // 39 digits times 39 digits, with no significant-digit ceiling in sight.
        let big = d("170141183460469231731687303715884105727");
        assert_eq!(
            big.mul(&big).to_fixed(0),
            "28948022309329048855892746252171976962977213799489202546401021394546514198529"
        );
    }

    #[test]
    fn division_truncates_toward_zero_at_the_requested_place() {
        assert_eq!(
            d("1").div_truncate(&d("3"), 18).unwrap().to_fixed(18),
            "0.333333333333333333"
        );
        assert_eq!(
            d("-1").div_truncate(&d("3"), 18).unwrap().to_fixed(18),
            "-0.333333333333333333"
        );
        // Below the truncation point the quotient disappears entirely.
        assert_eq!(
            d("1")
                .div_truncate(&d("10000000000000000000"), 18)
                .unwrap()
                .to_fixed(18),
            "0.000000000000000000"
        );
        assert!(d("1").div_truncate(&d("0"), 18).is_none());
        assert!(d("1").div_truncate(&d("-0"), 18).is_none());
    }

    #[test]
    fn to_fixed_keeps_the_sign_that_truncate_discards() {
        let v = d("-0.00001");
        assert_eq!(v.to_fixed(4), "-0.0000");
        assert_eq!(v.truncate(4).to_fixed(4), "0.0000");
        assert_eq!(d("-0.5").to_fixed(0), "-0");
        assert_eq!(d("-0").to_fixed(0), "0");
    }

    #[test]
    fn comparison_ignores_scale() {
        assert_eq!(d("1.000"), d("1"));
        assert_eq!(d("-0"), d("0"));
        assert!(d("0.1") < d("0.2"));
        assert!(d("-5") < d("0.0000000001"));
    }

    #[test]
    fn never_emits_scientific_notation() {
        assert_eq!(d("1e-18").to_fixed(18), "0.000000000000000001");
        assert_eq!(d("1e30").to_fixed(2), "1000000000000000000000000000000.00");
    }
}
