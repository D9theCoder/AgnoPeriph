//! Peripheral service seam between the GPUI frontend and the HID drivers.
//!
//! Every call on [`PeripheralService`] may block on USB HID I/O, so the UI only
//! invokes it from a background executor. Both implementations route every
//! outgoing frame through `GuardedTransport`, so validation, dry-run
//! suppression and packet tracing behave identically.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use agnoperiph_core::{
    BatteryStatus, Brightness, Capabilities, CoreError, CoreResult, DebounceMs, DeviceId,
    DeviceInfo, DeviceRegistry, DksBinding, DpiStage, HallEffectCapabilities, HallEffectSnapshot,
    KeyCode, KeyHallEffectConfig, KeyIndex, KeyTravelSample, KeyboardConfigCapabilities,
    KeyboardConfigSnapshot, KeymapCapabilities, KeymapSnapshot, LiftOffDistance, MacroCapabilities,
    MacroSlot, ModTapBinding, Peripheral, PointerCapabilities, PointerSnapshot, PollingRateHz,
    ProfileCapabilities, ProfileSlot, ProfileSnapshot, RapidSwitchPair, RapidTriggerConfig,
    RgbCapabilities, RgbEffect, RgbSnapshot, RgbZoneId, SocdPair, ToggleKeyBinding,
    TravelHundredthsMm,
};
use agnoperiph_driver_atk::AtkDevice;
use agnoperiph_driver_endgame_gear::EndgameGearDevice;
use agnoperiph_driver_finalmouse::FinalmouseDevice;
use agnoperiph_driver_lamzu::LamzuAtlantisDevice;
use agnoperiph_driver_logitech::LogitechHidppDevice;
use agnoperiph_driver_mchose::MchoseDevice;
use agnoperiph_driver_pulsar::PulsarDevice;
use agnoperiph_driver_razer::RazerDevice;
use agnoperiph_driver_via::ViaKeyboardDevice;
use agnoperiph_driver_wlmouse::WLmouseDevice;
use agnoperiph_driver_wooting::WootingDevice;
use agnoperiph_transport::{
    HidDeviceDescriptor, HidEnumerator, HidTransport, HidapiEnumerator, MemoryPacketSink,
    PacketSink, PacketTraceEntry, TransportResult,
};

mod mock;

pub use mock::{
    MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MOCK_PULSAR_ID, MOCK_RAZER_ID, MOCK_VIA_ID, MOCK_WOOTING_ID,
    MockPeripheralService,
};

/// Packet frames retained by the live HID packet inspector.
pub const PACKET_LOG_CAPACITY: usize = 500;

/// Pointer capability limits paired with the device's current pointer configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PointerState {
    pub capabilities: PointerCapabilities,
    pub snapshot: PointerSnapshot,
}

/// Hall Effect capability limits paired with the keyboard's current per-key configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HallEffectState {
    pub capabilities: HallEffectCapabilities,
    pub snapshot: HallEffectSnapshot,
}

impl HallEffectState {
    /// Returns the configuration of one key, when the device reported it.
    pub fn key(&self, key: KeyIndex) -> Option<&KeyHallEffectConfig> {
        self.snapshot.keys.iter().find(|config| config.key == key)
    }
}

/// RGB capability limits paired with the current lighting state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbState {
    pub capabilities: RgbCapabilities,
    pub snapshot: RgbSnapshot,
}

/// Onboard profile capability limits paired with the active profile.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileState {
    pub capabilities: ProfileCapabilities,
    pub snapshot: ProfileSnapshot,
}

/// Dynamic keymap capability limits paired with the keyboard's active layers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeymapState {
    pub capabilities: KeymapCapabilities,
    pub snapshot: KeymapSnapshot,
}

/// Onboard macro capability limits paired with the keyboard's macro slots.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MacroState {
    pub capabilities: MacroCapabilities,
    pub slots: Vec<MacroSlot>,
}

/// Advanced keyboard system configuration capability limits and active toggles.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyboardConfigState {
    pub capabilities: KeyboardConfigCapabilities,
    pub snapshot: KeyboardConfigSnapshot,
}

/// Per-capability read warning that did not prevent the device from loading.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceWarning {
    pub capability: &'static str,
    pub message: String,
}

impl DeviceWarning {
    pub fn new(capability: &'static str, message: impl Into<String>) -> Self {
        Self {
            capability,
            message: message.into(),
        }
    }
}

/// Failure returned by [`PeripheralService::apply`], distinguishing pre-send validation errors
/// from mid-flight transport errors and carrying a best-effort live re-read when available.
#[derive(Debug)]
#[non_exhaustive]
pub struct ApplyError {
    pub error: CoreError,
    /// `true` when validation rejected the change before any HID report was sent.
    pub rejected_before_send: bool,
    /// Best-effort snapshot re-read from hardware after a failed live write.
    pub refreshed_snapshot: Option<Box<DeviceSnapshot>>,
}

impl ApplyError {
    pub fn rejected(error: CoreError) -> Self {
        Self {
            error,
            rejected_before_send: true,
            refreshed_snapshot: None,
        }
    }

    pub fn failed_after_send(error: CoreError, refreshed_snapshot: Option<DeviceSnapshot>) -> Self {
        Self {
            error,
            rejected_before_send: false,
            refreshed_snapshot: refreshed_snapshot.map(Box::new),
        }
    }
}

/// Readonly presentation of one connected peripheral, prepared for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeviceSnapshot {
    pub info: DeviceInfo,
    pub capabilities: Capabilities,
    pub pointer: Option<PointerState>,
    pub hall_effect: Option<HallEffectState>,
    pub rgb: Option<RgbState>,
    pub battery: Option<BatteryStatus>,
    pub profiles: Option<ProfileState>,
    pub keymap: Option<KeymapState>,
    pub macros: Option<MacroState>,
    pub keyboard_config: Option<KeyboardConfigState>,
    /// Per-capability read failures that did not prevent the device from loading.
    pub warnings: Vec<DeviceWarning>,
    /// `true` when values include dry-run edits that were validated and logged but never
    /// written to the device. Cleared by the next refresh.
    pub is_preview: bool,
}

impl DeviceSnapshot {
    pub fn id(&self) -> &DeviceId {
        &self.info.id
    }
}

/// Pointer configuration change requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerChange {
    ActiveDpiStage(u8),
    DpiStages { stages: Vec<DpiStage>, active: u8 },
    PollingRate(PollingRateHz),
    LiftOffDistance(LiftOffDistance),
    Debounce(DebounceMs),
    MotionSync(bool),
    AngleSnapping(bool),
    RippleControl(bool),
}

/// Scope of an actuation point change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActuationTarget {
    Key(KeyIndex),
    AllKeys,
}

/// One validated configuration change a service can apply to a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingChange {
    Pointer(PointerChange),
    Actuation {
        target: ActuationTarget,
        point: TravelHundredthsMm,
    },
    ActuationMany {
        keys: Vec<KeyIndex>,
        point: TravelHundredthsMm,
    },
    RapidTrigger {
        key: KeyIndex,
        config: RapidTriggerConfig,
    },
    RapidTriggerMany {
        keys: Vec<KeyIndex>,
        config: RapidTriggerConfig,
    },
    Deadzones {
        top: TravelHundredthsMm,
        bottom: TravelHundredthsMm,
    },
    /// Writes a DKS binding into the hardware slot `binding.key` (`0..max_dks_bindings`).
    Dks(DksBinding),
    ClearDks {
        slot: KeyIndex,
    },
    Socd(Vec<SocdPair>),
    RapidSwitch(Vec<RapidSwitchPair>),
    ModTap(ModTapBinding),
    ClearModTap {
        slot: u8,
        key: KeyIndex,
    },
    ToggleKey(ToggleKeyBinding),
    ClearToggleKey {
        slot: u8,
        key: KeyIndex,
    },
    KeyBinding {
        layer: u8,
        key: KeyIndex,
        code: KeyCode,
    },
    ResetKeymap,
    Macros(Vec<MacroSlot>),
    KeyboardConfig(KeyboardConfigSnapshot),
    StartCalibration,
    FinishCalibration,
    PollCalibrationProgress,
    Rgb {
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    },
    Profile(ProfileSlot),
}

/// Blocking peripheral backend used by the frontend.
///
/// Implementations own the HID transports. Each call may block, so callers run it on
/// a background executor.
pub trait PeripheralService: Send + Sync {
    /// Returns whether mutating writes are validated and logged without reaching hardware.
    fn is_dry_run(&self) -> bool;

    /// Switches dry-run mode. Open device handles are closed; call [`Self::refresh`] next.
    fn set_dry_run(&self, dry_run: bool);

    /// Re-enumerates devices and reads their current state with readonly queries.
    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>>;

    /// Validates and applies one change, returning the device's resulting snapshot.
    fn apply(&self, device: &DeviceId, change: SettingChange)
    -> Result<DeviceSnapshot, ApplyError>;

    /// Validates and applies a sequence of changes, stopping at the first failure.
    ///
    /// Returns the latest confirmed snapshot (when at least one step succeeded) and any
    /// `(failed_step_index, total_steps, error)`.
    #[allow(clippy::type_complexity)]
    fn apply_batch(
        &self,
        device: &DeviceId,
        changes: Vec<SettingChange>,
    ) -> (Option<DeviceSnapshot>, Option<(usize, usize, ApplyError)>) {
        let total = changes.len();
        let mut last = None;
        for (ix, change) in changes.into_iter().enumerate() {
            match self.apply(device, change) {
                Ok(snapshot) => last = Some(snapshot),
                Err(err) => return (last, Some((ix, total, err))),
            }
        }
        (last, None)
    }

    /// Returns the most recent traced HID frames, oldest first.
    fn packet_log(&self) -> Vec<PacketTraceEntry>;

    fn clear_packet_log(&self);

    /// Reads currently depressed keys and their live travel depth for Hall Effect boards
    /// advertising [`HallEffectCapabilities::supports_live_travel`].
    fn read_live_key_travel(&self, device: &DeviceId) -> CoreResult<Vec<KeyTravelSample>> {
        let _ = device;
        Err(CoreError::UnsupportedCapability("live_key_travel"))
    }

    /// Reads live analog travel with an optional focus key hint for high-rate key tester
    /// polling. Defaults to [`Self::read_live_key_travel`].
    fn read_live_key_travel_focused(
        &self,
        device: &DeviceId,
        focus_key: Option<KeyIndex>,
    ) -> CoreResult<Vec<KeyTravelSample>> {
        let _ = focus_key;
        self.read_live_key_travel(device)
    }

    /// Lightweight read-only sync for settings users can change directly on hardware
    /// (RGB lighting, keyboard toggles like Win Lock / Mac mode, and active onboard profile).
    ///
    /// Returns `Ok(Some(snapshot))` only when hardware state changed from the cached snapshot.
    fn sync_hardware_state(&self, device: &DeviceId) -> CoreResult<Option<DeviceSnapshot>> {
        let _ = device;
        Ok(None)
    }

    fn apply_pointer_settings(
        &self,
        device: &DeviceId,
        change: PointerChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(device, SettingChange::Pointer(change))
    }

    fn apply_actuation(
        &self,
        device: &DeviceId,
        target: ActuationTarget,
        point: TravelHundredthsMm,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(device, SettingChange::Actuation { target, point })
    }

