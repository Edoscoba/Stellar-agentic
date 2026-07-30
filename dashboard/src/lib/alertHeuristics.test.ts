/**
 * Alert Heuristics Unit Tests — Issue #257
 * Pure function tests, no UI/DOM dependencies.
 * Run with: pnpm test:unit (vitest) from dashboard/
 */

import {
  detectSpendVelocityAnomaly,
  detectRepeatedNearLimitActivity,
  detectRateLimitHits,
  detectAgentKilled,
  runAllHeuristics,
  DEFAULT_THRESHOLDS,
  AgentEvent,
  AlertThresholds,
} from './alertHeuristics';

const NOW = new Date('2025-01-15T12:00:00Z').getTime();
const ONE_HOUR = 60 * 60 * 1000;

// ─── Spend Velocity Anomaly ────────────────────────────────────────────────────

describe('detectSpendVelocityAnomaly', () => {
  it('fires when current hour spend exceeds multiplier × historical avg', () => {
    const events: AgentEvent[] = [
      // Historical: 5 hourly windows, ~0.10 USDC/hr
      ...Array.from({ length: 5 }, (_, i) => ({
        agentId: 'agent-1',
        agentName: 'Test Agent',
        kind: 'payment' as const,
        amount: 0.10,
        windowTs: NOW - ONE_HOUR * (i + 2),
      })),
      // Current hour: 0.50 USDC — 5× the avg
      {
        agentId: 'agent-1',
        agentName: 'Test Agent',
        kind: 'payment' as const,
        amount: 0.50,
        windowTs: NOW - 10 * 60 * 1000,
      },
    ];

    const alerts = detectSpendVelocityAnomaly(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts).toHaveLength(1);
    expect(alerts[0].kind).toBe('spend_velocity_anomaly');
    expect(alerts[0].agentId).toBe('agent-1');
  });

  it('does NOT fire when spend is within normal range', () => {
    const events: AgentEvent[] = [
      ...Array.from({ length: 5 }, (_, i) => ({
        agentId: 'agent-2',
        agentName: 'Calm Agent',
        kind: 'payment' as const,
        amount: 0.20,
        windowTs: NOW - ONE_HOUR * (i + 2),
      })),
      {
        agentId: 'agent-2',
        agentName: 'Calm Agent',
        kind: 'payment' as const,
        amount: 0.22, // only 1.1× avg — within threshold
        windowTs: NOW - 5 * 60 * 1000,
      },
    ];

    const alerts = detectSpendVelocityAnomaly(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts).toHaveLength(0);
  });

  it('fires critical severity when ratio >= 3× multiplier', () => {
    const events: AgentEvent[] = [
      ...Array.from({ length: 5 }, (_, i) => ({
        agentId: 'agent-3',
        agentName: 'Spike Agent',
        kind: 'payment' as const,
        amount: 0.10,
        windowTs: NOW - ONE_HOUR * (i + 2),
      })),
      {
        agentId: 'agent-3',
        agentName: 'Spike Agent',
        kind: 'payment' as const,
        amount: 5.00, // 50× avg — critical
        windowTs: NOW - 2 * 60 * 1000,
      },
    ];

    const alerts = detectSpendVelocityAnomaly(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts[0].severity).toBe('critical');
  });

  it('handles multiple agents independently', () => {
    const events: AgentEvent[] = [
      // Agent A: normal
      ...Array.from({ length: 5 }, (_, i) => ({
        agentId: 'agent-a',
        agentName: 'Agent A',
        kind: 'payment' as const,
        amount: 0.10,
        windowTs: NOW - ONE_HOUR * (i + 2),
      })),
      { agentId: 'agent-a', agentName: 'Agent A', kind: 'payment' as const, amount: 0.12, windowTs: NOW - 5 * 60 * 1000 },
      // Agent B: anomalous
      ...Array.from({ length: 5 }, (_, i) => ({
        agentId: 'agent-b',
        agentName: 'Agent B',
        kind: 'payment' as const,
        amount: 0.05,
        windowTs: NOW - ONE_HOUR * (i + 2),
      })),
      { agentId: 'agent-b', agentName: 'Agent B', kind: 'payment' as const, amount: 1.50, windowTs: NOW - 5 * 60 * 1000 },
    ];

    const alerts = detectSpendVelocityAnomaly(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts).toHaveLength(1);
    expect(alerts[0].agentId).toBe('agent-b');
  });
});

// ─── Near Limit Repeated Activity ─────────────────────────────────────────────

