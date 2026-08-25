//! Building and reading `ScVal`s for every type the SDK's contracts use.
//!
//! Soroban's host values are a tagged union ([`ScVal`]); Rust contract types
//! written with `soroban-sdk` map onto them by a set of conventions that are
//! not obvious from the XDR alone:
//!
//! | Contract type | `ScVal` encoding |
//! |---|---|
//! | `u32` / `i32` / `u64` / `i64` | the matching scalar variant |
//! | `i128` / `u128` | [`ScVal::I128`] / [`ScVal::U128`], split into `hi`/`lo` halves |
//! | `bool` | [`ScVal::Bool`] |
//! | `Address` | [`ScVal::Address`], an account (`G…`) or contract (`C…`) |
//! | `Bytes` | [`ScVal::Bytes`] |
//! | `String` | [`ScVal::String`] |
//! | `Symbol` | [`ScVal::Symbol`], at most 32 characters |
//! | `Vec<T>` | [`ScVal::Vec`] |
//! | `Map<K, V>` | [`ScVal::Map`], **keys in sorted order** |
//! | `Option<T>` | the value itself for `Some`, [`ScVal::Void`] for `None` |
//! | `#[contracttype] struct` | a map keyed by field-name symbols |
//! | `#[contracttype] enum` (unit variants) | a one-element vector holding the variant symbol |
//!
//! The last two are the ones that catch people out. A struct is *not* a
//! vector of fields in declaration order — it is a map, and the host requires
//! its keys to be sorted, so [`map`] sorts them for you rather than trusting
//! the caller's insertion order. A fieldless enum variant is *not* a bare
//! symbol — it is a vector containing one, which is why
//! `enum_variant("Hourly")` exists rather than `symbol("Hourly")`.
//!
//! Every builder here validates its input and returns
//! [`ErrorCode::InvalidArgument`] rather than panicking, so a malformed
//! address or an over-long symbol fails at the call that supplied it and not
//! three frames deeper inside the XDR encoder.

use num_bigint::BigInt;
use stellar_xdr::curr::{
    Int128Parts, Limits, ReadXdr, ScAddress, ScBytes, ScMap, ScMapEntry, ScString, ScSymbol, ScVal,
    ScVec, UInt128Parts, WriteXdr,
};

use crate::error::{ErrorCode, Result, StellarAgentError};
use crate::math;

fn invalid(message: impl Into<String>) -> StellarAgentError {
    StellarAgentError::new(ErrorCode::InvalidArgument, message)
}

fn contract_error(message: impl Into<String>) -> StellarAgentError {
    StellarAgentError::new(ErrorCode::ContractError, message)
}

// ─── Builders ────────────────────────────────────────────────────────────────

/// A Stellar address — an account (`G…`), contract (`C…`), or muxed account.
///
/// ```
/// use stellaragent::scval;
///
/// let value = scval::address("CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE")?;
/// assert_eq!(scval::as_address_string(&value)?, "CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE");
/// assert!(scval::address("not-an-address").is_err());
/// # Ok::<(), stellaragent::StellarAgentError>(())
/// ```
pub fn address(value: &str) -> Result<ScVal> {
    let parsed: ScAddress = value
        .parse()
        .map_err(|_| invalid(format!("Invalid Stellar address: {value}")))?;
    Ok(ScVal::Address(parsed))
}

/// An `i128`, the type every monetary amount uses on-chain.
pub fn i128_value(value: i128) -> ScVal {
    ScVal::I128(Int128Parts {
        hi: (value >> 64) as i64,
        lo: value as u64,
    })
}

/// A `u128`.
pub fn u128_value(value: u128) -> ScVal {
    ScVal::U128(UInt128Parts {
        hi: (value >> 64) as u64,
        lo: value as u64,
    })
}

