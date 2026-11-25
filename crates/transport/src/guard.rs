//! Safety wrapper enforcing packet validation, `--dry-run` suppression, and wire tracing.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::descriptor::HidDeviceDescriptor;
use crate::error::TransportResult;
use crate::packet::{
    NoOpValidator, PacketDirection, PacketOutcome, PacketTraceEntry, PacketValidator, ReportKind,
};
use crate::traits::{HidTransport, NoOpPacketSink, PacketSink};

/// Wraps any [`HidTransport`] to enforce AgnoPeriph's non-negotiable wire safety rules:
///
/// 1. Validates every outgoing write (`Output` and `Feature`) through `PacketValidator`
///    **before** any bytes reach the hardware (even in `dry_run` mode).
/// 2. When `dry_run` is enabled, suppresses physical HID writes and logs them as
///    [`PacketOutcome::DryRunSuppressed`].
/// 3. Emits every TX/RX frame to the configured [`PacketSink`] (CLI `--trace-hex` or UI drawer).
pub struct GuardedTransport<T: HidTransport> {
    inner: T,
    validator: Arc<dyn PacketValidator>,
    sink: Arc<dyn PacketSink>,
    dry_run: bool,
    sequence: AtomicU64,
}

impl<T: HidTransport> fmt::Debug for GuardedTransport<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedTransport")
            .field("descriptor", self.inner.descriptor())
            .field("dry_run", &self.dry_run)
            .field("sequence", &self.sequence.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> GuardedTransport<T> {
    /// Creates a new `GuardedTransport` with default `NoOpValidator` and `NoOpPacketSink`.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            validator: Arc::new(NoOpValidator),
            sink: Arc::new(NoOpPacketSink),
            dry_run: false,
            sequence: AtomicU64::new(1),
        }
    }

    /// Configures a strict length/Report-ID/checksum validator.
    pub fn with_validator(mut self, validator: Arc<dyn PacketValidator>) -> Self {
        self.validator = validator;
        self
    }

    /// Configures a packet trace observer sink.
    pub fn with_sink(mut self, sink: Arc<dyn PacketSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Enables or disables `--dry-run` write suppression.
    pub const fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Returns `true` when outgoing writes are suppressed by dry-run mode.
    pub const fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Returns a shared reference to the wrapped transport (useful in unit tests).
    pub const fn inner(&self) -> &T {
        &self.inner
    }

    /// Sends a strictly **read-only** query packet (e.g., `ReadBattery`, `ReadFlash`, `Root.Ping`).
    ///
    /// Unlike [`HidTransport::send_output_report`], read-only queries still transmit when
    /// `dry_run` is enabled so `probe --dry-run` and `set --dry-run` can read device state
    /// while suppressing all mutating writes. Packet validation and tracing still apply.
    pub fn send_query_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
        if let Err(err) = self.validator.validate_outgoing(ReportKind::Output, report) {
            self.record(
                ReportKind::Output,
                PacketDirection::HostToDevice,
                PacketOutcome::ValidationFailed,
                report,
                Some(err.to_string()),
            );
            return Err(err);
        }

        let written = self.inner.send_output_report(report)?;
        self.record(
            ReportKind::Output,
            PacketDirection::HostToDevice,
            PacketOutcome::Transmitted,
            report,
            Some("read-only query".to_owned()),
        );
        Ok(written)
    }

    /// Sends a strictly **read-only** Feature query report (still transmits in `dry_run` mode
    /// after passing [`PacketValidator`] and recording to [`PacketSink`]).
    pub fn send_query_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
        if let Err(err) = self
            .validator
            .validate_outgoing(ReportKind::Feature, report)
        {
            self.record(
                ReportKind::Feature,
                PacketDirection::HostToDevice,
                PacketOutcome::ValidationFailed,
                report,
                Some(err.to_string()),
            );
            return Err(err);
        }

        self.inner.send_feature_report(report)?;
        self.record(
            ReportKind::Feature,
            PacketDirection::HostToDevice,
            PacketOutcome::Transmitted,
            report,
            Some("read-only feature query".to_owned()),
        );
        Ok(())
    }

    fn next_seq(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn record(
        &self,
        kind: ReportKind,
        direction: PacketDirection,
        outcome: PacketOutcome,
        bytes: &[u8],
        note: Option<String>,
    ) {
        let mut entry = PacketTraceEntry::new(
            self.next_seq(),
            self.inner.descriptor().short_id(),
            kind,
            direction,
            outcome,
            bytes.to_vec(),
        );
        if let Some(note) = note {
            entry = entry.with_note(note);
        }
        self.sink.record(entry);
    }
}

impl<T: HidTransport> HidTransport for GuardedTransport<T> {
    fn descriptor(&self) -> &HidDeviceDescriptor {
        self.inner.descriptor()
    }

    fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
        if let Err(err) = self.validator.validate_outgoing(ReportKind::Output, report) {
            self.record(
                ReportKind::Output,
                PacketDirection::HostToDevice,
                PacketOutcome::ValidationFailed,
                report,
                Some(err.to_string()),
            );
            return Err(err);
        }

        if self.dry_run {
            self.record(
                ReportKind::Output,
                PacketDirection::HostToDevice,
                PacketOutcome::DryRunSuppressed,
                report,
                Some("write suppressed by dry-run mode".to_owned()),
            );
            return Ok(report.len());
        }

        let written = self.inner.send_output_report(report)?;
        self.record(
            ReportKind::Output,
            PacketDirection::HostToDevice,
            PacketOutcome::Transmitted,
            report,
            None,
        );
        Ok(written)
    }

    fn read_input_report(&mut self, buf: &mut [u8], timeout: Duration) -> TransportResult<usize> {
        let read_len = self.inner.read_input_report(buf, timeout)?;
        self.record(
            ReportKind::Input,
            PacketDirection::DeviceToHost,
            PacketOutcome::Received,
            &buf[..read_len],
            None,
        );
        Ok(read_len)
    }

    fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
        if let Err(err) = self
            .validator
            .validate_outgoing(ReportKind::Feature, report)
        {
            self.record(
                ReportKind::Feature,
                PacketDirection::HostToDevice,
                PacketOutcome::ValidationFailed,
                report,
                Some(err.to_string()),
            );
            return Err(err);
        }

        if self.dry_run {
            self.record(
                ReportKind::Feature,
                PacketDirection::HostToDevice,
                PacketOutcome::DryRunSuppressed,
                report,
                Some("feature write suppressed by dry-run mode".to_owned()),
            );
            return Ok(());
        }

        self.inner.send_feature_report(report)?;
        self.record(
            ReportKind::Feature,
            PacketDirection::HostToDevice,
            PacketOutcome::Transmitted,
            report,
            None,
        );
        Ok(())
    }

    fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        let read_len = self.inner.get_feature_report(buf)?;
        self.record(
            ReportKind::Feature,
            PacketDirection::DeviceToHost,
            PacketOutcome::Received,
            &buf[..read_len],
            None,
        );
        Ok(read_len)
    }
}
