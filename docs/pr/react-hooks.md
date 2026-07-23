# [React SDK] Implement StellarAgentProvider + live hooks (replaces always-null stub)

## Problem

The entirety of `packages/react/src/index.ts` was:

```ts
export function useStellarAgent() {
  const [agent, setAgent] = useState<StellarAgent | null>(null);
  useEffect(() => {
    // Basic hook implementation for scaffolding
  }, []);
  return agent;
}
```

It always returned `null`. There was no way to initialize, configure, or
interact with a `StellarAgent` from React, despite `@stellaragent/react`
existing as a published-shaped package.

## Summary

- **`StellarAgentProvider`** owns a `StellarAgent` instance — built via
  `StellarAgent.create(config)` (same config shape `StellarAgent.create`
  accepts), or an injected `agent` prop for tests/demos — and exposes
  `{ agent, status, error }` through context, where `status` is `'idle' |
  'loading' | 'ready' | 'error'`.
- **`useStellarAgent()`** returns the live agent + status from that
  context, replacing the always-null stub.
- **`useChannel(channelId)`**, **`useJob(jobId)`**, **`useRateLimitStatus()`**
  poll their underlying `StellarAgent` query method on an interval
  (`internal/usePolling.ts`), each with proper `idle`/`loading`/`ready`/`error`
  state, cleanup on unmount, and no stale-response races (a monotonic
  request-id guards against a slow in-flight request landing after a
  newer one, or after unmount).
- **`usePayForAPI()`** is an optimistic-update mutation hook: calling it
  immediately adds the pending amount to a spend overlay shared (via
  `StellarAgentProvider`) with every `useSpendReport()` consumer anywhere
  in the tree — no direct reference between the two hooks needed. On
  settle, the pending entry is removed and a shared version counter is
  bumped, forcing every mounted `useSpendReport()` to refetch immediately:
  reconciling against the confirmed number on success, or reverting
  (rollback) on failure.
- **`packages/core`** gains `getChannel` / `getRateLimitStatus` (mirroring
  the existing `getJob` / `getSpendReport` stub pattern — still `Not yet
  implemented`, pending the companion SDK issue) and `RateLimitStatus` /
  `SpendReport` types — plus re-exports its **entire public type surface**,
  which previously wasn't exported from the package root at all (only
  imported internally), so `@stellaragent/react` had no way to import
  `StellarAgentConfig`, `ChannelInfo`, etc.
- Adds `@testing-library/react` + `vitest` to `packages/react` (neither
  was a dependency before) and **20 tests across 6 files**.
- Adds `packages/react/example`, a minimal Vite app exercising every hook.

## Why the spend overlay lives in the provider, not the mutation hook

`usePayForAPI` and `useSpendReport` are typically used in different
components (a "pay" button here, a dashboard there). For the optimistic
bump to be visible "immediately" per the requirement, both need to read
from the same source of truth — so `StellarAgentProvider` owns a small
`useReducer`-backed pending-payments list plus a version counter, and both
hooks read/write it via context rather than needing a direct handle to
each other. The version-counter bump is what turns "eventually catches up
on the next poll tick" into "reconciles right away."

All monetary arithmetic in the overlay (`applyPending` in
`useSpendReport.ts`) goes through `@stellaragent/core`'s deterministic
fixed-point helpers (`add`/`sub`/`clamp`/`toStr`) rather than native `+`/`-`,
per that module's own documented rule for monetary values — never touch
them with a JS float.

## Testing

`pnpm --filter @stellaragent/core --filter @stellaragent/react typecheck`,
`lint`, `test`, and `build` all pass. `pnpm --filter @stellaragent/react-example
typecheck` and `build` (real `tsc` + `vite build`) pass.

20 tests across:

- `StellarAgentProvider.test.tsx` (4) — injected-agent ready-immediately
  path, `idle -> loading -> ready` when constructing from config (via
  `vi.spyOn(StellarAgent, 'create')`), the error path, and
  `useStellarAgent()` throwing outside a provider.
- `usePolling.test.ts` (5) — the shared primitive in isolation: disabled
  when `fetcher` is `null`, interval re-fetch, manual `refetch`, error
  state, and no further fetches after unmount (all with `vi.useFakeTimers()`).
