import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useState,
  type ReactNode,
} from 'react';
import { StellarAgent, type StellarAgentConfig } from '@stellaragent/core';
import type { AsyncStatus } from './internal/usePolling.js';

function toError(err: unknown): Error {
  return err instanceof Error ? err : new Error(String(err));
}

// ─── Agent context ───────────────────────────────────────────────────────────

export interface StellarAgentContextValue {
  agent: StellarAgent | null;
  /** `idle` before the provider has started constructing the agent. */
  status: AsyncStatus;
  error: Error | null;
}

const StellarAgentContext = createContext<StellarAgentContextValue | undefined>(undefined);

/** The live agent + its async init status. Must be used within a `<StellarAgentProvider>`. */
export function useStellarAgent(): StellarAgentContextValue {
  const ctx = useContext(StellarAgentContext);
  if (!ctx) {
    throw new Error('useStellarAgent must be used within a <StellarAgentProvider>');
  }
  return ctx;
}

// ─── Pending (optimistic) payments ──────────────────────────────────────────
//
// Shared per-provider so a `usePayForAPI()` mutation in one component can
// make its optimistic delta visible to a `useSpendReport()` reader in a
// completely different component, without either hook needing a direct
// reference to the other. See `usePayForAPI` / `useSpendReport`.

export interface PendingPayment {
  id: string;
  amountStroops: bigint;
}

interface PendingPaymentsState {
  pending: PendingPayment[];
  /** Bumped on settle (success or failure) to force an immediate re-poll. */
  version: number;
}

type PendingPaymentsAction =
  | { type: 'add'; payment: PendingPayment }
  | { type: 'remove'; id: string }
  | { type: 'bump' };

function pendingPaymentsReducer(
  state: PendingPaymentsState,
  action: PendingPaymentsAction,
): PendingPaymentsState {
  switch (action.type) {
    case 'add':
      return { ...state, pending: [...state.pending, action.payment] };
    case 'remove':
      return { ...state, pending: state.pending.filter((p) => p.id !== action.id) };
    case 'bump':
      return { ...state, version: state.version + 1 };
    default:
      return state;
  }
}

export interface PendingPaymentsContextValue extends PendingPaymentsState {
  addPending: (payment: PendingPayment) => void;
  removePending: (id: string) => void;
  /** Force every mounted `useSpendReport()` to refetch immediately. */
  bump: () => void;
}

const PendingPaymentsContext = createContext<PendingPaymentsContextValue | undefined>(undefined);

/** @internal Used by `useSpendReport` / `usePayForAPI`. */
export function usePendingPayments(): PendingPaymentsContextValue {
  const ctx = useContext(PendingPaymentsContext);
  if (!ctx) {
    throw new Error('usePendingPayments must be used within a <StellarAgentProvider>');
  }
  return ctx;
}

// ─── Provider ────────────────────────────────────────────────────────────────

export interface StellarAgentProviderProps {
  /** Same config shape accepted by `StellarAgent.create`. */
  config: StellarAgentConfig;
  children: ReactNode;
  /**
   * Provide an already-constructed agent (real or mocked) instead of
   * having the provider call `StellarAgent.create(config)` itself. When
   * set, `config` is ignored for construction and the agent is considered
   * `ready` immediately. Intended for tests and for demos that don't want
   * to hit a real network — see `packages/react/example`.
   */
  agent?: StellarAgent;
}

/**
 * Owns a `StellarAgent` instance (built from `config` via
 * `StellarAgent.create`, unless `agent` is supplied directly) and exposes
 * it — plus its async init status — to `useStellarAgent()` and every hook
 * in this package. Also owns the optimistic-payment overlay shared
 * between `usePayForAPI` and `useSpendReport`.
 */
export function StellarAgentProvider({
  config,
  children,
  agent: injectedAgent,
}: StellarAgentProviderProps) {
  const [state, setState] = useState<StellarAgentContextValue>(() =>
    injectedAgent
      ? { agent: injectedAgent, status: 'ready', error: null }
      : { agent: null, status: 'idle', error: null },
  );

  // `config` is plain data (network name, optional secret key / spend
  // limit / contract overrides), so a content-based key lets an inline
  // `{ network: 'testnet' }` object literal at the call site avoid
  // re-triggering agent construction on every render.
  const configKey = useMemo(() => JSON.stringify(config), [config]);

  useEffect(() => {
    if (injectedAgent) {
      setState({ agent: injectedAgent, status: 'ready', error: null });
      return;
    }

    let cancelled = false;
    setState({ agent: null, status: 'loading', error: null });

    StellarAgent.create(config)
      .then((agent) => {
        if (!cancelled) setState({ agent, status: 'ready', error: null });
      })
      .catch((err) => {
        if (!cancelled) setState({ agent: null, status: 'error', error: toError(err) });
      });

    return () => {
      cancelled = true;
    };
    // `config` is intentionally represented by `configKey`; `injectedAgent`
    // covers the mock/test path.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configKey, injectedAgent]);

  const [pendingState, dispatch] = useReducer(pendingPaymentsReducer, {
    pending: [],
    version: 0,
  });
  const pendingValue = useMemo<PendingPaymentsContextValue>(
    () => ({
      ...pendingState,
      addPending: (payment) => dispatch({ type: 'add', payment }),
      removePending: (id) => dispatch({ type: 'remove', id }),
      bump: () => dispatch({ type: 'bump' }),
    }),
    [pendingState],
  );

  return (
    <StellarAgentContext.Provider value={state}>
      <PendingPaymentsContext.Provider value={pendingValue}>
        {children}
      </PendingPaymentsContext.Provider>
    </StellarAgentContext.Provider>
  );
}
