/**
 * Off-chain-verifiable attestations for bid scoring.
 *
 * ## Background
 *
 * {@link rankBids} and {@link selectBestBid} (see `./bid.ts`) are pure,
 * deterministic functions, but they are trusted, off-chain ones: whoever runs
 * the scoring service can simply not run it, or run it and then tell the
 * counterparty about a different "winner" than the one it actually computed.
 * A worker that is only handed a bid set and a claimed result has no way to
 * check that after the fact.
 *
 * This module lets the scoring service produce a **signed attestation**
 * alongside its result, and gives any third party — a worker, an auditor, an
 * arbiter — a standalone verifier that needs nothing from the scorer except
 * that attestation, the original bid set, and a directory of which public
 * keys the scorer is allowed to sign with. The verifier both re-derives the
 * ranking itself (catching a scorer that reports a different bid than the one
 * it actually computed) and checks the signature (catching tampering with the
 * bids or the reported result in transit).
 *
 * This is the lighter-weight, off-chain alternative to full on-chain
 * commit-reveal settlement — useful before that lands, or for callers that
 * don't want the on-chain settlement overhead at all.
 *
 * ## Key rotation
 *
 * An attestation names the `keyEpoch` it was signed under rather than only a
 * bare public key. Verification takes a {@link ScorerKeyDirectory} mapping
 * epochs to public keys (plus an optional validity window per epoch), not a
 * single hard-coded key. That makes rotation a directory update instead of a
 * breaking change:
 *
 * - Retiring a key means giving its epoch a `validUntil`, or dropping it from
 *   the directory outright once no attestation signed under it needs to
 *   verify any more. Attestations already issued before that cutoff keep
 *   verifying; new ones claiming that epoch after the cutoff do not — so a
 *   compromised key can be cut off going forward without invalidating
 *   history.
 * - `issuedAt` / `expiresAt` bound every attestation's own lifetime
 *   independently of key rotation, so a leaked (bids, result, attestation)
 *   tuple can't be replayed indefinitely as "proof" of a stale ranking.
 *
 * @module attestation
 */

import { Keypair, hash } from '@stellar/stellar-sdk';

import type { AgentBid, BidWeights, ScoredBid } from './bid.js';
import { rankBids, DEFAULT_BID_WEIGHTS } from './bid.js';

const ATTESTATION_VERSION = 1 as const;

/** Default attestation lifetime, in seconds. */
const DEFAULT_TTL_SECONDS = 300;

// ─── Types ───────────────────────────────────────────────────────────────────

/**
 * A signed claim that `rankBids(bids, weights)` produced `result` at
 * `issuedAt`, under the key identified by `keyEpoch` / `scorerPublicKey`.
 */
export interface BidAttestation {
  /** Attestation schema version. */
  version: typeof ATTESTATION_VERSION;
  /** Identifies which scorer keypair signed this — see key-rotation notes above. */
  keyEpoch: number;
  /** The scorer's Stellar public address (`G...`) for this epoch. */
  scorerPublicKey: string;
  /** The weights the scorer ran `rankBids` with. */
  weights: BidWeights;
  /** Unix seconds when this attestation was produced. */
  issuedAt: number;
  /** Unix seconds after which this attestation must no longer be trusted. */
  expiresAt: number;
  /** Hex sha256 over the canonicalized (bids, weights, result) triple. */
  digest: string;
  /** Base64 ed25519 signature, produced by the scorer keypair, over the rest of this object. */
  signature: string;
}

export interface AttestRankBidsOptions {
  /** Which epoch `scorerKeypair` belongs to. Bump this when rotating keys. */
  keyEpoch: number;
  /** How long the attestation remains valid for, in seconds. @default 300 */
  ttlSeconds?: number;
  /** Injectable clock, for tests. @default Date.now */
  now?: () => number;
}

export interface AttestedRanking {
  /** Identical to `rankBids(bids, weights)` — nothing about scoring changes. */
  result: ScoredBid[];
  attestation: BidAttestation;
}

/** One scorer key a verifier is willing to trust, and for how long. */
export interface ScorerKeyRecord {
  /** Matches {@link BidAttestation.keyEpoch}. */
  epoch: number;
  /** The Stellar public address this epoch is allowed to sign with. */
  publicKey: string;
  /** Unix seconds before which this epoch's key was not yet in use, if bounded. */
  validFrom?: number;
  /** Unix seconds after which this epoch's key was retired, if bounded. */
  validUntil?: number;
}

/** The set of scorer keys (current and, optionally, recently-retired) a verifier trusts. */
export type ScorerKeyDirectory = readonly ScorerKeyRecord[];

export interface VerifyBidAttestationOptions {
  /** Injectable clock, for tests. @default Date.now */
  now?: () => number;
}

export type BidAttestationVerification =
  | { valid: true; recomputed: ScoredBid[] }
  | { valid: false; reason: string };

