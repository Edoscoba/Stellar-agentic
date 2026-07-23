import { describe, expect, it } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { SpendReport, TxResult } from '@stellaragent/core';
import { StellarAgentProvider } from '../StellarAgentProvider.js';
import { useSpendReport } from '../hooks/useSpendReport.js';
import { usePayForAPI } from '../hooks/usePayForAPI.js';
import { createMockAgent } from '../test/mockAgent.js';

const baseline: SpendReport = {
  spentThisPeriod: '2.5',
  remainingThisPeriod: '7.5',
  totalLifetime: '2.5',
};

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

function createDeferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function useCombined() {
  return { spend: useSpendReport(), pay: usePayForAPI() };
}

describe('optimistic usePayForAPI + useSpendReport', () => {
  it('reflects a pending payment immediately, then reconciles with the confirmed total on success', async () => {
    let currentReport = baseline;
    const getSpendReport = async () => currentReport;
    const payDeferred = createDeferred<TxResult>();

    const mockAgent = createMockAgent({
      getSpendReport,
      payForAPI: () => payDeferred.promise,
    });

    const { result } = renderHook(() => useCombined(), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await waitFor(() => expect(result.current.spend.status).toBe('ready'));
    expect(result.current.spend.data).toEqual({
      spentThisPeriod: '2.5000000',
      remainingThisPeriod: '7.5000000',
      totalLifetime: '2.5000000',
    });
    expect(result.current.spend.hasPendingPayments).toBe(false);

    // Fire the mutation but don't await it yet — the underlying
    // agent.payForAPI call is still in flight (payDeferred unsettled).
    let payPromise!: Promise<TxResult>;
    act(() => {
      payPromise = result.current.pay.payForAPI({ endpoint: '/infer', amount: '1.0' });
    });

    expect(result.current.pay.status).toBe('pending');
    // Optimistic overlay applied immediately, well before confirmation.
    expect(result.current.spend.hasPendingPayments).toBe(true);
    expect(result.current.spend.data?.spentThisPeriod).toBe('3.5000000');
    expect(result.current.spend.data?.remainingThisPeriod).toBe('6.5000000');
    expect(result.current.spend.data?.totalLifetime).toBe('3.5000000');

    // "Confirm" the transaction: the server-side spend report now reflects
    // it for real, and the mock resolves.
    currentReport = {
      spentThisPeriod: '3.5',
      remainingThisPeriod: '6.5',
      totalLifetime: '3.5',
    };
    await act(async () => {
      payDeferred.resolve({ hash: '0xabc', success: true });
      await payPromise;
    });

    await waitFor(() => expect(result.current.pay.status).toBe('success'));
    // Pending entry cleared and reconciled against the confirmed number —
    // not double-counted (would be '4.5000000' if it were).
    await waitFor(() => expect(result.current.spend.hasPendingPayments).toBe(false));
    expect(result.current.spend.data?.spentThisPeriod).toBe('3.5000000');
  });

  it('rolls back the optimistic update when the payment fails', async () => {
    const getSpendReport = async () => baseline;
    const payDeferred = createDeferred<TxResult>();

    const mockAgent = createMockAgent({
      getSpendReport,
      payForAPI: () => payDeferred.promise,
    });

    const { result } = renderHook(() => useCombined(), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await waitFor(() => expect(result.current.spend.status).toBe('ready'));

    let payPromise!: Promise<TxResult>;
    act(() => {
      payPromise = result.current.pay.payForAPI({ endpoint: '/infer', amount: '1.0' });
    });
    payPromise.catch(() => {
      // Expected — asserted via `pay.status`/`pay.error` below.
    });

    expect(result.current.spend.data?.spentThisPeriod).toBe('3.5000000');

    await act(async () => {
      payDeferred.reject(new Error('insufficient funds'));
      await payPromise.catch(() => {});
    });

    await waitFor(() => expect(result.current.pay.status).toBe('error'));
    expect(result.current.pay.error?.message).toBe('insufficient funds');

    // Rolled back: no pending overlay, numbers back to the last real poll.
    expect(result.current.spend.hasPendingPayments).toBe(false);
    expect(result.current.spend.data).toEqual({
      spentThisPeriod: '2.5000000',
      remainingThisPeriod: '7.5000000',
      totalLifetime: '2.5000000',
    });
  });
});
