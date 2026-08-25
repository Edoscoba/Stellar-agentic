//! Signing abstraction — what to sign, separated from what holds the key.
//!
//! Rust port of `packages/core/src/signer.ts`.
//!
//! # Why this module exists
//!
//! An SDK built around a raw secret key holds that secret in a long-lived
//! process for its whole lifetime, reachable from a core dump, an environment
//! leak, an error report that serialises the object graph, or any transitive
//! dependency. For an agent moving real funds that is a serious risk.
//!
//! [`Signer`] separates *what to sign* from *what holds the key*. The agent
//! gets a `Signer`; where the key actually lives is the signer's problem. The
//! only things a [`crate::StellarAgent`] ever learns from one are a public
//! address and some signed bytes.
//!
//! # The interface shape
//!
//! [`Signer::sign_transaction`] and [`Signer::sign_auth_entry`] take and
//! return base64 XDR, deliberately the same shape as SEP-43, the Stellar
//! wallet-interface standard. An existing wallet, hardware device or signing
//! service can therefore be adapted with a thin wrapper, and the boundary
//! stays at "here are bytes, give me back signed bytes" — the narrowest
//! interface that never requires key material to cross it.
//!
//! Soroban needs both halves. `sign_transaction` covers the transaction
//! envelope; `sign_auth_entry` covers `SorobanAuthorizationEntry` values,
//! which are signed **separately** from the envelope that carries them. A
//! signer that only implements the first cannot authorise a contract
//! invocation at all.
//!
//! # Why a remote signing service rather than a hardware wallet
//!
//! A hardware wallet requires a physical button press per signature. That is a
//! good property for a human treasury and a fatal one here: the premise of
//! this SDK is an *autonomous* agent paying a fraction of a cent per API call
//! with no human in the loop, and the first payment would block forever
//! waiting for a press. Hardware signing is right for the *admin* keys that
//! deploy and configure contracts; it is wrong for the agent's hot operational
//! key.
//!
//! [`RemoteSigner`] fits: the key lives in an HSM or KMS behind a network
//! boundary, the agent process holds only a URL and a token, and the service
//! is where policy belongs — spend ceilings, rate limits, an audit log,
//! revocation. Compromising the agent then yields the ability to *request*
//! signatures subject to that policy, not the key itself, and rotation means
//! rotating a token rather than migrating every funded account.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use ed25519_dalek::{Signer as _, SigningKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use stellar_xdr::curr::{
    DecoratedSignature, Hash, HashIdPreimage, HashIdPreimageSorobanAuthorization, Limits, ReadXdr,
    ScBytes, ScMap, ScMapEntry, ScSymbol, ScVal, ScVec, Signature, SignatureHint,
    SorobanAuthorizationEntry, SorobanCredentials, TransactionEnvelope, WriteXdr,
};

use crate::error::{ErrorCode, StellarAgentError};

/// Options for [`Signer::sign_transaction`].
#[derive(Debug, Clone)]
pub struct SignTransactionOptions {
    /// Network passphrase the signature must be bound to.
    ///
    /// Domain separation: the same transaction bytes signed for testnet must
    /// not be replayable on mainnet, and this is what prevents it.
    pub network_passphrase: String,
}

/// Options for [`Signer::sign_auth_entry`].
#[derive(Debug, Clone)]
pub struct SignAuthEntryOptions {
    /// Network passphrase the signature must be bound to.
    pub network_passphrase: String,
    /// Ledger sequence after which the authorisation is no longer valid.
    pub valid_until_ledger_seq: u32,
}

/// A signature could not be produced.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SigningError {
    message: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl SigningError {
    /// Build a signing failure with a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Attach the underlying cause.
    #[must_use]
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
}

impl From<SigningError> for StellarAgentError {
    /// A signing failure is not a network failure, and must not be retried as
    /// one — a refused signature usually means policy said no.
    fn from(error: SigningError) -> Self {
        StellarAgentError::new(ErrorCode::NotAuthorized, error.to_string()).with_source(error)
    }
}

