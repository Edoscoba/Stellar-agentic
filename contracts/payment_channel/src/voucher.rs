//! Signed off-chain payment vouchers.
//!
//! The protocol is specified in `docs/payment-voucher-design.md`; this module
//! is the on-chain half. What follows repeats only the parts a reader of this
//! file needs in order to check it.
//!
//! # The claim
//!
//! A voucher says: *as of `sequence`, channel `channel_id` owes `recipient` a
//! **cumulative** total of `cumulative_amount`.* Cumulative rather than
//! incremental, so the recipient keeps exactly one voucher, settlement is
//! idempotent, and a dropped voucher costs nothing because the next supersedes
//! it.
//!
//! # Why settlement orders by amount, not by sequence
//!
//! Only the payer holds the voucher-signing key, and a payer's incentive is
//! always to understate what they owe. If the highest `sequence` won, the payer
//! could sign `(sequence = u64::MAX, cumulative_amount = 0)` and use it during
//! the dispute window to erase the recipient's entire claim.
//!
//! So [`supersedes`] compares `cumulative_amount`. A voucher can only ever win
//! by paying the recipient *more*, which makes minting a small one useless to
//! an attacker. `sequence` is still carried, still required to increase, and
//! still recorded — it is an audit aid, not the security-relevant comparison.
//!
//! # The signing domain
//!
//! 151 fixed-width bytes, no delimiters and no length prefixes, so the encoding
//! is trivially injective — every field starts at a known offset, so no two
//! distinct vouchers can produce the same preimage.
//!
//! ```text
//!   offset  len  field
//!        0   23  b"STELLARAGENT-VOUCHER-V1"
//!       23   32  network id            env.ledger().network_id()
//!       55   32  contract domain       sha256(xdr(this contract's Address))
//!       87   32  recipient domain      sha256(xdr(recipient Address))
//!      119    8  channel id            u64, big-endian
//!      127    8  sequence              u64, big-endian
//!      135   16  cumulative amount     i128, big-endian, two's complement
//!               151
//! ```
//!
//! Addresses enter as `sha256` of their XDR rather than as raw key bytes. A
//! contract cannot recover an Ed25519 key from an `Address` — `contract_id()`
//! is private to the SDK — and slicing the raw id out of the XDR at a fixed
//! offset would silently break if that encoding ever changed. Hashing the whole
//! serialised address is offset-independent, and it distinguishes an account
//! from a contract for free, because the two have different XDR discriminants.
//!
//! (The design doc's first draft specified raw key bytes plus a one-byte kind
//! tag. This is the same construction with the fragility removed; the doc has
//! been updated to match.)

use soroban_sdk::{crypto::Hash, xdr::ToXdr, Address, Bytes, BytesN, Env};

/// Domain-separation tag. Bumping this invalidates every voucher ever signed,
/// which is exactly what a breaking change to the format should do.
pub const VOUCHER_DOMAIN: &[u8; 23] = b"STELLARAGENT-VOUCHER-V1";

/// Length of the signing preimage. Asserted by a test rather than trusted.
pub const PREIMAGE_LEN: u32 = 151;

/// Build the bytes a voucher signature is made over.
///
/// Pure: it reads the ledger's network id and this contract's address, and
/// touches no channel state. That matters for reviewability — nothing about
/// *what is owed* can change the domain a signature is checked against.
pub fn preimage(
    env: &Env,
    channel_id: u64,
    recipient: &Address,
    sequence: u64,
    cumulative_amount: i128,
) -> Bytes {
    let mut bytes = Bytes::new(env);

    bytes.extend_from_array(VOUCHER_DOMAIN);
    bytes.append(&env.ledger().network_id().into());
    bytes.append(&address_domain(env, &env.current_contract_address()).into());
    bytes.append(&address_domain(env, recipient).into());
    bytes.extend_from_array(&channel_id.to_be_bytes());
    bytes.extend_from_array(&sequence.to_be_bytes());
    bytes.extend_from_array(&cumulative_amount.to_be_bytes());

    bytes
}

/// `sha256` of an address's XDR serialisation.
///
/// Offset-independent, and free disambiguation between an account address and a
/// contract address because their XDR discriminants differ.
fn address_domain(env: &Env, address: &Address) -> Hash<32> {
    env.crypto().sha256(&address.clone().to_xdr(env))
}

/// Verify a voucher signature, panicking if it does not check out.
///
/// Wraps the host's `ed25519_verify`, which panics on failure — deliberately
/// not converted to a `bool`. A caller that forgot to check a returned `false`
/// would sign off on an unverified voucher; a caller that forgets to call this
/// at all does not get a signature check, which is visible at the call site.
pub fn require_valid_signature(
    env: &Env,
    signer: &BytesN<32>,
    channel_id: u64,
    recipient: &Address,
    sequence: u64,
    cumulative_amount: i128,
    signature: &BytesN<64>,
) {
    let message = preimage(env, channel_id, recipient, sequence, cumulative_amount);
    env.crypto().ed25519_verify(signer, &message, signature);
}

