/**
 * Deterministic math utilities for Stellar Agent SDK.
 *
 * Import from this barrel to get both the fixed-point primitives and the
 * bidding algorithm helpers in one shot:
 *
 * @example
 * ```typescript
 * import { rankBids, fmt, toStroops } from '@stellaragent/core/math';
 * ```
 */

export * from './fixed-point.js';
export * from './bid.js';
export * from './attestation.js';
export * from './predict.js';