/// Somewhere that can sign on behalf of one Stellar account.
///
/// Implementations must never require the caller to hold key material.
#[async_trait]
pub trait Signer: Send + Sync {
    /// The Stellar public address (`G…`) this signer signs for.
    ///
    /// Must be obtainable **without** the secret being present in the calling
    /// process — a remote signer derives it on the far side of the boundary
    /// and returns just the address.
    async fn public_key(&self) -> Result<String, SigningError>;

    /// Sign a base64 transaction envelope, returning the signed envelope.
    async fn sign_transaction(
        &self,
        xdr: &str,
        options: &SignTransactionOptions,
    ) -> Result<String, SigningError>;

    /// Sign a base64 `SorobanAuthorizationEntry`, returning the signed entry.
    ///
    /// An entry whose credentials are `SourceAccount` needs no signature of
    /// its own — it is covered by the envelope signature — and implementations
    /// should return it unchanged rather than failing.
    async fn sign_auth_entry(
        &self,
        auth_entry_xdr: &str,
        options: &SignAuthEntryOptions,
    ) -> Result<String, SigningError>;
}

// ─── Keypair ─────────────────────────────────────────────────────────────────

/// An ed25519 keypair, held in this process.
///
/// `Debug` is implemented by hand to print only the public address: a derived
/// one would put the secret into every log line that formats a struct
/// containing it.
pub struct Keypair {
    signing_key: SigningKey,
}

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keypair")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

impl Keypair {
    /// Generate a fresh keypair from the OS random source.
    pub fn random() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut rand_core::OsRng),
        }
    }

    /// Restore from a Stellar secret seed (`S…`).
    ///
    /// ```
    /// use stellaragent::signer::Keypair;
    ///
    /// let keypair = Keypair::from_secret(
    ///     "SAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYPSBF5K"
    /// )?;
    /// assert!(keypair.public_key().starts_with('G'));
    /// # Ok::<(), stellaragent::signer::SigningError>(())
    /// ```
    pub fn from_secret(secret: &str) -> Result<Self, SigningError> {
        let parsed = stellar_strkey::ed25519::PrivateKey::from_string(secret)
            .map_err(|_| SigningError::new("Keypair: not a valid Stellar secret key (S…)"))?;
        Ok(Self {
            signing_key: SigningKey::from_bytes(&parsed.0),
        })
    }

    /// The Stellar public address (`G…`).
    pub fn public_key(&self) -> String {
        stellar_strkey::ed25519::PublicKey(self.signing_key.verifying_key().to_bytes()).to_string()
    }

    /// The raw 32-byte ed25519 public key.
    pub fn raw_public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// The Stellar secret seed (`S…`).
    ///
    /// Deliberately a method rather than a field, and deliberately absent from
    /// [`Keypair`]'s `Debug` output: reaching for the secret should be a
    /// visible act in the code that does it. Prefer handing a [`Signer`] to
    /// [`crate::StellarAgent`] over extracting this.
    pub fn secret_key(&self) -> String {
        stellar_strkey::ed25519::PrivateKey(self.signing_key.to_bytes()).to_string()
    }

    /// Sign a payload, returning the raw 64-byte signature.
    pub fn sign_payload(&self, payload: &[u8]) -> [u8; 64] {
        self.signing_key.sign(payload).to_bytes()
    }

    /// The four-byte signature hint Stellar uses to match a signature to a key.
    fn signature_hint(&self) -> SignatureHint {
        let public = self.raw_public_key();
        SignatureHint([public[28], public[29], public[30], public[31]])
    }
}

// ─── Keypair-backed signer ───────────────────────────────────────────────────

/// A [`Signer`] holding an in-memory [`Keypair`].
///
/// Fine for testnet, for development, and for agents holding negligible value.
/// Explicitly *not* what you want for an agent with real funds — see
/// [`RemoteSigner`].
#[derive(Debug)]
pub struct KeypairSigner {
    keypair: Keypair,
}

impl KeypairSigner {
    /// Wrap an existing keypair.
    pub fn new(keypair: Keypair) -> Self {
        Self { keypair }
    }

