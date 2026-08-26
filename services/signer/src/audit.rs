//! An append-only, hash-chained record of every request and decision.
//!
//! # The chain
//!
//! Each record carries the hash of the one before it:
//!
//! ```text
//! hash(n) = SHA-256( prev_hash(n-1) ‖ canonical_json(record(n) without hash) )
//! ```
//!
//! Editing or deleting any record breaks every hash from that point on, so
//! tampering is detectable by re-walking the file — [`verify_chain`].
//!
//! # What this does not give you
//!
//! A hash chain is tamper-**evident**, not tamper-**proof**. Anyone who can
//! rewrite the log can recompute the whole chain and produce a file that
//! verifies perfectly. It becomes evidence only once the head hash is anchored
//! somewhere the attacker cannot reach — shipped to a separate account's log
//! sink, written to append-only storage, or published periodically.
//!
//! The deployment guide says this in those words. Claiming otherwise would be
//! worse than having no chain, because it would invite someone to rely on it.
//!
//! # A failed write fails the request
//!
//! Signing something we cannot record is precisely the case this log exists
//! for, so [`AuditLog::record`] propagates a sink failure and the request is
//! refused. The alternative — sign anyway, log later, hope — turns the audit
//! trail into a best-effort convenience.
//!
//! # What is never written
//!
//! Tokens and key material, structurally: neither has a field here. Request
//! text that a caller controls (an endpoint, a task description) reaches this
//! module already truncated and stripped of control characters by
//! [`crate::inspect`], so a log line cannot be forged by an argument value.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::auth::{Subject, UnixSeconds};
use crate::error::Violation;

/// The genesis value for an empty chain.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Which endpoint a record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// `GET /v1/public-key`
    PublicKey,
    /// `POST /v1/sign/transaction`
    SignTransaction,
    /// `POST /v1/sign/auth-entry`
    SignAuthEntry,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Outcome {
    /// A signature was produced.
    Signed {
        /// Hex `SHA-256` of the exact payload signed, so a record can be tied
        /// to a signature without storing the whole envelope.
        payload_sha256: String,
        /// The signature, hex. Public information — it goes on-chain.
        signature: String,
    },
    /// The request was refused.
    Refused {
        /// The machine-readable reason.
        reason: String,
        /// Which policy rules objected, when it was a policy refusal.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        violations: Vec<Violation>,
    },
}

/// A decoded summary of what was requested, for the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSummary {
    /// The network passphrase the caller asked us to sign for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// The transaction's source account, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_account: Option<String>,
    /// Hex `SHA-256` of the submitted XDR, so two requests can be compared
    /// without storing either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_sha256: Option<String>,
    /// The contract calls, rendered `contract.function(args…)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// Total value moved, in stroops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_amount_stroops: Option<String>,
}

/// One line of the log.
///
/// Field order is load-bearing: it is what makes the canonical JSON
/// deterministic, and therefore what an independent verifier must reproduce.
/// Reordering these fields invalidates every existing chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Monotonic sequence number within this log.
    pub seq: u64,
    /// When the request was handled.
    pub at: UnixSeconds,
    /// The correlation id, echoed to the caller.
    pub request_id: String,
    /// Who called. `None` when authentication itself failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    /// Which credential was used, by id. Never the credential itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// The key that would have signed, as `backend:key_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The policy evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Which endpoint.
    pub operation: Operation,
    /// What was asked for.
    pub request: RequestSummary,
    /// What happened.
    #[serde(flatten)]
    pub outcome: Outcome,
    /// The previous record's hash.
    pub prev_hash: String,
    /// This record's hash.
    pub hash: String,
}

impl AuditRecord {
    /// Recompute this record's hash from its contents and `prev_hash`.
    ///
    /// The `hash` field is excluded by construction: the value is computed
    /// from a clone with the field blanked, so there is no way to accidentally
    /// hash a record that includes its own hash.
    pub fn compute_hash(&self) -> String {
        let mut unhashed = self.clone();
        unhashed.hash = String::new();
        let canonical = serde_json::to_vec(&unhashed).unwrap_or_default();

        let mut hasher = Sha256::new();
        hasher.update(self.prev_hash.as_bytes());
        hasher.update(&canonical);
        hex::encode(hasher.finalize())
    }
}

/// Somewhere records are written.
pub trait Sink: Send + Sync {
    /// Append one JSON line. Must be durable enough that a crash after this
    /// returns does not lose the record.
    fn append(&self, line: &str) -> std::io::Result<()>;
}

