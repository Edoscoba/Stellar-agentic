import { useState } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { Bot, Plus, Power, Settings2, TrendingUp, Zap } from 'lucide-react';

import {
  Card,
  Badge,
  StatusDot,
  AddressChip,
  SectionHeader,
  ProgressBar,
} from '../components/ui/index.js';
import { MOCK_AGENTS, type Agent } from '../lib/mockData.js';

function AgentCard({ agent, onSelect }: { agent: Agent; onSelect: (a: Agent) => void }) {
  const hourPct = (parseFloat(agent.spentThisHour) / parseFloat(agent.limitPerHour)) * 100;
  const dayPct = (parseFloat(agent.spentToday) / parseFloat(agent.limitPerDay)) * 100;

  return (
    <motion.div
      layout
      initial={{ opacity: 0, scale: 0.97 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.95 }}
      className="card p-5 hover:border-sa-accent/30 transition-all duration-200 cursor-pointer group"
      onClick={() => onSelect(agent)}
    >
      {/* Header */}
      <div className="flex items-start justify-between mb-4">
        <div className="flex items-center gap-3">
          <div className="relative w-10 h-10 rounded-xl bg-sa-bg border border-sa-border flex items-center justify-center group-hover:border-sa-accent/30 transition-colors">
            <Bot size={18} className="text-sa-accent" />
            <span className="absolute -bottom-0.5 -right-0.5">
              <StatusDot status={agent.status} pulse={agent.status === 'active'} />
            </span>
          </div>
          <div>
            <p className="font-medium text-sa-text text-sm">{agent.name}</p>
            <AddressChip address={agent.address} />
          </div>
        </div>
        <div className="flex items-center gap-2">
          {agent.status === 'warning' && <Badge variant="warning">Near limit</Badge>}
          {agent.status === 'inactive' && <Badge variant="neutral">Inactive</Badge>}
          {agent.status === 'active' && <Badge variant="success">Active</Badge>}
        </div>
      </div>

      {/* Stats */}
      <div className="grid grid-cols-2 gap-3 mb-4">
        <div className="bg-sa-bg rounded-lg p-3 border border-sa-border">
          <p className="label mb-1">Balance</p>
          <p className="font-mono font-semibold text-sa-text">
            ${agent.balance}
            <span className="text-xs text-sa-text-dim ml-1">{agent.asset}</span>
          </p>
        </div>
        <div className="bg-sa-bg rounded-lg p-3 border border-sa-border">
          <p className="label mb-1">Total Ops</p>
          <p className="font-mono font-semibold text-sa-text flex items-center gap-1">
            <Zap size={12} className="text-sa-accent" />
            {agent.totalOps.toLocaleString()}
          </p>
        </div>
      </div>

      {/* Spend limits */}
      <div className="space-y-2.5">
        <ProgressBar
          value={parseFloat(agent.spentThisHour)}
          max={parseFloat(agent.limitPerHour)}
          label={`Hourly: $${agent.spentThisHour} / $${agent.limitPerHour}`}
          showPercent
          danger={hourPct > 80}
        />
        <ProgressBar
          value={parseFloat(agent.spentToday)}
          max={parseFloat(agent.limitPerDay)}
          label={`Daily: $${agent.spentToday} / $${agent.limitPerDay}`}
          showPercent
          danger={dayPct > 80}
        />
      </div>

      {/* Footer */}
      <div className="flex items-center justify-between mt-4 pt-3 border-t border-sa-border">
        <p className="text-[11px] text-sa-text-dim">Last active {agent.lastActive}</p>
        <div className="flex gap-2 opacity-0 group-hover:opacity-100 transition-opacity">
          <button
            className="w-7 h-7 rounded-md bg-sa-bg border border-sa-border hover:border-sa-accent/40 flex items-center justify-center transition-colors"
            onClick={(e) => e.stopPropagation()}
          >
            <Power size={12} className="text-sa-text-dim" />
          </button>
          <button
            className="w-7 h-7 rounded-md bg-sa-bg border border-sa-border hover:border-sa-accent/40 flex items-center justify-center transition-colors"
            onClick={(e) => e.stopPropagation()}
          >
            <Settings2 size={12} className="text-sa-text-dim" />
          </button>
        </div>
      </div>
    </motion.div>
  );
}

export function AgentsPage() {
  const [selectedAgent, setSelectedAgent] = useState<Agent | null>(null);
  const [filter, setFilter] = useState<'all' | 'active' | 'inactive' | 'warning'>('all');

  const filtered = filter === 'all'
    ? MOCK_AGENTS
    : MOCK_AGENTS.filter((a) => a.status === filter);

  return (
    <div className="flex-1 overflow-auto">
      {/* Header */}
      <div className="border-b border-sa-border px-8 py-5 flex items-center justify-between bg-sa-bg/50 backdrop-blur sticky top-0 z-10">
        <div>
          <h1 className="font-display text-xl font-semibold text-sa-text">Agents</h1>
          <p className="text-xs text-sa-text-dim mt-0.5">Manage your AI agent wallets</p>
        </div>
        <button className="btn-primary flex items-center gap-2 text-sm">
          <Plus size={14} />
          New Agent
        </button>
      </div>

      <div className="p-8">
        {/* Filters */}
        <div className="flex gap-2 mb-6">
          {(['all', 'active', 'warning', 'inactive'] as const).map((f) => (
            <button
              key={f}
              onClick={() => setFilter(f)}
              className={`px-3 py-1.5 rounded-lg text-sm font-medium transition-all ${
                filter === f
                  ? 'bg-sa-accent/10 text-sa-accent border border-sa-accent/30'
                  : 'text-sa-text-dim hover:text-sa-text hover:bg-sa-surface border border-transparent'
              }`}
            >
              {f === 'all' ? `All (${MOCK_AGENTS.length})` : f.charAt(0).toUpperCase() + f.slice(1)}
            </button>
          ))}
        </div>

        {/* Grid */}
        <div className="grid grid-cols-2 gap-4">
          <AnimatePresence mode="popLayout">
            {filtered.map((agent) => (
              <AgentCard key={agent.id} agent={agent} onSelect={setSelectedAgent} />
            ))}
          </AnimatePresence>
        </div>

        {/* New agent card */}
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          transition={{ delay: 0.3 }}
          className="mt-4 border-2 border-dashed border-sa-border rounded-xl p-8 flex flex-col items-center justify-center gap-3 hover:border-sa-accent/30 transition-colors cursor-pointer group"
        >
          <div className="w-12 h-12 rounded-xl border-2 border-dashed border-sa-border group-hover:border-sa-accent/40 flex items-center justify-center transition-colors">
            <Plus size={20} className="text-sa-muted group-hover:text-sa-accent transition-colors" />
          </div>
          <p className="text-sm text-sa-text-dim group-hover:text-sa-text transition-colors">
            Deploy a new agent wallet
          </p>
          <p className="text-xs text-sa-muted">
            Creates a Soroban wallet + payment channel on Stellar
          </p>
        </motion.div>
      </div>
    </div>
  );
}