    /// Generate a fresh keypair and wrap it.
    pub fn random() -> Self {
        Self::new(Keypair::random())
    }

    /// Restore from a Stellar secret seed (`S…`).
    pub fn from_secret(secret: &str) -> Result<Self, SigningError> {
        Ok(Self::new(Keypair::from_secret(secret)?))
    }

    /// The Stellar public address, without the `async` hop [`Signer`] requires.
    pub fn address(&self) -> String {
        self.keypair.public_key()
    }

    /// The wrapped keypair — the only route to the secret.
    pub fn keypair(&self) -> &Keypair {
        &self.keypair
    }
}

#[async_trait]
impl Signer for KeypairSigner {
    async fn public_key(&self) -> Result<String, SigningError> {
        Ok(self.keypair.public_key())
    }

    async fn sign_transaction(
        &self,
        xdr: &str,
        options: &SignTransactionOptions,
    ) -> Result<String, SigningError> {
        let mut envelope =
            TransactionEnvelope::from_xdr_base64(xdr, Limits::none()).map_err(|error| {
                SigningError::new("KeypairSigner: transaction envelope is not valid XDR")
                    .with_source(error)
            })?;

        let hash = envelope
            .hash(network_id(&options.network_passphrase))
            .map_err(|error| {
                SigningError::new("KeypairSigner: could not compute the transaction hash")
                    .with_source(error)
            })?;

        let signature = DecoratedSignature {
            hint: self.keypair.signature_hint(),
            signature: Signature(
                self.keypair
                    .sign_payload(&hash)
                    .to_vec()
                    .try_into()
                    .map_err(|_| SigningError::new("KeypairSigner: signature was not 64 bytes"))?,
            ),
        };

        match &mut envelope {
            TransactionEnvelope::Tx(inner) => {
                let mut signatures = inner.signatures.to_vec();
                signatures.push(signature);
                inner.signatures = signatures.try_into().map_err(|_| {
                    SigningError::new("KeypairSigner: transaction already carries 20 signatures")
                })?;
            }
            // v0 and fee-bump envelopes are not something this SDK builds; a
            // caller that hands one over has almost certainly passed the wrong
            // XDR, and silently returning it unsigned would surface much later
            // as an opaque "tx_bad_auth".
            _ => {
                return Err(SigningError::new(
                    "KeypairSigner: only v1 transaction envelopes are supported",
                ))
            }
        }

        envelope.to_xdr_base64(Limits::none()).map_err(|error| {
            SigningError::new("KeypairSigner: could not re-encode the signed envelope")
                .with_source(error)
        })
    }

    async fn sign_auth_entry(
        &self,
        auth_entry_xdr: &str,
        options: &SignAuthEntryOptions,
    ) -> Result<String, SigningError> {
        let mut entry = SorobanAuthorizationEntry::from_xdr_base64(auth_entry_xdr, Limits::none())
            .map_err(|error| {
                SigningError::new("KeypairSigner: authorization entry is not valid XDR")
                    .with_source(error)
            })?;

        // A `SourceAccount` entry is covered by the envelope signature. It is
        // not an error to be asked to sign one; there is simply nothing to do.
        let root_invocation = entry.root_invocation.clone();
        let SorobanCredentials::Address(credentials) = &mut entry.credentials else {
            return Ok(auth_entry_xdr.to_string());
        };

        credentials.signature_expiration_ledger = options.valid_until_ledger_seq;

        // The signed payload is a hash over the network, the nonce, the
        // expiry, and the invocation tree — so a signature authorises one
        // specific call, once, on one network, until one ledger.
        let preimage = HashIdPreimage::SorobanAuthorization(HashIdPreimageSorobanAuthorization {
            network_id: Hash(network_id(&options.network_passphrase)),
            nonce: credentials.nonce,
            signature_expiration_ledger: credentials.signature_expiration_ledger,
            invocation: root_invocation,
        });
        let encoded = preimage.to_xdr(Limits::none()).map_err(|error| {
            SigningError::new("KeypairSigner: could not encode the authorization preimage")
                .with_source(error)
        })?;
        let payload: [u8; 32] = Sha256::digest(encoded).into();
        let signature = self.keypair.sign_payload(&payload);

        credentials.signature =
            account_signature_scval(&self.keypair.raw_public_key(), &signature)?;

        entry.to_xdr_base64(Limits::none()).map_err(|error| {
            SigningError::new("KeypairSigner: could not re-encode the signed authorization entry")
                .with_source(error)
        })
    }
}