/// Writes to standard output, one JSON object per line.
///
/// The right default for a container: the platform's log pipeline handles
/// shipping, rotation and retention, and the service does not have to.
#[derive(Debug, Default)]
pub struct StdoutSink;

impl Sink for StdoutSink {
    fn append(&self, line: &str) -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(line.as_bytes())?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    }
}

/// Appends to a file, flushing and syncing each record.
///
/// `sync_all` on every write is deliberately expensive: an audit record that is
/// still in a page cache when the machine loses power is a record that did not
/// happen, and the throughput of this service is bounded by KMS round trips
/// rather than by fsync.
#[derive(Debug)]
pub struct FileSink {
    file: Mutex<File>,
}

impl FileSink {
    /// Open (or create) `path` for appending.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl Sink for FileSink {
    fn append(&self, line: &str) -> std::io::Result<()> {
        let mut file = self.file.lock().expect("audit file");
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()
    }
}

/// Collects records in memory. For tests and the conformance suite.
#[derive(Debug, Default)]
pub struct MemorySink {
    lines: Mutex<Vec<String>>,
}

impl MemorySink {
    /// A new, empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every line written so far.
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().expect("memory sink").clone()
    }

    /// Every record written so far, parsed.
    pub fn records(&self) -> Vec<AuditRecord> {
        self.lines()
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

impl Sink for MemorySink {
    fn append(&self, line: &str) -> std::io::Result<()> {
        self.lines
            .lock()
            .expect("memory sink")
            .push(line.to_string());
        Ok(())
    }
}

/// A sink that always fails. Used to prove a request is refused when the log
/// is unavailable.
#[derive(Debug, Default)]
pub struct FailingSink;

impl Sink for FailingSink {
    fn append(&self, _: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("audit sink is unavailable"))
    }
}

/// What a caller fills in; the log supplies the rest.
#[derive(Debug, Clone)]
pub struct PendingRecord {
    /// When the request was handled.
    pub at: UnixSeconds,
    /// The correlation id.
    pub request_id: String,
    /// Who called, once known.
    pub subject: Option<Subject>,
    /// Which credential, by id.
    pub token_id: Option<String>,
    /// The key involved.
    pub key: Option<String>,
    /// The policy evaluated.
    pub policy: Option<String>,
    /// Which endpoint.
    pub operation: Operation,
    /// What was asked for.
    pub request: RequestSummary,
    /// What happened.
    pub outcome: Outcome,
}

/// The append-only log.
pub struct AuditLog {
    sink: Box<dyn Sink>,
    state: Mutex<ChainState>,
}

