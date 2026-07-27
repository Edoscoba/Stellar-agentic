import { describe, it, expect } from 'vitest';

import {
  scoreBid,
  rankBids,
  selectBestBid,
  isWithinSpendLimit,
  remainingBudget,
  DEFAULT_BID_WEIGHTS,
  type AgentBid,
  type BidWeights,
} from '../bid.js';

// ─── Fixtures ────────────────────────────────────────────────────────────────

function makeBid(overrides: Partial<AgentBid> = {}): AgentBid {
  return {
    agentAddress: 'GAAA',
    price: '1',
    reputation: '50',
    estimatedLatencySeconds: '10',
    successRate: '0.5',
    ...overrides,
  };
}

// ─── Weight validation ───────────────────────────────────────────────────────

describe('scoreBid — weight validation', () => {
  it('accepts the default weights', () => {
    expect(() => scoreBid(makeBid(), '1', '10')).not.toThrow();
  });

  it('throws RangeError when weights sum below 1', () => {
    const weights: BidWeights = {
      price: '0.25',
      reputation: '0.25',
      latency: '0.25',
      reliability: '0.20',
    };
    expect(() => scoreBid(makeBid(), '1', '10', weights)).toThrow(RangeError);
    expect(() => scoreBid(makeBid(), '1', '10', weights)).toThrow(
      /weights must sum to 1\.0, got 0\.9500/,
    );
  });

  it('throws RangeError when weights sum above 1', () => {
    const weights: BidWeights = {
      price: '0.5',
      reputation: '0.25',
      latency: '0.25',
      reliability: '0.25',
    };
    expect(() => scoreBid(makeBid(), '1', '10', weights)).toThrow(
      /weights must sum to 1\.0, got 1\.2500/,
    );
  });

  it('rejects a sum that is off by a single unit in the 18th decimal', () => {
    // The check is exact equality, not a tolerance — an epsilon-based check
    // would be architecture-sensitive and defeat the whole determinism point.
    const weights: BidWeights = {
      price: '0.250000000000000001',
      reputation: '0.25',
      latency: '0.25',
      reliability: '0.25',
    };
    expect(() => scoreBid(makeBid(), '1', '10', weights)).toThrow(RangeError);
  });

  it('accepts an unequal but valid weighting', () => {
    const weights: BidWeights = {
      price: '0.7',
      reputation: '0.1',
      latency: '0.1',
      reliability: '0.1',
    };
    expect(() => scoreBid(makeBid(), '1', '10', weights)).not.toThrow();
  });

  it('accepts weights that concentrate everything on one dimension', () => {
    const weights: BidWeights = {
      price: '1',
      reputation: '0',
      latency: '0',
      reliability: '0',
    };
    const scored = scoreBid(
      makeBid({ price: '0', reputation: '0', successRate: '0' }),
      '10',
      '10',
      weights,
    );
    expect(scored.score).toBe('100.0000');
  });

  it('rejects a non-numeric weight', () => {
    const weights = { ...DEFAULT_BID_WEIGHTS, price: 'oops' };
    expect(() => scoreBid(makeBid(), '1', '10', weights)).toThrow(RangeError);
  });

  it('DEFAULT_BID_WEIGHTS is an equal four-way split summing to exactly 1', () => {
    expect(DEFAULT_BID_WEIGHTS).toEqual({
      price: '0.25',
      reputation: '0.25',
      latency: '0.25',
      reliability: '0.25',
    });
  });
});

// ─── Sub-scores ──────────────────────────────────────────────────────────────

