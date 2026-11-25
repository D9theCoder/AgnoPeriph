//! Error types for the AgnoPeriph USB HID transport and safety guard layer.

use thiserror::Error;

/// Convenience result alias for transport operations.
pub type TransportResult<T> = Result<T, TransportError>;

/// Errors produced during HID enumeration, safety validation, or wire I/O.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Failed to initialize the native OS HID subsystem.
    #[error("failed to initialize HID API: {0}")]
    InitFailed(String),

    /// Failed to open a specific HID interface path.
    #[error("failed to open HID interface '{path}': {reason}")]
    OpenFailed { path: String, reason: String },

    /// Underlying `hidapi` read/write error.
    #[error("HID I/O error: {0}")]
    Io(String),

    /// An outgoing packet did not match the driver's strict length requirement.
    #[error("packet length mismatch: got {actual} bytes, expected {expected} bytes")]
    PacketLengthMismatch { actual: usize, expected: usize },

    /// An outgoing packet used an unapproved Report ID.
    #[error("disallowed HID report ID 0x{report_id:02X}")]
    DisallowedReportId { report_id: u8 },

    /// An outgoing packet failed checksum/CRC verification before transmission.
    #[error("packet checksum validation failed: {0}")]
    InvalidChecksum(String),

    /// A read timed out with zero bytes received.
    #[error("HID read timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },

    /// A mock transport received a write that did not match the next scripted expectation.
    #[error("mock transport unexpected write: expected {expected}, got {actual}")]
    UnexpectedMockPacket { expected: String, actual: String },

    /// A mock transport ran out of scripted responses or expectations.
    #[error("mock transport queue exhausted for {operation}")]
    MockQueueEmpty { operation: &'static str },
}

impl From<TransportError> for agnoperiph_core::CoreError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value.to_string())
    }
}