#[derive(Debug)]
struct ChainState {
    seq: u64,
    head: String,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl AuditLog {
    /// Start a fresh chain writing to `sink`.
    pub fn new(sink: Box<dyn Sink>) -> Self {
        Self {
            sink,
            state: Mutex::new(ChainState {
                seq: 0,
                head: GENESIS_HASH.to_string(),
            }),
        }
    }

    /// Continue an existing chain from `seq` and `head`.
    ///
    /// Used on restart so a log does not silently start a second chain that
    /// verifies on its own but is disconnected from what came before.
    pub fn resuming(sink: Box<dyn Sink>, seq: u64, head: impl Into<String>) -> Self {
        Self {
            sink,
            state: Mutex::new(ChainState {
                seq,
                head: head.into(),
            }),
        }
    }

    /// Append a record, returning what was written.
    ///
    /// # Errors
    ///
    /// Propagates a sink failure. The caller must refuse the request.
    pub fn record(&self, pending: PendingRecord) -> std::io::Result<AuditRecord> {
        let mut state = self.state.lock().expect("audit chain");

        let mut record = AuditRecord {
            seq: state.seq,
            at: pending.at,
            request_id: pending.request_id,
            subject: pending.subject,
            token_id: pending.token_id,
            key: pending.key,
            policy: pending.policy,
            operation: pending.operation,
            request: pending.request,
            outcome: pending.outcome,
            prev_hash: state.head.clone(),
            hash: String::new(),
        };
        record.hash = record.compute_hash();

        let line = serde_json::to_string(&record).map_err(|error| {
            std::io::Error::other(format!("could not encode audit record: {error}"))
        })?;

        // Only advance the chain once the write succeeded. Advancing first
        // would leave a gap that makes the chain unverifiable after a
        // transient sink failure.
        self.sink.append(&line)?;
        state.seq += 1;
        state.head = record.hash.clone();

        Ok(record)
    }

    /// The current head hash — the value to anchor externally.
    pub fn head(&self) -> String {
        self.state.lock().expect("audit chain").head.clone()
    }

    /// How many records have been written.
    pub fn len(&self) -> u64 {
        self.state.lock().expect("audit chain").seq
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Where a chain stopped verifying.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    /// A record's hash does not match its contents.
    #[error("record {seq} has been altered: its hash does not match its contents")]
    HashMismatch {
        /// The sequence number of the offending record.
        seq: u64,
    },
    /// A record does not point at its predecessor.
    #[error("record {seq} does not follow record {expected_seq}: the chain has been cut")]
    BrokenLink {
        /// The offending record.
        seq: u64,
        /// The record it should have followed.
        expected_seq: u64,
    },
    /// Sequence numbers are not contiguous.
    #[error("record {seq} appears where {expected} was expected: records have been removed")]
    SequenceGap {
        /// The sequence number found.
        seq: u64,
        /// The one expected.
        expected: u64,
    },
}

/// Walk a chain and confirm nothing has been altered or removed.
///
/// Remember what this does and does not prove — see the module docs.
#[allow(
    clippy::explicit_counter_loop,
    reason = "the counter is compared against each record's own seq before being incremented; \
              zipping a range would hide exactly the off-by-one this is checking for"
)]
pub fn verify_chain(records: &[AuditRecord]) -> Result<(), ChainError> {
    let mut previous_hash = GENESIS_HASH.to_string();
    let mut expected_seq = records.first().map_or(0, |first| first.seq);

    for record in records {
        if record.seq != expected_seq {
            return Err(ChainError::SequenceGap {
                seq: record.seq,
                expected: expected_seq,
            });
        }
        if record.prev_hash != previous_hash {
            return Err(ChainError::BrokenLink {
                seq: record.seq,
                expected_seq: expected_seq.saturating_sub(1),
            });
        }
        if record.compute_hash() != record.hash {
            return Err(ChainError::HashMismatch { seq: record.seq });
        }
        previous_hash = record.hash.clone();
        expected_seq += 1;
    }

    Ok(())
}

