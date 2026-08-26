//! Wall-clock estimation for Stellar's ledger-sequence-based windows.
//!
//! Rust port of `packages/core/src/ledgerTime.ts`.
//!
//! `RateLimiter` and `PaymentChannel` both track their rolling windows in
//! **ledger sequence numbers**, not timestamps — `hour_window_start`,
//! `day_window_start`, `period_start_ledger`. Ledgers close roughly every five
//! seconds, but that number drifts with network conditions and is not
//! contractually guaranteed, so hard-coding "5 seconds" would silently mislead
//! a UI showing "resets in ~N seconds" whenever the real network runs faster
//! or slower.
//!
//! This module instead derives an average close time from a handful of
//! recently observed ledgers and uses it to convert a ledger count into an
//! estimated number of seconds. Fetching those samples is
//! [`crate::StellarAgent::ledger_close_estimate`]; everything here is pure, so
//! it can be tested against fabricated samples without a network.
//!
//! # Floating point lives here, and only here
//!
//! Every other module in [`crate::math`] is float-free by construction,
//! because a rounding difference there changes which bid wins. This one
//! returns `f64`, because what it produces is explicitly an **estimate** for
//! human display — it never feeds a spend decision, a score, or a hash. Do not
//! route a monetary value through it.

use serde::{Deserialize, Serialize};

/// A single observed ledger close, as needed to derive an average close time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerCloseSample {
    /// The ledger's sequence number.
    pub sequence: u32,
    /// ISO 8601 timestamp, as returned by Horizon's `closed_at` field.
    pub closed_at: String,
}

/// Fallback average ledger close time, in seconds, used only when fewer than
/// two usable samples are available — e.g. a brand new standalone network with
/// a single ledger closed so far.
///
/// This is the commonly cited Stellar figure, but it is a fallback, not a
/// measurement. Prefer [`estimate_ledger_close_seconds`] against real samples
/// whenever they are available, and surface
/// [`LedgerCloseEstimate::observed`] so a caller can tell the two apart.
pub const FALLBACK_LEDGER_CLOSE_SECONDS: f64 = 5.0;

/// The current ledger tip plus a close-time estimate derived from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerCloseEstimate {
    /// The highest ledger sequence among the fetched samples — the current tip.
    pub current_ledger: u32,
    /// Observed (or, absent enough samples, fallback) average seconds per ledger.
    pub avg_ledger_close_seconds: f64,
    /// `true` when the average came from real observed closes; `false` when
    /// [`FALLBACK_LEDGER_CLOSE_SECONDS`] was used instead.
    ///
    /// Surface this alongside any "resets in ~N seconds" display, so it is
    /// clear when the estimate is a measurement and when it is a guess.
    pub observed: bool,
}

/// Derive the observed average seconds-per-ledger from recent close samples.
///
/// Sums the wall-clock gaps between consecutive sequences and divides by the
/// total number of ledgers those gaps span — rather than averaging per-pair
/// ratios, so one irregular gap does not get equal weight against many
/// one-ledger gaps.
///
/// Samples need not be pre-sorted or contiguous. Any pair with a non-positive
/// ledger delta or a negative/unparseable time delta is skipped — defensive
/// against a misbehaving RPC provider returning out-of-order or duplicate
/// records — and the result falls back to [`FALLBACK_LEDGER_CLOSE_SECONDS`] if
/// fewer than two usable samples remain after that filtering.
///
/// ```
/// use stellaragent::math::{estimate_ledger_close_seconds, LedgerCloseSample};
///
/// let samples = vec![
///     LedgerCloseSample { sequence: 100, closed_at: "2024-01-01T00:00:00Z".into() },
///     LedgerCloseSample { sequence: 110, closed_at: "2024-01-01T00:01:00Z".into() },
/// ];
/// assert_eq!(estimate_ledger_close_seconds(&samples), 6.0);
/// ```
pub fn estimate_ledger_close_seconds(samples: &[LedgerCloseSample]) -> f64 {
    let mut sorted: Vec<&LedgerCloseSample> = samples.iter().collect();
    sorted.sort_by_key(|s| s.sequence);

    let mut total_seconds = 0i64;
    let mut total_ledgers = 0u64;

    for pair in sorted.windows(2) {
        let (previous, current) = (pair[0], pair[1]);
        let ledger_delta = match current.sequence.checked_sub(previous.sequence) {
            Some(delta) if delta > 0 => delta,
            // Equal or out-of-order sequences carry no information about how
            // long a ledger takes; skipping beats folding a zero or a
            // negative into the average.
            _ => continue,
        };

        let (Some(previous_at), Some(current_at)) = (
            parse_iso8601_seconds(&previous.closed_at),
            parse_iso8601_seconds(&current.closed_at),
        ) else {
            continue;
        };
        let seconds_delta = current_at - previous_at;
        if seconds_delta < 0 {
            continue;
        }

        total_seconds += seconds_delta;
        total_ledgers += u64::from(ledger_delta);
    }

    if total_ledgers == 0 {
        return FALLBACK_LEDGER_CLOSE_SECONDS;
    }
    total_seconds as f64 / total_ledgers as f64
}