/// Whether `candidate` replaces `best`.
///
/// Strictly greater, on the amount. Equality does not supersede: re-submitting
/// the current best is a no-op rather than a way to reset anything, and a
/// voucher that pays less can never win. See the module docs for why this is
/// the amount rather than the sequence.
pub fn supersedes(candidate_amount: i128, best_amount: i128) -> bool {
    candidate_amount > best_amount
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn env() -> Env {
        Env::default()
    }

    /// A registered contract address. `as_contract` needs one that actually
    /// exists — a generated address has no instance storage to read from.
    fn contract(env: &Env) -> Address {
        env.register(crate::PaymentChannel, ())
    }

    #[test]
    fn the_preimage_is_exactly_the_specified_length() {
        // The whole injectivity argument rests on fixed widths. If a field's
        // encoding ever changes size, this fails before anything subtle does.
        let env = env();
        let recipient = Address::generate(&env);
        let instance = contract(&env);
        let bytes = env.as_contract(&instance, || preimage(&env, 1, &recipient, 1, 1));
        assert_eq!(bytes.len(), PREIMAGE_LEN);
    }

    #[test]
    fn every_field_changes_the_preimage() {
        // Each of these is a replay attack the domain is supposed to close.
        let env = env();
        let instance = contract(&env);
        let recipient = Address::generate(&env);
        let other_recipient = Address::generate(&env);

        let base = env.as_contract(&instance, || preimage(&env, 1, &recipient, 1, 100));

        let different_channel =
            env.as_contract(&instance, || preimage(&env, 2, &recipient, 1, 100));
        let different_recipient =
            env.as_contract(&instance, || preimage(&env, 1, &other_recipient, 1, 100));
        let different_sequence =
            env.as_contract(&instance, || preimage(&env, 1, &recipient, 2, 100));
        let different_amount = env.as_contract(&instance, || preimage(&env, 1, &recipient, 1, 101));

        assert_ne!(base, different_channel, "channel id must be bound");
        assert_ne!(base, different_recipient, "recipient must be bound");
        assert_ne!(base, different_sequence, "sequence must be bound");
        assert_ne!(base, different_amount, "amount must be bound");
    }

    #[test]
    fn a_different_contract_instance_produces_a_different_preimage() {
        // Two deployments of this contract on the same network must not accept
        // each other's vouchers.
        let env = env();
        let recipient = Address::generate(&env);

        let first = contract(&env);
        let second = contract(&env);
        let a = env.as_contract(&first, || preimage(&env, 1, &recipient, 1, 100));
        let b = env.as_contract(&second, || preimage(&env, 1, &recipient, 1, 100));

        assert_ne!(a, b);
    }

    #[test]
    fn the_domain_tag_leads_the_preimage() {
        let env = env();
        let recipient = Address::generate(&env);
        let instance = contract(&env);
        let bytes = env.as_contract(&instance, || preimage(&env, 1, &recipient, 1, 1));

        let mut tag = [0u8; 23];
        bytes.slice(0..23).copy_into_slice(&mut tag);
        assert_eq!(&tag, VOUCHER_DOMAIN);
    }

    #[test]
    fn integers_are_big_endian_at_the_specified_offsets() {
        // Pins the wire format the TypeScript and Python SDKs have to match.
        let env = env();
        let recipient = Address::generate(&env);
        let instance = contract(&env);
        let bytes = env.as_contract(&instance, || {
            preimage(
                &env,
                0x0102_0304_0506_0708,
                &recipient,
                0x1112_1314_1516_1718,
                1,
            )
        });

        let mut channel = [0u8; 8];
        bytes.slice(119..127).copy_into_slice(&mut channel);
        assert_eq!(channel, [1, 2, 3, 4, 5, 6, 7, 8]);

        let mut sequence = [0u8; 8];
        bytes.slice(127..135).copy_into_slice(&mut sequence);
        assert_eq!(sequence, [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);

        let mut amount = [0u8; 16];
        bytes.slice(135..151).copy_into_slice(&mut amount);
        assert_eq!(amount, 1i128.to_be_bytes());
    }

    #[test]
    fn a_negative_amount_encodes_as_twos_complement() {
        // Not a valid voucher — the contract rejects negatives — but the
        // encoding must still be unambiguous, and the SDKs must match it.
        let env = env();
        let recipient = Address::generate(&env);
        let instance = contract(&env);
        let bytes = env.as_contract(&instance, || preimage(&env, 1, &recipient, 1, -1));

        let mut amount = [0u8; 16];
        bytes.slice(135..151).copy_into_slice(&mut amount);
        assert_eq!(amount, [0xff; 16]);
    }

    #[test]
    fn only_a_strictly_larger_amount_supersedes() {
        assert!(supersedes(101, 100));
        assert!(!supersedes(100, 100), "equal is a no-op, not a reset");
        assert!(!supersedes(99, 100), "a payer must not be able to pay less");
        // The attack the amount ordering exists to close: a voucher with an
        // enormous sequence but a tiny amount is still worthless.
        assert!(!supersedes(0, 1));
    }
}