/// The `signature` value a classic (`G…`) account's authorisation expects: a
/// vector of `{ public_key, signature }` maps.
///
/// Not an obvious shape — a bare 64-byte `Bytes` is the intuitive guess and is
/// rejected by the host with an unhelpful error, because a Stellar account can
/// have several signers and the contract has to know which one signed.
fn account_signature_scval(
    public_key: &[u8; 32],
    signature: &[u8; 64],
) -> Result<ScVal, SigningError> {
    let bytes = |raw: &[u8]| -> Result<ScVal, SigningError> {
        Ok(ScVal::Bytes(ScBytes(raw.to_vec().try_into().map_err(
            |_| SigningError::new("KeypairSigner: signature payload was too long"),
        )?)))
    };
    let symbol = |name: &str| -> Result<ScVal, SigningError> {
        Ok(ScVal::Symbol(ScSymbol(name.try_into().map_err(|_| {
            SigningError::new("KeypairSigner: signature field name was too long")
        })?)))
    };

    // Map keys must be sorted; "public_key" sorts before "signature".
    let entries = vec![
        ScMapEntry {
            key: symbol("public_key")?,
            val: bytes(public_key)?,
        },
        ScMapEntry {
            key: symbol("signature")?,
            val: bytes(signature)?,
        },
    ];
    let map = ScVal::Map(Some(ScMap(entries.try_into().map_err(|_| {
        SigningError::new("KeypairSigner: could not build the signature map")
    })?)));

    Ok(ScVal::Vec(Some(ScVec(vec![map].try_into().map_err(
        |_| SigningError::new("KeypairSigner: could not build the signature vector"),
    )?))))
}

/// The network ID every Stellar signature is domain-separated by.
fn network_id(passphrase: &str) -> [u8; 32] {
    Sha256::digest(passphrase.as_bytes()).into()
}

// ─── Remote signer ───────────────────────────────────────────────────────────

/// Configuration for [`RemoteSigner`].
#[derive(Debug, Clone)]
pub struct RemoteSignerOptions {
    /// Base URL of the signing service, e.g. `https://signer.internal:8443`.
    pub url: String,
    /// Bearer token presented on every request.
    ///
    /// This is the *only* credential the agent process holds — losing it costs
    /// a token rotation, not a key rotation and a migration of every funded
    /// account.
    pub token: Option<String>,
    /// Address this signer is expected to sign for.
    ///
    /// When set, it is checked against what the service reports and a mismatch
    /// is refused, so a misconfigured or substituted service cannot quietly
    /// sign as a different account.
    pub expected_public_key: Option<String>,
    /// Per-request timeout. Defaults to 10 seconds.
    pub timeout: Duration,
    /// Extra headers — mTLS proxies, tracing, tenant routing.
    pub headers: Vec<(String, String)>,
}

impl RemoteSignerOptions {
    /// Options pointing at `url`, with defaults for everything else.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            expected_public_key: None,
            timeout: Duration::from_secs(10),
            headers: Vec::new(),
        }
    }

    /// Set the bearer token.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Pin the address the service must sign for.
    #[must_use]
    pub fn expect_public_key(mut self, address: impl Into<String>) -> Self {
        self.expected_public_key = Some(address.into());
        self
    }

    /// Override the per-request timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Add one extra header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A [`Signer`] backed by an HTTP signing service.
