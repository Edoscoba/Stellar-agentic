import { describe, expect, it } from 'vitest';
import {
  predictPaymentOutcome,
  isWindowExpired,
  ledgersRemainingInWindow,
  RATE_LIMIT_LEDGERS_PER_HOUR,
  RATE_LIMIT_LEDGERS_PER_DAY,
  type ChannelSpendState,
  type RateLimitSpendState,
} from '../predict.js';

// ─── Fixtures ────────────────────────────────────────────────────────────────

function makeChannel(overrides: Partial<ChannelSpendState> = {}): ChannelSpendState {
  return {
    active: true,
    limitPerPeriod: '10000',
    spentThisPeriod: '0',
    periodStartLedger: 1000,
    period: 'hourly',
    ...overrides,
  };
}

function makeRateLimit(overrides: Partial<RateLimitSpendState> = {}): RateLimitSpendState {
  return {
    configured: true,
    active: true,
    maxPerTx: '100',
    maxPerHour: '500',
    maxPerDay: '2000',
    maxTxsPerHour: 10,
    hourlySpend: '0',
    dailySpend: '0',
    hourlyTxCount: 0,
    hourWindowStartLedger: 1000,
    dayWindowStartLedger: 1000,
    ...overrides,
  };
}

// ─── isWindowExpired / ledgersRemainingInWindow ──────────────────────────────

describe('isWindowExpired', () => {
  it('is not expired before the window boundary', () => {
    expect(isWindowExpired(1000, 720, 1719)).toBe(false);
  });

  it('is expired exactly at the window boundary (>=)', () => {
    expect(isWindowExpired(1000, 720, 1720)).toBe(true);
  });

  it('is expired well past the boundary', () => {
    expect(isWindowExpired(1000, 720, 5000)).toBe(true);
  });
});

describe('ledgersRemainingInWindow', () => {
  it('returns ledgers left before expiry', () => {
    expect(ledgersRemainingInWindow(1000, 720, 1700)).toBe(20);
  });

  it('returns 0 exactly at expiry, not negative', () => {
    expect(ledgersRemainingInWindow(1000, 720, 1720)).toBe(0);
  });

  it('floors at 0 well past expiry', () => {
    expect(ledgersRemainingInWindow(1000, 720, 5000)).toBe(0);
  });
});

// ─── predictPaymentOutcome — amount validation ───────────────────────────────

