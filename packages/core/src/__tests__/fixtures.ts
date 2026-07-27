import type { ContractAddresses } from '../types/index.js';

/**
 * Shared test fixtures.
 *
 * Not a spec file — `vitest` only collects `*.test.ts` / `*.spec.ts`, so this
 * is safe to import from several suites without re-running anything.
 */

/**
 * A deterministic, well-formed test keypair, derived from an all-0x07 ed25519
 * seed so assertions against it are reproducible. It holds nothing, is never
 * funded on any real network, and must never be used outside these tests.
 */
export const TEST_SECRET = 'SADQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQP54X';
export const TEST_PUBLIC = 'GDVEU3DD4KOFECV66VIHWEZOYX4ZKR3WV27L464SIIPOU2IUI3JCZA57';

/**
 * Structurally valid contract IDs — strkey-encoded from fixed 32-byte
 * payloads, so they carry real checksums and pass `isDeployedAddress`.
 *
 * Nothing is deployed at these addresses. They exist so unit tests can get
 * past `StellarAgent.create()`'s deployed-contracts check without pretending
 * to be an integration test.
 */
export const DEPLOYED_CONTRACTS: ContractAddresses = {
  agentWalletFactory: 'CAAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQC526',
  paymentChannel: 'CABAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAFNSZ',
  escrow: 'CABQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGAYDAMBQGCK3',
  rateLimiter: 'CACAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIBAEAQCAINCW',
  circuitBreaker: 'CACQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQKBIFAUCQLC2U',
};
