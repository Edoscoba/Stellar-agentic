import { useCallback, useEffect, useRef, useState } from 'react';
import { toStroops, type PayForAPIParams, type TxResult } from '@stellaragent/core';
import { useStellarAgent, usePendingPayments } from '../StellarAgentProvider.js';

export type PayForAPIStatus = 'idle' | 'pending' | 'success' | 'error';

export interface UsePayForAPIResult {
  /** Invoke a payment. Resolves/rejects the same as `StellarAgent.payForAPI`. */
  payForAPI: (params: PayForAPIParams) => Promise<TxResult>;
  status: PayForAPIStatus;
  error: Error | null;
  /** Back to `idle` — does not affect any in-flight call. */
  reset: () => void;
}

function toError(err: unknown): Error {
  return err instanceof Error ? err : new Error(String(err));
}

let nextPendingId = 0;

/**
 * Mutation hook for `StellarAgent.payForAPI`, with optimistic-update
 * support: as soon as `payForAPI(params)` is called, `params.amount` is
 * recorded in the provider's shared pending-payments state, so any
 * `useSpendReport()` mounted under the same `<StellarAgentProvider>`
 * immediately reflects the pending spend — before the transaction has
 * even been submitted, let alone confirmed.
 *
 * On settle (success *or* failure) the pending entry is removed and the
 * provider's spend-report version counter is bumped to force an
 * immediate refetch:
 * - On success, the next `getSpendReport()` call already reflects the
 *   confirmed payment server-side, so removing the optimistic entry and
 *   refetching hands off from "optimistic" to "confirmed" with no gap.
 * - On failure, nothing was ever applied server-side, so removing the
 *   optimistic entry *is* the rollback — the spend report reverts to
 *   whatever the last real poll said.
 */
export function usePayForAPI(): UsePayForAPIResult {
  const { agent } = useStellarAgent();
  const { addPending, removePending, bump } = usePendingPayments();
  const [status, setStatus] = useState<PayForAPIStatus>('idle');
  const [error, setError] = useState<Error | null>(null);
  const mountedRef = useRef(true);
  useEffect(
    () => () => {
      mountedRef.current = false;
    },
    [],
  );

  const payForAPI = useCallback(
    async (params: PayForAPIParams): Promise<TxResult> => {
      if (!agent) {
        const err = new Error('usePayForAPI: agent is not ready yet');
        setStatus('error');
        setError(err);
        throw err;
      }

      setStatus('pending');
      setError(null);

      const pendingId = `pending-${++nextPendingId}`;
      addPending({ id: pendingId, amountStroops: toStroops(params.amount) });

      try {
        const result = await agent.payForAPI(params);
        if (mountedRef.current) setStatus('success');
        return result;
      } catch (err) {
        const e = toError(err);
        if (mountedRef.current) {
          setStatus('error');
          setError(e);
        }
        throw e;
      } finally {
        removePending(pendingId);
        bump();
      }
    },
    [agent, addPending, removePending, bump],
  );

  const reset = useCallback(() => {
    setStatus('idle');
    setError(null);
  }, []);

  return { payForAPI, status, error, reset };
}