    fn apply_rapid_trigger(
        &self,
        device: &DeviceId,
        key: KeyIndex,
        config: RapidTriggerConfig,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(device, SettingChange::RapidTrigger { key, config })
    }

    fn apply_socd(
        &self,
        device: &DeviceId,
        pairs: Vec<SocdPair>,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(device, SettingChange::Socd(pairs))
    }

    fn apply_dks(
        &self,
        device: &DeviceId,
        binding: DksBinding,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(device, SettingChange::Dks(binding))
    }

    fn apply_rgb(
        &self,
        device: &DeviceId,
        zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(
            device,
            SettingChange::Rgb {
                zone,
                effect,
                brightness,
            },
        )
    }

    fn switch_profile(
        &self,
        device: &DeviceId,
        slot: ProfileSlot,
    ) -> Result<DeviceSnapshot, ApplyError> {
        self.apply(device, SettingChange::Profile(slot))
    }
}

/// Renumbers frames from every per-device `GuardedTransport` into one monotonic sequence.
///
/// Each guarded transport counts from 1, so without this the inspector would show
/// colliding sequence numbers across devices and after every reopen.
#[derive(Debug)]
struct SequencedSink {
    log: MemoryPacketSink,
    next: AtomicU64,
}

impl SequencedSink {
    fn new() -> Self {
        Self {
            log: MemoryPacketSink::bounded(PACKET_LOG_CAPACITY),
            next: AtomicU64::new(1),
        }
    }
}

impl PacketSink for SequencedSink {
    fn record(&self, mut entry: PacketTraceEntry) {
        entry.sequence = self.next.fetch_add(1, Ordering::Relaxed);
        self.log.record(entry);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn require<T>(value: Option<T>, capability: &'static str) -> CoreResult<T> {
    value.ok_or(CoreError::UnsupportedCapability(capability))
}

/// Returns every key currently claimed by any paired or advanced key binding (RS, SOCD,
/// DKS, MT, or TGL) along with the short label of the binding that owns it.
///
/// Enforced across both UI pickers and [`validate_change`] so one physical key cannot be
/// double-bound to conflicting advanced triggers.
pub fn occupied_advanced_keys(
    snapshot: &DeviceSnapshot,
) -> std::collections::BTreeMap<KeyIndex, &'static str> {
    let mut map = std::collections::BTreeMap::new();
    let Some(he) = snapshot.hall_effect.as_ref() else {
        return map;
    };
    for pair in &he.snapshot.rapid_switch_pairs {
        map.insert(pair.key_a, "RS");
        map.insert(pair.key_b, "RS");
    }
    for pair in &he.snapshot.socd_pairs {
        map.insert(pair.key_a, "SOCD");
        map.insert(pair.key_b, "SOCD");
    }
    if let Some(layer0) = snapshot
        .keymap
        .as_ref()
        .and_then(|km| km.snapshot.layers.first())
    {
        for binding in &he.snapshot.dks_bindings {
            if let Ok(slot_u8) = u8::try_from(binding.key.value()) {
                let code_id = 5u8.saturating_add(slot_u8);
                if let Some(ix) = layer0
                    .iter()
                    .position(|c| c.class == 0x7E && c.id == code_id)
                    && let Ok(key_ix) = u16::try_from(ix)
                {
                    map.insert(KeyIndex::new(key_ix), "DKS");
                }
            }
        }
    }
    for binding in &he.snapshot.mod_tap_bindings {
        map.insert(binding.key, "MT");
    }
    for binding in &he.snapshot.toggle_bindings {
        map.insert(binding.key, "TGL");
    }
    map
}

fn check_key_exclusive(
    snapshot: &DeviceSnapshot,
    key: KeyIndex,
    ignore_kind: &'static str,
    ignore_slot: Option<u8>,
) -> CoreResult<()> {
    let Some(he) = snapshot.hall_effect.as_ref() else {
        return Ok(());
    };
    let conflict = |owner: &'static str| {
        Err(CoreError::Validation(format!(
            "key #{} is already assigned to {owner}; remove it there first",
            key.value()
        )))
    };
    if ignore_kind != "RS"
        && he
            .snapshot
            .rapid_switch_pairs
            .iter()
            .any(|p| p.key_a == key || p.key_b == key)
    {
        return conflict("RS");
    }
    if ignore_kind != "SOCD"
        && he
            .snapshot
            .socd_pairs
            .iter()
            .any(|p| p.key_a == key || p.key_b == key)
    {
        return conflict("SOCD");
    }
    if let Some(layer0) = snapshot
        .keymap
        .as_ref()
        .and_then(|km| km.snapshot.layers.first())
    {
        for binding in &he.snapshot.dks_bindings {
            if let Ok(slot_u8) = u8::try_from(binding.key.value()) {
                if ignore_kind == "DKS" && ignore_slot == Some(slot_u8) {
                    continue;
                }
                if layer0
                    .get(usize::from(key.value()))
                    .is_some_and(|c| c.class == 0x7E && c.id == 5u8.saturating_add(slot_u8))
                {
                    return conflict("DKS");
                }
            }
        }
    }
    if he
        .snapshot
        .mod_tap_bindings
        .iter()
        .any(|b| b.key == key && !(ignore_kind == "MT" && ignore_slot == Some(b.slot)))
    {
        return conflict("MT");
    }
    if he
        .snapshot
        .toggle_bindings
        .iter()
        .any(|b| b.key == key && !(ignore_kind == "TGL" && ignore_slot == Some(b.slot)))
    {
        return conflict("TGL");
    }
    Ok(())
}

/// Validates a change against the capability limits reported in `snapshot`.
///
/// Drivers validate again before encoding; validating here keeps dry-run previews
/// from ever showing values the device would reject.
#[allow(clippy::too_many_lines)]
pub fn validate_change(snapshot: &DeviceSnapshot, change: &SettingChange) -> CoreResult<()> {
    match change {
        SettingChange::Pointer(change) => {
            let pointer = require(snapshot.pointer.as_ref(), "pointer")?;
            validate_pointer_change(pointer, change)
        }
        SettingChange::Actuation { target, point } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            if let ActuationTarget::Key(key) = target {
                he.capabilities.validate_key(*key)?;
            }
            he.capabilities.validate_actuation(*point)
        }
        SettingChange::ActuationMany { keys, point } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            for &key in keys {
                he.capabilities.validate_key(key)?;
            }
            he.capabilities.validate_actuation(*point)
        }
        SettingChange::RapidTrigger { key, config } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_key(*key)?;
            he.capabilities.validate_rapid_trigger(config)
        }
        SettingChange::RapidTriggerMany { keys, config } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            for &key in keys {
                he.capabilities.validate_key(key)?;
            }
            he.capabilities.validate_rapid_trigger(config)
        }
        SettingChange::Deadzones { top, bottom } => {
            let _ = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            if top.as_hundredths() > 100 {
                return Err(CoreError::OutOfRange {
                    field: "top_deadzone",
                    actual: top.to_string(),
                    expected: "0.00..=1.00 mm".to_owned(),
                });
            }
            if bottom.as_hundredths() > 100 {
                return Err(CoreError::OutOfRange {
                    field: "bottom_deadzone",
                    actual: bottom.to_string(),
                    expected: "0.00..=1.00 mm".to_owned(),
                });
            }
            Ok(())
        }
        SettingChange::Dks(DksBinding { key: slot, .. }) | SettingChange::ClearDks { slot } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            if !he.capabilities.has_dks() {
                return Err(CoreError::UnsupportedCapability("dks"));
            }
            if slot.value() >= u16::from(he.capabilities.max_dks_bindings) {
                return Err(CoreError::OutOfRange {
                    field: "dks_slot",
                    actual: slot.value().to_string(),
                    expected: format!("0..{}", he.capabilities.max_dks_bindings),
                });
            }
            Ok(())
        }
        SettingChange::Socd(pairs) => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_socd_pairs(pairs)?;
            let mut seen = std::collections::BTreeSet::new();
            for pair in pairs {
                if !seen.insert(pair.key_a) || !seen.insert(pair.key_b) {
                    return Err(CoreError::Validation(
                        "a key cannot appear in more than one SOCD pair".to_owned(),
                    ));
                }
                check_key_exclusive(snapshot, pair.key_a, "SOCD", None)?;
                check_key_exclusive(snapshot, pair.key_b, "SOCD", None)?;
            }
            Ok(())
        }
        SettingChange::RapidSwitch(pairs) => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_rapid_switch_pairs(pairs)?;
            let mut seen = std::collections::BTreeSet::new();
            for pair in pairs {
                if !seen.insert(pair.key_a) || !seen.insert(pair.key_b) {
                    return Err(CoreError::Validation(
                        "a key cannot appear in more than one RS pair".to_owned(),
                    ));
                }
                check_key_exclusive(snapshot, pair.key_a, "RS", None)?;
                check_key_exclusive(snapshot, pair.key_b, "RS", None)?;
            }
            Ok(())
        }
        SettingChange::ModTap(binding) => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_key(binding.key)?;
            check_key_exclusive(snapshot, binding.key, "MT", Some(binding.slot))
        }
        SettingChange::ClearModTap { key, .. } | SettingChange::ClearToggleKey { key, .. } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_key(*key)
        }
        SettingChange::ToggleKey(binding) => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_key(binding.key)?;
            check_key_exclusive(snapshot, binding.key, "TGL", Some(binding.slot))
        }
        SettingChange::KeyBinding { layer, key, code } => {
            let km = require(snapshot.keymap.as_ref(), "keymap")?;
            km.capabilities.validate_layer(*layer)?;
            let max_keys = u16::from(km.capabilities.rows) * u16::from(km.capabilities.cols);
            if key.value() >= max_keys {
                return Err(CoreError::OutOfRange {
                    field: "key_index",
                    actual: key.value().to_string(),
                    expected: format!("0..{max_keys}"),
                });
            }
            if *layer == 0 && code.class == 0x7E && (5..25).contains(&code.id) {
                check_key_exclusive(snapshot, *key, "DKS", Some(code.id - 5))?;
            }
            Ok(())
        }
        SettingChange::ResetKeymap => {
            let _ = require(snapshot.keymap.as_ref(), "keymap")?;
            Ok(())
        }
        SettingChange::Macros(slots) => {
            let mac = require(snapshot.macros.as_ref(), "macros")?;
            if slots.len() > usize::from(mac.capabilities.max_slots) {
                return Err(CoreError::OutOfRange {
                    field: "macros",
                    actual: slots.len().to_string(),
                    expected: format!("0..={}", mac.capabilities.max_slots),
                });
            }
            let expected_repeat = if mac.capabilities.supports_repeat {
                "1..=255"
            } else {
                "1"
            };
            if let Some(slot) = slots.iter().find(|slot| {
                slot.repeat == 0 || (!mac.capabilities.supports_repeat && slot.repeat != 1)
            }) {
                return Err(CoreError::OutOfRange {
                    field: "macro_repeat",
                    actual: slot.repeat.to_string(),
                    expected: expected_repeat.to_owned(),
                });
            }
            let edits_readonly_slot = slots.iter().any(|slot| {
                mac.slots
                    .iter()
                    .find(|stored| stored.index == slot.index)
                    .is_some_and(|stored| stored.is_readonly && stored != slot)
            });
            if edits_readonly_slot {
                return Err(CoreError::UnsupportedCapability("macro_readonly_slot"));
            }
            for slot in slots {
                if slot.is_readonly {
                    continue;
                }
                for step in &slot.steps {
                    if step.kind.is_mouse_button() {
                        if !mac.capabilities.supports_mouse_buttons {
                            return Err(CoreError::UnsupportedCapability("macro_mouse_buttons"));
                        }
                        if step.mouse_button.is_none() {
                            return Err(CoreError::Validation(
                                "mouse macro step is missing a target button".to_owned(),
                            ));
                        }
                    } else if step.kind.is_media() && !mac.capabilities.supports_media_keys {
                        return Err(CoreError::UnsupportedCapability("macro_media_keys"));
                    } else if step.kind.is_pointer_move() && !mac.capabilities.supports_pointer_move
                    {
                        return Err(CoreError::UnsupportedCapability("macro_pointer_move"));
                    }
                }
            }
            Ok(())
        }
        SettingChange::KeyboardConfig(cfg) => {
            let kc = require(snapshot.keyboard_config.as_ref(), "keyboard_config")?;
            if !kc.capabilities.supported_polling_rates.is_empty()
                && !kc
                    .capabilities
                    .supported_polling_rates
                    .contains(&cfg.polling_rate)
            {
                return Err(CoreError::OutOfRange {
                    field: "polling_rate",
                    actual: cfg.polling_rate.to_string(),
                    expected: "a polling rate supported by the keyboard".to_owned(),
                });
            }
            Ok(())
        }
        SettingChange::StartCalibration
        | SettingChange::FinishCalibration
        | SettingChange::PollCalibrationProgress => {
            let _ = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            Ok(())
        }
        SettingChange::Rgb { zone, effect, .. } => {
            let rgb = require(snapshot.rgb.as_ref(), "rgb")?;
            if !rgb.capabilities.zones.iter().any(|info| info.id == *zone) {
                return Err(CoreError::OutOfRange {
                    field: "rgb_zone",
                    actual: zone.value().to_string(),
                    expected: "a zone reported by the device".to_owned(),
                });
            }
            if !rgb.capabilities.supported_effects.contains(&effect.kind()) {
                return Err(CoreError::UnsupportedCapability("rgb_effect"));
            }
            Ok(())
        }
        SettingChange::Profile(slot) => {
            let profiles = require(snapshot.profiles.as_ref(), "profiles")?;
            profiles.capabilities.validate_slot(*slot)?;
            if profiles
                .snapshot
                .profiles
                .iter()
                .find(|info| info.slot == *slot)
                .is_some_and(|info| !info.is_enabled())
            {
                return Err(CoreError::Validation(format!(
                    "{slot} is disabled in onboard memory"
                )));
            }
            Ok(())
        }
    }
}

