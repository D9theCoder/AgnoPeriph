//! Deterministic in-memory peripherals for offline UI work and headless UI tests.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

mod brands;

pub use brands::{MOCK_PULSAR_ID, MOCK_RAZER_ID, MOCK_VIA_ID, MOCK_WOOTING_ID};

use agnoperiph_core::{
    BatteryStatus, Brightness, CalibrationStatus, Capabilities, ChargingState, ConnectionKind,
    CoreError, CoreResult, DebounceMs, DeviceId, DeviceInfo, DksBinding, DksStageAction,
    DksTriggerDirection, DpiStage, DpiValue, HallEffectCapabilities, HallEffectSnapshot, KeyCode,
    KeyHallEffectConfig, KeyIndex, KeyTravelSample, KeyboardConfigCapabilities,
    KeyboardConfigSnapshot, KeymapCapabilities, KeymapSnapshot, LiftOffDistance, MacroActionKind,
    MacroCapabilities, MacroSlot, MacroStep, MouseButton, PointerCapabilities, PointerSnapshot,
    PollingRateHz, ProfileCapabilities, ProfileInfo, ProfileMode, ProfileSlot, ProfileSnapshot,
    RapidSwitchPair, RapidTriggerConfig, RgbCapabilities, RgbColor, RgbEffect, RgbEffectKind,
    RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState, SocdMode, SocdPair, TravelHundredthsMm,
    VendorKind,
};
use agnoperiph_driver_atk::protocol as atk;
use agnoperiph_driver_lamzu::atlantis::{self, AtlantisRateFamily, Command as LamzuCommand};
use agnoperiph_driver_lamzu::crystall;
use agnoperiph_transport::{
    FixedReportValidator, GuardedTransport, HidDeviceDescriptor, HidTransport, MockHidTransport,
    PacketSink, PacketTraceEntry, ReportKind,
};

use super::{
    ActuationTarget, ApplyError, DeviceSnapshot, HallEffectState, KeyboardConfigState, KeymapState,
    MacroState, PeripheralService, PointerChange, PointerState, ProfileState, RgbState,
    SequencedSink, SettingChange, UnavailableDevice, apply_change_to_snapshot, lock,
    validate_change,
};

/// Stable id of the emulated Lamzu Atlantis 1K receiver.
pub const MOCK_MOUSE_ID: &str = "lamzu:3554:F50D:mock-atlantis-1k";
/// Stable id of the emulated ATK RS7 keyboard.
pub const MOCK_KEYBOARD_ID: &str = "atk:373B:1044:ATK RS HE";

#[derive(Debug, Clone, Copy)]
enum MockCodec {
    LamzuAtlantis(AtlantisRateFamily),
    AtkRs7,
    Razer(brands::RazerProductProfile),
    Pulsar,
    Via { cols: u8 },
    Wooting(brands::WootingProductProfile),
}

impl MockCodec {
    fn validator(self) -> FixedReportValidator {
        match self {
            Self::LamzuAtlantis(_) => atlantis::atlantis_validator(),
            Self::AtkRs7 => atk::atk_rs7_validator(),
            Self::Razer(_) => brands::razer_validator(),
            Self::Pulsar => brands::pulsar_validator(),
            Self::Via { .. } => brands::via_validator(),
            Self::Wooting(_) => brands::wooting_validator(),
        }
    }

    /// HID report type the matching driver writes with.
    const fn report_kind(self) -> ReportKind {
        match self {
            Self::Razer(_) | Self::Wooting(_) => ReportKind::Feature,
            Self::LamzuAtlantis(_) | Self::AtkRs7 | Self::Pulsar | Self::Via { .. } => {
                ReportKind::Output
            }
        }
    }

    /// Encodes the exact frame sequence the matching driver sends for `change`.
    fn encode(self, change: &SettingChange, current: &DeviceSnapshot) -> CoreResult<Vec<Vec<u8>>> {
        match self {
            Self::LamzuAtlantis(family) => encode_lamzu(family, change, current),
            Self::AtkRs7 => {
                let (key_count, previous_socd_len, previous_rs_len) =
                    current.hall_effect.as_ref().map_or((0, 0, 0), |he| {
                        (
                            he.capabilities.key_count,
                            he.snapshot.socd_pairs.len(),
                            he.snapshot.rapid_switch_pairs.len(),
                        )
                    });
                encode_atk(change, key_count, previous_socd_len, previous_rs_len)
            }
            Self::Razer(profile) => brands::encode_razer(profile, change, current),
            Self::Pulsar => brands::encode_pulsar(change),
            Self::Via { cols } => brands::encode_via(cols, change, current),
            Self::Wooting(profile) => brands::encode_wooting(profile, change),
        }
    }
}

fn lamzu_write(address: u16, data: &[u8]) -> CoreResult<Vec<u8>> {
    Ok(atlantis::encode_request(LamzuCommand::WriteFlash, address, data)?.to_vec())
}