- `useChannel.test.tsx` (4) / `useJob.test.tsx` (3) / `useRateLimitStatus.test.tsx`
  (2) — disabled until ready/id-defined, data loads, agent errors surface,
  interval polling + unmount cleanup.
- `optimisticPayForAPI.test.tsx` (2) — **the core requirement**: a payment
  in flight (deferred promise, not yet resolved) is already reflected in
  `useSpendReport()`'s numbers; on confirmation the pending entry clears
  and the numbers match the new confirmed total exactly (not double-counted);
  on rejection, the optimistic bump is fully rolled back to the last real
  poll's numbers.

### Verified in a real browser, not just jsdom

Built the example app for real (`tsc && vite build`) and rendered the
built output in headless Chrome (`google-chrome --headless=new --dump-dom`)
against `vite preview` — confirmed all four cards (Channel, Spend report,
Rate limit, Pay for API call) render live mock data end-to-end through the
actual hook stack, not simulated in a test harness.

## Scope decision: the example uses a mock agent by default

The acceptance criteria asks for the example to render live data "against
a local Soroban standalone network." I could not honestly deliver that:
every Soroban-facing method on `StellarAgent` — `getChannel`,
`getSpendReport`, `getRateLimitStatus`, `payForAPI`, `openChannel`, `getJob`
— is currently a stub that throws `Not yet implemented`, regardless of
which network it's pointed at. That's blocked on the companion SDK issue,
not anything in `packages/react`.

So the example defaults to a small in-memory mock agent
(`example/src/demoAgent.ts`) so the hook layer — polling, loading/error
states, optimistic updates — can be demonstrated end-to-end today, with a
toggle that switches to a real `StellarAgentProvider config={{ network:
'local' }}` (no injected agent) to make explicit that the hook layer
itself is decoupled from that work landing: same components, same hooks,
zero code changes, once the underlying `StellarAgent` methods are wired up
to real contract calls. Toggling it today correctly surfaces each card's
real `error` state rather than silently showing nothing — see
`example/README.md` for the full explanation.

## Also included

- Pinned `packageManager` in the root `package.json` (`pnpm@9`, via
  `corepack use pnpm@9`) — the unpinned default resolved to pnpm 11, which
  requires Node ≥22.13 and fails outright (`ERR_UNKNOWN_BUILTIN_MODULE:
  node:sqlite`) under the Node 20 in this environment. Without this,
  `pnpm install` doesn't work at all here.

## Out of scope / follow-ups

- Event-indexer-based subscriptions (mentioned in the issue as an
  alternative to polling) — no such indexer exists yet; polling is the
  documented fallback the issue itself calls out ("via polling initially
  ... once it exists").
- `StellarAgent`'s actual Soroban contract calls — blocked on the
  companion SDK issue; `getChannel`/`getRateLimitStatus` were added here
  only as typed stubs so the hooks have a real, correctly-typed method to
  call once that work lands.
- Dashboard app (`dashboard/`) doesn't yet use `@stellaragent/react`'s
  hooks — out of scope for this issue, not touched.

## Files changed

- `packages/react/src/StellarAgentProvider.tsx` — provider + both contexts.
- `packages/react/src/internal/usePolling.ts` — shared polling primitive.
- `packages/react/src/hooks/` — `useChannel`, `useJob`, `useRateLimitStatus`,
  `useSpendReport`, `usePayForAPI`.
- `packages/react/src/index.ts` — barrel export (replaces the stub).
- `packages/react/src/__tests__/`, `packages/react/src/test/` — 20 tests +
  mock-agent helper + vitest setup.
- `packages/react/vitest.config.ts`, `packages/react/package.json`,
  `packages/react/.eslintrc.cjs` — test tooling + `eslint-plugin-react-hooks`.
- `packages/react/example/` — the example app.
- `packages/core/src/index.ts`, `packages/core/src/types/index.ts` —
  `getChannel`/`getRateLimitStatus` stubs, `RateLimitStatus`/`SpendReport`
  types, full public type re-export.
- `package.json`, `pnpm-workspace.yaml`, `pnpm-lock.yaml` — `packageManager`
  pin, example app added to the workspace.
