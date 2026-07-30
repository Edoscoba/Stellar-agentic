import { useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import {
  AlertTriangle,
  XCircle,
  Zap,
  Bell,
  BellOff,
  Settings2,
  X,
  ChevronDown,
  ChevronUp,
  Activity,
  Shield,
  TrendingUp,
  Webhook,
  TestTube,
} from 'lucide-react';
import { clsx } from 'clsx';
import { useAlertStore } from '../lib/useAlertStore';
import type { FiredAlert, AlertThresholds } from '../lib/alertHeuristics';
import { MOCK_AGENTS } from '../lib/mockData';

// ─── Severity colours ─────────────────────────────────────────────────────────
const severityStyles = {
  critical: {
    border: 'border-red-500/40',
    bg: 'bg-red-500/10',
    icon: 'text-red-400',
    badge: 'bg-red-500/20 text-red-300 border border-red-500/30',
    dot: 'bg-red-400',
  },
  warning: {
    border: 'border-amber-500/40',
    bg: 'bg-amber-500/10',
    icon: 'text-amber-400',
    badge: 'bg-amber-500/20 text-amber-300 border border-amber-500/30',
    dot: 'bg-amber-400',
  },
};

const kindMeta: Record<string, { label: string; Icon: React.ElementType }> = {
  spend_velocity_anomaly: { label: 'Velocity Anomaly', Icon: TrendingUp },
  near_limit_repeated: { label: 'Near-Limit', Icon: Activity },
  rate_limit_hit: { label: 'Rate Limit Hit', Icon: Shield },
  agent_killed: { label: 'Agent Killed', Icon: XCircle },
};

// ─── Alert Card ───────────────────────────────────────────────────────────────
function AlertCard({
  alert,
  onDismiss,
}: {
  alert: FiredAlert;
  onDismiss: (id: string) => void;
}) {
  const s = severityStyles[alert.severity];
  const meta = kindMeta[alert.kind] ?? { label: alert.kind, Icon: Bell };
  const { Icon } = meta;

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: -8, scale: 0.97 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, x: 40, scale: 0.95 }}
      transition={{ duration: 0.2 }}
      className={clsx(
        'rounded-xl border p-4 flex gap-4 items-start',
        s.border,
        s.bg,
      )}
    >
      <div className="mt-0.5 shrink-0">
        <span className={clsx('relative flex h-8 w-8 items-center justify-center rounded-lg', s.bg, s.border, 'border')}>
          <Icon size={16} className={s.icon} />
          <span className={clsx('absolute -top-1 -right-1 w-2 h-2 rounded-full', s.dot, 'animate-pulse')} />
        </span>
      </div>

      <div className="flex-1 min-w-0">
        <div className="flex items-start gap-2 flex-wrap mb-1">
          <span className={clsx('text-[10px] font-mono font-semibold px-2 py-0.5 rounded', s.badge)}>
            {alert.severity.toUpperCase()} · {meta.label}
          </span>
          <span className="text-[10px] text-sa-text-dim font-mono">
            {new Date(alert.timestamp).toLocaleTimeString()}
          </span>
        </div>
        <p className="text-sm font-semibold text-sa-text mb-1">{alert.message}</p>
        <p className="text-xs text-sa-text-dim leading-relaxed">{alert.detail}</p>
        <p className="text-[10px] font-mono text-sa-muted mt-1">ID: {alert.id}</p>
      </div>

      <button
        onClick={() => onDismiss(alert.id)}
        className="shrink-0 text-sa-muted hover:text-sa-text transition-colors p-1 rounded hover:bg-sa-bg/50"
        aria-label="Dismiss alert"
      >
        <X size={14} />
      </button>
    </motion.div>
  );
}

// ─── Threshold Slider Row ─────────────────────────────────────────────────────
function SliderRow({
  label,
  value,
  min,
  max,
  step,
  onChange,
  display,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  display: string;
}) {
  return (
    <div className="space-y-1">
      <div className="flex justify-between items-center">
        <span className="text-xs text-sa-text-dim">{label}</span>
        <span className="text-xs font-mono text-sa-accent">{display}</span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(parseFloat(e.target.value))}
        className="w-full accent-sa-accent h-1 rounded cursor-pointer"
      />
    </div>
  );
}