describe('predictPaymentOutcome — invalid amount', () => {
  it('blocks a zero amount', () => {
    const result = predictPaymentOutcome({ amount: '0', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toContain('invalid_amount');
  });

  it('blocks a negative amount', () => {
    const result = predictPaymentOutcome({ amount: '-5', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toContain('invalid_amount');
  });

  it('allows a positive amount with no channel or rate-limit state', () => {
    const result = predictPaymentOutcome({ amount: '5', currentLedger: 1000 });
    expect(result).toEqual({ wouldBlock: false, reasons: [] });
  });
});

// ─── predictPaymentOutcome — channel spend limit ─────────────────────────────

describe('predictPaymentOutcome — channel spend limit', () => {
  it('allows a payment under all limits', () => {
    const channelState = makeChannel({ spentThisPeriod: '1000', limitPerPeriod: '10000' });
    const result = predictPaymentOutcome({ channelState, amount: '500', currentLedger: 1000 });
    expect(result).toEqual({ wouldBlock: false, reasons: [] });
  });

  it('allows a payment landing exactly on the limit (boundary, inclusive)', () => {
    const channelState = makeChannel({ spentThisPeriod: '9500', limitPerPeriod: '10000' });
    const result = predictPaymentOutcome({ channelState, amount: '500', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(false);
  });

  it('blocks a payment one stroop over the limit', () => {
    const channelState = makeChannel({ spentThisPeriod: '9500', limitPerPeriod: '10000' });
    const result = predictPaymentOutcome({ channelState, amount: '501', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['channel_spend_limit']);
  });

  it('blocks any payment on an inactive (closed) channel', () => {
    const channelState = makeChannel({ active: false, spentThisPeriod: '0', limitPerPeriod: '10000' });
    const result = predictPaymentOutcome({ channelState, amount: '1', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['channel_inactive']);
  });

  it('does not double-report the spend limit for a closed channel', () => {
    // Closed *and* would've exceeded the limit anyway — only channel_inactive
    // should fire, since `pay()` panics on `!active` before ever reaching the
    // spend-limit check.
    const channelState = makeChannel({
      active: false,
      spentThisPeriod: '9999',
      limitPerPeriod: '10000',
    });
    const result = predictPaymentOutcome({ channelState, amount: '500', currentLedger: 1000 });
    expect(result.reasons).toEqual(['channel_inactive']);
  });

  it('resets spentThisPeriod once the period has expired before checking', () => {
    // Would block if `spentThisPeriod` were taken at face value, but the
    // period expired (currentLedger >= periodStartLedger + ledgersPerPeriod
    // for 'hourly' == 720), so the effective spend is 0.
    const channelState = makeChannel({
      spentThisPeriod: '9999',
      limitPerPeriod: '10000',
      periodStartLedger: 1000,
      period: 'hourly',
    });
    const result = predictPaymentOutcome({ channelState, amount: '5000', currentLedger: 1720 });
    expect(result.wouldBlock).toBe(false);
  });

  it('does not reset spentThisPeriod one ledger before expiry', () => {
    const channelState = makeChannel({
      spentThisPeriod: '9999',
      limitPerPeriod: '10000',
      periodStartLedger: 1000,
      period: 'hourly',
    });
    const result = predictPaymentOutcome({ channelState, amount: '5000', currentLedger: 1719 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['channel_spend_limit']);
  });

  it('uses per_ledger cadence (1 ledger) when configured', () => {
    const channelState = makeChannel({
      spentThisPeriod: '9999',
      limitPerPeriod: '10000',
      periodStartLedger: 1000,
      period: 'per_ledger',
    });
    // One ledger later, the per-ledger period has already rolled over.
    const result = predictPaymentOutcome({ channelState, amount: '10000', currentLedger: 1001 });
    expect(result.wouldBlock).toBe(false);
  });

  it('uses daily cadence (17280 ledgers) when configured', () => {
    const channelState = makeChannel({
      spentThisPeriod: '9999',
      limitPerPeriod: '10000',
      periodStartLedger: 1000,
      period: 'daily',
    });
    expect(
      predictPaymentOutcome({ channelState, amount: '5000', currentLedger: 1000 + 17_279 })
        .wouldBlock,
    ).toBe(true);
    expect(
      predictPaymentOutcome({ channelState, amount: '5000', currentLedger: 1000 + 17_280 })
        .wouldBlock,
    ).toBe(false);
  });
});

// ─── predictPaymentOutcome — rate limiter ────────────────────────────────────

describe('predictPaymentOutcome — rate limiter unconfigured', () => {
  it('allows any amount when no rate limit has ever been configured', () => {
    const rateLimitState = makeRateLimit({ configured: false, maxPerTx: '1' });
    const result = predictPaymentOutcome({
      rateLimitState,
      amount: '1000000',
      currentLedger: 1000,
    });
    expect(result).toEqual({ wouldBlock: false, reasons: [] });
  });
});

describe('predictPaymentOutcome — rate limiter: under all limits', () => {
  it('allows a payment comfortably under every configured limit', () => {
    const rateLimitState = makeRateLimit({
      maxPerTx: '100',
      maxPerHour: '500',
      maxPerDay: '2000',
      maxTxsPerHour: 10,
      hourlySpend: '50',
      dailySpend: '200',
      hourlyTxCount: 2,
    });
    const result = predictPaymentOutcome({ rateLimitState, amount: '10', currentLedger: 1000 });
    expect(result).toEqual({ wouldBlock: false, reasons: [] });
  });
});

describe('predictPaymentOutcome — rate limiter: per-tx limit', () => {
  it('allows an amount exactly at the per-tx cap (boundary, inclusive)', () => {
    const rateLimitState = makeRateLimit({ maxPerTx: '100' });
    const result = predictPaymentOutcome({ rateLimitState, amount: '100', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(false);
  });

  it('blocks an amount one stroop over the per-tx cap', () => {
    const rateLimitState = makeRateLimit({ maxPerTx: '100' });
    const result = predictPaymentOutcome({ rateLimitState, amount: '101', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['rate_limit_per_tx']);
  });
});

describe('predictPaymentOutcome — rate limiter: hourly limit but under daily', () => {
  it('blocks on hourly even though the daily budget has plenty of room', () => {
    const rateLimitState = makeRateLimit({
      maxPerTx: '1000',
      maxPerHour: '500',
      maxPerDay: '100000',
      hourlySpend: '480',
      dailySpend: '480',
    });
    const result = predictPaymentOutcome({ rateLimitState, amount: '30', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['rate_limit_hourly']);
  });

  it('allows an amount landing exactly on the hourly cap (boundary, inclusive)', () => {
    const rateLimitState = makeRateLimit({
      maxPerTx: '1000',
      maxPerHour: '500',
      maxPerDay: '100000',
      hourlySpend: '480',
      dailySpend: '480',
    });
    const result = predictPaymentOutcome({ rateLimitState, amount: '20', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(false);
  });
});

describe('predictPaymentOutcome — rate limiter: daily limit', () => {
  it('blocks on daily once the daily budget is the binding constraint', () => {
    const rateLimitState = makeRateLimit({
      maxPerTx: '1000',
      maxPerHour: '100000',
      maxPerDay: '500',
      hourlySpend: '0',
      dailySpend: '480',
    });
    const result = predictPaymentOutcome({ rateLimitState, amount: '30', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['rate_limit_daily']);
  });

  it('allows an amount landing exactly on the daily cap (boundary, inclusive)', () => {
    const rateLimitState = makeRateLimit({
      maxPerTx: '1000',
      maxPerHour: '100000',
      maxPerDay: '500',
      hourlySpend: '0',
      dailySpend: '480',
    });
    const result = predictPaymentOutcome({ rateLimitState, amount: '20', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(false);
  });
});

describe('predictPaymentOutcome — rate limiter: tx-count limit uses >=, not >', () => {
  it('allows the last available slot when count is one below the cap', () => {
    const rateLimitState = makeRateLimit({ maxTxsPerHour: 10, hourlyTxCount: 9 });
    const result = predictPaymentOutcome({ rateLimitState, amount: '1', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(false);
  });

  it('blocks once count has already reached the cap (boundary is >=, not >)', () => {
    const rateLimitState = makeRateLimit({ maxTxsPerHour: 10, hourlyTxCount: 10 });
    const result = predictPaymentOutcome({ rateLimitState, amount: '1', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['rate_limit_tx_count']);
  });
});

describe('predictPaymentOutcome — rate limiter: window resets', () => {
  it('resets hourly spend/tx-count once the hourly window has expired', () => {
    const rateLimitState = makeRateLimit({
      maxPerHour: '500',
      maxTxsPerHour: 10,
      hourlySpend: '499',
      hourlyTxCount: 10,
      hourWindowStartLedger: 1000,
      dailySpend: '0',
    });
    const result = predictPaymentOutcome({
      rateLimitState,
      amount: '100',
      currentLedger: 1000 + RATE_LIMIT_LEDGERS_PER_HOUR,
    });
    expect(result.wouldBlock).toBe(false);
  });

  it('does not reset one ledger before the hourly window expires', () => {
    const rateLimitState = makeRateLimit({
      maxPerHour: '500',
      hourlySpend: '499',
      hourWindowStartLedger: 1000,
    });
    const result = predictPaymentOutcome({
      rateLimitState,
      amount: '100',
      currentLedger: 1000 + RATE_LIMIT_LEDGERS_PER_HOUR - 1,
    });
    expect(result.wouldBlock).toBe(true);
  });

  it('resets daily spend independently once the daily window has expired', () => {
    const rateLimitState = makeRateLimit({
      maxPerDay: '500',
      dailySpend: '499',
      dayWindowStartLedger: 1000,
      hourlySpend: '0',
    });
    const result = predictPaymentOutcome({
      rateLimitState,
      amount: '100',
      currentLedger: 1000 + RATE_LIMIT_LEDGERS_PER_DAY,
    });
    expect(result.wouldBlock).toBe(false);
  });
});

describe('predictPaymentOutcome — rate limiter: active/killed does not gate check()', () => {
  it('still evaluates normally (and can pass) for a killed agent', () => {
    // Mirrors contracts/rate_limiter/src/lib.rs::check, which never reads
    // `RateLimit.active` — only `is_active()` (a separate query) does.
    const rateLimitState = makeRateLimit({ active: false, maxPerTx: '100' });
    const result = predictPaymentOutcome({ rateLimitState, amount: '50', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(false);
  });

  it('still blocks a killed agent that exceeds a limit', () => {
    const rateLimitState = makeRateLimit({ active: false, maxPerTx: '100' });
    const result = predictPaymentOutcome({ rateLimitState, amount: '101', currentLedger: 1000 });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(['rate_limit_per_tx']);
  });
});

// ─── predictPaymentOutcome — combined channel + rate limiter ─────────────────

describe('predictPaymentOutcome — combined channel + rate limiter', () => {
  it('reports every independent reason a payment would fail, not just the first', () => {
    const channelState = makeChannel({ spentThisPeriod: '9900', limitPerPeriod: '10000' });
    const rateLimitState = makeRateLimit({ maxPerTx: '50' });
    const result = predictPaymentOutcome({
      channelState,
      rateLimitState,
      amount: '200',
      currentLedger: 1000,
    });
    expect(result.wouldBlock).toBe(true);
    expect(result.reasons).toEqual(
      expect.arrayContaining(['channel_spend_limit', 'rate_limit_per_tx']),
    );
    expect(result.reasons).toHaveLength(2);
  });

  it('allows a payment that clears both an active channel and a configured rate limit', () => {
    const channelState = makeChannel({ spentThisPeriod: '100', limitPerPeriod: '10000' });
    const rateLimitState = makeRateLimit({
      maxPerTx: '100',
      maxPerHour: '500',
      maxPerDay: '2000',
      maxTxsPerHour: 10,
    });
    const result = predictPaymentOutcome({
      channelState,
      rateLimitState,
      amount: '50',
      currentLedger: 1000,
    });
    expect(result).toEqual({ wouldBlock: false, reasons: [] });
  });
});

// ─── Fuzz vs. an independent "shadow" re-implementation ──────────────────────
//
// The real cross-language ground truth is `contracts/rate_limiter/src/lib.rs`
// itself, checked against a live contract in
// `src/__tests__/integration.local.test.ts` (gated behind
// `STELLAR_LOCAL_INTEGRATION=1`, since it needs a running local Soroban
// network). That suite can't run in every environment this repo is checked
// out in, so this fuzz stays useful without one: `shadowCheck` below is a
// second, independent transcription of `RateLimiter::check` — using plain
// `bigint` arithmetic rather than reusing any of `predict.ts`'s own helpers —
// so a bug shared between `predictPaymentOutcome` and this test would have
// to be a bug both authors made in the exact same way while implementing the
// exact same source lines differently. Fuzzing many random (state, amount)
// pairs through both catches boundary mistakes that hand-picked cases (like
// the ones above) can miss.
function shadowCheck(state: RateLimitSpendState, amount: bigint): boolean {
  if (!state.configured) return true;

  const maxPerTx = BigInt(state.maxPerTx);
  const maxPerHour = BigInt(state.maxPerHour);
  const maxPerDay = BigInt(state.maxPerDay);
  if (amount > maxPerTx) return false;
  if (BigInt(state.hourlySpend) + amount > maxPerHour) return false;
  if (BigInt(state.dailySpend) + amount > maxPerDay) return false;
  if (state.hourlyTxCount >= state.maxTxsPerHour) return false;
  return true;
}

describe('predictPaymentOutcome — fuzz vs. independent shadow re-implementation', () => {
  // Deterministic LCG so a failure reproduces without capturing random state.
  function makeRng(seed: number) {
    let state = seed;
    return () => {
      state = (state * 1103515245 + 12345) & 0x7fffffff;
      return state / 0x7fffffff;
    };
  }

  it('agrees with the shadow model across many random rate-limit scenarios (no window expiry)', () => {
    const rand = makeRng(1);
    for (let i = 0; i < 2000; i++) {
      const maxPerTx = 1 + Math.floor(rand() * 1000);
      const maxPerHour = maxPerTx + Math.floor(rand() * 5000);
      const maxPerDay = maxPerHour + Math.floor(rand() * 20000);
      const maxTxsPerHour = 1 + Math.floor(rand() * 20);

      const rateLimitState = makeRateLimit({
        maxPerTx: String(maxPerTx),
        maxPerHour: String(maxPerHour),
        maxPerDay: String(maxPerDay),
        maxTxsPerHour,
        hourlySpend: String(Math.floor(rand() * maxPerHour)),
        dailySpend: String(Math.floor(rand() * maxPerDay)),
        hourlyTxCount: Math.floor(rand() * (maxTxsPerHour + 2)),
        // Fixed, non-expiring windows: this pass isolates the amount/limit
        // comparisons from window-reset behavior (covered by the dedicated
        // window-reset tests above and the expiry pass below).
        hourWindowStartLedger: 0,
        dayWindowStartLedger: 0,
      });
      const amount = BigInt(Math.floor(rand() * 1500));
      const currentLedger = 1; // well within both non-expiring windows

      const expected = !shadowCheck(rateLimitState, amount);
      const actual = predictPaymentOutcome({
        rateLimitState,
        amount: amount.toString(),
        currentLedger,
      }).wouldBlock;

      expect(actual).toBe(expected);
    }
  });

  it('agrees with the shadow model when window expiry resets spend/count to zero', () => {
    const rand = makeRng(2);
    for (let i = 0; i < 500; i++) {
      const maxPerTx = 1 + Math.floor(rand() * 1000);
      const maxPerHour = maxPerTx + Math.floor(rand() * 5000);
      const maxPerDay = maxPerHour + Math.floor(rand() * 20000);
      const maxTxsPerHour = 1 + Math.floor(rand() * 20);
      const hourExpired = rand() < 0.5;
      const dayExpired = rand() < 0.5;

      const rateLimitState = makeRateLimit({
        maxPerTx: String(maxPerTx),
        maxPerHour: String(maxPerHour),
        maxPerDay: String(maxPerDay),
        maxTxsPerHour,
        hourlySpend: String(Math.floor(rand() * maxPerHour)),
        dailySpend: String(Math.floor(rand() * maxPerDay)),
        hourlyTxCount: Math.floor(rand() * (maxTxsPerHour + 2)),
        hourWindowStartLedger: hourExpired ? 0 : 1_000_000,
        dayWindowStartLedger: dayExpired ? 0 : 1_000_000,
      });
      const amount = BigInt(Math.floor(rand() * 1500));
      const currentLedger = 100_000; // past the "expired" windows, before the "fresh" ones

      // The shadow model (like the real contract) resets to 0 whenever the
      // window has expired, before comparing.
      const effective: RateLimitSpendState = {
        ...rateLimitState,
        hourlySpend: hourExpired ? '0' : rateLimitState.hourlySpend,
        hourlyTxCount: hourExpired ? 0 : rateLimitState.hourlyTxCount,
        dailySpend: dayExpired ? '0' : rateLimitState.dailySpend,
      };

      const expected = !shadowCheck(effective, amount);
      const actual = predictPaymentOutcome({
        rateLimitState,
        amount: amount.toString(),
        currentLedger,
      }).wouldBlock;

      expect(actual).toBe(expected);
    }
  });
});
