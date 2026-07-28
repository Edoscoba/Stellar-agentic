import { describe, it, expect } from 'vitest';

import {
  bn,
  add,
  sub,
  mul,
  div,
  pct,
  clamp,
  sumStrings,
  toStroops,
  fromStroops,
  fmt,
  toStr,
  gt,
  gte,
  lt,
  lte,
  eq,
  isZero,
  isPositive,
  STROOP_SCALE,
  BPS_SCALE,
  BigNumber,
} from '../fixed-point.js';

// ─── Constants ───────────────────────────────────────────────────────────────

describe('constants', () => {
  it('STROOP_SCALE is 10^7, matching Stellar', () => {
    expect(STROOP_SCALE.toFixed(0)).toBe('10000000');
  });

  it('BPS_SCALE is 10^4 (100.00% = 10000 bps)', () => {
    expect(BPS_SCALE.toFixed(0)).toBe('10000');
  });
});

// ─── bn() ────────────────────────────────────────────────────────────────────

describe('bn', () => {
  it('accepts decimal strings', () => {
    expect(bn('1.25').toFixed(2)).toBe('1.25');
  });

  it('accepts numbers', () => {
    expect(bn(42).toFixed(0)).toBe('42');
  });

  it('accepts bigints without going through Number', () => {
    // 2^63 - 1 is not representable as a JS number; going via `Number` would
    // lose the low bits. bn() stringifies bigints precisely for this reason.
    expect(bn(9223372036854775807n).toFixed(0)).toBe('9223372036854775807');
  });

  it('accepts an existing BigNumber', () => {
    expect(bn(new BigNumber('3.5')).toFixed(1)).toBe('3.5');
  });

  it('preserves precision far beyond IEEE-754 doubles', () => {
    const huge = '123456789012345678901234567890.123456789012345678';
    expect(bn(huge).toFixed(18)).toBe('123456789012345678901234567890.123456789012345678');
  });

  it.each([
    ['not-a-number', 'abc'],
    ['empty string', ''],
    ['whitespace', '   '],
    ['trailing garbage', '1.2.3'],
  ])('throws RangeError on non-finite input: %s', (_label, value) => {
    expect(() => bn(value)).toThrow(RangeError);
  });

  it('throws RangeError on Infinity', () => {
    expect(() => bn(Infinity)).toThrow(RangeError);
    expect(() => bn(-Infinity)).toThrow(RangeError);
  });

  it('throws RangeError on NaN', () => {
    expect(() => bn(NaN)).toThrow(RangeError);
  });

  it('names the offending value in the error message', () => {
    expect(() => bn('abc')).toThrow(/value "abc" is not a finite decimal/);
  });
});

// ─── Arithmetic ──────────────────────────────────────────────────────────────

describe('add / sub / mul', () => {
  it('adds without float error', () => {
    // The canonical float failure: 0.1 + 0.2 === 0.30000000000000004
    expect(add('0.1', '0.2').toFixed(1)).toBe('0.3');
    expect(add('0.1', '0.2').isEqualTo(bn('0.3'))).toBe(true);
  });

  it('subtracts without float error', () => {
    expect(sub('0.3', '0.1').isEqualTo(bn('0.2'))).toBe(true);
  });

  it('multiplies without float error', () => {
    // 0.1 * 0.2 === 0.020000000000000004 in native floats
    expect(mul('0.1', '0.2').isEqualTo(bn('0.02'))).toBe(true);
  });

  it('accepts BigNumber operands as well as strings', () => {
    expect(add(bn('1'), bn('2')).toFixed(0)).toBe('3');
    expect(sub(bn('5'), '2').toFixed(0)).toBe('3');
    expect(mul('3', bn('4')).toFixed(0)).toBe('12');
  });

  it('propagates bn() validation to operands', () => {
    expect(() => add('1', 'oops')).toThrow(RangeError);
    expect(() => sub('oops', '1')).toThrow(RangeError);
    expect(() => mul('1', '')).toThrow(RangeError);
  });

  it('handles negative operands', () => {
    expect(add('-1.5', '0.5').toFixed(1)).toBe('-1.0');
    expect(mul('-2', '-3').toFixed(0)).toBe('6');
  });
});

