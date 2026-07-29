import { describe, expect, it, vi, afterEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { StellarAgent, type ChannelInfo, type RateLimitStatus } from '@stellaragent/core';
import { StellarAgentProvider } from '../StellarAgentProvider.js';
import { useRateLimitStatus } from '../hooks/useRateLimitStatus.js';
import { createMockAgent } from '../test/mockAgent.js';

afterEach(() => {
  vi.restoreAllMocks();
});

const AGENT_ADDRESS = 'GAGENTXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX';

const configuredStatus: RateLimitStatus = {
  configured: true,
  active: true,
  maxPerTx: '10',
  maxPerHour: '50',
  maxPerDay: '200',
  maxTxsPerHour: 100,
  spentThisHour: '12.5',
  spentToday: '40',
  txsThisHour: 8,
  hourWindowStartLedger: 1000,
  dayWindowStartLedger: 1000,
};

const unconfiguredStatus: RateLimitStatus = {
  configured: false,
  active: true,
  maxPerTx: '0',
  maxPerHour: '0',
  maxPerDay: '0',
  maxTxsPerHour: 0,
  spentThisHour: '0',
  spentToday: '0',
  txsThisHour: 0,
  hourWindowStartLedger: 0,
  dayWindowStartLedger: 0,
};

const channel: ChannelInfo = {
  id: 1n,
  agent: AGENT_ADDRESS,
  owner: 'GOWNER',
  token: 'USDC',
  limitPerPeriod: 100_0000000n,
  spentThisPeriod: 10_0000000n,
  totalSpent: 10_0000000n,
  active: true,
  period: 'hourly',
  periodStartLedger: 1000,
};

const ledgerEstimate = { currentLedger: 1100, avgLedgerCloseSeconds: 5, observed: true };

function renderStatus(
  overrides: Parameters<typeof createMockAgent>[0],
  agentAddress: string = AGENT_ADDRESS,
  options?: Parameters<typeof useRateLimitStatus>[1],
) {
  const mockAgent = createMockAgent(overrides);
  return renderHook(() => useRateLimitStatus(agentAddress, options), {
    wrapper: ({ children }) => (
      <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
        {children}
      </StellarAgentProvider>
    ),
  });
}

describe('useRateLimitStatus', () => {
  it('is disabled until the agent is ready', () => {
    // Never resolves, so the provider stays in 'loading' for the whole
    // test — isolates this test from StellarAgent.create's real behavior
    // and avoids a state update landing after the test has finished.
    vi.spyOn(StellarAgent, 'create').mockReturnValue(new Promise(() => {}));

    const { result } = renderHook(() => useRateLimitStatus(AGENT_ADDRESS), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }}>{children}</StellarAgentProvider>
      ),
    });

    expect(result.current.status).toBe('idle');
  });

  it('loads rate-limit status (with no channel) once the agent is ready', async () => {
    const { result } = renderStatus({
      getRateLimitStatus: async () => configuredStatus,
      getLedgerCloseEstimate: async () => ledgerEstimate,
    });

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.data?.rateLimit).toEqual(configuredStatus);
    expect(result.current.data?.channel).toBeNull();
    expect(result.current.data?.channelPeriodWindow).toBeNull();
  });

  it('queries getRateLimitStatus with the given agentAddress', async () => {
    const spy = vi.fn(async () => configuredStatus);
    renderStatus({ getRateLimitStatus: spy, getLedgerCloseEstimate: async () => ledgerEstimate });

    await waitFor(() => expect(spy).toHaveBeenCalledWith(AGENT_ADDRESS));
  });

  it('distinguishes unconfigured (unrestricted) from configured-and-exhausted', async () => {
    const { result } = renderStatus({
      getRateLimitStatus: async () => unconfiguredStatus,
      getLedgerCloseEstimate: async () => ledgerEstimate,
    });

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.data?.rateLimitConfigured).toBe(false);
    // Unconfigured means no rate-limit block is possible, regardless of amount.
    expect(result.current.data?.wouldBlock('1000000')).toBe(false);
  });

  it('reports rateLimitKilled distinctly from unconfigured', async () => {
    const killedStatus: RateLimitStatus = { ...configuredStatus, active: false };
    const { result } = renderStatus({
      getRateLimitStatus: async () => killedStatus,
      getLedgerCloseEstimate: async () => ledgerEstimate,
    });

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.data?.rateLimitConfigured).toBe(true);
    expect(result.current.data?.rateLimitKilled).toBe(true);
  });

  it('folds in channel state and computes wouldBlock/predict when channelId is given', async () => {
    const { result } = renderStatus(
      {
        getRateLimitStatus: async () => configuredStatus,
        getChannel: async () => channel,
        getLedgerCloseEstimate: async () => ledgerEstimate,
      },
      AGENT_ADDRESS,
      { channelId: 1n },
    );

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.data?.channel).toEqual(channel);
    expect(result.current.data?.channelPeriodWindow).not.toBeNull();

    // Under every limit.
    expect(result.current.data?.wouldBlock('1')).toBe(false);
    // Exceeds the rate limiter's configured per-tx cap (maxPerTx: '10').
    const prediction = result.current.data?.predict('20');
    expect(prediction?.wouldBlock).toBe(true);
    expect(prediction?.reasons).toContain('rate_limit_per_tx');
  });

  it('exposes ledger-count and estimated-seconds-remaining forms for both rate-limit windows', async () => {
    const { result } = renderStatus({
      getRateLimitStatus: async () => configuredStatus,
      getLedgerCloseEstimate: async () => ledgerEstimate,
    });

    await waitFor(() => expect(result.current.status).toBe('ready'));
    const { hourWindow, dayWindow } = result.current.data!;
    expect(hourWindow.ledgersRemaining).toBeGreaterThanOrEqual(0);
    expect(hourWindow.estimatedSecondsRemaining).toBe(
      hourWindow.ledgersRemaining * ledgerEstimate.avgLedgerCloseSeconds,
    );
    expect(dayWindow.ledgersRemaining).toBeGreaterThanOrEqual(0);
    expect(dayWindow.estimatedSecondsRemaining).toBe(
      dayWindow.ledgersRemaining * ledgerEstimate.avgLedgerCloseSeconds,
    );
  });
});
