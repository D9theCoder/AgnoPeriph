//! Deterministic offline `MockHidTransport` for protocol unit testing and packet replay.

use std::collections::VecDeque;
use std::time::Duration;

use crate::descriptor::HidDeviceDescriptor;
use crate::error::{TransportError, TransportResult};
use crate::packet::{ReportKind, format_hex_spaced};
use crate::traits::HidTransport;

/// Recorded outgoing write captured by [`MockHidTransport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedWrite {
    pub kind: ReportKind,
    pub bytes: Vec<u8>,
}

/// Optional scripted write expectation verified on each outgoing report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedWrite {
    pub kind: ReportKind,
    pub bytes: Vec<u8>,
}

/// Deterministic offline HID transport for mock-first driver testing.
///
/// Drivers can enqueue expected writes and canned input/feature responses, then
/// assert exact wire frames without plugging in physical peripherals.
#[derive(Debug)]
pub struct MockHidTransport {
    descriptor: HidDeviceDescriptor,
    expected_writes: VecDeque<ExpectedWrite>,
    strict_writes: bool,
    input_responses: VecDeque<Vec<u8>>,
    feature_responses: VecDeque<Vec<u8>>,
    recorded_writes: Vec<RecordedWrite>,
}

impl MockHidTransport {
    /// Creates a new `MockHidTransport` that records all writes without strict expectation checks.
    pub fn new(descriptor: HidDeviceDescriptor) -> Self {
        Self {
            descriptor,
            expected_writes: VecDeque::new(),
            strict_writes: false,
            input_responses: VecDeque::new(),
            feature_responses: VecDeque::new(),
            recorded_writes: Vec::new(),
        }
    }

    /// Enables strict mode: every outgoing write must match the next queued [`ExpectedWrite`].
    pub const fn with_strict_writes(mut self, strict: bool) -> Self {
        self.strict_writes = strict;
        self
    }

    /// Enqueues an expected outgoing packet (automatically enables strict write matching).
    pub fn expect_write(&mut self, kind: ReportKind, bytes: Vec<u8>) {
        self.strict_writes = true;
        self.expected_writes
            .push_back(ExpectedWrite { kind, bytes });
    }

    /// Enqueues a canned input report returned by the next [`HidTransport::read_input_report`].
    pub fn push_input_response(&mut self, bytes: Vec<u8>) {
        self.input_responses.push_back(bytes);
    }

    /// Enqueues a canned feature report returned by the next [`HidTransport::get_feature_report`].
    pub fn push_feature_response(&mut self, bytes: Vec<u8>) {
        self.feature_responses.push_back(bytes);
    }

    /// Returns all outgoing writes transmitted to this mock transport.
    pub fn recorded_writes(&self) -> &[RecordedWrite] {
        &self.recorded_writes
    }

    /// Returns the number of unconsumed write expectations remaining.
    pub fn remaining_expected_writes(&self) -> usize {
        self.expected_writes.len()
    }

    fn handle_write(&mut self, kind: ReportKind, report: &[u8]) -> TransportResult<()> {
        if self.strict_writes {
            let expected =
                self.expected_writes
                    .pop_front()
                    .ok_or(TransportError::MockQueueEmpty {
                        operation: "expected_writes",
                    })?;
            if expected.kind != kind || expected.bytes != report {
                return Err(TransportError::UnexpectedMockPacket {
                    expected: format!("{} [{}]", expected.kind, format_hex_spaced(&expected.bytes)),
                    actual: format!("{kind} [{}]", format_hex_spaced(report)),
                });
            }
        }
        self.recorded_writes.push(RecordedWrite {
            kind,
            bytes: report.to_vec(),
        });
        Ok(())
    }
}

impl HidTransport for MockHidTransport {
    fn descriptor(&self) -> &HidDeviceDescriptor {
        &self.descriptor
    }

    fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
        self.handle_write(ReportKind::Output, report)?;
        Ok(report.len())
    }

    fn read_input_report(&mut self, buf: &mut [u8], timeout: Duration) -> TransportResult<usize> {
        let Some(next) = self.input_responses.pop_front() else {
            return Err(TransportError::Timeout {
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            });
        };
        let copy_len = next.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&next[..copy_len]);
        Ok(copy_len)
    }

    fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
        self.handle_write(ReportKind::Feature, report)
    }

    fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        let next = self
            .feature_responses
            .pop_front()
            .ok_or(TransportError::MockQueueEmpty {
                operation: "feature_responses",
            })?;
        let copy_len = next.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&next[..copy_len]);
        Ok(copy_len)
    }
}
