#!/usr/bin/env tsx
/**
 * Generate `fixtures/determinism.json` from the **TypeScript** implementation.
 *
 * The TS math modules are the reference: they are what the on-chain bid
 * scores were computed against, so the Python port has to match them, not the
 * other way round. This script runs every case through `packages/core`'s
 * `fixed-point` and `bid` modules and records the exact output strings.
 *
 * Both suites then assert against the same file:
 *   - packages/core/src/math/__tests__/determinism-fixtures.test.ts
 *   - python/tests/test_determinism.py
 *
 * If they agree, the two implementations are byte-identical for every case
 * here. If a change to either makes them diverge, one of the two suites
 * fails — which is the actual regression test that matters.
 *
 * ```bash
 * pnpm fixtures:generate          # regenerate
 * pnpm fixtures:check             # verify the committed file is current
 * ```
 *
 * Regenerating is a deliberate act: a diff in this file means the numeric
 * contract changed, and should be reviewed as such.
 */

import { writeFileSync, readFileSync, existsSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import BigNumber from 'bignumber.js';

import * as fp from '../packages/core/src/math/fixed-point.js';
import * as bid from '../packages/core/src/math/bid.js';
import type { AgentBid, BidWeights } from '../packages/core/src/math/bid.js';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT_PATH = resolve(REPO_ROOT, 'fixtures/determinism.json');

/** Canonical serialisation of a BigNumber result: 18 dp, truncated, no exponent. */
const canon = (value: BigNumber): string => value.toFixed(18, BigNumber.ROUND_DOWN);

// ─── Case definitions ────────────────────────────────────────────────────────

type Kind = 'decimal' | 'string' | 'int' | 'bool';

interface Case {
  id: string;
  fn: string;
  args: (string | number)[];
  kind: Kind;
  /** Present when the call is expected to raise in both languages. */
  throws?: true;
  expect?: string;
}

/** Values chosen to stress representation, not just happy paths. */
const AMOUNTS = [
  '0',
  '1',
  '0.1',
  '0.2',
  '0.3',
  '2.5',
  '8.2399999',
  '8.235',
  '-8.239',
  '0.0000001',
  '1.5000001',
  '1.50000019',
  '123456.7891234',
  '999999999.9999999',
  '-42.0000001',
  '0.000000000000000001', // exactly 1e-18, the division floor
  '0.0000000000000000001', // 1e-19, below it
  '12345678901234567.89012345678901234567',
  '170141183460469231731687303715884105727', // 2^127 - 1
];

const fixedPointCases: Case[] = [];

function addCase(fn: string, args: (string | number)[], kind: Kind): void {
  const id = `${fn}(${args.map((a) => JSON.stringify(a)).join(', ')})`;
  fixedPointCases.push({ id, fn, args, kind });
}

// bn — round-trips through the canonical form
for (const a of AMOUNTS) addCase('bn', [a], 'decimal');

// Binary arithmetic across a representative cross-product
const PAIRS: [string, string][] = [
  ['0.1', '0.2'],
  ['0.3', '0.1'],
  ['1', '3'],
  ['2', '3'],
  ['-1', '3'],
  ['-2', '3'],
  ['10', '4'],
  ['1', '1000000000000000000'],
  ['1', '10000000000000000000'],
  ['123456789012345678901234567890', '987654321098765432109876543210'],
  ['170141183460469231731687303715884105727', '1'],
  ['0.0000001', '0.0000001'],
  ['8.2399999', '100'],
  ['-42.0000001', '3'],
];
for (const [a, b] of PAIRS) {
  addCase('add', [a, b], 'decimal');
  addCase('sub', [a, b], 'decimal');
  addCase('mul', [a, b], 'decimal');
  addCase('div', [a, b], 'decimal');
}

// pct — including the zero-total short circuit and custom precision
for (const [v, t] of [['1.45', '5.00'], ['2', '3'], ['1', '0'], ['0', '0'], ['10', '5'], ['1', '7']] as [string, string][]) {
  addCase('pct', [v, t], 'decimal');
  for (const dp of [0, 2, 8]) addCase('pct', [v, t, dp], 'decimal');
}

// clamp — inside, below, above, exact boundaries, inverted range
for (const [v, lo, hi] of [
  ['5', '0', '10'],
  ['-3', '0', '10'],
  ['99', '0', '10'],
  ['0', '0', '10'],
  ['10', '0', '10'],
  ['7', '3', '3'],
  ['7', '10', '0'],
  ['0.00000001', '0', '1'],
] as [string, string, string][]) {
  addCase('clamp', [v, lo, hi], 'decimal');
}

// sumStrings — order independence is asserted separately in each suite
for (const values of [
  [],
  ['1.5'],
  ['0.1', '0.1', '0.1', '0.1', '0.1', '0.1', '0.1', '0.1', '0.1', '0.1'],
  ['0.1', '0.02', '3', '0.000004'],
  ['5', '-2', '-1'],
  ['170141183460469231731687303715884105727', '1'],
]) {
  fixedPointCases.push({
    id: `sumStrings(${JSON.stringify(values)})`,
    fn: 'sumStrings',
    args: values,
    kind: 'decimal',
  });
}

// Stroop conversions
for (const a of AMOUNTS) addCase('toStroops', [a], 'int');
for (const s of ['0', '1', '10000000', '15000001', '-15000001', '170141183460469231731687303715884105727']) {
  for (const dp of [0, 2, 7]) addCase('fromStroops', [s, dp], 'string');
}

// Formatting — the ROUND_DOWN contract
for (const a of AMOUNTS) {
  for (const places of [0, 2, 4, 7, 18]) {
    addCase('fmt', [a, places], 'string');
    addCase('toStr', [a, places], 'string');
  }
}

// Comparisons
for (const [a, b] of [
  ['1', '1'],
  ['1.0', '1'],
  ['0', '-0'],
  ['2', '1'],
  ['1', '2'],
  ['1.00000000000000000001', '1.00000000000000000002'],
] as [string, string][]) {
  for (const fn of ['gt', 'gte', 'lt', 'lte', 'eq']) addCase(fn, [a, b], 'bool');
}
for (const a of ['0', '0.0', '-0', '0.0000001', '-1', '1']) {
  addCase('isZero', [a], 'bool');
  addCase('isPositive', [a], 'bool');
}

// Cases both implementations must reject
const throwingCases: Case[] = [
  { id: 'bn("abc")', fn: 'bn', args: ['abc'], kind: 'decimal', throws: true },
  { id: 'bn("")', fn: 'bn', args: [''], kind: 'decimal', throws: true },
  { id: 'bn("1.2.3")', fn: 'bn', args: ['1.2.3'], kind: 'decimal', throws: true },
  { id: 'div(1,0)', fn: 'div', args: ['1', '0'], kind: 'decimal', throws: true },
  { id: 'div(1,"0.0")', fn: 'div', args: ['1', '0.0'], kind: 'decimal', throws: true },
  { id: 'div(1,"-0")', fn: 'div', args: ['1', '-0'], kind: 'decimal', throws: true },
];

// ─── Evaluation ──────────────────────────────────────────────────────────────

const FIXED_POINT_FNS: Record<string, (...args: never[]) => unknown> = {
  bn: fp.bn,
  add: fp.add,
  sub: fp.sub,
  mul: fp.mul,
  div: fp.div,
  pct: fp.pct,
  clamp: fp.clamp,
  sumStrings: fp.sumStrings,
  toStroops: fp.toStroops,
  fromStroops: (s: string, dp: number) => fp.fromStroops(BigInt(s), dp),
  fmt: fp.fmt,
  toStr: (v: string, places: number) => fp.toStr(fp.bn(v), places),
  gt: fp.gt,
  gte: fp.gte,
  lt: fp.lt,
  lte: fp.lte,
  eq: fp.eq,
  isZero: fp.isZero,
  isPositive: fp.isPositive,
};

function evaluate(testCase: Case): Case {
  const fn = FIXED_POINT_FNS[testCase.fn];
  if (!fn) throw new Error(`No such function: ${testCase.fn}`);

  const args = testCase.fn === 'sumStrings' ? [testCase.args] : testCase.args;
  const result = (fn as (...a: unknown[]) => unknown)(...args);

  let expect: string;
  switch (testCase.kind) {
    case 'decimal': expect = canon(result as BigNumber); break;
    case 'int': expect = (result as bigint).toString(); break;
    case 'bool': expect = String(result); break;
    default: expect = String(result);
  }
  return { ...testCase, expect };
}

// ─── Bid fixtures ────────────────────────────────────────────────────────────

const BIDS: Record<string, AgentBid> = {
  perfect: { agentAddress: 'GPERFECT', price: '0', reputation: '100', estimatedLatencySeconds: '0', successRate: '1' },
  worst: { agentAddress: 'GWORST', price: '10', reputation: '0', estimatedLatencySeconds: '10', successRate: '0' },
  mid: { agentAddress: 'GMID', price: '2.5', reputation: '60', estimatedLatencySeconds: '5', successRate: '0.8' },
  repeating: { agentAddress: 'GREPEAT', price: '1', reputation: '77.7777777', estimatedLatencySeconds: '1.1111111', successRate: '0.9999999' },
  precise: { agentAddress: 'GPRECISE', price: '0.0333333', reputation: '33.3333333', estimatedLatencySeconds: '0.6666666', successRate: '0.6666666' },
  overRange: { agentAddress: 'GOVER', price: '20', reputation: '150', estimatedLatencySeconds: '20', successRate: '1.5' },
  negative: { agentAddress: 'GNEG', price: '1', reputation: '-20', estimatedLatencySeconds: '1', successRate: '-0.5' },
};

const WEIGHT_SETS: Record<string, BidWeights> = {
  default: bid.DEFAULT_BID_WEIGHTS,
  priceHeavy: { price: '0.7', reputation: '0.1', latency: '0.1', reliability: '0.1' },
  priceOnly: { price: '1', reputation: '0', latency: '0', reliability: '0' },
  reputationHeavy: { price: '0.05', reputation: '0.9', latency: '0.025', reliability: '0.025' },
  thirds: { price: '0.3333333333333333', reputation: '0.3333333333333333', latency: '0.3333333333333334', reliability: '0' },
};

interface ScoreBidCase {
  id: string;
  bid: AgentBid;
  maxBid: string;
  maxLatency: string;
  weights: string;
  expect: { score: string; breakdown: Record<string, string> };
}

const scoreBidCases: ScoreBidCase[] = [];
for (const [bidName, b] of Object.entries(BIDS)) {
  for (const [maxBid, maxLatency] of [['10', '10'], ['0', '10'], ['10', '0'], ['0', '0'], ['3', '7'], ['0.7777777', '9.9999999']]) {
    for (const [weightName, weights] of Object.entries(WEIGHT_SETS)) {
      const scored = bid.scoreBid(b, maxBid!, maxLatency!, weights);
      scoreBidCases.push({
        id: `${bidName}/${maxBid}/${maxLatency}/${weightName}`,
        bid: b,
        maxBid: maxBid!,
        maxLatency: maxLatency!,
        weights: weightName,
        expect: { score: scored.score, breakdown: scored.breakdown },
      });
    }
  }
}

interface RankCase {
  id: string;
  bids: AgentBid[];
  weights: string;
  expect: { agentAddress: string; score: string }[];
}

const POOLS: Record<string, AgentBid[]> = {
  empty: [],
  single: [BIDS.mid!],
  spread: [BIDS.mid!, BIDS.perfect!, BIDS.worst!],
  ties: ['GDELTA', 'GALPHA', 'GCHARLIE', 'GBRAVO'].map((agentAddress) => ({
    agentAddress, price: '1', reputation: '50', estimatedLatencySeconds: '10', successRate: '0.5',
  })),
  allFree: [
    { agentAddress: 'GA', price: '0', reputation: '50', estimatedLatencySeconds: '5', successRate: '0.5' },
    { agentAddress: 'GB', price: '0', reputation: '70', estimatedLatencySeconds: '5', successRate: '0.5' },
  ],
  allInstant: [
    { agentAddress: 'GA', price: '1', reputation: '50', estimatedLatencySeconds: '0', successRate: '0.5' },
    { agentAddress: 'GB', price: '2', reputation: '70', estimatedLatencySeconds: '0', successRate: '0.5' },
  ],
  precise: [BIDS.precise!, BIDS.repeating!, BIDS.mid!],
  large: Array.from({ length: 40 }, (_, i) => ({
    agentAddress: `G${String(i).padStart(4, '0')}`,
    price: `${i % 17}.${i % 7}`,
    reputation: `${i % 101}`,
    estimatedLatencySeconds: `${i % 23}`,
    successRate: `0.${String(i % 100).padStart(2, '0')}`,
  })),
};

const rankCases: RankCase[] = [];
for (const [poolName, pool] of Object.entries(POOLS)) {
  for (const weightName of ['default', 'priceHeavy', 'priceOnly'] as const) {
    const ranked = bid.rankBids(pool, WEIGHT_SETS[weightName]);
    rankCases.push({
      id: `${poolName}/${weightName}`,
      bids: pool,
      weights: weightName,
      expect: ranked.map((r) => ({ agentAddress: r.agentAddress, score: r.score })),
    });
  }
}

interface SpendCase {
  id: string;
  spent: string;
  limit: string;
  amount?: string;
  expect: { withinLimit?: boolean; remaining?: string };
}

const SPEND_SCENARIOS: [string, string, string][] = [
  ['1000', '10000', '500'],
  ['9500', '10000', '500'],
  ['9500', '10000', '501'],
  ['10000', '10000', '0'],
  ['10000', '10000', '1'],
  ['0', '0', '0'],
  ['0', '0', '1'],
  ['20000', '10000', '0'],
  ['170141183460469231731687303715884105726', '170141183460469231731687303715884105727', '1'],
  ['170141183460469231731687303715884105726', '170141183460469231731687303715884105727', '2'],
  ['0.9999999', '1', '0.0000001'],
  ['0.9999999', '1', '0.0000002'],
  ['0.1', '1', '0'],
  ['0.5', '2', '0'],
  ['3000', '10000', '0'],
  ['15000', '10000', '0'],
];

const spendCases: SpendCase[] = SPEND_SCENARIOS.map(([spent, limit, amount]) => ({
  id: `${spent}/${limit}/${amount}`,
  spent,
  limit,
  amount,
  expect: {
    withinLimit: bid.isWithinSpendLimit(spent, limit, amount),
    remaining: bid.remainingBudget(spent, limit),
  },
}));

/** Weight sets both implementations must reject. */
const invalidWeightCases = [
  { id: 'sum-below-1', weights: { price: '0.25', reputation: '0.25', latency: '0.25', reliability: '0.20' } },
  { id: 'sum-above-1', weights: { price: '0.5', reputation: '0.25', latency: '0.25', reliability: '0.25' } },
  { id: 'off-by-1e-18', weights: { price: '0.250000000000000001', reputation: '0.25', latency: '0.25', reliability: '0.25' } },
  { id: 'all-ones', weights: { price: '1', reputation: '1', latency: '1', reliability: '1' } },
  { id: 'all-zero', weights: { price: '0', reputation: '0', latency: '0', reliability: '0' } },
];

// ─── Emit ────────────────────────────────────────────────────────────────────

const fixtures = {
  $comment:
    'GENERATED by scripts/generate-fixtures.ts from the TypeScript implementation. ' +
    'Do not hand-edit. Consumed by both the vitest and pytest suites to prove the ' +
    'TS and Python math modules produce byte-identical output. Regenerate with ' +
    '`pnpm fixtures:generate`; a diff here means the numeric contract changed.',
  version: 1,
  canonicalForm: 'decimal results are serialised as toFixed(18, ROUND_DOWN)',
  weightSets: WEIGHT_SETS,
  fixedPoint: [...fixedPointCases.map(evaluate), ...throwingCases],
  bid: {
    scoreBid: scoreBidCases,
    rankBids: rankCases,
    spendLimit: spendCases,
    invalidWeights: invalidWeightCases,
  },
};

const serialised = `${JSON.stringify(fixtures, null, 2)}\n`;

if (process.argv.includes('--check')) {
  if (!existsSync(OUT_PATH)) {
    console.error(`error: ${OUT_PATH} does not exist. Run: pnpm fixtures:generate`);
    process.exit(1);
  }
  if (readFileSync(OUT_PATH, 'utf8') !== serialised) {
    console.error(
      'error: fixtures/determinism.json is out of date with the TypeScript\n' +
        '       implementation. Regenerate with `pnpm fixtures:generate` and review\n' +
        '       the diff — it means the numeric contract changed.',
    );
    process.exit(1);
  }
  console.log('fixtures/determinism.json is up to date.');
} else {
  writeFileSync(OUT_PATH, serialised);
  const total =
    fixtures.fixedPoint.length +
    scoreBidCases.length +
    rankCases.length +
    spendCases.length +
    invalidWeightCases.length;
  console.log(`Wrote ${OUT_PATH}`);
  console.log(`  fixed-point cases : ${fixtures.fixedPoint.length}`);
  console.log(`  scoreBid cases    : ${scoreBidCases.length}`);
  console.log(`  rankBids cases    : ${rankCases.length}`);
  console.log(`  spend-limit cases : ${spendCases.length}`);
  console.log(`  invalid weights   : ${invalidWeightCases.length}`);
  console.log(`  total             : ${total}`);
}
