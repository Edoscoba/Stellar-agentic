import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import type { ChannelInfo } from '@stellaragent/core';
import { StellarAgentProvider } from '../StellarAgentProvider.js';
import { useChannel } from '../hooks/useChannel.js';
import { createMockAgent } from '../test/mockAgent.js';

const channel: ChannelInfo = {
  id: 1n,
  agent: 'GAGENT',
  owner: 'GOWNER',
  token: 'USDC',
  limitPerPeriod: 1_000_0000000n,
  spentThisPeriod: 250_0000000n,
  totalSpent: 750_0000000n,
  active: true,
};

// `@testing-library/react`'s `waitFor` polls via `setTimeout`, which is
// exactly what fake timers freeze — so under `vi.useFakeTimers()` we
// advance time explicitly (inside `act`) and assert directly.
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

describe('useChannel', () => {
  it('stays idle when channelId is undefined', () => {
    const mockAgent = createMockAgent();
    const { result } = renderHook(() => useChannel(undefined), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    expect(result.current.status).toBe('idle');
    expect(mockAgent.getChannel).not.toHaveBeenCalled();
  });

  it('loads and returns channel data', async () => {
    const mockAgent = createMockAgent({ getChannel: async () => channel });

    const { result } = renderHook(() => useChannel(1n), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await flush();
    expect(result.current.status).toBe('ready');
    expect(result.current.data).toEqual(channel);
    expect(mockAgent.getChannel).toHaveBeenCalledWith(1n);
  });

  it('surfaces errors from the agent', async () => {
    const mockAgent = createMockAgent({
      getChannel: async () => {
        throw new Error('channel not found');
      },
    });

    const { result } = renderHook(() => useChannel(99n), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await flush();
    expect(result.current.status).toBe('error');
    expect(result.current.error?.message).toBe('channel not found');
    expect(result.current.data).toBeNull();
  });

  it('re-polls on the configured interval and stops after unmount', async () => {
    const getChannel = vi.fn(async () => channel);
    const mockAgent = createMockAgent({ getChannel });

    const { result, unmount } = renderHook(() => useChannel(1n, { intervalMs: 1000 }), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await flush();
    expect(result.current.status).toBe('ready');
    expect(getChannel).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(getChannel).toHaveBeenCalledTimes(2);

    unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(getChannel).toHaveBeenCalledTimes(2);
  });
});
