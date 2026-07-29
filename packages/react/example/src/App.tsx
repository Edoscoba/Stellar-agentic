import { useMemo, useState } from 'react';
import {
  StellarAgentProvider,
  useChannel,
  usePayForAPI,
  useRateLimitStatus,
  useSpendReport,
  useStellarAgent,
} from '@stellaragent/react';
import { createDemoAgent, DEMO_CHANNEL_ID } from './demoAgent.js';

type Mode = 'mock' | 'local';

export function App() {
  const [mode, setMode] = useState<Mode>('mock');
  // Recreated (fresh demo channel) each time the mode is toggled, rather
  // than on every render.
  const demoAgent = useMemo(() => createDemoAgent(), [mode]);

  return (
    <div className="page">
      <header>
        <h1>@stellaragent/react example</h1>
        <p className="subtitle">
          Live channel, spend, and rate-limit data driven entirely through this package's hooks.
        </p>
        <label className="mode-toggle">
          <input
            type="checkbox"
            checked={mode === 'local'}
            onChange={(e) => setMode(e.target.checked ? 'local' : 'mock')}
          />
          Connect to a real local Soroban network instead of the built-in mock agent
        </label>
        {mode === 'local' && (
          <p className="warning">
            <code>StellarAgent</code>&apos;s query/mutation methods are still stubs (see the
            companion SDK issue) — this will show the provider&apos;s real <code>loading</code>/
            <code>error</code> states against <code>network: &quot;local&quot;</code>, but not
            populated data, until that lands. See this package&apos;s README.
          </p>
        )}
      </header>

      {mode === 'mock' ? (
        <StellarAgentProvider key="mock" config={{ network: 'local' }} agent={demoAgent}>
          <Dashboard />
        </StellarAgentProvider>
      ) : (
        <StellarAgentProvider key="local" config={{ network: 'local' }}>
          <Dashboard />
        </StellarAgentProvider>
      )}
    </div>
  );
}

function Dashboard() {
  const { agent } = useStellarAgent();
  return (
    <div className="dashboard">
      <ChannelCard />
      <SpendReportCard />
      {agent && <RateLimitCard agentAddress={agent.address} />}
      <PayForAPICard />
    </div>
  );
}

function ChannelCard() {
  const { data, status, error } = useChannel(DEMO_CHANNEL_ID, { intervalMs: 2000 });

  return (
    <section className="card">
      <h2>Channel</h2>
      <StatusLine status={status} error={error} />
      {data && (
        <dl>
          <dt>Token</dt>
          <dd>{data.token}</dd>
          <dt>Limit / period</dt>
          <dd>{(Number(data.limitPerPeriod) / 1e7).toFixed(2)}</dd>
          <dt>Spent this period</dt>
          <dd>{(Number(data.spentThisPeriod) / 1e7).toFixed(2)}</dd>
          <dt>Active</dt>
          <dd>{data.active ? 'yes' : 'no'}</dd>
        </dl>
      )}
    </section>
  );
}

function SpendReportCard() {
  const { data, status, error, hasPendingPayments } = useSpendReport({ intervalMs: 2000 });

  return (
    <section className="card">
      <h2>Spend report {hasPendingPayments && <span className="badge">pending</span>}</h2>
      <StatusLine status={status} error={error} />
      {data && (
        <dl>
          <dt>Spent this period</dt>
          <dd>{data.spentThisPeriod}</dd>
          <dt>Remaining this period</dt>
          <dd>{data.remainingThisPeriod}</dd>
          <dt>Total lifetime</dt>
          <dd>{data.totalLifetime}</dd>
        </dl>
      )}
    </section>
  );
}

function RateLimitCard({ agentAddress }: { agentAddress: string }) {
  const { data, status, error } = useRateLimitStatus(agentAddress, { intervalMs: 4000 });

  return (
    <section className="card">
      <h2>Rate limit</h2>
      <StatusLine status={status} error={error} />
      {data && (
        <dl>
          <dt>Spent this hour</dt>
          <dd>
            {data.rateLimit.spentThisHour} / {data.rateLimit.maxPerHour}
          </dd>
          <dt>Txs this hour</dt>
          <dd>
            {data.rateLimit.txsThisHour} / {data.rateLimit.maxTxsPerHour}
          </dd>
          <dt>Hourly window resets in</dt>
          <dd>~{Math.round(data.hourWindow.estimatedSecondsRemaining)}s (estimate)</dd>
        </dl>
      )}
    </section>
  );
}

function PayForAPICard() {
  const { payForAPI, status, error, reset } = usePayForAPI();
  const [amount, setAmount] = useState('0.5');

  return (
    <section className="card">
      <h2>Pay for API call</h2>
      <p className="hint">
        Watch the spend report update immediately (optimistically), then reconcile ~900ms later
        once the simulated payment confirms.
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          reset();
          payForAPI({ endpoint: 'https://api.example.com/inference', amount }).catch(() => {
            // Surfaced via `error` below.
          });
        }}
      >
        <input
          type="number"
          step="0.1"
          min="0.1"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
        />
        <button type="submit" disabled={status === 'pending'}>
          {status === 'pending' ? 'Paying…' : 'Pay'}
        </button>
      </form>
      {status === 'success' && <p className="success">Payment confirmed.</p>}
      {status === 'error' && <p className="error">{error?.message}</p>}
    </section>
  );
}

function StatusLine({ status, error }: { status: string; error: Error | null }) {
  if (status === 'error') {
    return <p className="error">{error?.message}</p>;
  }
  if (status === 'idle' || status === 'loading') {
    return <p className="hint">{status}…</p>;
  }
  return null;
}