/// Build a [`LedgerCloseEstimate`] from samples already in hand.
///
/// Returns `None` for an empty sample set: without a single ledger there is no
/// tip to report, and inventing one would be worse than saying so.
pub fn ledger_close_estimate(samples: &[LedgerCloseSample]) -> Option<LedgerCloseEstimate> {
    let current_ledger = samples.iter().map(|s| s.sequence).max()?;
    Some(LedgerCloseEstimate {
        current_ledger,
        avg_ledger_close_seconds: estimate_ledger_close_seconds(samples),
        observed: samples.len() >= 2,
    })
}

/// Convert a ledger count into an estimated number of wall-clock seconds.
///
/// Purely `ledgers * avg_ledger_close_seconds` — split out from
/// [`estimate_ledger_close_seconds`] so a caller can recompute it on every
/// render as a countdown ticks down, without re-deriving the average.
///
/// ```
/// use stellaragent::math::estimate_seconds_remaining;
/// assert_eq!(estimate_seconds_remaining(720, 5.2), 3744.0);
/// ```
pub fn estimate_seconds_remaining(ledgers_remaining: u32, avg_ledger_close_seconds: f64) -> f64 {
    f64::from(ledgers_remaining) * avg_ledger_close_seconds
}

/// Parse an ISO 8601 / RFC 3339 timestamp to whole seconds since the Unix epoch.
///
/// Hand-rolled rather than pulling in a date-time crate: the only producer is
/// Horizon's `closed_at`, whose shape is fixed, and this is the sole place the
/// SDK needs to read a date at all. Returns `None` for anything it does not
/// recognise, which the caller treats as "skip this sample" rather than as a
/// hard failure.
fn parse_iso8601_seconds(input: &str) -> Option<i64> {
    let bytes = input.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if bytes[10] != b'T' && bytes[10] != b' ' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }

    let year: i64 = input[0..4].parse().ok()?;
    let month: u32 = input[5..7].parse().ok()?;
    let day: u32 = input[8..10].parse().ok()?;
    let hour: i64 = input[11..13].parse().ok()?;
    let minute: i64 = input[14..16].parse().ok()?;
    let second: i64 = input[17..19].parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut timestamp =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;

    // Trailing zone: `Z`, nothing, or a ±HH:MM offset. Fractional seconds are
    // dropped — Horizon reports whole seconds, and sub-second precision would
    // not survive the integer average anyway.
    let tail = &input[19..];
    let tail = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(offset) = tail.strip_prefix('+').or_else(|| tail.strip_prefix('-')) {
        let sign = if tail.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = match offset.split_once(':') {
            Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
            None if offset.len() == 4 => (
                offset[0..2].parse::<i64>().ok()?,
                offset[2..4].parse::<i64>().ok()?,
            ),
            None => (offset.parse::<i64>().ok()?, 0),
        };
        timestamp -= sign * (hours * 3600 + minutes * 60);
    }

    Some(timestamp)
}

