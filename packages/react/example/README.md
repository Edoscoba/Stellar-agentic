# `@stellaragent/react` example

A minimal Vite + React app exercising every hook in `@stellaragent/react`:
`StellarAgentProvider`, `useChannel`, `useSpendReport`, `useRateLimitStatus`,
and the optimistic `usePayForAPI`.

## Running it

```sh
pnpm install
pnpm --filter @stellaragent/react build   # the example imports the built package
pnpm --filter @stellaragent/react-example dev
```

Then open the printed local URL. You should see three cards (Channel, Spend
report, Rate limit) populate with data within a couple of seconds, and a
"Pay for API call" form — submitting it bumps the spend report's numbers
immediately, then reconciles once the (simulated) payment confirms.

## Mock agent vs. a real local Soroban network

By default this app runs against a small in-memory **mock agent**
(`src/demoAgent.ts`), not a real network. Toggle "Connect to a real local
Soroban network instead" and it switches to a genuine
`StellarAgentProvider config={{ network: 'local' }}` with no injected agent
— which calls the real `StellarAgent.create` from `@stellaragent/core`.

That flip is meant to demonstrate that the hook layer itself doesn't care
which one it's talking to — same components, same hooks, zero code changes.
What it will **not** do yet is show populated data in that mode: every
Soroban-facing method on `StellarAgent` (`getChannel`, `getSpendReport`,
`getRateLimitStatus`, `payForAPI`, …) is currently a stub that throws `Not
yet implemented` — see `packages/core/src/index.ts` and the companion SDK
issue for wiring these up to real contract calls. Toggling "local" mode
today will correctly show each card's `error` state reflecting exactly
that, which is itself the intended, honest behavior: the provider's
`loading`/`error` machinery is real and working, the contract calls behind
it aren't implemented yet.

Once that companion work lands, running this same app against a real
`stellar-cli` / `soroban` standalone network (deploy the contracts in
`contracts/`, fund an account via friendbot, point `config.contracts` at
the deployed addresses) should populate the same cards with live on-chain
data with no changes needed here.