// ─── Webhook Config Panel ─────────────────────────────────────────────────────
function WebhookPanel({
  config,
  setConfig,
}: {
  config: { enabled: boolean; url: string; secret?: string };
  setConfig: (c: typeof config) => void;
}) {
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <span className="text-xs text-sa-text-dim">Enable webhook</span>
        <button
          onClick={() => setConfig({ ...config, enabled: !config.enabled })}
          className={clsx(
            'w-9 h-5 rounded-full transition-colors relative',
            config.enabled ? 'bg-sa-accent' : 'bg-sa-border',
          )}
        >
          <span
            className={clsx(
              'absolute top-0.5 w-4 h-4 rounded-full bg-white shadow transition-transform',
              config.enabled ? 'translate-x-4' : 'translate-x-0.5',
            )}
          />
        </button>
      </div>
      <div>
        <label className="text-[10px] text-sa-text-dim block mb-1 uppercase tracking-wider">POST URL</label>
        <input
          type="url"
          placeholder="https://hooks.slack.com/services/..."
          value={config.url}
          onChange={(e) => setConfig({ ...config, url: e.target.value })}
          className="w-full bg-sa-bg border border-sa-border rounded px-3 py-2 text-xs text-sa-text font-mono placeholder-sa-muted focus:outline-none focus:border-sa-accent transition-colors"
        />
      </div>
      <div>
        <label className="text-[10px] text-sa-text-dim block mb-1 uppercase tracking-wider">Secret (X-Webhook-Secret)</label>
        <input
          type="password"
          placeholder="optional"
          value={config.secret ?? ''}
          onChange={(e) => setConfig({ ...config, secret: e.target.value })}
          className="w-full bg-sa-bg border border-sa-border rounded px-3 py-2 text-xs text-sa-text font-mono placeholder-sa-muted focus:outline-none focus:border-sa-accent transition-colors"
        />
      </div>
      <div className="rounded-lg bg-sa-bg border border-sa-border p-3">
        <p className="text-[10px] text-sa-text-dim font-semibold uppercase tracking-wider mb-2">Example Payload</p>
        <pre className="text-[9px] text-sa-accent font-mono leading-relaxed overflow-x-auto whitespace-pre-wrap">{JSON.stringify({
          event: 'alert_fired',
          timestamp: new Date().toISOString(),
          alert: {
            id: 'velocity-agent-1-1720000000000',
            kind: 'spend_velocity_anomaly',
            severity: 'warning',
            agentId: '1',
            agentName: 'Inference Agent',
            message: 'Spend velocity anomaly on Inference Agent',
            detail: 'Current hour spend ($1.45) is 3.2× trailing avg ($0.45/hr).',
          },
          source: 'stellaragent-dashboard',
        }, null, 2)}</pre>
      </div>
    </div>
  );
}

