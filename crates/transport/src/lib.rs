//! USB HID transport layer, safety validation guard, `--dry-run` wire logger,
//! native Windows `hidapi` backend, and deterministic `MockHidTransport`.

#![forbid(unsafe_code)]

pub mod descriptor;
pub mod error;
pub mod guard;
pub mod hidapi_backend;
pub mod mock;
pub mod packet;
pub mod traits;

pub use descriptor::{HidBusType, HidDeviceDescriptor};
pub use error::{TransportError, TransportResult};
pub use guard::GuardedTransport;
pub use hidapi_backend::{HidapiEnumerator, HidapiTransport};
pub use mock::{ExpectedWrite, MockHidTransport, RecordedWrite};
pub use packet::{
    ChecksumFn, FixedReportValidator, NoOpValidator, PacketDirection, PacketOutcome,
    PacketTraceEntry, PacketValidator, ReportKind, format_hex_spaced,
};
pub use traits::{
    HidEnumerator, HidTransport, MemoryPacketSink, NoOpPacketSink, PacketSink, TracingPacketSink,
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    fn sample_descriptor() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(0x046D, 0xC547, 0xFF00, 0x0002, 2, "mock://hid/logitech")
            .with_strings(
                Some("Logitech".to_owned()),
                Some("USB Receiver".to_owned()),
                Some("SN-001".to_owned()),
            )
    }

    fn verify_xor_last_byte(_kind: ReportKind, report: &[u8]) -> Result<(), String> {
        let Some((&last, body)) = report.split_last() else {
            return Err("empty packet".to_owned());
        };
        let expected = body.iter().fold(0u8, |acc, &b| acc ^ b);
        if last == expected {
            Ok(())
        } else {
            Err(format!("expected 0x{expected:02X}, got 0x{last:02X}"))
        }
    }

    #[test]
    fn guarded_dry_run_validates_and_suppresses_physical_writes() {
        let mock = MockHidTransport::new(sample_descriptor());
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let validator = Arc::new(
            FixedReportValidator::new(vec![4], vec![0x08]).with_checksum(verify_xor_last_byte),
        );

        let mut guarded = GuardedTransport::new(mock)
            .with_validator(validator)
            .with_sink(sink.clone())
            .with_dry_run(true);

        // Valid packet: 0x08 ^ 0x01 ^ 0x02 == 0x0B.
        let valid_pkt = [0x08, 0x01, 0x02, 0x0B];
        let written = guarded.send_output_report(&valid_pkt).unwrap();
        assert_eq!(written, 4);

        // Underlying transport must have received ZERO writes in dry-run mode.
        assert_eq!(guarded.inner().recorded_writes(), []);

        // Bad checksum must still fail even in dry-run mode.
        let bad_checksum = [0x08, 0x01, 0x02, 0xFF];
        assert!(matches!(
            guarded.send_output_report(&bad_checksum),
            Err(TransportError::InvalidChecksum(_))
        ));

        // Wrong report ID must fail.
        let bad_report_id = [0x10, 0x01, 0x02, 0x13];
        assert!(matches!(
            guarded.send_feature_report(&bad_report_id),
            Err(TransportError::DisallowedReportId { report_id: 0x10 })
        ));

        // Wrong packet length must fail.
        let bad_len = [0x08, 0x01, 0x09];
        assert!(matches!(
            guarded.send_output_report(&bad_len),
            Err(TransportError::PacketLengthMismatch {
                actual: 3,
                expected: 4
            })
        ));

        let trace = sink.snapshot();
        assert_eq!(trace.len(), 4);
        assert_eq!(trace[0].outcome, PacketOutcome::DryRunSuppressed);
        assert_eq!(trace[0].hex_spaced(), "08 01 02 0B");
        assert_eq!(trace[1].outcome, PacketOutcome::ValidationFailed);
        assert_eq!(trace[2].outcome, PacketOutcome::ValidationFailed);
        assert_eq!(trace[3].outcome, PacketOutcome::ValidationFailed);
    }

    #[test]
    fn guarded_live_mode_transmits_and_records_tx_and_rx() {
        let mut mock = MockHidTransport::new(sample_descriptor());
        let tx_pkt = vec![0x10, 0xFF, 0x00, 0x1A, 0x00, 0x00, 0x00];
        let rx_pkt = vec![0x10, 0xFF, 0x00, 0x1A, 0x04, 0x02, 0x00];
        mock.expect_write(ReportKind::Output, tx_pkt.clone());
        mock.push_input_response(rx_pkt.clone());

        let sink = Arc::new(MemoryPacketSink::bounded(16));
        let validator = Arc::new(FixedReportValidator::new(vec![7, 20], vec![0x10, 0x11]));

        let mut guarded = GuardedTransport::new(mock)
            .with_validator(validator)
            .with_sink(sink.clone())
            .with_dry_run(false);

        assert_eq!(guarded.send_output_report(&tx_pkt).unwrap(), 7);
        let mut buf = [0u8; 7];
        assert_eq!(
            guarded
                .read_input_report(&mut buf, Duration::from_millis(50))
                .unwrap(),
            7
        );
        assert_eq!(buf.as_slice(), rx_pkt.as_slice());
        assert_eq!(guarded.inner().remaining_expected_writes(), 0);

        let trace = sink.snapshot();
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[0].direction, PacketDirection::HostToDevice);
        assert_eq!(trace[0].outcome, PacketOutcome::Transmitted);
        assert_eq!(trace[1].direction, PacketDirection::DeviceToHost);
        assert_eq!(trace[1].outcome, PacketOutcome::Received);
    }

    #[test]
    fn mock_transport_detects_mismatched_writes_and_handles_feature_reports() {
        let mut mock = MockHidTransport::new(sample_descriptor());
        mock.expect_write(ReportKind::Feature, vec![0x08, 0x04]);
        mock.push_feature_response(vec![0x08, 0x04, 0x55]);

        let err = mock.send_feature_report(&[0x08, 0x99]).unwrap_err();
        assert!(matches!(err, TransportError::UnexpectedMockPacket { .. }));

        let mut buf = [0u8; 8];
        let len = mock.get_feature_report(&mut buf).unwrap();
        assert_eq!(&buf[..len], &[0x08, 0x04, 0x55]);
    }
}
