import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { usePolling } from '../internal/usePolling.js';

// `@testing-library/react`'s `waitFor` polls via `setTimeout`, which is
// exactly what fake timers freeze — so under `vi.useFakeTimers()` we
// advance time explicitly (inside `act`) and assert directly, rather than
// mixing in `waitFor`.
async function flush() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('usePolling', () => {
  it('stays idle when fetcher is null', () => {
    const { result } = renderHook(() => usePolling<number>(null));
    expect(result.current.status).toBe('idle');
    expect(result.current.data).toBeNull();
  });

  it('fetches immediately, then again on the interval', async () => {
    const fetcher = vi.fn().mockResolvedValue(1);
    const { result } = renderHook(() => usePolling(fetcher, { intervalMs: 1000 }));

    await flush();
    expect(result.current.status).toBe('ready');
    expect(result.current.data).toBe(1);
    expect(fetcher).toHaveBeenCalledTimes(1);

    fetcher.mockResolvedValue(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(result.current.data).toBe(2);
  });

  it('exposes a manual refetch that bypasses the interval', async () => {
    const fetcher = vi.fn().mockResolvedValue('a');
    const { result } = renderHook(() => usePolling(fetcher, { intervalMs: 60_000 }));

    await flush();
    expect(fetcher).toHaveBeenCalledTimes(1);

    fetcher.mockResolvedValue('b');
    await act(async () => {
      result.current.refetch();
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(result.current.data).toBe('b');
  });

  it('surfaces a rejected fetch as an error state', async () => {
    const fetcher = vi.fn().mockRejectedValue(new Error('boom'));
    const { result } = renderHook(() => usePolling(fetcher));

    await flush();
    expect(result.current.status).toBe('error');
    expect(result.current.error?.message).toBe('boom');
    expect(result.current.data).toBeNull();
  });

  it('stops polling after unmount', async () => {
    const fetcher = vi.fn().mockResolvedValue(1);
    const { result, unmount } = renderHook(() => usePolling(fetcher, { intervalMs: 1000 }));

    await flush();
    expect(result.current.status).toBe('ready');
    expect(fetcher).toHaveBeenCalledTimes(1);

    unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});
