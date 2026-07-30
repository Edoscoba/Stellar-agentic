/**
 * Alert Heuristics — Issue #257
 * Pure functions: take event history + config, return fired alerts.
 * No UI or side-effect dependencies.
 */

// ─── Types ────────────────────────────────────────────────────────────────────

export type AlertSeverity = 'warning' | 'critical';

export type AlertKind =
  | 'spend_velocity_anomaly'
  | 'near_limit_repeated'
  | 'rate_limit_hit'
  | 'agent_killed';

export interface AgentEvent {
  agentId: string;
  agentName: string;
  kind: 'payment' | 'rate_limit_hit' | 'agent_killed' | 'near_limit';
  amount?: number;           // spend in USDC
  hourlyTxCount?: number;    // tx count this hour
  maxTxsPerHour?: number;    // the agent's configured cap
  windowTs: number;          // unix ms timestamp of this event window
}

export interface AlertThresholds {
  /** Fraction above trailing average that triggers velocity anomaly (default 2.0 = 2x avg) */
  velocityMultiplier: number;
  /** Fraction of max_txs_per_hour that counts as "near limit" (default 0.85 = 85%) */
  nearLimitFraction: number;
  /** How many consecutive near-limit windows trigger the alert (default 3) */
  nearLimitWindowCount: number;
  /** Lookback window in ms for trailing spend average (default 6 hours) */
  velocityLookbackMs: number;
}

export interface FiredAlert {
  id: string;
  kind: AlertKind;
  severity: AlertSeverity;
  agentId: string;
  agentName: string;
  message: string;
  detail: string;
  timestamp: number; // unix ms
}

export const DEFAULT_THRESHOLDS: AlertThresholds = {
  velocityMultiplier: 2.0,
  nearLimitFraction: 0.85,
  nearLimitWindowCount: 3,
  velocityLookbackMs: 6 * 60 * 60 * 1000, // 6 hours
};

// ─── Heuristic 1: Spend Velocity Anomaly ─────────────────────────────────────

/**
 * Detects when the most recent hourly spend rate significantly exceeds
 * the agent's trailing historical average spend rate.
 *
 * Compares events in the last hour vs the lookback window.
 */
export function detectSpendVelocityAnomaly(
  events: AgentEvent[],
  thresholds: AlertThresholds,
  nowMs: number = Date.now(),
): FiredAlert[] {
  const alerts: FiredAlert[] = [];
  const agentIds = [...new Set(events.map((e) => e.agentId))];

  for (const agentId of agentIds) {
    const agentEvents = events.filter(
      (e) => e.agentId === agentId && e.kind === 'payment' && e.amount != null,
    );

    if (agentEvents.length < 2) continue;

    const oneHourAgo = nowMs - 60 * 60 * 1000;
    const lookbackStart = nowMs - thresholds.velocityLookbackMs;

    const recentSpend = agentEvents
      .filter((e) => e.windowTs >= oneHourAgo)
      .reduce((sum, e) => sum + (e.amount ?? 0), 0);

    const historicalEvents = agentEvents.filter(
      (e) => e.windowTs >= lookbackStart && e.windowTs < oneHourAgo,
    );

    if (historicalEvents.length === 0) continue;

    // Compute average hourly spend from historical windows
    const totalHistoricalSpend = historicalEvents.reduce(
      (sum, e) => sum + (e.amount ?? 0),
      0,
    );
    const historicalHours = thresholds.velocityLookbackMs / (60 * 60 * 1000) - 1;
    const avgHourlySpend = totalHistoricalSpend / Math.max(historicalHours, 1);

    if (avgHourlySpend === 0) continue;

    const ratio = recentSpend / avgHourlySpend;

    if (ratio >= thresholds.velocityMultiplier) {
      const agentName = agentEvents[0].agentName;
      alerts.push({
        id: `velocity-${agentId}-${nowMs}`,
        kind: 'spend_velocity_anomaly',
        severity: ratio >= thresholds.velocityMultiplier * 1.5 ? 'critical' : 'warning',
        agentId,
        agentName,
        message: `Spend velocity anomaly on ${agentName}`,
        detail: `Current hour spend ($${recentSpend.toFixed(4)}) is ${ratio.toFixed(1)}× the trailing average ($${avgHourlySpend.toFixed(4)}/hr). Threshold: ${thresholds.velocityMultiplier}×.`,
        timestamp: nowMs,
      });
    }
  }

  return alerts;
}