describe('div', () => {
  it('divides exactly when the result terminates', () => {
    expect(div('10', '4').toFixed(2)).toBe('2.50');
  });

  it('truncates (ROUND_DOWN) rather than rounding at 18 decimal places', () => {
    // 1/3 = 0.333... → truncated at DECIMAL_PLACES: 18
    expect(div('1', '3').toFixed(18)).toBe('0.333333333333333333');
    // 2/3 = 0.666...7 if rounded half-up; ROUND_DOWN must give ...6
    expect(div('2', '3').toFixed(18)).toBe('0.666666666666666666');
  });

  it('throws RangeError on division by zero', () => {
    expect(() => div('1', '0')).toThrow(RangeError);
    expect(() => div('1', '0')).toThrow(/division by zero/);
  });

  it('throws on division by "0.0" and "-0" too', () => {
    expect(() => div('1', '0.0')).toThrow(RangeError);
    expect(() => div('1', '-0')).toThrow(RangeError);
  });

  it('allows a zero numerator', () => {
    expect(div('0', '5').isZero()).toBe(true);
  });

  it('floors at the configured 18-decimal-place precision', () => {
    // DECIMAL_PLACES: 18 applies to division, so a quotient smaller than
    // 1e-18 truncates all the way to zero rather than carrying on. This is a
    // real boundary of the module: callers must not use div() to compare
    // quantities below 1e-18. Asserted so a config change is caught here.
    expect(div('1', '1000000000000000000').toFixed(18)).toBe('0.000000000000000001');
    expect(div('1', '10000000000000000000').isZero()).toBe(true);
  });

  it('truncates toward zero for negative results', () => {
    // ROUND_DOWN is truncation toward zero, so a negative quotient
    // truncates upward in absolute-value terms.
    expect(div('-2', '3').toFixed(18)).toBe('-0.666666666666666666');
  });
});

// ─── pct ─────────────────────────────────────────────────────────────────────

describe('pct', () => {
  it('computes a percentage to 4 decimal places by default', () => {
    expect(pct('1.45', '5.00').toFixed(4)).toBe('29.0000');
  });

  it('returns exactly zero when the total is zero (no divide-by-zero throw)', () => {
    // Deliberately different from div(): a zero total is a normal state for a
    // progress bar (nothing budgeted yet), not a programming error.
    expect(pct('1', '0').isZero()).toBe(true);
    expect(pct('0', '0').isZero()).toBe(true);
  });

  it('truncates rather than rounding', () => {
    // 1/3 → 33.3333...% ; rounding would give 33.3333 either way, so use a
    // case where the 5th decimal is ≥5: 2/3 → 66.66666...%
    expect(pct('2', '3').toFixed(4)).toBe('66.6666');
  });

  it('honours a custom decimalPlaces argument', () => {
    expect(pct('2', '3', 0).toFixed(0)).toBe('66');
    expect(pct('2', '3', 2).toFixed(2)).toBe('66.66');
    expect(pct('2', '3', 8).toFixed(8)).toBe('66.66666666');
  });

  it('can exceed 100 when value > total (no implicit clamping)', () => {
    expect(pct('10', '5').toFixed(4)).toBe('200.0000');
  });

  it('validates its inputs', () => {
    expect(() => pct('abc', '1')).toThrow(RangeError);
    expect(() => pct('1', 'abc')).toThrow(RangeError);
  });
});

// ─── clamp ───────────────────────────────────────────────────────────────────

describe('clamp', () => {
  it('returns the value untouched when inside the range', () => {
    expect(clamp('5', '0', '10').toFixed(0)).toBe('5');
  });

  it('clamps below the minimum', () => {
    expect(clamp('-3', '0', '10').toFixed(0)).toBe('0');
  });

  it('clamps above the maximum', () => {
    expect(clamp('99', '0', '10').toFixed(0)).toBe('10');
  });

  it('is inclusive at the exact lower boundary', () => {
    expect(clamp('0', '0', '10').toFixed(0)).toBe('0');
  });

  it('is inclusive at the exact upper boundary', () => {
    expect(clamp('10', '0', '10').toFixed(0)).toBe('10');
  });

  it('handles a degenerate range where min === max', () => {
    expect(clamp('7', '3', '3').toFixed(0)).toBe('3');
  });

  it('resolves an inverted range (min > max) to max, because max wins last', () => {
    // max(7, 10) = 10, then min(10, 0) = 0. Documented here so a future
    // refactor that changes the operator order fails loudly.
    expect(clamp('7', '10', '0').toFixed(0)).toBe('0');
  });

  it('clamps sub-stroop precision without losing it', () => {
    expect(clamp('0.00000001', '0', '1').toFixed(8)).toBe('0.00000001');
  });
});

// ─── sumStrings ──────────────────────────────────────────────────────────────