/// Days since 1970-01-01 for a civil date, by Howard Hinnant's `days_from_civil`.
///
/// Correct for every proleptic Gregorian date, which is more than Horizon will
/// ever hand us, and short enough not to justify a dependency.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(sequence: u32, closed_at: &str) -> LedgerCloseSample {
        LedgerCloseSample {
            sequence,
            closed_at: closed_at.to_string(),
        }
    }

    #[test]
    fn averages_over_total_ledgers_not_over_pairs() {
        // One 10-ledger/60s gap and one 1-ledger/5s gap. Averaging the two
        // ratios would give 5.5; weighting by ledgers spanned gives 65/11.
        let samples = vec![
            sample(100, "2024-01-01T00:00:00Z"),
            sample(110, "2024-01-01T00:01:00Z"),
            sample(111, "2024-01-01T00:01:05Z"),
        ];
        assert!((estimate_ledger_close_seconds(&samples) - 65.0 / 11.0).abs() < 1e-12);
    }

    #[test]
    fn samples_need_not_arrive_sorted() {
        let ascending = vec![
            sample(100, "2024-01-01T00:00:00Z"),
            sample(110, "2024-01-01T00:01:00Z"),
        ];
        let descending = vec![
            sample(110, "2024-01-01T00:01:00Z"),
            sample(100, "2024-01-01T00:00:00Z"),
        ];
        assert_eq!(
            estimate_ledger_close_seconds(&ascending),
            estimate_ledger_close_seconds(&descending)
        );
    }

    #[test]
    fn falls_back_when_there_is_nothing_usable_to_measure() {
        assert_eq!(
            estimate_ledger_close_seconds(&[]),
            FALLBACK_LEDGER_CLOSE_SECONDS
        );
        assert_eq!(
            estimate_ledger_close_seconds(&[sample(1, "2024-01-01T00:00:00Z")]),
            FALLBACK_LEDGER_CLOSE_SECONDS
        );
        // Duplicate sequences carry no timing information.
        let duplicates = vec![
            sample(5, "2024-01-01T00:00:00Z"),
            sample(5, "2024-01-01T00:00:05Z"),
        ];
        assert_eq!(
            estimate_ledger_close_seconds(&duplicates),
            FALLBACK_LEDGER_CLOSE_SECONDS
        );
    }

    #[test]
    fn a_provider_returning_time_travel_is_skipped_not_averaged_in() {
        let samples = vec![
            sample(100, "2024-01-01T00:01:00Z"),
            sample(101, "2024-01-01T00:00:00Z"), // closed_at goes backwards
            sample(102, "2024-01-01T00:01:05Z"),
        ];
        // Only the 101→102 pair is usable, and it is 65 seconds wide.
        assert_eq!(estimate_ledger_close_seconds(&samples), 65.0);
    }

    #[test]
    fn unparseable_timestamps_are_skipped() {
        let samples = vec![
            sample(1, "not-a-date"),
            sample(2, "2024-01-01T00:00:00Z"),
            sample(3, "2024-01-01T00:00:04Z"),
        ];
        assert_eq!(estimate_ledger_close_seconds(&samples), 4.0);
    }

    #[test]
    fn estimate_reports_the_tip_and_whether_it_measured_anything() {
        let estimate = ledger_close_estimate(&[
            sample(7, "2024-01-01T00:00:00Z"),
            sample(9, "2024-01-01T00:00:10Z"),
        ])
        .unwrap();
        assert_eq!(estimate.current_ledger, 9);
        assert_eq!(estimate.avg_ledger_close_seconds, 5.0);
        assert!(estimate.observed);

        let single = ledger_close_estimate(&[sample(3, "2024-01-01T00:00:00Z")]).unwrap();
        assert!(!single.observed);
        assert_eq!(
            single.avg_ledger_close_seconds,
            FALLBACK_LEDGER_CLOSE_SECONDS
        );

        assert!(ledger_close_estimate(&[]).is_none());
    }

    #[test]
    fn parses_the_timestamp_shapes_horizon_emits() {
        assert_eq!(parse_iso8601_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso8601_seconds("2024-01-15T12:34:56Z"),
            Some(1_705_322_096)
        );
        // Fractional seconds are dropped, not rejected.
        assert_eq!(
            parse_iso8601_seconds("2024-01-15T12:34:56.789Z"),
            Some(1_705_322_096)
        );
        // An explicit offset is applied.
        assert_eq!(
            parse_iso8601_seconds("2024-01-15T13:34:56+01:00"),
            Some(1_705_322_096)
        );
        assert_eq!(parse_iso8601_seconds("garbage"), None);
        assert_eq!(parse_iso8601_seconds("2024-13-01T00:00:00Z"), None);
    }

    #[test]
    fn seconds_remaining_is_a_plain_product() {
        assert_eq!(estimate_seconds_remaining(0, 5.0), 0.0);
        assert_eq!(estimate_seconds_remaining(720, 5.0), 3600.0);
    }
}
