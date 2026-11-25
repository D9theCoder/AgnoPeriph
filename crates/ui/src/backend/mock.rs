//! Deterministic in-memory peripherals for offline UI work and headless UI tests.

use std::sync::{Arc, Mutex};

use agnoperiph_core::{
    BatteryStatus, Brightness, CalibrationStatus, Capabilities, ChargingState, ConnectionKind,
    CoreError, CoreResult, DebounceMs, DeviceId, DeviceInfo, DksBinding, DksStageAction,
    DksTriggerDirection, DpiStage, DpiValue, HallEffectCapabilities, HallEffectSnapshot,
    KeyHallEffectConfig, KeyIndex, LiftOffDistance, PointerCapabilities, PointerSnapshot,
    PollingRateHz, ProfileCapabilities, ProfileInfo, ProfileMode, ProfileSlot, ProfileSnapshot,
    RapidTriggerConfig, RgbCapabilities, RgbColor, RgbEffect, RgbEffectKind, RgbSnapshot,
    RgbZoneId, RgbZoneInfo, RgbZoneState, SocdMode, SocdPair, TravelHundredthsMm, VendorKind,
};
use agnoperiph_driver_atk::protocol as atk;
use agnoperiph_driver_lamzu::atlantis::{self, AtlantisRateFamily, Command as LamzuCommand};
use agnoperiph_transport::{
    FixedReportValidator, GuardedTransport, HidDeviceDescriptor, HidTransport, MockHidTransport,
    PacketSink, PacketTraceEntry,
};

use super::{
    ActuationTarget, DeviceSnapshot, HallEffectState, PeripheralService, PointerChange,
    PointerState, ProfileState, RgbState, SequencedSink, SettingChange, apply_change_to_snapshot,
    lock, validate_change,
};

/// Stable id of the emulated Lamzu Atlantis 1K receiver.
pub const MOCK_MOUSE_ID: &str = "lamzu:3554:F50D:mock-atlantis-1k";
/// Stable id of the emulated ATK RS7 keyboard.
pub const MOCK_KEYBOARD_ID: &str = "atk:373B:1044:ATK RS HE";

#[derive(Debug, Clone, Copy)]
enum MockCodec {
    LamzuAtlantis(AtlantisRateFamily),
    AtkRs7,
}

impl MockCodec {
    fn validator(self) -> FixedReportValidator {
        match self {
            Self::LamzuAtlantis(_) => atlantis::atlantis_validator(),
            Self::AtkRs7 => atk::atk_rs7_validator(),
        }
    }

    /// Encodes the exact frame sequence the matching driver sends for `change`.
    fn encode(self, change: &SettingChange, key_count: u16) -> CoreResult<Vec<Vec<u8>>> {
        match self {
            Self::LamzuAtlantis(family) => encode_lamzu(family, change),
            Self::AtkRs7 => encode_atk(change, key_count),
        }
    }
}

fn lamzu_write(address: u16, data: &[u8]) -> CoreResult<Vec<u8>> {
    Ok(atlantis::encode_request(LamzuCommand::WriteFlash, address, data)?.to_vec())
}

fn lamzu_flash_u8(address: u16, value: u8) -> CoreResult<Vec<u8>> {
    lamzu_write(address, &atlantis::seal_field(&[value]))
}

