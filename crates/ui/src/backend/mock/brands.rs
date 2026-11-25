//! Emulated Razer, Pulsar, QMK/VIA and Wooting peripherals for [`super::MockPeripheralService`].
//!
//! Each fixture mirrors the capabilities its real driver reports, and every write is encoded
//! with that driver's public protocol helpers, so the packet inspector shows genuine frames.

use agnoperiph_core::{
    BatteryStatus, Brightness, CalibrationStatus, Capabilities, ChargingState, ConnectionKind,
    CoreError, CoreResult, DebounceMs, DeviceId, DeviceInfo, DpiStage, HallEffectCapabilities,
    HallEffectSnapshot, KeyCode, KeyHallEffectConfig, KeyIndex, KeymapCapabilities, KeymapSnapshot,
    LiftOffDistance, MacroActionKind, MacroCapabilities, MacroSlot, MacroStep, PointerCapabilities,
    PointerSnapshot, PollingRateHz, ProfileCapabilities, ProfileInfo, ProfileMode, ProfileSlot,
    ProfileSnapshot, RapidTriggerConfig, RgbCapabilities, RgbColor, RgbEffect, RgbEffectKind,
    RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState, VendorKind,
};
use agnoperiph_driver_pulsar::protocol::{self as pulsar, Command as PulsarCommand};
use agnoperiph_driver_razer::protocol::{self as razer, RazerLightingFamily};
use agnoperiph_driver_via::protocol as via;
use agnoperiph_driver_wooting::protocol as wooting;
use agnoperiph_transport::HidDeviceDescriptor;

pub(super) use agnoperiph_driver_pulsar::protocol::pulsar_validator;
pub(super) use agnoperiph_driver_razer::protocol::{RazerProductProfile, razer_validator};
pub(super) use agnoperiph_driver_via::protocol::via_validator;
pub(super) use agnoperiph_driver_wooting::protocol::{WootingProductProfile, wooting_validator};

use super::{
    DeviceSnapshot, HallEffectState, KeymapState, MacroState, MockCodec, PointerChange,
    PointerState, ProfileState, RgbState, SettingChange, dpi, travel,
};

/// Stable id of the emulated Razer Basilisk V3 (pointer with three RGB zones).
pub const MOCK_RAZER_ID: &str = "razer:1532:0099:mock-basilisk-v3";
/// Stable id of the emulated Pulsar X2 CrazyLight (Low / Medium / High lift-off).
pub const MOCK_PULSAR_ID: &str = "pulsar:3710:0001:mock-x2";
/// Stable id of the emulated Keychron Q1 (QMK/VIA keymap, macros and RGB, no Hall Effect).
pub const MOCK_VIA_ID: &str = "via:3434:0100:mock-q1";
/// Stable id of the emulated Wooting 60HE (readonly actuation, 6 x 14 matrix).
pub const MOCK_WOOTING_ID: &str = "wooting:31E3:1302:mock-60he";

const RAZER_BASILISK_V3_PID: u16 = 0x0099;
/// Pulsar's match rule is VID + usage only, so the receiver PID is a placeholder.
const PULSAR_MOCK_PID: u16 = 0x0001;
/// The VIA driver matches any Keychron PID, so the keyboard PID is a placeholder.
const VIA_MOCK_PID: u16 = 0x0100;
const WOOTING_60HE_PID: u16 = 0x1302;
const VIA_ROWS: u8 = 6;
const VIA_COLS: u8 = 16;
const VIA_LAYERS: u8 = 4;
const VIA_MACRO_SLOTS: u8 = 16;
const VIA_MACRO_BYTES: u16 = 1024;
const WOOTING_PROFILE_SLOTS: u8 = 4;

fn razer_profile() -> RazerProductProfile {
    razer::lookup_product(RAZER_BASILISK_V3_PID)
        .unwrap_or_else(|| unreachable!("Basilisk V3 is in the Razer product catalog"))
}

fn wooting_profile() -> WootingProductProfile {
    wooting::lookup_product(WOOTING_60HE_PID)
        .unwrap_or_else(|| unreachable!("60HE is in the Wooting product catalog"))
}

