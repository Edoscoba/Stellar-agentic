/**
 * Cross-language determinism suite — TypeScript half.
 *
 * `fixtures/determinism.json` is generated from *this* implementation by
 * `scripts/generate-fixtures.ts`, and asserted against by both this suite and
 * `python/tests/test_determinism.py`.
 *
 * Running the fixtures back through TypeScript is not circular busywork: it
 * pins the numeric contract. If someone changes `fixed-point.ts` or `bid.ts`
 * without regenerating, this suite fails and says so — rather than the change
 * silently landing and only breaking the Python side later. `pnpm
 * fixtures:check` enforces the same thing from the other direction.
 *
 * Comparison is string equality throughout. Numeric closeness would defeat the
 * purpose: the whole reason these modules exist is that "close enough" is what
 * makes x86 and ARM disagree about a bid score.
 */

import { describe, it, expect } from 'vitest';
import { readFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';

import BigNumber from 'bignumber.js';

import * as fp from '../fixed-point.js';
import * as bid from '../bid.js';
import type { AgentBid, BidWeights, ScoredBid } from '../bid.js';

// ─── Load ────────────────────────────────────────────────────────────────────

// vitest runs with cwd at the package root; the fixtures are shared at the
// repo root so pytest can reach the identical file.
const FIXTURES_PATH = resolve(process.cwd(), '../../fixtures/determinism.json');

if (!existsSync(FIXTURES_PATH)) {
  throw new Error(
    `Shared fixtures missing at ${FIXTURES_PATH}. Generate with: pnpm fixtures:generate`,
  );
}

interface FixedPointCase {
  id: string;
  fn: string;
  args: (string | number)[];
  kind: 'decimal' | 'string' | 'int' | 'bool';
  throws?: true;
  expect?: string;
}

interface ScoreBidCase {
  id: string;
  bid: AgentBid;
  maxBid: string;
  maxLatency: string;
  weights: string;
  expect: { score: string; breakdown: Record<string, string> };
}

interface RankCase {
  id: string;
  bids: AgentBid[];
  weights: string;
  expect: { agentAddress: string; score: string }[];
}

interface SpendCase {
  id: string;
  spent: string;
  limit: string;
  amount: string;
  expect: { withinLimit: boolean; remaining: string };
}

interface Fixtures {
  version: number;
  weightSets: Record<string, BidWeights>;
  fixedPoint: FixedPointCase[];
  bid: {
    scoreBid: ScoreBidCase[];
    rankBids: RankCase[];
    spendLimit: SpendCase[];
    invalidWeights: { id: string; weights: BidWeights }[];
  };
}

const FIXTURES: Fixtures = JSON.parse(readFileSync(FIXTURES_PATH, 'utf8'));

// ─── Dispatch — mirrors python/tests/test_determinism.py ─────────────────────

const FNS: Record<string, (...args: never[]) => unknown> = {
  bn: fp.bn,
  add: fp.add,
  sub: fp.sub,
  mul: fp.mul,
  div: fp.div,
  pct: fp.pct,
  clamp: fp.clamp,
  sumStrings: fp.sumStrings,
  toStroops: fp.toStroops,
  fromStroops: ((s: string, dp: number) => fp.fromStroops(BigInt(s), dp)) as never,
  fmt: fp.fmt,
  toStr: ((v: string, places: number) => fp.toStr(fp.bn(v), places)) as never,
  gt: fp.gt,
  gte: fp.gte,
  lt: fp.lt,
  lte: fp.lte,
  eq: fp.eq,
  isZero: fp.isZero,
  isPositive: fp.isPositive,
};

function canonical(result: unknown, kind: FixedPointCase['kind']): string {
  switch (kind) {
    case 'decimal':
      return (result as BigNumber).toFixed(18, BigNumber.ROUND_DOWN);
    case 'int':
      return (result as bigint).toString();
    case 'bool':
      return String(result);
    default:
      return String(result);
  }
}

const invoke = (testCase: FixedPointCase): unknown => {
  const fn = FNS[testCase.fn];
  if (!fn) throw new Error(`Fixture references unknown function: ${testCase.fn}`);
  const args = testCase.fn === 'sumStrings' ? [testCase.args] : testCase.args;
  return (fn as (...a: unknown[]) => unknown)(...args);
};

// ─── Sanity ──────────────────────────────────────────────────────────────────

describe('fixture file', () => {
  it('is present and populated', () => {
    // A silently empty file would make every test below vacuous.
    expect(FIXTURES.version).toBe(1);
    expect(FIXTURES.fixedPoint.length).toBeGreaterThan(300);
    expect(FIXTURES.bid.scoreBid.length).toBeGreaterThan(100);
    expect(FIXTURES.bid.rankBids.length).toBeGreaterThan(10);
    expect(FIXTURES.bid.spendLimit.length).toBeGreaterThan(10);
  });

  it('covers both value and throwing cases', () => {
    expect(FIXTURES.fixedPoint.some((c) => c.throws)).toBe(true);
    expect(FIXTURES.fixedPoint.some((c) => !c.throws)).toBe(true);
  });

  it('exercises every exported fixed-point function', () => {
    // Guards against a new export slipping in without fixture coverage, which
    // would leave a cross-language gap nothing detects.
    const covered = new Set(FIXTURES.fixedPoint.map((c) => c.fn));
    for (const name of Object.keys(FNS)) {
      expect(covered, `${name} has no fixture coverage`).toContain(name);
    }
  });
});

// ─── fixed-point ─────────────────────────────────────────────────────────────

describe('fixed-point fixtures', () => {
  const valueCases = FIXTURES.fixedPoint.filter((c) => !c.throws);
  const throwCases = FIXTURES.fixedPoint.filter((c) => c.throws);

  it.each(valueCases.map((c) => [c.id, c] as const))('%s', (_id, testCase) => {
    expect(canonical(invoke(testCase), testCase.kind)).toBe(testCase.expect);
  });

  it.each(throwCases.map((c) => [c.id, c] as const))('%s throws', (_id, testCase) => {
    expect(() => invoke(testCase)).toThrow(RangeError);
  });
});

// ─── scoreBid ────────────────────────────────────────────────────────────────

describe('scoreBid fixtures', () => {
  it.each(FIXTURES.bid.scoreBid.map((c) => [c.id, c] as const))('%s', (_id, testCase) => {
    const scored = bid.scoreBid(
      testCase.bid,
      testCase.maxBid,
      testCase.maxLatency,
      FIXTURES.weightSets[testCase.weights]!,
    );
    expect(scored.score).toBe(testCase.expect.score);
    expect(scored.breakdown).toEqual(testCase.expect.breakdown);
  });
});

// ─── rankBids ────────────────────────────────────────────────────────────────

describe('rankBids fixtures', () => {
  const simplify = (ranked: ScoredBid[]) =>
    ranked.map((r) => ({ agentAddress: r.agentAddress, score: r.score }));

  it.each(FIXTURES.bid.rankBids.map((c) => [c.id, c] as const))('%s', (_id, testCase) => {
    const ranked = bid.rankBids(testCase.bids, FIXTURES.weightSets[testCase.weights]!);
    expect(simplify(ranked)).toEqual(testCase.expect);
  });

  it.each(FIXTURES.bid.rankBids.map((c) => [c.id, c] as const))(
    '%s is order-independent',
    (_id, testCase) => {
      const weights = FIXTURES.weightSets[testCase.weights]!;
      const forward = bid.rankBids(testCase.bids, weights);
      const reversed = bid.rankBids([...testCase.bids].reverse(), weights);
      expect(simplify(reversed)).toEqual(simplify(forward));
    },
  );

  it.each(FIXTURES.bid.rankBids.map((c) => [c.id, c] as const))(
    '%s agrees with selectBestBid',
    (_id, testCase) => {
      const best = bid.selectBestBid(testCase.bids, FIXTURES.weightSets[testCase.weights]!);
      if (testCase.expect.length === 0) {
        expect(best).toBeNull();
      } else {
        expect(best!.agentAddress).toBe(testCase.expect[0]!.agentAddress);
        expect(best!.score).toBe(testCase.expect[0]!.score);
      }
    },
  );
});

// ─── Spend limits ────────────────────────────────────────────────────────────

describe('spend-limit fixtures', () => {
  it.each(FIXTURES.bid.spendLimit.map((c) => [c.id, c] as const))('%s', (_id, testCase) => {
    expect(bid.isWithinSpendLimit(testCase.spent, testCase.limit, testCase.amount))
      .toBe(testCase.expect.withinLimit);
    expect(bid.remainingBudget(testCase.spent, testCase.limit))
      .toBe(testCase.expect.remaining);
  });
});

// ─── Invalid weights ─────────────────────────────────────────────────────────

describe('invalid-weight fixtures', () => {
  const sample: AgentBid = {
    agentAddress: 'GTEST',
    price: '1',
    reputation: '50',
    estimatedLatencySeconds: '10',
    successRate: '0.5',
  };

  it.each(FIXTURES.bid.invalidWeights.map((c) => [c.id, c] as const))(
    '%s is rejected',
    (_id, testCase) => {
      expect(() => bid.scoreBid(sample, '10', '10', testCase.weights))
        .toThrow(/weights must sum to 1\.0/);
    },
  );
});
