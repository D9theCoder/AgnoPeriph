use super::*;
use agnoperiph_core::{ChargingState, DeviceRegistry, DpiValue, HidMatchDescriptor};
use agnoperiph_transport::{
    HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
};

use crate::protocol_a5_gen1::{self as a5p, Command, command as a5c};
use crate::protocol_v2::{self as v2p, command as v2c};
use crate::protocol_v3::{self as v3p, command as v3c};

fn descriptor(
    vid: u16,
    pid: u16,
    usage_page: u16,
    usage: u16,
    product: &str,
) -> HidDeviceDescriptor {
    HidDeviceDescriptor::new(
        vid,
        pid,
        usage_page,
        usage,
        1,
        format!("mock://{vid:04x}:{pid:04x}"),
    )
    .with_strings(Some("MCHOSE".to_owned()), Some(product.to_owned()), None)
}

fn suppressed_count(sink: &MemoryPacketSink) -> usize {
    sink.snapshot()
        .into_iter()
        .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
        .count()
}

// ── V2 ───────────────────────────────────────────────────────────────────────

fn v2_reply(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let channel = v2p::channel_for(cmd).unwrap();
    let mut body = [0u8; 63];
    body[..payload.len()].copy_from_slice(payload);
    let mut raw = vec![channel.report_id(), cmd ^ 0xFF];
    raw.extend(body.iter().map(|b| b ^ 0xFF));
    raw
}

/// V2 reads require two identical consecutive replies.
fn push_v2(mock: &mut MockHidTransport, cmd: u8, payload: &[u8]) {
    mock.push_feature_response(v2_reply(cmd, payload));
    mock.push_feature_response(v2_reply(cmd, payload));
}

fn v2_config(wired: u8, wireless: u8, sensor: u8, profile: u8) -> Vec<u8> {
    let mut payload = vec![0u8; 63];
    payload[0] = profile;
    payload[1] = wired;
    payload[2] = wireless;
    for (ix, dpi) in [400u16, 800, 1600, 3200, 6400, 12_800].iter().enumerate() {
        payload[4 + ix * 2..6 + ix * 2].copy_from_slice(&dpi.to_le_bytes());
    }
    payload[16] = 4;
    payload[17] = sensor;
    payload[18] = 4;
    payload[19] = 5;
    // Button / macro bytes the driver does not understand and must echo verbatim.
    for (ix, slot) in payload.iter_mut().enumerate().skip(20) {
        *slot = u8::try_from(ix).unwrap() ^ 0x5A;
    }
    payload
}

fn v2_battery(mouse_pid: u16, percent: u8, charging: bool) -> Vec<u8> {
    let mut payload = vec![0u8; 11];
    payload[0..2].copy_from_slice(&0x3837u16.to_le_bytes());
    payload[2..4].copy_from_slice(&mouse_pid.to_le_bytes());
    payload[8] = 0x10;
    payload[9] = percent;
    payload[10] = u8::from(charging);
    payload
}

fn v2_tokens(frame: &[u8]) -> Vec<u8> {
    frame[1..].iter().map(|b| b ^ 0xFF).collect()
}

fn v2_writes_with(dev: &MchoseDevice<MockHidTransport>, cmd: u8) -> Vec<Vec<u8>> {
    dev.transport()
        .inner()
        .recorded_writes()
        .iter()
        .filter(|w| w.bytes[1] ^ 0xFF == cmd)
        .map(|w| v2_tokens(&w.bytes))
        .collect()
}

