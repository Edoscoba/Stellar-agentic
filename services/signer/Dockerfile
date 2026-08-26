# Build the signing service into a distroless image.
#
# The deployable artifact is a static-ish binary with no shell, no package
# manager and no runtime to patch. That is not aesthetics: this container holds
# the authority to move money, so every byte in it that is not the service is
# attack surface someone else maintains.

# ─── Build ────────────────────────────────────────────────────────────────────
FROM rust:1.82-bookworm AS build

WORKDIR /build

# Dependencies first, so a source-only change does not re-download and rebuild
# the world. The dummy sources are replaced below.
COPY Cargo.toml Cargo.lock ./
COPY conformance/Cargo.toml conformance/Cargo.toml
RUN mkdir -p src conformance/src \
 && echo 'fn main() {}' > src/main.rs \
 && echo '' > src/lib.rs \
 && echo 'fn main() {}' > conformance/src/main.rs \
 && echo '' > conformance/src/lib.rs \
 && cargo build --release --bin stellaragent-signer \
 && rm -rf src conformance/src

COPY src src
COPY conformance/src conformance/src

# Touch the real sources so cargo does not reuse the dummy build's fingerprint.
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --bin stellaragent-signer \
 && strip target/release/stellaragent-signer

# ─── Runtime ──────────────────────────────────────────────────────────────────
# `cc` rather than `static`: rustls and the standard library want a libc, and
# nonroot gives us uid 65532 without a useradd we would have to trust.
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=build /build/target/release/stellaragent-signer /usr/local/bin/stellaragent-signer

# Distroless has no shell, so this is exec form by necessity as well as by
# preference — signals reach the process directly and graceful shutdown works.
USER nonroot:nonroot
EXPOSE 8443

# The config is mounted, never baked in: it names key ids and token hashes, and
# an image is a thing that gets pushed to a registry.
VOLUME ["/etc/signer"]

ENTRYPOINT ["/usr/local/bin/stellaragent-signer"]
CMD ["serve", "--config", "/etc/signer/config.toml"]

# Deliberately no HEALTHCHECK instruction: the orchestrator should own probe
# cadence and failure thresholds, and a Docker-level healthcheck duplicates
# that with different semantics. Point liveness at /healthz and readiness at
# /readyz — see docs/signer-deployment.md.