describe('scoreBid — sub-scores', () => {
  it('gives a full price score to a free bid', () => {
    const scored = scoreBid(makeBid({ price: '0' }), '10', '10');
    expect(scored.breakdown.priceScore).toBe('100.0000');
  });

  it('gives a zero price score to the most expensive bid', () => {
    const scored = scoreBid(makeBid({ price: '10' }), '10', '10');
    expect(scored.breakdown.priceScore).toBe('0.0000');
  });

  it('scales the price score linearly in between', () => {
    const scored = scoreBid(makeBid({ price: '2.5' }), '10', '10');
    expect(scored.breakdown.priceScore).toBe('75.0000');
  });

  it('gives a full price score when maxBid is zero (all bids free)', () => {
    // Special case: the normaliser is zero, so the ratio is undefined. The
    // module short-circuits to 100 rather than dividing by zero.
    const scored = scoreBid(makeBid({ price: '0' }), '0', '10');
    expect(scored.breakdown.priceScore).toBe('100.0000');
  });

  it('gives a full price score when maxBid is zero even if the bid is not', () => {
    // Degenerate input, but must not throw a division-by-zero RangeError.
    const scored = scoreBid(makeBid({ price: '5' }), '0', '10');
    expect(scored.breakdown.priceScore).toBe('100.0000');
  });

  it('clamps the price score at 0 when a bid exceeds maxBid', () => {
    const scored = scoreBid(makeBid({ price: '20' }), '10', '10');
    expect(scored.breakdown.priceScore).toBe('0.0000');
  });

  it('passes reputation through unchanged in 0–100', () => {
    expect(scoreBid(makeBid({ reputation: '73.5' }), '10', '10').breakdown.reputationScore)
      .toBe('73.5000');
  });

  it('clamps reputation above 100 and below 0', () => {
    expect(scoreBid(makeBid({ reputation: '150' }), '10', '10').breakdown.reputationScore)
      .toBe('100.0000');
    expect(scoreBid(makeBid({ reputation: '-20' }), '10', '10').breakdown.reputationScore)
      .toBe('0.0000');
  });

  it('gives a full latency score to an instant bid', () => {
    const scored = scoreBid(makeBid({ estimatedLatencySeconds: '0' }), '10', '10');
    expect(scored.breakdown.latencyScore).toBe('100.0000');
  });

  it('gives a zero latency score to the slowest bid', () => {
    const scored = scoreBid(makeBid({ estimatedLatencySeconds: '10' }), '10', '10');
    expect(scored.breakdown.latencyScore).toBe('0.0000');
  });

  it('gives a full latency score when maxLatency is zero', () => {
    // Same special case as maxBid: every bid claims instant completion.
    const scored = scoreBid(makeBid({ estimatedLatencySeconds: '0' }), '10', '0');
    expect(scored.breakdown.latencyScore).toBe('100.0000');
  });

  it('gives a full latency score when maxLatency is zero even for a nonzero latency', () => {
    const scored = scoreBid(makeBid({ estimatedLatencySeconds: '7' }), '10', '0');
    expect(scored.breakdown.latencyScore).toBe('100.0000');
  });

  it('handles both normalisers being zero at once', () => {
    const scored = scoreBid(
      makeBid({ price: '0', estimatedLatencySeconds: '0', reputation: '100', successRate: '1' }),
      '0',
      '0',
    );
    expect(scored.score).toBe('100.0000');
  });

  it('scales reliability from a 0–1 fraction to 0–100', () => {
    expect(scoreBid(makeBid({ successRate: '0.97' }), '10', '10').breakdown.reliabilityScore)
      .toBe('97.0000');
    expect(scoreBid(makeBid({ successRate: '1' }), '10', '10').breakdown.reliabilityScore)
      .toBe('100.0000');
    expect(scoreBid(makeBid({ successRate: '0' }), '10', '10').breakdown.reliabilityScore)
      .toBe('0.0000');
  });

  it('clamps reliability when successRate is out of its 0–1 range', () => {
    expect(scoreBid(makeBid({ successRate: '1.5' }), '10', '10').breakdown.reliabilityScore)
      .toBe('100.0000');
    expect(scoreBid(makeBid({ successRate: '-0.5' }), '10', '10').breakdown.reliabilityScore)
      .toBe('0.0000');
  });
});

// ─── Composite score ─────────────────────────────────────────────────────────