fn encode_lamzu(family: AtlantisRateFamily, change: &SettingChange) -> CoreResult<Vec<Vec<u8>>> {
    use atlantis::flash;
    let frames = match change {
        SettingChange::Pointer(PointerChange::ActiveDpiStage(stage_ix)) => {
            vec![lamzu_flash_u8(flash::ACTIVE_DPI_STAGE, *stage_ix)?]
        }
        SettingChange::Pointer(PointerChange::DpiStages { stages, active }) => {
            let count = u8::try_from(stages.len()).unwrap_or(1);
            let mut frames = vec![lamzu_flash_u8(flash::DPI_STAGE_COUNT, count)?];
            for (ix, stage) in stages.iter().enumerate() {
                let offset = u16::try_from(ix).unwrap_or(0) * flash::DPI_STAGE_STRIDE;
                let encoded = atlantis::encode_dpi_stage(stage)?;
                frames.push(lamzu_write(flash::DPI_STAGES + offset, &encoded)?);
                if let Some(color) = stage.indicator_color {
                    let sealed = atlantis::seal_field(&[color.r, color.g, color.b]);
                    frames.push(lamzu_write(flash::DPI_COLORS + offset, &sealed)?);
                }
            }
            frames.push(lamzu_flash_u8(flash::ACTIVE_DPI_STAGE, *active)?);
            frames
        }
        SettingChange::Pointer(PointerChange::PollingRate(rate)) => {
            let raw = atlantis::encode_polling_rate(*rate, family)?;
            vec![lamzu_flash_u8(flash::POLL_RATE, raw)?]
        }
        SettingChange::Pointer(PointerChange::LiftOffDistance(lod)) => {
            let raw = atlantis::encode_lod(*lod)?;
            vec![lamzu_flash_u8(flash::LIFT_OFF_DISTANCE, raw)?]
        }
        SettingChange::Pointer(PointerChange::Debounce(debounce)) => {
            vec![lamzu_flash_u8(flash::DEBOUNCE_MS, debounce.value())?]
        }
        SettingChange::Pointer(PointerChange::MotionSync(enabled)) => {
            vec![lamzu_flash_u8(flash::MOTION_SYNC, u8::from(*enabled))?]
        }
        SettingChange::Profile(slot) => {
            let frame =
                atlantis::encode_request(LamzuCommand::WriteActiveProfile, 0, &[slot.index()])?;
            vec![frame.to_vec()]
        }
        _ => return Err(CoreError::UnsupportedCapability("lamzu_setting")),
    };
    Ok(frames)
}

fn encode_atk(change: &SettingChange, key_count: u16) -> CoreResult<Vec<Vec<u8>>> {
    let payloads = match change {
        SettingChange::Actuation {
            target: ActuationTarget::Key(key),
            point,
        } => {
            let (row, col) = atk::key_to_row_col(*key)?;
            vec![atk::req_single_actuation_set(row, col, *point)]
        }
        SettingChange::Actuation {
            target: ActuationTarget::AllKeys,
            point,
        } => {
            let all = vec![*point; usize::from(key_count)];
            let chunks: Vec<&[TravelHundredthsMm]> = all.chunks(11).collect();
            let last_ix = chunks.len().saturating_sub(1);
            chunks
                .iter()
                .enumerate()
                .map(|(ix, chunk)| {
                    let op = if ix == 0 {
                        atk::flash_op::ERASE
                    } else if ix == last_ix {
                        atk::flash_op::WRITE
                    } else {
                        atk::flash_op::NORMAL
                    };
                    let offset = u16::try_from(ix * 11).unwrap_or(0);
                    let count = u8::try_from(chunk.len()).unwrap_or(11);
                    atk::req_travel_buffer_set(offset, count, op, chunk)
                })
                .collect()
        }
        SettingChange::RapidTrigger { key, config } => {
            let (row, col) = atk::key_to_row_col(*key)?;
            vec![atk::req_single_rt_set(row, col, config)]
        }
        SettingChange::Dks(binding) => {
            let slot = u8::try_from(binding.key.value()).unwrap_or(u8::MAX);
            vec![atk::req_dks_set(slot, binding)]
        }
        SettingChange::ClearDks { slot } => {
            let empty = DksBinding::new(*slot, [None, None, None, None]);
            let slot = u8::try_from(slot.value()).unwrap_or(u8::MAX);
            vec![atk::req_dks_set(slot, &empty)]
        }
        SettingChange::Socd(pairs) => pairs
            .iter()
            .enumerate()
            .map(|(ix, pair)| atk::req_socd_set(u8::try_from(ix).unwrap_or(0), Some(pair)))
            .collect::<CoreResult<Vec<_>>>()?,
        SettingChange::Rgb {
            effect, brightness, ..
        } => vec![atk::req_backlight_set(*effect, brightness.percent())],
        _ => return Err(CoreError::UnsupportedCapability("atk_setting")),
    };
    Ok(payloads
        .into_iter()
        .map(|payload| atk::wrap_tx(payload).to_vec())
        .collect())
}