fn brightness(percent: u8) -> Brightness {
    Brightness::new(percent).unwrap_or_else(|_| unreachable!("fixture brightness is a percentage"))
}

/// Every extra fixture, in sidebar order.
pub(super) fn fixtures() -> Vec<(HidDeviceDescriptor, MockCodec, DeviceSnapshot)> {
    let razer_profile = razer_profile();
    let wooting_profile = wooting_profile();
    vec![
        (
            HidDeviceDescriptor::new(
                razer::RAZER_VID,
                RAZER_BASILISK_V3_PID,
                razer::RAZER_USAGE_PAGE_MOUSE,
                razer::RAZER_USAGE_ID_MOUSE,
                0,
                "mock://razer/basilisk-v3",
            ),
            MockCodec::Razer(razer_profile),
            razer_snapshot(razer_profile),
        ),
        (
            HidDeviceDescriptor::new(
                pulsar::PULSAR_VID,
                PULSAR_MOCK_PID,
                pulsar::PULSAR_USAGE_PAGE,
                pulsar::PULSAR_USAGE_ID,
                1,
                "mock://pulsar/x2",
            ),
            MockCodec::Pulsar,
            pulsar_snapshot(),
        ),
        (
            HidDeviceDescriptor::new(
                0x3434,
                VIA_MOCK_PID,
                via::VIA_USAGE_PAGE,
                via::VIA_USAGE_ID,
                1,
                "mock://keychron/q1",
            ),
            MockCodec::Via { cols: VIA_COLS },
            via_snapshot(),
        ),
        (
            HidDeviceDescriptor::new(
                wooting::WOOTING_VID,
                WOOTING_60HE_PID,
                wooting::WOOTING_CONFIG_USAGE_PAGE,
                wooting::WOOTING_CONFIG_USAGE_ID,
                2,
                "mock://wooting/60he",
            ),
            MockCodec::Wooting(wooting_profile),
            wooting_snapshot(wooting_profile),
        ),
    ]
}

fn stages(values: &[u32]) -> Vec<DpiStage> {
    values
        .iter()
        .zip(0u8..)
        .map(|(&value, ix)| DpiStage::new(ix, dpi(value)))
        .collect()
}

fn razer_snapshot(profile: RazerProductProfile) -> DeviceSnapshot {
    let info = DeviceInfo::new(
        DeviceId::new(MOCK_RAZER_ID),
        VendorKind::Razer,
        profile.model_name,
        profile.connection,
        razer::RAZER_VID,
        profile.pid,
    )
    .with_firmware_version("v1.04");
    let capabilities = PointerCapabilities::new(
        razer::RAZER_MIN_DPI,
        profile.max_dpi,
        razer::RAZER_DPI_STEP,
        razer::RAZER_MAX_DPI_STAGES,
    )
    .with_xy_independent_dpi(true)
    .with_polling_rates(razer::supported_rates_for(profile));
    let pointer = PointerSnapshot::new(
        1,
        stages(&[400, 800, 1600, 3200, 6400]),
        PollingRateHz::Hz1000,
    );

    let zones = vec![
        RgbZoneInfo::new(RgbZoneId::new(razer::led::ALL), "Mouse"),
        RgbZoneInfo::new(RgbZoneId::new(razer::led::SCROLL_WHEEL), "Scroll Wheel"),
        RgbZoneInfo::new(RgbZoneId::new(razer::led::LOGO), "Logo"),
    ];
    let states = vec![
        RgbZoneState::new(
            RgbZoneId::new(razer::led::ALL),
            RgbEffect::Static {
                color: razer::default_static_color(),
            },
            brightness(80),
        ),
        RgbZoneState::new(
            RgbZoneId::new(razer::led::SCROLL_WHEEL),
            RgbEffect::Static {
                color: razer::default_static_color(),
            },
            brightness(80),
        ),
        RgbZoneState::new(
            RgbZoneId::new(razer::led::LOGO),
            RgbEffect::Breathing {
                color: RgbColor::new(0x44, 0xD6, 0x2C),
                speed: 2,
            },
            brightness(60),
        ),
    ];
    let rgb = RgbState {
        capabilities: RgbCapabilities::new(
            zones,
            vec![
                RgbEffectKind::Off,
                RgbEffectKind::Static,
                RgbEffectKind::Breathing,
                RgbEffectKind::SpectrumCycle,
                RgbEffectKind::Wave,
                RgbEffectKind::Reactive,
            ],
            false,
        ),
        snapshot: RgbSnapshot::new(states),
    };

    DeviceSnapshot {
        info,
        capabilities: Capabilities::empty().with_pointer(true).with_rgb(true),
        pointer: Some(PointerState {
            capabilities,
            snapshot: pointer,
        }),
        hall_effect: None,
        rgb: Some(rgb),
        battery: None,
        profiles: None,
        keymap: None,
        macros: None,
        keyboard_config: None,
        warnings: Vec::new(),
        is_preview: false,
    }
}

