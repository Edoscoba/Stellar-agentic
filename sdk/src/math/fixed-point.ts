/**
 * Deterministic fixed-point arithmetic for Stellar agent payment calculations.
 *
 * ## Why this module exists
 *
 * JavaScript's native `number` type uses IEEE 754 double-precision floating
 * point.  The x87/SSE2 FPU on x86 processors and the VFP/NEON unit on ARM
 * (M1/M2) can produce **different intermediate rounding results** for the same
 * mathematical expression, causing hash mismatches when agent bid scores are
 * used in on-chain operations.
 *
 * This module solves that by delegating every calculation to `bignumber.js`,
 * which implements arbitrary-precision decimal arithmetic in **pure
 * JavaScript** (no native bindings) and is therefore completely deterministic
 * across all CPU architectures and operating systems.
 *
 * ## Design rules
 * - **Never** use native `+`, `-`, `*`, `/` or `Math.*` on monetary/score
 *   values.  Always use the helpers exported here.
 * - Monetary amounts are always represented as **strings** at the API
 *   boundary so JS floats never touch them.
 * - On-chain values use `i128` stroops (integer).  Use `toStroops` /
 *   `fromStroops` to convert.
 * - `STROOP_SCALE` = 10 000 000 (1 XLM = 10^7 stroops), matching Stellar.
 *
 * @module fixed-point
 */

import BigNumber from 'bignumber.js';

// ─── Global BigNumber configuration ─────────────────────────────────────────
//
// ROUND_DOWN prevents any unintentional rounding-up that could cause an
// agent to exceed its spend limit.  DECIMAL_PLACES is set high enough for
// bid score calculations (rate, reputation, latency etc.).
//
BigNumber.config({
  DECIMAL_PLACES: 18,
  ROUNDING_MODE: BigNumber.ROUND_DOWN,
  EXPONENTIAL_AT: [-18, 36],
});

// ─── Constants ────────────────────────────────────────────────────────────────

/** Stellar stroop denominator: 1 XLM = 10^7 stroops */
export const STROOP_SCALE = new BigNumber('10000000');

/** Basis-point denominator (100.00% = 10 000 bps) */
export const BPS_SCALE = new BigNumber('10000');

// ─── Core helpers ─────────────────────────────────────────────────────────────

/**
 * Wrap any string/number into a BigNumber, throwing immediately if the value
 * is not a valid finite decimal.  Using this at every entry point prevents
 * `NaN` / `Infinity` from silently propagating through calculations.
 */
export function bn(value: string | number | bigint | BigNumber): BigNumber {
  const x = new BigNumber(typeof value === 'bigint' ? value.toString() : value);
  if (!x.isFinite()) {
    throw new RangeError(`FixedPoint: value "${value}" is not a finite decimal`);
  }
  return x;
}

// ─── Arithmetic ──────────────────────────────────────────────────────────────

/** Deterministic addition:  a + b */
export function add(a: string | BigNumber, b: string | BigNumber): BigNumber {
  return bn(a).plus(bn(b));
}

/** Deterministic subtraction:  a − b */
export function sub(a: string | BigNumber, b: string | BigNumber): BigNumber {
  return bn(a).minus(bn(b));
}

/** Deterministic multiplication:  a × b */
export function mul(a: string | BigNumber, b: string | BigNumber): BigNumber {
  return bn(a).times(bn(b));
}

/**
 * Deterministic division:  a ÷ b
 * Uses ROUND_DOWN (truncation) so the result can never exceed the true value,
 * which is the safe direction for spend-limit comparisons.
 */
export function div(a: string | BigNumber, b: string | BigNumber): BigNumber {
  const divisor = bn(b);
  if (divisor.isZero()) throw new RangeError('FixedPoint: division by zero');
  return bn(a).dividedBy(divisor);
}

/**
 * Percentage of `value` out of `total` (0 – 100), rounded down to
 * `decimalPlaces` (default 4).  Safe for progress-bar rendering.
 *
 * @example
 * pct('1.45', '5.00')  // → BigNumber('29.0000')
 */
export function pct(
  value: string | BigNumber,
  total: string | BigNumber,
  decimalPlaces = 4,
): BigNumber {
  const t = bn(total);
  if (t.isZero()) return new BigNumber(0);
  return bn(value).dividedBy(t).times(100).decimalPlaces(decimalPlaces, BigNumber.ROUND_DOWN);
}

/** Clamp a value to [min, max] */
export function clamp(
  value: string | BigNumber,
  min: string | BigNumber,
  max: string | BigNumber,
): BigNumber {
  return BigNumber.minimum(BigNumber.maximum(bn(value), bn(min)), bn(max));
}

/** Sum an array of string decimal values deterministically */
export function sumStrings(values: string[]): BigNumber {
  return values.reduce((acc, v) => acc.plus(bn(v)), new BigNumber(0));
}

// ─── Stroop conversions ──────────────────────────────────────────────────────

/**
 * Convert a human-readable decimal amount (e.g. "1.50") to Stellar stroops
 * as a `bigint` (i128-compatible).  Truncates sub-stroop fractions.
 *
 * @example
 * toStroops('1.5000001')  // → 15000001n
 */
export function toStroops(amount: string): bigint {
  return BigInt(bn(amount).times(STROOP_SCALE).integerValue(BigNumber.ROUND_DOWN).toFixed(0));
}

/**
 * Convert on-chain stroops (i128 represented as `bigint`) to a
 * human-readable decimal string with `decimalPlaces` precision.
 *
 * @example
 * fromStroops(15000001n, 7)  // → '1.5000001'
 */
export function fromStroops(stroops: bigint, decimalPlaces = 7): string {
  return bn(stroops.toString())
    .dividedBy(STROOP_SCALE)
    .toFixed(decimalPlaces, BigNumber.ROUND_DOWN);
}

// ─── Formatting ──────────────────────────────────────────────────────────────

/**
 * Format a decimal amount for display, rounding down to `places` decimal
 * places.  Never uses `Number.toFixed` to avoid float coercion.
 *
 * @example
 * fmt('8.2300001', 2)  // → '8.23'
 */
export function fmt(value: string | BigNumber, places = 2): string {
  return bn(value).toFixed(places, BigNumber.ROUND_DOWN);
}

/**
 * Stringify a BigNumber result back to a plain decimal string for storage or
 * wire transmission.  Uses enough precision to avoid any scientific notation.
 */
export function toStr(value: BigNumber, places = 7): string {
  return value.toFixed(places, BigNumber.ROUND_DOWN);
}

// ─── Comparison helpers ──────────────────────────────────────────────────────

export function gt(a: string | BigNumber, b: string | BigNumber): boolean {
  return bn(a).isGreaterThan(bn(b));
}

export function gte(a: string | BigNumber, b: string | BigNumber): boolean {
  return bn(a).isGreaterThanOrEqualTo(bn(b));
}

export function lt(a: string | BigNumber, b: string | BigNumber): boolean {
  return bn(a).isLessThan(bn(b));
}

export function lte(a: string | BigNumber, b: string | BigNumber): boolean {
  return bn(a).isLessThanOrEqualTo(bn(b));
}

export function eq(a: string | BigNumber, b: string | BigNumber): boolean {
  return bn(a).isEqualTo(bn(b));
}

export function isZero(a: string | BigNumber): boolean {
  return bn(a).isZero();
}

export function isPositive(a: string | BigNumber): boolean {
  return bn(a).isPositive() && !bn(a).isZero();
}

// ─── Re-export BigNumber for callers that need raw access ────────────────────
export { BigNumber };