describe('scoreBid — composite', () => {
  it('scores a perfect bid at 100', () => {
    const scored = scoreBid(
      makeBid({
        price: '0',
        reputation: '100',
        estimatedLatencySeconds: '0',
        successRate: '1',
      }),
      '10',
      '10',
    );
    expect(scored.score).toBe('100.0000');
  });

  it('scores a worst-case bid at 0', () => {
    const scored = scoreBid(
      makeBid({
        price: '10',
        reputation: '0',
        estimatedLatencySeconds: '10',
        successRate: '0',
      }),
      '10',
      '10',
    );
    expect(scored.score).toBe('0.0000');
  });

  it('is the exact weighted sum of the four sub-scores', () => {
    // price 75, reputation 60, latency 50, reliability 80 → equal weights
    // → (75 + 60 + 50 + 80) / 4 = 66.25
    const scored = scoreBid(
      makeBid({
        price: '2.5',
        reputation: '60',
        estimatedLatencySeconds: '5',
        successRate: '0.8',
      }),
      '10',
      '10',
    );
    expect(scored.breakdown).toEqual({
      priceScore: '75.0000',
      reputationScore: '60.0000',
      latencyScore: '50.0000',
      reliabilityScore: '80.0000',
    });
    expect(scored.score).toBe('66.2500');
  });

  it('respects unequal weights', () => {
    // Same bid as above, but price carries 70% of the decision.
    const scored = scoreBid(
      makeBid({
        price: '2.5',
        reputation: '60',
        estimatedLatencySeconds: '5',
        successRate: '0.8',
      }),
      '10',
      '10',
      { price: '0.7', reputation: '0.1', latency: '0.1', reliability: '0.1' },
    );
    // 0.7×75 + 0.1×60 + 0.1×50 + 0.1×80 = 52.5 + 6 + 5 + 8 = 71.5
    expect(scored.score).toBe('71.5000');
  });

  it('carries the agent address through unchanged', () => {
    const scored = scoreBid(makeBid({ agentAddress: 'GWORKER123' }), '10', '10');
    expect(scored.agentAddress).toBe('GWORKER123');
  });

  it('always emits scores with exactly 4 decimal places', () => {
    const scored = scoreBid(makeBid(), '10', '10');
    for (const value of [scored.score, ...Object.values(scored.breakdown)]) {
      expect(value).toMatch(/^-?\d+\.\d{4}$/);
    }
  });

  it('truncates the composite score rather than rounding it up', () => {
    // priceScore = 100 × (1 − 1/3) = 66.666...  With all weight on price the
    // composite inherits that repeating decimal; ROUND_DOWN must give
    // ...6666, not ...6667.
    const scored = scoreBid(
      makeBid({ price: '1' }),
      '3',
      '10',
      { price: '1', reputation: '0', latency: '0', reliability: '0' },
    );
    expect(scored.score).toBe('66.6666');
    expect(scored.breakdown.priceScore).toBe('66.6666');
  });

  it('rejects a malformed bid field', () => {
    expect(() => scoreBid(makeBid({ price: 'free' }), '10', '10')).toThrow(RangeError);
    expect(() => scoreBid(makeBid({ reputation: '' }), '10', '10')).toThrow(RangeError);
  });

  it('is byte-identical across repeated evaluation', () => {
    const bid = makeBid({
      price: '0.0333333',
      reputation: '77.7777777',
      estimatedLatencySeconds: '1.1111111',
      successRate: '0.9999999',
    });
    const first = scoreBid(bid, '0.7777777', '9.9999999');
    for (let i = 0; i < 50; i++) {
      expect(scoreBid(bid, '0.7777777', '9.9999999')).toEqual(first);
    }
  });
});

// ─── rankBids ────────────────────────────────────────────────────────────────

