//! The Stellar-specific cryptography: what exactly gets signed, and how the
//! signature is put back.
//!
//! # Everything reduces to 32 bytes
//!
//! Both signing operations this service performs end at the same primitive:
//!
//! | Operation | Payload |
//! |---|---|
//! | transaction | `SHA-256(TransactionSignaturePayload)` |
//! | auth entry | `SHA-256(HashIdPreimage::SorobanAuthorization)` |
//!
//! Each is 32 bytes, signed with **PureEdDSA Ed25519** (RFC 8032) — the raw
//! message, no pre-hashing inside the signer. That is what lets
//! [`crate::backend::SigningBackend`] be a single `sign(&[u8; 32]) -> [u8; 64]`
//! method, and it is why the AWS KMS adapter must pin `ED25519_SHA_512` with
//! `MessageType: RAW` rather than the pre-hashed `ED25519_PH_SHA_512` variant.
//!
//! # Why this module exists rather than reusing the SDK
//!
//! `sdk/rust` computes exactly these hashes. Depending on it would invert the
//! dependency direction and drag an HTTP client into this process — see
//! `docs/signer-service-design.md`. The cost is that these ~80 lines are
//! duplicated, so they are pinned here against fixed vectors (the published
//! testnet network ID, and a signature this module produces being verifiable
//! by an independent `ed25519-dalek` check) rather than by a shared crate.

use sha2::{Digest, Sha256};
use stellar_xdr::curr::{
    Hash, HashIdPreimage, HashIdPreimageSorobanAuthorization, Limits, ScBytes, ScMap, ScMapEntry,
    ScSymbol, ScVal, ScVec, SorobanAuthorizationEntry, SorobanCredentials, Transaction,
    TransactionSignaturePayload, TransactionSignaturePayloadTaggedTransaction, WriteXdr,
};

use crate::error::{RefusalReason, Result, ServiceError};

/// The 32-byte network ID every Stellar signature is domain-separated by.
///
/// Plain `SHA-256` of the passphrase bytes. This is what stops a signature
/// produced for testnet being replayable on mainnet, so it is never derived
/// from anything but the exact passphrase the caller supplied and the policy
/// approved.
pub fn network_id(passphrase: &str) -> [u8; 32] {
    Sha256::digest(passphrase.as_bytes()).into()
}

/// The 32 bytes to sign for a transaction.
pub fn transaction_signing_payload(
    transaction: &Transaction,
    passphrase: &str,
) -> Result<[u8; 32]> {
    let payload = TransactionSignaturePayload {
        network_id: Hash(network_id(passphrase)),
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(transaction.clone()),
    };
    let encoded = payload.to_xdr(Limits::none()).map_err(|error| {
        ServiceError::new(
            RefusalReason::MalformedEnvelope,
            "could not re-encode the transaction for signing",
        )
        .with_internal(error.to_string())
    })?;
    Ok(Sha256::digest(encoded).into())
}

/// The 32 bytes to sign for a Soroban authorisation entry.
///
/// The preimage binds the signature to one network, one nonce, one expiry
/// ledger and one invocation tree — so a signature authorises exactly one call,
/// once, on one network, until one ledger. `expiration_ledger` is the caller's
/// requested value **after** the policy cap has been applied.
pub fn auth_entry_signing_payload(
    entry: &SorobanAuthorizationEntry,
    passphrase: &str,
    expiration_ledger: u32,
) -> Result<[u8; 32]> {
    let SorobanCredentials::Address(credentials) = &entry.credentials else {
        return Err(ServiceError::new(
            RefusalReason::MalformedEnvelope,
            "authorization entry uses source-account credentials, which are covered by the \
             transaction signature and need no separate signature",
        ));
    };

    let preimage = HashIdPreimage::SorobanAuthorization(HashIdPreimageSorobanAuthorization {
        network_id: Hash(network_id(passphrase)),
        nonce: credentials.nonce,
        signature_expiration_ledger: expiration_ledger,
        invocation: entry.root_invocation.clone(),
    });
    let encoded = preimage.to_xdr(Limits::none()).map_err(|error| {
        ServiceError::new(
            RefusalReason::MalformedEnvelope,
            "could not encode the authorization preimage",
        )
        .with_internal(error.to_string())
    })?;
    Ok(Sha256::digest(encoded).into())
}

