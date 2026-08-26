# Running the signing service

Deployment guide and threat model for `services/signer`, the server side of the
`RemoteSigner` protocol in [`docs/signing.md`](signing.md).

Design rationale lives in [`docs/signer-service-design.md`](signer-service-design.md).

---

## Contents

- [What you need first](#what-you-need-first)
- [Creating the key](#creating-the-key)
- [Configuration](#configuration)
- [Issuing and rotating tokens](#issuing-and-rotating-tokens)
- [Running it](#running-it)
- [Pointing an agent at it](#pointing-an-agent-at-it)
- [Health, metrics and alerting](#health-metrics-and-alerting)
- [The audit log](#the-audit-log)
- [Conformance](#conformance)
- [Threat model](#threat-model)
- [Known limitations](#known-limitations)

---

## What you need first

- A KMS key that can do **Ed25519**. This is newer than most people expect —
  AWS KMS only gained EdDSA in November 2025.
- TLS termination in front of the service. It speaks plain HTTP and expects a
  proxy, service mesh or load balancer to own the certificate.
- Somewhere to ship the audit log that the service itself cannot rewrite.

---

## Creating the key

### AWS KMS

```bash
aws kms create-key \
  --key-spec ECC_NIST_EDWARDS25519 \
  --key-usage SIGN_VERIFY \
  --description "stellaragent signer — inference fleet"
```

`ECC_NIST_EDWARDS25519` is the only spec that works. A key created with
`ECC_NIST_P256` produces structurally valid signatures that Stellar rejects,
and the service refuses to start against one rather than discovering it on the
first payment.

The service's IAM role needs `kms:GetPublicKey` and `kms:Sign` on that key, and
nothing else. In particular it must **not** have `kms:ScheduleKeyDeletion` or
`kms:PutKeyPolicy`.

> **The algorithm is pinned, deliberately.** AWS also offers
> `ED25519_PH_SHA_512`, the pre-hashed variant. Configured by mistake it returns
> a perfect 64-byte Ed25519 signature over the wrong message — nothing local
> complains and every transaction is rejected on-chain with an error that points
> nowhere near the cause. The adapter hard-codes `ED25519_SHA_512` with
> `MessageType: RAW` and the conformance suite asserts cross-backend byte
> equality specifically to catch someone "fixing" it later.

### GCP Cloud KMS

```bash
gcloud kms keys create signer-key \
  --keyring stellaragent --location europe-west1 \
  --purpose asymmetric-signing \
  --default-algorithm ec-sign-ed25519
```

### Development

The local keystore holds raw seeds on disk. It exists so the conformance suite
can run in CI without cloud credentials and so a contributor can work against a
local network. It refuses to load without `acknowledge_insecure = true` and
warns on every startup. **Do not point it at anything holding value.**

---

## Configuration

Start from [`services/signer/config.example.toml`](../services/signer/config.example.toml).
Then, before deploying anything:

```bash
stellaragent-signer check --config config.toml
```

That parses every ceiling, resolves every policy reference, and reports the
warnings worth seeing — a token with no expiry, an identity with no token, a key
shared by several identities.

Policy is a file and only a file. There is no endpoint that can change a spend
ceiling, so a change is a reviewed, diffable commit and a compromised caller
cannot loosen its own limits however far it gets.

---

## Issuing and rotating tokens

```bash
stellaragent-signer issue-token
```

Prints the token once — it is not recoverable — and the `token_sha256` to paste
into the config. The service stores only the hash.

**Rotation is overlap, not swap:**

1. Issue a new token and add it to `[[tokens]]` alongside the current one.
2. Reload. Both now work.
3. Roll the fleet onto the new token.
4. Set `revoked_at` on the old one and reload.

There is no window in which an agent cannot sign. Revocation is checked per
request, so step 4 takes effect on the next request rather than at the next
restart.

---

## Running it

```bash
docker build -t stellaragent-signer services/signer
docker run --rm \
  -v /etc/signer:/etc/signer:ro \
  -p 127.0.0.1:8443:8443 \
  stellaragent-signer
```

The image is distroless and runs as `nonroot` (uid 65532). Recommended pod
settings:

```yaml
securityContext:
  runAsNonRoot: true
  readOnlyRootFilesystem: true
  allowPrivilegeEscalation: false
  capabilities: { drop: ["ALL"] }
livenessProbe:  { httpGet: { path: /healthz, port: 8443 } }
readinessProbe: { httpGet: { path: /readyz,  port: 8443 } }
```

The config is mounted, never baked into the image: it names key ids and token
hashes, and an image is a thing that gets pushed to a registry.

`/readyz` deliberately does **not** call KMS. It is unauthenticated and hit
constantly by load balancers, and turning each probe into a billed API call is
a way to be surprised by a bill and to rate-limit yourself out of KMS.

---

## Pointing an agent at it

```typescript
import { StellarAgent, RemoteSigner } from '@stellaragent/core';

const agent = await StellarAgent.create({
  network: 'testnet',
  signer: new RemoteSigner({
    url: 'https://signer.internal',
    token: process.env.SIGNER_TOKEN,
    expectedPublicKey: process.env.AGENT_ADDRESS,
  }),
});

agent.holdsSecretKey;  // false — this is the thing to assert in a smoke test
```

```rust
let agent = StellarAgent::builder()
    .network(Network::Testnet)
    .signer(Arc::new(RemoteSigner::new(
        RemoteSignerOptions::new("https://signer.internal")
            .token(std::env::var("SIGNER_TOKEN")?)
            .expect_public_key(std::env::var("AGENT_ADDRESS")?),
    )?))
    .build()
    .await?;
```

**Set `expectedPublicKey`.** Without it a misconfigured or substituted service
can quietly sign as a different account and the agent will never notice.

---

## Health, metrics and alerting

`/metrics` is Prometheus text format and is served on a separate listener — it
describes signing volume and refusal patterns, which is useful to an operator
and equally useful to someone probing what a policy will accept.

What to alert on, roughly in order of how much it should wake someone:

| Signal | Means |
|---|---|
| `signer_refusals_total{reason="unauthenticated"}` rising | someone is guessing tokens |
| `signer_refusals_total{reason="uninspectable"}` rising | a contract changed shape and the decoder has fallen behind, so **legitimate payments are failing** |
| `signer_policy_violations_total{rule="recipient_allowlist"}` rising | an agent is trying to pay somewhere new |
| `signer_backend_errors_total` rising | KMS is unreachable or the IAM role changed |
| `signer_refusals_total{reason="audit_unavailable"}` non-zero | the log sink is down and **the service is refusing to sign** |
| `signer_signed_stroops_total` rate outside its baseline | see below |

That last one is the important one and the least automatic. Policy catches
requests that break a rule. **Nothing catches a compromised agent operating
quietly within policy** — it shows up only as an unusual rate against a
baseline a human established. If you set up one alert, make it that one.

---

## The audit log

One JSON object per line, hash-chained: each record carries `prev_hash`, and
its own `hash` covers both. Verify by re-walking the file.

A record names the caller, the key, the policy, the decoded calls, the amounts,
the decision and any violations. It never contains a token or key material —
structurally, because there is no field for either.

### What the chain does and does not prove

A hash chain is tamper-**evident**, not tamper-**proof**. Anyone who can rewrite
the log can recompute the whole chain and produce a file that verifies
perfectly.

It becomes evidence only once the head hash is anchored somewhere the attacker
cannot reach:

- ship the stream to a log sink in a **different account** with write-only
  credentials, or
- write to append-only object storage with an object-lock retention policy, or
- publish the head hash periodically somewhere out of reach.

The service prints its head hash on shutdown for exactly this reason. Treating
the chain alone as sufficient would be worse than having no chain, because it
would invite someone to rely on it.

---

## Conformance

```bash
# Grade this service — or any implementation of the protocol:
signer-conformance server --url https://signer.internal --token "$TOKEN"

# Grade a client: start the reference server, point the client at it, Ctrl-C.
signer-conformance client --port 8099
```

Two suites, because Phase 2 and Phase 5 of the epic mean different things by the
word. Backend conformance
(`stellaragent_signer::backend::conformance`) checks that a *key backend* signs
correctly, in-process. Protocol conformance is the binary above, black-box over
HTTP, so it can verify any implementation in any language — including the
TypeScript and Rust `RemoteSigner` clients.

---

## Threat model

### What this protects against

- **Agent process compromise or memory dump.** The agent holds a URL and a
  token. There is no key to find.
- **Secret leakage through the usual channels** — `process.env`, a heap dump,
  an error reporter serialising the object graph, a log line.
- **Unbounded spend by a compromised agent.** A stolen token buys the ability
  to *request* signatures subject to policy — capped per payment, per
  transaction, per window and per recipient — not the key.
- **Signing something nobody looked at.** Every envelope is decoded and every
  contract call matched against a known signature. Anything the service cannot
  fully understand is refused, not signed.
- **Credential rotation without fund migration.** Rotating a token is a config
  change; rotating a key would mean moving funds off every account.
- **Absence of an audit trail.** Every request, granted or refused, is recorded
  before the response is sent, and a sink failure fails the request.

### What it does not protect against

Stated plainly, because a threat model that only lists wins is marketing.

- **Compromise of the signing service itself.** It is now the crown jewels. An
  attacker with code execution here signs whatever policy allows, and can
  rewrite the audit log unless the head hash is anchored externally.
- **A caller acting maliciously within policy.** If policy permits 10 XLM/hour
  to an allowlisted recipient, a compromised agent can spend exactly that,
  every hour, and every request will be correctly authorised and cleanly
  logged. Policy tightness is an operator decision the service cannot make for
  you.
- **Compromise of the KMS credentials.** If the service's IAM role is stolen,
  the thief signs directly against KMS and never touches this service, its
  policy or its log. KMS key policy and CloudTrail are the controls there, not
  this codebase.
- **A malicious or coerced operator.** Whoever can change `policy.toml` and
  restart the service can authorise anything. The mitigation is process — code
  review on the policy file, separation of duties — not code.
- **Transport misconfiguration.** The service expects TLS to terminate in front
  of it. The segment behind that termination is yours to secure.
- **Semantic attacks on the policy engine.** Policy reasons about what it can
  decode. A contract call shaped so its real effect is not visible in the
  arguments inspected would pass. Unrecognised calls are refused rather than
  allowed, so the failure mode is availability rather than silent
  authorisation — but the guarantee is "we understood what we signed", not
  "what we signed was safe".
- **Availability.** This service is a hard dependency for every payment. It is
  a single point of failure by construction.

---

## Known limitations

### v1 is single-replica

Rate-limit budgets live in memory. **Two replicas means two independent
budgets**, so a `500/hour` limit silently becomes `1000/hour`, and a restart
resets the window.

Run one replica. Shared state (Redis, or a small persistent store) is the
obvious follow-up and was deliberately left out of v1, because it turns a
self-contained service into one with a stateful dependency and that trade
deserves its own discussion.

### The ledger estimate is a ratchet, not an oracle

Capping `validUntilLedgerSeq` needs to know the current ledger. The service
does **not** call Soroban RPC for it — that would make signing unavailable
whenever RPC is, and would require trusting an RPC server that could widen the
replay window by under-reporting.

Instead it tracks the highest value it has accepted and refuses to advance by
more than `ledger.max_advance` in one step. The estimate self-calibrates from
honest traffic, and one stolen request cannot buy a long-lived signature.

A patient attacker holding a valid token can still walk the ratchet forward
across many requests — each a separate request, rate-limited and audited. This
bounds the blast radius of a single request; it is not a substitute for
revoking the token.

### Contract coverage is a fixed table

The decoder knows this repo's contracts. A new contract needs a row in
`services/signer/src/inspect.rs`, which is deliberate — a new row is a
reviewable change to what the signer will authorise. Until it is added, calls to
it are refused with `uninspectable`, and `signer_refusals_total{reason="uninspectable"}`
is the signal that this has happened.