fn pulsar_snapshot() -> DeviceSnapshot {
    use pulsar::flash;
    let info = DeviceInfo::new(
        DeviceId::new(MOCK_PULSAR_ID),
        VendorKind::Pulsar,
        "Pulsar X2 CrazyLight",
        ConnectionKind::WirelessDongle24Ghz,
        pulsar::PULSAR_VID,
        PULSAR_MOCK_PID,
    )
    .with_firmware_version("v1.10");
    let capabilities = PointerCapabilities::new(
        flash::MIN_DPI,
        flash::MAX_DPI,
        flash::DPI_STEP,
        flash::MAX_DPI_STAGES,
    )
    .with_polling_rates(vec![
        PollingRateHz::Hz125,
        PollingRateHz::Hz250,
        PollingRateHz::Hz500,
        PollingRateHz::Hz1000,
        PollingRateHz::Hz2000,
        PollingRateHz::Hz4000,
    ])
    .with_lods(vec![
        LiftOffDistance::Low,
        LiftOffDistance::Medium,
        LiftOffDistance::High,
    ])
    .with_debounce_range_ms(0, flash::MAX_DEBOUNCE_MS)
    .with_motion_sync(true)
    .with_angle_snapping(true)
    .with_ripple_control(true);
    let pointer = PointerSnapshot::new(0, stages(&[800, 1600, 3200]), PollingRateHz::Hz2000)
        .with_lod(LiftOffDistance::Medium)
        .with_debounce_ms(DebounceMs::new(3))
        .with_motion_sync(true)
        .with_angle_snapping(false)
        .with_ripple_control(true);
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
            Some(64),
            None,
            ChargingState::Discharging,
            true,
        )),
        profiles: Some(ProfileState {
            capabilities: ProfileCapabilities::new(flash::PROFILE_COUNT, false, false),
            snapshot: profiles,
        }),
        keymap: None,
        macros: None,
        keyboard_config: None,
        warnings: Vec::new(),
        is_preview: false,
    }
}

/// Base layer of a 75% board on the `6 x 16` VIA matrix, as HID usages (`0` = no key).
const VIA_BASE_LAYER: [[u8; 16]; 6] = [
    [
        0x29, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x4C, 0x00,
        0x00,
    ],
    [
        0x35, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2D, 0x2E, 0x2A, 0x4B,
        0x00,
    ],
    [
        0x2B, 0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, 0x2F, 0x30, 0x31, 0x4E,
        0x00,
    ],
    [
        0x39, 0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, 0x33, 0x34, 0x00, 0x28, 0x4A,
        0x00,
    ],
    [
        0xE1, 0x00, 0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10, 0x36, 0x37, 0x38, 0x00, 0xE5, 0x52,
        0x00,
    ],
    [
        0xE0, 0xE3, 0xE2, 0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0xE6, 0x00, 0xE4, 0x50, 0x51,
        0x4F,
    ],
];