describe('sumStrings', () => {
  it('returns zero for an empty array', () => {
    expect(sumStrings([]).isZero()).toBe(true);
  });

  it('sums a single element', () => {
    expect(sumStrings(['1.5']).toFixed(1)).toBe('1.5');
  });

  it('sums many values without accumulating float drift', () => {
    // 10 × 0.1 === 0.9999999999999999 with native floats.
    const tenth = Array.from({ length: 10 }, () => '0.1');
    expect(sumStrings(tenth).isEqualTo(bn('1'))).toBe(true);
  });

  it('is order-independent (a determinism prerequisite)', () => {
    const values = ['0.1', '0.02', '3', '0.000004'];
    const forward = sumStrings(values).toFixed(7);
    const reversed = sumStrings([...values].reverse()).toFixed(7);
    expect(forward).toBe(reversed);
    expect(forward).toBe('3.1200040');
  });

  it('handles negatives', () => {
    expect(sumStrings(['5', '-2', '-1']).toFixed(0)).toBe('2');
  });

  it('rejects an invalid element', () => {
    expect(() => sumStrings(['1', 'abc'])).toThrow(RangeError);
  });
});

// ─── Stroop conversions ──────────────────────────────────────────────────────

describe('toStroops', () => {
  it('converts whole units', () => {
    expect(toStroops('1')).toBe(10000000n);
  });

  it('converts the documented example', () => {
    expect(toStroops('1.5000001')).toBe(15000001n);
  });

  it('converts zero', () => {
    expect(toStroops('0')).toBe(0n);
  });

  it('truncates sub-stroop fractions rather than rounding up', () => {
    // The 8th decimal is below stroop resolution. Rounding up here would let
    // an agent spend one stroop more than its limit allows.
    expect(toStroops('1.50000019')).toBe(15000001n);
    expect(toStroops('0.00000009')).toBe(0n);
  });

  it('handles negative amounts by truncating toward zero', () => {
    expect(toStroops('-1.50000019')).toBe(-15000001n);
  });

  it('handles amounts far beyond Number.MAX_SAFE_INTEGER in stroops', () => {
    // 10^12 XLM = 10^19 stroops, which exceeds 2^53 by ~3 orders of magnitude.
    expect(toStroops('1000000000000')).toBe(10000000000000000000n);
  });

  it('rejects invalid input', () => {
    expect(() => toStroops('abc')).toThrow(RangeError);
  });
});

describe('fromStroops', () => {
  it('converts the documented example', () => {
    expect(fromStroops(15000001n, 7)).toBe('1.5000001');
  });

  it('defaults to 7 decimal places', () => {
    expect(fromStroops(15000001n)).toBe('1.5000001');
  });

  it('pads to the requested precision', () => {
    expect(fromStroops(10000000n)).toBe('1.0000000');
    expect(fromStroops(0n)).toBe('0.0000000');
  });

  it('truncates when asked for fewer places than the value carries', () => {
    expect(fromStroops(15000009n, 2)).toBe('1.50');
  });

  it('handles negative stroops', () => {
    expect(fromStroops(-15000001n)).toBe('-1.5000001');
  });

  it('never emits scientific notation for very small values', () => {
    expect(fromStroops(1n)).toBe('0.0000001');
  });

  it('handles i128-scale values without precision loss', () => {
    const big = 170141183460469231731687303715884105727n; // 2^127 - 1
    expect(fromStroops(big, 7)).toBe('17014118346046923173168730371588.4105727');
  });
});

describe('toStroops / fromStroops round-trip', () => {
  it.each([
    '0',
    '1',
    '0.0000001',
    '1.5000001',
    '123456.7891234',
    '999999999.9999999',
    '-42.0000001',
  ])('round-trips %s at stroop precision', (amount) => {
    expect(fromStroops(toStroops(amount), 7)).toBe(bn(amount).toFixed(7));
  });

  it('round-trips every stroop in a small exhaustive range', () => {
    for (let i = 0n; i < 1000n; i++) {
      expect(toStroops(fromStroops(i, 7))).toBe(i);
    }
  });

  it('loses only the sub-stroop remainder, as documented', () => {
    // Input carries 9 decimals; stroops carry 7. The extra two are dropped.
    expect(fromStroops(toStroops('1.123456789'), 7)).toBe('1.1234567');
  });
});

// ─── Formatting ──────────────────────────────────────────────────────────────

describe('fmt', () => {
  it('rounds down (truncates) rather than rounding half-up', () => {
    // The behaviour the module doc promises: 8.2399999 must NOT become 8.24.
    expect(fmt('8.2399999', 2)).toBe('8.23');
  });

  it('truncates at the exact half, where round-half-up would go up', () => {
    expect(fmt('8.235', 2)).toBe('8.23');
    expect(fmt('0.5', 0)).toBe('0');
    expect(fmt('1.5', 0)).toBe('1');
  });

  it('matches the documented example', () => {
    expect(fmt('8.2300001', 2)).toBe('8.23');
  });

  it('defaults to 2 places', () => {
    expect(fmt('1.239')).toBe('1.23');
  });

  it('pads short values up to the requested precision', () => {
    expect(fmt('1.2', 4)).toBe('1.2000');
    expect(fmt('7', 2)).toBe('7.00');
  });

  it('truncates toward zero for negatives', () => {
    // ROUND_DOWN is truncation toward zero, not floor: -8.239 → -8.23.
    expect(fmt('-8.239', 2)).toBe('-8.23');
  });

  it('accepts a BigNumber', () => {
    expect(fmt(bn('3.14159'), 3)).toBe('3.141');
  });

  it('never emits scientific notation for tiny values', () => {
    expect(fmt('0.0000000001', 12)).toBe('0.000000000100');
  });

  it('validates its input', () => {
    expect(() => fmt('abc')).toThrow(RangeError);
  });
});

