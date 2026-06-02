import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { Sidebar } from './components/dashboard/Sidebar.js';
import { OverviewPage } from './pages/OverviewPage.js';
import { AgentsPage } from './pages/AgentsPage.js';
import { PaymentsPage } from './pages/PaymentsPage.js';
import { JobsPage } from './pages/JobsPage.js';

function PlaceholderPage({ title }: { title: string }) {
  return (
    <div className="flex-1 flex items-center justify-center">
      <div className="text-center">
        <p className="font-display text-xl font-semibold text-sa-text mb-2">{title}</p>
        <p className="text-sa-text-dim text-sm">Coming soon · Contributions welcome!</p>
        <a
          href="https://github.com/yourusername/stellaragent"
          className="text-sa-accent text-sm hover:underline mt-3 inline-block"
        >
          See CONTRIBUTING.md →
        </a>
      </div>
    </div>
  );
}

export function App() {
  return (
    <BrowserRouter>
      <div className="flex min-h-screen bg-sa-bg bg-grid-pattern bg-grid">
        {/* Radial glow overlay */}
        <div className="fixed inset-0 bg-radial-glow pointer-events-none" />

        <Sidebar />

        <main className="flex flex-1 overflow-hidden relative">
          <Routes>
            <Route path="/" element={<OverviewPage />} />
            <Route path="/agents" element={<AgentsPage />} />
            <Route path="/payments" element={<PaymentsPage />} />
            <Route path="/jobs" element={<JobsPage />} />
            <Route path="/limits" element={<PlaceholderPage title="Rate Limits" />} />
            <Route path="/settings" element={<PlaceholderPage title="Settings" />} />
          </Routes>
        </main>
      </div>
    </BrowserRouter>
  );
}
