import { describe, expect, it, vi, afterEach } from 'vitest';
import {
  estimateLedgerCloseSeconds,
  estimateSecondsRemaining,
  fetchLedgerCloseEstimate,
  FALLBACK_LEDGER_CLOSE_SECONDS,
  type LedgerCloseSample,
} from '../ledgerTime.js';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('estimateLedgerCloseSeconds', () => {
  it('averages evenly-spaced 5s ledgers to 5', () => {
    const samples: LedgerCloseSample[] = [
      { sequence: 100, closedAt: '2024-01-01T00:00:00Z' },
      { sequence: 101, closedAt: '2024-01-01T00:00:05Z' },
      { sequence: 102, closedAt: '2024-01-01T00:00:10Z' },
      { sequence: 103, closedAt: '2024-01-01T00:00:15Z' },
    ];
    expect(estimateLedgerCloseSeconds(samples)).toBe(5);
  });

  it('weights by ledger count, not by pair, when one gap spans multiple ledgers', () => {
    // 10 ledgers in 30s (avg 3s/ledger) then 1 ledger in 3s: overall average
    // must be total-seconds / total-ledgers = 33 / 11 = 3, not the mean of
    // the two per-pair ratios.
    const samples: LedgerCloseSample[] = [
      { sequence: 100, closedAt: '2024-01-01T00:00:00Z' },
      { sequence: 110, closedAt: '2024-01-01T00:00:30Z' },
      { sequence: 111, closedAt: '2024-01-01T00:00:33Z' },
    ];
    expect(estimateLedgerCloseSeconds(samples)).toBe(3);
  });

  it('does not require samples to be pre-sorted', () => {
    const samples: LedgerCloseSample[] = [
      { sequence: 102, closedAt: '2024-01-01T00:00:10Z' },
      { sequence: 100, closedAt: '2024-01-01T00:00:00Z' },
      { sequence: 101, closedAt: '2024-01-01T00:00:05Z' },
    ];
    expect(estimateLedgerCloseSeconds(samples)).toBe(5);
  });

  it('falls back when fewer than two samples are given', () => {
    expect(estimateLedgerCloseSeconds([])).toBe(FALLBACK_LEDGER_CLOSE_SECONDS);
    expect(
      estimateLedgerCloseSeconds([{ sequence: 100, closedAt: '2024-01-01T00:00:00Z' }]),
    ).toBe(FALLBACK_LEDGER_CLOSE_SECONDS);
  });

  it('skips a duplicate/out-of-order pair (non-positive ledger delta)', () => {
    const samples: LedgerCloseSample[] = [
      { sequence: 100, closedAt: '2024-01-01T00:00:00Z' },
      { sequence: 100, closedAt: '2024-01-01T00:00:05Z' }, // duplicate sequence, skipped
      { sequence: 101, closedAt: '2024-01-01T00:00:10Z' },
    ];
    // Only the (100 -> 101, 10s) — wait, sorted order makes duplicates
    // adjacent; the usable pair is the second (100@:05 -> 101@:10, 5s/ledger).
    expect(estimateLedgerCloseSeconds(samples)).toBe(5);
  });

  it('skips a pair with an invalid/negative time delta', () => {
    const samples: LedgerCloseSample[] = [
      { sequence: 100, closedAt: 'not-a-date' },
      { sequence: 101, closedAt: '2024-01-01T00:00:05Z' },
      { sequence: 102, closedAt: '2024-01-01T00:00:10Z' },
    ];
    // First pair's delta is NaN and is skipped; second pair (101->102) gives 5s/ledger.
    expect(estimateLedgerCloseSeconds(samples)).toBe(5);
  });

  it('falls back when every pair is unusable', () => {
    const samples: LedgerCloseSample[] = [
      { sequence: 100, closedAt: 'garbage' },
      { sequence: 100, closedAt: 'garbage-2' },
    ];
    expect(estimateLedgerCloseSeconds(samples)).toBe(FALLBACK_LEDGER_CLOSE_SECONDS);
  });
});

describe('estimateSecondsRemaining', () => {
  it('multiplies ledgers remaining by the average close time', () => {
    expect(estimateSecondsRemaining(100, 5)).toBe(500);
  });

  it('is zero when no ledgers remain', () => {
    expect(estimateSecondsRemaining(0, 5)).toBe(0);
  });
});

describe('fetchLedgerCloseEstimate', () => {
  function mockFetchOk(records: Array<{ sequence: number; closed_at: string }>) {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: true,
        status: 200,
        json: async () => ({ _embedded: { records } }),
      })),
    );
  }

  it('derives currentLedger and an observed average from the fetched page', async () => {
    mockFetchOk([
      { sequence: 103, closed_at: '2024-01-01T00:00:15Z' },
      { sequence: 102, closed_at: '2024-01-01T00:00:10Z' },
      { sequence: 101, closed_at: '2024-01-01T00:00:05Z' },
      { sequence: 100, closed_at: '2024-01-01T00:00:00Z' },
    ]);

    const result = await fetchLedgerCloseEstimate('https://horizon.example.org');
    expect(result).toEqual({ currentLedger: 103, avgLedgerCloseSeconds: 5, observed: true });
  });

  it('requests order=desc with the given sample size and trims a trailing slash', async () => {
    const fetchMock = vi.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ _embedded: { records: [{ sequence: 1, closed_at: '2024-01-01T00:00:00Z' }] } }),
    }));
    vi.stubGlobal('fetch', fetchMock);

    await fetchLedgerCloseEstimate('https://horizon.example.org/', 7);

    expect(fetchMock).toHaveBeenCalledWith('https://horizon.example.org/ledgers?order=desc&limit=7');
  });

  it('reports observed: false with a single-ledger page (fallback average)', async () => {
    mockFetchOk([{ sequence: 500, closed_at: '2024-01-01T00:00:00Z' }]);

    const result = await fetchLedgerCloseEstimate('https://horizon.example.org');
    expect(result).toEqual({
      currentLedger: 500,
      avgLedgerCloseSeconds: FALLBACK_LEDGER_CLOSE_SECONDS,
      observed: false,
    });
  });

  it('throws when Horizon responds with a non-OK status', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: false, status: 503, json: async () => ({}) })),
    );

    await expect(fetchLedgerCloseEstimate('https://horizon.example.org')).rejects.toThrow(/503/);
  });

  it('throws when Horizon returns an empty ledgers page', async () => {
    mockFetchOk([]);

    await expect(fetchLedgerCloseEstimate('https://horizon.example.org')).rejects.toThrow(
      /no ledgers/,
    );
  });
});
