//! Wire-level packet trace types and pre-transmission safety validators.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{TransportError, TransportResult};

/// USB HID report transfer category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReportKind {
    /// Interrupt / `SET_REPORT` output packet (`hid_write`).
    Output,
    /// `GET_REPORT` / `SET_REPORT` feature packet (`hid_send_feature_report` / `hid_get_feature_report`).
    Feature,
    /// Interrupt input packet (`hid_read_timeout`).
    Input,
}

impl fmt::Display for ReportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output => f.write_str("OUTPUT"),
            Self::Feature => f.write_str("FEATURE"),
            Self::Input => f.write_str("INPUT"),
        }
    }
}

/// Direction of a traced HID packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PacketDirection {
    HostToDevice,
    DeviceToHost,
}

impl fmt::Display for PacketDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostToDevice => f.write_str("TX->"),
            Self::DeviceToHost => f.write_str("<-RX"),
        }
    }
}

/// Disposition of a traced HID transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PacketOutcome {
    Transmitted,
    Received,
    /// Packet passed validation but was suppressed because `--dry-run` is active.
    DryRunSuppressed,
    /// Packet was rejected by a `PacketValidator` before reaching hardware.
    ValidationFailed,
}

impl fmt::Display for PacketOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transmitted => f.write_str("SENT"),
            Self::Received => f.write_str("RECV"),
            Self::DryRunSuppressed => f.write_str("DRY-RUN"),
            Self::ValidationFailed => f.write_str("REJECTED"),
        }
    }
}

/// Immutable record of one HID wire frame for CLI `--trace-hex` and the GPUI packet inspector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PacketTraceEntry {
    pub sequence: u64,
    pub timestamp_unix_ms: u128,
    pub device_label: String,
    pub kind: ReportKind,
    pub direction: PacketDirection,
    pub outcome: PacketOutcome,
    pub bytes: Vec<u8>,
    pub note: Option<String>,
}

impl PacketTraceEntry {
    pub fn new(
        sequence: u64,
        device_label: impl Into<String>,
        kind: ReportKind,
        direction: PacketDirection,
        outcome: PacketOutcome,
        bytes: Vec<u8>,
    ) -> Self {
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        Self {
            sequence,
            timestamp_unix_ms,
            device_label: device_label.into(),
            kind,
            direction,
            outcome,
            bytes,
            note: None,
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Formats the packet bytes as space-separated uppercase hexadecimal bytes.
    pub fn hex_spaced(&self) -> String {
        format_hex_spaced(&self.bytes)
    }
}

/// Formats a byte slice as uppercase space-separated hex (e.g. `10 FF 00 1A`).
pub fn format_hex_spaced(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(3));
    for (ix, byte) in bytes.iter().enumerate() {
        if ix > 0 {
            out.push(' ');
        }
        let encoded = hex::encode_upper([*byte]);
        out.push_str(&encoded);
    }
    out
}

/// Pre-transmission safety validator invoked by `GuardedTransport` before any write.
pub trait PacketValidator: Send + Sync {
    /// Validates an outgoing host-to-device HID report.
    fn validate_outgoing(&self, kind: ReportKind, report: &[u8]) -> TransportResult<()>;
}

/// Permissive validator intended only for tests or protocols with no fixed framing yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpValidator;

impl PacketValidator for NoOpValidator {
    fn validate_outgoing(&self, _kind: ReportKind, _report: &[u8]) -> TransportResult<()> {
        Ok(())
    }
}

/// Function pointer type for verifying a protocol-specific packet checksum/CRC.
pub type ChecksumFn = fn(ReportKind, &[u8]) -> Result<(), String>;

/// Strict validator enforcing exact report lengths, allowed Report IDs, and checksum integrity.
pub struct FixedReportValidator {
    allowed_lengths: Vec<usize>,
    allowed_report_ids: Vec<u8>,
    checksum: Option<ChecksumFn>,
}

impl fmt::Debug for FixedReportValidator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FixedReportValidator")
            .field("allowed_lengths", &self.allowed_lengths)
            .field("allowed_report_ids", &self.allowed_report_ids)
            .field("has_checksum", &self.checksum.is_some())
            .finish()
    }
}

impl FixedReportValidator {
    /// Creates a validator requiring exact byte lengths and an allow-listed Report ID at byte 0.
    pub fn new(allowed_lengths: Vec<usize>, allowed_report_ids: Vec<u8>) -> Self {
        Self {
            allowed_lengths,
            allowed_report_ids,
            checksum: None,
        }
    }

    /// Attaches a protocol-specific checksum verification callback.
    pub fn with_checksum(mut self, checksum: ChecksumFn) -> Self {
        self.checksum = Some(checksum);
        self
    }
}

impl PacketValidator for FixedReportValidator {
    fn validate_outgoing(&self, kind: ReportKind, report: &[u8]) -> TransportResult<()> {
        if !self.allowed_lengths.contains(&report.len()) {
            let expected = self.allowed_lengths.first().copied().unwrap_or(0);
            return Err(TransportError::PacketLengthMismatch {
                actual: report.len(),
                expected,
            });
        }

        let Some(&report_id) = report.first() else {
            return Err(TransportError::PacketLengthMismatch {
                actual: 0,
                expected: self.allowed_lengths.first().copied().unwrap_or(1),
            });
        };

        if !self.allowed_report_ids.is_empty() && !self.allowed_report_ids.contains(&report_id) {
            return Err(TransportError::DisallowedReportId { report_id });
        }

        if let Some(check) = self.checksum {
            check(kind, report).map_err(TransportError::InvalidChecksum)?;
        }

        Ok(())
    }
}