#[test]
fn v2_receiver_reads_and_read_modify_writes_preserve_payload() {
    let mut mock =
        MockHidTransport::new(descriptor(0x3837, 0x100B, 0xFF01, 0x0001, "2.4G Receiver"));
    // Wired half: 1000 Hz / stage 1. Wireless half: 8000 Hz / stage 2. Bit 5 unexplained.
    let config = v2_config(0x21, 0x52, 0b0010_0001, 1);
    let after_rate = v2_config(0x21, 0x22, 0b0010_0001, 1);
    let after_lod = v2_config(0x21, 0x22, 0b0010_0010, 1);
    let after_motion = v2_config(0x21, 0x22, 0b0011_0010, 1);

    // open(): firmware + battery (mouse PID resolves the model).
    push_v2(&mut mock, v2c::VERSION, b"\x085.46.2.4");
    push_v2(&mut mock, v2c::BATTERY, &v2_battery(0x4021, 87, true));
    // read_pointer_state()
    push_v2(&mut mock, v2c::CONFIG, &config);
    // set_polling_rate(): read, (write), confirm
    push_v2(&mut mock, v2c::CONFIG, &config);
    push_v2(&mut mock, v2c::CONFIG, &after_rate);
    // set_lod(): (write), confirm
    push_v2(&mut mock, v2c::CONFIG, &after_lod);
    // set_motion_sync(): read current LOD, (write), confirm
    push_v2(&mut mock, v2c::CONFIG, &after_lod);
    push_v2(&mut mock, v2c::CONFIG, &after_motion);
    // read_battery()
    push_v2(&mut mock, v2c::BATTERY, &v2_battery(0x4021, 87, true));
    // read_profile_state(): config + three names
    push_v2(&mut mock, v2c::CONFIG, &after_motion);
    for (ix, name) in [(0u8, "Config 1"), (1, "Config 2"), (2, "Config 3")] {
        let mut payload = vec![ix];
        payload.extend_from_slice(name.as_bytes());
        push_v2(&mut mock, v2c::READ_PROFILE_NAME, &payload);
    }
    // set_active_profile(): (write), confirm
    push_v2(
        &mut mock,
        v2c::CONFIG,
        &v2_config(0x21, 0x22, 0b0011_0010, 2),
    );

    let mut dev = MchoseDevice::open(mock, false, None).unwrap();
    assert_eq!(dev.protocol_family(), ProtocolFamily::V2);
    assert_eq!(dev.info().model_name, "MCHOSE A7 V2 Ultra+");
    assert_eq!(dev.info().connection, ConnectionKind::WirelessDongle24Ghz);
    assert_eq!(dev.info().firmware_version.as_deref(), Some("5.46.2.4"));
    assert_eq!(dev.pointer_capabilities().dpi_max, 42_000);

    let snap = dev.read_pointer_state().unwrap();
    assert_eq!(snap.polling_rate, PollingRateHz::Hz8000);
    assert_eq!(snap.active_dpi_stage, 2);
    assert_eq!(snap.dpi_stages.len(), 4);
    assert_eq!(snap.dpi_stages[3].x.value(), 3200);
    assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(10)));
    assert_eq!(snap.debounce_ms, Some(DebounceMs::new(4)));
    assert_eq!(snap.motion_sync, Some(false));

    // Polling write touches only the wireless rate nibble; every other byte is echoed.
    dev.set_polling_rate(PollingRateHz::Hz1000).unwrap();
    let writes = v2_writes_with(&dev, v2c::WRITE_CONFIG);
    assert_eq!(writes.len(), 1);
    let tokens = &writes[0];
    assert_eq!(tokens.len(), 64);
    assert_eq!(tokens[2], 0x21, "wired half untouched");
    assert_eq!(
        tokens[3], 0x22,
        "wireless rate index 2 = 1000 Hz, stage kept"
    );
    for (ix, byte) in config.iter().enumerate() {
        if ix != 2 {
            assert_eq!(tokens[ix + 1], *byte, "config offset {ix} must be echoed");
        }
    }

    // Lift-off goes through 0x42 and is confirmed from the config sensor bits.
    dev.set_lod(LiftOffDistance::TenthsMm(20)).unwrap();
    let perf = v2_writes_with(&dev, v2c::SET_PERFORMANCE);
    assert_eq!(&perf[0][..10], &[0x42, 2, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(perf[0][10..20].iter().all(|b| *b == 0));

    // Toggle carries the current lift-off index and leaves the other toggles alone.
    dev.set_motion_sync(true).unwrap();
    let perf = v2_writes_with(&dev, v2c::SET_PERFORMANCE);
    assert_eq!(&perf[1][..5], &[0x42, 2, 0, 0, 1]);

    let battery = dev.read_battery().unwrap();
    assert_eq!(battery.percentage, Some(87));
    assert_eq!(battery.state, ChargingState::Charging);
    assert!(battery.is_wireless_connected());

    let profiles = dev.read_profile_state().unwrap();
    assert_eq!(profiles.active_slot, ProfileSlot::new(1));
    assert_eq!(profiles.profiles[2].label.as_deref(), Some("Config 3"));

    dev.set_active_profile(ProfileSlot::new(2)).unwrap();
    let profile_writes = v2_writes_with(&dev, v2c::SET_PROFILE);
    assert_eq!(&profile_writes[0][..2], &[0x58, 2]);
    assert!(dev.set_active_profile(ProfileSlot::new(3)).is_err());
}

#[test]
fn v2_wired_dry_run_validates_but_never_transmits_writes() {
    let mut mock = MockHidTransport::new(descriptor(
        0x3837,
        0x4018,
        0xFF01,
        0x0001,
        "MCHOSE A7 V2 Pro",
    ));
    push_v2(&mut mock, v2c::VERSION, b"\x035.1");
    let config = v2_config(0x21, 0x52, 0x01, 0);
    // polling, dpi stages, debounce, motion sync (current LOD) each read the config once.
    for _ in 0..4 {
        push_v2(&mut mock, v2c::CONFIG, &config);
    }
    let sink = Arc::new(MemoryPacketSink::unbounded());
    let mut dev = MchoseDevice::open(mock, true, Some(sink.clone())).unwrap();
    assert_eq!(dev.info().connection, ConnectionKind::WiredUsb);
    assert_eq!(dev.info().model_name, "MCHOSE A7 V2 Pro");

    dev.set_polling_rate(PollingRateHz::Hz2000).unwrap();
    let stages = [
        DpiStage::new(0, DpiValue::new(800).unwrap()),
        DpiStage::new(1, DpiValue::new(1600).unwrap()),
    ];
    dev.set_dpi_stages(&stages, 1).unwrap();
    dev.set_debounce_ms(DebounceMs::new(2)).unwrap();
    dev.set_motion_sync(true).unwrap();
    dev.set_lod(LiftOffDistance::TenthsMm(10)).unwrap();
    dev.set_active_profile(ProfileSlot::new(1)).unwrap();

    for write in dev.transport().inner().recorded_writes() {
        let cmd = write.bytes[1] ^ 0xFF;
        assert!(
            !v2p::is_mutating(cmd),
            "mutating 0x{cmd:02X} reached the device"
        );
    }
    assert_eq!(suppressed_count(&sink), 6);
    let dpi_write = sink
        .snapshot()
        .into_iter()
        .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
        .map(|e| v2_tokens(&e.bytes))
        .find(|t| t[0] == v2c::WRITE_CONFIG && t[17] == 2)
        .unwrap();
    assert_eq!(&dpi_write[5..9], &[0x20, 0x03, 0x40, 0x06]);
    assert_eq!(dpi_write[2], 0x21, "wired rate kept, active stage 1");
    assert_eq!(&dpi_write[9..17], &config[8..16], "unused stages echoed");

    // Out-of-range values never reach an encoder.
    assert!(dev.set_debounce_ms(DebounceMs::new(21)).is_err());
    assert!(dev.set_lod(LiftOffDistance::TenthsMm(7)).is_err());
}

#[test]
fn v2_short_config_read_never_becomes_a_partial_write() {
    let mut mock = MockHidTransport::new(descriptor(
        0x3837,
        0x4018,
        0xFF01,
        0x0001,
        "MCHOSE A7 V2 Pro",
    ));
    push_v2(&mut mock, v2c::VERSION, b"5.1");
    let full = v2_config(0x21, 0x52, 0x01, 0);
    let mut short = v2_reply(v2c::CONFIG, &full);
    short.truncate(2 + 30);
    mock.push_feature_response(short.clone());
    mock.push_feature_response(short);
    let mut dev = MchoseDevice::open(mock, false, None).unwrap();

    let err = dev.set_polling_rate(PollingRateHz::Hz1000).unwrap_err();
    assert!(matches!(err, CoreError::Protocol(_)), "{err}");
    assert_eq!(v2_writes_with(&dev, v2c::WRITE_CONFIG).len(), 0);
}

// ── V3 ───────────────────────────────────────────────────────────────────────

fn v3_reply(cmd: u16, data: &[u8]) -> Vec<u8> {
    v3p::encode_frame(cmd, data).unwrap().to_vec()
}

fn v3_device_info(profile_count: u8, connected: bool, percent: u8) -> Vec<u8> {
    let mut data = vec![0u8; 14];
    data[0..2].copy_from_slice(&0x3837u16.to_le_bytes());
    // A7 V3 Ultra+ reports the A5 V3 Ultra+ id here; the product string must win.
    data[2..4].copy_from_slice(&0x4026u16.to_le_bytes());
    data[4] = profile_count;
    data[10] = u8::from(connected);
    data[12] = percent;
    data
}

const V3_SENSOR: u8 = 0b1001_0110; // glass, motion sync, ripple, mode 2

fn v3_settings(profile: u8, wired: u8, sensor: u8) -> Vec<u8> {
    let mut data = vec![profile, wired, 0x62, 5, 0, sensor, 0xF1, 8, 8];
    data.extend_from_slice(&[8; 10]);
    data
}

fn v3_table(profile: u8, count: u8, active: u8) -> Vec<u8> {
    let mut data = vec![profile, 0, count, active, 0];
    for dpi in [400u16, 800, 1600, 3200, 6400, 12_800] {
        data.extend_from_slice(&dpi.to_le_bytes());
    }
    data
}

fn v3_output_writes(dev: &MchoseDevice<MockHidTransport>, cmd: u16) -> Vec<Vec<u8>> {
    dev.transport()
        .inner()
        .recorded_writes()
        .iter()
        .filter(|w| u16::from_le_bytes([w.bytes[4], w.bytes[5]]) == cmd)
        .map(|w| {
            let len = usize::from(w.bytes[3]);
            w.bytes[8..8 + len].to_vec()
        })
        .collect()
}

/// Every recorded frame must pass the V3 validator (checksum + allow-list).
fn assert_all_writes_pass_v3_validator(dev: &MchoseDevice<MockHidTransport>) {
    use agnoperiph_transport::PacketValidator;
    let validator = v3p::v3_validator();
    for write in dev.transport().inner().recorded_writes() {
        assert!(
            validator
                .validate_outgoing(write.kind, &write.bytes)
                .is_ok()
        );
    }
}

#[test]
fn v3_wired_five_step_model_reads_and_writes_blocks() {
    let mut mock = MockHidTransport::new(descriptor(
        0x3837,
        0x4033,
        0xFF01,
        0x0001,
        "MCHOSE A7 V3 Ultra+",
    ));
    // open()
    mock.push_input_response(v3_reply(
        v3c::READ_DEVICE_INFO,
        &v3_device_info(4, true, 66),
    ));
    // read_pointer_state(): unrelated motion report first, then settings, table, lift-off.
    mock.push_input_response(vec![0x01, 0x00, 0x05, 0xFB, 0x00]);
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x31, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(v3c::READ_DPI, &v3_table(1, 4, 2)));
    mock.push_input_response(v3_reply(v3c::READ_LIFT_OFF, &[3]));
    // set_polling_rate(8000): read, (write), confirm
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x31, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x61, V3_SENSOR),
    ));
    // set_lod(0.9 mm) on a 0x0109 model: settings, (write), confirm via 0x0009
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x61, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(v3c::READ_LIFT_OFF, &[1]));
    // set_motion_sync(false)
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x61, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x61, V3_SENSOR & !v3p::SENSOR_MOTION_SYNC),
    ));
    // set_active_dpi_stage(0): settings (profile), table, (write), table
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(1, 0x61, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(v3c::READ_DPI, &v3_table(1, 4, 2)));
    mock.push_input_response(v3_reply(v3c::READ_DPI, &v3_table(1, 4, 0)));
    // read_battery()
    mock.push_input_response(v3_reply(
        v3c::READ_DEVICE_INFO,
        &v3_device_info(4, true, 66),
    ));

    let mut dev = MchoseDevice::open(mock, false, None).unwrap();
    assert_eq!(dev.protocol_family(), ProtocolFamily::V3);
    assert_eq!(dev.info().model_name, "MCHOSE A7 V3 Ultra+");
    assert_eq!(dev.pointer_capabilities().dpi_max, 50_000);
    assert_eq!(dev.pointer_capabilities().supported_lods.len(), 5);
    assert_eq!(dev.profile_capabilities().slot_count, 4);

    let snap = dev.read_pointer_state().unwrap();
    assert_eq!(
        snap.polling_rate,
        PollingRateHz::Hz1000,
        "slot 3 = option 2"
    );
    assert_eq!(snap.active_dpi_stage, 2);
    assert_eq!(snap.dpi_stages.len(), 4);
    assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(14)));
    assert_eq!(snap.motion_sync, Some(true));
    assert_eq!(snap.ripple_control, Some(true));
    assert_eq!(snap.angle_snapping, Some(false));
    assert_eq!(snap.debounce_ms, Some(DebounceMs::new(8)));

    dev.set_polling_rate(PollingRateHz::Hz8000).unwrap();
    let written = &v3_output_writes(&dev, v3c::WRITE_SETTINGS)[0];
    let mut expected = v3_settings(1, 0x61, V3_SENSOR);
    expected[1] = 0x61;
    assert_eq!(
        written, &expected,
        "only wired rate nibble changed; extra echoed"
    );

    dev.set_lod(LiftOffDistance::TenthsMm(9)).unwrap();
    assert_eq!(v3_output_writes(&dev, v3c::WRITE_LIFT_OFF)[0], vec![1, 1]);

    dev.set_motion_sync(false).unwrap();
    let written = &v3_output_writes(&dev, v3c::WRITE_SETTINGS)[1];
    assert_eq!(
        written[5], 0b1000_0110,
        "glass, ripple and mode bits preserved"
    );

    dev.set_active_dpi_stage(0).unwrap();
    let table = &v3_output_writes(&dev, v3c::WRITE_DPI)[0];
    assert_eq!(
        &table[..5],
        &[1, 0, 0, 4, 0],
        "write order: profile, axis, sep_y, count, active"
    );
    assert_eq!(&table[5..], &v3_table(1, 4, 2)[5..], "stages echoed");
    assert!(dev.set_active_dpi_stage(6).is_err());

    let battery = dev.read_battery().unwrap();
    assert_eq!(battery.percentage, Some(66));
    assert_eq!(battery.state, ChargingState::Discharging);

    assert_all_writes_pass_v3_validator(&dev);
}