describe('toStr', () => {
  it('defaults to 7 decimal places (stroop precision)', () => {
    expect(toStr(bn('1.5'))).toBe('1.5000000');
  });

  it('truncates rather than rounding', () => {
    expect(toStr(bn('1.99999999'), 7)).toBe('1.9999999');
  });

  it('honours an explicit precision', () => {
    expect(toStr(bn('1.23456789'), 3)).toBe('1.234');
    expect(toStr(bn('1.5'), 0)).toBe('1');
  });

  it('produces plain decimal strings for values that would otherwise go exponential', () => {
    // EXPONENTIAL_AT: [-18, 36] would make toString() switch to scientific
    // notation here; toFixed() must not.
    const tiny = bn('0.0000000000000000000001');
    expect(toStr(tiny, 25)).toBe('0.0000000000000000000001000');
    expect(toStr(tiny, 25)).not.toMatch(/e/i);
  });
});

// ─── Comparisons ─────────────────────────────────────────────────────────────

describe('comparison helpers', () => {
  it('gt', () => {
    expect(gt('2', '1')).toBe(true);
    expect(gt('1', '1')).toBe(false);
    expect(gt('1', '2')).toBe(false);
  });

  it('gte', () => {
    expect(gte('2', '1')).toBe(true);
    expect(gte('1', '1')).toBe(true);
    expect(gte('0', '1')).toBe(false);
  });

  it('lt', () => {
    expect(lt('1', '2')).toBe(true);
    expect(lt('1', '1')).toBe(false);
    expect(lt('2', '1')).toBe(false);
  });

  it('lte', () => {
    expect(lte('1', '2')).toBe(true);
    expect(lte('1', '1')).toBe(true);
    expect(lte('2', '1')).toBe(false);
  });

  it('eq treats differently-spelled equal values as equal', () => {
    expect(eq('1', '1')).toBe(true);
    expect(eq('1.0', '1')).toBe(true);
    expect(eq('1.000000', '1')).toBe(true);
    expect(eq('0', '-0')).toBe(true);
    expect(eq('1', '2')).toBe(false);
  });

  it('compares at full precision, not float precision', () => {
    // These two differ in the 20th decimal — indistinguishable as doubles.
    const a = '1.00000000000000000001';
    const b = '1.00000000000000000002';
    expect(eq(a, b)).toBe(false);
    expect(lt(a, b)).toBe(true);
  });

  it('accepts BigNumber operands', () => {
    expect(gt(bn('2'), bn('1'))).toBe(true);
  });

  it('validates operands', () => {
    expect(() => gt('abc', '1')).toThrow(RangeError);
  });
});

describe('isZero / isPositive', () => {
  it('isZero recognises every spelling of zero', () => {
    expect(isZero('0')).toBe(true);
    expect(isZero('0.0')).toBe(true);
    expect(isZero('-0')).toBe(true);
    expect(isZero('0.0000000')).toBe(true);
    expect(isZero('0.0000001')).toBe(false);
  });

  it('isPositive excludes zero (strictly greater than zero)', () => {
    expect(isPositive('1')).toBe(true);
    expect(isPositive('0.0000001')).toBe(true);
    expect(isPositive('0')).toBe(false);
    expect(isPositive('-0')).toBe(false);
    expect(isPositive('-1')).toBe(false);
  });

  it('validates input', () => {
    expect(() => isZero('abc')).toThrow(RangeError);
    expect(() => isPositive('abc')).toThrow(RangeError);
  });
});

// ─── The determinism guarantee itself ────────────────────────────────────────

describe('determinism guarantee', () => {
  it('produces byte-identical output across repeated evaluation', () => {
    const run = () =>
      toStr(
        div(mul(add('0.1', '0.2'), '7.7777777'), '3'),
        18,
      );
    const first = run();
    for (let i = 0; i < 100; i++) expect(run()).toBe(first);
    expect(first).toBe('0.777777770000000000');
  });

  it('never routes a monetary value through a JS float', () => {
    // If any helper coerced through Number, this 20-significant-digit value
    // would come back rounded at the 17th digit.
    const precise = '12345678901234567.89012345678901234567';
    expect(toStr(add(precise, '0'), 20)).toBe('12345678901234567.89012345678901234567');
  });
});