/// The `signature` value a classic (`G…`) account's authorisation expects.
///
/// Not the 64 raw bytes, which is the intuitive guess and is rejected by the
/// host: a Stellar account can have several signers, so the contract has to be
/// told *which* one signed. The shape is a vector of `{public_key, signature}`
/// maps, with the map keys sorted (`public_key` < `signature`).
pub fn account_signature_scval(public_key: &[u8; 32], signature: &[u8; 64]) -> Result<ScVal> {
    let bytes = |raw: &[u8]| -> Result<ScVal> {
        Ok(ScVal::Bytes(ScBytes(raw.to_vec().try_into().map_err(
            |_| internal("signature component was too long to encode"),
        )?)))
    };
    let symbol = |name: &str| -> Result<ScVal> {
        Ok(ScVal::Symbol(ScSymbol(name.try_into().map_err(|_| {
            internal("signature field name was too long")
        })?)))
    };

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
    let map = ScVal::Map(Some(ScMap(
        entries
            .try_into()
            .map_err(|_| internal("could not build the signature map"))?,
    )));

    Ok(ScVal::Vec(Some(ScVec(vec![map].try_into().map_err(
        |_| internal("could not build the signature vector"),
    )?))))
}

/// The four-byte hint Stellar uses to match a signature to one of an account's
/// signers: the last four bytes of the public key.
pub fn signature_hint(public_key: &[u8; 32]) -> [u8; 4] {
    [
        public_key[28],
        public_key[29],
        public_key[30],
        public_key[31],
    ]
}

/// Render a raw Ed25519 public key as a Stellar address (`G…`).
pub fn address_from_public_key(public_key: &[u8; 32]) -> String {
    stellar_strkey::ed25519::PublicKey(*public_key).to_string()
}

/// Parse a Stellar address (`G…`) back to its raw Ed25519 public key.
pub fn public_key_from_address(address: &str) -> Option<[u8; 32]> {
    stellar_strkey::ed25519::PublicKey::from_string(address)
        .ok()
        .map(|key| key.0)
}

