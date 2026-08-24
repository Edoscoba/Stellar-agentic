/**
 * Alert Store — Issue #257
 * Manages alert state, configurable thresholds (client-side),
 * webhook config, and live event simulation/polling.
 */

import { useState, useEffect, useCallback, useRef } from 'react';
import {
  runAllHeuristics,
  DEFAULT_THRESHOLDS,
  type AgentEvent,
  type FiredAlert,
  type AlertThresholds,
} from './alertHeuristics';
import { fireWebhooksForAlerts, type WebhookConfig } from './webhookService';
import { MOCK_AGENTS, MOCK_PAYMENTS } from './mockData';

// ─── Persist thresholds & webhook config in localStorage ─────────────────────

const THRESHOLDS_KEY = 'sa_alert_thresholds';
const WEBHOOK_KEY = 'sa_webhook_config';

function loadThresholds(): AlertThresholds {
  try {
    const raw = localStorage.getItem(THRESHOLDS_KEY);
    if (raw) return { ...DEFAULT_THRESHOLDS, ...JSON.parse(raw) };
  } catch {
    // Unreadable or malformed storage — fall back to the defaults below.
  }
  return DEFAULT_THRESHOLDS;
}

function saveThresholds(t: AlertThresholds) {
  localStorage.setItem(THRESHOLDS_KEY, JSON.stringify(t));
}

function loadWebhookConfig(): WebhookConfig {
  try {
    const raw = localStorage.getItem(WEBHOOK_KEY);
    if (raw) return JSON.parse(raw);
  } catch {
    // Unreadable or malformed storage — fall back to webhooks disabled.
  }
  return { enabled: false, url: '' };
}

function saveWebhookConfig(c: WebhookConfig) {
  localStorage.setItem(WEBHOOK_KEY, JSON.stringify(c));
}

// ─── Seed mock event history from MOCK_AGENTS & MOCK_PAYMENTS ────────────────

function buildSeedEvents(): AgentEvent[] {
  const now = Date.now();
  const oneHour = 60 * 60 * 1000;
  const events: AgentEvent[] = [];

  // Build historical payment events per agent
  MOCK_AGENTS.forEach((agent) => {
    const hourlyLimit = parseFloat(agent.limitPerHour);
    const histAvg = parseFloat(agent.spentToday) / 24;

    // 8 hours of history
    for (let h = 8; h >= 1; h--) {
      events.push({
        agentId: agent.id,
        agentName: agent.name,
        kind: 'payment',
        amount: histAvg + Math.random() * 0.05,
        windowTs: now - h * oneHour,
      });
    }

    // Current hour spend (from MOCK_PAYMENTS)
    const agentPayments = MOCK_PAYMENTS.filter(
      (p) => p.agentId === agent.id && p.status === 'success',
    );
    if (agentPayments.length > 0) {
      const currentSpend = agentPayments.reduce(
        (sum, p) => sum + parseFloat(p.amount),
        0,
      );
      events.push({
        agentId: agent.id,
        agentName: agent.name,
        kind: 'payment',
        amount: currentSpend,
        windowTs: now - 5 * 60 * 1000,
      });
    }

    // Near-limit data for agent 3 (Summarizer Bot — already in warning state)
    if (agent.id === '3') {
      const txPerHour = Math.round((parseFloat(agent.spentThisHour) / hourlyLimit) * 40);
      for (let h = 4; h >= 0; h--) {
        events.push({
          agentId: agent.id,
          agentName: agent.name,
          kind: 'near_limit',
          hourlyTxCount: txPerHour,
          maxTxsPerHour: 40,
          windowTs: now - h * oneHour,
        });
      }
    }
  });

  return events;
}

// ─── Hook ─────────────────────────────────────────────────────────────────────

export function useAlertStore() {
  const [thresholds, setThresholdsState] = useState<AlertThresholds>(loadThresholds);
  const [webhookConfig, setWebhookConfigState] = useState<WebhookConfig>(loadWebhookConfig);
  const [events, setEvents] = useState<AgentEvent[]>(() => buildSeedEvents());
  const [alerts, setAlerts] = useState<FiredAlert[]>([]);
  const [dismissedIds, setDismissedIds] = useState<Set<string>>(new Set());
  const prevAlertIds = useRef<Set<string>>(new Set());

  // Run heuristics whenever events or thresholds change
  useEffect(() => {
    const all = runAllHeuristics(events, thresholds);
    setAlerts(all);

    // Fire webhooks only for newly-fired alerts
    const newAlerts = all.filter((a) => !prevAlertIds.current.has(a.id));
    if (newAlerts.length > 0) {
      fireWebhooksForAlerts(newAlerts, webhookConfig);
    }
    prevAlertIds.current = new Set(all.map((a) => a.id));
  }, [events, thresholds, webhookConfig]);

  // Live-tail simulation: inject new events every 8s
  useEffect(() => {
    const timer = setInterval(() => {
      const now = Date.now();
      // Randomly pick an agent and inject a payment event
      const agent = MOCK_AGENTS[Math.floor(Math.random() * MOCK_AGENTS.length)];
      const isBurst = Math.random() < 0.15; // 15% chance of burst payment
      setEvents((prev) => [
        ...prev,
        {
          agentId: agent.id,
          agentName: agent.name,
          kind: 'payment',
          amount: isBurst ? parseFloat(agent.limitPerHour) * 0.9 : 0.01 + Math.random() * 0.05,
          windowTs: now,
        },
      ]);
    }, 8000);

    return () => clearInterval(timer);
  }, []);

  const setThresholds = useCallback((t: AlertThresholds) => {
    setThresholdsState(t);
    saveThresholds(t);
  }, []);

  const setWebhookConfig = useCallback((c: WebhookConfig) => {
    setWebhookConfigState(c);
    saveWebhookConfig(c);
  }, []);

  const dismissAlert = useCallback((id: string) => {
    setDismissedIds((prev) => new Set([...prev, id]));
  }, []);

  const injectRateLimitHit = useCallback((agentId: string, agentName: string) => {
    setEvents((prev) => [
      ...prev,
      {
        agentId,
        agentName,
        kind: 'rate_limit_hit',
        windowTs: Date.now(),
      },
    ]);
  }, []);

  const injectAgentKilled = useCallback((agentId: string, agentName: string) => {
    setEvents((prev) => [
      ...prev,
      {
        agentId,
        agentName,
        kind: 'agent_killed',
        windowTs: Date.now(),
      },
    ]);
  }, []);

  const visibleAlerts = alerts.filter((a) => !dismissedIds.has(a.id));

  return {
    alerts: visibleAlerts,
    allAlerts: alerts,
    thresholds,
    setThresholds,
    webhookConfig,
    setWebhookConfig,
    dismissAlert,
    injectRateLimitHit,
    injectAgentKilled,
    eventCount: events.length,
  };
}
