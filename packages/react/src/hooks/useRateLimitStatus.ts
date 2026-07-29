import { useCallback } from 'react';
import {
  predictPaymentOutcome,
  ledgersRemainingInWindow,
  estimateSecondsRemaining,
  LEDGERS_PER_CHANNEL_PERIOD,
  RATE_LIMIT_LEDGERS_PER_HOUR,
  RATE_LIMIT_LEDGERS_PER_DAY,
  type RateLimitStatus,
  type ChannelInfo,
  type ChannelSpendState,
  type RateLimitSpendState,
  type PaymentPrediction,
  type LedgerCloseEstimate,
} from '@stellaragent/core';
import { useStellarAgent } from '../StellarAgentProvider.js';
import { usePolling, type UsePollingOptions, type UsePollingResult } from '../internal/usePolling.js';

export interface UseRateLimitStatusOptions extends UsePollingOptions {
  /**
   * Channel to fold into `wouldBlock`/`predict` alongside the rate limiter,
   * via `PaymentChannel.get_channel`/`remaining_this_period`. Omit if the
   * proposed payment doesn't go through a channel at all — `wouldBlock`
   * then reflects the rate limiter only.
   */
  channelId?: bigint;
}

/** A ledger-count window, plus a wall-clock estimate derived from recently observed ledger close times. */
export interface RateLimitWindowEstimate {
  /** Ledgers remaining until this window resets (0 once it has). */
  ledgersRemaining: number;
  /**
   * **Estimated** wall-clock seconds remaining — `ledgersRemaining *`
   * an average ledger close time measured from recent Horizon ledgers, not
   * a hard-coded "5 seconds". Ledger close times drift with network
   * conditions, so treat this as an approximation, not a countdown timer.
   */
  estimatedSecondsRemaining: number;
}

export interface UseRateLimitStatusData {
  /** Raw `RateLimiter.get_limits` result for the queried agent. */
  rateLimit: RateLimitStatus;
  /** Raw `PaymentChannel.get_channel` result, or `null` if no `channelId` was given. */
  channel: ChannelInfo | null;
  /**
   * `false` only when `RateLimiter.set_limits` has never been called for
   * this agent — payments are then unrestricted by the rate limiter
   * (`RateLimiter.check` always returns `true`), though a configured
   * channel's own spend limit can still apply. Distinct from
   * `rateLimitKilled`.
   */
  rateLimitConfigured: boolean;
  /**
   * `true` when a rate limit *is* configured but has been disabled via
   * `RateLimiter.kill_agent`. Informational only: today's on-chain
   * `RateLimiter.check` does not itself gate on this flag (only
   * `is_active()`, a separate query, does) — see `predictPaymentOutcome`'s
   * doc comment in `@stellaragent/core` for the full explanation. Surface
   * this as a "killed" badge, not as something that changes `wouldBlock`.
   */
  rateLimitKilled: boolean;
  /** Time until the rate limiter's rolling hourly window resets. */
  hourWindow: RateLimitWindowEstimate;
  /** Time until the rate limiter's rolling daily window resets. */
  dayWindow: RateLimitWindowEstimate;
  /** Time until the channel's own spend-limit period resets, or `null` when no `channelId` was given. */
  channelPeriodWindow: RateLimitWindowEstimate | null;
  /** Whether `amount` would be blocked by the channel's spend limit and/or the configured rate limiter. */
  wouldBlock: (amount: string) => boolean;
  /** Same check as `wouldBlock`, with the specific reasons attached. */
  predict: (amount: string) => PaymentPrediction;
}

export type UseRateLimitStatusResult = UsePollingResult<UseRateLimitStatusData>;

function toChannelSpendState(channel: ChannelInfo): ChannelSpendState {
  return {
    active: channel.active,
    limitPerPeriod: channel.limitPerPeriod.toString(),
    spentThisPeriod: channel.spentThisPeriod.toString(),
    periodStartLedger: channel.periodStartLedger,
    period: channel.period,
  };
}