fn validate_pointer_change(pointer: &PointerState, change: &PointerChange) -> CoreResult<()> {
    let caps = &pointer.capabilities;
    match change {
        PointerChange::ActiveDpiStage(stage_ix) => {
            if usize::from(*stage_ix) >= pointer.snapshot.dpi_stages.len() {
                return Err(CoreError::OutOfRange {
                    field: "active_dpi_stage",
                    actual: stage_ix.to_string(),
                    expected: format!("0..{}", pointer.snapshot.dpi_stages.len()),
                });
            }
            Ok(())
        }
        PointerChange::DpiStages { stages, active } => caps.validate_dpi_stages(stages, *active),
        PointerChange::PollingRate(rate) => caps.validate_polling_rate(*rate),
        PointerChange::LiftOffDistance(lod) => caps.validate_lod(*lod),
        PointerChange::Debounce(debounce) => caps.validate_debounce(*debounce),
        PointerChange::MotionSync(_) => {
            if caps.supports_motion_sync {
                Ok(())
            } else {
                Err(CoreError::UnsupportedCapability("motion_sync"))
            }
        }
        PointerChange::AngleSnapping(_) => {
            if caps.supports_angle_snapping {
                Ok(())
            } else {
                Err(CoreError::UnsupportedCapability("angle_snapping"))
            }
        }
        PointerChange::RippleControl(_) => {
            if caps.supports_ripple_control {
                Ok(())
            } else {
                Err(CoreError::UnsupportedCapability("ripple_control"))
            }
        }
    }
}

/// Applies an already validated change to a snapshot without touching hardware.
#[allow(clippy::too_many_lines)]
pub fn apply_change_to_snapshot(snapshot: &mut DeviceSnapshot, change: &SettingChange) {
    match change {
        SettingChange::Pointer(change) => {
            if let Some(pointer) = snapshot.pointer.as_mut() {
                apply_pointer_change(&mut pointer.snapshot, change);
            }
        }
        SettingChange::Actuation { target, point } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                for config in &mut he.snapshot.keys {
                    if matches!(target, ActuationTarget::AllKeys)
                        || *target == ActuationTarget::Key(config.key)
                    {
                        config.actuation = *point;
                    }
                }
            }
        }
        SettingChange::ActuationMany { keys, point } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                for config in &mut he.snapshot.keys {
                    if keys.contains(&config.key) {
                        config.actuation = *point;
                    }
                }
            }
        }
        SettingChange::RapidTrigger { key, config } => {
            if let Some(entry) = snapshot
                .hall_effect
                .as_mut()
                .and_then(|he| he.snapshot.keys.iter_mut().find(|k| k.key == *key))
            {
                entry.rapid_trigger = *config;
            }
        }
        SettingChange::RapidTriggerMany { keys, config } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                for entry in &mut he.snapshot.keys {
                    if keys.contains(&entry.key) {
                        entry.rapid_trigger = *config;
                    }
                }
            }
        }
        SettingChange::Deadzones { top, bottom } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                for entry in &mut he.snapshot.keys {
                    entry.top_deadzone = Some(*top);
                    entry.bottom_deadzone = Some(*bottom);
                }
            }
        }
        SettingChange::Dks(binding) => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                let bindings = &mut he.snapshot.dks_bindings;
                bindings.retain(|existing| existing.key != binding.key);
                if binding.stages.iter().any(Option::is_some) {
                    bindings.push(binding.clone());
                    bindings.sort_by_key(|existing| existing.key);
                }
            }
        }
        SettingChange::ClearDks { slot } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot.dks_bindings.retain(|b| b.key != *slot);
            }
        }
        SettingChange::Socd(pairs) => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot.socd_pairs.clone_from(pairs);
            }
        }
        SettingChange::RapidSwitch(pairs) => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot.rapid_switch_pairs.clone_from(pairs);
            }
        }
        SettingChange::ModTap(binding) => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot
                    .mod_tap_bindings
                    .retain(|b| b.slot != binding.slot);
                he.snapshot.mod_tap_bindings.push(*binding);
            }
        }
        SettingChange::ClearModTap { slot, .. } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot.mod_tap_bindings.retain(|b| b.slot != *slot);
            }
        }
        SettingChange::ToggleKey(binding) => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot
                    .toggle_bindings
                    .retain(|b| b.slot != binding.slot);
                he.snapshot.toggle_bindings.push(*binding);
            }
        }
        SettingChange::ClearToggleKey { slot, .. } => {
            if let Some(he) = snapshot.hall_effect.as_mut() {
                he.snapshot.toggle_bindings.retain(|b| b.slot != *slot);
            }
        }
        SettingChange::KeyBinding { layer, key, code } => {
            if let Some(km) = snapshot.keymap.as_mut()
                && let Some(layer_vec) = km.snapshot.layers.get_mut(usize::from(*layer))
                && let Some(slot) = layer_vec.get_mut(usize::from(key.value()))
            {
                *slot = *code;
            }
        }
        SettingChange::ResetKeymap
        | SettingChange::StartCalibration
        | SettingChange::FinishCalibration
        | SettingChange::PollCalibrationProgress => {}
        SettingChange::Macros(slots) => {
            if let Some(mac) = snapshot.macros.as_mut() {
                mac.slots.clone_from(slots);
            }
        }
        SettingChange::KeyboardConfig(cfg) => {
            if let Some(kc) = snapshot.keyboard_config.as_mut() {
                kc.snapshot = *cfg;
            }
        }
        SettingChange::Rgb {
            zone,
            effect,
            brightness,
        } => {
            if let Some(state) = snapshot
                .rgb
                .as_mut()
                .and_then(|rgb| rgb.snapshot.zones.iter_mut().find(|z| z.zone == *zone))
            {
                state.effect = *effect;
                state.brightness = *brightness;
            }
        }
        SettingChange::Profile(slot) => {
            if let Some(profiles) = snapshot.profiles.as_mut() {
                profiles.snapshot.active_slot = *slot;
            }
        }
    }
}

fn apply_pointer_change(snapshot: &mut PointerSnapshot, change: &PointerChange) {
    match change {
        PointerChange::ActiveDpiStage(stage_ix) => snapshot.active_dpi_stage = *stage_ix,
        PointerChange::DpiStages { stages, active } => {
            snapshot.dpi_stages.clone_from(stages);
            snapshot.active_dpi_stage = *active;
        }
        PointerChange::PollingRate(rate) => snapshot.polling_rate = *rate,
        PointerChange::LiftOffDistance(lod) => snapshot.lod = Some(*lod),
        PointerChange::Debounce(debounce) => snapshot.debounce_ms = Some(*debounce),
        PointerChange::MotionSync(enabled) => snapshot.motion_sync = Some(*enabled),
        PointerChange::AngleSnapping(enabled) => snapshot.angle_snapping = Some(*enabled),
        PointerChange::RippleControl(enabled) => snapshot.ripple_control = Some(*enabled),
    }
}

/// Reads every advertised capability with readonly queries, mirroring `agnoperiph-cli probe`.
fn collect_snapshot(peripheral: &mut dyn Peripheral) -> DeviceSnapshot {
    let mut warnings = Vec::new();
    let mut record = |capability: &'static str, err: CoreError| {
        warnings.push(DeviceWarning::new(capability, err.to_string()));
    };

    let battery = peripheral
        .as_battery_mut()
        .and_then(|b| b.read_battery().map_err(|err| record("Battery", err)).ok());
    let pointer = peripheral.as_pointer_mut().and_then(|p| {
        let capabilities = p.pointer_capabilities();
        p.read_pointer_state()
            .map(|snapshot| PointerState {
                capabilities,
                snapshot,
            })
            .map_err(|err| record("Pointer", err))
            .ok()
    });
    let hall_effect = peripheral.as_hall_effect_mut().and_then(|he| {
        let capabilities = he.hall_effect_capabilities();
        he.read_hall_effect_state()
            .map(|snapshot| HallEffectState {
                capabilities,
                snapshot,
            })
            .map_err(|err| record("Hall Effect", err))
            .ok()
    });
    let rgb = peripheral.as_rgb_mut().and_then(|rgb| {
        let capabilities = rgb.rgb_capabilities();
        rgb.read_rgb_state()
            .map(|snapshot| RgbState {
                capabilities,
                snapshot,
            })
            .map_err(|err| record("Lighting", err))
            .ok()
    });
    let profiles = peripheral.as_profiles_mut().and_then(|pr| {
        let capabilities = pr.profile_capabilities();
        pr.read_profile_state()
            .map(|snapshot| ProfileState {
                capabilities,
                snapshot,
            })
            .map_err(|err| record("Profiles", err))
            .ok()
    });
    let keymap = peripheral.as_keymap_mut().and_then(|km| {
        let capabilities = km.keymap_capabilities();
        km.read_keymap()
            .map(|snapshot| KeymapState {
                capabilities,
                snapshot,
            })
            .map_err(|err| record("Keymap", err))
            .ok()
    });
    let macros = peripheral.as_macros_mut().and_then(|mac| {
        let capabilities = mac.macro_capabilities();
        mac.read_macros()
            .map(|slots| MacroState {
                capabilities,
                slots,
            })
            .map_err(|err| record("Macros", err))
            .ok()
    });
    let keyboard_config = peripheral.as_keyboard_config_mut().and_then(|kc| {
        let capabilities = kc.keyboard_config_capabilities();
        kc.read_keyboard_config()
            .map(|snapshot| KeyboardConfigState {
                capabilities,
                snapshot,
            })
            .map_err(|err| record("Keyboard Config", err))
            .ok()
    });

    DeviceSnapshot {
        info: peripheral.info().clone(),
        capabilities: peripheral.capabilities(),
        pointer,
        hall_effect,
        rgb,
        battery,
        profiles,
        keymap,
        macros,
        keyboard_config,
        warnings,
        is_preview: false,
    }
}