#[test]
fn v3_three_step_model_writes_lod_into_sensor_bits_5_6() {
    let mut mock = MockHidTransport::new(descriptor(
        0x3837,
        0x4031,
        0xFF01,
        0x0001,
        "MCHOSE A7 V3 Pro",
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_DEVICE_INFO,
        &v3_device_info(4, true, 50),
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(0, 0x31, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(0, 0x31, V3_SENSOR | 0x40),
    ));
    let mut dev = MchoseDevice::open(mock, false, None).unwrap();
    dev.set_lod(LiftOffDistance::TenthsMm(20)).unwrap();
    assert_eq!(v3_output_writes(&dev, v3c::WRITE_LIFT_OFF).len(), 0);
    let written = &v3_output_writes(&dev, v3c::WRITE_SETTINGS)[0];
    assert_eq!(written[5], V3_SENSOR | 0x40);
}

#[test]
fn v3_receiver_busy_rejection_and_unresolved_model() {
    let busy = v3_reply(v3c::READ_DEVICE_INFO, &[0xFF]);
    let mut mock = MockHidTransport::new(descriptor(0x3837, 0x1018, 0xFF01, 0x0001, "8K Receiver"));
    // open(): busy twice -> not connected, model unresolved, profile count unknown.
    mock.push_input_response(busy.clone());
    mock.push_input_response(busy.clone());
    // read_battery(): busy twice -> disconnected status.
    mock.push_input_response(busy.clone());
    mock.push_input_response(busy);
    // refresh(): mouse now awake -> re-probe resolves profiles; device info ok, version refused.
    for _ in 0..2 {
        mock.push_input_response(v3_reply(
            v3c::READ_DEVICE_INFO,
            &v3_device_info(2, true, 40),
        ));
    }
    let mut rejection = vec![0u8; 64];
    rejection[0] = v3p::REPORT_ID;
    rejection[1] = 0x01;
    rejection[7] = 0xFF;
    mock.push_input_response(rejection);

    let mut dev = MchoseDevice::open(mock, false, None).unwrap();
    assert_eq!(dev.info().connection, ConnectionKind::WirelessDongle24Ghz);
    assert!(!dev.capabilities().has_profiles());
    assert!(dev.as_profiles().is_none());
    assert_eq!(dev.pointer_capabilities().supported_polling_rates.len(), 6);
    assert!(matches!(
        dev.set_lod(LiftOffDistance::TenthsMm(10)),
        Err(CoreError::UnsupportedCapability(_))
    ));

    let battery = dev.read_battery().unwrap();
    assert!(!battery.is_wireless_connected());
    assert_eq!(battery.percentage, None);

    dev.refresh().unwrap();
    assert_eq!(dev.info().firmware_version, None);
    assert!(dev.capabilities().has_profiles());
    assert_eq!(dev.profile_capabilities().slot_count, 2);
    // Falls back to the 0x0900 id (documented upstream as unreliable) to resolve a model.
    assert_eq!(dev.pointer_capabilities().supported_lods.len(), 3);
}

#[test]
fn v3_dry_run_suppresses_writes_and_factory_reset_is_blocked() {
    let mut mock = MockHidTransport::new(descriptor(
        0x3837,
        0x4035,
        0xFF01,
        0x0001,
        "MCHOSE A5 V3 Pro",
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_DEVICE_INFO,
        &v3_device_info(4, true, 50),
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(0, 0x31, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(
        v3c::READ_SETTINGS,
        &v3_settings(0, 0x31, V3_SENSOR),
    ));
    mock.push_input_response(v3_reply(v3c::READ_DPI, &v3_table(0, 4, 2)));
    let sink = Arc::new(MemoryPacketSink::unbounded());
    let mut dev = MchoseDevice::open(mock, true, Some(sink.clone())).unwrap();

    dev.set_polling_rate(PollingRateHz::Hz500).unwrap();
    let stages = [DpiStage::new(0, DpiValue::new(1600).unwrap())];
    dev.set_dpi_stages(&stages, 0).unwrap();

    for write in dev.transport().inner().recorded_writes() {
        let cmd = u16::from_le_bytes([write.bytes[4], write.bytes[5]]);
        assert!(
            !v3p::is_mutating(cmd),
            "mutating 0x{cmd:04X} reached the device"
        );
    }
    assert_eq!(suppressed_count(&sink), 2);

    // Factory reset is rejected before transmission, even in live mode.
    for dry_run in [false, true] {
        let mut guarded = GuardedTransport::new(MockHidTransport::new(descriptor(
            0x3837, 0x4035, 0xFF01, 0x0001, "x",
        )))
        .with_validator(Arc::new(v3p::v3_validator()))
        .with_dry_run(dry_run);
        let reset = v3p::encode_frame(v3c::FACTORY_RESET, &[0]).unwrap();
        let err = guarded.send_output_report(&reset).unwrap_err();
        assert!(err.to_string().contains("factory reset"));
        assert_eq!(guarded.inner().recorded_writes().len(), 0);
    }
}

// ── A5 Gen1 ──────────────────────────────────────────────────────────────────

fn a5_reply(cmd: Command, data: &[u8]) -> Vec<u8> {
    let mut raw = vec![0u8; 65];
    raw[1] = a5p::REPLY_MAGIC;
    raw[5] = cmd.page;
    raw[6] = cmd.cmd;
    raw[7..7 + data.len()].copy_from_slice(data);
    raw
}

fn a5_dpi_reply(stages: &[u16]) -> Vec<u8> {
    let mut data = vec![0, u8::try_from(stages.len()).unwrap()];
    for dpi in stages {
        let [hi, lo] = dpi.to_be_bytes();
        data.extend_from_slice(&[hi, lo, hi, lo]);
    }
    a5_reply(a5c::READ_DPI, &data)
}

#[test]
fn a5_gen1_wired_reads_and_writes() {
    let mut mock = MockHidTransport::new(descriptor(0x2023, 0xF019, 0xFFFF, 0x0002, "A5 Pro Max"));
    mock.push_feature_response(a5_reply(a5c::READ_FIRMWARE, &[1, 2, 3, 4]));
    // read_pointer_state()
    mock.push_feature_response(a5_reply(a5c::READ_PROFILE, &[2]));
    mock.push_feature_response(a5_dpi_reply(&[800, 1600, 3200]));
    mock.push_feature_response(a5_reply(a5c::READ_ACTIVE_STAGE, &[0, 2]));
    mock.push_feature_response(a5_reply(a5c::READ_POLLING, &[0x02]));
    mock.push_feature_response(a5_reply(a5c::READ_DEBOUNCE, &[0, 6]));
    mock.push_feature_response(a5_reply(a5c::READ_LOD, &[2]));
    mock.push_feature_response(a5_reply(a5c::READ_MOTION_SYNC, &[1]));
    mock.push_feature_response(a5_reply(a5c::READ_ANGLE_SNAP, &[0]));
    mock.push_feature_response(a5_reply(a5c::READ_RIPPLE, &[1]));
    // set_dpi_stages(): profile, ack dpi, ack active, confirm dpi + active
    mock.push_feature_response(a5_reply(a5c::READ_PROFILE, &[2]));
    mock.push_feature_response(a5_reply(a5p::dpi_write_command(10), &[]));
    mock.push_feature_response(a5_reply(a5c::SET_ACTIVE_STAGE, &[]));
    mock.push_feature_response(a5_dpi_reply(&[800, 26_000]));
    mock.push_feature_response(a5_reply(a5c::READ_ACTIVE_STAGE, &[0, 2]));
    // set_polling_rate(1000): ack, confirm
    mock.push_feature_response(a5_reply(a5c::SET_POLLING, &[]));
    mock.push_feature_response(a5_reply(a5c::READ_POLLING, &[0x01]));
    // read_battery(), read_profile_state()
    mock.push_feature_response(a5_reply(a5c::READ_BATTERY, &[1, 73]));
    mock.push_feature_response(a5_reply(a5c::READ_PROFILE, &[2]));

    let mut dev = MchoseDevice::open(mock, false, None).unwrap();
    assert_eq!(dev.protocol_family(), ProtocolFamily::A5Gen1);
    assert_eq!(dev.info().firmware_version.as_deref(), Some("01.02.03.04"));
    assert_eq!(dev.info().connection, ConnectionKind::WiredUsb);

    let snap = dev.read_pointer_state().unwrap();
    assert_eq!(snap.dpi_stages.len(), 3);
    assert_eq!(snap.dpi_stages[2].x.value(), 3200);
    assert_eq!(snap.active_dpi_stage, 1);
    assert_eq!(snap.polling_rate, PollingRateHz::Hz500);
    assert_eq!(snap.debounce_ms, Some(DebounceMs::new(6)));
    assert_eq!(snap.lod, Some(LiftOffDistance::High));
    assert_eq!(snap.motion_sync, Some(true));
    assert_eq!(snap.angle_snapping, Some(false));

    let stages = [
        DpiStage::new(0, DpiValue::new(800).unwrap()),
        DpiStage::new(1, DpiValue::new(26_000).unwrap()),
    ];
    dev.set_dpi_stages(&stages, 1).unwrap();
    let dpi_write = dev
        .transport()
        .inner()
        .recorded_writes()
        .iter()
        .find(|w| w.bytes[5] == 1 && w.bytes[6] == 0x01)
        .unwrap()
        .bytes
        .clone();
    assert_eq!(&dpi_write[1..7], &[0, 0, 0x02, 10, 1, 0x01]);
    assert_eq!(
        &dpi_write[7..17],
        &[2, 2, 0x03, 0x20, 0x03, 0x20, 0x65, 0x90, 0x65, 0x90]
    );

    // The wired A5 firmware has no 250 Hz.
    assert!(dev.set_polling_rate(PollingRateHz::Hz250).is_err());
    dev.set_polling_rate(PollingRateHz::Hz1000).unwrap();

    let battery = dev.read_battery().unwrap();
    assert_eq!(battery.percentage, Some(73));
    assert_eq!(battery.state, ChargingState::Charging);

    let profiles = dev.read_profile_state().unwrap();
    assert_eq!(
        profiles.active_slot,
        ProfileSlot::new(1),
        "wire profile is 1-based"
    );
    assert!(dev.set_debounce_ms(DebounceMs::new(16)).is_err());
}

#[test]
fn a5_gen1_dry_run_suppresses_writes() {
    let mut mock = MockHidTransport::new(descriptor(0x2023, 0xF013, 0xFFFF, 0x0001, "A5 Pro Max"));
    mock.push_feature_response(a5_reply(a5c::READ_FIRMWARE, &[1, 0, 0, 0]));
    let sink = Arc::new(MemoryPacketSink::unbounded());
    let mut dev = MchoseDevice::open(mock, true, Some(sink.clone())).unwrap();
    assert_eq!(dev.info().connection, ConnectionKind::WirelessDongle24Ghz);
    dev.set_polling_rate(PollingRateHz::Hz250).unwrap();
    dev.set_lod(LiftOffDistance::Low).unwrap();
    dev.set_ripple_control(true).unwrap();
    assert_eq!(dev.transport().inner().recorded_writes().len(), 1);
    assert_eq!(suppressed_count(&sink), 3);
}

// ── Matching & refusal ──────────────────────────────────────────────────────

#[test]
fn registry_matches_documented_interfaces_and_skips_magdock() {
    let mut registry = DeviceRegistry::new();
    registry.register_driver(driver_descriptor());
    let matched = |vid, pid, up, usage| {
        registry.match_interface(&HidMatchDescriptor::new(vid, pid, up, usage, 1, "p"))
    };

    let v2_rx = matched(0x3837, 0x100B, 0xFF01, 0x0001).unwrap();
    assert_eq!(v2_rx.driver_id, DRIVER_ID);
    assert_eq!(v2_rx.vendor, VendorKind::Mchose);
    assert_eq!(v2_rx.connection, ConnectionKind::WirelessDongle24Ghz);
    assert_eq!(
        matched(0x3837, 0x100A, 0xFF01, 0x0001).unwrap().connection,
        ConnectionKind::Bluetooth
    );
    assert!(matched(0x5253, 0x1020, 0xFF01, 0x0001).is_some());
    assert_eq!(
        matched(0x5253, 0x00B0, 0xFF01, 0x0001).unwrap().connection,
        ConnectionKind::WiredUsb
    );
    assert!(matched(0x5253, 0x100B, 0xFF01, 0x0001).is_none());
    assert_eq!(
        matched(0x3837, 0x4033, 0xFF01, 0x0001).unwrap().model_hint,
        "MCHOSE A7 V3 Ultra+"
    );
    assert!(matched(0x3837, 0x1018, 0xFF01, 0x0001).is_some());
    // Boot mouse / keyboard collections are never claimed.
    assert!(matched(0x3837, 0x4033, 0x0001, 0x0002).is_none());
    assert!(matched(0x3837, 0x100B, 0xFF01, 0x0002).is_none());
    // A5 Gen1 matches on usage page only.
    assert!(matched(0x2023, 0xF019, 0xFFFF, 0x0007).is_some());
    assert!(matched(0x2023, 0xF019, 0xFF00, 0x0001).is_none());
    // MagDock is excluded on every collection.
    assert!(matched(0x3837, MAGDOCK_PID, 0xFF00, 0x0001).is_none());
    assert!(matched(0x3837, MAGDOCK_PID, 0xFF01, 0x0001).is_none());

    for rule in &driver_descriptor().rules {
        assert!(rule.pid.is_some(), "every rule names an explicit PID");
        assert!(
            classify(rule.vid, rule.pid.unwrap()).is_some(),
            "rule 0x{:04X}:0x{:04X} must open",
            rule.vid,
            rule.pid.unwrap()
        );
    }
}

#[test]
fn open_refuses_magdock_unknown_pids_and_wrong_collections() {
    for (vid, pid, up, usage) in [
        (0x3837, MAGDOCK_PID, 0xFF00, 0x0001),
        (0x3837, MAGDOCK_PID, 0xFF01, 0x0001),
        (0x5253, 0x100B, 0xFF01, 0x0001),
        (0x3837, 0x4033, 0x0001, 0x0002),
        (0x2023, 0xF019, 0xFF01, 0x0001),
    ] {
        let mock = MockHidTransport::new(descriptor(vid, pid, up, usage, "x"));
        let err = MchoseDevice::open(mock, false, None).unwrap_err();
        assert!(
            matches!(err, CoreError::Validation(_)),
            "{vid:04X}:{pid:04X}: {err}"
        );
    }
    assert_eq!(classify(0x3837, MAGDOCK_PID), None);
    assert_eq!(classify(0x3837, 0x4021), Some(ProtocolFamily::V2));
    assert_eq!(classify(0x3837, 0x1014), Some(ProtocolFamily::V3));
    assert_eq!(classify(0x2023, 0xF015), Some(ProtocolFamily::A5Gen1));
}