describe('rankBids', () => {
  it('returns an empty array for an empty pool', () => {
    expect(rankBids([])).toEqual([]);
  });

  it('returns a single bid unchanged', () => {
    const ranked = rankBids([makeBid({ agentAddress: 'GONLY' })]);
    expect(ranked).toHaveLength(1);
    expect(ranked[0]!.agentAddress).toBe('GONLY');
  });

  it('scores a lone bid against itself as the normaliser', () => {
    // With one bid, maxBid === its own price → priceScore 0, and likewise
    // for latency. Only reputation and reliability contribute.
    const ranked = rankBids([
      makeBid({ price: '5', reputation: '100', estimatedLatencySeconds: '5', successRate: '1' }),
    ]);
    expect(ranked[0]!.breakdown.priceScore).toBe('0.0000');
    expect(ranked[0]!.breakdown.latencyScore).toBe('0.0000');
    expect(ranked[0]!.score).toBe('50.0000'); // (0 + 100 + 0 + 100) / 4
  });

  it('sorts best-first by score', () => {
    const bids = [
      makeBid({ agentAddress: 'GMEDIOCRE', price: '5', reputation: '50', estimatedLatencySeconds: '5', successRate: '0.5' }),
      makeBid({ agentAddress: 'GBEST', price: '0', reputation: '100', estimatedLatencySeconds: '0', successRate: '1' }),
      makeBid({ agentAddress: 'GWORST', price: '10', reputation: '0', estimatedLatencySeconds: '10', successRate: '0' }),
    ];
    expect(rankBids(bids).map((b) => b.agentAddress)).toEqual(['GBEST', 'GMEDIOCRE', 'GWORST']);
  });

  it('normalises against the maximum across the whole set', () => {
    const bids = [
      makeBid({ agentAddress: 'GA', price: '1', estimatedLatencySeconds: '2' }),
      makeBid({ agentAddress: 'GB', price: '4', estimatedLatencySeconds: '8' }),
    ];
    const ranked = rankBids(bids);
    const a = ranked.find((b) => b.agentAddress === 'GA')!;
    const b = ranked.find((b) => b.agentAddress === 'GB')!;
    // maxBid = 4, maxLatency = 8
    expect(a.breakdown.priceScore).toBe('75.0000');
    expect(a.breakdown.latencyScore).toBe('75.0000');
    expect(b.breakdown.priceScore).toBe('0.0000');
    expect(b.breakdown.latencyScore).toBe('0.0000');
  });

  it('breaks ties by agentAddress ascending, not by input order', () => {
    // All four bids are identical apart from the address, so every score is
    // equal and only the tie-break decides the order.
    const identical = { price: '1', reputation: '50', estimatedLatencySeconds: '10', successRate: '0.5' };
    const bids = [
      makeBid({ agentAddress: 'GDELTA', ...identical }),
      makeBid({ agentAddress: 'GALPHA', ...identical }),
      makeBid({ agentAddress: 'GCHARLIE', ...identical }),
      makeBid({ agentAddress: 'GBRAVO', ...identical }),
    ];
    expect(rankBids(bids).map((b) => b.agentAddress)).toEqual([
      'GALPHA',
      'GBRAVO',
      'GCHARLIE',
      'GDELTA',
    ]);
  });

  it('produces the same ranking regardless of input order', () => {
    // The core determinism property: shuffling the bid pool must not change
    // the winner or the ordering.
    const bids = [
      makeBid({ agentAddress: 'GA', price: '1', reputation: '90', estimatedLatencySeconds: '3', successRate: '0.9' }),
      makeBid({ agentAddress: 'GB', price: '2', reputation: '90', estimatedLatencySeconds: '2', successRate: '0.9' }),
      makeBid({ agentAddress: 'GC', price: '3', reputation: '70', estimatedLatencySeconds: '1', successRate: '0.95' }),
      makeBid({ agentAddress: 'GD', price: '1', reputation: '90', estimatedLatencySeconds: '3', successRate: '0.9' }),
    ];
    const baseline = rankBids(bids);
    const permutations = [
      [3, 2, 1, 0],
      [1, 0, 3, 2],
      [2, 3, 0, 1],
      [0, 2, 1, 3],
    ];
    for (const order of permutations) {
      expect(rankBids(order.map((i) => bids[i]!))).toEqual(baseline);
    }
  });

  it('ties GA and GD (identical bids) and orders them lexicographically', () => {
    const bids = [
      makeBid({ agentAddress: 'GD', price: '1', reputation: '90', estimatedLatencySeconds: '3', successRate: '0.9' }),
      makeBid({ agentAddress: 'GA', price: '1', reputation: '90', estimatedLatencySeconds: '3', successRate: '0.9' }),
    ];
    const ranked = rankBids(bids);
    expect(ranked[0]!.score).toBe(ranked[1]!.score);
    expect(ranked.map((b) => b.agentAddress)).toEqual(['GA', 'GD']);
  });

  it('applies custom weights to the whole set', () => {
    const bids = [
      makeBid({ agentAddress: 'GCHEAP', price: '1', reputation: '10', estimatedLatencySeconds: '5', successRate: '0.1' }),
      makeBid({ agentAddress: 'GTRUSTED', price: '10', reputation: '100', estimatedLatencySeconds: '5', successRate: '1' }),
    ];
    // Price-dominant: the cheap agent wins.
    expect(rankBids(bids, { price: '0.9', reputation: '0.05', latency: '0.025', reliability: '0.025' })[0]!.agentAddress)
      .toBe('GCHEAP');
    // Reputation-dominant: the trusted agent wins.
    expect(rankBids(bids, { price: '0.05', reputation: '0.9', latency: '0.025', reliability: '0.025' })[0]!.agentAddress)
      .toBe('GTRUSTED');
  });

  it('propagates weight-validation errors', () => {
    expect(() =>
      rankBids([makeBid()], { price: '1', reputation: '1', latency: '1', reliability: '1' }),
    ).toThrow(RangeError);
  });

  it('handles a pool where every price is zero', () => {
    const bids = [
      makeBid({ agentAddress: 'GA', price: '0' }),
      makeBid({ agentAddress: 'GB', price: '0' }),
    ];
    // maxBid is 0 → the special case applies to every bid.
    expect(rankBids(bids).every((b) => b.breakdown.priceScore === '100.0000')).toBe(true);
  });

  it('handles a pool where every latency is zero', () => {
    const bids = [
      makeBid({ agentAddress: 'GA', estimatedLatencySeconds: '0' }),
      makeBid({ agentAddress: 'GB', estimatedLatencySeconds: '0' }),
    ];
    expect(rankBids(bids).every((b) => b.breakdown.latencyScore === '100.0000')).toBe(true);
  });

  it('does not mutate the input array', () => {
    const bids = [
      makeBid({ agentAddress: 'GZ', price: '10' }),
      makeBid({ agentAddress: 'GA', price: '1' }),
    ];
    const snapshot = JSON.parse(JSON.stringify(bids));
    rankBids(bids);
    expect(bids).toEqual(snapshot);
  });

  it('scales to a large pool without losing determinism', () => {
    const bids = Array.from({ length: 200 }, (_, i) =>
      makeBid({
        agentAddress: `G${String(i).padStart(4, '0')}`,
        price: `${i % 17}.${i % 7}`,
        reputation: `${i % 101}`,
        estimatedLatencySeconds: `${i % 23}`,
        successRate: `0.${String(i % 100).padStart(2, '0')}`,
      }),
    );
    const first = rankBids(bids);
    expect(first).toHaveLength(200);
    expect(rankBids([...bids].reverse())).toEqual(first);
    // Scores are monotonically non-increasing.
    for (let i = 1; i < first.length; i++) {
      expect(Number(first[i - 1]!.score)).toBeGreaterThanOrEqual(Number(first[i]!.score));
    }
  });
});