fn open_mock_transport(
    descriptor: &HidDeviceDescriptor,
    codec: MockCodec,
    dry_run: bool,
    sink: &Arc<SequencedSink>,
) -> GuardedTransport<MockHidTransport> {
    let sink: Arc<dyn PacketSink> = sink.clone();
    GuardedTransport::new(MockHidTransport::new(descriptor.clone()))
        .with_validator(Arc::new(codec.validator()))
        .with_sink(sink)
        .with_dry_run(dry_run)
}

#[derive(Debug)]
struct MockDevice {
    descriptor: HidDeviceDescriptor,
    codec: MockCodec,
    /// The emulated device's stored state; only writes outside dry-run change it.
    stored: DeviceSnapshot,
    presented: DeviceSnapshot,
    transport: GuardedTransport<MockHidTransport>,
}

impl MockDevice {
    fn new(
        descriptor: HidDeviceDescriptor,
        codec: MockCodec,
        snapshot: DeviceSnapshot,
        sink: &Arc<SequencedSink>,
    ) -> Self {
        let transport = open_mock_transport(&descriptor, codec, true, sink);
        Self {
            descriptor,
            codec,
            presented: snapshot.clone(),
            stored: snapshot,
            transport,
        }
    }

    /// Replays the readonly status query its driver sends on refresh, including the reply.
    fn replay_status_query(&mut self, sink: &Arc<SequencedSink>) -> CoreResult<()> {
        let (request, reply) = match self.codec {
            MockCodec::LamzuAtlantis(_) => {
                let battery = self.stored.battery;
                let pct = battery.and_then(|b| b.percentage).unwrap_or(0);
                let millivolts = battery.and_then(|b| b.voltage_mv).unwrap_or(0);
                let [mv_hi, mv_lo] = millivolts.to_be_bytes();
                let request = atlantis::encode_request(LamzuCommand::ReadBattery, 0, &[])?;
                let reply = atlantis::encode_request(
                    LamzuCommand::ReadBattery,
                    0,
                    &[pct, 0, mv_hi, mv_lo],
                )?;
                (request.to_vec(), reply.to_vec())
            }
            MockCodec::AtkRs7 => {
                let request = atk::req_firmware_version();
                let mut reply = request;
                // `decode_firmware_version` reads bytes 5..=6 as `{major:x}.{minor:02x}`.
                reply[5] = 0x01;
                reply[6] = 0x07;
                (atk::wrap_tx(request).to_vec(), reply.to_vec())
            }
        };
        self.transport.send_query_output_report(&request)?;

        // The guarded transport exposes its inner mock only immutably, so the scripted
        // reply is read through a second guarded reader that shares the same sink.
        let mut responder = MockHidTransport::new(self.descriptor.clone());
        responder.push_input_response(reply);
        let sink: Arc<dyn PacketSink> = sink.clone();
        let mut reader = GuardedTransport::new(responder).with_sink(sink);
        let mut buf = [0u8; 64];
        reader.read_input_report(&mut buf, std::time::Duration::from_millis(1))?;
        Ok(())
    }
}

#[derive(Debug)]
struct MockState {
    dry_run: bool,
    devices: Vec<MockDevice>,
}