// ─── Canonicalization ────────────────────────────────────────────────────────
//
// Fixed field order regardless of how the caller's objects were constructed
// or deserialized, so hashing is stable and equality checks can't be fooled
// by key reordering or extra properties tacked onto an input object.

function canonicalBid(bid: AgentBid) {
  return {
    agentAddress: bid.agentAddress,
    price: bid.price,
    reputation: bid.reputation,
    estimatedLatencySeconds: bid.estimatedLatencySeconds,
    successRate: bid.successRate,
  };
}

function canonicalWeights(weights: BidWeights) {
  return {
    price: weights.price,
    reputation: weights.reputation,
    latency: weights.latency,
    reliability: weights.reliability,
  };
}

function canonicalScoredBid(scored: ScoredBid) {
  return {
    agentAddress: scored.agentAddress,
    score: scored.score,
    breakdown: {
      priceScore: scored.breakdown.priceScore,
      reputationScore: scored.breakdown.reputationScore,
      latencyScore: scored.breakdown.latencyScore,
      reliabilityScore: scored.breakdown.reliabilityScore,
    },
  };
}

/** Order-independent equality for two `ScoredBid[]` (field order, not array order, is normalized). */
function scoredBidsEqual(a: ScoredBid[], b: ScoredBid[]): boolean {
  if (a.length !== b.length) return false;
  return a.every(
    (bid, i) => JSON.stringify(canonicalScoredBid(bid)) === JSON.stringify(canonicalScoredBid(b[i]!)),
  );
}

/**
 * sha256 over the canonicalized (bids, weights, result) triple.
 *
 * Bids are sorted by `agentAddress` before hashing — `rankBids` itself is
 * input-order-independent (see bid.test.ts), so two bid arrays containing the
 * same set in different orders must digest identically. `result` is *not*
 * sorted: its order is the ranking, and is exactly what this is meant to
 * pin down.
 */
function digestBidSet(bids: AgentBid[], weights: BidWeights, result: ScoredBid[]): Buffer {
  const canonicalBids = bids
    .map(canonicalBid)
    .sort((x, y) => (x.agentAddress < y.agentAddress ? -1 : x.agentAddress > y.agentAddress ? 1 : 0));
  const payload = JSON.stringify({
    bids: canonicalBids,
    weights: canonicalWeights(weights),
    result: result.map(canonicalScoredBid),
  });
  return hash(Buffer.from(payload, 'utf8'));
}

/** sha256 over every attestation field except the signature itself. */
function digestAttestationHeader(header: Omit<BidAttestation, 'signature'>): Buffer {
  const payload = JSON.stringify({
    version: header.version,
    keyEpoch: header.keyEpoch,
    scorerPublicKey: header.scorerPublicKey,
    weights: canonicalWeights(header.weights),
    issuedAt: header.issuedAt,
    expiresAt: header.expiresAt,
    digest: header.digest,
  });
  return hash(Buffer.from(payload, 'utf8'));
}

// ─── Attest ──────────────────────────────────────────────────────────────────

/**
 * Run `rankBids` and sign an attestation over the (bids, weights, result)
 * triple with `scorerKeypair`.
 *
 * `scorerKeypair` must hold a secret key — this is meant to run inside the
 * scoring service, not on a verifier. See {@link verifyBidAttestation} for
 * the side that only needs the public key.
 *
 * @throws {RangeError} if `scorerKeypair` has no secret, `keyEpoch` isn't a
 *   non-negative integer, or `ttlSeconds` isn't positive. Propagates
 *   `rankBids`'s own `RangeError` for invalid weights or bid fields.
 */
export function attestRankBids(
  bids: AgentBid[],
  weights: BidWeights = DEFAULT_BID_WEIGHTS,
  scorerKeypair: Keypair,
  options: AttestRankBidsOptions,
): AttestedRanking {
  if (!scorerKeypair.canSign()) {
    throw new RangeError('attestRankBids: scorerKeypair must hold a secret key to sign attestations');
  }
  if (!Number.isInteger(options.keyEpoch) || options.keyEpoch < 0) {
    throw new RangeError(`attestRankBids: keyEpoch must be a non-negative integer, got ${options.keyEpoch}`);
  }
  const ttlSeconds = options.ttlSeconds ?? DEFAULT_TTL_SECONDS;
  if (!(ttlSeconds > 0)) {
    throw new RangeError(`attestRankBids: ttlSeconds must be positive, got ${ttlSeconds}`);
  }

  const result = rankBids(bids, weights);
  const digest = digestBidSet(bids, weights, result).toString('hex');

  const nowMs = options.now ? options.now() : Date.now();
  const issuedAt = Math.floor(nowMs / 1000);
  const expiresAt = issuedAt + ttlSeconds;

  const header: Omit<BidAttestation, 'signature'> = {
    version: ATTESTATION_VERSION,
    keyEpoch: options.keyEpoch,
    scorerPublicKey: scorerKeypair.publicKey(),
    weights: canonicalWeights(weights),
    issuedAt,
    expiresAt,
    digest,
  };
  const signature = scorerKeypair.sign(digestAttestationHeader(header)).toString('base64');

  return { result, attestation: { ...header, signature } };
}

