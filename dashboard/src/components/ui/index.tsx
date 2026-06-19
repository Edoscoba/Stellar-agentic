import { motion } from 'framer-motion';
import { clsx } from 'clsx';
import type { ReactNode } from 'react';
import { pctNumber, clamp100 } from '../../lib/deterministic-math.js';

// ─── Badge ────────────────────────────────────────────────────────────────────

interface BadgeProps {
  children: ReactNode;
  variant?: 'success' | 'warning' | 'danger' | 'neutral' | 'info';
  size?: 'sm' | 'md';
}

export function Badge({ children, variant = 'neutral', size = 'sm' }: BadgeProps) {
  return (
    <span
      className={clsx(
        'inline-flex items-center gap-1 rounded-full font-medium',
        size === 'sm' ? 'px-2 py-0.5 text-xs' : 'px-3 py-1 text-sm',
        {
          'bg-sa-green/10 text-sa-green': variant === 'success',
          'bg-sa-yellow/10 text-sa-yellow': variant === 'warning',
          'bg-sa-red/10 text-sa-red': variant === 'danger',
          'bg-sa-muted/20 text-sa-text-dim': variant === 'neutral',
          'bg-sa-accent/10 text-sa-accent': variant === 'info',
        },
      )}
    >
      {children}
    </span>
  );
}

// ─── StatusDot ────────────────────────────────────────────────────────────────

interface StatusDotProps {
  status: 'active' | 'inactive' | 'warning';
  pulse?: boolean;
}

export function StatusDot({ status, pulse = false }: StatusDotProps) {
  const colors = {
    active: 'bg-sa-green shadow-[0_0_6px_rgba(0,255,178,0.7)]',
    inactive: 'bg-sa-muted',
    warning: 'bg-sa-yellow shadow-[0_0_6px_rgba(255,184,0,0.7)]',
  };

  return (
    <span className="relative inline-flex">
      {pulse && status === 'active' && (
        <span className="absolute inset-0 rounded-full bg-sa-green animate-ping opacity-40" />
      )}
      <span className={clsx('w-2 h-2 rounded-full', colors[status])} />
    </span>
  );
}

// ─── Card ─────────────────────────────────────────────────────────────────────

interface CardProps {
  children: ReactNode;
  className?: string;
  hover?: boolean;
  animate?: boolean;
}

export function Card({ children, className, hover = false, animate = true }: CardProps) {
  const Comp = animate ? motion.div : 'div';
  const animProps = animate
    ? { initial: { opacity: 0, y: 8 }, animate: { opacity: 1, y: 0 }, transition: { duration: 0.3 } }
    : {};

  return (
    <Comp
      {...animProps}
      className={clsx(
        'card p-5',
        hover && 'hover:border-sa-accent/40 hover:bg-sa-surface/80 transition-all duration-200 cursor-pointer',
        className,
      )}
    >
      {children}
    </Comp>
  );
}

// ─── StatCard ─────────────────────────────────────────────────────────────────

interface StatCardProps {
  label: string;
  value: string;
  sub?: string;
  trend?: 'up' | 'down' | 'neutral';
  trendValue?: string;
  icon?: ReactNode;
  accent?: boolean;
}

export function StatCard({ label, value, sub, trend, trendValue, icon, accent }: StatCardProps) {
  return (
    <Card>
      <div className="flex items-start justify-between">
        <div className="flex-1">
          <p className="label mb-2">{label}</p>
          <p
            className={clsx(
              'font-display text-2xl font-semibold',
              accent ? 'text-sa-accent' : 'text-sa-text',
            )}
          >
            {value}
          </p>
          {sub && <p className="text-xs text-sa-text-dim mt-1">{sub}</p>}
          {trendValue && (
            <p
              className={clsx(
                'text-xs mt-2 font-medium',
                trend === 'up' && 'text-sa-green',
                trend === 'down' && 'text-sa-red',
                trend === 'neutral' && 'text-sa-text-dim',
              )}
            >
              {trendValue}
            </p>
          )}
        </div>
        {icon && (
          <div className="text-sa-accent opacity-60 ml-3">{icon}</div>
        )}
      </div>
    </Card>
  );
}

// ─── AddressChip ──────────────────────────────────────────────────────────────

export function AddressChip({ address }: { address: string }) {
  const short = `${address.slice(0, 4)}…${address.slice(-4)}`;
  return (
    <span
      className="address bg-sa-bg px-2 py-0.5 rounded border border-sa-border cursor-pointer hover:border-sa-accent/40 transition-colors"
      title={address}
      onClick={() => navigator.clipboard?.writeText(address)}
    >
      {short}
    </span>
  );
}

// ─── SectionHeader ────────────────────────────────────────────────────────────

interface SectionHeaderProps {
  title: string;
  subtitle?: string;
  action?: ReactNode;
}

export function SectionHeader({ title, subtitle, action }: SectionHeaderProps) {
  return (
    <div className="flex items-center justify-between mb-5">
      <div>
        <h2 className="section-title">{title}</h2>
        {subtitle && <p className="text-xs text-sa-text-dim mt-0.5">{subtitle}</p>}
      </div>
      {action && <div>{action}</div>}
    </div>
  );
}

// ─── EmptyState ──────────────────────────────────────────────────────────────

export function EmptyState({ message }: { message: string }) {
  return (
    <div className="flex flex-col items-center justify-center py-12 text-sa-text-dim">
      <div className="w-12 h-12 rounded-full border border-sa-border flex items-center justify-center mb-3">
        <span className="text-xl">∅</span>
      </div>
      <p className="text-sm">{message}</p>
    </div>
  );
}

// ─── ProgressBar ──────────────────────────────────────────────────────────────

interface ProgressBarProps {
  value: number; // 0–100
  max?: number;
  label?: string;
  showPercent?: boolean;
  danger?: boolean;
}

export function ProgressBar({ value, max = 100, label, showPercent, danger }: ProgressBarProps) {
  // Compute percentage deterministically.
  // When callers already pre-compute a 0–100 value via pctNumber() and pass max=100,
  // this is a simple clamp. When max != 100 (legacy or chart use), we do the
  // deterministic division via pctNumber on string representations.
  const pct = max === 100
    ? clamp100(value)
    : clamp100(pctNumber(String(value), String(max)));
  const isHigh = pct > 80;

  return (
    <div className="w-full">
      {(label || showPercent) && (
        <div className="flex justify-between mb-1">
          {label && <span className="text-xs text-sa-text-dim">{label}</span>}
          {showPercent && (
            <span className={clsx('text-xs font-mono', isHigh || danger ? 'text-sa-yellow' : 'text-sa-text-dim')}>
              {pct.toFixed(0)}%
            </span>
          )}
        </div>
      )}
      <div className="h-1.5 bg-sa-bg rounded-full overflow-hidden">
        <motion.div
          className={clsx(
            'h-full rounded-full',
            danger || isHigh ? 'bg-sa-yellow' : 'bg-sa-accent',
          )}
          initial={{ width: 0 }}
          animate={{ width: `${pct}%` }}
          transition={{ duration: 0.6, ease: 'easeOut' }}
        />
      </div>
    </div>
  );
}
