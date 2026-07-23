import { useCallback } from 'react';
import type { ChannelInfo } from '@stellaragent/core';
import { useStellarAgent } from '../StellarAgentProvider.js';
import { usePolling, type UsePollingOptions, type UsePollingResult } from '../internal/usePolling.js';

/**
 * Polls `PaymentChannel.get_channel` for `channelId` via the current
 * `StellarAgent`. Disabled (stays `idle`) until both the agent is `ready`
 * and `channelId` is defined, so it's safe to call before a channel has
 * been opened yet — e.g. `useChannel(channelId)` where `channelId` starts
 * `undefined`.
 */
export function useChannel(
  channelId: bigint | undefined,
  options?: UsePollingOptions,
): UsePollingResult<ChannelInfo> {
  const { agent, status } = useStellarAgent();

  const fetcher = useCallback(() => {
    if (!agent || channelId === undefined) {
      return Promise.reject(new Error('useChannel: agent not ready or channelId not set'));
    }
    return agent.getChannel(channelId);
  }, [agent, channelId]);

  const enabled = Boolean(agent) && status === 'ready' && channelId !== undefined;

  return usePolling(enabled ? fetcher : null, options);
}
