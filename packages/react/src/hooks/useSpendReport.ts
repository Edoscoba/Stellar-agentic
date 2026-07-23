import { useCallback, useMemo } from 'react';
import { add, bn, clamp, fromStroops, sub, toStr, type SpendReport } from '@stellaragent/core';
import { useStellarAgent, usePendingPayments, type PendingPayment } from '../StellarAgentProvider.js';
import { usePolling, type UsePollingOptions, type UsePollingResult } from '../internal/usePolling.js';

/**
 * Overlays optimistic pending-payment amounts (in settlement-asset units)
 * on top of a polled `SpendReport`, so a payment that's in flight but not
 * yet confirmed is already reflected: `spentThisPeriod` and
 * `totalLifetime` go up, `remainingThisPeriod` goes down (floored at 0,
 * since the real on-chain limit can never go negative — if pending
 * amounts alone would push it below zero that just means the true
 * remaining balance is about to hit zero once they land).
 *
 * Uses `@stellaragent/core`'s deterministic fixed-point helpers rather
 * than native arithmetic, per that module's own rule: these are monetary
 * values and must never touch a JS float.
 */
function applyPending(report: SpendReport, pending: PendingPayment[]): SpendReport {
  // Always routed through the same fixed-point formatting, even with no
  // pending payments (pendingTotal = 0), so callers see a consistent
  // decimal-places convention regardless of whether an overlay is active.
  let pendingTotal = bn('0');
  for (const payment of pending) {
    pendingTotal = add(pendingTotal, fromStroops(payment.amountStroops));
  }

  return {
    spentThisPeriod: toStr(add(report.spentThisPeriod, pendingTotal)),
    remainingThisPeriod: toStr(
      clamp(sub(report.remainingThisPeriod, pendingTotal), '0', report.remainingThisPeriod),
    ),
    totalLifetime: toStr(add(report.totalLifetime, pendingTotal)),
  };
}

export interface UseSpendReportResult extends UsePollingResult<SpendReport> {
  /** Whether `data` currently includes unconfirmed optimistic payments. */
  hasPendingPayments: boolean;
}

/**
 * Polls `PaymentChannel.remaining_this_period` (and friends) for the
 * agent's active channel, with any in-flight `usePayForAPI()` payments
 * from anywhere else in the tree overlaid optimistically — see
 * `applyPending`. Reconciles automatically: `usePayForAPI` bumps a shared
 * version counter on settle, which forces every mounted `useSpendReport`
 * to refetch immediately rather than waiting out the poll interval.
 */
export function useSpendReport(options?: UsePollingOptions): UseSpendReportResult {
  const { agent, status } = useStellarAgent();
  const { pending, version } = usePendingPayments();

  const fetcher = useCallback(() => {
    if (!agent) {
      return Promise.reject(new Error('useSpendReport: agent not ready'));
    }
    return agent.getSpendReport();
    // `version` is a deliberate dependency, not used in the body: bumping
    // it gives this callback a new identity, which is `usePolling`'s
    // signal to refetch immediately instead of waiting for the interval.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agent, version]);

  const enabled = Boolean(agent) && status === 'ready';
  const poll = usePolling(enabled ? fetcher : null, options);

  const data = useMemo(
    () => (poll.data ? applyPending(poll.data, pending) : poll.data),
    [poll.data, pending],
  );

  return { ...poll, data, hasPendingPayments: pending.length > 0 };
}
