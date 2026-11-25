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
    KeyHallEffectConfig, KeyIndex, LiftOffDistance, Peripheral, PointerCapabilities,
    PointerSnapshot, PollingRateHz, ProfileCapabilities, ProfileSlot, ProfileSnapshot,
    RapidTriggerConfig, RgbCapabilities, RgbEffect, RgbSnapshot, RgbZoneId, SocdPair,
    TravelHundredthsMm,
};
use agnoperiph_driver_atk::AtkRs7Device;
use agnoperiph_driver_lamzu::LamzuAtlantisDevice;
use agnoperiph_driver_logitech::LogitechHidppDevice;
use agnoperiph_transport::{
    HidDeviceDescriptor, HidEnumerator, HidTransport, HidapiEnumerator, MemoryPacketSink,
    PacketSink, PacketTraceEntry, TransportResult,
};

mod mock;

pub use mock::{MOCK_KEYBOARD_ID, MOCK_MOUSE_ID, MockPeripheralService};

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
    RapidTrigger {
        key: KeyIndex,
        config: RapidTriggerConfig,
    },
    /// Writes a DKS binding into the hardware slot `binding.key` (`0..max_dks_bindings`).
    Dks(DksBinding),
    ClearDks {
        slot: KeyIndex,
    },
    Socd(Vec<SocdPair>),
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

    /// Returns the most recent traced HID frames, oldest first.
    fn packet_log(&self) -> Vec<PacketTraceEntry>;

    fn clear_packet_log(&self);

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

/// Validates a change against the capability limits reported in `snapshot`.
///
/// Drivers validate again before encoding; validating here keeps dry-run previews
/// from ever showing values the device would reject.
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
        SettingChange::RapidTrigger { key, config } => {
            let he = require(snapshot.hall_effect.as_ref(), "hall_effect")?;
            he.capabilities.validate_key(*key)?;
            he.capabilities.validate_rapid_trigger(config)
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
            he.capabilities.validate_socd_pairs(pairs)
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
            profiles.capabilities.validate_slot(*slot)
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
    }
}

/// Applies an already validated change to a snapshot without touching hardware.
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
        SettingChange::RapidTrigger { key, config } => {
            if let Some(entry) = snapshot
                .hall_effect
                .as_mut()
                .and_then(|he| he.snapshot.keys.iter_mut().find(|k| k.key == *key))
            {
                entry.rapid_trigger = *config;
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

    DeviceSnapshot {
        info: peripheral.info().clone(),
        capabilities: peripheral.capabilities(),
        pointer,
        hall_effect,
        rgb,
        battery,
        profiles,
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
            }
        }
        SettingChange::Actuation { target, point } => {
            let he = require(peripheral.as_hall_effect_mut(), "hall_effect")?;
            match target {
                ActuationTarget::Key(key) => he.set_key_actuation(*key, *point),
                ActuationTarget::AllKeys => he.set_global_actuation(*point),
            }
        }
        SettingChange::RapidTrigger { key, config } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?
                .set_rapid_trigger(*key, *config)
        }
        SettingChange::Dks(binding) => require(peripheral.as_hall_effect_mut(), "hall_effect")?
            .set_dks_binding(binding.clone()),
        SettingChange::ClearDks { slot } => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.clear_dks_binding(*slot)
        }
        SettingChange::Socd(pairs) => {
            require(peripheral.as_hall_effect_mut(), "hall_effect")?.set_socd_pairs(pairs)
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
        "atk-rs7" => Ok(Box::new(AtkRs7Device::open(wrapped, dry_run, Some(sink))?)),
        other => Err(CoreError::DeviceNotFound(format!(
            "no driver registered as '{other}'"
        ))),
    }
}

struct LiveDevice {
    peripheral: Box<dyn Peripheral>,
    snapshot: DeviceSnapshot,
}

struct LiveState {
    dry_run: bool,
    devices: Vec<LiveDevice>,
}

/// Real Windows HID backend driving Logitech, Lamzu and ATK peripherals through `hidapi`.
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
        Self::with_enumerator(Arc::new(HidapiEnumerator::new()))
    }

    /// Creates a [`LiveHidService`] backed by a custom [`HidEnumerator`] (used by real-driver
    /// integration tests with scripted `MockHidTransport` replies).
    pub fn with_enumerator(enumerator: Arc<dyn HidEnumerator>) -> Self {
        Self {
            enumerator,
            state: Mutex::new(LiveState {
                dry_run: true,
                devices: Vec::new(),
            }),
            sink: Arc::new(SequencedSink::new()),
        }
    }
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
                peripheral,
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
        let target = state
            .devices
            .iter_mut()
            .find(|d| d.snapshot.info.id == *device)
            .ok_or_else(|| ApplyError::rejected(CoreError::DeviceNotFound(device.to_string())))?;

        if let Err(err) = validate_change(&target.snapshot, &change) {
            return Err(ApplyError::rejected(err));
        }

        if let Err(err) = apply_to_peripheral(target.peripheral.as_mut(), &change) {
            if dry_run {
                return Err(ApplyError::rejected(err));
            }
            // Frames may have already reached hardware before the failure (e.g. a timeout
            // mid-chunk during a global actuation write); re-read best-effort live state.
            let refreshed = collect_snapshot(target.peripheral.as_mut());
            target.snapshot = refreshed.clone();
            return Err(ApplyError::failed_after_send(err, Some(refreshed)));
        }

        if dry_run {
            apply_change_to_snapshot(&mut target.snapshot, &change);
            target.snapshot.is_preview = true;
        } else {
            target.snapshot = collect_snapshot(target.peripheral.as_mut());
        }
        Ok(target.snapshot.clone())
    }

    fn packet_log(&self) -> Vec<PacketTraceEntry> {
        self.sink.log.snapshot()
    }

    fn clear_packet_log(&self) {
        self.sink.log.clear();
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
        // Push a wrong-command byte 8 times to trigger Protocol failure after send, then fresh snapshot.
        for _ in 0..8 {
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
}