fn apply_to_peripheral(peripheral: &mut dyn Peripheral, change: &SettingChange) -> CoreResult<()> {
    match change {
        SettingChange::Pointer(change) => {
            let pointer = require(peripheral.as_pointer_mut(), "pointer")?;
            match change {
                PointerChange::ActiveDpiStage(stage_ix) => pointer.set_active_dpi_stage(*stage_ix),
                PointerChange::DpiStages { stages, active } => {
                    pointer.set_dpi_stages(stages, *active)
                }
                PointerChange::PollingRate(rate) => pointer.set_polling_rate(*rate),
                PointerChange::LiftOffDistance(lod) => pointer.set_lod(*lod),
                PointerChange::Debounce(debounce) => pointer.set_debounce_ms(*debounce),
                PointerChange::MotionSync(enabled) => pointer.set_motion_sync(*enabled),
                PointerChange::AngleSnapping(enabled) => pointer.set_angle_snapping(*enabled),
                PointerChange::RippleControl(enabled) => pointer.set_ripple_control(*enabled),
            }
        }
        SettingChange::Actuation { target, point } => {
            let he = require(peripheral.as_hall_effect_mut(), "hall_effect")?;
            match target {
                ActuationTarget::Key(key) => he.set_key_actuation(*key, *point),
                ActuationTarget::AllKeys => he.set_global_actuation(*point),
            }
        }
        SettingChange::ActuationMany { keys, point } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .set_keys_actuation(keys, *point)
        }
        SettingChange::RapidTrigger { key, config } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .set_rapid_trigger(*key, *config)
        }
        SettingChange::RapidTriggerMany { keys, config } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .set_keys_rapid_trigger(keys, *config)
        }
        SettingChange::Deadzones { top, bottom } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.set_deadzones(*top, *bottom)
        }
        SettingChange::Dks(binding) => require(peripheral.as_hall_effect_mut(), "hall_effect")?
            .set_dks_binding(binding.clone()),
        SettingChange::ClearDks { slot } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.clear_dks_binding(*slot)
        }
        SettingChange::Socd(pairs) => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.set_socd_pairs(pairs)
        }
        SettingChange::RapidSwitch(pairs) => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.set_rapid_switch_pairs(pairs)
        }
        SettingChange::ModTap(binding) => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.set_mod_tap_binding(*binding)
        }
        SettingChange::ClearModTap { slot, key } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .clear_mod_tap_binding(*slot, *key)
        }
        SettingChange::ToggleKey(binding) => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.set_toggle_binding(*binding)
        }
        SettingChange::ClearToggleKey { slot, key } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .clear_toggle_binding(*slot, *key)
        }
        SettingChange::KeyBinding { layer, key, code } => {
            require(peripheral.as_keymap_mut(), "keymap")?.set_key_binding(*layer, *key, *code)
        }
        SettingChange::ResetKeymap => require(peripheral.as_keymap_mut(), "keymap")?.reset_keymap(),
        SettingChange::Macros(slots) => {
            require(peripheral.as_macros_mut(), "macros")?.write_macros(slots)
        }
        SettingChange::KeyboardConfig(cfg) => {
            require(peripheral.as_keyboard_config_mut(), "keyboard_config")?
                .set_keyboard_config(cfg)
        }
        SettingChange::StartCalibration => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.start_calibration()
        }
        SettingChange::FinishCalibration => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.finish_calibration()
        }
        SettingChange::PollCalibrationProgress => {
            let _ = require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .read_calibration_key_progress()?;
            Ok(())
        }
        SettingChange::Rgb {
            zone,
            effect,
            brightness,
        } => require(peripheral.as_rgb_mut(), "rgb")?.set_zone_effect(*zone, *effect, *brightness),
        SettingChange::Profile(slot) => {
            require(peripheral.as_profiles_mut(), "profiles")?.set_active_profile(*slot)
        }
    }
}

/// Lets a boxed enumerator transport satisfy the drivers' `T: HidTransport` bound.
struct BoxedTransport(Box<dyn HidTransport>);

impl HidTransport for BoxedTransport {
    fn descriptor(&self) -> &HidDeviceDescriptor {
        self.0.descriptor()
    }

    fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
        self.0.send_output_report(report)
    }

    fn read_input_report(
        &mut self,
        buf: &mut [u8],
        timeout: std::time::Duration,
    ) -> TransportResult<usize> {
        self.0.read_input_report(buf, timeout)
    }

    fn drain_input_reports(&mut self) -> TransportResult<usize> {
        self.0.drain_input_reports()
    }

    fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
        self.0.send_feature_report(report)
    }

    fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        self.0.get_feature_report(buf)
    }
}

fn build_registry() -> DeviceRegistry {
    let mut registry = DeviceRegistry::new();
    registry.register_driver(agnoperiph_driver_logitech::driver_descriptor());
    registry.register_driver(agnoperiph_driver_lamzu::driver_descriptor());
    registry.register_driver(agnoperiph_driver_atk::driver_descriptor());
    registry.register_driver(agnoperiph_driver_mchose::driver_descriptor());
    registry.register_driver(agnoperiph_driver_razer::driver_descriptor());
    registry.register_driver(agnoperiph_driver_pulsar::driver_descriptor());
    registry.register_driver(agnoperiph_driver_finalmouse::driver_descriptor());
    registry.register_driver(agnoperiph_driver_wlmouse::driver_descriptor());
    registry.register_driver(agnoperiph_driver_endgame_gear::driver_descriptor());
    registry.register_driver(agnoperiph_driver_wooting::driver_descriptor());
    registry.register_driver(agnoperiph_driver_via::driver_descriptor());
    registry
}

fn open_driver(
    driver_id: &str,
    transport: Box<dyn HidTransport>,
    dry_run: bool,
    sink: Arc<dyn PacketSink>,
) -> CoreResult<Box<dyn Peripheral>> {
    let wrapped = BoxedTransport(transport);
    match driver_id {
        "logitech-hidpp" => Ok(Box::new(LogitechHidppDevice::open(
            wrapped,
            None,
            dry_run,
            Some(sink),
        )?)),
        "lamzu-atlantis" => Ok(Box::new(LamzuAtlantisDevice::open(
            wrapped,
            dry_run,
            Some(sink),
        )?)),
        "atk-rs7" => Ok(Box::new(AtkDevice::open(wrapped, dry_run, Some(sink))?)),
        "mchose-hid" => Ok(Box::new(MchoseDevice::open(wrapped, dry_run, Some(sink))?)),
        "razer-hid" => Ok(Box::new(RazerDevice::open(wrapped, dry_run, Some(sink))?)),
        "pulsar-hid" => Ok(Box::new(PulsarDevice::open(wrapped, dry_run, Some(sink))?)),
        "finalmouse-hid" => Ok(Box::new(FinalmouseDevice::open(
            wrapped,
            dry_run,
            Some(sink),
        )?)),
        "wlmouse-hid" => Ok(Box::new(WLmouseDevice::open(wrapped, dry_run, Some(sink))?)),
        "endgame-gear-hid" => Ok(Box::new(EndgameGearDevice::open(
            wrapped,
            dry_run,
            Some(sink),
        )?)),
        "wooting-he" => Ok(Box::new(WootingDevice::open(wrapped, dry_run, Some(sink))?)),
        "qmk-via" => Ok(Box::new(ViaKeyboardDevice::open(
            wrapped,
            dry_run,
            Some(sink),
        )?)),
        other => Err(CoreError::DeviceNotFound(format!(
            "no driver registered as '{other}'"
        ))),
    }
}

struct LiveDevice {
    /// Active HID handle, or `None` after a transient disconnect while awaiting auto-reopen.
    peripheral: Option<Box<dyn Peripheral>>,
    snapshot: DeviceSnapshot,
}

struct LiveState {
    dry_run: bool,
    devices: Vec<LiveDevice>,
}

/// Real Windows HID backend driving every registered peripheral driver through `hidapi`.
///
/// Starts in dry-run mode. Because each driver fixes dry-run at `open()`, changing the
/// mode closes every handle under the same lock, so a write can never reach a handle
/// opened in the other mode.
pub struct LiveHidService {
    enumerator: Arc<dyn HidEnumerator>,
    state: Mutex<LiveState>,
    sink: Arc<SequencedSink>,
}

impl fmt::Debug for LiveHidService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("LiveHidService")
            .field("dry_run", &state.dry_run)
            .field("devices", &state.devices.len())
            .finish_non_exhaustive()
    }
}

impl Default for LiveHidService {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveHidService {
    pub fn new() -> Self {
        Self::new_with_dry_run(true)
    }

    /// Creates a [`LiveHidService`] starting in the requested dry-run mode.
    pub fn new_with_dry_run(dry_run: bool) -> Self {
        Self::with_enumerator_and_mode(Arc::new(HidapiEnumerator::new()), dry_run)
    }

    /// Creates a [`LiveHidService`] backed by a custom [`HidEnumerator`] (used by real-driver
    /// integration tests with scripted `MockHidTransport` replies).
    pub fn with_enumerator(enumerator: Arc<dyn HidEnumerator>) -> Self {
        Self::with_enumerator_and_mode(enumerator, true)
    }

    fn with_enumerator_and_mode(enumerator: Arc<dyn HidEnumerator>, dry_run: bool) -> Self {
        Self {
            enumerator,
            state: Mutex::new(LiveState {
                dry_run,
                devices: Vec::new(),
            }),
            sink: Arc::new(SequencedSink::new()),
        }
    }

    /// Immediately enumerates and opens a fresh driver handle for `device_id` without sleeping.
    fn open_matching_peripheral(
        &self,
        device_id: &DeviceId,
        dry_run: bool,
    ) -> Option<Box<dyn Peripheral>> {
        let registry = build_registry();
        let sink: Arc<dyn PacketSink> = self.sink.clone();
        let Ok(descriptors) = self.enumerator.enumerate() else {
            return None;
        };
        for descriptor in &descriptors {
            let Some(matched) = registry.match_interface(&descriptor.to_match_descriptor()) else {
                continue;
            };
            let Ok(transport) = self.enumerator.open_path(descriptor) else {
                continue;
            };
            let Ok(peripheral) = open_driver(&matched.driver_id, transport, dry_run, sink.clone())
            else {
                continue;
            };
            if peripheral.info().id == *device_id {
                return Some(peripheral);
            }
        }
        None
    }

