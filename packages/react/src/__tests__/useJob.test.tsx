import { describe, expect, it } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import type { JobInfo } from '@stellaragent/core';
import { StellarAgentProvider } from '../StellarAgentProvider.js';
import { useJob } from '../hooks/useJob.js';
import { createMockAgent } from '../test/mockAgent.js';

const job: JobInfo = {
  id: 7n,
  requester: 'GREQ',
  worker: 'GWORK',
  arbiter: null,
  token: 'USDC',
  amount: 500_0000000n,
  taskDescription: 'Summarize a document',
  result: null,
  deadlineLedger: 123,
  status: 'in_progress',
  createdAt: 1000,
};

describe('useJob', () => {
  it('stays idle when jobId is undefined', () => {
    const mockAgent = createMockAgent();
    const { result } = renderHook(() => useJob(undefined), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    expect(result.current.status).toBe('idle');
    expect(mockAgent.getJob).not.toHaveBeenCalled();
  });

  it('loads job data', async () => {
    const mockAgent = createMockAgent({ getJob: async () => job });

    const { result } = renderHook(() => useJob(7n), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.data).toEqual(job);
  });

  it('surfaces errors from the agent', async () => {
    const mockAgent = createMockAgent({
      getJob: async () => {
        throw new Error('job not found');
      },
    });

    const { result } = renderHook(() => useJob(404n), {
      wrapper: ({ children }) => (
        <StellarAgentProvider config={{ network: 'local' }} agent={mockAgent}>
          {children}
        </StellarAgentProvider>
      ),
    });

    await waitFor(() => expect(result.current.status).toBe('error'));
    expect(result.current.error?.message).toBe('job not found');
  });
});