// ─── selectBestBid ───────────────────────────────────────────────────────────

describe('selectBestBid', () => {
  it('returns null for an empty pool', () => {
    expect(selectBestBid([])).toBeNull();
  });

  it('returns the only bid in a single-bid pool', () => {
    expect(selectBestBid([makeBid({ agentAddress: 'GSOLO' })])!.agentAddress).toBe('GSOLO');
  });

  it('returns the highest-scoring bid', () => {
    const bids = [
      makeBid({ agentAddress: 'GA', price: '10', reputation: '0', estimatedLatencySeconds: '10', successRate: '0' }),
      makeBid({ agentAddress: 'GB', price: '0', reputation: '100', estimatedLatencySeconds: '0', successRate: '1' }),
    ];
    expect(selectBestBid(bids)!.agentAddress).toBe('GB');
  });

  it('agrees with rankBids()[0]', () => {
    const bids = [
      makeBid({ agentAddress: 'GA', price: '3', reputation: '80', estimatedLatencySeconds: '4', successRate: '0.8' }),
      makeBid({ agentAddress: 'GB', price: '2', reputation: '85', estimatedLatencySeconds: '6', successRate: '0.7' }),
      makeBid({ agentAddress: 'GC', price: '4', reputation: '95', estimatedLatencySeconds: '2', successRate: '0.99' }),
    ];
    expect(selectBestBid(bids)).toEqual(rankBids(bids)[0]);
  });

  it('breaks a tie for first place by the lexicographically smaller address', () => {
    const identical = { price: '1', reputation: '50', estimatedLatencySeconds: '10', successRate: '0.5' };
    const bids = [
      makeBid({ agentAddress: 'GZZZ', ...identical }),
      makeBid({ agentAddress: 'GAAA', ...identical }),
    ];
    expect(selectBestBid(bids)!.agentAddress).toBe('GAAA');
  });

  it('honours custom weights', () => {
    const bids = [
      makeBid({ agentAddress: 'GFAST', price: '10', reputation: '50', estimatedLatencySeconds: '1', successRate: '0.5' }),
      makeBid({ agentAddress: 'GSLOW', price: '1', reputation: '50', estimatedLatencySeconds: '10', successRate: '0.5' }),
    ];
    expect(selectBestBid(bids, { price: '0', reputation: '0', latency: '1', reliability: '0' })!.agentAddress)
      .toBe('GFAST');
    expect(selectBestBid(bids, { price: '1', reputation: '0', latency: '0', reliability: '0' })!.agentAddress)
      .toBe('GSLOW');
  });
});