fn lamzu_flash_u8(address: u16, value: u8) -> CoreResult<Vec<u8>> {
    lamzu_write(address, &atlantis::seal_field(&[value]))
}

/// Splits one flash field into the ≤10-byte `WriteFlash` transfers the driver sends.
fn lamzu_write_field(address: u16, data: &[u8]) -> CoreResult<Vec<Vec<u8>>> {
    data.chunks(atlantis::ATLANTIS_MAX_PAYLOAD)
        .enumerate()
        .map(|(ix, chunk)| {
            let offset = u16::try_from(ix * atlantis::ATLANTIS_MAX_PAYLOAD).unwrap_or(0);
            lamzu_write(address.saturating_add(offset), chunk)
        })
        .collect()
}

/// Mirrors `write_macros`: per slot, contexts → name → trigger, skipping unchanged fields.
fn encode_lamzu_macros(slots: &[MacroSlot], current: &DeviceSnapshot) -> CoreResult<Vec<Vec<u8>>> {
    let previous = current
        .macros
        .as_ref()
        .map_or(&[][..], |m| m.slots.as_slice());
    let mut frames = Vec::new();
    for slot in slots {
        let index = slot.index;
        let address = crystall::macro_slot_address(index)?;
        let count_address = address + crystall::layout::CONTEXT_COUNT_OFFSET;
        let before = previous.iter().find(|s| s.index == index);
        // Validation only lets an unchanged read-only slot through, and the driver skips it.
        if before.is_some_and(|s| s.is_readonly) {
            continue;
        }
        let before_steps = before.map_or(&[][..], |s| s.steps.as_slice());
        let before_name = before.filter(|s| !s.steps.is_empty()).map_or_else(
            || crystall::encode_name(""),
            |s| crystall::encode_name(&s.name),
        );

        if slot.steps.is_empty() {
            if !before_steps.is_empty() {
                frames.extend(lamzu_write_field(
                    count_address,
                    &atlantis::seal_field(&[0]),
                )?);
            }
            if before_name.len() > 1 {
                frames.extend(lamzu_write_field(address, &[0])?);
            }
            continue;
        }

        let contexts = crystall::encode_contexts(&slot.steps)?;
        if contexts != crystall::encode_contexts(before_steps)? {
            frames.extend(lamzu_write_field(
                count_address,
                &atlantis::seal_field(&contexts),
            )?);
        }
        let name = crystall::encode_name(&slot.name);
        if name != before_name {
            frames.extend(lamzu_write_field(address, &name)?);
        }
        // The driver only refreshes a trigger that already carries a macro; a slot
        // without one keeps its current button action.
        let binding = crystall::macro_binding(index, slot.repeat);
        let before_binding = before
            .filter(|s| !s.steps.is_empty())
            .map(|s| crystall::macro_binding(index, s.repeat));
        if before_binding.is_some_and(|previous| previous != binding) {
            frames.extend(lamzu_write_field(
                crystall::key_slot_address(index)?,
                &atlantis::seal_field(&binding),
            )?);
        }
    }
    Ok(frames)
}

fn encode_lamzu(
    family: AtlantisRateFamily,
    change: &SettingChange,
    current: &DeviceSnapshot,
) -> CoreResult<Vec<Vec<u8>>> {
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
        SettingChange::Pointer(PointerChange::AngleSnapping(enabled)) => {
            vec![lamzu_flash_u8(flash::ANGLE_SNAPPING, u8::from(*enabled))?]
        }
        SettingChange::Pointer(PointerChange::RippleControl(enabled)) => {
            vec![lamzu_flash_u8(flash::RIPPLE_CONTROL, u8::from(*enabled))?]
        }
        SettingChange::Profile(slot) => {
            let frame =
                atlantis::encode_request(LamzuCommand::WriteActiveProfile, 0, &[slot.index()])?;
            vec![frame.to_vec()]
        }
        SettingChange::Macros(slots) => encode_lamzu_macros(slots, current)?,
        _ => return Err(CoreError::UnsupportedCapability("lamzu_setting")),
    };
    Ok(frames)
}