fn via_snapshot() -> DeviceSnapshot {
    let info = DeviceInfo::new(
        DeviceId::new(MOCK_VIA_ID),
        VendorKind::Keychron,
        "Keychron Q1",
        ConnectionKind::WiredUsb,
        0x3434,
        VIA_MOCK_PID,
    )
    .with_firmware_version("VIA v12");

    let base: Vec<KeyCode> = VIA_BASE_LAYER
        .iter()
        .flatten()
        .map(|&usage| KeyCode::standard(usage))
        .collect();
    let transparent = vec![KeyCode::TRANSPARENT; base.len()];
    let mut layers = vec![base];
    layers.extend((1..VIA_LAYERS).map(|_| transparent.clone()));
    let keymap = KeymapState {
        capabilities: KeymapCapabilities::new(
            (0..VIA_LAYERS).map(|ix| format!("Layer {ix}")).collect(),
            VIA_ROWS,
            VIA_COLS,
            true,
        ),
        snapshot: KeymapSnapshot::new(layers, 0),
    };

    let mut slots: Vec<MacroSlot> = (0..VIA_MACRO_SLOTS)
        .map(|ix| MacroSlot::new(ix, format!("M{}", ix + 1), Vec::new()))
        .collect();
    slots[0].steps = vec![
        MacroStep::new(MacroActionKind::KeyDown, 0xE0, 0),
        MacroStep::new(MacroActionKind::KeyDown, 0x06, 0),
        MacroStep::new(MacroActionKind::KeyUp, 0x06, 0),
        MacroStep::new(MacroActionKind::KeyUp, 0xE0, 0),
    ];
    let macros = MacroState {
        capabilities: MacroCapabilities::new(VIA_MACRO_SLOTS, VIA_MACRO_BYTES),
        slots,
    };

    let rgb = RgbState {
        capabilities: RgbCapabilities::new(
            vec![
                RgbZoneInfo::new(RgbZoneId::new(0), "RGB Matrix")
                    .with_presets(via::rgb_matrix_presets()),
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
            false,
        ),
        snapshot: RgbSnapshot::new(vec![RgbZoneState::new(
            RgbZoneId::new(0),
            RgbEffect::Preset {
                id: 13,
                color: RgbColor::WHITE,
                speed: 128,
                colorful: true,
            },
            brightness(70),
        )]),
    };

    DeviceSnapshot {
        info,
        capabilities: Capabilities::empty()
            .with_keymap(true)
            .with_macros(true)
            .with_rgb(true),
        pointer: None,
        hall_effect: None,
        rgb: Some(rgb),
        battery: None,
        profiles: None,
        keymap: Some(keymap),
        macros: Some(macros),
        keyboard_config: None,
        warnings: Vec::new(),
        is_preview: false,
    }
}

fn wooting_snapshot(profile: WootingProductProfile) -> DeviceSnapshot {
    let info = DeviceInfo::new(
        DeviceId::new(MOCK_WOOTING_ID),
        VendorKind::Wooting,
        profile.model_name,
        ConnectionKind::WiredUsb,
        wooting::WOOTING_VID,
        WOOTING_60HE_PID,
    )
    .with_firmware_version("v2.8.1");
    let capabilities =
        HallEffectCapabilities::new(profile.key_count(), travel(10), travel(400), travel(5))
            .with_rapid_trigger_range(travel(10), travel(235), true);
    let keys = (0..profile.key_count())
        .map(|ix| {
            KeyHallEffectConfig::new(
                KeyIndex::new(ix),
                travel(200),
                RapidTriggerConfig::disabled(),
            )
        })
        .collect();
    let profiles = ProfileSnapshot::new(
        ProfileSlot::new(0),
        ProfileMode::OnboardMemory,
        (0..WOOTING_PROFILE_SLOTS)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect(),
    );
    let rgb = RgbState {
        capabilities: RgbCapabilities::new(
            vec![RgbZoneInfo::new(RgbZoneId::new(0), "Per-Key Backlight")],
            vec![RgbEffectKind::Off, RgbEffectKind::Static],
            true,
        ),
        snapshot: RgbSnapshot::new(vec![RgbZoneState::new(
            RgbZoneId::new(0),
            RgbEffect::Static {
                color: RgbColor::WHITE,
            },
            brightness(100),
        )]),
    };

    DeviceSnapshot {
        info,
        capabilities: Capabilities::empty()
            .with_hall_effect(true)
            .with_profiles(true)
            .with_rgb(true),
        pointer: None,
        hall_effect: Some(HallEffectState {
            capabilities,
            snapshot: HallEffectSnapshot::new(
                keys,
                Vec::new(),
                Vec::new(),
                CalibrationStatus::Calibrated,
            ),
        }),
        rgb: Some(rgb),
        battery: None,
        profiles: Some(ProfileState {
            capabilities: ProfileCapabilities::new(WOOTING_PROFILE_SLOTS, false, false),
            snapshot: profiles,
        }),
        keymap: None,
        macros: None,
        keyboard_config: None,
        warnings: Vec::new(),
        is_preview: false,
    }
}

fn razer_frame(command: &razer::RazerCommand, tx: u8) -> CoreResult<Vec<u8>> {
    Ok(razer::encode_request(command, tx)?.to_vec())
}

/// Mirrors `RazerDevice` writes; the active-stage write resends the whole stage table.
pub(super) fn encode_razer(
    profile: RazerProductProfile,
    change: &SettingChange,
    current: &DeviceSnapshot,
) -> CoreResult<Vec<Vec<u8>>> {
    let tx = profile.transaction_id;
    let storage = profile.dpi_storage;
    let frames = match change {
        SettingChange::Pointer(PointerChange::DpiStages { stages, active }) => {
            vec![razer_frame(
                &razer::cmd_set_dpi_stages(stages, *active, storage)?,
                tx,
            )?]
        }
        SettingChange::Pointer(PointerChange::ActiveDpiStage(stage_ix)) => {
            let stages = current
                .pointer
                .as_ref()
                .map_or_else(Vec::new, |p| p.snapshot.dpi_stages.clone());
            vec![razer_frame(
                &razer::cmd_set_dpi_stages(&stages, *stage_ix, storage)?,
                tx,
            )?]
        }
        SettingChange::Pointer(PointerChange::PollingRate(rate)) if !profile.high_rate_polling => {
            vec![razer_frame(&razer::cmd_set_polling_legacy(*rate)?, tx)?]
        }
        SettingChange::Pointer(PointerChange::LiftOffDistance(lod)) => {
            vec![razer_frame(&razer::cmd_set_tracking_distance(*lod)?, tx)?]
        }
        SettingChange::Rgb {
            zone,
            effect,
            brightness,
        } => match profile.lighting {
            RazerLightingFamily::ExtendedMatrix => vec![
                razer_frame(
                    &razer::cmd_set_extended_effect(zone.value(), *effect)?,
                    razer::TX_1F,
                )?,
                razer_frame(
                    &razer::cmd_set_extended_brightness(zone.value(), brightness.percent()),
                    razer::TX_1F,
                )?,
            ],
            RazerLightingFamily::StandardMatrix => vec![
                razer_frame(&razer::cmd_set_standard_effect(*effect)?, tx)?,
                razer_frame(
                    &razer::cmd_set_standard_brightness(brightness.percent()),
                    tx,
                )?,
            ],
            RazerLightingFamily::None => return Err(CoreError::UnsupportedCapability("rgb")),
        },
        _ => return Err(CoreError::UnsupportedCapability("razer_setting")),
    };
    Ok(frames)
}

fn pulsar_flash(address: u16, data: &[u8]) -> CoreResult<Vec<u8>> {
    Ok(pulsar::encode_request(PulsarCommand::WriteFlash, address, data)?.to_vec())
}

fn pulsar_flash_u8(address: u16, value: u8) -> CoreResult<Vec<u8>> {
    pulsar_flash(address, &pulsar::seal_field(&[value]))
}

/// Mirrors the dry-run frames `PulsarDevice` sends (no receiver online handshake).
pub(super) fn encode_pulsar(change: &SettingChange) -> CoreResult<Vec<Vec<u8>>> {
    use pulsar::flash;
    let frames = match change {
        SettingChange::Pointer(PointerChange::ActiveDpiStage(stage_ix)) => {
            vec![pulsar_flash_u8(flash::ACTIVE_DPI_STAGE, *stage_ix)?]
        }
        SettingChange::Pointer(PointerChange::DpiStages { stages, active }) => {
            let count = u8::try_from(stages.len()).unwrap_or(1);
            let mut frames = vec![pulsar_flash_u8(flash::DPI_STAGE_COUNT, count)?];
            for (ix, stage) in stages.iter().enumerate() {
                let offset = u16::try_from(ix).unwrap_or(0) * flash::DPI_STAGE_STRIDE;
                let encoded = pulsar::encode_dpi_stage(stage)?;
                frames.push(pulsar_flash(flash::DPI_STAGES + offset, &encoded)?);
            }
            frames.push(pulsar_flash_u8(flash::ACTIVE_DPI_STAGE, *active)?);
            frames
        }
        SettingChange::Pointer(PointerChange::PollingRate(rate)) => {
            vec![pulsar_flash_u8(
                flash::POLL_RATE,
                pulsar::encode_polling_rate(*rate)?,
            )?]
        }
        SettingChange::Pointer(PointerChange::LiftOffDistance(lod)) => {
            vec![pulsar_flash_u8(
                flash::LIFT_OFF_DISTANCE,
                pulsar::encode_lod(*lod)?,
            )?]
        }
        SettingChange::Pointer(PointerChange::Debounce(debounce)) => {
            vec![pulsar_flash_u8(flash::DEBOUNCE_MS, debounce.value())?]
        }
        SettingChange::Pointer(PointerChange::MotionSync(enabled)) => {
            vec![pulsar_flash_u8(flash::MOTION_SYNC, u8::from(*enabled))?]
        }
        SettingChange::Pointer(PointerChange::AngleSnapping(enabled)) => {
            vec![pulsar_flash_u8(flash::ANGLE_SNAPPING, u8::from(*enabled))?]
        }
        SettingChange::Pointer(PointerChange::RippleControl(enabled)) => {
            vec![pulsar_flash_u8(flash::RIPPLE_CONTROL, u8::from(*enabled))?]
        }
        SettingChange::Profile(slot) => vec![
            pulsar::encode_request(PulsarCommand::SetCurrentConfig, 0, &[slot.index()])?.to_vec(),
        ],
        _ => return Err(CoreError::UnsupportedCapability("pulsar_setting")),
    };
    Ok(frames)
}

/// Mirrors `ViaKeyboardDevice` writes on the QMK RGB Matrix channel.
pub(super) fn encode_via(
    cols: u8,
    change: &SettingChange,
    current: &DeviceSnapshot,
) -> CoreResult<Vec<Vec<u8>>> {
    let payloads = match change {
        SettingChange::KeyBinding { layer, key, code } => {
            let cols = u16::from(cols.max(1));
            let row = u8::try_from(key.value() / cols).unwrap_or(0);
            let col = u8::try_from(key.value() % cols).unwrap_or(0);
            vec![via::req_keymap_set_keycode(*layer, row, col, *code)]
        }
        SettingChange::ResetKeymap => vec![via::req_keymap_reset()],
        SettingChange::Macros(slots) => {
            let max_bytes = current
                .macros
                .as_ref()
                .map_or(0, |m| usize::from(m.capabilities.max_buffer_bytes));
            via::encode_via_macros(slots, max_bytes)
                .chunks(via::VIA_MAX_CHUNK)
                .enumerate()
                .map(|(ix, chunk)| {
                    let offset = u16::try_from(ix * via::VIA_MAX_CHUNK).unwrap_or(u16::MAX);
                    via::req_macro_set_buffer(offset, chunk)
                })
                .collect()
        }
        SettingChange::Rgb {
            effect, brightness, ..
        } => {
            let chan = via::channel::QMK_RGB_MATRIX;
            let (mode, color, speed) = via::encode_rgb_effect(*effect);
            let bright_raw =
                u8::try_from((u16::from(brightness.percent()) * 255 + 50) / 100).unwrap_or(255);
            let mut payloads = vec![
                via::req_custom_set(chan, via::rgb_val::EFFECT, &[mode]),
                via::req_custom_set(chan, via::rgb_val::BRIGHTNESS, &[bright_raw]),
            ];
            if let Some(speed) = speed {
                payloads.push(via::req_custom_set(
                    chan,
                    via::rgb_val::EFFECT_SPEED,
                    &[speed],
                ));
            }
            if let Some(color) = color {
                let (h, s) = via::rgb_to_qmk_hs(color);
                payloads.push(via::req_custom_set(chan, via::rgb_val::COLOR, &[h, s]));
            }
            payloads.push(via::req_custom_save(chan));
            payloads
        }
        _ => return Err(CoreError::UnsupportedCapability("via_setting")),
    };
    Ok(payloads
        .into_iter()
        .map(|payload| via::wrap_tx(payload).to_vec())
        .collect())
}

/// Mirrors `WootingDevice`: lighting writes go out, Hall Effect and profile writes are
/// refused before any frame is sent because the driver's actuation table is readonly.
pub(super) fn encode_wooting(
    profile: WootingProductProfile,
    change: &SettingChange,
) -> CoreResult<Vec<Vec<u8>>> {
    match change {
        SettingChange::Rgb {
            effect, brightness, ..
        } => match effect {
            RgbEffect::Off => Ok(vec![
                wooting::encode_command(wooting::cmd::RESET_ALL, 0, 0, 0, 0, true).to_vec(),
            ]),
            RgbEffect::Static { color } => {
                let scale = |c: u8| {
                    u8::try_from((u16::from(c) * u16::from(brightness.percent()) + 50) / 100)
                        .unwrap_or(0)
                };
                let scaled = RgbColor::new(scale(color.r), scale(color.g), scale(color.b));
                let mut frames = vec![
                    wooting::encode_command(wooting::cmd::COLOR_INIT, 0, 0, 0, 0, true).to_vec(),
                ];
                for row in 0..profile.rows {
                    for col in 0..profile.cols {
                        frames.push(wooting::encode_single_color(row, col, scaled, true).to_vec());
                    }
                }
                Ok(frames)
            }
            _ => Err(CoreError::UnsupportedCapability("wooting_rgb_effect")),
        },
        SettingChange::Actuation { .. } | SettingChange::ActuationMany { .. } => Err(
            CoreError::UnsupportedCapability("wooting_actuation_write_readonly"),
        ),
        SettingChange::RapidTrigger { .. } | SettingChange::RapidTriggerMany { .. } => Err(
            CoreError::UnsupportedCapability("wooting_rapid_trigger_write_readonly"),
        ),
        SettingChange::Profile(_) => Err(CoreError::UnsupportedCapability(
            "wooting_profile_write_readonly",
        )),
        _ => Err(CoreError::UnsupportedCapability("wooting_setting")),
    }
}

pub(super) fn razer_status_query(profile: RazerProductProfile) -> CoreResult<(Vec<u8>, Vec<u8>)> {
    let request = razer::encode_request(&razer::cmd_read_firmware(), profile.transaction_id)?;
    let mut reply = request;
    reply[1] = razer::status::OK;
    Ok((request.to_vec(), reply.to_vec()))
}

pub(super) fn pulsar_status_query(
    battery: Option<BatteryStatus>,
) -> CoreResult<(Vec<u8>, Vec<u8>)> {
    let pct = battery.and_then(|b| b.percentage).unwrap_or(0);
    let request = pulsar::encode_request(PulsarCommand::BatteryLevel, 0, &[])?;
    let reply = pulsar::encode_request(PulsarCommand::BatteryLevel, 0, &[pct])?;
    Ok((request.to_vec(), reply.to_vec()))
}

pub(super) fn via_status_query() -> (Vec<u8>, Vec<u8>) {
    let request = via::req_protocol_version();
    let mut reply = request;
    // Protocol version 12 (`0x000C`), big-endian in bytes 1..=2.
    reply[2] = 0x0C;
    (via::wrap_tx(request).to_vec(), reply.to_vec())
}

pub(super) fn wooting_status_query() -> (Vec<u8>, Vec<u8>) {
    let request = wooting::encode_command(wooting::cmd::GET_VERSION, 0, 0, 0, 0, true);
    (request.to_vec(), request.to_vec())
}