/// A human-readable decimal amount, converted to `i128` stroops.
///
/// Truncates sub-stroop fractions through [`math::to_stroops`], so this
/// rounds the same direction as every other amount conversion in the SDK, and
/// then range-checks against `i128` — a value that will not fit is caught here,
/// naming the amount, rather than wrapping silently into a different payment.
///
/// ```
/// use stellaragent::scval;
///
/// assert_eq!(scval::amount("1.5000001")?, scval::i128_value(15_000_001));
/// assert!(scval::amount("1e40").is_err()); // beyond i128 once scaled
/// # Ok::<(), stellaragent::StellarAgentError>(())
/// ```
pub fn amount(value: &str) -> Result<ScVal> {
    let stroops = math::to_stroops(value)?;
    Ok(i128_value(bigint_to_i128(&stroops).ok_or_else(|| {
        invalid(format!(
            "Amount {value} is {stroops} stroops, which is outside the i128 range the contracts use"
        ))
    })?))
}

/// A `u64`.
pub fn u64_value(value: u64) -> ScVal {
    ScVal::U64(value)
}

/// An `i64`.
pub fn i64_value(value: i64) -> ScVal {
    ScVal::I64(value)
}

/// A `u32`.
pub fn u32_value(value: u32) -> ScVal {
    ScVal::U32(value)
}

/// An `i32`.
pub fn i32_value(value: i32) -> ScVal {
    ScVal::I32(value)
}

/// A `bool`.
pub fn bool_value(value: bool) -> ScVal {
    ScVal::Bool(value)
}

/// The unit value — also how a `None` is encoded.
pub fn void() -> ScVal {
    ScVal::Void
}

/// A `Symbol`.
///
/// # Errors
///
/// Symbols are capped at 32 characters on-chain. Exceeding that is rejected
/// here, where the offending symbol can be named.
pub fn symbol(value: &str) -> Result<ScVal> {
    Ok(ScVal::Symbol(sc_symbol(value)?))
}

/// A `String`.
pub fn string(value: &str) -> Result<ScVal> {
    Ok(ScVal::String(ScString(value.try_into().map_err(|_| {
        invalid(format!(
            "String of {} bytes is too long for an ScVal",
            value.len()
        ))
    })?)))
}

/// A `Bytes`, from raw bytes.
pub fn bytes(value: &[u8]) -> Result<ScVal> {
    Ok(ScVal::Bytes(ScBytes(value.to_vec().try_into().map_err(
        |_| {
            invalid(format!(
                "Byte string of {} bytes is too long for an ScVal",
                value.len()
            ))
        },
    )?)))
}

/// A `Bytes` holding the UTF-8 encoding of `value`.
///
/// The contracts store free text (an endpoint URL, a task description, a job
/// result) as `Bytes` rather than `String`, so this is the encoder those
/// arguments want. [`as_utf8`] is the inverse.
pub fn bytes_from_str(value: &str) -> Result<ScVal> {
    bytes(value.as_bytes())
}

/// A `Vec<T>`.
pub fn vec(values: Vec<ScVal>) -> Result<ScVal> {
    let len = values.len();
    Ok(ScVal::Vec(Some(ScVec(values.try_into().map_err(
        |_| invalid(format!("Vector of {len} elements is too long for an ScVal")),
    )?))))
}