///
/// # Protocol
///
/// Three JSON endpoints. The key never crosses the boundary.
///
/// ```text
/// GET  {url}/v1/public-key
///   → 200 { "publicKey": "G..." }
///
/// POST {url}/v1/sign/transaction
///   ← { "xdr": "<base64 envelope>", "networkPassphrase": "..." }
///   → 200 { "signedXdr": "<base64 signed envelope>" }
///
/// POST {url}/v1/sign/auth-entry
///   ← { "authEntryXdr": "<base64>", "networkPassphrase": "...",
///       "validUntilLedgerSeq": 12345 }
///   → 200 { "signedAuthEntryXdr": "<base64>" }
/// ```
///
/// Errors return a non-2xx status with `{ "error": "<message>" }`. A service
/// refusing on policy grounds — spend ceiling, rate limit, revoked token —
/// should use `403` with a description, which surfaces here as a
/// [`SigningError`] carrying that text.
///
/// # Why signed XDR rather than a raw signature
///
/// Returning `signedXdr` means the service parses what it is signing and can
/// therefore apply policy to it: reject payments over a ceiling, enforce a
/// destination allow-list, log the operation. A service that only signed an
/// opaque hash could do none of that, which would waste the main advantage of
/// moving the key behind a boundary in the first place.
#[derive(Debug)]
pub struct RemoteSigner {
    options: RemoteSignerOptions,
    client: reqwest::Client,
    /// Cached: the address cannot change for a given signing identity.
    cached_public_key: tokio::sync::OnceCell<String>,
}

#[derive(Deserialize)]
struct PublicKeyResponse {
    #[serde(rename = "publicKey")]
    public_key: String,
}

#[derive(Deserialize)]
struct SignTransactionResponse {
    #[serde(rename = "signedXdr")]
    signed_xdr: String,
}

#[derive(Deserialize)]
struct SignAuthEntryResponse {
    #[serde(rename = "signedAuthEntryXdr")]
    signed_auth_entry_xdr: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: Option<String>,
}

