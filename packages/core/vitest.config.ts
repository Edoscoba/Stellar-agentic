import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'node',
    globals: true,
    include: ['src/**/*.{test,spec}.ts'],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'lcov', 'json-summary'],
      // Only the modules we actually assert on — pulling the whole package
      // into the report would let the un-implemented Soroban stubs in
      // index.ts drag the global numbers around for no signal.
      include: ['src/math/**/*.ts'],
      exclude: ['src/math/index.ts'],
      // math/ is the correctness-critical core of the SDK: every agent bid
      // score and spend-limit check flows through it, and a regression here
      // is a silent cross-platform determinism break rather than a crash.
      // The suite covers it fully today, so the gate is pinned at 100 — a new
      // uncovered branch here should fail CI, not slip through under an
      // averaged-down percentage. This strictness applies to math/ only.
      thresholds: {
        lines: 100,
        functions: 100,
        branches: 100,
        statements: 100,
      },
    },
  },
});
