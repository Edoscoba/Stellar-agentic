import { motion } from 'framer-motion';
import { ArrowUpRight, CheckCircle2, XCircle, Clock } from 'lucide-react';
import { Badge, AddressChip, SectionHeader, Card } from '../components/ui/index.js';
import { MOCK_PAYMENTS } from '../lib/mockData.js';

function statusIcon(status: string) {
  if (status === 'success') return <CheckCircle2 size={14} className="text-sa-green" />;
  if (status === 'failed') return <XCircle size={14} className="text-sa-red" />;
  return <Clock size={14} className="text-sa-yellow" />;
}

function statusBadge(status: string) {
  if (status === 'success') return <Badge variant="success">Success</Badge>;
  if (status === 'failed') return <Badge variant="danger">Failed</Badge>;
  return <Badge variant="warning">Pending</Badge>;
}

export function PaymentsPage() {
  const totalSuccess = MOCK_PAYMENTS.filter((p) => p.status === 'success').length;
  const totalFailed = MOCK_PAYMENTS.filter((p) => p.status === 'failed').length;
  const totalVolume = MOCK_PAYMENTS
    .filter((p) => p.status === 'success')
    .reduce((sum, p) => sum + parseFloat(p.amount), 0)
    .toFixed(4);

  return (
    <div className="flex-1 overflow-auto">
      <div className="border-b border-sa-border px-8 py-5 bg-sa-bg/50 backdrop-blur sticky top-0 z-10">
        <h1 className="font-display text-xl font-semibold text-sa-text">Payments</h1>
        <p className="text-xs text-sa-text-dim mt-0.5">On-chain audit trail for all agent payments</p>
      </div>

      <div className="p-8 space-y-6">
        {/* Stats */}
        <div className="grid grid-cols-3 gap-4">
          {[
            { label: 'Successful', value: totalSuccess.toString(), variant: 'success' as const },
            { label: 'Failed', value: totalFailed.toString(), variant: 'danger' as const },
            { label: 'Volume (USDC)', value: `$${totalVolume}`, variant: 'info' as const },
          ].map(({ label, value, variant }) => (
            <Card key={label}>
              <p className="label mb-2">{label}</p>
              <p className="font-display text-2xl font-semibold text-sa-text">{value}</p>
            </Card>
          ))}
        </div>

        {/* Table */}
        <Card>
          <SectionHeader title="Transaction Feed" subtitle="Most recent first" />
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-sa-border">
                  {['Status', 'Agent', 'Endpoint', 'Amount', 'Recipient', 'Ledger', 'Time'].map((h) => (
                    <th key={h} className="label text-left py-2.5 px-3 first:pl-0">{h}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {MOCK_PAYMENTS.map((p, i) => (
                  <motion.tr
                    key={p.id}
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    transition={{ delay: i * 0.04 }}
                    className="border-b border-sa-border/50 hover:bg-sa-bg/40 transition-colors"
                  >
                    <td className="py-3 px-3 first:pl-0">
                      <div className="flex items-center gap-2">
                        {statusIcon(p.status)}
                        {statusBadge(p.status)}
                      </div>
                    </td>
                    <td className="py-3 px-3">
                      <p className="text-sa-text font-medium text-xs">{p.agentName}</p>
                    </td>
                    <td className="py-3 px-3">
                      <p className="font-mono text-xs text-sa-text-dim truncate max-w-[180px]">
                        {p.endpoint}
                      </p>
                    </td>
                    <td className="py-3 px-3">
                      <p className={`font-mono text-sm font-medium ${
                        p.status === 'failed' ? 'text-sa-red line-through' : 'text-sa-text'
                      }`}>
                        ${p.amount}
                      </p>
                    </td>
                    <td className="py-3 px-3">
                      <AddressChip address={p.recipient} />
                    </td>
                    <td className="py-3 px-3">
                      <a
                        href={`https://stellar.expert/explorer/testnet/tx/${p.ledger}`}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="font-mono text-xs text-sa-accent hover:underline flex items-center gap-1"
                      >
                        #{p.ledger}
                        <ArrowUpRight size={10} />
                      </a>
                    </td>
                    <td className="py-3 px-3">
                      <p className="text-xs text-sa-text-dim">{p.timestamp}</p>
                    </td>
                  </motion.tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
      </div>
    </div>
  );
}
