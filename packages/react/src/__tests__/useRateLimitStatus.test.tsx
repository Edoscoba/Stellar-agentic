import { describe, expect, it, vi, afterEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { StellarAgent, type RateLimitStatus } from '@stellaragent/core';
import { StellarAgentProvider } from '../StellarAgentProvider.js';
import { useRateLimitStatus } from '../hooks/useRateLimitStatus.js';
import { createMockAgent } from '../test/mockAgent.js';

afterEach(() => {
  vi.restoreAllMocks();
});

const status: RateLimitStatus = {
  maxPerTx: '10',
  maxPerHour: '50',
  maxPerDay: '200',
  maxTxsPerHour: 100,
  spentThisHour: '12.5',
  spentToday: '40',
  txsThisHour: 8,
};

describe('useRateLimitStatus', () => {
  it('is disabled until the agent is ready', () => {
    // Never resolves, so the provider stays in 'loading' for the whole
    // test — isolates this test from StellarAgent.create's real behavior
    // and avoids a state update landing after the test has finished.
    vi.spyOn(StellarAgent, 'create').mockReturnValue(new Promise(() => {}));

    const { result } = renderHook(() => useRateLimitStatus(), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }}>{children}</StellarAgentProvider>
      ),
    });

    expect(result.current.status).toBe('idle');
  });

  it('loads rate-limit status once the agent is ready', async () => {
    const mockAgent = createMockAgent({ getRateLimitStatus: async () => status });

    const { result } = renderHook(() => useRateLimitStatus(), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.data).toEqual(status);
  });
});