// ─── Heuristic 2: Repeated Near-Limit Activity ────────────────────────────────

/**
 * Detects agents whose hourly_tx_count has been consistently close to
 * max_txs_per_hour across multiple consecutive windows.
 */
export function detectRepeatedNearLimitActivity(
  events: AgentEvent[],
  thresholds: AlertThresholds,
  nowMs: number = Date.now(),
): FiredAlert[] {
  const alerts: FiredAlert[] = [];
  const agentIds = [...new Set(events.map((e) => e.agentId))];

  for (const agentId of agentIds) {
    const agentEvents = events
      .filter(
        (e) =>
          e.agentId === agentId &&
          (e.kind === 'near_limit' || e.kind === 'payment') &&
          e.hourlyTxCount != null &&
          e.maxTxsPerHour != null,
      )
      .sort((a, b) => a.windowTs - b.windowTs);

    if (agentEvents.length < thresholds.nearLimitWindowCount) continue;

    // Check last N windows
    const recent = agentEvents.slice(-thresholds.nearLimitWindowCount);
    const allNearLimit = recent.every(
      (e) =>
        e.hourlyTxCount! / e.maxTxsPerHour! >= thresholds.nearLimitFraction,
    );

    if (allNearLimit) {
      const agentName = agentEvents[0].agentName;
      const lastEvent = recent[recent.length - 1];
      const utilPct = (
        (lastEvent.hourlyTxCount! / lastEvent.maxTxsPerHour!) *
        100
      ).toFixed(0);

      alerts.push({
        id: `near-limit-${agentId}-${nowMs}`,
        kind: 'near_limit_repeated',
        severity: 'warning',
        agentId,
        agentName,
        message: `Repeated near-limit activity on ${agentName}`,
        detail: `Agent has been at ${utilPct}%+ of tx rate limit for ${thresholds.nearLimitWindowCount} consecutive windows (threshold: ${(thresholds.nearLimitFraction * 100).toFixed(0)}%).`,
        timestamp: nowMs,
      });
    }
  }

  return alerts;
}

// ─── Heuristic 3: Rate Limit Hit events ──────────────────────────────────────

export function detectRateLimitHits(
  events: AgentEvent[],
  nowMs: number = Date.now(),
): FiredAlert[] {
  return events
    .filter((e) => e.kind === 'rate_limit_hit')
    .map((e) => ({
      id: `rl-${e.agentId}-${e.windowTs}`,
      kind: 'rate_limit_hit' as AlertKind,
      severity: 'warning' as AlertSeverity,
      agentId: e.agentId,
      agentName: e.agentName,
      message: `Rate limit hit on ${e.agentName}`,
      detail: `Agent attempted a payment that was blocked by the on-chain rate limit. Contract emitted a rejected event at ${new Date(e.windowTs).toLocaleTimeString()}.`,
      timestamp: e.windowTs,
    }));
}

// ─── Heuristic 4: Agent Killed events ────────────────────────────────────────

export function detectAgentKilled(events: AgentEvent[]): FiredAlert[] {
  return events
    .filter((e) => e.kind === 'agent_killed')
    .map((e) => ({
      id: `killed-${e.agentId}-${e.windowTs}`,
      kind: 'agent_killed' as AlertKind,
      severity: 'critical' as AlertSeverity,
      agentId: e.agentId,
      agentName: e.agentName,
      message: `Agent killed: ${e.agentName}`,
      detail: `The on-chain agent_killed event was emitted at ${new Date(e.windowTs).toLocaleTimeString()}. Investigate spend pattern immediately.`,
      timestamp: e.windowTs,
    }));
}

// ─── Composite runner ─────────────────────────────────────────────────────────

export function runAllHeuristics(
  events: AgentEvent[],
  thresholds: AlertThresholds,
  nowMs: number = Date.now(),
): FiredAlert[] {
  return [
    ...detectSpendVelocityAnomaly(events, thresholds, nowMs),
    ...detectRepeatedNearLimitActivity(events, thresholds, nowMs),
    ...detectRateLimitHits(events, nowMs),
    ...detectAgentKilled(events),
  ].sort((a, b) => b.timestamp - a.timestamp);
}