/// Hex `SHA-256` of a string, for envelope and payload digests.
pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn pending(request_id: &str) -> PendingRecord {
        PendingRecord {
            at: 1_700_000_000,
            request_id: request_id.into(),
            subject: Some(Subject::new("agent-1")),
            token_id: Some("t1".into()),
            key: Some("local:k1".into()),
            policy: Some("default".into()),
            operation: Operation::SignTransaction,
            request: RequestSummary {
                network: Some("Test SDF Network ; September 2015".into()),
                source_account: Some("GAGENT".into()),
                envelope_sha256: Some(digest("envelope")),
                calls: vec!["CCONTRACT.pay(10000000)".into()],
                total_amount_stroops: Some("10000000".into()),
            },
            outcome: Outcome::Signed {
                payload_sha256: digest("payload"),
                signature: hex::encode([1u8; 64]),
            },
        }
    }

    fn log_with(sink: Arc<MemorySink>) -> AuditLog {
        struct Shared(Arc<MemorySink>);
        impl Sink for Shared {
            fn append(&self, line: &str) -> std::io::Result<()> {
                self.0.append(line)
            }
        }
        AuditLog::new(Box::new(Shared(sink)))
    }

    #[test]
    fn a_chain_of_records_verifies() {
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));

        for i in 0..5 {
            log.record(pending(&format!("req-{i}"))).unwrap();
        }

        let records = sink.records();
        assert_eq!(records.len(), 5);
        verify_chain(&records).unwrap();
        assert_eq!(log.head(), records.last().unwrap().hash);
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn the_first_record_links_to_genesis() {
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));
        log.record(pending("first")).unwrap();
        assert_eq!(sink.records()[0].prev_hash, GENESIS_HASH);
    }

    #[test]
    fn altering_a_record_breaks_the_chain() {
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));
        for i in 0..3 {
            log.record(pending(&format!("req-{i}"))).unwrap();
        }

        let mut records = sink.records();
        // Someone rewrites the amount to hide a large payment.
        records[1].request.total_amount_stroops = Some("1".into());

        assert_eq!(
            verify_chain(&records),
            Err(ChainError::HashMismatch { seq: 1 })
        );
    }

    #[test]
    fn deleting_a_record_breaks_the_chain() {
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));
        for i in 0..3 {
            log.record(pending(&format!("req-{i}"))).unwrap();
        }

        let mut records = sink.records();
        records.remove(1);

        // The gap is caught before the link check, and names what was removed.
        assert_eq!(
            verify_chain(&records),
            Err(ChainError::SequenceGap {
                seq: 2,
                expected: 1
            })
        );
    }

    #[test]
    fn re_hashing_an_altered_record_still_breaks_the_link_to_the_next_one() {
        // The point of chaining rather than per-record hashing: an editor who
        // recomputes one record's own hash has not fixed the successor.
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));
        for i in 0..3 {
            log.record(pending(&format!("req-{i}"))).unwrap();
        }

        let mut records = sink.records();
        records[1].request.total_amount_stroops = Some("1".into());
        records[1].hash = records[1].compute_hash();

        assert_eq!(
            verify_chain(&records),
            Err(ChainError::BrokenLink {
                seq: 2,
                expected_seq: 1
            })
        );
    }

    #[test]
    fn a_refusal_is_recorded_as_carefully_as_a_signature() {
        // "Log every signature request, granted or refused" —
        // docs/signing.md, service responsibilities.
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));

        log.record(PendingRecord {
            outcome: Outcome::Refused {
                reason: "policy_violation".into(),
                violations: vec![Violation::new("amount_cap", "over the limit")],
            },
            ..pending("refused")
        })
        .unwrap();

        let record = &sink.records()[0];
        match &record.outcome {
            Outcome::Refused { reason, violations } => {
                assert_eq!(reason, "policy_violation");
                assert_eq!(violations[0].rule, "amount_cap");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        verify_chain(&sink.records()).unwrap();
    }

    #[test]
    fn a_sink_failure_propagates_so_the_request_can_be_refused() {
        // Signing something we cannot record is the case the log exists for.
        let log = AuditLog::new(Box::new(FailingSink));
        assert!(log.record(pending("doomed")).is_err());
    }

    #[test]
    fn a_failed_write_does_not_advance_the_chain() {
        // Advancing on failure would leave a gap that makes every later
        // record unverifiable.
        let log = AuditLog::new(Box::new(FailingSink));
        assert!(log.record(pending("doomed")).is_err());
        assert_eq!(log.len(), 0);
        assert_eq!(log.head(), GENESIS_HASH);
    }

    #[test]
    fn a_resumed_chain_continues_rather_than_starting_a_second_one() {
        let sink = Arc::new(MemorySink::new());
        let first = log_with(Arc::clone(&sink));
        first.record(pending("before-restart")).unwrap();
        let (seq, head) = (first.len(), first.head());

        struct Shared(Arc<MemorySink>);
        impl Sink for Shared {
            fn append(&self, line: &str) -> std::io::Result<()> {
                self.0.append(line)
            }
        }
        let resumed = AuditLog::resuming(Box::new(Shared(Arc::clone(&sink))), seq, head);
        resumed.record(pending("after-restart")).unwrap();

        verify_chain(&sink.records()).unwrap();
        assert_eq!(sink.records()[1].seq, 1);
    }

    #[test]
    fn a_record_never_contains_a_token_or_key_material() {
        // Structural: there is no field for either. This asserts it stays that
        // way as fields are added.
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));
        log.record(pending("req")).unwrap();

        let line = &sink.lines()[0];
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let object = value.as_object().unwrap();
        for forbidden in ["token", "secret", "seed", "private_key", "token_sha256"] {
            assert!(!object.contains_key(forbidden), "{line}");
        }
        // The token is referenced by id only.
        assert_eq!(object["token_id"], "t1");
    }

    #[test]
    fn a_file_sink_round_trips_a_verifiable_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        {
            let log = AuditLog::new(Box::new(FileSink::open(&path).unwrap()));
            for i in 0..3 {
                log.record(pending(&format!("req-{i}"))).unwrap();
            }
        }

        let contents = std::fs::read_to_string(&path).unwrap();
        let records: Vec<AuditRecord> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 3);
        verify_chain(&records).unwrap();
    }

    #[test]
    fn an_empty_chain_verifies_vacuously() {
        verify_chain(&[]).unwrap();
    }

    #[test]
    fn the_hash_never_includes_itself() {
        // Guards the one mistake that would make every hash unreproducible.
        let sink = Arc::new(MemorySink::new());
        let log = log_with(Arc::clone(&sink));
        let written = log.record(pending("req")).unwrap();
        assert_eq!(written.compute_hash(), written.hash);
    }
}