    /// Re-enumerates and reopens `device_id` after a brief USB reset (such as changing
    /// polling rate on an ATK RS7 keyboard) or a stale/suspended HID handle. Retries with
    /// short backoff so the OS HID interface has time to come back online.
    fn reopen_device(
        &self,
        device_id: &DeviceId,
        dry_run: bool,
        wait_for_usb_reset: bool,
    ) -> Option<LiveDevice> {
        let delays: &[u64] = if wait_for_usb_reset {
            &[350, 450, 550, 650, 750, 850]
        } else {
            &[0, 150, 350, 600]
        };
        for &delay_ms in delays {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            let Some(mut peripheral) = self.open_matching_peripheral(device_id, dry_run) else {
                continue;
            };
            let snapshot = collect_snapshot(peripheral.as_mut());
            if !has_disconnect_warning(&snapshot) {
                return Some(LiveDevice {
                    peripheral: Some(peripheral),
                    snapshot,
                });
            }
        }
        None
    }

    fn read_live_key_travel_inner(
        &self,
        device: &DeviceId,
        focus_key: Option<KeyIndex>,
        focused: bool,
    ) -> CoreResult<Vec<KeyTravelSample>> {
        let mut state = lock(&self.state);
        let dry_run = state.dry_run;
        let target_ix = state
            .devices
            .iter()
            .position(|d| d.snapshot.info.id == *device)
            .ok_or_else(|| CoreError::DeviceNotFound(device.to_string()))?;

        if state.devices[target_ix].peripheral.is_none() {
            let Some(fresh) = self.open_matching_peripheral(device, dry_run) else {
                return Err(CoreError::NotConnected(device.to_string()));
            };
            state.devices[target_ix].peripheral = Some(fresh);
        }

        let read_once = |peripheral: &mut dyn Peripheral| -> CoreResult<Vec<KeyTravelSample>> {
            let he = require(peripheral.as_hall_effect_mut(), "hall_effect")?;
            if focused {
                he.read_live_key_travel_focused(focus_key)
            } else {
                he.read_live_key_travel()
            }
        };

        let attempt = read_once(
            state.devices[target_ix]
                .peripheral
                .as_deref_mut()
                .expect("checked Some above"),
        );
        match attempt {
            Ok(samples) => Ok(samples),
            Err(err) if is_recoverable_live_error(&err) => {
                // Close the stale/suspended Windows HID handle before reopening so the OS
                // releases it cleanly, keeping the device entry intact for automatic retries.
                state.devices[target_ix].peripheral = None;
                if let Some(mut fresh) = self.open_matching_peripheral(device, dry_run) {
                    let retry = read_once(fresh.as_mut());
                    if let Err(ref retry_err) = retry
                        && is_recoverable_live_error(retry_err)
                    {
                        return retry;
                    }
                    state.devices[target_ix].peripheral = Some(fresh);
                    return retry;
                }
                Err(err)
            }
            Err(err) => Err(err),
        }
    }
}

/// Returns `true` when an error message indicates the USB HID handle disconnected
/// (for example Windows `ERROR_DEVICE_NOT_CONNECTED` `0x0000048F` during a polling-rate reset
/// or after USB selective suspend).
pub fn is_disconnect_message(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("0x0000048f")
        || lower.contains("0x0000001f")
        || lower.contains("not connected")
        || lower.contains("disconnected")
        || lower.contains("not functioning")
        || lower.contains("writefile")
        || lower.contains("readfile")
        || lower.contains("getoverlappedresult")
}

fn is_recoverable_live_error(err: &CoreError) -> bool {
    matches!(err, CoreError::NotConnected(_)) || is_disconnect_message(&err.to_string())
}

fn has_disconnect_warning(snapshot: &DeviceSnapshot) -> bool {
    snapshot
        .warnings
        .iter()
        .any(|w| is_disconnect_message(&w.message))
}

impl PeripheralService for LiveHidService {
    fn is_dry_run(&self) -> bool {
        lock(&self.state).dry_run
    }

    fn set_dry_run(&self, dry_run: bool) {
        let mut state = lock(&self.state);
        state.dry_run = dry_run;
        state.devices.clear();
    }

    fn refresh(&self) -> CoreResult<Vec<DeviceSnapshot>> {
        let mut state = lock(&self.state);
        state.devices.clear();

        let descriptors = self.enumerator.enumerate()?;
        let registry = build_registry();
        let sink: Arc<dyn PacketSink> = self.sink.clone();

        for descriptor in &descriptors {
            let Some(matched) = registry.match_interface(&descriptor.to_match_descriptor()) else {
                continue;
            };
            // A device that fails to open (busy, asleep, vendor app holding it) is skipped
            // so one unresponsive interface does not hide every other device.
            let Ok(transport) = self.enumerator.open_path(descriptor) else {
                continue;
            };
            let Ok(mut peripheral) =
                open_driver(&matched.driver_id, transport, state.dry_run, sink.clone())
            else {
                continue;
            };
            if state
                .devices
                .iter()
                .any(|device| device.snapshot.info.id == peripheral.info().id)
            {
                continue;
            }
            let snapshot = collect_snapshot(peripheral.as_mut());
            state.devices.push(LiveDevice {
                peripheral: Some(peripheral),
                snapshot,
            });
        }

        Ok(state.devices.iter().map(|d| d.snapshot.clone()).collect())
    }

    fn apply(
        &self,
        device: &DeviceId,
        change: SettingChange,
    ) -> Result<DeviceSnapshot, ApplyError> {
        let mut state = lock(&self.state);
        let dry_run = state.dry_run;
        let target_ix = state
            .devices
            .iter()
            .position(|d| d.snapshot.info.id == *device)
            .ok_or_else(|| ApplyError::rejected(CoreError::DeviceNotFound(device.to_string())))?;

        if let Err(err) = validate_change(&state.devices[target_ix].snapshot, &change) {
            return Err(ApplyError::rejected(err));
        }

        let is_polling_rate_change = matches!(
            (&change, &state.devices[target_ix].snapshot.keyboard_config),
            (SettingChange::KeyboardConfig(next), Some(current))
                if next.polling_rate != current.snapshot.polling_rate
        );

        if state.devices[target_ix].peripheral.is_none() {
            if let Some(fresh) = self.open_matching_peripheral(device, dry_run) {
                state.devices[target_ix].peripheral = Some(fresh);
            } else {
                return Err(ApplyError::failed_after_send(
                    CoreError::NotConnected(device.to_string()),
                    None,
                ));
            }
        }

        if matches!(change, SettingChange::PollCalibrationProgress) {
            let target = &mut state.devices[target_ix];
            let peripheral = target
                .peripheral
                .as_deref_mut()
                .expect("checked Some above");
            let flags = require(peripheral.as_hall_effect_mut(), "hall_effect")
                .map_err(ApplyError::rejected)?
                .read_calibration_key_progress()
                .map_err(|err| ApplyError::failed_after_send(err, None))?;
            if let Some(he) = target.snapshot.hall_effect.as_mut() {
                he.snapshot.calibrated_keys = flags;
            }
            return Ok(target.snapshot.clone());
        }

        let sequence_before = self.sink.next.load(Ordering::Relaxed);
        if let Err(err) = apply_to_peripheral(
            state.devices[target_ix]
                .peripheral
                .as_deref_mut()
                .expect("checked Some above"),
            &change,
        ) {
            // A driver that refuses a write it cannot express (e.g. Wooting's readonly actuation
            // table) fails before tracing any frame; only then is it safe to skip the re-read.
            let nothing_sent = self.sink.next.load(Ordering::Relaxed) == sequence_before;
            if dry_run || nothing_sent {
                return Err(ApplyError::rejected(err));
            }
            // Changing USB polling rate resets the keyboard's USB interface immediately; drop the
            // stale handle and reopen it automatically once Windows re-enumerates the device.
            if is_polling_rate_change && is_disconnect_message(&err.to_string()) {
                state.devices[target_ix].peripheral = None;
                if let Some(reopened) = self.reopen_device(device, dry_run, true) {
                    let snap = reopened.snapshot.clone();
                    state.devices[target_ix] = reopened;
                    return Ok(snap);
                }
                return Err(ApplyError::failed_after_send(err, None));
            }
            // A stale/suspended HID handle (e.g. after hours idle in a game) can fail with a
            // disconnect error; reopen cleanly and retry this write once on the fresh handle.
            if is_disconnect_message(&err.to_string()) {
                state.devices[target_ix].peripheral = None;
                if let Some(mut reopened) = self.reopen_device(device, dry_run, false) {
                    let peripheral = reopened
                        .peripheral
                        .as_deref_mut()
                        .expect("reopened device always has a live peripheral");
                    if apply_to_peripheral(peripheral, &change).is_ok() {
                        let snapshot = collect_snapshot(peripheral);
                        reopened.snapshot = snapshot.clone();
                        state.devices[target_ix] = reopened;
                        return Ok(snapshot);
                    }
                    let refreshed = collect_snapshot(peripheral);
                    reopened.snapshot = refreshed.clone();
                    state.devices[target_ix] = reopened;
                    return Err(ApplyError::failed_after_send(err, Some(refreshed)));
                }
                return Err(ApplyError::failed_after_send(err, None));
            }
            // Frames may have already reached hardware before the failure (e.g. a timeout
            // mid-chunk during a global actuation write); re-read best-effort live state.
            let refreshed = collect_snapshot(
                state.devices[target_ix]
                    .peripheral
                    .as_deref_mut()
                    .expect("checked Some above"),
            );
            state.devices[target_ix].snapshot = refreshed.clone();
            return Err(ApplyError::failed_after_send(err, Some(refreshed)));
        }

        if dry_run {
            let target = &mut state.devices[target_ix];
            apply_change_to_snapshot(&mut target.snapshot, &change);
            target.snapshot.is_preview = true;
            return Ok(target.snapshot.clone());
        }

        let snapshot = collect_snapshot(
            state.devices[target_ix]
                .peripheral
                .as_deref_mut()
                .expect("checked Some above"),
        );
        if is_polling_rate_change || has_disconnect_warning(&snapshot) {
            // Device briefly disconnected / re-enumerated (e.g. polling rate change): close the
            // stale HID handle and reopen cleanly so the UI never shows disconnect errors.
            state.devices[target_ix].peripheral = None;
            if let Some(reopened) = self.reopen_device(device, dry_run, is_polling_rate_change) {
                let snap = reopened.snapshot.clone();
                state.devices[target_ix] = reopened;
                return Ok(snap);
            }
            return Err(ApplyError::failed_after_send(
                CoreError::NotConnected(device.to_string()),
                Some(snapshot),
            ));
        }

        state.devices[target_ix].snapshot = snapshot.clone();
        Ok(snapshot)
    }

