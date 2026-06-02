import { NavLink } from 'react-router-dom';
import { motion } from 'framer-motion';
import {
  LayoutDashboard,
  Bot,
  ArrowLeftRight,
  Briefcase,
  ShieldCheck,
  Settings,
  ExternalLink,
  Zap,
} from 'lucide-react';
import { clsx } from 'clsx';

const NAV_ITEMS = [
  { to: '/', icon: LayoutDashboard, label: 'Overview' },
  { to: '/agents', icon: Bot, label: 'Agents' },
  { to: '/payments', icon: ArrowLeftRight, label: 'Payments' },
  { to: '/jobs', icon: Briefcase, label: 'Escrow Jobs' },
  { to: '/limits', icon: ShieldCheck, label: 'Rate Limits' },
  { to: '/settings', icon: Settings, label: 'Settings' },
];

export function Sidebar() {
  return (
    <aside className="w-60 shrink-0 flex flex-col bg-sa-surface border-r border-sa-border h-screen sticky top-0">
      {/* Logo */}
      <div className="flex items-center gap-2.5 px-5 py-5 border-b border-sa-border">
        <div className="w-8 h-8 rounded-lg bg-sa-accent/20 border border-sa-accent/30 flex items-center justify-center">
          <Zap size={16} className="text-sa-accent" />
        </div>
        <div>
          <p className="font-display font-semibold text-sm text-sa-text">StellarAgent</p>
          <p className="text-[10px] text-sa-text-dim font-mono">v0.1.0 · testnet</p>
        </div>
      </div>

      {/* Nav */}
      <nav className="flex-1 py-4 px-3">
        <p className="label px-2 mb-2">Navigation</p>
        <ul className="space-y-0.5">
          {NAV_ITEMS.map(({ to, icon: Icon, label }) => (
            <li key={to}>
              <NavLink
                to={to}
                end={to === '/'}
                className={({ isActive }) =>
                  clsx(
                    'flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm font-medium transition-all duration-150',
                    isActive
                      ? 'bg-sa-accent/10 text-sa-accent border border-sa-accent/20'
                      : 'text-sa-text-dim hover:text-sa-text hover:bg-sa-bg/60',
                  )
                }
              >
                {({ isActive }) => (
                  <>
                    <Icon
                      size={16}
                      className={isActive ? 'text-sa-accent' : 'text-sa-muted'}
                    />
                    {label}
                    {isActive && (
                      <motion.span
                        layoutId="nav-active"
                        className="ml-auto w-1.5 h-1.5 rounded-full bg-sa-accent"
                      />
                    )}
                  </>
                )}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>

      {/* Footer */}
      <div className="p-4 border-t border-sa-border space-y-2">
        <a
          href="https://github.com/yourusername/stellaragent"
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-2 text-xs text-sa-text-dim hover:text-sa-text transition-colors px-2 py-1.5 rounded"
        >
          <ExternalLink size={12} />
          GitHub · Contribute
        </a>
        <a
          href="https://developers.stellar.org"
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-2 text-xs text-sa-text-dim hover:text-sa-text transition-colors px-2 py-1.5 rounded"
        >
          <ExternalLink size={12} />
          Stellar Docs
        </a>
      </div>
    </aside>
  );
}
