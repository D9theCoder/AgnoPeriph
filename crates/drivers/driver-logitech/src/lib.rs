//! Logitech HID++ 2.0 peripheral driver implementing `Peripheral`, `PointerDevice`,
//! `BatteryMonitor`, `ProfileManager`, `RgbController`, `KeyboardConfigController`,
//! and `KeymapController`.

#![forbid(unsafe_code)]

mod controls;
mod keyboard;
#[cfg(test)]
mod keyboard_tests;
pub mod protocol;
mod rgb;

pub use controls::LogitechControlInfo;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, ConnectionKind, CoreError, CoreResult, DeviceId,
    DeviceInfo, DeviceMatchRule, DpiStage, DpiValue, DriverDescriptor, KeyboardConfigController,
    KeymapController, Peripheral, PointerCapabilities, PointerDevice, PointerSnapshot,
    PollingRateHz, ProfileCapabilities, ProfileInfo, ProfileManager, ProfileMode, ProfileSlot,
    ProfileSnapshot, RgbController, RgbZoneState, VendorKind,
};

use crate::keyboard::FnInversionVariant;
use crate::rgb::LedDescriptor;
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, ReadMatch};

use crate::protocol::{
    HIDPP_USAGE_PAGE_USB, HIDPP20_ERR_BUSY, HIDPP20_ERR_INVALID_ARGUMENT, HidppReply, LOGITECH_VID,
    OnboardProfileEntry, decode_battery_status, decode_battery_voltage, decode_dpi_list,
    decode_firmware_entity, decode_profile_mode, decode_report_rate_mask, decode_unified_battery,
    encode_long_request, encode_profile_mode, feature, hidpp_validator, hidpp20_error_name,
    is_excluded_webcam_pid, is_receiver_pid, match_reply, parse_profile_directory,
    polling_rate_from_ms, polling_rate_to_ms, resolve_active_profile_slot,
};

const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(1_000);
const MAX_READ_ATTEMPTS: usize = 128;
const MAX_BUSY_RETRIES: usize = 2;
const READ_BUF_LEN: usize = 64;
const DEFAULT_PROFILE_SECTOR_SIZE: u16 = 256;
const MEMORY_TYPE_WRITEABLE: u8 = 0x00;
const MEMORY_TYPE_ROM: u8 = 0x01;

/// Returns the declarative `DriverDescriptor` for registering Logitech HID++ in `DeviceRegistry`.
pub fn driver_descriptor() -> DriverDescriptor {
    DriverDescriptor::new(
        "logitech-hidpp",
        VendorKind::Logitech,
        vec![
            // Wireless receivers (Lightspeed / Bolt / Unifying)
            DeviceMatchRule::new(
                LOGITECH_VID,
                ConnectionKind::WirelessDongle24Ghz,
                "Logitech Lightspeed Receiver",
            )
            .with_pid(0xC547)
            .with_usage_page(HIDPP_USAGE_PAGE_USB),
            DeviceMatchRule::new(
                LOGITECH_VID,
                ConnectionKind::WirelessDongle24Ghz,
                "Logitech Lightspeed Receiver",
            )
            .with_pid(0xC539)
            .with_usage_page(HIDPP_USAGE_PAGE_USB),
            DeviceMatchRule::new(
                LOGITECH_VID,
                ConnectionKind::WirelessDongle24Ghz,
                "Logitech Unifying Receiver",
            )
            .with_pid(0xC52B)
            .with_usage_page(HIDPP_USAGE_PAGE_USB),
            DeviceMatchRule::new(
                LOGITECH_VID,
                ConnectionKind::WirelessDongle24Ghz,
                "Logitech Bolt Receiver",
            )
            .with_pid(0xC548)
            .with_usage_page(HIDPP_USAGE_PAGE_USB),
            // Generic Logitech USB HID++ vendor page (wired or other receivers; webcams rejected on open)
            DeviceMatchRule::new(
                LOGITECH_VID,
                ConnectionKind::WiredUsb,
                "Logitech HID++ Device",
            )
            .with_usage_page(HIDPP_USAGE_PAGE_USB),
        ],
    )
}

/// Active Logitech HID++ 2.0 peripheral backed by a [`GuardedTransport`].
pub struct LogitechHidppDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    device_index: u8,
    info: DeviceInfo,
    capabilities: Capabilities,
    feature_indices: HashMap<u16, u8>,
    pointer_caps: Option<PointerCapabilities>,
    profile_caps: Option<ProfileCapabilities>,
    profile_sector_size: u16,
    profile_entries: Vec<OnboardProfileEntry>,
    ping_marker: u8,
    report_rates: Vec<PollingRateHz>,
    led: Option<LedDescriptor>,
    // The device cannot report these back, so they survive `refresh()`.
    rgb_cache: HashMap<u8, RgbZoneState>,
    is_game_mode_enabled: bool,
    fn_inversion: Option<FnInversionVariant>,
    disable_keys_capacity: Option<usize>,
    controls: Vec<LogitechControlInfo>,
}