describe('detectRepeatedNearLimitActivity', () => {
  it('fires when agent is near limit for N consecutive windows', () => {
    const events: AgentEvent[] = Array.from({ length: 4 }, (_, i) => ({
      agentId: 'agent-1',
      agentName: 'Busy Agent',
      kind: 'payment' as const,
      hourlyTxCount: 90, // 90% of 100 — above 85% threshold
      maxTxsPerHour: 100,
      windowTs: NOW - ONE_HOUR * (4 - i),
    }));

    const alerts = detectRepeatedNearLimitActivity(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts).toHaveLength(1);
    expect(alerts[0].kind).toBe('near_limit_repeated');
  });

  it('does NOT fire when only 2 of 3 windows are near limit', () => {
    const events: AgentEvent[] = [
      { agentId: 'agent-2', agentName: 'Mixed Agent', kind: 'payment' as const, hourlyTxCount: 50, maxTxsPerHour: 100, windowTs: NOW - 3 * ONE_HOUR },
      { agentId: 'agent-2', agentName: 'Mixed Agent', kind: 'payment' as const, hourlyTxCount: 90, maxTxsPerHour: 100, windowTs: NOW - 2 * ONE_HOUR },
      { agentId: 'agent-2', agentName: 'Mixed Agent', kind: 'payment' as const, hourlyTxCount: 92, maxTxsPerHour: 100, windowTs: NOW - ONE_HOUR },
    ];

    const alerts = detectRepeatedNearLimitActivity(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts).toHaveLength(0);
  });

  it('respects custom nearLimitFraction threshold', () => {
    const customThresholds: AlertThresholds = { ...DEFAULT_THRESHOLDS, nearLimitFraction: 0.5 };
    const events: AgentEvent[] = Array.from({ length: 3 }, (_, i) => ({
      agentId: 'agent-3',
      agentName: 'Agent 3',
      kind: 'payment' as const,
      hourlyTxCount: 55, // 55% — above 50% custom threshold
      maxTxsPerHour: 100,
      windowTs: NOW - ONE_HOUR * (3 - i),
    }));

    const alerts = detectRepeatedNearLimitActivity(events, customThresholds, NOW);
    expect(alerts).toHaveLength(1);
  });
});

// ─── Rate Limit Hits ──────────────────────────────────────────────────────────

describe('detectRateLimitHits', () => {
  it('creates an alert for each rate_limit_hit event', () => {
    const events: AgentEvent[] = [
      { agentId: 'a1', agentName: 'Agent 1', kind: 'rate_limit_hit', windowTs: NOW - 1000 },
      { agentId: 'a1', agentName: 'Agent 1', kind: 'rate_limit_hit', windowTs: NOW - 500 },
      { agentId: 'a2', agentName: 'Agent 2', kind: 'payment', windowTs: NOW - 200 },
    ];

    const alerts = detectRateLimitHits(events, NOW);
    expect(alerts).toHaveLength(2);
    expect(alerts.every((a) => a.kind === 'rate_limit_hit')).toBe(true);
  });
});

// ─── Agent Killed ─────────────────────────────────────────────────────────────

describe('detectAgentKilled', () => {
  it('fires critical alert for agent_killed events', () => {
    const events: AgentEvent[] = [
      { agentId: 'dying-agent', agentName: 'Dying Agent', kind: 'agent_killed', windowTs: NOW },
    ];

    const alerts = detectAgentKilled(events);
    expect(alerts).toHaveLength(1);
    expect(alerts[0].severity).toBe('critical');
    expect(alerts[0].kind).toBe('agent_killed');
  });
});

// ─── Composite Runner ─────────────────────────────────────────────────────────

describe('runAllHeuristics', () => {
  it('returns alerts sorted by timestamp descending', () => {
    const events: AgentEvent[] = [
      { agentId: 'x', agentName: 'X', kind: 'rate_limit_hit', windowTs: NOW - 2000 },
      { agentId: 'y', agentName: 'Y', kind: 'agent_killed', windowTs: NOW - 500 },
    ];

    const alerts = runAllHeuristics(events, DEFAULT_THRESHOLDS, NOW);
    expect(alerts.length).toBeGreaterThanOrEqual(2);
    // Check descending order
    for (let i = 0; i < alerts.length - 1; i++) {
      expect(alerts[i].timestamp).toBeGreaterThanOrEqual(alerts[i + 1].timestamp);
    }
  });

  it('returns empty array when no events', () => {
    const alerts = runAllHeuristics([], DEFAULT_THRESHOLDS, NOW);
    expect(alerts).toHaveLength(0);
  });
});