// ─── Verify ──────────────────────────────────────────────────────────────────

/**
 * Independently confirm that a scoring service didn't cheat.
 *
 * Given only `bids`, the `result` it claims to have produced, its
 * `attestation`, and a directory of which scorer keys are trusted, this:
 *
 * 1. Looks up `attestation.keyEpoch` in `trustedKeys` and rejects an unknown
 *    epoch, a public-key mismatch for a known epoch, or an epoch used outside
 *    its trusted validity window.
 * 2. Rejects an expired attestation.
 * 3. Verifies the ed25519 signature over the attestation header — this
 *    authenticates every field on the attestation, including `digest` and
 *    `weights`.
 * 4. Recomputes the digest from `bids`/`result` and checks it matches
 *    `attestation.digest` — this catches tampering with either in transit.
 * 5. Recomputes `rankBids(bids, attestation.weights)` from scratch using the
 *    exported `bid.ts` functions and checks it structurally matches `result`
 *    — this is what catches a scorer that computed one ranking but reported
 *    a different "winner".
 *
 * All five must pass for `valid: true`.
 */
export function verifyBidAttestation(
  bids: AgentBid[],
  result: ScoredBid[],
  attestation: BidAttestation,
  trustedKeys: ScorerKeyDirectory,
  options: VerifyBidAttestationOptions = {},
): BidAttestationVerification {
  if (attestation.version !== ATTESTATION_VERSION) {
    return { valid: false, reason: `unsupported attestation version: ${String(attestation.version)}` };
  }

  const record = trustedKeys.find((k) => k.epoch === attestation.keyEpoch);
  if (!record) {
    return { valid: false, reason: `no trusted key registered for epoch ${attestation.keyEpoch}` };
  }
  if (record.publicKey !== attestation.scorerPublicKey) {
    return {
      valid: false,
      reason:
        `attestation's scorerPublicKey does not match the trusted key for epoch ${attestation.keyEpoch}`,
    };
  }
  if (record.validFrom !== undefined && attestation.issuedAt < record.validFrom) {
    return {
      valid: false,
      reason: `key for epoch ${attestation.keyEpoch} was not yet valid when this attestation was issued`,
    };
  }
  if (record.validUntil !== undefined && attestation.issuedAt > record.validUntil) {
    return {
      valid: false,
      reason: `key for epoch ${attestation.keyEpoch} had already been retired when this attestation was issued`,
    };
  }

  const nowSeconds = Math.floor((options.now ? options.now() : Date.now()) / 1000);
  if (attestation.expiresAt < nowSeconds) {
    return { valid: false, reason: 'attestation has expired' };
  }

  let scorer: Keypair;
  try {
    scorer = Keypair.fromPublicKey(attestation.scorerPublicKey);
  } catch {
    return { valid: false, reason: 'attestation scorerPublicKey is not a valid Stellar public key' };
  }

  let signatureBytes: Buffer;
  try {
    signatureBytes = Buffer.from(attestation.signature, 'base64');
  } catch {
    return { valid: false, reason: 'attestation signature is not valid base64' };
  }

  const header: Omit<BidAttestation, 'signature'> = {
    version: attestation.version,
    keyEpoch: attestation.keyEpoch,
    scorerPublicKey: attestation.scorerPublicKey,
    weights: attestation.weights,
    issuedAt: attestation.issuedAt,
    expiresAt: attestation.expiresAt,
    digest: attestation.digest,
  };
  let signatureValid: boolean;
  try {
    signatureValid = scorer.verify(digestAttestationHeader(header), signatureBytes);
  } catch {
    signatureValid = false;
  }
  if (!signatureValid) {
    return { valid: false, reason: 'signature is invalid for the attested scorer public key' };
  }

  const expectedDigest = digestBidSet(bids, attestation.weights, result).toString('hex');
  if (expectedDigest !== attestation.digest) {
    return {
      valid: false,
      reason: 'digest mismatch — the supplied bids/result do not match what the scorer attested to',
    };
  }

  let recomputed: ScoredBid[];
  try {
    recomputed = rankBids(bids, attestation.weights);
  } catch (err) {
    return { valid: false, reason: `recomputation failed: ${(err as Error).message}` };
  }

  if (!scoredBidsEqual(recomputed, result)) {
    return {
      valid: false,
      reason:
        'recomputed ranking does not match the attested result — the scorer may have reported a ' +
        'different bid than the one it actually computed',
    };
  }

  return { valid: true, recomputed };
}
