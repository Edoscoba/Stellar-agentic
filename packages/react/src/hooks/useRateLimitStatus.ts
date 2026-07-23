import { useCallback } from 'react';
import type { RateLimitStatus } from '@stellaragent/core';
import { useStellarAgent } from '../StellarAgentProvider.js';
import { usePolling, type UsePollingOptions, type UsePollingResult } from '../internal/usePolling.js';

/**
 * Polls `RateLimiter.get_status` (current usage + configured limits) via
 * the current `StellarAgent`. Disabled until the agent is `ready`.
 */
export function useRateLimitStatus(
  options?: UsePollingOptions,
): UsePollingResult<RateLimitStatus> {
  const { agent, status } = useStellarAgent();

  const fetcher = useCallback(() => {
    if (!agent) {
      return Promise.reject(new Error('useRateLimitStatus: agent not ready'));
    }
    return agent.getRateLimitStatus();
  }, [agent]);

  const enabled = Boolean(agent) && status === 'ready';

  return usePolling(enabled ? fetcher : null, options);
}