// ─── Main Page ────────────────────────────────────────────────────────────────
export function AlertsPage() {
  const {
    alerts,
    thresholds,
    setThresholds,
    webhookConfig,
    setWebhookConfig,
    dismissAlert,
    injectRateLimitHit,
    injectAgentKilled,
    eventCount,
  } = useAlertStore();

  const [showSettings, setShowSettings] = useState(false);
  const [showWebhook, setShowWebhook] = useState(false);
  const [filterSeverity, setFilterSeverity] = useState<'all' | 'critical' | 'warning'>('all');

  const criticalCount = alerts.filter((a) => a.severity === 'critical').length;
  const warningCount = alerts.filter((a) => a.severity === 'warning').length;

  const filteredAlerts =
    filterSeverity === 'all'
      ? alerts
      : alerts.filter((a) => a.severity === filterSeverity);

  return (
    <div className="flex-1 overflow-y-auto p-6 space-y-6">
      {/* Header */}
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <h1 className="font-display text-2xl font-bold text-sa-text flex items-center gap-2">
            <Bell size={22} className="text-sa-accent" />
            Anomaly Alerts
          </h1>
          <p className="text-sa-text-dim text-sm mt-1">
            Live monitoring · {eventCount} events indexed · {alerts.length} active alert{alerts.length !== 1 ? 's' : ''}
          </p>
        </div>
        <div className="flex gap-2 flex-wrap">
          <button
            onClick={() => setShowWebhook((v) => !v)}
            className={clsx(
              'flex items-center gap-2 px-3 py-2 rounded-lg text-sm transition-all border',
              showWebhook
                ? 'bg-sa-accent/10 text-sa-accent border-sa-accent/30'
                : 'bg-sa-surface border-sa-border text-sa-text-dim hover:text-sa-text',
            )}
          >
            <Webhook size={14} />
            Webhook
            {webhookConfig.enabled && (
              <span className="w-1.5 h-1.5 rounded-full bg-green-400" />
            )}
          </button>
          <button
            onClick={() => setShowSettings((v) => !v)}
            className={clsx(
              'flex items-center gap-2 px-3 py-2 rounded-lg text-sm transition-all border',
              showSettings
                ? 'bg-sa-accent/10 text-sa-accent border-sa-accent/30'
                : 'bg-sa-surface border-sa-border text-sa-text-dim hover:text-sa-text',
            )}
          >
            <Settings2 size={14} />
            Thresholds
            {showSettings ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
          </button>
        </div>
      </div>

      {/* Stats Bar */}
      <div className="grid grid-cols-3 gap-4">
        {[
          { label: 'Critical', count: criticalCount, color: 'text-red-400', bg: 'bg-red-500/10 border-red-500/30' },
          { label: 'Warning', count: warningCount, color: 'text-amber-400', bg: 'bg-amber-500/10 border-amber-500/30' },
          { label: 'Total', count: alerts.length, color: 'text-sa-accent', bg: 'bg-sa-accent/10 border-sa-accent/30' },
        ].map(({ label, count, color, bg }) => (
          <div key={label} className={clsx('rounded-xl border p-4 flex flex-col gap-1', bg)}>
            <span className="text-xs text-sa-text-dim">{label}</span>
            <span className={clsx('text-2xl font-bold font-display', color)}>{count}</span>
          </div>
        ))}
      </div>

      {/* Threshold Settings Panel */}
      <AnimatePresence>
        {showSettings && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: 'auto' }}
            exit={{ opacity: 0, height: 0 }}
            className="overflow-hidden"
          >
            <div className="rounded-xl border border-sa-border bg-sa-surface p-5 space-y-4">
              <div className="flex items-center gap-2 mb-3">
                <Settings2 size={14} className="text-sa-accent" />
                <p className="text-sm font-semibold text-sa-text">Alert Thresholds</p>
                <span className="text-[10px] text-sa-muted">(saved client-side)</span>
              </div>
              <SliderRow
                label="Velocity anomaly multiplier"
                value={thresholds.velocityMultiplier}
                min={1.2}
                max={5}
                step={0.1}
                onChange={(v) => setThresholds({ ...thresholds, velocityMultiplier: v })}
                display={`${thresholds.velocityMultiplier.toFixed(1)}×`}
              />
              <SliderRow
                label="Near-limit fraction"
                value={thresholds.nearLimitFraction}
                min={0.5}
                max={0.99}
                step={0.01}
                onChange={(v) => setThresholds({ ...thresholds, nearLimitFraction: v })}
                display={`${(thresholds.nearLimitFraction * 100).toFixed(0)}%`}
              />
              <SliderRow
                label="Near-limit consecutive windows"
                value={thresholds.nearLimitWindowCount}
                min={2}
                max={10}
                step={1}
                onChange={(v) => setThresholds({ ...thresholds, nearLimitWindowCount: v })}
                display={`${thresholds.nearLimitWindowCount} windows`}
              />
            </div>
          </motion.div>
        )}
      </AnimatePresence>

      {/* Webhook Panel */}
      <AnimatePresence>
        {showWebhook && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: 'auto' }}
            exit={{ opacity: 0, height: 0 }}
            className="overflow-hidden"
          >
            <div className="rounded-xl border border-sa-border bg-sa-surface p-5">
              <div className="flex items-center gap-2 mb-4">
                <Webhook size={14} className="text-sa-accent" />
                <p className="text-sm font-semibold text-sa-text">Webhook Integration</p>
              </div>
              <WebhookPanel config={webhookConfig} setConfig={setWebhookConfig} />
            </div>
          </motion.div>
        )}
      </AnimatePresence>

      {/* Simulation Controls */}
      <div className="rounded-xl border border-sa-border bg-sa-surface p-4">
        <div className="flex items-center gap-2 mb-3">
          <TestTube size={14} className="text-sa-accent" />
          <p className="text-sm font-semibold text-sa-text">Simulate Events</p>
          <span className="text-[10px] text-sa-muted">(trigger alerts for testing)</span>
        </div>
        <div className="flex flex-wrap gap-2">
          {MOCK_AGENTS.slice(0, 3).map((agent) => (
            <div key={agent.id} className="flex gap-1">
              <button
                onClick={() => injectRateLimitHit(agent.id, agent.name)}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-amber-500/10 border border-amber-500/30 text-amber-300 hover:bg-amber-500/20 transition-colors"
              >
                <Shield size={11} />
                RL Hit · {agent.name}
              </button>
              <button
                onClick={() => injectAgentKilled(agent.id, agent.name)}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-red-500/10 border border-red-500/30 text-red-300 hover:bg-red-500/20 transition-colors"
              >
                <XCircle size={11} />
                Kill
              </button>
            </div>
          ))}
        </div>
      </div>

      {/* Filter tabs */}
      <div className="flex gap-2">
        {(['all', 'critical', 'warning'] as const).map((f) => (
          <button
            key={f}
            onClick={() => setFilterSeverity(f)}
            className={clsx(
              'px-4 py-1.5 rounded-full text-xs font-medium transition-all border capitalize',
              filterSeverity === f
                ? 'bg-sa-accent/10 text-sa-accent border-sa-accent/30'
                : 'bg-sa-surface border-sa-border text-sa-text-dim hover:text-sa-text',
            )}
          >
            {f === 'all' ? `All (${alerts.length})` : f === 'critical' ? `Critical (${criticalCount})` : `Warning (${warningCount})`}
          </button>
        ))}
      </div>

      {/* Alert Feed */}
      <div className="space-y-3">
        <AnimatePresence mode="popLayout">
          {filteredAlerts.length === 0 ? (
            <motion.div
              key="empty"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              className="rounded-xl border border-sa-border bg-sa-surface/50 p-12 text-center"
            >
              <BellOff size={32} className="text-sa-muted mx-auto mb-3 opacity-50" />
              <p className="text-sa-text-dim text-sm">No active alerts</p>
              <p className="text-sa-muted text-xs mt-1">
                All agents are operating within normal parameters.
              </p>
            </motion.div>
          ) : (
            filteredAlerts.map((alert) => (
              <AlertCard key={alert.id} alert={alert} onDismiss={dismissAlert} />
            ))
          )}
        </AnimatePresence>
      </div>
    </div>
  );
}