    #[allow(clippy::too_many_lines)]
    fn apply_batch(
        &self,
        device: &DeviceId,
        changes: Vec<SettingChange>,
    ) -> (Option<DeviceSnapshot>, Option<(usize, usize, ApplyError)>) {
        if changes.is_empty() {
            return (None, None);
        }
        if changes.len() == 1 {
            let change = changes.into_iter().next().expect("checked non-empty");
            return match self.apply(device, change) {
                Ok(snapshot) => (Some(snapshot), None),
                Err(err) => (None, Some((0, 1, err))),
            };
        }

        let total = changes.len();
        let mut state = lock(&self.state);
        let dry_run = state.dry_run;
        let Some(target_ix) = state
            .devices
            .iter()
            .position(|d| d.snapshot.info.id == *device)
        else {
            return (
                None,
                Some((
                    0,
                    total,
                    ApplyError::rejected(CoreError::DeviceNotFound(device.to_string())),
                )),
            );
        };

        let mut applied_any = false;
        let mut polling_rate_changed = false;

        if state.devices[target_ix].peripheral.is_none() {
            if let Some(fresh) = self.open_matching_peripheral(device, dry_run) {
                state.devices[target_ix].peripheral = Some(fresh);
            } else {
                return (
                    None,
                    Some((
                        0,
                        total,
                        ApplyError::failed_after_send(
                            CoreError::NotConnected(device.to_string()),
                            None,
                        ),
                    )),
                );
            }
        }

        for (ix, change) in changes.into_iter().enumerate() {
            if let Err(err) = validate_change(&state.devices[target_ix].snapshot, &change) {
                let last = if applied_any {
                    if dry_run {
                        Some(state.devices[target_ix].snapshot.clone())
                    } else if let Some(peripheral) =
                        state.devices[target_ix].peripheral.as_deref_mut()
                    {
                        let refreshed = collect_snapshot(peripheral);
                        state.devices[target_ix].snapshot = refreshed.clone();
                        Some(refreshed)
                    } else {
                        None
                    }
                } else {
                    None
                };
                return (last, Some((ix, total, ApplyError::rejected(err))));
            }

            let is_polling_rate_change = matches!(
                (&change, &state.devices[target_ix].snapshot.keyboard_config),
                (SettingChange::KeyboardConfig(next), Some(current))
                    if next.polling_rate != current.snapshot.polling_rate
            );

            if matches!(change, SettingChange::PollCalibrationProgress) {
                let target = &mut state.devices[target_ix];
                let peripheral = target
                    .peripheral
                    .as_deref_mut()
                    .expect("checked Some above");
                let flags = match require(peripheral.as_hall_effect_mut(), "hall_effect")
                    .and_then(agnoperiph_core::HallEffectKeyboard::read_calibration_key_progress)
                {
                    Ok(flags) => flags,
                    Err(err) => {
                        let last = applied_any.then(|| target.snapshot.clone());
                        return (
                            last,
                            Some((ix, total, ApplyError::failed_after_send(err, None))),
                        );
                    }
                };
                if let Some(he) = target.snapshot.hall_effect.as_mut() {
                    he.snapshot.calibrated_keys = flags;
                }
                applied_any = true;
                continue;
            }

            let sequence_before = self.sink.next.load(Ordering::Relaxed);
            if let Err(err) = apply_to_peripheral(
                state.devices[target_ix]
                    .peripheral
                    .as_deref_mut()
                    .expect("checked Some above"),
                &change,
            ) {
                let nothing_sent = self.sink.next.load(Ordering::Relaxed) == sequence_before;
                if dry_run || nothing_sent {
                    let last = if applied_any {
                        if dry_run {
                            Some(state.devices[target_ix].snapshot.clone())
                        } else if let Some(peripheral) =
                            state.devices[target_ix].peripheral.as_deref_mut()
                        {
                            let refreshed = collect_snapshot(peripheral);
                            state.devices[target_ix].snapshot = refreshed.clone();
                            Some(refreshed)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    return (last, Some((ix, total, ApplyError::rejected(err))));
                }
                if is_polling_rate_change && is_disconnect_message(&err.to_string()) {
                    state.devices[target_ix].peripheral = None;
                    if let Some(reopened) = self.reopen_device(device, dry_run, true) {
                        state.devices[target_ix] = reopened;
                        applied_any = true;
                        polling_rate_changed = true;
                        continue;
                    }
                    return (
                        None,
                        Some((ix, total, ApplyError::failed_after_send(err, None))),
                    );
                }
                if is_disconnect_message(&err.to_string()) {
                    state.devices[target_ix].peripheral = None;
                    if let Some(mut reopened) = self.reopen_device(device, dry_run, false) {
                        let peripheral = reopened
                            .peripheral
                            .as_deref_mut()
                            .expect("reopened device always has a live peripheral");
                        if apply_to_peripheral(peripheral, &change).is_ok() {
                            apply_change_to_snapshot(&mut reopened.snapshot, &change);
                            state.devices[target_ix] = reopened;
                            applied_any = true;
                            continue;
                        }
                        let refreshed = collect_snapshot(peripheral);
                        reopened.snapshot = refreshed.clone();
                        state.devices[target_ix] = reopened;
                        return (
                            None,
                            Some((
                                ix,
                                total,
                                ApplyError::failed_after_send(err, Some(refreshed)),
                            )),
                        );
                    }
                    return (
                        None,
                        Some((ix, total, ApplyError::failed_after_send(err, None))),
                    );
                }
                let refreshed = collect_snapshot(
                    state.devices[target_ix]
                        .peripheral
                        .as_deref_mut()
                        .expect("checked Some above"),
                );
                state.devices[target_ix].snapshot = refreshed.clone();
                return (
                    None,
                    Some((
                        ix,
                        total,
                        ApplyError::failed_after_send(err, Some(refreshed)),
                    )),
                );
            }

            applied_any = true;
            let target = &mut state.devices[target_ix];
            apply_change_to_snapshot(&mut target.snapshot, &change);
            if dry_run {
                target.snapshot.is_preview = true;
            } else if is_polling_rate_change {
                polling_rate_changed = true;
                if ix + 1 < total {
                    state.devices[target_ix].peripheral = None;
                    if let Some(reopened) = self.reopen_device(device, dry_run, true) {
                        state.devices[target_ix] = reopened;
                    } else {
                        return (
                            None,
                            Some((
                                ix,
                                total,
                                ApplyError::failed_after_send(
                                    CoreError::NotConnected(device.to_string()),
                                    None,
                                ),
                            )),
                        );
                    }
                }
            }
        }

        if dry_run {
            return (Some(state.devices[target_ix].snapshot.clone()), None);
        }

        let snapshot = collect_snapshot(
            state.devices[target_ix]
                .peripheral
                .as_deref_mut()
                .expect("checked Some above"),
        );
        if polling_rate_changed || has_disconnect_warning(&snapshot) {
            state.devices[target_ix].peripheral = None;
            if let Some(reopened) = self.reopen_device(device, dry_run, polling_rate_changed) {
                let snap = reopened.snapshot.clone();
                state.devices[target_ix] = reopened;
                return (Some(snap), None);
            }
            return (
                None,
                Some((
                    total - 1,
                    total,
                    ApplyError::failed_after_send(
                        CoreError::NotConnected(device.to_string()),
                        Some(snapshot),
                    ),
                )),
            );
        }

        state.devices[target_ix].snapshot = snapshot.clone();
        (Some(snapshot), None)
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.sink.log.snapshot()
    }

    fn clear_packet_log(&self) {
        self.sink.log.clear();
    }

    fn read_live_key_travel(&self, device: &DeviceId) -> CoreResult<Vec<KeyTravelSample>> {
        self.read_live_key_travel_inner(device, None, false)
    }

    fn read_live_key_travel_focused(
        &self,
        device: &DeviceId,
        focus_key: Option<KeyIndex>,
    ) -> CoreResult<Vec<KeyTravelSample>> {
        self.read_live_key_travel_inner(device, focus_key, true)
    }

    fn sync_hardware_state(&self, device: &DeviceId) -> CoreResult<Option<DeviceSnapshot>> {
        let mut state = lock(&self.state);
        let dry_run = state.dry_run;
        let target_ix = state
            .devices
            .iter()
            .position(|d| d.snapshot.info.id == *device)
            .ok_or_else(|| CoreError::DeviceNotFound(device.to_string()))?;
        if state.devices[target_ix].snapshot.is_preview {
            return Ok(None);
        }
        if state.devices[target_ix].peripheral.is_none() {
            let Some(fresh) = self.open_matching_peripheral(device, dry_run) else {
                return Ok(None);
            };
            state.devices[target_ix].peripheral = Some(fresh);
        }

        let target = &mut state.devices[target_ix];
        let peripheral = target
            .peripheral
            .as_deref_mut()
            .expect("checked Some above");

        let mut next = target.snapshot.clone();
        let mut changed = false;

        if let Some(rgb) = peripheral.as_rgb_mut() {
            let capabilities = rgb.rgb_capabilities();
            if let Ok(snapshot) = rgb.read_rgb_state() {
                let updated = RgbState {
                    capabilities,
                    snapshot,
                };
                if next.rgb.as_ref() != Some(&updated) {
                    next.rgb = Some(updated);
                    changed = true;
                }
            }
        }

        if let Some(kc) = peripheral.as_keyboard_config_mut() {
            let capabilities = kc.keyboard_config_capabilities();
            if let Ok(snapshot) = kc.read_keyboard_config() {
                let updated = KeyboardConfigState {
                    capabilities,
                    snapshot,
                };
                if next.keyboard_config.as_ref() != Some(&updated) {
                    next.keyboard_config = Some(updated);
                    changed = true;
                }
            }
        }

        let mut profile_changed = false;
        if let Some(pr) = peripheral.as_profiles_mut() {
            let capabilities = pr.profile_capabilities();
            if let Ok(snapshot) = pr.read_profile_state() {
                let updated = ProfileState {
                    capabilities,
                    snapshot,
                };
                if next.profiles.as_ref() != Some(&updated) {
                    profile_changed = true;
                    next.profiles = Some(updated);
                    changed = true;
                }
            }
        }

        if profile_changed && let Some(pointer) = peripheral.as_pointer_mut() {
            let capabilities = pointer.pointer_capabilities();
            if let Ok(snapshot) = pointer.read_pointer_state() {
                next.pointer = Some(PointerState {
                    capabilities,
                    snapshot,
                });
            }
        }

        if changed {
            target.snapshot = next.clone();
            Ok(Some(next))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use agnoperiph_core::{RgbColor, TravelHundredthsMm};
    use agnoperiph_driver_atk::protocol as atk;
    use agnoperiph_transport::{MockHidTransport, PacketOutcome, TransportError};

    use super::*;

    struct ScriptedEnumerator {
        descriptor: HidDeviceDescriptor,
        transports: Mutex<VecDeque<MockHidTransport>>,
    }

    impl ScriptedEnumerator {
        fn new(descriptor: HidDeviceDescriptor, transports: Vec<MockHidTransport>) -> Self {
            Self {
                descriptor,
                transports: Mutex::new(VecDeque::from(transports)),
            }
        }
    }

    impl HidEnumerator for ScriptedEnumerator {
        fn enumerate(&self) -> TransportResult<Vec<HidDeviceDescriptor>> {
            Ok(vec![self.descriptor.clone()])
        }

        fn open_path(
            &self,
            _descriptor: &HidDeviceDescriptor,
        ) -> TransportResult<Box<dyn HidTransport>> {
            lock(&self.transports)
                .pop_front()
                .map(|t| Box::new(t) as Box<dyn HidTransport>)
                .ok_or_else(|| TransportError::OpenFailed {
                    path: self.descriptor.path.clone(),
                    reason: "no scripted transport remaining".to_owned(),
                })
        }
    }

    fn rs7_descriptor() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            atk::ATK_VID,
            atk::ATK_RS7_PID,
            atk::ATK_RS7_USAGE_PAGE,
            atk::ATK_RS7_USAGE_ID,
            1,
            "mock://live/atk-rs7",
        )
        .with_strings(
            Some("Shenzhen Yizhita Technology".to_owned()),
            Some("ATK RS7 RGB".to_owned()),
            Some("ATK-LIVE-TEST".to_owned()),
        )
    }

    fn push_rs7_open(mock: &mut MockHidTransport) {
        let mut fw = vec![0u8; atk::RS7_PAYLOAD_LEN];
        fw[0] = atk::cmd::GET_KEYBOARD_VALUE;
        fw[1] = atk::sub::CUSTOM_ID;
        fw[5] = 0x01;
        fw[6] = 0x07;
        mock.push_input_response(fw);

        let mut gm = vec![0u8; atk::RS7_PAYLOAD_LEN];
        gm[0] = atk::cmd::GET_KEYBOARD_VALUE;
        gm[1] = atk::sub::CUSTOM_ID;
        gm[2] = atk::sub::GAME_MODE;
        mock.push_input_response(gm);

        let mut rr = vec![0u8; atk::RS7_PAYLOAD_LEN];
        rr[0] = atk::cmd::GET_KEYBOARD_VALUE;
        rr[1] = atk::sub::CUSTOM_ID;
        rr[2] = atk::sub::REPORT_RATE;
        rr[3] = 4;
        mock.push_input_response(rr);
    }

    fn push_rs7_snapshot(mock: &mut MockHidTransport, actuation_hundredths: u16) {
        let mut dz = vec![0u8; atk::RS7_PAYLOAD_LEN];
        dz[0] = atk::cmd::GET_KEYBOARD_VALUE;
        dz[3..5].copy_from_slice(&10u16.to_be_bytes());
        dz[5..7].copy_from_slice(&10u16.to_be_bytes());
        mock.push_input_response(dz);

        for _ in 0..8 {
            let mut chunk = vec![0u8; atk::RS7_PAYLOAD_LEN];
            chunk[0] = atk::cmd::GET_KEYBOARD_VALUE;
            for k in 0..12 {
                chunk[8 + k * 2..10 + k * 2].copy_from_slice(&actuation_hundredths.to_be_bytes());
            }
            mock.push_input_response(chunk);
        }

        for _ in 0..24 {
            let mut chunk = vec![0u8; atk::RS7_PAYLOAD_LEN];
            chunk[0] = atk::cmd::GET_KEYBOARD_VALUE;
            mock.push_input_response(chunk);
        }

        for _ in 0..20 {
            let mut s = vec![0u8; atk::RS7_PAYLOAD_LEN];
            s[0] = atk::cmd::GET_KEYBOARD_VALUE;
            mock.push_input_response(s);
        }

        for _ in 0..20 {
            let mut d = vec![0u8; atk::RS7_PAYLOAD_LEN];
            d[0] = atk::cmd::GET_KEYBOARD_VALUE;
            mock.push_input_response(d);
        }

        for _ in 0..4 {
            let mut c = vec![0u8; atk::RS7_PAYLOAD_LEN];
            c[0] = atk::cmd::GET_KEYBOARD_VALUE;
            for k in 0..24 {
                c[8 + k] = 1;
            }
            mock.push_input_response(c);
        }

        let mut rgb = vec![0u8; atk::RS7_PAYLOAD_LEN];
        rgb[0] = atk::cmd::LIGHTING_GET_VALUE;
        rgb[2] = 2;
        rgb[5] = RgbColor::WHITE.r;
        rgb[6] = RgbColor::WHITE.g;
        rgb[7] = RgbColor::WHITE.b;
        rgb[8] = 80;
        mock.push_input_response(rgb);

        // Logo Light (zone 1)
        let mut logo = vec![0u8; atk::RS7_PAYLOAD_LEN];
        logo[0] = atk::cmd::GET_KEYBOARD_VALUE;
        logo[3] = 3; // Always On
        logo[5] = 120; // 75%
        logo[7] = RgbColor::WHITE.r;
        logo[8] = RgbColor::WHITE.g;
        logo[9] = RgbColor::WHITE.b;
        mock.push_input_response(logo);

        // Keymap: 768 bytes in 28-byte chunks = 28 reads + active layer read
        for _ in 0..28 {
            let mut km = vec![0u8; atk::RS7_PAYLOAD_LEN];
            km[0] = atk::cmd::DYNAMIC_KEYMAP_GET_BUFFER;
            mock.push_input_response(km);
        }
        let mut layer = vec![0u8; atk::RS7_PAYLOAD_LEN];
        layer[0] = atk::cmd::GET_KEYBOARD_VALUE;
        layer[3] = 1;
        mock.push_input_response(layer);

        // Macros: buffer size = 0 (no chunk reads needed)
        let mut mac_sz = vec![0u8; atk::RS7_PAYLOAD_LEN];
        mac_sz[0] = atk::cmd::MACRO_GET_BUFFER_SIZE;
        mock.push_input_response(mac_sz);

        // Keyboard Config: feature flags, anti-accidental touch, game mode, polling rate
        let mut feat = vec![0u8; atk::RS7_PAYLOAD_LEN];
        feat[0] = atk::cmd::GET_KEYBOARD_VALUE;
        feat[7] = 1; // NKRO
        mock.push_input_response(feat);

        let mut anti = vec![0u8; atk::RS7_PAYLOAD_LEN];
        anti[0] = atk::cmd::GET_KEYBOARD_VALUE;
        mock.push_input_response(anti);

        let mut gm = vec![0u8; atk::RS7_PAYLOAD_LEN];
        gm[0] = atk::cmd::GET_KEYBOARD_VALUE;
        mock.push_input_response(gm);

        let mut rr = vec![0u8; atk::RS7_PAYLOAD_LEN];
        rr[0] = atk::cmd::GET_KEYBOARD_VALUE;
        rr[3] = 8; // 8000 Hz
        mock.push_input_response(rr);
    }

    #[test]
    fn live_service_exercises_real_driver_and_rereads_after_failed_live_write() {
        let desc = rs7_descriptor();
        let mut dry_transport = MockHidTransport::new(desc.clone());
        push_rs7_open(&mut dry_transport);
        push_rs7_snapshot(&mut dry_transport, 120);

        let mut live_transport = MockHidTransport::new(desc.clone());
        push_rs7_open(&mut live_transport);
        push_rs7_snapshot(&mut live_transport, 120);
        // Live single-key actuation write succeeds on the driver, then re-read returns 140.
        let mut ack = vec![0u8; atk::RS7_PAYLOAD_LEN];
        ack[0] = atk::cmd::SET_KEYBOARD_VALUE;
        live_transport.push_input_response(ack);
        push_rs7_snapshot(&mut live_transport, 140);
        // Next live write sends a frame, gets NO matching reply (timeout -> NotConnected),
        // and the post-failure re-read reports 160 so the UI gets the actual live device state.
        // Push a wrong-command byte RS7_MAX_READ_ATTEMPTS times to trigger Protocol failure after send, then fresh snapshot.
        for _ in 0..agnoperiph_driver_atk::RS7_MAX_READ_ATTEMPTS {
            let mut bogus = vec![0u8; atk::RS7_PAYLOAD_LEN];
            bogus[0] = 0xFE;
            live_transport.push_input_response(bogus);
        }
        push_rs7_snapshot(&mut live_transport, 160);

        let enumerator = Arc::new(ScriptedEnumerator::new(
            desc,
            vec![dry_transport, live_transport],
        ));
        let service = LiveHidService::with_enumerator(enumerator);

        // 1. Starts in dry-run: real driver opens, reads, validates, and suppresses writes.
        let devices = service.refresh().unwrap();
        assert_eq!(devices.len(), 1);
        let id = devices[0].id().clone();
        assert_eq!(
            devices[0].hall_effect.as_ref().unwrap().snapshot.keys[0].actuation,
            TravelHundredthsMm::from_hundredths(120)
        );

        let preview = service
            .apply_actuation(
                &id,
                ActuationTarget::Key(KeyIndex::new(0)),
                TravelHundredthsMm::from_hundredths(140),
            )
            .unwrap();
        assert!(preview.is_preview);
        assert!(
            service
                .packet_log()
                .iter()
                .any(|e| e.outcome == PacketOutcome::DryRunSuppressed)
        );

        // 2. Switch to live mode: handle reopens, real driver transmits and re-reads 140.
        service.set_dry_run(false);
        service.refresh().unwrap();
        let updated = service
            .apply_actuation(
                &id,
                ActuationTarget::Key(KeyIndex::new(0)),
                TravelHundredthsMm::from_hundredths(140),
            )
            .unwrap();
        assert!(!updated.is_preview);
        assert_eq!(
            updated.hall_effect.as_ref().unwrap().snapshot.keys[0].actuation,
            TravelHundredthsMm::from_hundredths(140)
        );

        // 3. Mid-flight live failure: rejected_before_send is false AND refreshed_snapshot carries 160.
        let failure = service
            .apply_actuation(
                &id,
                ActuationTarget::Key(KeyIndex::new(0)),
                TravelHundredthsMm::from_hundredths(180),
            )
            .unwrap_err();
        assert!(!failure.rejected_before_send);
        let refreshed = failure
            .refreshed_snapshot
            .expect("failed live write must re-read live device state");
        assert_eq!(
            refreshed.hall_effect.as_ref().unwrap().snapshot.keys[0].actuation,
            TravelHundredthsMm::from_hundredths(160)
        );
    }

    #[test]
    fn readonly_macro_slots_only_pass_unchanged() {
        let devices = MockPeripheralService::new().refresh().unwrap();
        let mouse = devices
            .iter()
            .find(|d| {
                d.macros
                    .as_ref()
                    .is_some_and(|m| m.slots.iter().any(|s| s.is_readonly))
            })
            .unwrap();
        let slots = mouse.macros.as_ref().unwrap().slots.clone();
        assert!(validate_change(mouse, &SettingChange::Macros(slots.clone())).is_ok());

        let mut edited = slots;
        let readonly = edited.iter_mut().find(|s| s.is_readonly).unwrap();
        readonly.name = "renamed".to_owned();
        assert!(matches!(
            validate_change(mouse, &SettingChange::Macros(edited)),
            Err(CoreError::UnsupportedCapability("macro_readonly_slot"))
        ));
    }

    #[test]
    fn live_service_apply_batch_drains_stale_reports_and_rereads_once() {
        let desc = rs7_descriptor();
        let mut live_transport = MockHidTransport::new(desc.clone());
        push_rs7_open(&mut live_transport);
        push_rs7_snapshot(&mut live_transport, 120);

        // Simulate 16 unread unsolicited frames queued while playing a game, followed by ACKs
        // for 3 batched writes and ONLY ONE post-batch snapshot re-read.
        for _ in 0..16 {
            let mut unsolicited = vec![0u8; atk::RS7_PAYLOAD_LEN];
            unsolicited[0] = atk::cmd::LIGHTING_GET_VALUE;
            live_transport.push_stale_input_report(unsolicited);
        }
        for _ in 0..3 {
            let mut ack = vec![0u8; atk::RS7_PAYLOAD_LEN];
            ack[0] = atk::cmd::SET_KEYBOARD_VALUE;
            live_transport.push_input_response(ack);
        }
        push_rs7_snapshot(&mut live_transport, 150);

        let enumerator = Arc::new(ScriptedEnumerator::new(desc, vec![live_transport]));
        let service = LiveHidService::with_enumerator_and_mode(enumerator, false);
        let devices = service.refresh().unwrap();
        let id = devices[0].id().clone();

        let (snapshot, failure) = service.apply_batch(
            &id,
            vec![
                SettingChange::Actuation {
                    target: ActuationTarget::Key(KeyIndex::new(0)),
                    point: TravelHundredthsMm::from_hundredths(150),
                },
                SettingChange::Actuation {
                    target: ActuationTarget::Key(KeyIndex::new(1)),
                    point: TravelHundredthsMm::from_hundredths(150),
                },
                SettingChange::Deadzones {
                    top: TravelHundredthsMm::from_hundredths(10),
                    bottom: TravelHundredthsMm::from_hundredths(10),
                },
            ],
        );
        assert!(failure.is_none(), "batch should succeed without error");
        let snapshot = snapshot.expect("batch should return final snapshot");
        assert_eq!(
            snapshot.hall_effect.as_ref().unwrap().snapshot.keys[0].actuation,
            TravelHundredthsMm::from_hundredths(150)
        );
    }

    #[test]
    fn live_service_sync_hardware_state_detects_onboard_rgb_change_with_lightweight_queries() {
        let desc = rs7_descriptor();
        let mut live_transport = MockHidTransport::new(desc.clone());
        push_rs7_open(&mut live_transport);
        push_rs7_snapshot(&mut live_transport, 120);

        let push_lightweight_sync =
            |mock: &mut MockHidTransport, color: RgbColor, brightness: u8| {
                // 1. Backlight RGB (0x08 0x41)
                let mut rgb = vec![0u8; atk::RS7_PAYLOAD_LEN];
                rgb[0] = atk::cmd::LIGHTING_GET_VALUE;
                rgb[2] = 2;
                rgb[5] = color.r;
                rgb[6] = color.g;
                rgb[7] = color.b;
                rgb[8] = brightness;
                mock.push_input_response(rgb);

                // 2. Logo Light (0x02 0x96 0x21)
                let mut logo = vec![0u8; atk::RS7_PAYLOAD_LEN];
                logo[0] = atk::cmd::GET_KEYBOARD_VALUE;
                logo[3] = 3;
                logo[5] = 120;
                logo[7] = RgbColor::WHITE.r;
                logo[8] = RgbColor::WHITE.g;
                logo[9] = RgbColor::WHITE.b;
                mock.push_input_response(logo);

                // 3..6. Keyboard Config (feature flags, anti-accidental, game mode, report rate)
                let mut feat = vec![0u8; atk::RS7_PAYLOAD_LEN];
                feat[0] = atk::cmd::GET_KEYBOARD_VALUE;
                feat[7] = 1;
                mock.push_input_response(feat);
                for _ in 0..2 {
                    let mut cfg = vec![0u8; atk::RS7_PAYLOAD_LEN];
                    cfg[0] = atk::cmd::GET_KEYBOARD_VALUE;
                    mock.push_input_response(cfg);
                }
                let mut rr = vec![0u8; atk::RS7_PAYLOAD_LEN];
                rr[0] = atk::cmd::GET_KEYBOARD_VALUE;
                rr[3] = 8;
                mock.push_input_response(rr);
            };

        // First sync: unchanged (still white @ 80%) -> returns Ok(None).
        push_lightweight_sync(&mut live_transport, RgbColor::WHITE, 80);
        // Second sync: user pressed Fn+RGB directly on keyboard -> red @ 60% -> returns Ok(Some).
        push_lightweight_sync(&mut live_transport, RgbColor::new(0xFF, 0x10, 0x20), 60);

        let enumerator = Arc::new(ScriptedEnumerator::new(desc, vec![live_transport]));
        let service = LiveHidService::with_enumerator_and_mode(enumerator, false);
        let devices = service.refresh().unwrap();
        let id = devices[0].id().clone();

        assert!(service.sync_hardware_state(&id).unwrap().is_none());
        let updated = service
            .sync_hardware_state(&id)
            .unwrap()
            .expect("onboard RGB edit must be detected");
        let zone0 = &updated.rgb.as_ref().unwrap().snapshot.zones[0];
        assert_eq!(zone0.brightness.percent(), 60);
        assert_eq!(
            zone0.effect,
            RgbEffect::Preset {
                id: 2,
                color: RgbColor::new(0xFF, 0x10, 0x20),
                speed: 0,
                colorful: false,
            }
        );
    }

    #[test]
    fn validate_change_rejects_disabled_onboard_profile_slot() {
        let mock = MockPeripheralService::new();
        let mut mouse = mock
            .refresh()
            .unwrap()
            .into_iter()
            .find(|d| d.profiles.is_some())
            .unwrap();
        let profiles = mouse.profiles.as_mut().unwrap();
        profiles.snapshot.profiles = vec![
            agnoperiph_core::ProfileInfo::new(ProfileSlot::new(0), true),
            agnoperiph_core::ProfileInfo::new(ProfileSlot::new(1), false),
            agnoperiph_core::ProfileInfo::new(ProfileSlot::new(2), false),
        ];

        assert!(validate_change(&mouse, &SettingChange::Profile(ProfileSlot::new(0))).is_ok());
        assert!(matches!(
            validate_change(&mouse, &SettingChange::Profile(ProfileSlot::new(1))),
            Err(CoreError::Validation(_))
        ));
    }

    struct DisconnectOnceTransport {
        inner: MockHidTransport,
        fail_writes: bool,
    }

    impl HidTransport for DisconnectOnceTransport {
        fn descriptor(&self) -> &HidDeviceDescriptor {
            self.inner.descriptor()
        }

        fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
            if self.fail_writes {
                return Err(TransportError::Io(
                    "hidapi error: WriteFile: (0x0000048F) The device is not connected.".to_owned(),
                ));
            }
            self.inner.send_output_report(report)
        }

        fn read_input_report(
            &mut self,
            buf: &mut [u8],
            timeout: std::time::Duration,
        ) -> TransportResult<usize> {
            self.inner.read_input_report(buf, timeout)
        }

        fn drain_input_reports(&mut self) -> TransportResult<usize> {
            self.inner.drain_input_reports()
        }

        fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
            self.inner.send_feature_report(report)
        }

        fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
            self.inner.get_feature_report(buf)
        }
    }

