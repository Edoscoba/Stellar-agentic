import { useCallback, useEffect, useRef, useState } from 'react';

export type AsyncStatus = 'idle' | 'loading' | 'ready' | 'error';

export interface AsyncState<T> {
  data: T | null;
  status: AsyncStatus;
  error: Error | null;
}

export interface UsePollingOptions {
  /** Poll interval in ms. Default 5000. */
  intervalMs?: number;
  /** Skip fetching entirely (e.g. a dependency isn't ready yet). Default true. */
  enabled?: boolean;
}

export interface UsePollingResult<T> extends AsyncState<T> {
  /** Fetch immediately, outside the regular interval. */
  refetch: () => void;
}

function toError(err: unknown): Error {
  return err instanceof Error ? err : new Error(String(err));
}

/**
 * Polls `fetcher` on an interval, exposing `idle` -> `loading` -> `ready` |
 * `error` state and a manual `refetch`.
 *
 * `fetcher`'s identity is the effect's dependency key: hooks built on top
 * of this should build it with `useCallback` over their real dependencies
 * (e.g. `agent`, `channelId`) so a poll cycle only restarts when those
 * actually change, not on every render. Passing `null` disables polling
 * (e.g. while the agent isn't ready yet) without unmounting the caller.
 *
 * Guards against the two classic polling bugs: no `setState` after
 * unmount (the interval's `cancelled` flag), and no stale-response races
 * when `fetcher` changes mid-flight (the monotonic `requestId` ref —
 * only the most recently issued request's result is ever applied).
 */
export function usePolling<T>(
  fetcher: (() => Promise<T>) | null,
  { intervalMs = 5000, enabled = true }: UsePollingOptions = {},
): UsePollingResult<T> {
  const [state, setState] = useState<AsyncState<T>>({
    data: null,
    status: 'idle',
    error: null,
  });
  const requestIdRef = useRef(0);

  const runOnce = useCallback(async () => {
    if (!fetcher) return;
    const requestId = ++requestIdRef.current;
    setState((s) => ({ ...s, status: 'loading' }));
    try {
      const data = await fetcher();
      if (requestIdRef.current === requestId) {
        setState({ data, status: 'ready', error: null });
      }
    } catch (err) {
      if (requestIdRef.current === requestId) {
        setState((s) => ({ ...s, status: 'error', error: toError(err) }));
      }
    }
  }, [fetcher]);

  useEffect(() => {
    if (!fetcher || !enabled) {
      setState({ data: null, status: 'idle', error: null });
      return;
    }

    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const tick = async () => {
      await runOnce();
      if (!cancelled) {
        timer = setTimeout(tick, intervalMs);
      }
    };

    void tick();

    return () => {
      cancelled = true;
      // Invalidate any request still in flight so its response can never
      // land after this effect has torn down. Intentionally mutates the
      // ref's *current* value at cleanup time (not a snapshot) — this is
      // a plain incrementing counter, not a DOM-node ref, so the usual
      // "value may be stale by cleanup time" concern doesn't apply.
      // eslint-disable-next-line react-hooks/exhaustive-deps
      requestIdRef.current++;
      if (timer) clearTimeout(timer);
    };
  }, [fetcher, enabled, intervalMs, runOnce]);

  return { ...state, refetch: runOnce };
}
