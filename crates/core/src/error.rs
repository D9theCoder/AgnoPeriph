//! Error types for the brand-agnostic core abstraction layer.

use thiserror::Error;

/// Convenience result alias for `agnoperiph-core` operations.
pub type CoreResult<T> = Result<T, CoreError>;

/// Errors returned by domain validation, capability queries, and peripheral drivers.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A caller supplied a value outside the device's declared capability bounds.
    #[error("value out of range for {field}: got {actual}, expected {expected}")]
    OutOfRange {
        field: &'static str,
        actual: String,
        expected: String,
    },

    /// A domain invariant failed validation before reaching a wire encoder.
    #[error("validation failed: {0}")]
    Validation(String),

    /// The device does not advertise or support the requested capability or feature.
    #[error("unsupported capability: {0}")]
    UnsupportedCapability(&'static str),

    /// A protocol-level encoding, decoding, or checksum mismatch occurred.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Underlying HID transport failure surfaced by a driver.
    #[error("transport error: {0}")]
    Transport(String),

    /// Requested peripheral was not found in the registry.
    #[error("device not found: {0}")]
    DeviceNotFound(String),

    /// Target peripheral (for example, a sleeping wireless mouse behind a dongle) is offline.
    #[error("device not connected or asleep: {0}")]
    NotConnected(String),

    /// Requested operation is not valid in the device's current mode or state.
    #[error("invalid device state: {0}")]
    InvalidState(String),
}