function toRateLimitSpendState(status: RateLimitStatus): RateLimitSpendState {
  return {
    configured: status.configured,
    active: status.active,
    maxPerTx: status.maxPerTx,
    maxPerHour: status.maxPerHour,
    maxPerDay: status.maxPerDay,
    maxTxsPerHour: status.maxTxsPerHour,
    hourlySpend: status.spentThisHour,
    dailySpend: status.spentToday,
    hourlyTxCount: status.txsThisHour,
    hourWindowStartLedger: status.hourWindowStartLedger,
    dayWindowStartLedger: status.dayWindowStartLedger,
  };
}

function windowEstimate(
  windowStartLedger: number,
  ledgersPerWindow: number,
  currentLedger: number,
  avgLedgerCloseSeconds: number,
): RateLimitWindowEstimate {
  const ledgersRemaining = ledgersRemainingInWindow(
    windowStartLedger,
    ledgersPerWindow,
    currentLedger,
  );
  return {
    ledgersRemaining,
    estimatedSecondsRemaining: estimateSecondsRemaining(ledgersRemaining, avgLedgerCloseSeconds),
  };
}

function buildData(
  rateLimit: RateLimitStatus,
  channel: ChannelInfo | null,
  ledger: LedgerCloseEstimate,
): UseRateLimitStatusData {
  const { currentLedger, avgLedgerCloseSeconds } = ledger;
  const rateLimitState = toRateLimitSpendState(rateLimit);
  const channelState = channel ? toChannelSpendState(channel) : null;

  const predict = (amount: string): PaymentPrediction =>
    predictPaymentOutcome({ channelState, rateLimitState, amount, currentLedger });

  return {
    rateLimit,
    channel,
    rateLimitConfigured: rateLimit.configured,
    rateLimitKilled: rateLimit.configured && !rateLimit.active,
    hourWindow: windowEstimate(
      rateLimit.hourWindowStartLedger,
      RATE_LIMIT_LEDGERS_PER_HOUR,
      currentLedger,
      avgLedgerCloseSeconds,
    ),
    dayWindow: windowEstimate(
      rateLimit.dayWindowStartLedger,
      RATE_LIMIT_LEDGERS_PER_DAY,
      currentLedger,
      avgLedgerCloseSeconds,
    ),
    channelPeriodWindow: channel
      ? windowEstimate(
          channel.periodStartLedger,
          LEDGERS_PER_CHANNEL_PERIOD[channel.period],
          currentLedger,
          avgLedgerCloseSeconds,
        )
      : null,
    wouldBlock: (amount) => predict(amount).wouldBlock,
    predict,
  };
}

/**
 * Pre-flight rate-limit + spend-limit status for `agentAddress`, polling
 * `RateLimiter.get_limits` and (when `channelId` is given)
 * `PaymentChannel.get_channel`/`remaining_this_period`, plus a Horizon-derived
 * ledger-close estimate to translate ledger-count windows into wall-clock
 * time. Exposes `wouldBlock(amount)` / `predict(amount)`, built on
 * `@stellaragent/core`'s `predictPaymentOutcome`, so a caller can check
 * "would my next payment be blocked?" without a network round trip or a
 * transaction fee.
 *
 * Disabled (stays `idle`) until the agent is `ready`.
 */
export function useRateLimitStatus(
  agentAddress: string,
  options?: UseRateLimitStatusOptions,
): UseRateLimitStatusResult {
  const { agent, status } = useStellarAgent();
  const { channelId, ...pollingOptions } = options ?? {};

  const fetcher = useCallback(async (): Promise<UseRateLimitStatusData> => {
    if (!agent) {
      throw new Error('useRateLimitStatus: agent not ready');
    }
    const [rateLimit, channel, ledger] = await Promise.all([
      agent.getRateLimitStatus(agentAddress),
      channelId !== undefined ? agent.getChannel(channelId) : Promise.resolve(null),
      agent.getLedgerCloseEstimate(),
    ]);
    return buildData(rateLimit, channel, ledger);
  }, [agent, agentAddress, channelId]);

  const enabled = Boolean(agent) && status === 'ready';

  return usePolling(enabled ? fetcher : null, pollingOptions);
}