/// Deterministic in-memory backend emulating a Lamzu Atlantis 1K receiver and an ATK RS7.
///
/// Writes are encoded with the real driver codecs and pass through `GuardedTransport`
/// with the real validators, so the packet inspector shows genuine frames and outcomes.
/// Starts in dry-run mode, like [`super::LiveHidService`].
#[derive(Debug)]
pub struct MockPeripheralService {
    state: Mutex<MockState>,
    sink: Arc<SequencedSink>,
}

impl Default for MockPeripheralService {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPeripheralService {
    /// Creates the service with the representative mouse and keyboard.
    pub fn new() -> Self {
        let service = Self::empty();
        {
            let mut state = lock(&service.state);
            state.devices.push(MockDevice::new(
                lamzu_descriptor(),
                MockCodec::LamzuAtlantis(AtlantisRateFamily::Wired),
                lamzu_atlantis_snapshot(),
                &service.sink,
            ));
            state.devices.push(MockDevice::new(
                atk_descriptor(),
                MockCodec::AtkRs7,
                atk_rs7_snapshot(),
                &service.sink,
            ));
        }
        service
    }

    /// Creates a service with no connected devices.
    pub fn empty() -> Self {
        Self {
            state: Mutex::new(MockState {
                dry_run: true,
                devices: Vec::new(),
            }),
            sink: Arc::new(SequencedSink::new()),
        }
    }
}

impl PeripheralService for MockPeripheralService {
    fn is_dry_run(&self) -> bool {
        lock(&self.state).dry_run
    }

    fn set_dry_run(&self, dry_run: bool) {
        let mut state = lock(&self.state);
        state.dry_run = dry_run;
        for device in &mut state.devices {
            device.transport =
                open_mock_transport(&device.descriptor, device.codec, dry_run, &self.sink);
            device.presented = device.stored.clone();
        }
    }

    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
        let mut state = lock(&self.state);
        for device in &mut state.devices {
            device.replay_status_query(&self.sink)?;
            device.presented = device.stored.clone();
        }
        Ok(state.devices.iter().map(|d| d.presented.clone()).collect())
    }

    fn apply(&self, device: &DeviceId, change: SettingChange) -> CoreResult<DeviceSnapshot> {
        let mut state = lock(&self.state);
        let dry_run = state.dry_run;
        let target = state
            .devices
            .iter_mut()
            .find(|d| d.presented.info.id == *device)
            .ok_or_else(|| CoreError::DeviceNotFound(device.to_string()))?;

        validate_change(&target.presented, &change)?;
        let key_count = target
            .presented
            .hall_effect
            .as_ref()
            .map_or(0, |he| he.capabilities.key_count);
        for frame in target.codec.encode(&change, key_count)? {
            target.transport.send_output_report(&frame)?;
        }

        if dry_run {
            apply_change_to_snapshot(&mut target.presented, &change);
            target.presented.is_preview = true;
        } else {
            apply_change_to_snapshot(&mut target.stored, &change);
            target.presented = target.stored.clone();
        }
        Ok(target.presented.clone())
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.sink.log.snapshot()
    }

    fn clear_packet_log(&self) {
        self.sink.log.clear();
    }
}

fn lamzu_descriptor() -> HidDeviceDescriptor {
    HidDeviceDescriptor::new(
        atlantis::LAMZU_ATLANTIS_VID,
        0xF50D,
        atlantis::LAMZU_ATLANTIS_USAGE_PAGE,
        atlantis::LAMZU_ATLANTIS_USAGE_ID,
        1,
        "mock://lamzu/atlantis-1k",
    )
}

fn atk_descriptor() -> HidDeviceDescriptor {
    HidDeviceDescriptor::new(
        atk::ATK_VID,
        atk::ATK_RS7_PID,
        atk::ATK_RS7_USAGE_PAGE,
        atk::ATK_RS7_USAGE_ID,
        1,
        "mock://atk/rs7",
    )
}