    struct DisconnectAfterOpenTransport {
        inner: MockHidTransport,
        remaining_ok_writes: usize,
    }

    impl HidTransport for DisconnectAfterOpenTransport {
        fn descriptor(&self) -> &HidDeviceDescriptor {
            self.inner.descriptor()
        }

        fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
            if self.remaining_ok_writes == 0 {
                return Err(TransportError::Io(
                    "hidapi error: WriteFile: (0x0000048F) The device is not connected.".to_owned(),
                ));
            }
            self.remaining_ok_writes -= 1;
            self.inner.send_output_report(report)
        }

        fn read_input_report(
            &mut self,
            buf: &mut [u8],
            timeout: std::time::Duration,
        ) -> TransportResult<usize> {
            self.inner.read_input_report(buf, timeout)
        }

        fn drain_input_reports(&mut self) -> TransportResult<usize> {
            self.inner.drain_input_reports()
        }

        fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
            self.inner.send_feature_report(report)
        }

        fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
            self.inner.get_feature_report(buf)
        }
    }

    struct BoxedScriptedEnumerator {
        descriptor: HidDeviceDescriptor,
        transports: Mutex<VecDeque<Option<Box<dyn HidTransport>>>>,
    }

    impl HidEnumerator for BoxedScriptedEnumerator {
        fn enumerate(&self) -> TransportResult<Vec<HidDeviceDescriptor>> {
            Ok(vec![self.descriptor.clone()])
        }

        fn open_path(
            &self,
            _descriptor: &HidDeviceDescriptor,
        ) -> TransportResult<Box<dyn HidTransport>> {
            match lock(&self.transports).pop_front() {
                Some(Some(transport)) => Ok(transport),
                _ => Err(TransportError::OpenFailed {
                    path: self.descriptor.path.clone(),
                    reason: "device temporarily unavailable".to_owned(),
                }),
            }
        }
    }

    #[test]
    fn live_travel_recovers_automatically_from_windows_0x48f_disconnect() {
        let desc = rs7_descriptor();

        // 1. Initial transport succeeds through open + snapshot, then fails with 0x0000048F.
        let mut stale = MockHidTransport::new(desc.clone());
        push_rs7_open(&mut stale);
        push_rs7_snapshot(&mut stale, 120);
        // Count the exact writes consumed by open + snapshot:
        // open(3) + deadzone(1) + actuation(8) + rt(24) + socd(20) + dks(20) + calib(4)
        // + rgb(1) + logo(1) + keymap(28) + layer(1) + macro_sz(1) + config(4) = 116 writes.
        let stale_transport: Box<dyn HidTransport> = Box::new(DisconnectAfterOpenTransport {
            inner: stale,
            remaining_ok_writes: 116,
        });

        // 2. First reopen attempt fails (e.g. keyboard mid-reset); second reopen succeeds
        //    and returns live travel for key 33 (chunk 8, k = 1, 1.90 mm).
        let mut recovered = MockHidTransport::new(desc.clone());
        push_rs7_open(&mut recovered);
        let mut focus_resp = vec![0u8; atk::RS7_PAYLOAD_LEN];
        focus_resp[0] = atk::cmd::GET_KEYBOARD_VALUE;
        focus_resp[8 + 5 + 2..8 + 5 + 4].copy_from_slice(&190u16.to_be_bytes());
        recovered.push_input_response(focus_resp);
        let mut scan_resp = vec![0u8; atk::RS7_PAYLOAD_LEN];
        scan_resp[0] = atk::cmd::GET_KEYBOARD_VALUE;
        recovered.push_input_response(scan_resp);
        let recovered_transport: Box<dyn HidTransport> = Box::new(DisconnectOnceTransport {
            inner: recovered,
            fail_writes: false,
        });

        let enumerator = Arc::new(BoxedScriptedEnumerator {
            descriptor: desc,
            transports: Mutex::new(VecDeque::from([
                Some(stale_transport),
                None,
                Some(recovered_transport),
            ])),
        });
        let service = LiveHidService::with_enumerator_and_mode(enumerator, false);
        let devices = service.refresh().unwrap();
        let id = devices[0].id().clone();

        // First call hits 0x0000048F, drops dead handle, and immediate reopen fails while device
        // is still unavailable.
        let first_err = service
            .read_live_key_travel_focused(&id, Some(KeyIndex::new(33)))
            .unwrap_err();
        assert!(is_disconnect_message(&first_err.to_string()));

        // Next call reopens automatically without calling `refresh()` and returns the sample.
        let samples = service
            .read_live_key_travel_focused(&id, Some(KeyIndex::new(33)))
            .expect("live travel must self-recover without manual refresh");
        assert_eq!(
            samples,
            vec![KeyTravelSample::new(
                KeyIndex::new(33),
                TravelHundredthsMm::from_hundredths(190),
            )]
        );
    }
}
