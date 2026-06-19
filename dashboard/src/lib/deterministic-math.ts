/**
 * Deterministic math helpers for the dashboard UI.
 *
 * Wraps `bignumber.js` so the dashboard never uses native JS float arithmetic
 * on monetary values or on any derived number that could be compared or hashed
 * on-chain.  Results are identical on x86 and ARM (M1/M2).
 *
 * Mirrors the API surface of `@stellaragent/sdk/math` so call sites are
 * consistent across the repo.
 */

import BigNumber from 'bignumber.js';

// ─── Global configuration (mirrors sdk/src/math/fixed-point.ts) ──────────────
BigNumber.config({
  DECIMAL_PLACES: 18,
  ROUNDING_MODE: BigNumber.ROUND_DOWN,
  EXPONENTIAL_AT: [-18, 36],
});

// ─── Core ─────────────────────────────────────────────────────────────────────

/** Parse a decimal string into a BigNumber.  Throws on NaN / Infinity. */
export function bn(value: string | number | BigNumber): BigNumber {
  const x = new BigNumber(value);
  if (!x.isFinite()) {
    throw new RangeError(`deterministic-math: "${value}" is not a finite decimal`);
  }
  return x;
}

// ─── Arithmetic ───────────────────────────────────────────────────────────────

export function add(a: string | BigNumber, b: string | BigNumber): BigNumber {
  return bn(a).plus(bn(b));
}

export function sub(a: string | BigNumber, b: string | BigNumber): BigNumber {
  return bn(a).minus(bn(b));
}

export function mul(a: string | BigNumber, b: string | BigNumber): BigNumber {
  return bn(a).times(bn(b));
}

export function div(a: string | BigNumber, b: string | BigNumber): BigNumber {
  const divisor = bn(b);
  if (divisor.isZero()) throw new RangeError('deterministic-math: division by zero');
  return bn(a).dividedBy(divisor);
}

// ─── Currency helpers ─────────────────────────────────────────────────────────

/**
 * Sum an array of decimal string amounts (e.g. payment amounts).
 * Returns a BigNumber — call `.toFixed(n)` to display.
 */
export function sumAmounts(amounts: string[]): BigNumber {
  return amounts.reduce((acc, v) => acc.plus(bn(v)), new BigNumber(0));
}

/**
 * Format a decimal string for display without touching JS floats.
 * Rounds DOWN (safe for spend/budget display).
 *
 * @example
 * fmt('8.2399999', 2)  // → '8.23'
 */
export function fmt(value: string | BigNumber, places = 2): string {
  return bn(value).toFixed(places, BigNumber.ROUND_DOWN);
}

// ─── Percentage helpers ───────────────────────────────────────────────────────

/**
 * Compute percentage (0 – 100) of `value` out of `total`, rounded down.
 * Returns a plain JS `number` suitable for passing to Recharts / ProgressBar
 * props that only accept `number`.
 *
 * @example
 * pctNumber('1.45', '5.00')  // → 29   (as number, safe for ProgressBar max=100)
 */
export function pctNumber(value: string, total: string, decimalPlaces = 4): number {
  if (bn(total).isZero()) return 0;
  return bn(value)
    .dividedBy(bn(total))
    .times(100)
    .decimalPlaces(decimalPlaces, BigNumber.ROUND_DOWN)
    .toNumber();
}

/**
 * Like `pctNumber` but returns a BigNumber for further computation before
 * converting to a display value.
 */
export function pctBN(value: string | BigNumber, total: string | BigNumber): BigNumber {
  const t = bn(total);
  if (t.isZero()) return new BigNumber(0);
  return bn(value).dividedBy(t).times(100);
}

/**
 * Clamp a `number` to [0, 100].  For ProgressBar `value` props.
 * Operates on plain numbers because it is only for clamping already-computed
 * display values — not involved in any on-chain calculation.
 */
export function clamp100(n: number): number {
  return Math.min(Math.max(n, 0), 100);
}