impl<T: HidTransport> fmt::Debug for LogitechHidppDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogitechHidppDevice")
            .field("device_index", &self.device_index)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .field("feature_indices", &self.feature_indices)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> LogitechHidppDevice<T> {
    /// Connects to a Logitech HID++ 2.0 endpoint, runs read-only feature discovery,
    /// and populates [`DeviceInfo`] and [`Capabilities`].
    pub fn open(
        transport: T,
        device_index: Option<u8>,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != LOGITECH_VID {
            return Err(CoreError::Validation(format!(
                "expected Logitech VID 0x{LOGITECH_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        if is_excluded_webcam_pid(desc.pid) {
            return Err(CoreError::Validation(format!(
                "refusing to probe Logitech webcam PID 0x{:04X}",
                desc.pid
            )));
        }

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(hidpp_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let connection = if is_receiver_pid(desc.pid) {
            ConnectionKind::WirelessDongle24Ghz
        } else {
            ConnectionKind::WiredUsb
        };
        let resolved_index = device_index.unwrap_or(match connection {
            ConnectionKind::WirelessDongle24Ghz => 1,
            ConnectionKind::WiredUsb | ConnectionKind::Bluetooth => 0xFF,
        });

        let fallback_name = desc
            .product
            .clone()
            .unwrap_or_else(|| format!("Logitech {:04X}", desc.pid));
        let stable_id = DeviceId::new(format!(
            "logitech:{:04X}:{:04X}:slot{:02X}:{}",
            desc.vid,
            desc.pid,
            resolved_index,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let mut dev = Self {
            transport: guarded,
            device_index: resolved_index,
            info: DeviceInfo::new(
                stable_id,
                VendorKind::Logitech,
                fallback_name,
                connection,
                desc.vid,
                desc.pid,
            ),
            capabilities: Capabilities::empty(),
            feature_indices: HashMap::new(),
            pointer_caps: None,
            profile_caps: None,
            profile_sector_size: DEFAULT_PROFILE_SECTOR_SIZE,
            profile_entries: Vec::new(),
            ping_marker: 0x5A,
            report_rates: Vec::new(),
            led: None,
            rgb_cache: HashMap::new(),
            is_game_mode_enabled: false,
            fn_inversion: None,
            disable_keys_capacity: None,
            controls: Vec::new(),
        };

        if let Some(serial) = desc.serial_number {
            dev.info = dev.info.with_serial_number(serial);
        }

        dev.discover_readonly()?;
        Ok(dev)
    }

    /// Returns the underlying [`GuardedTransport`] (useful for inspecting dry-run/mock recordings).
    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    /// Sends a HID++ 2.0 request and waits for its matching response frame.
    fn call_index(
        &mut self,
        feature_index: u8,
        function: u8,
        params: &[u8],
        mutating: bool,
    ) -> CoreResult<[u8; 16]> {
        match self.exchange(feature_index, function, params, mutating)? {
            HidppReply::Ok(data) => Ok(data),
            HidppReply::Hidpp20Error(code) => Err(CoreError::Protocol(format!(
                "HID++ 2.0 error 0x{code:02X} ({}) on feat_idx={feature_index} func={function}",
                hidpp20_error_name(code)
            ))),
            HidppReply::Hidpp10Error(code) => Err(CoreError::Protocol(format!(
                "HID++ 1.0 error 0x{code:02X} on slot 0x{:02X}",
                self.device_index
            ))),
        }
    }

    /// Sends a request and returns the matched reply, including HID++ error frames.
    /// `Err` means no reply was matched (timeout, transport failure, or read window exhausted).
    fn exchange(
        &mut self,
        feature_index: u8,
        function: u8,
        params: &[u8],
        mutating: bool,
    ) -> CoreResult<HidppReply> {
        let req = encode_long_request(self.device_index, feature_index, function, params);
        for attempt in 0..=MAX_BUSY_RETRIES {
            if !mutating || !self.transport.is_dry_run() {
                let _ = self.transport.drain_input_reports();
            }

            if mutating {
                self.transport.send_output_report(&req)?;
                if self.transport.is_dry_run() {
                    return Ok(HidppReply::Ok([0u8; 16]));
                }
            } else {
                self.transport.send_query_output_report(&req)?;
            }

            let reply = self.read_matching_reply(feature_index, function)?;
            if reply == HidppReply::Hidpp20Error(HIDPP20_ERR_BUSY) && attempt < MAX_BUSY_RETRIES {
                let backoff_ms = 50u64 * u64::try_from(attempt + 1).unwrap_or(1);
                std::thread::sleep(Duration::from_millis(backoff_ms));
                continue;
            }
            return Ok(reply);
        }

        unreachable!("busy retry loop always returns on final attempt")
    }

    fn read_matching_reply(&mut self, feature_index: u8, function: u8) -> CoreResult<HidppReply> {
        let device_index = self.device_index;
        let mut buf = [0u8; READ_BUF_LEN];
        match self.transport.read_matching_input(
            &mut buf,
            EXCHANGE_TIMEOUT,
            MAX_READ_ATTEMPTS,
            |frame| match_reply(frame, device_index, feature_index, function),
        )? {
            ReadMatch::Matched(reply) => Ok(reply),
            ReadMatch::TimedOut => Err(CoreError::NotConnected(format!(
                "timed out waiting for Logitech HID++ reply (slot 0x{:02X})",
                self.device_index
            ))),
            ReadMatch::WindowExhausted => Err(CoreError::Protocol(
                "no matching HID++ reply received within read window".to_owned(),
            )),
        }
    }

    /// Returns the cached feature index without querying the device.
    pub(crate) fn cached_feature(&self, feature_id: u16) -> Option<u8> {
        self.feature_indices
            .get(&feature_id)
            .copied()
            .filter(|&idx| idx != 0)
    }

    /// Probes the optional keyboard features. A HID++ error frame marks only that feature as
    /// absent. An unanswered probe stops the pass without caching anything, because
    /// `Root.GetFeature` replies do not echo the feature ID: a late reply would otherwise be
    /// matched to the next probe and cache a wrong index.
    fn probe_keyboard_features(&mut self) -> CoreResult<()> {
        for feature_id in feature::KEYBOARD_PROBE_ORDER {
            if self.feature_indices.contains_key(&feature_id) {
                continue;
            }
            match self.exchange(0x00, 0, &feature_id.to_be_bytes(), false) {
                Ok(HidppReply::Ok(reply)) => {
                    self.feature_indices.insert(feature_id, reply[0]);
                }
                Ok(HidppReply::Hidpp20Error(_) | HidppReply::Hidpp10Error(_)) => {
                    self.feature_indices.insert(feature_id, 0);
                }
                Err(CoreError::NotConnected(msg) | CoreError::Protocol(msg)) => {
                    tracing::warn!(
                        feature = format_args!("0x{feature_id:04X}"),
                        "stopping optional keyboard feature probes: {msg}"
                    );
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    fn discover_keyboard_features(&mut self) -> CoreResult<()> {
        self.probe_keyboard_features()?;

        self.led = match self.discover_led_effects() {
            Ok(led) => led.filter(|l| !l.zones.is_empty()),
            Err(err) => {
                tracing::warn!("disabling Logitech RGB: zone discovery failed: {err}");
                None
            }
        };

        self.fn_inversion = FnInversionVariant::detect(self);

        self.disable_keys_capacity = match self.discover_disable_keys_capacity() {
            Ok(capacity) => capacity,
            Err(err) => {
                tracing::warn!("disabling Logitech DisableKeysByUsage: {err}");
                None
            }
        };

        self.controls = match self.discover_controls() {
            Ok(controls) => controls,
            Err(err) => {
                tracing::warn!("disabling Logitech ReprogControlsV4: {err}");
                Vec::new()
            }
        };
        Ok(())
    }

    fn has_keyboard_config(&self) -> bool {
        self.fn_inversion.is_some() || self.has_game_mode()
    }

    fn resolve_feature(&mut self, feature_id: u16) -> CoreResult<Option<u8>> {
        if let Some(&idx) = self.feature_indices.get(&feature_id) {
            return Ok((idx != 0).then_some(idx));
        }
        let reply = self.call_index(0x00, 0, &feature_id.to_be_bytes(), false)?;
        let idx = reply[0];
        self.feature_indices.insert(feature_id, idx);
        Ok((idx != 0).then_some(idx))
    }

    fn call_feature(
        &mut self,
        feature_id: u16,
        function: u8,
        params: &[u8],
        mutating: bool,
    ) -> CoreResult<[u8; 16]> {
        let Some(idx) = self.resolve_feature(feature_id)? else {
            return Err(CoreError::UnsupportedCapability("hidpp_feature"));
        };
        self.call_index(idx, function, params, mutating)
    }

    /// Performs read-only HID++ 2.0 Root ping, device name, firmware version, and capability discovery.
    fn discover_readonly(&mut self) -> CoreResult<()> {
        // 1. Root Ping (feature 0x0000, func 1)
        let marker = self.ping_marker;
        self.ping_marker = self.ping_marker.wrapping_add(1);
        let ping = self.call_index(0x00, 1, &[0x00, 0x00, marker], false)?;
        if ping[2] != marker {
            return Err(CoreError::Protocol(format!(
                "HID++ Root.Ping marker mismatch: expected 0x{marker:02X}, got 0x{:02X}",
                ping[2]
            )));
        }

        // 2. Device Name (0x0005)
        if self.resolve_feature(feature::DEVICE_NAME_TYPE)?.is_some() {
            let len_resp = self.call_feature(feature::DEVICE_NAME_TYPE, 0, &[], false)?;
            let total_len = usize::from(len_resp[0]);
            if total_len > 0 {
                let mut raw_name = Vec::with_capacity(total_len);
                while raw_name.len() < total_len {
                    let offset = u8::try_from(raw_name.len()).unwrap_or(u8::MAX);
                    let chunk =
                        self.call_feature(feature::DEVICE_NAME_TYPE, 1, &[offset], false)?;
                    let take = (total_len - raw_name.len()).min(16);
                    raw_name.extend_from_slice(&chunk[..take]);
                }
                let text = String::from_utf8_lossy(&raw_name)
                    .trim_end_matches('\0')
                    .trim()
                    .to_owned();
                if !text.is_empty() {
                    self.info.model_name = text;
                }
            }
        }

        // 3. Firmware version (0x0003) — prefer Main Firmware (entity type 0x00) over Bootloader (0x01)
        if self.resolve_feature(feature::DEVICE_FW_VERSION)?.is_some() {
            let count_resp = self.call_feature(feature::DEVICE_FW_VERSION, 0, &[], false)?;
            let entity_count = count_resp[0].min(8);
            let mut main_fw: Option<String> = None;
            let mut fallback_fw: Option<String> = None;
            for entity_ix in 0..entity_count {
                let entity =
                    self.call_feature(feature::DEVICE_FW_VERSION, 1, &[entity_ix], false)?;
                if let Some((kind, version)) = decode_firmware_entity(&entity) {
                    if kind == 0x00 {
                        main_fw = Some(version);
                        break;
                    }
                    if fallback_fw.is_none() {
                        fallback_fw = Some(version);
                    }
                }
            }
            if let Some(version) = main_fw.or(fallback_fw) {
                self.info.firmware_version = Some(version);
            }
        }

        // 4. Battery capability probe (Unified -> Status -> Voltage)
        let has_battery = self.resolve_feature(feature::UNIFIED_BATTERY)?.is_some()
            || self.resolve_feature(feature::BATTERY_STATUS)?.is_some()
            || self.resolve_feature(feature::BATTERY_VOLTAGE)?.is_some();

        // 5. Pointer capability probe (AdjustableDPI + ReportRate)
        let has_dpi = self.resolve_feature(feature::ADJUSTABLE_DPI)?.is_some();
        let has_rate = self.resolve_feature(feature::REPORT_RATE)?.is_some();
        let dpi_bounds = if has_dpi {
            let list_resp = self.call_feature(feature::ADJUSTABLE_DPI, 1, &[0], false)?;
            Some(decode_dpi_list(&list_resp)?)
        } else {
            None
        };
        self.report_rates = if has_rate {
            let mask_resp = self.call_feature(feature::REPORT_RATE, 0, &[], false)?;
            decode_report_rate_mask(mask_resp[0])
        } else {
            Vec::new()
        };
        self.pointer_caps = dpi_bounds.map(|(dpi_min, dpi_max, dpi_step)| {
            PointerCapabilities::new(dpi_min, dpi_max, dpi_step, 5)
                .with_polling_rates(self.report_rates.clone())
        });

        // 6. Onboard profiles capability probe (0x8100)
        self.profile_caps = None;
        self.profile_entries.clear();
        if self.resolve_feature(feature::ONBOARD_PROFILES)?.is_some() {
            let info_resp = self.call_feature(feature::ONBOARD_PROFILES, 0, &[], false)?;
            let slot_count = info_resp[3].max(1);
            let raw_sector_size = u16::from_be_bytes([info_resp[7], info_resp[8]]);
            self.profile_sector_size = if raw_sector_size >= 16 {
                raw_sector_size
            } else {
                DEFAULT_PROFILE_SECTOR_SIZE
            };
            self.profile_caps = Some(ProfileCapabilities::new(slot_count, true, false));
        }

        // 7. Optional keyboard features (RGB, Fn-inversion, Game Mode, control remapping)
        self.discover_keyboard_features()?;

        self.capabilities = Capabilities::empty()
            .with_pointer(self.pointer_caps.is_some())
            .with_battery(has_battery)
            .with_profiles(self.profile_caps.is_some())
            .with_rgb(self.led.is_some())
            .with_keyboard_config(self.has_keyboard_config())
            .with_keymap(!self.controls.is_empty());

        Ok(())
    }
}

impl<T: HidTransport> Peripheral for LogitechHidppDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        self.discover_readonly()
    }

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        self.pointer_caps
            .is_some()
            .then_some(self as &dyn PointerDevice)
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        self.pointer_caps
            .is_some()
            .then_some(self as &mut dyn PointerDevice)
    }

    fn as_battery(&self) -> Option<&dyn BatteryMonitor> {
        self.capabilities
            .has_battery()
            .then_some(self as &dyn BatteryMonitor)
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        self.capabilities
            .has_battery()
            .then_some(self as &mut dyn BatteryMonitor)
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        self.profile_caps
            .is_some()
            .then_some(self as &dyn ProfileManager)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        self.profile_caps
            .is_some()
            .then_some(self as &mut dyn ProfileManager)
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        self.capabilities
            .has_rgb()
            .then_some(self as &dyn RgbController)
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        self.capabilities
            .has_rgb()
            .then_some(self as &mut dyn RgbController)
    }

    fn as_keyboard_config(&self) -> Option<&dyn KeyboardConfigController> {
        self.capabilities
            .has_keyboard_config()
            .then_some(self as &dyn KeyboardConfigController)
    }

    fn as_keyboard_config_mut(&mut self) -> Option<&mut dyn KeyboardConfigController> {
        self.capabilities
            .has_keyboard_config()
            .then_some(self as &mut dyn KeyboardConfigController)
    }

    fn as_keymap(&self) -> Option<&dyn KeymapController> {
        self.capabilities
            .has_keymap()
            .then_some(self as &dyn KeymapController)
    }

    fn as_keymap_mut(&mut self) -> Option<&mut dyn KeymapController> {
        self.capabilities
            .has_keymap()
            .then_some(self as &mut dyn KeymapController)
    }
}

impl<T: HidTransport> BatteryMonitor for LogitechHidppDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        if self.resolve_feature(feature::UNIFIED_BATTERY)?.is_some() {
            let resp = self.call_feature(feature::UNIFIED_BATTERY, 1, &[], false)?;
            return Ok(decode_unified_battery(&resp));
        }
        if self.resolve_feature(feature::BATTERY_STATUS)?.is_some() {
            let resp = self.call_feature(feature::BATTERY_STATUS, 0, &[], false)?;
            return Ok(decode_battery_status(&resp));
        }
        if self.resolve_feature(feature::BATTERY_VOLTAGE)?.is_some() {
            let resp = self.call_feature(feature::BATTERY_VOLTAGE, 0, &[], false)?;
            return Ok(decode_battery_voltage(&resp));
        }
        Err(CoreError::UnsupportedCapability("battery"))
    }
}

impl<T: HidTransport> PointerDevice for LogitechHidppDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps
            .clone()
            .unwrap_or_else(|| PointerCapabilities::new(100, 16_000, 50, 1))
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        let dpi_resp = self.call_feature(feature::ADJUSTABLE_DPI, 2, &[0], false)?;
        let current_dpi = u32::from(u16::from_be_bytes([dpi_resp[1], dpi_resp[2]]));
        let stage = DpiStage::new(0, DpiValue::new(current_dpi)?);

        let polling_rate = if self.resolve_feature(feature::REPORT_RATE)?.is_some() {
            let rate_resp = self.call_feature(feature::REPORT_RATE, 1, &[], false)?;
            polling_rate_from_ms(rate_resp[0])?
        } else {
            PollingRateHz::Hz1000
        };

        Ok(PointerSnapshot::new(0, vec![stage], polling_rate))
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        let caps = self.pointer_capabilities();
        caps.validate_dpi_stages(stages, active_ix)?;
        let active_dpi = stages[usize::from(active_ix)].x.value();
        let dpi_u16 = u16::try_from(active_dpi).map_err(|_| CoreError::OutOfRange {
            field: "dpi",
            actual: active_dpi.to_string(),
            expected: "1..=65535".to_owned(),
        })?;
        let [hi, lo] = dpi_u16.to_be_bytes();
        self.call_feature(feature::ADJUSTABLE_DPI, 3, &[0x00, hi, lo], true)?;
        Ok(())
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        let _ = stage_ix;
        Err(CoreError::UnsupportedCapability(
            "logitech_direct_dpi_stage_switch_requires_set_dpi_stages",
        ))
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        let caps = self.pointer_capabilities();
        caps.validate_polling_rate(rate)?;
        let ms = polling_rate_to_ms(rate)?;
        self.call_feature(feature::REPORT_RATE, 2, &[ms], true)?;
        Ok(())
    }
}

fn disabled_profile_error(slot: ProfileSlot) -> CoreError {
    CoreError::Validation(format!(
        "{slot} is disabled in the device's onboard memory. Enable it in Logitech Onboard Memory Manager or G HUB before switching to it."
    ))
}

impl<T: HidTransport> LogitechHidppDevice<T> {
    /// Reads the 4-byte-per-entry profile directory from `mem_type` (`0x00` writeable or `0x01` ROM),
    /// page `0`, via `OnboardProfiles.MemoryRead` (`0x8100`, func `5`).
    fn read_directory_bank(
        &mut self,
        mem_type: u8,
        slot_count: u8,
    ) -> CoreResult<Vec<OnboardProfileEntry>> {
        let sector_size = usize::from(self.profile_sector_size.max(16));
        let needed_bytes = (usize::from(slot_count) * 4 + 4).min(sector_size);
        let max_offset = sector_size.saturating_sub(16);
        let mut raw = Vec::with_capacity(needed_bytes);
        let mut offset = 0usize;

        while raw.len() < needed_bytes {
            let read_off = offset.min(max_offset);
            let [off_hi, off_lo] = u16::try_from(read_off).unwrap_or(0).to_be_bytes();
            let chunk = self.call_feature(
                feature::ONBOARD_PROFILES,
                5,
                &[mem_type, 0x00, off_hi, off_lo],
                false,
            )?;

            if offset == 0 && (chunk[..4] == [0x00; 4] || chunk[..4] == [0xFF; 4]) {
                return Ok(Vec::new());
            }

            let skip = offset.saturating_sub(read_off);
            let slice = &chunk[skip..];
            raw.extend_from_slice(slice);

            // Stop early as soon as a 4-byte entry hits the 0xFFFF/0x0000 terminator.
            let (entries, _) = slice.as_chunks::<4>();
            if entries
                .iter()
                .any(|&[hi, lo, _, _]| matches!(u16::from_be_bytes([hi, lo]), 0xFFFF | 0x0000))
            {
                break;
            }
            offset += 16;
        }

        Ok(parse_profile_directory(&raw, usize::from(slot_count)))
    }

    /// Populates `self.profile_entries` from the active `0x8100` profile directory
    /// (`0x0000` writeable memory or `0x0100` ROM fallback).
    fn refresh_profile_directory(&mut self, active_sector: u16) -> CoreResult<()> {
        let slot_count = self.profile_capabilities().slot_count;
        let prefer_rom = (active_sector >> 8) == u16::from(MEMORY_TYPE_ROM);
        let banks: &[u8] = if prefer_rom {
            &[MEMORY_TYPE_ROM]
        } else {
            &[MEMORY_TYPE_WRITEABLE, MEMORY_TYPE_ROM]
        };

        for &bank in banks {
            match self.read_directory_bank(bank, slot_count) {
                Ok(entries) if !entries.is_empty() => {
                    self.profile_entries = entries;
                    return Ok(());
                }
                Ok(_) => {}
                Err(CoreError::Protocol(msg)) => {
                    // Some devices (e.g. G305) return ERR_INVALID_ARGUMENT on unwritten
                    // writeable flash sectors; fall through to the ROM bank.
                    tracing::debug!(bank, "0x8100 directory read returned protocol error: {msg}");
                }
                Err(err) => return Err(err),
            }
        }

        // Defensive fallback if neither bank answered (e.g. legacy firmware without MemoryRead).
        self.profile_entries = (1..=slot_count)
            .map(|page| OnboardProfileEntry {
                sector: u16::from(page),
                enabled: true,
            })
            .collect();
        Ok(())
    }

    fn read_active_profile_sector(&mut self, mode: ProfileMode) -> CoreResult<u16> {
        match self.call_feature(feature::ONBOARD_PROFILES, 4, &[], false) {
            Ok(resp) => Ok(u16::from_be_bytes([resp[0], resp[1]])),
            Err(CoreError::Protocol(_)) if mode == ProfileMode::HostControlled => Ok(0),
            Err(err) => Err(err),
        }
    }
}

impl<T: HidTransport> ProfileManager for LogitechHidppDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps
            .clone()
            .unwrap_or_else(|| ProfileCapabilities::new(1, false, false))
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let caps = self.profile_capabilities();
        let mode_resp = self.call_feature(feature::ONBOARD_PROFILES, 2, &[], false)?;
        let mode = decode_profile_mode(mode_resp[0])?;

        let active_sector = self.read_active_profile_sector(mode)?;
        self.refresh_profile_directory(active_sector)?;

        let active_slot =
            resolve_active_profile_slot(active_sector, &self.profile_entries, caps.slot_count);

        let profiles = (0..caps.slot_count)
            .map(|ix| {
                let enabled = self
                    .profile_entries
                    .get(usize::from(ix))
                    .is_some_and(|e| e.enabled);
                ProfileInfo::new(ProfileSlot::new(ix), enabled)
            })
            .collect();

        Ok(ProfileSnapshot::new(active_slot, mode, profiles))
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        let caps = self.profile_capabilities();
        caps.validate_slot(slot)?;

        // Check current mode (read-only) and ensure we have the live directory before any write.
        let mode_resp = self.call_feature(feature::ONBOARD_PROFILES, 2, &[], false)?;
        let current_mode = decode_profile_mode(mode_resp[0])?;

        if self.profile_entries.is_empty() {
            let active_sector = self.read_active_profile_sector(current_mode)?;
            self.refresh_profile_directory(active_sector)?;
        }

        let initial_entry = self
            .profile_entries
            .get(usize::from(slot.index()))
            .copied()
            .filter(|e| e.enabled)
            .ok_or_else(|| disabled_profile_error(slot))?;

        // Logitech 0x8100 rejects SetCurrentProfile (func 3) with INVALID_ARGUMENT (0x02)
        // when the device is in Host Controlled mode (mode 2). Switch to Onboard Memory
        // mode (mode 1) first, matching Solaar and libratbag.
        let mut target_entry = initial_entry;
        if current_mode != ProfileMode::OnboardMemory {
            self.call_feature(
                feature::ONBOARD_PROFILES,
                1,
                &[encode_profile_mode(ProfileMode::OnboardMemory)],
                true,
            )?;
            if !self.transport.is_dry_run() {
                // Entering onboard mode may activate either Writeable (0x00xx) or ROM (0x01xx)
                // memory depending on flash CRC; re-check the active bank so func 3 receives an
                // address from the directory the firmware actually loaded.
                let active_sector = self.read_active_profile_sector(ProfileMode::OnboardMemory)?;
                self.refresh_profile_directory(active_sector)?;
                target_entry = self
                    .profile_entries
                    .get(usize::from(slot.index()))
                    .copied()
                    .filter(|e| e.enabled)
                    .ok_or_else(|| disabled_profile_error(slot))?;
            }
        }

        let idx = self
            .resolve_feature(feature::ONBOARD_PROFILES)?
            .ok_or(CoreError::UnsupportedCapability("hidpp_feature"))?;

        match self.exchange(idx, 3, &target_entry.sector.to_be_bytes(), true)? {
            HidppReply::Ok(_) => Ok(()),
            HidppReply::Hidpp20Error(HIDPP20_ERR_INVALID_ARGUMENT)
                if !self.transport.is_dry_run() =>
            {
                // Directory may have changed externally or fallen back to ROM; refresh once
                // from the live bank and retry if the sector address changed.
                let active_sector = self.read_active_profile_sector(ProfileMode::OnboardMemory)?;
                self.refresh_profile_directory(active_sector)?;
                let refreshed = self
                    .profile_entries
                    .get(usize::from(slot.index()))
                    .copied()
                    .filter(|e| e.enabled)
                    .ok_or_else(|| disabled_profile_error(slot))?;
                if refreshed.sector != target_entry.sector {
                    self.call_index(idx, 3, &refreshed.sector.to_be_bytes(), true)?;
                    return Ok(());
                }
                Err(CoreError::Protocol(format!(
                    "HID++ 2.0 error 0x{HIDPP20_ERR_INVALID_ARGUMENT:02X} ({}) on feat_idx={idx} func=3",
                    hidpp20_error_name(HIDPP20_ERR_INVALID_ARGUMENT)
                )))
            }
            HidppReply::Hidpp20Error(code) => Err(CoreError::Protocol(format!(
                "HID++ 2.0 error 0x{code:02X} ({}) on feat_idx={idx} func=3",
                hidpp20_error_name(code)
            ))),
            HidppReply::Hidpp10Error(code) => Err(CoreError::Protocol(format!(
                "HID++ 1.0 error 0x{code:02X} on slot 0x{:02X}",
                self.device_index
            ))),
        }
    }

    fn set_profile_mode(&mut self, mode: ProfileMode) -> CoreResult<()> {
        let raw = encode_profile_mode(mode);
        self.call_feature(feature::ONBOARD_PROFILES, 1, &[raw], true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::ChargingState;
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    pub(crate) fn make_reply(dev_idx: u8, feat_idx: u8, func: u8, params: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; crate::protocol::LONG_REPORT_LEN];
        frame[0] = crate::protocol::REPORT_ID_LONG;
        frame[1] = dev_idx;
        frame[2] = feat_idx;
        frame[3] = ((func & 0x0F) << 4) | crate::protocol::SOFTWARE_ID;
        frame[4..4 + params.len()].copy_from_slice(params);
        frame
    }

    fn make_hidpp20_error(dev_idx: u8, feat_idx: u8, func: u8, code: u8) -> Vec<u8> {
        let mut frame = vec![0u8; crate::protocol::LONG_REPORT_LEN];
        frame[0] = crate::protocol::REPORT_ID_LONG;
        frame[1] = dev_idx;
        frame[2] = crate::protocol::HIDPP20_ERROR_SUB_ID;
        frame[3] = feat_idx;
        frame[4] = ((func & 0x0F) << 4) | crate::protocol::SOFTWARE_ID;
        frame[5] = code;
        frame
    }

    fn make_notification(dev_idx: u8, feat_idx: u8, event: u8, params: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; crate::protocol::LONG_REPORT_LEN];
        frame[0] = crate::protocol::REPORT_ID_LONG;
        frame[1] = dev_idx;
        frame[2] = feat_idx;
        frame[3] = (event & 0x0F) << 4; // sw_id == 0 => unsolicited notification
        frame[4..4 + params.len()].copy_from_slice(params);
        frame
    }

    fn sample_lightspeed_mock() -> MockHidTransport {
        let desc = HidDeviceDescriptor::new(
            LOGITECH_VID,
            0xC547,
            HIDPP_USAGE_PAGE_USB,
            0x0002,
            2,
            "mock://logitech/lightspeed",
        );
        let mut mock = MockHidTransport::new(desc);

        // 1. Root Ping (feat 0, func 1) -> HID++ 4.2, echo 0x5A
        mock.push_input_response(make_reply(1, 0x00, 1, &[0x04, 0x02, 0x5A]));
        // 2. GetFeature(0x0005 DeviceNameType) -> idx 2
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x02]));
        //    Name len = 10 ("PRO X SL 2")
        mock.push_input_response(make_reply(1, 0x02, 0, &[10]));
        mock.push_input_response(make_reply(1, 0x02, 1, b"PRO X SL 2"));
        // 3. GetFeature(0x0003 DeviceFWVersion) -> idx 3
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x03]));
        mock.push_input_response(make_reply(1, 0x03, 0, &[1]));
        mock.push_input_response(make_reply(
            1,
            0x03,
            1,
            &[0x01, b'R', b'Q', b'M', 0x32, 0x01, 0x00, 0x17],
        ));
        // 4. GetFeature(0x1004 UnifiedBattery) -> idx 4
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x04]));
        // 5. GetFeature(0x2201 AdjustableDPI) -> idx 5
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x05]));
        // 6. GetFeature(0x8060 ReportRate) -> idx 6
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x06]));
        //    AdjustableDPI.List(sensor=0): 100 .. step 50 .. 25600
        //    0x0064 (100), 0xE032 (step 50: 0b111 << 13 | 50), 0x6400 (25600)
        mock.push_input_response(make_reply(
            1,
            0x05,
            1,
            &[0x00, 0x00, 0x64, 0xE0, 0x32, 0x64, 0x00],
        ));
        //    ReportRate.List(): bitmask 0x8B => 1ms(bit0), 2ms(bit1), 4ms(bit3), 8ms(bit7)
        mock.push_input_response(make_reply(1, 0x06, 0, &[0x8B]));
        // 7. GetFeature(0x8100 OnboardProfiles) -> idx 7
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x07]));
        //    OnboardProfiles.Info(): 5 profiles, 1 OOB, 5 buttons, 6 sectors, 256-byte sector
        mock.push_input_response(make_reply(
            1,
            0x07,
            0,
            &[0x01, 0x04, 0x01, 0x05, 0x01, 0x05, 0x06, 0x01, 0x00],
        ));
        // 8. Keyboard feature probes (feature::KEYBOARD_PROBE_ORDER) -> all absent
        for _ in feature::KEYBOARD_PROBE_ORDER {
            mock.push_input_response(make_reply(1, 0x00, 0, &[0x00]));
        }

        mock
    }

    #[test]
    fn discovers_capabilities_and_reads_battery_pointer_and_profiles() {
        let mut mock = sample_lightspeed_mock();
        // Battery read (0x1004, func 1) -> 84%, discharging
        mock.push_input_response(make_reply(1, 0x04, 1, &[84, 0x00, 0x00, 0x00]));
        // Pointer state: DPI = 800 (0x0320), ReportRate = 1 ms (1000 Hz)
        mock.push_input_response(make_reply(1, 0x05, 2, &[0x00, 0x03, 0x20, 0x03, 0x20]));
        mock.push_input_response(make_reply(1, 0x06, 1, &[0x01]));
        // Profile state: OnboardMemory (1), active sector 2 (slot index 1), writeable directory
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x01]));
        mock.push_input_response(make_reply(1, 0x07, 4, &[0x00, 0x02]));
        // Sector 0x0000 directory: 5 enabled writeable profiles (2 reads: off 0 & off 16)
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[
                0x00, 0x01, 0x01, 0x00, 0x00, 0x02, 0x01, 0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x04,
                0x01, 0x00,
            ],
        ));
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[
                0x00, 0x05, 0x01, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                0xFF, 0xFF,
            ],
        ));

        let mut dev = LogitechHidppDevice::open(mock, Some(1), false, None).unwrap();
        assert_eq!(dev.info().model_name, "PRO X SL 2");
        assert_eq!(
            dev.info().firmware_version.as_deref(),
            Some("RQM 32.01.0017")
        );
        assert_eq!(dev.info().connection, ConnectionKind::WirelessDongle24Ghz);
        assert!(dev.capabilities().has_pointer());
        assert!(dev.capabilities().has_battery());
        assert!(dev.capabilities().has_profiles());
        assert!(!dev.capabilities().has_rgb());
        assert!(!dev.capabilities().has_keyboard_config());
        assert!(!dev.capabilities().has_keymap());
        assert!(dev.as_rgb().is_none());

        let battery = dev.as_battery_mut().unwrap().read_battery().unwrap();
        assert_eq!(battery.percentage, Some(84));
        assert_eq!(battery.state, ChargingState::Discharging);

        let pointer = dev.as_pointer_mut().unwrap();
        let caps = pointer.pointer_capabilities();
        assert_eq!(
            (caps.dpi_min, caps.dpi_max, caps.dpi_step),
            (100, 25_600, 50)
        );
        assert_eq!(
            caps.supported_polling_rates,
            vec![
                PollingRateHz::Hz125,
                PollingRateHz::Hz250,
                PollingRateHz::Hz500,
                PollingRateHz::Hz1000
            ]
        );
        let snap = pointer.read_pointer_state().unwrap();
        assert_eq!(snap.dpi_stages[0].x.value(), 800);
        assert_eq!(snap.polling_rate, PollingRateHz::Hz1000);

        let prof = dev.as_profiles_mut().unwrap().read_profile_state().unwrap();
        assert_eq!(prof.mode, ProfileMode::OnboardMemory);
        assert_eq!(prof.active_slot, ProfileSlot::new(1));
    }

    #[test]
    fn writes_dpi_and_polling_rate_with_validation() {
        let mut mock = sample_lightspeed_mock();
        // Reply for SetSensorDpi (feat 5, func 3) and SetReportRate (feat 6, func 2)
        mock.push_input_response(make_reply(1, 0x05, 3, &[0x00, 0x06, 0x40]));
        mock.push_input_response(make_reply(1, 0x06, 2, &[0x02]));

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LogitechHidppDevice::open(mock, Some(1), false, Some(sink.clone())).unwrap();

        // Out-of-step DPI must be rejected before touching transport.
        let bad_stages = vec![DpiStage::new(0, DpiValue::new(825).unwrap())];
        assert!(dev.set_dpi_stages(&bad_stages, 0).is_err());

        // Valid 1600 DPI write.
        let good_stages = vec![DpiStage::new(0, DpiValue::new(1600).unwrap())];
        dev.set_dpi_stages(&good_stages, 0).unwrap();

        // Valid 500 Hz write.
        dev.set_polling_rate(PollingRateHz::Hz500).unwrap();

        let writes = dev.transport().inner().recorded_writes();
        let dpi_write = &writes[writes.len() - 2].bytes;
        assert_eq!(dpi_write[0], 0x11);
        assert_eq!(dpi_write[2], 0x05);
        assert_eq!(dpi_write[3], 0x3C); // func 3 | sw_id 0x0C
        assert_eq!(&dpi_write[4..7], &[0x00, 0x06, 0x40]); // sensor 0, 1600 BE

        let rate_write = &writes[writes.len() - 1].bytes;
        assert_eq!(rate_write[2], 0x06);
        assert_eq!(rate_write[3], 0x2C); // func 2 | sw_id 0x0C
        assert_eq!(rate_write[4], 0x02); // 2 ms = 500 Hz

        assert!(
            sink.snapshot()
                .iter()
                .any(|e| e.outcome == PacketOutcome::Transmitted)
        );
    }

    #[test]
    fn refuses_webcam_pid_and_decodes_voltage_curve() {
        let webcam = HidDeviceDescriptor::new(
            LOGITECH_VID,
            0x085E,
            HIDPP_USAGE_PAGE_USB,
            0x0001,
            0,
            "mock://webcam",
        );
        let mock = MockHidTransport::new(webcam);
        assert!(LogitechHidppDevice::open(mock, None, false, None).is_err());

        let batt =
            decode_battery_voltage(&[0x0F, 0xA0, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(batt.voltage_mv, Some(4000));
        assert_eq!(batt.percentage, Some(81));
        assert_eq!(batt.state, ChargingState::Discharging);
    }

    #[test]
    fn prefers_main_firmware_entity_over_bootloader() {
        let desc = HidDeviceDescriptor::new(
            LOGITECH_VID,
            0xC547,
            HIDPP_USAGE_PAGE_USB,
            0x0002,
            2,
            "mock://logitech/pro-x-wireless",
        );
        let mut mock = MockHidTransport::new(desc);
        mock.push_input_response(make_reply(1, 0x00, 1, &[0x04, 0x02, 0x5A]));
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x02]));
        mock.push_input_response(make_reply(1, 0x02, 0, &[14]));
        mock.push_input_response(make_reply(1, 0x02, 1, b"PRO X Wireless"));
        mock.push_input_response(make_reply(1, 0x00, 0, &[0x03]));
        mock.push_input_response(make_reply(1, 0x03, 0, &[2]));
        // Entity 0 is Bootloader (type 0x01), Entity 1 is Main Firmware (type 0x00)
        mock.push_input_response(make_reply(
            1,
            0x03,
            1,
            &[0x01, b'B', b'L', b'1', 0x25, 0x00, 0x00, 0x13],
        ));
        mock.push_input_response(make_reply(
            1,
            0x03,
            1,
            &[0x00, b'M', b'P', b'K', 0x25, 0x01, 0x00, 0x18],
        ));
        // Remaining feature probes absent
        for _ in 0..(6 + feature::KEYBOARD_PROBE_ORDER.len()) {
            mock.push_input_response(make_reply(1, 0x00, 0, &[0x00]));
        }

        let dev = LogitechHidppDevice::open(mock, Some(1), false, None).unwrap();
        assert_eq!(dev.info().model_name, "PRO X Wireless");
        assert_eq!(
            dev.info().firmware_version.as_deref(),
            Some("MPK 25.01.0018")
        );
    }

    #[test]
    fn host_mode_switches_to_onboard_mode_before_selecting_profile() {
        let mut mock = sample_lightspeed_mock();
        // read_profile_state: Host mode (2), active sector 0x0000, writeable directory has slots 1 & 2 enabled
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x02]));
        mock.push_input_response(make_reply(1, 0x07, 4, &[0x00, 0x00]));
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[
                0x00, 0x01, 0x01, 0x00, 0x00, 0x02, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04,
                0x00, 0x00,
            ],
        ));
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[0x00, 0x05, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00],
        ));

        // set_active_profile(slot 1 = Profile 2):
        // 1. GetMode -> Host (2)
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x02]));
        // 2. SetMode(1 = Onboard) -> ok
        mock.push_input_response(make_reply(1, 0x07, 1, &[]));
        // 3. GetCurrentProfile -> 0x0001 (confirms writeable bank is active)
        mock.push_input_response(make_reply(1, 0x07, 4, &[0x00, 0x01]));
        // 4. Refresh writeable directory
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[
                0x00, 0x01, 0x01, 0x00, 0x00, 0x02, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04,
                0x00, 0x00,
            ],
        ));
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[0x00, 0x05, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00],
        ));
        // 5. SetCurrentProfile(0x0002) -> ok
        mock.push_input_response(make_reply(1, 0x07, 3, &[]));

        // Attempting disabled Profile 3 (slot 2) only reads GetMode and rejects before any write
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x01]));

        let mut dev = LogitechHidppDevice::open(mock, Some(1), false, None).unwrap();
        let snap = dev.as_profiles_mut().unwrap().read_profile_state().unwrap();
        assert_eq!(snap.mode, ProfileMode::HostControlled);
        assert_eq!(snap.active_slot, ProfileSlot::new(0));
        let enabled: Vec<bool> = snap.profiles.iter().map(ProfileInfo::is_enabled).collect();
        assert_eq!(enabled, vec![true, true, false, false, false]);

        dev.as_profiles_mut()
            .unwrap()
            .set_active_profile(ProfileSlot::new(1))
            .unwrap();

        let writes = dev.transport().inner().recorded_writes();
        let set_mode = writes
            .iter()
            .find(|w| w.bytes[2] == 0x07 && w.bytes[3] == 0x1C)
            .expect("must switch to Onboard mode before SetCurrentProfile");
        assert_eq!(set_mode.bytes[4], 0x01);
        let set_prof = writes.last().unwrap();
        assert_eq!(set_prof.bytes[2], 0x07);
        assert_eq!(set_prof.bytes[3], 0x3C);
        assert_eq!(&set_prof.bytes[4..6], &[0x00, 0x02]);

        let writes_before = dev.transport().inner().recorded_writes().len();
        let err = dev
            .as_profiles_mut()
            .unwrap()
            .set_active_profile(ProfileSlot::new(2))
            .unwrap_err();
        assert!(matches!(err, CoreError::Validation(_)));
        // Only the read-only GetMode ran; no SetCurrentProfile was transmitted.
        let writes_after = dev.transport().inner().recorded_writes();
        assert_eq!(writes_after.len(), writes_before + 1);
        assert_eq!(writes_after.last().unwrap().bytes[3], 0x2C);
    }

    #[test]
    fn rom_directory_fallback_resolves_and_switches_rom_sectors() {
        let mut mock = sample_lightspeed_mock();
        // Onboard mode, active profile is ROM sector 0x0102 (Profile 2)
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x01]));
        mock.push_input_response(make_reply(1, 0x07, 4, &[0x01, 0x02]));
        // Because active_sector high byte is 0x01 (ROM), driver reads ROM directory 0x0100 directly
        mock.push_input_response(make_reply(
            1,
            0x07,
            5,
            &[
                0x01, 0x01, 0x01, 0x00, 0x01, 0x02, 0x01, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF,
                0xFF, 0xFF,
            ],
        ));
        // Switch to Profile 1 (ROM sector 0x0101): GetMode(1) + SetCurrentProfile(0x0101)
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x01]));
        mock.push_input_response(make_reply(1, 0x07, 3, &[]));

        let mut dev = LogitechHidppDevice::open(mock, Some(1), false, None).unwrap();
        let snap = dev.as_profiles_mut().unwrap().read_profile_state().unwrap();
        assert_eq!(snap.active_slot, ProfileSlot::new(1));
        let enabled: Vec<bool> = snap.profiles.iter().map(ProfileInfo::is_enabled).collect();
        assert_eq!(enabled, vec![true, true, false, false, false]);

        dev.as_profiles_mut()
            .unwrap()
            .set_active_profile(ProfileSlot::new(0))
            .unwrap();
        let last = dev.transport().inner().recorded_writes().last().unwrap();
        assert_eq!(last.bytes[2], 0x07);
        assert_eq!(last.bytes[3], 0x3C);
        assert_eq!(&last.bytes[4..6], &[0x01, 0x01]);
    }

    #[test]
    fn drains_stale_reports_skips_notification_bursts_and_retries_busy() {
        let mut mock = sample_lightspeed_mock();
        // Queue 20 stale reports sitting in the OS input buffer before our request
        for i in 0..20u8 {
            mock.push_stale_input_report(make_notification(1, 0x04, 0, &[i]));
        }
        // First attempt: 12 mid-flight unsolicited notifications arrive before a BUSY (0x08) error
        for i in 0..12u8 {
            mock.push_input_response(make_notification(1, 0x07, 0, &[0x00, i]));
        }
        mock.push_input_response(make_hidpp20_error(1, 0x04, 1, HIDPP20_ERR_BUSY));
        // Retry attempt succeeds with 85% battery
        mock.push_input_response(make_reply(1, 0x04, 1, &[85, 0x00, 0x00, 0x00]));

        let mut dev = LogitechHidppDevice::open(mock, Some(1), false, None).unwrap();
        let battery = dev.as_battery_mut().unwrap().read_battery().unwrap();
        assert_eq!(battery.percentage, Some(85));
    }
}
