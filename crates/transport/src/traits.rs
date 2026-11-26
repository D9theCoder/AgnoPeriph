//! Core traits for HID transports, device enumeration, and packet trace sinks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::descriptor::HidDeviceDescriptor;
use crate::error::TransportResult;
use crate::packet::PacketTraceEntry;

/// Low-level host-to-device USB HID report transport.
pub trait HidTransport: Send + Sync {
    /// Returns metadata for the opened HID interface.
    fn descriptor(&self) -> &HidDeviceDescriptor;

    /// Sends an output report (`hid_write`). Byte 0 is the HID Report ID (`0x00` if unnumbered).
    fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize>;

    /// Reads an input report with a timeout (`hid_read_timeout`).
    fn read_input_report(&mut self, buf: &mut [u8], timeout: Duration) -> TransportResult<usize>;

    /// Discards any stale or unsolicited input reports currently queued in the OS HID
    /// input buffer without blocking, returning how many reports were drained.
    ///
    /// Drivers call this before starting a request/reply exchange so unread notifications
    /// accumulated while the device handle sat idle cannot push the real reply past the
    /// read attempt limit.
    fn drain_input_reports(&mut self) -> TransportResult<usize> {
        Ok(0)
    }

    /// Sends a HID Feature report (`SET_REPORT`). Byte 0 is the Report ID.
    fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()>;

    /// Reads a HID Feature report (`GET_REPORT`). Byte 0 of `buf` selects the Report ID.
    fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize>;
}

/// Enumerator for discovering and opening host HID interfaces.
pub trait HidEnumerator: Send + Sync {
    /// Lists all currently attached HID interfaces visible to the host OS.
    fn enumerate(&self) -> TransportResult<Vec<HidDeviceDescriptor>>;

    /// Opens a specific HID interface by its OS device path.
    fn open_path(&self, descriptor: &HidDeviceDescriptor)
    -> TransportResult<Box<dyn HidTransport>>;
}

/// Observer receiving every HID frame processed by [`crate::GuardedTransport`].
pub trait PacketSink: Send + Sync {
    fn record(&self, entry: PacketTraceEntry);
}

/// Sink that discards packet traces.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpPacketSink;

impl PacketSink for NoOpPacketSink {
    fn record(&self, _entry: PacketTraceEntry) {}
}

/// Thread-safe in-memory ring/vector sink for unit tests and the GPUI live packet inspector.
#[derive(Debug, Clone)]
pub struct MemoryPacketSink {
    entries: Arc<Mutex<Vec<PacketTraceEntry>>>,
    capacity: Option<usize>,
}

impl Default for MemoryPacketSink {
    fn default() -> Self {
        Self::unbounded()
    }
}

impl MemoryPacketSink {
    /// Creates an unbounded trace buffer (ideal for deterministic unit tests).
    pub fn unbounded() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            capacity: None,
        }
    }

    /// Creates a bounded trace buffer retaining at most `max_entries` recent frames.
    pub fn bounded(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::with_capacity(max_entries))),
            capacity: Some(max_entries.max(1)),
        }
    }

    /// Returns a cloned snapshot of all recorded trace entries.
    pub fn snapshot(&self) -> Vec<PacketTraceEntry> {
        self.entries.lock().map_or_default(|guard| guard.clone())
    }

    /// Clears all recorded entries.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }
    }
}

impl PacketSink for MemoryPacketSink {
    fn record(&self, entry: PacketTraceEntry) {
        if let Ok(mut guard) = self.entries.lock() {
            if let Some(cap) = self.capacity
                && guard.len() >= cap
            {
                let overflow = guard.len() + 1 - cap;
                guard.drain(0..overflow);
            }
            guard.push(entry);
        }
    }
}

/// Sink that emits every HID frame through `tracing` with uppercase hex payloads.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingPacketSink;

impl PacketSink for TracingPacketSink {
    fn record(&self, entry: PacketTraceEntry) {
        tracing::info!(
            target: "agnoperiph::wire",
            seq = entry.sequence,
            device = %entry.device_label,
            kind = %entry.kind,
            dir = %entry.direction,
            outcome = %entry.outcome,
            len = entry.bytes.len(),
            hex = %entry.hex_spaced(),
            note = entry.note.as_deref().unwrap_or(""),
            "HID packet"
        );
    }
}
