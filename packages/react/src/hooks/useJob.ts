import { useCallback } from 'react';
import type { JobInfo } from '@stellaragent/core';
import { useStellarAgent } from '../StellarAgentProvider.js';
import { usePolling, type UsePollingOptions, type UsePollingResult } from '../internal/usePolling.js';

/**
 * Polls `Escrow.get_job` for `jobId` via the current `StellarAgent`.
 * Disabled until both the agent is `ready` and `jobId` is defined.
 */
export function useJob(
  jobId: bigint | undefined,
  options?: UsePollingOptions,
): UsePollingResult<JobInfo> {
  const { agent, status } = useStellarAgent();

  const fetcher = useCallback(() => {
    if (!agent || jobId === undefined) {
      return Promise.reject(new Error('useJob: agent not ready or jobId not set'));
    }
    return agent.getJob(jobId);
  }, [agent, jobId]);

  const enabled = Boolean(agent) && status === 'ready' && jobId !== undefined;

  return usePolling(enabled ? fetcher : null, options);
}