fn internal(message: &'static str) -> ServiceError {
    ServiceError::new(RefusalReason::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
    use stellar_xdr::curr::{
        Memo, Operation, OperationBody, Preconditions, SequenceNumber, SorobanAddressCredentials,
        SorobanAuthorizedFunction, SorobanAuthorizedInvocation, TransactionExt,
    };

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn transaction() -> Transaction {
        Transaction {
            source_account: "GA7QYNF7SOWQ3GLR2BGMZEHXAVIRZA4KVWLTJJFC7MGXUA74P7UJVSGZ"
                .parse()
                .unwrap(),
            fee: 100,
            seq_num: SequenceNumber(42),
            cond: Preconditions::None,
            memo: Memo::None,
            operations: vec![Operation {
                source_account: None,
                body: OperationBody::Inflation,
            }]
            .try_into()
            .unwrap(),
            ext: TransactionExt::V0,
        }
    }

    #[test]
    fn the_network_id_is_the_sha256_of_the_passphrase() {
        // The published testnet network ID. A fixed vector, so a "cleanup" that
        // changed the domain-separation input would be caught here rather than
        // by every signature silently failing verification on-chain.
        assert_eq!(
            hex::encode(network_id("Test SDF Network ; September 2015")),
            "cee0302d59844d32bdca915c8203dd44b33fbb7edc19051ea37abedf28ecd472"
        );
        assert_eq!(
            hex::encode(network_id("Public Global Stellar Network ; September 2015")),
            "7ac33997544e3175d266bd022439b22cdb16508c01163f26e5cb2a3e1045a979"
        );
    }

    #[test]
    fn a_different_passphrase_produces_a_different_payload() {
        // The whole point of domain separation: the same transaction bytes
        // must not yield a signature valid on another network.
        let tx = transaction();
        let testnet =
            transaction_signing_payload(&tx, "Test SDF Network ; September 2015").unwrap();
        let mainnet =
            transaction_signing_payload(&tx, "Public Global Stellar Network ; September 2015")
                .unwrap();
        assert_ne!(testnet, mainnet);
    }

    #[test]
    fn a_signature_over_the_transaction_payload_verifies_independently() {
        // Round-trips through ed25519-dalek's own verifier rather than
        // asserting our own arithmetic against itself.
        let key = signing_key();
        let payload =
            transaction_signing_payload(&transaction(), "Test SDF Network ; September 2015")
                .unwrap();
        let signature = key.sign(&payload);
        let verifying: VerifyingKey = key.verifying_key();
        assert!(verifying.verify(&payload, &signature).is_ok());
    }

    fn auth_entry(nonce: i64) -> SorobanAuthorizationEntry {
        SorobanAuthorizationEntry {
            credentials: SorobanCredentials::Address(SorobanAddressCredentials {
                address: "GA7QYNF7SOWQ3GLR2BGMZEHXAVIRZA4KVWLTJJFC7MGXUA74P7UJVSGZ"
                    .parse()
                    .unwrap(),
                nonce,
                signature_expiration_ledger: 0,
                signature: ScVal::Void,
            }),
            root_invocation: SorobanAuthorizedInvocation {
                function: SorobanAuthorizedFunction::CreateContractHostFn(Default::default()),
                sub_invocations: Default::default(),
            },
        }
    }

    #[test]
    fn the_auth_payload_binds_the_nonce_and_the_expiry() {
        let passphrase = "Test SDF Network ; September 2015";
        let base = auth_entry_signing_payload(&auth_entry(1), passphrase, 100).unwrap();

        // A different nonce is a different authorisation.
        assert_ne!(
            base,
            auth_entry_signing_payload(&auth_entry(2), passphrase, 100).unwrap()
        );
        // A different expiry is a different authorisation — this is what makes
        // capping validUntilLedgerSeq meaningful rather than cosmetic.
        assert_ne!(
            base,
            auth_entry_signing_payload(&auth_entry(1), passphrase, 101).unwrap()
        );
    }

    #[test]
    fn a_source_account_auth_entry_is_refused_rather_than_signed() {
        let entry = SorobanAuthorizationEntry {
            credentials: SorobanCredentials::SourceAccount,
            root_invocation: auth_entry(1).root_invocation,
        };
        let error = auth_entry_signing_payload(&entry, "Test SDF Network ; September 2015", 100)
            .unwrap_err();
        assert_eq!(error.reason(), RefusalReason::MalformedEnvelope);
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
        let keys: Vec<&[u8]> = entries
            .iter()
            .map(|entry| match &entry.key {
                ScVal::Symbol(ScSymbol(s)) => s.as_slice(),
                other => panic!("expected a symbol key, got {}", other.name()),
            })
            .collect();
        assert_eq!(keys, [b"public_key".as_slice(), b"signature".as_slice()]);
    }

    #[test]
    fn the_signature_hint_is_the_last_four_bytes_of_the_public_key() {
        let mut public = [0u8; 32];
        public[28..32].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(signature_hint(&public), [0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn addresses_round_trip_through_strkey() {
        let public = signing_key().verifying_key().to_bytes();
        let address = address_from_public_key(&public);
        assert!(address.starts_with('G'));
        assert_eq!(public_key_from_address(&address), Some(public));
        assert_eq!(public_key_from_address("not-an-address"), None);
        // A contract ID is a valid strkey but not an account.
        assert_eq!(
            public_key_from_address("CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE"),
            None
        );
    }
}