fn dpi(value: u32) -> DpiValue {
    DpiValue::new(value).unwrap_or_else(|_| unreachable!("fixture DPI values are non-zero"))
}

fn travel(hundredths: u16) -> TravelHundredthsMm {
    TravelHundredthsMm::from_hundredths(hundredths)
}

fn lamzu_atlantis_snapshot() -> DeviceSnapshot {
    use atlantis::flash;
    let info = DeviceInfo::new(
        DeviceId::new(MOCK_MOUSE_ID),
        VendorKind::Lamzu,
        "Lamzu Atlantis (1K Receiver)",
        ConnectionKind::WirelessDongle24Ghz,
        atlantis::LAMZU_ATLANTIS_VID,
        0xF50D,
    )
    .with_firmware_version("v1.24");

    let capabilities = PointerCapabilities::new(
        flash::MIN_DPI,
        flash::MAX_DPI,
        flash::DPI_STEP,
        flash::MAX_DPI_STAGES,
    )
    .with_xy_independent_dpi(true)
    .with_polling_rates(vec![
        PollingRateHz::Hz125,
        PollingRateHz::Hz250,
        PollingRateHz::Hz500,
        PollingRateHz::Hz1000,
    ])
    .with_lods(vec![
        LiftOffDistance::TenthsMm(10),
        LiftOffDistance::TenthsMm(20),
    ])
    .with_debounce_range_ms(0, flash::MAX_DEBOUNCE_MS)
    .with_motion_sync(true)
    .with_angle_snapping(true)
    .with_ripple_control(true);

    let stages = [
        (400, RgbColor::new(255, 0, 0)),
        (800, RgbColor::new(0, 255, 0)),
        (1600, RgbColor::new(0, 0, 255)),
        (3200, RgbColor::new(255, 255, 0)),
    ]
    .into_iter()
    .zip(0u8..)
    .map(|((value, color), ix)| DpiStage::new(ix, dpi(value)).with_indicator_color(color))
    .collect();

    let pointer = PointerSnapshot::new(1, stages, PollingRateHz::Hz1000)
        .with_lod(LiftOffDistance::TenthsMm(10))
        .with_debounce_ms(DebounceMs::new(2))
        .with_motion_sync(false)
        .with_angle_snapping(false)
        .with_ripple_control(false);

    let profiles = ProfileSnapshot::new(
        ProfileSlot::new(0),
        ProfileMode::OnboardMemory,
        (0..flash::PROFILE_COUNT)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect(),
    );

    DeviceSnapshot {
        info,
        capabilities: Capabilities::empty()
            .with_pointer(true)
            .with_battery(true)
            .with_profiles(true),
        pointer: Some(PointerState {
            capabilities,
            snapshot: pointer,
        }),
        hall_effect: None,
        rgb: None,
        battery: Some(BatteryStatus::new(
            Some(78),
            Some(3_940),
            ChargingState::Discharging,
            true,
        )),
        profiles: Some(ProfileState {
            capabilities: ProfileCapabilities::new(flash::PROFILE_COUNT, false, false),
            snapshot: profiles,
        }),
        warnings: Vec::new(),
        is_preview: false,
    }
}

fn atk_rs7_snapshot() -> DeviceSnapshot {
    let info = DeviceInfo::new(
        DeviceId::new(MOCK_KEYBOARD_ID),
        VendorKind::Atk,
        "ATK RS7 RGB",
        ConnectionKind::WiredUsb,
        atk::ATK_VID,
        atk::ATK_RS7_PID,
    )
    .with_firmware_version("v1.07")
    .with_serial_number("ATK RS HE");

    let capabilities = HallEffectCapabilities::new(
        atk::RS7_KEY_COUNT,
        atk::RS7_TRAVEL_MIN,
        atk::RS7_TRAVEL_MAX,
        atk::RS7_STEP_NORMAL,
    )
    .with_rapid_trigger_range(atk::RS7_RT_MIN, atk::RS7_RT_MAX, true)
    .with_max_dks_bindings(atk::RS7_MAX_DKS_SLOTS)
    .with_max_socd_pairs(atk::RS7_MAX_SOCD_SLOTS)
    .with_calibration_status(true);

    DeviceSnapshot {
        info,
        capabilities: Capabilities::empty().with_hall_effect(true).with_rgb(true),
        pointer: None,
        hall_effect: Some(HallEffectState {
            capabilities,
            snapshot: atk_rs7_hall_effect(),
        }),
        rgb: Some(atk_rs7_rgb()),
        battery: None,
        profiles: None,
        warnings: Vec::new(),
        is_preview: false,
    }
}

