/**
 * Wall-clock estimation for Stellar's ledger-sequence-based windows.
 *
 * `RateLimiter` (`contracts/rate_limiter/src/lib.rs`) and `PaymentChannel`
 * (`contracts/payment_channel/src/lib.rs`) both track their rolling windows
 * in **ledger sequence numbers**, not timestamps — `hour_window_start`,
 * `day_window_start`, `period_start_ledger`. Ledgers close roughly every 5
 * seconds, but that number drifts with network conditions and is not
 * contractually guaranteed, so hard-coding "5 seconds" would silently
 * mislead a UI showing "resets in ~N seconds" whenever the real network runs
 * faster or slower than that assumption.
 *
 * This module instead derives an actual average close time from a handful of
 * recently observed ledgers (via Horizon's `/ledgers` endpoint) and uses that
 * to convert a ledger count into an estimated number of seconds. Every value
 * this produces is explicitly an estimate — never treat
 * `estimateSecondsRemaining`'s result as exact.
 *
 * @module ledgerTime
 */

/** A single observed ledger close, as needed to derive an average close time. */
export interface LedgerCloseSample {
  sequence: number;
  /** ISO 8601 timestamp, as returned by Horizon's `closed_at` field. */
  closedAt: string;
}

/**
 * Fallback average ledger close time, in seconds, used only when fewer than
 * two samples are available to derive a real observed average from (e.g. a
 * brand new standalone network with a single ledger closed so far). This is
 * the commonly cited Stellar figure, but it is a fallback, not a
 * measurement — prefer {@link estimateLedgerCloseSeconds} against real
 * samples whenever they're available.
 */
export const FALLBACK_LEDGER_CLOSE_SECONDS = 5;

/**
 * Derive the observed average seconds-per-ledger from a set of recent ledger
 * close samples, by summing the wall-clock gaps between consecutive
 * sequences and dividing by the total number of ledgers those gaps span
 * (rather than simply averaging per-pair ratios, so a single irregular gap
 * doesn't get equal weight against many one-ledger gaps).
 *
 * Samples need not be pre-sorted or contiguous. Any pair with a
 * non-positive ledger delta or a negative/invalid time delta is skipped —
 * defensive against a misbehaving RPC provider returning out-of-order or
 * duplicate records — and falls back to {@link FALLBACK_LEDGER_CLOSE_SECONDS}
 * if fewer than two usable samples remain after that filtering.
 */
export function estimateLedgerCloseSeconds(samples: readonly LedgerCloseSample[]): number {
  const sorted = [...samples].sort((a, b) => a.sequence - b.sequence);

  let totalSeconds = 0;
  let totalLedgers = 0;

  for (let i = 1; i < sorted.length; i++) {
    const prev = sorted[i - 1]!;
    const curr = sorted[i]!;
    const ledgerDelta = curr.sequence - prev.sequence;
    if (ledgerDelta <= 0) continue;

    const prevMs = Date.parse(prev.closedAt);
    const currMs = Date.parse(curr.closedAt);
    const secondsDelta = (currMs - prevMs) / 1000;
    if (!Number.isFinite(secondsDelta) || secondsDelta < 0) continue;

    totalSeconds += secondsDelta;
    totalLedgers += ledgerDelta;
  }

  if (totalLedgers === 0) {
    return FALLBACK_LEDGER_CLOSE_SECONDS;
  }
  return totalSeconds / totalLedgers;
}

/**
 * Convert a ledger count into an estimated number of wall-clock seconds
 * using an already-derived average close time. Purely `ledgers *
 * avgLedgerCloseSeconds` — split out from {@link estimateLedgerCloseSeconds}
 * so callers (e.g. `useRateLimitStatus`) can recompute this on every render
 * as `ledgersRemaining` ticks down without re-deriving the average each time.
 */
export function estimateSecondsRemaining(
  ledgersRemaining: number,
  avgLedgerCloseSeconds: number,
): number {
  return ledgersRemaining * avgLedgerCloseSeconds;
}

/** Shape of the fields this module reads off a Horizon `/ledgers` record. */
interface HorizonLedgerRecord {
  sequence: number;
  closed_at: string;
}

interface HorizonLedgersPage {
  _embedded?: {
    records?: HorizonLedgerRecord[];
  };
}

export interface LedgerCloseEstimate {
  /** The highest ledger sequence among the fetched samples — i.e. the current tip. */
  currentLedger: number;
  /** Observed (or, absent enough samples, fallback) average seconds per ledger. */
  avgLedgerCloseSeconds: number;
  /**
   * `true` when `avgLedgerCloseSeconds` came from real observed ledger
   * closes; `false` when there weren't enough samples and the
   * {@link FALLBACK_LEDGER_CLOSE_SECONDS} constant was used instead. Surface
   * this alongside any "resets in ~N seconds" display so it's clear when the
   * estimate is a network measurement versus a rough guess.
   */
  observed: boolean;
}

/**
 * Fetch the most recent `sampleSize` ledgers from Horizon and derive both
 * the current ledger sequence and an observed average close time from them
 * — a single round trip covers everything a caller needs to turn a
 * ledger-count window into a wall-clock estimate.
 */
export async function fetchLedgerCloseEstimate(
  horizonUrl: string,
  sampleSize = 20,
): Promise<LedgerCloseEstimate> {
  const base = horizonUrl.replace(/\/+$/, '');
  const response = await fetch(`${base}/ledgers?order=desc&limit=${sampleSize}`);
  if (!response.ok) {
    throw new Error(
      `fetchLedgerCloseEstimate: Horizon request failed with status ${response.status}`,
    );
  }

  const page = (await response.json()) as HorizonLedgersPage;
  const records = page._embedded?.records ?? [];
  if (records.length === 0) {
    throw new Error('fetchLedgerCloseEstimate: Horizon returned no ledgers');
  }

  const samples: LedgerCloseSample[] = records.map((r) => ({
    sequence: r.sequence,
    closedAt: r.closed_at,
  }));

  return {
    currentLedger: Math.max(...samples.map((s) => s.sequence)),
    avgLedgerCloseSeconds: estimateLedgerCloseSeconds(samples),
    observed: samples.length >= 2,
  };
}