/// A `Map<Symbol, T>`, with its keys sorted as the host requires.
///
/// Sorting here rather than trusting the caller is deliberate: an unsorted map
/// is rejected by the host with an error that says nothing about which key was
/// out of place, and insertion order is exactly the kind of thing that differs
/// between two SDKs building the same argument.
pub fn map(entries: Vec<(&str, ScVal)>) -> Result<ScVal> {
    let mut built = entries
        .into_iter()
        .map(|(key, val)| {
            Ok(ScMapEntry {
                key: ScVal::Symbol(sc_symbol(key)?),
                val,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    built.sort_by(|a, b| a.key.cmp(&b.key));

    let len = built.len();
    Ok(ScVal::Map(Some(ScMap(built.try_into().map_err(|_| {
        invalid(format!("Map of {len} entries is too long for an ScVal"))
    })?))))
}

/// A fieldless `#[contracttype]` enum variant.
///
/// Encoded as a one-element vector holding the variant symbol — *not* as a
/// bare symbol. Passing a bare symbol is the single most common way to get an
/// `UnexpectedType` back from a Soroban contract that expected an enum.
///
/// ```
/// use stellaragent::scval;
/// use stellaragent::types::SpendPeriod;
///
/// let period = scval::enum_variant(SpendPeriod::Hourly.contract_variant())?;
/// assert_eq!(scval::as_enum_variant(&period)?, "Hourly");
/// # Ok::<(), stellaragent::StellarAgentError>(())
/// ```
pub fn enum_variant(variant: &str) -> Result<ScVal> {
    vec(vec![symbol(variant)?])
}

/// An `Option<T>`: the value itself for `Some`, [`ScVal::Void`] for `None`.
pub fn option(value: Option<ScVal>) -> ScVal {
    value.unwrap_or(ScVal::Void)
}

// ─── Decoders ────────────────────────────────────────────────────────────────

/// Read an `i128`.
pub fn as_i128(value: &ScVal) -> Result<i128> {
    match value {
        ScVal::I128(parts) => Ok((i128::from(parts.hi) << 64) | i128::from(parts.lo)),
        ScVal::U128(parts) => i128::try_from((u128::from(parts.hi) << 64) | u128::from(parts.lo))
            .map_err(|_| contract_error("u128 return value does not fit in an i128")),
        ScVal::I64(value) => Ok(i128::from(*value)),
        ScVal::U64(value) => Ok(i128::from(*value)),
        ScVal::I32(value) => Ok(i128::from(*value)),
        ScVal::U32(value) => Ok(i128::from(*value)),
        other => Err(unexpected("an integer", other)),
    }
}

/// Read a `u64`.
pub fn as_u64(value: &ScVal) -> Result<u64> {
    match value {
        ScVal::U64(value) | ScVal::Timepoint(stellar_xdr::curr::TimePoint(value)) => Ok(*value),
        ScVal::U32(value) => Ok(u64::from(*value)),
        ScVal::I64(value) => u64::try_from(*value)
            .map_err(|_| contract_error(format!("expected a u64, got the negative i64 {value}"))),
        ScVal::I128(_) | ScVal::U128(_) => u64::try_from(as_i128(value)?)
            .map_err(|_| contract_error("128-bit return value does not fit in a u64")),
        other => Err(unexpected("a u64", other)),
    }
}

/// Read a `u32` — ledger sequences and transaction counts.
pub fn as_u32(value: &ScVal) -> Result<u32> {
    match value {
        ScVal::U32(value) => Ok(*value),
        ScVal::U64(value) => u32::try_from(*value)
            .map_err(|_| contract_error(format!("expected a u32, got {value}"))),
        ScVal::I32(value) => u32::try_from(*value)
            .map_err(|_| contract_error(format!("expected a u32, got {value}"))),
        other => Err(unexpected("a u32", other)),
    }
}

/// Read a `bool`.
pub fn as_bool(value: &ScVal) -> Result<bool> {
    match value {
        ScVal::Bool(value) => Ok(*value),
        other => Err(unexpected("a bool", other)),
    }
}

/// Read an address back as its strkey string (`G…` or `C…`).
pub fn as_address_string(value: &ScVal) -> Result<String> {
    match value {
        ScVal::Address(address) => Ok(address.to_string()),
        other => Err(unexpected("an address", other)),
    }
}

/// Read a `String`, `Symbol` or `Bytes` as UTF-8 text.
///
/// The three are interchangeable at the call sites that read free text back:
/// the contracts store it as `Bytes`, but a `String` or `Symbol` from a
/// different contract version should not force the caller to branch.
///
/// # Errors
///
/// [`ErrorCode::ContractError`] when the bytes are not valid UTF-8 — which is
/// possible, since `Bytes` is arbitrary binary on-chain.
pub fn as_utf8(value: &ScVal) -> Result<String> {
    let raw: &[u8] = match value {
        ScVal::String(ScString(s)) => s.as_slice(),
        ScVal::Symbol(ScSymbol(s)) => s.as_slice(),
        ScVal::Bytes(ScBytes(b)) => b.as_slice(),
        other => return Err(unexpected("text", other)),
    };
    String::from_utf8(raw.to_vec())
        .map_err(|_| contract_error("contract returned bytes that are not valid UTF-8"))
}

/// Read the elements of a `Vec`.
pub fn as_vec(value: &ScVal) -> Result<&[ScVal]> {
    match value {
        ScVal::Vec(Some(ScVec(items))) => Ok(items.as_slice()),
        ScVal::Vec(None) => Ok(&[]),
        other => Err(unexpected("a vector", other)),
    }
}

/// Read the variant name of a fieldless `#[contracttype]` enum.
///
/// Accepts a bare symbol as well as the one-element vector the host actually
/// produces, so a caller reading a value that has already been unwrapped one
/// level does not have to care.
pub fn as_enum_variant(value: &ScVal) -> Result<String> {
    match value {
        ScVal::Symbol(_) => as_utf8(value),
        ScVal::Vec(_) => {
            let items = as_vec(value)?;
            let first = items
                .first()
                .ok_or_else(|| contract_error("enum variant vector was empty"))?;
            as_utf8(first)
        }
        other => Err(unexpected("an enum variant", other)),
    }
}

/// Read one field out of a `#[contracttype]` struct.
///
/// # Errors
///
/// [`ErrorCode::ContractError`] when `value` is not a map, or when the field is
/// absent — naming the field, so a contract-version mismatch is legible rather
/// than presenting as a null somewhere downstream.
pub fn field<'a>(value: &'a ScVal, name: &str) -> Result<&'a ScVal> {
    let entries = match value {
        ScVal::Map(Some(ScMap(entries))) => entries.as_slice(),
        ScVal::Map(None) => &[],
        other => return Err(unexpected("a struct (map)", other)),
    };

    entries
        .iter()
        .find(|entry| matches!(&entry.key, ScVal::Symbol(ScSymbol(key)) if key.as_slice() == name.as_bytes()))
        .map(|entry| &entry.val)
        .ok_or_else(|| {
            let available: Vec<String> = entries
                .iter()
                .map(|entry| as_utf8(&entry.key).unwrap_or_else(|_| "?".into()))
                .collect();
            contract_error(format!(
                "contract struct has no field `{name}` (it has: {})",
                available.join(", ")
            ))
        })
}

/// Read an `Option<T>`: `None` for [`ScVal::Void`], `Some` otherwise.
pub fn as_option(value: &ScVal) -> Option<&ScVal> {
    match value {
        ScVal::Void => None,
        other => Some(other),
    }
}

/// Read an optional address field, mapping `Void` to `None`.
pub fn as_optional_address(value: &ScVal) -> Result<Option<String>> {
    as_option(value).map(as_address_string).transpose()
}

/// Read an `i128` and format it as a decimal XLM-style string.
///
/// The inverse of [`amount`], for reporting a balance or a limit back to a
/// caller in the units they supplied it in.
pub fn as_amount(value: &ScVal, decimal_places: u32) -> Result<String> {
    Ok(math::from_stroops(
        &BigInt::from(as_i128(value)?),
        decimal_places,
    )?)
}

// ─── XDR ─────────────────────────────────────────────────────────────────────

/// Encode to standard base64 XDR — the wire format the RPC server speaks.
pub fn to_xdr_base64(value: &ScVal) -> Result<String> {
    value
        .to_xdr_base64(Limits::none())
        .map_err(|error| invalid(format!("could not encode ScVal as XDR: {error}")))
}

/// Decode from base64 XDR.
///
/// ```
/// use stellaragent::scval;
///
/// let value = scval::i128_value(-42);
/// let encoded = scval::to_xdr_base64(&value)?;
/// assert_eq!(scval::from_xdr_base64(&encoded)?, value);
/// # Ok::<(), stellaragent::StellarAgentError>(())
/// ```
pub fn from_xdr_base64(encoded: &str) -> Result<ScVal> {
    ScVal::from_xdr_base64(encoded, Limits::none()).map_err(|error| {
        StellarAgentError::new(
            ErrorCode::ContractError,
            format!("could not decode ScVal from XDR: {error}"),
        )
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

fn sc_symbol(value: &str) -> Result<ScSymbol> {
    Ok(ScSymbol(value.try_into().map_err(|_| {
        invalid(format!(
            "Symbol \"{value}\" is {} characters; Soroban symbols are capped at 32",
            value.chars().count()
        ))
    })?))
}

fn unexpected(expected: &str, actual: &ScVal) -> StellarAgentError {
    contract_error(format!(
        "expected {expected} from the contract, got {}",
        actual.name()
    ))
}

/// `BigInt` → `i128`, or `None` when it does not fit.
fn bigint_to_i128(value: &BigInt) -> Option<i128> {
    i128::try_from(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "GA7QYNF7SOWQ3GLR2BGMZEHXAVIRZA4KVWLTJJFC7MGXUA74P7UJVSGZ";
    const CONTRACT: &str = "CA3D5KRYM6CB7OWQ6TWYRR3Z4T7GNZLKERYNZGGA5SOAOPIFY6YQGAXE";

    #[test]
    fn addresses_round_trip_for_both_account_and_contract_strkeys() {
        for input in [ACCOUNT, CONTRACT] {
            let value = address(input).unwrap();
            assert_eq!(as_address_string(&value).unwrap(), input);
        }
    }

    #[test]
    fn an_invalid_address_is_rejected_at_the_call_that_supplied_it() {
        let error = address("GNOPE").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("GNOPE"));
    }

    #[test]
    fn i128_round_trips_across_the_full_range() {
        for value in [0, 1, -1, i128::MAX, i128::MIN, 15_000_001, -82_390_000] {
            assert_eq!(as_i128(&i128_value(value)).unwrap(), value);
        }
    }

    #[test]
    fn u128_round_trips_across_the_full_range() {
        for value in [0u128, 1, u128::from(u64::MAX), u128::MAX] {
            let encoded = u128_value(value);
            match &encoded {
                ScVal::U128(parts) => {
                    assert_eq!((u128::from(parts.hi) << 64) | u128::from(parts.lo), value);
                }
                other => panic!("expected U128, got {}", other.name()),
            }
        }
    }

    #[test]
    fn a_u128_too_large_for_i128_is_reported_not_wrapped() {
        let error = as_i128(&u128_value(u128::MAX)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ContractError);
    }

    #[test]
    fn amounts_convert_through_the_deterministic_stroop_path() {
        assert_eq!(amount("1.5000001").unwrap(), i128_value(15_000_001));
        assert_eq!(amount("-8.239").unwrap(), i128_value(-82_390_000));
        // Truncation, not rounding — an extra stroop would break a limit check.
        assert_eq!(amount("1.50000019").unwrap(), i128_value(15_000_001));
    }

    #[test]
    fn an_amount_beyond_i128_is_rejected_rather_than_wrapped() {
        let error = amount("1e40").unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("i128"));
    }

    #[test]
    fn amounts_read_back_as_the_string_they_came_from() {
        assert_eq!(
            as_amount(&amount("123456.7891234").unwrap(), 7).unwrap(),
            "123456.7891234"
        );
    }

    #[test]
    fn a_unit_enum_is_a_vector_holding_one_symbol_not_a_bare_symbol() {
        let value = enum_variant("Hourly").unwrap();
        assert_eq!(as_vec(&value).unwrap().len(), 1);
        assert_eq!(as_enum_variant(&value).unwrap(), "Hourly");
        // A bare symbol still reads, for tolerance on the decode side.
        assert_eq!(as_enum_variant(&symbol("Daily").unwrap()).unwrap(), "Daily");
    }

    #[test]
    fn map_keys_are_sorted_regardless_of_insertion_order() {
        let unsorted = map(vec![
            ("zebra", u32_value(1)),
            ("alpha", u32_value(2)),
            ("middle", u32_value(3)),
        ])
        .unwrap();
        let sorted = map(vec![
            ("alpha", u32_value(2)),
            ("middle", u32_value(3)),
            ("zebra", u32_value(1)),
        ])
        .unwrap();
        assert_eq!(unsorted, sorted, "the host rejects an unsorted map");
    }

    #[test]
    fn struct_fields_are_read_by_name() {
        let value = map(vec![
            ("agent", address(ACCOUNT).unwrap()),
            ("active", bool_value(true)),
            ("total_spent", i128_value(42)),
        ])
        .unwrap();

        assert_eq!(
            as_address_string(field(&value, "agent").unwrap()).unwrap(),
            ACCOUNT
        );
        assert!(as_bool(field(&value, "active").unwrap()).unwrap());
        assert_eq!(as_i128(field(&value, "total_spent").unwrap()).unwrap(), 42);
    }

    #[test]
    fn a_missing_field_names_itself_and_lists_what_was_there() {
        let value = map(vec![("agent", void()), ("active", bool_value(true))]).unwrap();
        let error = field(&value, "owner").unwrap_err();
        assert_eq!(error.code(), ErrorCode::ContractError);
        assert!(error.message().contains("owner"), "{}", error.message());
        assert!(error.message().contains("active"), "{}", error.message());
    }

    #[test]
    fn options_encode_as_the_value_or_void() {
        assert_eq!(option(None), ScVal::Void);
        assert_eq!(option(Some(u32_value(7))), u32_value(7));
        assert!(as_option(&ScVal::Void).is_none());
        assert_eq!(
            as_optional_address(&address(ACCOUNT).unwrap()).unwrap(),
            Some(ACCOUNT.to_string())
        );
        assert_eq!(as_optional_address(&ScVal::Void).unwrap(), None);
    }

    #[test]
    fn text_reads_back_from_string_symbol_and_bytes_alike() {
        assert_eq!(as_utf8(&string("hello").unwrap()).unwrap(), "hello");
        assert_eq!(as_utf8(&symbol("hello").unwrap()).unwrap(), "hello");
        assert_eq!(as_utf8(&bytes_from_str("hello").unwrap()).unwrap(), "hello");
    }

    #[test]
    fn invalid_utf8_bytes_are_reported_rather_than_lossily_decoded() {
        let value = bytes(&[0xff, 0xfe]).unwrap();
        assert_eq!(
            as_utf8(&value).unwrap_err().code(),
            ErrorCode::ContractError
        );
    }

    #[test]
    fn an_over_long_symbol_is_rejected_with_its_length() {
        let error = symbol(&"a".repeat(33)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(error.message().contains("32"));
    }

    #[test]
    fn reading_the_wrong_type_names_both_sides() {
        let error = as_bool(&u32_value(1)).unwrap_err();
        assert!(error.message().contains("a bool"), "{}", error.message());
        assert!(error.message().contains("U32"), "{}", error.message());
    }

    #[test]
    fn every_builder_survives_an_xdr_round_trip() {
        let values = vec![
            void(),
            bool_value(true),
            u32_value(7),
            i32_value(-7),
            u64_value(u64::MAX),
            i64_value(i64::MIN),
            i128_value(i128::MIN),
            u128_value(u128::MAX),
            address(ACCOUNT).unwrap(),
            address(CONTRACT).unwrap(),
            symbol("transfer").unwrap(),
            string("https://api.example.com/inference").unwrap(),
            bytes_from_str("task description").unwrap(),
            enum_variant("PerLedger").unwrap(),
            vec(vec![u32_value(1), u32_value(2)]).unwrap(),
            map(vec![("b", u32_value(2)), ("a", u32_value(1))]).unwrap(),
        ];

        for value in values {
            let encoded = to_xdr_base64(&value).unwrap();
            assert_eq!(
                from_xdr_base64(&encoded).unwrap(),
                value,
                "round-tripping {encoded}"
            );
        }
    }

    #[test]
    fn malformed_xdr_is_an_error_not_a_panic() {
        assert!(from_xdr_base64("not base64 at all!!").is_err());
        assert!(from_xdr_base64("AAAA").is_err());
    }
}
