export interface Agent {
  id: string;
  name: string;
  address: string;
  status: 'active' | 'inactive' | 'warning';
  balance: string;
  asset: string;
  spentToday: string;
  spentThisHour: string;
  limitPerHour: string;
  limitPerDay: string;
  totalOps: number;
  lastActive: string;
  channelId: string;
}

export interface Payment {
  id: string;
  agentId: string;
  agentName: string;
  recipient: string;
  amount: string;
  asset: string;
  endpoint: string;
  ledger: number;
  timestamp: string;
  status: 'success' | 'failed' | 'pending';
}

export interface Job {
  id: string;
  requester: string;
  requesterName: string;
  worker: string | null;
  workerName: string | null;
  task: string;
  amount: string;
  asset: string;
  status: 'open' | 'in_progress' | 'pending_release' | 'completed' | 'refunded' | 'disputed';
  deadline: string;
  createdAt: string;
}

export interface SpendDataPoint {
  time: string;
  spend: number;
  ops: number;
}

// ─── Mock Data ────────────────────────────────────────────────────────────────

export const MOCK_AGENTS: Agent[] = [
  {
    id: '1',
    name: 'Inference Agent',
    address: 'GDQP2KPQGKIHYJGXNUIYOMHARUARCA7DJT5FO2FFOOKY3B2WSQHG4W37',
    status: 'active',
    balance: '142.50',
    asset: 'USDC',
    spentToday: '8.23',
    spentThisHour: '1.45',
    limitPerHour: '5.00',
    limitPerDay: '25.00',
    totalOps: 4821,
    lastActive: '2 seconds ago',
    channelId: 'ch_001',
  },
  {
    id: '2',
    name: 'Data Scraper',
    address: 'GBZXN7PIRZGNMHGA7MUUUF4GWPY5AYPGK6XZDSTBY6BMUHC765WUKRQ',
    status: 'active',
    balance: '57.10',
    asset: 'USDC',
    spentToday: '2.10',
    spentThisHour: '0.20',
    limitPerHour: '3.00',
    limitPerDay: '15.00',
    totalOps: 1230,
    lastActive: '14 seconds ago',
    channelId: 'ch_002',
  },
  {
    id: '3',
    name: 'Summarizer Bot',
    address: 'GCKFBEIYV2U22IO2BJ4KVJOIP7XPWQGQFKKWXR62YQCZYHUFGDABRQ5',
    status: 'warning',
    balance: '3.80',
    asset: 'USDC',
    spentToday: '14.90',
    spentThisHour: '4.80',
    limitPerHour: '5.00',
    limitPerDay: '15.00',
    totalOps: 892,
    lastActive: '1 minute ago',
    channelId: 'ch_003',
  },
  {
    id: '4',
    name: 'Code Review Agent',
    address: 'GA2224DCGO3WHC4EALA2PR2BZEMAYZPBPTHS243ZYYWQMBWRPJSZH5A6',
    status: 'inactive',
    balance: '200.00',
    asset: 'USDC',
    spentToday: '0',
    spentThisHour: '0',
    limitPerHour: '10.00',
    limitPerDay: '50.00',
    totalOps: 0,
    lastActive: '3 hours ago',
    channelId: 'ch_004',
  },
];

export const MOCK_PAYMENTS: Payment[] = [
  {
    id: 'p1',
    agentId: '1',
    agentName: 'Inference Agent',
    recipient: 'GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ',
    amount: '0.002',
    asset: 'USDC',
    endpoint: 'api.openai.com/v1/chat',
    ledger: 52241983,
    timestamp: 'Just now',
    status: 'success',
  },
  {
    id: 'p2',
    agentId: '1',
    agentName: 'Inference Agent',
    recipient: 'GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ',
    amount: '0.002',
    asset: 'USDC',
    endpoint: 'api.openai.com/v1/chat',
    ledger: 52241981,
    timestamp: '5s ago',
    status: 'success',
  },
  {
    id: 'p3',
    agentId: '2',
    agentName: 'Data Scraper',
    recipient: 'GBOVKZBEM2YYLOCDCUXJ4IMRKHN4LCJAE7WEAEA2KF562XFAGDBOB64',
    amount: '0.001',
    asset: 'USDC',
    endpoint: 'api.firecrawl.dev/v1/scrape',
    ledger: 52241979,
    timestamp: '14s ago',
    status: 'success',
  },
  {
    id: 'p4',
    agentId: '3',
    agentName: 'Summarizer Bot',
    recipient: 'GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ',
    amount: '0.003',
    asset: 'USDC',
    endpoint: 'api.anthropic.com/v1/messages',
    ledger: 52241970,
    timestamp: '1m ago',
    status: 'success',
  },
  {
    id: 'p5',
    agentId: '3',
    agentName: 'Summarizer Bot',
    recipient: 'GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ',
    amount: '5.10',
    asset: 'USDC',
    endpoint: 'api.anthropic.com/v1/messages',
    ledger: 52241966,
    timestamp: '2m ago',
    status: 'failed',
  },
];

export const MOCK_JOBS: Job[] = [
  {
    id: 'j1',
    requester: 'GDQP2KPQGKIHYJGXNUIYOMHARUARCA7DJT5FO2FFOOKY3B2WSQHG4W37',
    requesterName: 'Inference Agent',
    worker: 'GCKFBEIYV2U22IO2BJ4KVJOIP7XPWQGQFKKWXR62YQCZYHUFGDABRQ5',
    workerName: 'Summarizer Bot',
    task: 'Summarize Q3 earnings report PDF — ipfs://QmX9...',
    amount: '0.05',
    asset: 'USDC',
    status: 'pending_release',
    deadline: 'in 2 hours',
    createdAt: '10 minutes ago',
  },
  {
    id: 'j2',
    requester: 'GBZXN7PIRZGNMHGA7MUUUF4GWPY5AYPGK6XZDSTBY6BMUHC765WUKRQ',
    requesterName: 'Data Scraper',
    worker: null,
    workerName: null,
    task: 'Classify 500 product images into categories',
    amount: '0.25',
    asset: 'USDC',
    status: 'open',
    deadline: 'in 6 hours',
    createdAt: '3 minutes ago',
  },
];

export const MOCK_SPEND_DATA: SpendDataPoint[] = [
  { time: '00:00', spend: 0.8, ops: 120 },
  { time: '02:00', spend: 0.4, ops: 60 },
  { time: '04:00', spend: 0.2, ops: 30 },
  { time: '06:00', spend: 1.1, ops: 180 },
  { time: '08:00', spend: 3.4, ops: 520 },
  { time: '10:00', spend: 5.8, ops: 890 },
  { time: '12:00', spend: 4.2, ops: 640 },
  { time: '14:00', spend: 6.1, ops: 940 },
  { time: '16:00', spend: 7.3, ops: 1120 },
  { time: '18:00', spend: 5.5, ops: 850 },
  { time: '20:00', spend: 3.8, ops: 580 },
  { time: '22:00', spend: 2.1, ops: 320 },
];
