//! Extracting a raw Ed25519 public key from what a KMS actually returns.
//!
//! Neither cloud KMS hands back 32 bytes. AWS `GetPublicKey` returns DER-encoded
//! `SubjectPublicKeyInfo`; GCP `GetPublicKey` returns the same structure
//! PEM-armoured. Both wrap the key we want in a fixed 12-byte prefix:
//!
//! ```text
//! SEQUENCE (0x30) len 0x2a
//!   SEQUENCE (0x30) len 0x05
//!     OID (0x06) len 0x03 : 2b 65 70        -- 1.3.101.112, id-Ed25519
//!   BIT STRING (0x03) len 0x21 : 00 <32 bytes>
//! ```
//!
//! # Why parse it strictly rather than take the last 32 bytes
//!
//! Slicing the tail would "work" and would also silently accept an ECDSA
//! P-256 key, whose SPKI is a different length and different OID — producing a
//! 32-byte value that is not a public key at all. Every signature made under it
//! would verify against nothing, and the failure would surface on-chain as a
//! rejected transaction with no clue as to why.
//!
//! So the OID is checked. A key of the wrong type is the single most likely
//! misconfiguration when standing this service up, and it should fail at
//! startup with a message naming the problem.

/// The full DER prefix of an Ed25519 `SubjectPublicKeyInfo`.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, // SEQUENCE, 42 bytes
    0x30, 0x05, // SEQUENCE, 5 bytes
    0x06, 0x03, 0x2b, 0x65, 0x70, // OID 1.3.101.112 (id-Ed25519)
    0x03, 0x21, 0x00, // BIT STRING, 33 bytes, 0 unused bits
];

/// Total length of an Ed25519 SPKI: the 12-byte prefix plus the key.
const ED25519_SPKI_LEN: usize = ED25519_SPKI_PREFIX.len() + 32;

/// Why an SPKI blob was not usable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpkiError {
    /// Not the length an Ed25519 SPKI has.
    #[error("expected a {ED25519_SPKI_LEN}-byte Ed25519 SubjectPublicKeyInfo, got {0} bytes")]
    WrongLength(usize),

    /// The right length, but not an Ed25519 key.
    #[error(
        "this is not an Ed25519 key — the algorithm identifier does not match id-Ed25519 \
         (1.3.101.112). A KMS key created with an ECDSA or RSA key spec cannot produce \
         Stellar-compatible signatures."
    )]
    NotEd25519,

    /// The PEM armour was malformed.
    #[error("could not decode the PEM body: {0}")]
    Pem(String),
}

/// Pull the 32-byte key out of a DER `SubjectPublicKeyInfo`.
pub fn ed25519_from_der(der: &[u8]) -> Result<[u8; 32], SpkiError> {
    if der.len() != ED25519_SPKI_LEN {
        return Err(SpkiError::WrongLength(der.len()));
    }
    if der[..ED25519_SPKI_PREFIX.len()] != ED25519_SPKI_PREFIX {
        return Err(SpkiError::NotEd25519);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&der[ED25519_SPKI_PREFIX.len()..]);
    Ok(key)
}

/// Pull the 32-byte key out of a PEM-armoured `SubjectPublicKeyInfo`.
///
/// Tolerant about line endings and trailing whitespace, because what arrives
/// from a cloud API is not always what the RFC describes.
pub fn ed25519_from_pem(pem: &str) -> Result<[u8; 32], SpkiError> {
    use base64::Engine;

    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();

    if body.is_empty() {
        return Err(SpkiError::Pem(
            "no base64 body between the PEM markers".into(),
        ));
    }

    let der = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|error| SpkiError::Pem(error.to_string()))?;

    ed25519_from_der(&der)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn der_for(key: [u8; 32]) -> Vec<u8> {
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(&key);
        der
    }

    #[test]
    fn a_well_formed_ed25519_spki_yields_its_key() {
        let key = [0xABu8; 32];
        assert_eq!(ed25519_from_der(&der_for(key)).unwrap(), key);
    }

    #[test]
    fn a_p256_key_is_rejected_rather_than_truncated() {
        // The failure this module exists to prevent: an ECDSA key sliced to 32
        // bytes looks like a public key and is not one, and every signature
        // under it would fail on-chain with no clue why.
        //
        // A P-256 SPKI is 91 bytes and starts with a different OID.
        let p256_spki = vec![0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce];
        assert!(matches!(
            ed25519_from_der(&p256_spki),
            Err(SpkiError::WrongLength(_))
        ));

        // Same length as Ed25519, wrong OID — the case a length check alone
        // would let through.
        let mut wrong_oid = der_for([0u8; 32]);
        wrong_oid[6] = 0x2a; // corrupt the OID body
        assert_eq!(ed25519_from_der(&wrong_oid), Err(SpkiError::NotEd25519));
    }

    #[test]
    fn the_error_explains_what_to_do_about_it() {
        // An operator hitting this at startup should not have to read the
        // source to work out that their KMS key spec is wrong.
        let mut wrong_oid = der_for([0u8; 32]);
        wrong_oid[6] = 0x2a;
        let message = ed25519_from_der(&wrong_oid).unwrap_err().to_string();
        assert!(message.contains("Ed25519"), "{message}");
        assert!(message.contains("key spec"), "{message}");
    }

    #[test]
    fn pem_armour_is_stripped_before_decoding() {
        let key = [0x11u8; 32];
        let body = base64::engine::general_purpose::STANDARD.encode(der_for(key));
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n");
        assert_eq!(ed25519_from_pem(&pem).unwrap(), key);
    }

    #[test]
    fn pem_with_wrapped_lines_and_crlf_still_decodes() {
        // What a cloud API returns is not always what the RFC describes.
        let key = [0x22u8; 32];
        let body = base64::engine::general_purpose::STANDARD.encode(der_for(key));
        let wrapped: Vec<&str> = body
            .as_bytes()
            .chunks(16)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\r\n{}\r\n-----END PUBLIC KEY-----\r\n",
            wrapped.join("\r\n")
        );
        assert_eq!(ed25519_from_pem(&pem).unwrap(), key);
    }

    #[test]
    fn malformed_pem_is_an_error_not_a_panic() {
        assert!(matches!(ed25519_from_pem(""), Err(SpkiError::Pem(_))));
        assert!(matches!(
            ed25519_from_pem("-----BEGIN PUBLIC KEY-----\n-----END PUBLIC KEY-----"),
            Err(SpkiError::Pem(_))
        ));
        assert!(matches!(
            ed25519_from_pem("-----BEGIN PUBLIC KEY-----\nnot base64!!\n-----END PUBLIC KEY-----"),
            Err(SpkiError::Pem(_))
        ));
    }
}
