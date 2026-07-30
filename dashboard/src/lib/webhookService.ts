/**
 * Webhook Service — Issue #257
 * Fires an HTTP POST to a configurable URL when an alert is triggered.
 * Compatible with Slack Incoming Webhooks, PagerDuty Events API, and generic HTTP.
 */

import type { FiredAlert } from './alertHeuristics';

export interface WebhookConfig {
  enabled: boolean;
  url: string;
  /** Optional secret added as X-Webhook-Secret header */
  secret?: string;
  /** Custom headers to add to every request */
  customHeaders?: Record<string, string>;
}

export interface WebhookPayload {
  event: 'alert_fired';
  timestamp: string; // ISO 8601
  alert: {
    id: string;
    kind: string;
    severity: string;
    agentId: string;
    agentName: string;
    message: string;
    detail: string;
  };
  source: 'stellaragent-dashboard';
}

/**
 * Builds the documented webhook payload from a FiredAlert.
 */
export function buildWebhookPayload(alert: FiredAlert): WebhookPayload {
  return {
    event: 'alert_fired',
    timestamp: new Date(alert.timestamp).toISOString(),
    alert: {
      id: alert.id,
      kind: alert.kind,
      severity: alert.severity,
      agentId: alert.agentId,
      agentName: alert.agentName,
      message: alert.message,
      detail: alert.detail,
    },
    source: 'stellaragent-dashboard',
  };
}

/**
 * Fires a webhook POST for the given alert.
 * Silently swallows errors — monitoring should not crash the dashboard.
 */
export async function fireWebhook(
  alert: FiredAlert,
  config: WebhookConfig,
): Promise<{ success: boolean; status?: number; error?: string }> {
  if (!config.enabled || !config.url) {
    return { success: false, error: 'Webhook not configured or disabled' };
  }

  const payload = buildWebhookPayload(alert);
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    'User-Agent': 'StellarAgent-Dashboard/0.1.0',
    ...config.customHeaders,
  };

  if (config.secret) {
    headers['X-Webhook-Secret'] = config.secret;
  }

  try {
    const response = await fetch(config.url, {
      method: 'POST',
      headers,
      body: JSON.stringify(payload),
    });

    return { success: response.ok, status: response.status };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.error('[StellarAgent] Webhook delivery failed:', message);
    return { success: false, error: message };
  }
}

/**
 * Fires webhooks for multiple alerts. Runs in parallel.
 */
export async function fireWebhooksForAlerts(
  alerts: FiredAlert[],
  config: WebhookConfig,
): Promise<void> {
  if (!config.enabled || !config.url || alerts.length === 0) return;
  await Promise.allSettled(alerts.map((alert) => fireWebhook(alert, config)));
}