#[allow(clippy::too_many_lines)]
fn encode_atk(
    change: &SettingChange,
    key_count: u16,
    previous_socd_len: usize,
    previous_rs_len: usize,
) -> CoreResult<Vec<Vec<u8>>> {
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
        SettingChange::ActuationMany { keys, point } => keys
            .iter()
            .map(|&key| {
                let (row, col) = atk::key_to_row_col(key)?;
                Ok(atk::req_single_actuation_set(row, col, *point))
            })
            .collect::<CoreResult<Vec<_>>>()?,
        SettingChange::RapidTrigger { key, config } => {
            let (row, col) = atk::key_to_row_col(*key)?;
            vec![atk::req_single_rt_set(row, col, config)]
        }
        SettingChange::RapidTriggerMany { keys, config } => keys
            .iter()
            .map(|&key| {
                let (row, col) = atk::key_to_row_col(key)?;
                Ok(atk::req_single_rt_set(row, col, config))
            })
            .collect::<CoreResult<Vec<_>>>()?,
        SettingChange::Deadzones { top, bottom } => vec![atk::req_deadzone_set(*top, *bottom)],
        SettingChange::Dks(binding) => {
            let slot = u8::try_from(binding.key.value()).unwrap_or(u8::MAX);
            vec![atk::req_dks_set(slot, binding)]
        }
        SettingChange::ClearDks { slot } => {
            let empty = DksBinding::new(*slot, [None, None, None, None]);
            let slot = u8::try_from(slot.value()).unwrap_or(u8::MAX);
            vec![atk::req_dks_set(slot, &empty)]
        }
        SettingChange::Socd(pairs) => {
            let mut frames: Vec<[u8; atk::RS7_PAYLOAD_LEN]> = pairs
                .iter()
                .enumerate()
                .map(|(ix, pair)| atk::req_socd_set(u8::try_from(ix).unwrap_or(0), Some(pair)))
                .collect::<CoreResult<Vec<_>>>()?;
            for ix in pairs.len()..previous_socd_len {
                frames.push(atk::req_socd_set(u8::try_from(ix).unwrap_or(0), None)?);
            }
            frames
        }
        SettingChange::RapidSwitch(pairs) => {
            let mut frames: Vec<[u8; atk::RS7_PAYLOAD_LEN]> = pairs
                .iter()
                .enumerate()
                .map(|(ix, pair)| atk::req_rs_pair_set(u8::try_from(ix).unwrap_or(0), Some(pair)))
                .collect::<CoreResult<Vec<_>>>()?;
            for ix in pairs.len()..previous_rs_len {
                frames.push(atk::req_rs_pair_set(u8::try_from(ix).unwrap_or(0), None)?);
            }
            frames
        }
        SettingChange::ModTap(binding) => {
            let (row, col) = atk::key_to_row_col(binding.key)?;
            vec![
                atk::req_tap_dance_set(
                    binding.slot,
                    binding.tap_keycode,
                    binding.hold_keycode,
                    binding.hold_delay_ms,
                ),
                atk::req_keymap_set_keycode(
                    0,
                    row,
                    col,
                    KeyCode::new(atk::key_class::TAP_DANCE, binding.slot),
                ),
            ]
        }
        SettingChange::ClearModTap { slot, .. } => {
            vec![atk::req_tap_dance_set(*slot, (0, 0), (0, 0), 200)]
        }
        SettingChange::ToggleKey(binding) => {
            let (row, col) = atk::key_to_row_col(binding.key)?;
            let hw_slot = binding.slot + atk::RS7_MAX_MT_SLOTS;
            vec![
                atk::req_tap_dance_set(hw_slot, binding.keycode, (0, 0), binding.delay_ms),
                atk::req_keymap_set_keycode(
                    0,
                    row,
                    col,
                    KeyCode::new(atk::key_class::TAP_DANCE, hw_slot),
                ),
            ]
        }
        SettingChange::ClearToggleKey { slot, .. } => {
            vec![atk::req_tap_dance_set(
                *slot + atk::RS7_MAX_MT_SLOTS,
                (0, 0),
                (0, 0),
                200,
            )]
        }
        SettingChange::KeyBinding { layer, key, code } => {
            let (row, col) = atk::key_to_row_col(*key)?;
            vec![atk::req_keymap_set_keycode(*layer, row, col, *code)]
        }
        SettingChange::ResetKeymap => vec![atk::req_keymap_reset()],
        SettingChange::Macros(slots) => atk::encode_macros(slots)
            .chunks(28)
            .enumerate()
            .map(|(ix, chunk)| {
                atk::req_macro_buffer_set(u16::try_from(ix * 28).unwrap_or(0), chunk)
            })
            .collect(),
        SettingChange::KeyboardConfig(cfg) => vec![
            atk::req_feature_flags_set(
                0,
                cfg.exchange_wasd,
                cfg.mac_mode,
                cfg.win_lock,
                cfg.six_key_rollover,
            ),
            atk::req_anti_accidental_touch_set(cfg.anti_accidental_touch),
            atk::req_game_mode_set(cfg.extreme_mode),
            atk::req_report_rate_set(cfg.polling_rate)?,
        ],
        SettingChange::StartCalibration => vec![atk::req_calibration_start()],
        SettingChange::FinishCalibration => vec![atk::req_calibration_finish()],
        SettingChange::PollCalibrationProgress => Vec::new(),
        SettingChange::Rgb {
            zone,
            effect,
            brightness,
        } => {
            if zone.value() == 1 {
                vec![atk::req_logo_light_set(*effect, brightness.percent())]
            } else {
                vec![atk::req_backlight_set(*effect, brightness.percent())]
            }
        }
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
    /// Factory state restored by a keymap reset.
    factory: DeviceSnapshot,
    /// The emulated device's stored state; only writes outside dry-run change it.
    stored: DeviceSnapshot,
    presented: DeviceSnapshot,
    /// Simulated live Hall Effect key-travel samples for offline testing.
    live_travel: Vec<KeyTravelSample>,
    /// Remaining simulated live-travel disconnect errors before succeeding again.
    live_travel_error_count: usize,
    live_travel_error_message: Option<String>,
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
            factory: snapshot.clone(),
            presented: snapshot.clone(),
            stored: snapshot,
            live_travel: Vec::new(),
            live_travel_error_count: 0,
            live_travel_error_message: None,
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
            MockCodec::Razer(profile) => brands::razer_status_query(profile)?,
            MockCodec::Pulsar => brands::pulsar_status_query(self.stored.battery)?,
            MockCodec::Via { .. } => brands::via_status_query(),
            MockCodec::Wooting(_) => brands::wooting_status_query(),
        };
        let kind = self.codec.report_kind();
        if kind == ReportKind::Feature {
            self.transport.send_query_feature_report(&request)?;
        } else {
            self.transport.send_query_output_report(&request)?;
        }

        // The guarded transport exposes its inner mock only immutably, so the scripted
        // reply is read through a second guarded reader that shares the same sink.
        let mut responder = MockHidTransport::new(self.descriptor.clone());
        let sink: Arc<dyn PacketSink> = sink.clone();
        let mut buf = vec![0u8; reply.len().max(64)];
        if kind == ReportKind::Feature {
            responder.push_feature_response(reply);
            let mut reader = GuardedTransport::new(responder).with_sink(sink);
            reader.get_feature_report(&mut buf)?;
        } else {
            responder.push_input_response(reply);
            let mut reader = GuardedTransport::new(responder).with_sink(sink);
            reader.read_input_report(&mut buf, std::time::Duration::from_millis(1))?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct MockState {
    dry_run: bool,
    devices: Vec<MockDevice>,
    /// Devices unplugged by [`MockPeripheralService::unplug`], waiting to return.
    unplugged: Vec<MockDevice>,
    /// Interfaces attached at the last refresh.
    attached: BTreeSet<String>,
    unavailable: Vec<UnavailableDevice>,
    /// Whether the unavailable devices would open now.
    is_openable: bool,
}

impl MockState {
    fn interfaces(&self) -> BTreeSet<String> {
        self.devices
            .iter()
            .map(|d| d.descriptor.path.clone())
            .collect()
    }
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

    /// Creates the representative pair plus one emulated device per newer capability mix:
    /// a Razer mouse with RGB, a Pulsar mouse with Medium lift-off, a QMK/VIA Keychron
    /// keyboard without Hall Effect, and a Wooting 60HE with a readonly actuation table.
    pub fn multi_brand() -> Self {
        let service = Self::new();
        {
            let mut state = lock(&service.state);
            for (descriptor, codec, snapshot) in brands::fixtures() {
                state
                    .devices
                    .push(MockDevice::new(descriptor, codec, snapshot, &service.sink));
            }
        }
        service
    }

    /// Creates a service with no connected devices.
    pub fn empty() -> Self {
        Self {
            state: Mutex::new(MockState {
                dry_run: true,
                devices: Vec::new(),
                unplugged: Vec::new(),
                attached: BTreeSet::new(),
                unavailable: Vec::new(),
                is_openable: false,
            }),
            sink: Arc::new(SequencedSink::new()),
        }
    }

    /// Detaches a device, as pulling its cable or receiver would.
    pub fn unplug(&self, device: &DeviceId) {
        let mut state = lock(&self.state);
        if let Some(ix) = state
            .devices
            .iter()
            .position(|d| d.presented.info.id == *device)
        {
            let removed = state.devices.remove(ix);
            state.unplugged.push(removed);
        }
    }

    /// Reattaches a device detached by [`Self::unplug`].
    pub fn plug_in(&self, device: &DeviceId) {
        let mut state = lock(&self.state);
        if let Some(ix) = state
            .unplugged
            .iter()
            .position(|d| d.presented.info.id == *device)
        {
            let returned = state.unplugged.remove(ix);
            state.devices.push(returned);
        }
    }

    /// Changes what a device's next battery read reports.
    pub fn set_battery(&self, device: &DeviceId, battery: BatteryStatus) {
        self.simulate_hardware_change(device, |stored| stored.battery = Some(battery));
    }

    /// Sets the peripherals that are attached but can't be opened.
    pub fn set_unavailable(&self, unavailable: Vec<UnavailableDevice>) {
        let mut state = lock(&self.state);
        state.unavailable = unavailable;
        state.is_openable = false;
    }

    /// Lets the unavailable peripherals open, as exiting a vendor app would.
    pub fn release_unavailable(&self) {
        lock(&self.state).is_openable = true;
    }

    /// Injects simulated live Hall Effect travel samples for a mock keyboard (for tests & `--mock`).
    pub fn set_live_key_travel(&self, device: &DeviceId, samples: Vec<KeyTravelSample>) {
        let mut state = lock(&self.state);
        if let Some(target) = state
            .devices
            .iter_mut()
            .find(|d| d.presented.info.id == *device)
        {
            target.live_travel = samples;
        }
    }

    /// Causes the next `count` live travel reads on `device` to fail with `message`,
    /// simulating a transient USB disconnect (`0x0000048F`) for self-recovery tests.
    pub fn fail_live_key_travel(
        &self,
        device: &DeviceId,
        count: usize,
        message: impl Into<String>,
    ) {
        let mut state = lock(&self.state);
        if let Some(target) = state
            .devices
            .iter_mut()
            .find(|d| d.presented.info.id == *device)
        {
            target.live_travel_error_count = count;
            target.live_travel_error_message = Some(message.into());
        }
    }

    /// Mutates a mock device's stored onboard state as if changed directly via hardware
    /// shortcuts (e.g. `Fn + RGB` on a keyboard), without updating `presented` until synced.
    pub fn simulate_hardware_change(
        &self,
        device: &DeviceId,
        edit: impl FnOnce(&mut DeviceSnapshot),
    ) {
        let mut state = lock(&self.state);
        if let Some(target) = state
            .devices
            .iter_mut()
            .find(|d| d.stored.info.id == *device)
        {
            edit(&mut target.stored);
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
        state.attached = state.interfaces();
        if state.is_openable {
            state.unavailable.clear();
            state.is_openable = false;
        }
        Ok(state.devices.iter().map(|d| d.presented.clone()).collect())
    }

    fn unavailable_devices(&self) -> Vec<UnavailableDevice> {
        lock(&self.state).unavailable.clone()
    }

    fn changed_interfaces(&self) -> CoreResult<Option<BTreeSet<String>>> {
        let state = lock(&self.state);
        let attached = state.interfaces();
        Ok((attached != state.attached).then_some(attached))
    }

    fn can_open_unavailable(&self) -> bool {
        let state = lock(&self.state);
        state.is_openable && !state.unavailable.is_empty()
    }

    fn read_battery(&self, device: &DeviceId) -> CoreResult<BatteryStatus> {
        let state = lock(&self.state);
        state
            .devices
            .iter()
            .find(|d| d.presented.info.id == *device)
            .ok_or_else(|| CoreError::DeviceNotFound(device.to_string()))?
            .stored
            .battery
            .ok_or(CoreError::UnsupportedCapability("battery"))
    }

    fn apply(
        &self,
        device: &DeviceId,
        change: SettingChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        let mut state = lock(&self.state);
        let dry_run = state.dry_run;
        let target = state
            .devices
            .iter_mut()
            .find(|d| d.presented.info.id == *device)
            .ok_or_else(|| ApplyError::rejected(CoreError::DeviceNotFound(device.to_string())))?;

        validate_change(&target.presented, &change).map_err(ApplyError::rejected)?;
        let frames = target
            .codec
            .encode(&change, &target.presented)
            .map_err(ApplyError::rejected)?;
        let is_feature = target.codec.report_kind() == ReportKind::Feature;
        for frame in frames {
            let sent = if is_feature {
                target.transport.send_feature_report(&frame)
            } else {
                target.transport.send_output_report(&frame).map(|_| ())
            };
            sent.map_err(|err| ApplyError::failed_after_send(err.into(), None))?;
        }

        let is_reset = matches!(change, SettingChange::ResetKeymap);
        if dry_run {
            apply_change_to_snapshot(&mut target.presented, &change);
            if is_reset {
                target.presented.keymap = target.factory.keymap.clone();
            }
            target.presented.is_preview = true;
        } else {
            apply_change_to_snapshot(&mut target.stored, &change);
            if is_reset {
                target.stored.keymap = target.factory.keymap.clone();
            }
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

    fn read_live_key_travel(&self, device: &DeviceId) -> CoreResult<Vec<KeyTravelSample>> {
        let mut state = lock(&self.state);
        let target = state
            .devices
            .iter_mut()
            .find(|d| d.presented.info.id == *device)
            .ok_or_else(|| CoreError::DeviceNotFound(device.to_string()))?;
        let Some(he) = target.presented.hall_effect.as_ref() else {
            return Err(CoreError::UnsupportedCapability("hall_effect"));
        };
        if !he.capabilities.has_live_travel() {
            return Err(CoreError::UnsupportedCapability("live_key_travel"));
        }
        if target.live_travel_error_count > 0 {
            target.live_travel_error_count -= 1;
            let message = target
                .live_travel_error_message
                .clone()
                .unwrap_or_else(|| "The device is not connected.".to_owned());
            return Err(CoreError::Transport(message));
        }
        Ok(target.live_travel.clone())
    }

    fn sync_hardware_state(&self, device: &DeviceId) -> CoreResult<Option<DeviceSnapshot>> {
        let mut state = lock(&self.state);
        let target = state
            .devices
            .iter_mut()
            .find(|d| d.presented.info.id == *device)
            .ok_or_else(|| CoreError::DeviceNotFound(device.to_string()))?;
        if target.presented.is_preview {
            return Ok(None);
        }
        let mut next = target.presented.clone();
        let mut changed = false;
        if next.rgb != target.stored.rgb {
            next.rgb.clone_from(&target.stored.rgb);
            changed = true;
        }
        if next.keyboard_config != target.stored.keyboard_config {
            next.keyboard_config
                .clone_from(&target.stored.keyboard_config);
            changed = true;
        }
        if next.profiles != target.stored.profiles {
            next.profiles.clone_from(&target.stored.profiles);
            next.pointer.clone_from(&target.stored.pointer);
            changed = true;
        }
        if changed {
            target.presented = next.clone();
            Ok(Some(next))
        } else {
            Ok(None)
        }
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
    .with_firmware_version("v2.17");

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
            .with_profiles(true)
            .with_macros(true),
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
        keymap: None,
        macros: Some(lamzu_macros()),
        keyboard_config: None,
        warnings: Vec::new(),
        is_preview: false,
    }
}

/// Macros on the side buttons' key slots (3 = back, 4 = forward); the main buttons stay
/// unbound. Slot 5 mirrors the captured `spam click` mouse-button macro (now editable);
/// slot 7 simulates an unsupported ACPI Power event that remains read-only.
fn lamzu_macros() -> MacroState {
    let mut slots: Vec<MacroSlot> = (0..crystall::layout::MACRO_SLOT_COUNT)
        .map(|ix| MacroSlot::new(ix, "", Vec::new()))
        .collect();
    slots[3] = MacroSlot::new(
        3,
        "Burst",
        vec![
            MacroStep::new(MacroActionKind::KeyDown, 0x04, 20),
            MacroStep::new(MacroActionKind::KeyUp, 0x04, 20),
        ],
    )
    .with_repeat(3);
    slots[4] = MacroSlot::new(
        4,
        "Jump crouch",
        vec![
            MacroStep::new(MacroActionKind::KeyDown, 0x2C, 10),
            MacroStep::new(MacroActionKind::KeyDown, 0xE0, 30),
            MacroStep::new(MacroActionKind::KeyUp, 0xE0, 0),
            MacroStep::new(MacroActionKind::KeyUp, 0x2C, 0),
        ],
    );
    slots[5] = MacroSlot::new(
        5,
        "spam click",
        vec![
            MacroStep::mouse(MacroActionKind::MouseDown, MouseButton::Left, 10),
            MacroStep::mouse(MacroActionKind::MouseUp, MouseButton::Left, 10),
        ],
    );
    slots[7] = MacroSlot::new(7, "Power event", Vec::new()).with_readonly(true);
    MacroState {
        capabilities: MacroCapabilities::new(
            crystall::layout::MACRO_SLOT_COUNT,
            u16::from(crystall::layout::MACRO_SLOT_COUNT) * crystall::layout::MACRO_SLOT_STRIDE,
        )
        .with_repeat_support(true)
        .with_mouse_buttons(true)
        .with_media_keys(true)
        .with_pointer_move(true),
        slots,
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
    .with_calibration_status(true)
    .with_live_travel(true);

    DeviceSnapshot {
        info,
        capabilities: Capabilities::empty()
            .with_hall_effect(true)
            .with_rgb(true)
            .with_keymap(true)
            .with_macros(true)
            .with_keyboard_config(true),
        pointer: None,
        hall_effect: Some(HallEffectState {
            capabilities,
            snapshot: atk_rs7_hall_effect(),
        }),
        rgb: Some(atk_rs7_rgb()),
        battery: None,
        profiles: None,
        keymap: Some(atk_rs7_keymap()),
        macros: Some(atk_rs7_macros()),
        keyboard_config: Some(atk_rs7_keyboard_config()),
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
    let rs = RapidSwitchPair::new(atk::row_col_to_key(2, 2), atk::row_col_to_key(3, 2))
        .unwrap_or_else(|_| unreachable!("fixture RS keys are distinct"));
    HallEffectSnapshot::new(keys, vec![dks], vec![socd], CalibrationStatus::Calibrated)
        .with_rapid_switch_pairs(vec![rs])
        .with_calibrated_keys(vec![true; usize::from(atk::RS7_KEY_COUNT)])
}

fn atk_rs7_rgb() -> RgbState {
    let rgb_capabilities = RgbCapabilities::new(
        vec![
            RgbZoneInfo::new(RgbZoneId::new(0), "Back Lamp")
                .with_presets(atk::back_lamp_presets())
                .with_max_speed(255),
            RgbZoneInfo::new(RgbZoneId::new(1), "Logo Light")
                .with_presets(atk::logo_light_presets())
                .with_max_speed(40),
        ],
        vec![
            RgbEffectKind::Off,
            RgbEffectKind::Static,
            RgbEffectKind::Breathing,
            RgbEffectKind::SpectrumCycle,
            RgbEffectKind::Wave,
            RgbEffectKind::Reactive,
            RgbEffectKind::Preset,
        ],
        true,
    );
    let brightness =
        Brightness::new(80).unwrap_or_else(|_| unreachable!("fixture brightness is a percentage"));
    let logo_brightness =
        Brightness::new(75).unwrap_or_else(|_| unreachable!("fixture brightness is a percentage"));
    RgbState {
        capabilities: rgb_capabilities,
        snapshot: RgbSnapshot::new(vec![
            RgbZoneState::new(
                RgbZoneId::new(0),
                RgbEffect::Preset {
                    id: 2,
                    color: RgbColor::new(255, 255, 255),
                    speed: 128,
                    colorful: false,
                },
                brightness,
            ),
            RgbZoneState::new(
                RgbZoneId::new(1),
                RgbEffect::Preset {
                    id: 3,
                    color: RgbColor::new(255, 255, 255),
                    speed: 20,
                    colorful: false,
                },
                logo_brightness,
            ),
        ]),
    }
}

/// Each RS7 matrix position sending its own key, as HID usages (`0` = no key there).
const RS7_BASE_LAYER: [[u8; 16]; 6] = [
    [
        0x29, 0x3A, 0x3B, 0x3C, 0x3D, 0x00, 0x3E, 0x3F, 0x40, 0x41, 0x00, 0x42, 0x43, 0x44, 0x45,
        0x4C,
    ],
    [
        0x35, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x00, 0x2A,
        0x49,
    ],
    [
        0x2B, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x00, 0x31,
        0x4B,
    ],
    [
        0x39, 0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x00, 0x00, 0x28,
        0x4E,
    ],
    [
        0xE1, 0x00, 0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0xE5, 0x00, 0x52,
        0x00,
    ],
    [
        0xE0, 0xE3, 0xE2, 0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0xE6, 0x00, 0x50, 0x00, 0x51,
        0x4F,
    ],
];

/// The Fn key at row 5, column 11: holds the Fn layer.
const RS7_FN_KEY: usize = 5 * 16 + 11;

fn atk_rs7_keymap() -> KeymapState {
    let mut base_layer: Vec<KeyCode> = RS7_BASE_LAYER
        .iter()
        .flatten()
        .map(|&usage| KeyCode::standard(usage))
        .collect();
    base_layer.resize(usize::from(atk::RS7_KEY_COUNT), KeyCode::standard(0));
    base_layer[RS7_FN_KEY] = KeyCode::new(0x52, 1);
    let transparent = vec![KeyCode::TRANSPARENT; usize::from(atk::RS7_KEY_COUNT)];
    KeymapState {
        capabilities: KeymapCapabilities::new(
            vec![
                "Win Base".to_owned(),
                "Win Fn".to_owned(),
                "Mac Base".to_owned(),
                "Mac Fn".to_owned(),
            ],
            atk::RS7_ROWS,
            atk::RS7_COLS,
            true,
        ),
        snapshot: KeymapSnapshot::new(
            vec![
                base_layer.clone(),
                transparent.clone(),
                base_layer,
                transparent,
            ],
            0,
        ),
    }
}

fn atk_rs7_macros() -> MacroState {
    let mut slots: Vec<MacroSlot> = (0..atk::RS7_MAX_MACRO_SLOTS)
        .map(|ix| MacroSlot::new(ix, format!("M{}", ix + 1), Vec::new()))
        .collect();
    slots[0].steps = vec![
        MacroStep::new(MacroActionKind::KeyDown, 0x04, 25),
        MacroStep::new(MacroActionKind::KeyUp, 0x04, 0),
    ];
    MacroState {
        capabilities: MacroCapabilities::new(atk::RS7_MAX_MACRO_SLOTS, 512),
        slots,
    }
}

fn atk_rs7_keyboard_config() -> KeyboardConfigState {
    KeyboardConfigState {
        capabilities: KeyboardConfigCapabilities::new(
            true,
            true,
            true,
            true,
            true,
            true,
            vec![
                PollingRateHz::Hz1000,
                PollingRateHz::Hz2000,
                PollingRateHz::Hz4000,
                PollingRateHz::Hz8000,
            ],
        ),
        snapshot: KeyboardConfigSnapshot::new(
            false,
            false,
            false,
            false,
            false,
            false,
            PollingRateHz::Hz8000,
        ),
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
                &atk_rs7_snapshot(),
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
        let pointer = snapshot.pointer.clone().unwrap();
        let change = SettingChange::Pointer(PointerChange::DpiStages {
            stages: pointer.snapshot.dpi_stages,
            active: 2,
        });
        let frames = MockCodec::LamzuAtlantis(AtlantisRateFamily::Wired)
            .encode(&change, &snapshot)
            .unwrap();
        assert_eq!(frames.len(), 1 + 4 * 2 + 1);
        let validator = atlantis::atlantis_validator();
        for frame in &frames {
            validator
                .validate_outgoing(ReportKind::Output, frame)
                .unwrap();
        }
    }

    #[test]
    fn multi_brand_lists_every_capability_mix_and_refreshes() {
        let service = MockPeripheralService::multi_brand();
        let devices = service.refresh().unwrap();
        let ids: Vec<&str> = devices.iter().map(|d| d.id().as_str()).collect();
        assert_eq!(
            ids,
            [
                MOCK_MOUSE_ID,
                MOCK_KEYBOARD_ID,
                MOCK_RAZER_ID,
                MOCK_PULSAR_ID,
                MOCK_VIA_ID,
                MOCK_WOOTING_ID
            ]
        );
        let razer = &devices[2];
        assert!(razer.capabilities.has_pointer() && razer.capabilities.has_rgb());
        let via = &devices[4];
        assert!(via.capabilities.has_keymap() && !via.capabilities.has_hall_effect());
    }

    #[test]
    fn new_codec_writes_pass_their_driver_validators() {
        let service = MockPeripheralService::multi_brand();
        service.refresh().unwrap();
        service.set_dry_run(false);
        service.clear_packet_log();

        let razer = DeviceId::new(MOCK_RAZER_ID);
        let after = service
            .apply_rgb(
                &razer,
                RgbZoneId::new(4),
                RgbEffect::Static {
                    color: RgbColor::new(0, 0, 255),
                },
                Brightness::new(50).unwrap(),
            )
            .unwrap();
        let logo = after.rgb.unwrap().snapshot.zones;
        assert!(logo.iter().any(|z| z.zone == RgbZoneId::new(4)
            && z.effect
                == RgbEffect::Static {
                    color: RgbColor::new(0, 0, 255)
                }));

        let pulsar = DeviceId::new(MOCK_PULSAR_ID);
        let after = service
            .apply_pointer_settings(
                &pulsar,
                PointerChange::LiftOffDistance(LiftOffDistance::High),
            )
            .unwrap();
        assert_eq!(
            after.pointer.unwrap().snapshot.lod,
            Some(LiftOffDistance::High)
        );
        let after = service
            .apply_pointer_settings(&pulsar, PointerChange::AngleSnapping(true))
            .unwrap();
        assert_eq!(after.pointer.unwrap().snapshot.angle_snapping, Some(true));

        let via = DeviceId::new(MOCK_VIA_ID);
        let after = service
            .apply(
                &via,
                SettingChange::KeyBinding {
                    layer: 1,
                    key: KeyIndex::new(17),
                    code: KeyCode::standard(0x04),
                },
            )
            .unwrap();
        assert_eq!(
            after.keymap.unwrap().snapshot.layers[1][17],
            KeyCode::standard(0x04)
        );

        let log = service.packet_log();
        assert_ne!(log.len(), 0);
        assert!(
            log.iter()
                .all(|entry| entry.outcome == PacketOutcome::Transmitted)
        );
    }

    #[test]
    fn readonly_wooting_actuation_is_rejected_before_send() {
        let service = MockPeripheralService::multi_brand();
        service.refresh().unwrap();
        service.set_dry_run(false);
        service.clear_packet_log();
        let err = service
            .apply_actuation(
                &DeviceId::new(MOCK_WOOTING_ID),
                ActuationTarget::AllKeys,
                travel(150),
            )
            .unwrap_err();
        assert!(err.rejected_before_send);
        assert!(matches!(err.error, CoreError::UnsupportedCapability(_)));
        assert_eq!(service.packet_log(), Vec::new());
    }

    #[test]
    fn keymap_reset_restores_the_devices_own_factory_layers() {
        let service = MockPeripheralService::multi_brand();
        service.refresh().unwrap();
        let via = DeviceId::new(MOCK_VIA_ID);
        let factory = service.refresh().unwrap()[4].keymap.clone();
        service
            .apply(
                &via,
                SettingChange::KeyBinding {
                    layer: 0,
                    key: KeyIndex::new(0),
                    code: KeyCode::standard(0x04),
                },
            )
            .unwrap();
        let reset = service.apply(&via, SettingChange::ResetKeymap).unwrap();
        assert_eq!(reset.keymap, factory);
    }
}