// ─── Spend limits ────────────────────────────────────────────────────────────

describe('isWithinSpendLimit', () => {
  it('allows a payment well under the limit', () => {
    expect(isWithinSpendLimit('1000', '10000', '500')).toBe(true);
  });

  it('allows a payment landing exactly on the limit (inclusive)', () => {
    expect(isWithinSpendLimit('9500', '10000', '500')).toBe(true);
  });

  it('rejects a payment one stroop over the limit', () => {
    expect(isWithinSpendLimit('9500', '10000', '501')).toBe(false);
  });

  it('allows a zero-amount payment at the limit', () => {
    expect(isWithinSpendLimit('10000', '10000', '0')).toBe(true);
  });

  it('rejects any payment once already at the limit', () => {
    expect(isWithinSpendLimit('10000', '10000', '1')).toBe(false);
  });

  it('rejects everything under a zero limit', () => {
    expect(isWithinSpendLimit('0', '0', '1')).toBe(false);
    expect(isWithinSpendLimit('0', '0', '0')).toBe(true);
  });

  it('reports over-limit when already past the limit', () => {
    expect(isWithinSpendLimit('20000', '10000', '0')).toBe(false);
  });

  it('is exact at i128 stroop magnitudes', () => {
    // 2^127 - 1 stroops. A float-based check would round both sides to the
    // same value and wrongly allow the payment.
    const limit = '170141183460469231731687303715884105727';
    const spent = '170141183460469231731687303715884105726';
    expect(isWithinSpendLimit(spent, limit, '1')).toBe(true);
    expect(isWithinSpendLimit(spent, limit, '2')).toBe(false);
  });

  it('handles sub-stroop decimal strings exactly', () => {
    expect(isWithinSpendLimit('0.9999999', '1', '0.0000001')).toBe(true);
    expect(isWithinSpendLimit('0.9999999', '1', '0.0000002')).toBe(false);
  });

  it('validates its inputs', () => {
    expect(() => isWithinSpendLimit('abc', '1', '1')).toThrow(RangeError);
    expect(() => isWithinSpendLimit('1', 'abc', '1')).toThrow(RangeError);
    expect(() => isWithinSpendLimit('1', '1', 'abc')).toThrow(RangeError);
  });
});

describe('remainingBudget', () => {
  it('returns the unspent remainder', () => {
    expect(remainingBudget('3000', '10000')).toBe('7000');
  });

  it('returns zero when exactly exhausted', () => {
    expect(remainingBudget('10000', '10000')).toBe('0');
  });

  it('floors at zero rather than going negative when over-spent', () => {
    expect(remainingBudget('15000', '10000')).toBe('0');
  });

  it('returns the full limit when nothing is spent', () => {
    expect(remainingBudget('0', '10000')).toBe('10000');
  });

  it('returns zero for a zero limit', () => {
    expect(remainingBudget('0', '0')).toBe('0');
  });

  it('returns an integer stroop string with no decimal point', () => {
    expect(remainingBudget('0', '10000')).not.toContain('.');
  });

  it('truncates a fractional remainder toward zero', () => {
    // toFixed(0) with the global ROUND_DOWN: 0.9 stroops of headroom reads as
    // 0, which is the safe direction for a spend guard.
    expect(remainingBudget('0.1', '1')).toBe('0');
    expect(remainingBudget('0.5', '2')).toBe('1');
  });

  it('is exact at i128 stroop magnitudes', () => {
    expect(remainingBudget('1', '170141183460469231731687303715884105727')).toBe(
      '170141183460469231731687303715884105726',
    );
  });

  it('agrees with isWithinSpendLimit at the boundary', () => {
    const spent = '7500';
    const limit = '10000';
    const remaining = remainingBudget(spent, limit);
    expect(isWithinSpendLimit(spent, limit, remaining)).toBe(true);
    expect(isWithinSpendLimit(spent, limit, `${Number(remaining) + 1}`)).toBe(false);
  });

  it('validates its inputs', () => {
    expect(() => remainingBudget('abc', '1')).toThrow(RangeError);
    expect(() => remainingBudget('1', 'abc')).toThrow(RangeError);
  });
});