impl RemoteSigner {
    /// Build a remote signer.
    ///
    /// # Errors
    ///
    /// Rejects an empty URL and an `expected_public_key` that is not a valid
    /// Stellar address — both at construction, so a typo fails at start-up
    /// rather than on the first payment.
    pub fn new(options: RemoteSignerOptions) -> Result<Self, SigningError> {
        if options.url.trim().is_empty() {
            return Err(SigningError::new("RemoteSigner requires a url"));
        }
        if let Some(expected) = &options.expected_public_key {
            if stellar_strkey::ed25519::PublicKey::from_string(expected).is_err() {
                return Err(SigningError::new(format!(
                    "RemoteSigner: expectedPublicKey is not a valid Stellar address: {expected}"
                )));
            }
        }

        let client = reqwest::Client::builder()
            .timeout(options.timeout)
            .build()
            .map_err(|error| {
                SigningError::new("RemoteSigner: could not build an HTTP client").with_source(error)
            })?;

        let mut options = options;
        options.url = options.url.trim_end_matches('/').to_string();

        Ok(Self {
            options,
            client,
            cached_public_key: tokio::sync::OnceCell::new(),
        })
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, SigningError> {
        let url = format!("{}{path}", self.options.url);
        let mut request = self.client.request(method, &url);

        if let Some(token) = &self.options.token {
            request = request.bearer_auth(token);
        }
        for (name, value) in &self.options.headers {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request.send().await.map_err(|error| {
            SigningError::new(format!("RemoteSigner: request to {url} failed")).with_source(error)
        })?;

        let status = response.status();
        let text = response.text().await.map_err(|error| {
            SigningError::new(format!(
                "RemoteSigner: could not read the response from {url}"
            ))
            .with_source(error)
        })?;

        if !status.is_success() {
            // A policy refusal is the interesting case, and the service puts
            // its reason in `error`. Fall back to the raw body so an
            // unexpected 502 from a proxy is still legible.
            let detail = serde_json::from_str::<ErrorResponse>(&text)
                .ok()
                .and_then(|body| body.error)
                .unwrap_or_else(|| text.clone());
            return Err(SigningError::new(format!(
                "RemoteSigner: {url} responded {status}: {detail}"
            )));
        }

        serde_json::from_str(&text).map_err(|error| {
            SigningError::new(format!("RemoteSigner: unexpected response body from {url}"))
                .with_source(error)
        })
    }
}

#[async_trait]
impl Signer for RemoteSigner {
    async fn public_key(&self) -> Result<String, SigningError> {
        self.cached_public_key
            .get_or_try_init(|| async {
                let response: PublicKeyResponse = self
                    .request(reqwest::Method::GET, "/v1/public-key", None)
                    .await?;

                if stellar_strkey::ed25519::PublicKey::from_string(&response.public_key).is_err() {
                    return Err(SigningError::new(format!(
                        "RemoteSigner: service returned an invalid public key: {}",
                        response.public_key
                    )));
                }
                if let Some(expected) = &self.options.expected_public_key {
                    if &response.public_key != expected {
                        return Err(SigningError::new(format!(
                            "RemoteSigner: service signs for {}, but {expected} was expected. \
                             Refusing to continue — this signer may be misconfigured or substituted.",
                            response.public_key
                        )));
                    }
                }
                Ok(response.public_key)
            })
            .await
            .cloned()
    }

    async fn sign_transaction(
        &self,
        xdr: &str,
        options: &SignTransactionOptions,
    ) -> Result<String, SigningError> {
        let response: SignTransactionResponse = self
            .request(
                reqwest::Method::POST,
                "/v1/sign/transaction",
                Some(serde_json::json!({
                    "xdr": xdr,
                    "networkPassphrase": options.network_passphrase,
                })),
            )
            .await?;

        if response.signed_xdr.is_empty() {
            return Err(SigningError::new(
                "RemoteSigner: service returned no signedXdr",
            ));
        }
        Ok(response.signed_xdr)
    }

    async fn sign_auth_entry(
        &self,
        auth_entry_xdr: &str,
        options: &SignAuthEntryOptions,
    ) -> Result<String, SigningError> {
        let response: SignAuthEntryResponse = self
            .request(
                reqwest::Method::POST,
                "/v1/sign/auth-entry",
                Some(serde_json::json!({
                    "authEntryXdr": auth_entry_xdr,
                    "networkPassphrase": options.network_passphrase,
                    "validUntilLedgerSeq": options.valid_until_ledger_seq,
                })),
            )
            .await?;

        if response.signed_auth_entry_xdr.is_empty() {
            return Err(SigningError::new(
                "RemoteSigner: service returned no signedAuthEntryXdr",
            ));
        }
        Ok(response.signed_auth_entry_xdr)
    }
}

/// Base64-decode a value the way every XDR field in this SDK is encoded.
///
/// Exposed because callers implementing their own [`Signer`] over a transport
/// that hands back raw signature bytes need it, and reaching for a different
/// base64 alphabet is a silent way to produce signatures nothing accepts.
pub fn decode_base64(value: &str) -> Result<Vec<u8>, SigningError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| SigningError::new("not valid standard base64").with_source(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A test-only seed. Not funded, not secret, and used only to exercise the
    // encoding paths.
    const SECRET: &str = "SAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYPSBF5K";

    #[test]
    fn a_keypair_round_trips_through_its_own_strkeys() {
        let keypair = Keypair::from_secret(SECRET).unwrap();
        assert_eq!(keypair.secret_key(), SECRET);
        assert!(keypair.public_key().starts_with('G'));

        let restored = Keypair::from_secret(&keypair.secret_key()).unwrap();
        assert_eq!(restored.public_key(), keypair.public_key());
    }

    #[test]
    fn a_random_keypair_is_a_different_one_every_time() {
        assert_ne!(
            Keypair::random().public_key(),
            Keypair::random().public_key()
        );
    }

    #[test]
    fn an_invalid_secret_is_rejected() {
        assert!(Keypair::from_secret("SNOPE").is_err());
        assert!(Keypair::from_secret("").is_err());
        // A public key is not a secret key, however valid it is on its own.
        let public = Keypair::random().public_key();
        assert!(Keypair::from_secret(&public).is_err());
    }

    #[test]
    fn debug_output_never_contains_the_secret() {
        let keypair = Keypair::from_secret(SECRET).unwrap();
        let rendered = format!("{keypair:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains(&keypair.public_key()));
    }

    #[test]
    fn the_signature_hint_is_the_last_four_bytes_of_the_public_key() {
        let keypair = Keypair::from_secret(SECRET).unwrap();
        let public = keypair.raw_public_key();
        assert_eq!(keypair.signature_hint().0, public[28..32]);
    }

    #[test]
    fn the_network_id_is_the_sha256_of_the_passphrase() {
        // The published testnet network ID, as a fixed check that the
        // domain-separation input is the passphrase bytes and nothing else.
        let id = network_id("Test SDF Network ; September 2015");
        assert_eq!(
            hex::encode(id),
            "cee0302d59844d32bdca915c8203dd44b33fbb7edc19051ea37abedf28ecd472"
        );
    }

    #[test]
    fn the_account_signature_scval_is_a_vector_of_sorted_maps() {
        let value = account_signature_scval(&[1u8; 32], &[2u8; 64]).unwrap();
        let ScVal::Vec(Some(ScVec(items))) = &value else {
            panic!("expected a vector, got {}", value.name());
        };
        assert_eq!(items.len(), 1);
        let ScVal::Map(Some(ScMap(entries))) = &items[0] else {
            panic!("expected a map inside the vector");
        };
        let keys: Vec<String> = entries
            .iter()
            .map(|entry| crate::scval::as_utf8(&entry.key).unwrap())
            .collect();
        assert_eq!(keys, ["public_key", "signature"]);
    }

    #[test]
    fn remote_signer_rejects_bad_configuration_at_construction() {
        assert!(RemoteSigner::new(RemoteSignerOptions::new("")).is_err());
        assert!(RemoteSigner::new(
            RemoteSignerOptions::new("https://signer.internal").expect_public_key("not-an-address")
        )
        .is_err());
        assert!(RemoteSigner::new(RemoteSignerOptions::new("https://signer.internal/")).is_ok());
    }

    #[test]
    fn remote_signer_trims_the_trailing_slash_so_paths_do_not_double_up() {
        let signer =
            RemoteSigner::new(RemoteSignerOptions::new("https://signer.internal//")).unwrap();
        assert_eq!(signer.options.url, "https://signer.internal");
    }

    #[tokio::test]
    async fn signing_a_source_account_auth_entry_is_a_no_op_rather_than_an_error() {
        let entry = SorobanAuthorizationEntry {
            credentials: SorobanCredentials::SourceAccount,
            root_invocation: Default::default(),
        };
        let encoded = entry.to_xdr_base64(Limits::none()).unwrap();
        let signer = KeypairSigner::from_secret(SECRET).unwrap();

        let signed = signer
            .sign_auth_entry(
                &encoded,
                &SignAuthEntryOptions {
                    network_passphrase: "Test SDF Network ; September 2015".into(),
                    valid_until_ledger_seq: 100,
                },
            )
            .await
            .unwrap();

        assert_eq!(signed, encoded);
    }

    #[tokio::test]
    async fn signing_malformed_xdr_is_an_error_not_a_panic() {
        let signer = KeypairSigner::from_secret(SECRET).unwrap();
        let options = SignTransactionOptions {
            network_passphrase: "Test SDF Network ; September 2015".into(),
        };
        assert!(signer.sign_transaction("not xdr", &options).await.is_err());
    }

    #[tokio::test]
    async fn the_public_key_is_reachable_through_the_trait() {
        let signer = KeypairSigner::from_secret(SECRET).unwrap();
        assert_eq!(signer.public_key().await.unwrap(), signer.address());
    }

    #[test]
    fn base64_decoding_rejects_a_different_alphabet() {
        assert!(decode_base64("AAAA").is_ok());
        assert!(decode_base64("not base64!").is_err());
    }
}