fn atk_rs7_hall_effect() -> HallEffectSnapshot {
    // W, A, S, D carry Rapid Trigger with a shallower actuation point.
    let wasd = [
        atk::row_col_to_key(2, 2),
        atk::row_col_to_key(3, 1),
        atk::row_col_to_key(3, 2),
        atk::row_col_to_key(3, 3),
    ];
    let keys = (0..atk::RS7_KEY_COUNT)
        .map(KeyIndex::new)
        .map(|key| {
            let (actuation, rapid_trigger) = if wasd.contains(&key) {
                (
                    travel(80),
                    RapidTriggerConfig::new(true, true, travel(16), travel(16)),
                )
            } else {
                (travel(120), RapidTriggerConfig::disabled())
            };
            KeyHallEffectConfig::new(key, actuation, rapid_trigger)
                .with_deadzones(Some(travel(10)), Some(travel(10)))
        })
        .collect();

    let dks = DksBinding::new(
        KeyIndex::new(0),
        [
            Some(DksStageAction::new(
                DksTriggerDirection::Press,
                travel(100),
                0x2C,
                false,
            )),
            Some(DksStageAction::new(
                DksTriggerDirection::Release,
                travel(100),
                0x2C,
                true,
            )),
            None,
            None,
        ],
    );
    let socd = SocdPair::new(
        atk::row_col_to_key(3, 1),
        atk::row_col_to_key(3, 3),
        SocdMode::LastInputPriority,
    )
    .unwrap_or_else(|_| unreachable!("fixture SOCD keys are distinct"));
    HallEffectSnapshot::new(keys, vec![dks], vec![socd], CalibrationStatus::Calibrated)
}

fn atk_rs7_rgb() -> RgbState {
    let rgb_capabilities = RgbCapabilities::new(
        vec![RgbZoneInfo::new(RgbZoneId::new(0), "Key Backlight")],
        vec![
            RgbEffectKind::Off,
            RgbEffectKind::Static,
            RgbEffectKind::Breathing,
            RgbEffectKind::SpectrumCycle,
            RgbEffectKind::Wave,
            RgbEffectKind::Reactive,
        ],
        true,
    );
    let brightness =
        Brightness::new(80).unwrap_or_else(|_| unreachable!("fixture brightness is a percentage"));
    RgbState {
        capabilities: rgb_capabilities,
        snapshot: RgbSnapshot::new(vec![RgbZoneState::new(
            RgbZoneId::new(0),
            RgbEffect::Static {
                color: RgbColor::new(255, 255, 255),
            },
            brightness,
        )]),
    }
}

#[cfg(test)]
mod tests {
    use agnoperiph_transport::{PacketDirection, PacketOutcome, PacketValidator, ReportKind};

    use super::*;

    fn keyboard() -> DeviceId {
        DeviceId::new(MOCK_KEYBOARD_ID)
    }

    fn mouse() -> DeviceId {
        DeviceId::new(MOCK_MOUSE_ID)
    }

    #[test]
    fn starts_in_dry_run_and_lists_both_devices() {
        let service = MockPeripheralService::new();
        assert!(service.is_dry_run());
        let devices = service.refresh().unwrap();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].capabilities.has_pointer());
        assert!(devices[1].capabilities.has_hall_effect());

        let log = service.packet_log();
        assert_eq!(log.len(), 4, "one query and one reply per device");
        assert_eq!(log[1].direction, PacketDirection::DeviceToHost);
        assert_eq!(log[1].outcome, PacketOutcome::Received);
        assert!(
            log.windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[test]
    fn dry_run_write_is_suppressed_and_only_previewed() {
        let service = MockPeripheralService::new();
        service.refresh().unwrap();
        service.clear_packet_log();
        let key = atk::row_col_to_key(2, 1);

        let preview = service
            .apply_actuation(&keyboard(), ActuationTarget::Key(key), travel(200))
            .unwrap();
        assert!(preview.is_preview);
        let he = preview.hall_effect.as_ref().unwrap();
        assert_eq!(he.key(key).unwrap().actuation, travel(200));

        let log = service.packet_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].outcome, PacketOutcome::DryRunSuppressed);
        assert_eq!(log[0].bytes.len(), atk::RS7_TX_REPORT_LEN);

        let reread = service.refresh().unwrap();
        let he = reread[1].hall_effect.as_ref().unwrap();
        assert_eq!(he.key(key).unwrap().actuation, travel(120));
        assert!(!reread[1].is_preview);
    }

    #[test]
    fn writes_outside_dry_run_persist() {
        let service = MockPeripheralService::new();
        service.set_dry_run(false);
        service.refresh().unwrap();
        service
            .apply_pointer_settings(&mouse(), PointerChange::PollingRate(PollingRateHz::Hz500))
            .unwrap();
        let reread = service.refresh().unwrap();
        let pointer = reread[0].pointer.as_ref().unwrap();
        assert_eq!(pointer.snapshot.polling_rate, PollingRateHz::Hz500);
        assert!(service.packet_log().iter().any(|entry| {
            entry.outcome == PacketOutcome::Transmitted
                && entry.bytes.len() == atlantis::ATLANTIS_PACKET_LEN
        }));
    }

    #[test]
    fn invalid_changes_are_rejected_before_any_frame_is_sent() {
        let service = MockPeripheralService::new();
        service.refresh().unwrap();
        service.clear_packet_log();
        let too_deep = service.apply_actuation(&keyboard(), ActuationTarget::AllKeys, travel(500));
        assert!(too_deep.is_err());
        let unsupported = service
            .apply_pointer_settings(&mouse(), PointerChange::PollingRate(PollingRateHz::Hz8000));
        assert!(unsupported.is_err());
        assert_eq!(service.packet_log(), Vec::new());
    }

    #[test]
    fn global_actuation_matches_driver_chunking_and_validator() {
        let frames = MockCodec::AtkRs7
            .encode(
                &SettingChange::Actuation {
                    target: ActuationTarget::AllKeys,
                    point: travel(150),
                },
                atk::RS7_KEY_COUNT,
            )
            .unwrap();
        assert_eq!(frames.len(), 9);
        let validator = atk::atk_rs7_validator();
        for frame in &frames {
            validator
                .validate_outgoing(ReportKind::Output, frame)
                .unwrap();
        }
    }

    #[test]
    fn lamzu_dpi_stage_frames_pass_the_checksum_validator() {
        let snapshot = lamzu_atlantis_snapshot();
        let pointer = snapshot.pointer.unwrap();
        let change = SettingChange::Pointer(PointerChange::DpiStages {
            stages: pointer.snapshot.dpi_stages,
            active: 2,
        });
        let frames = MockCodec::LamzuAtlantis(AtlantisRateFamily::Wired)
            .encode(&change, 0)
            .unwrap();
        assert_eq!(frames.len(), 1 + 4 * 2 + 1);
        let validator = atlantis::atlantis_validator();
        for frame in &frames {
            validator
                .validate_outgoing(ReportKind::Output, frame)
                .unwrap();
        }
    }
}
