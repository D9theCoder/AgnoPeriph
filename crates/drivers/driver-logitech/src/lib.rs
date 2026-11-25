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
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::protocol::{
    HIDPP_USAGE_PAGE_USB, HidppReply, LOGITECH_VID, LONG_REPORT_LEN, decode_battery_status,
    decode_battery_voltage, decode_dpi_list, decode_profile_mode, decode_report_rate_mask,
    decode_unified_battery, encode_long_request, encode_profile_mode, feature, hidpp_validator,
    is_excluded_webcam_pid, is_receiver_pid, match_reply, polling_rate_from_ms, polling_rate_to_ms,
};

const READ_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_READ_ATTEMPTS: usize = 8;

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
                "HID++ 2.0 error 0x{code:02X} on feat_idx={feature_index} func={function}"
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
        if mutating {
            self.transport.send_output_report(&req)?;
            if self.transport.is_dry_run() {
                return Ok(HidppReply::Ok([0u8; 16]));
            }
        } else {
            self.transport.send_query_output_report(&req)?;
        }

        let mut buf = [0u8; LONG_REPORT_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let read_len = match self.transport.read_input_report(&mut buf, READ_TIMEOUT) {
                Ok(n) => n,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for Logitech HID++ reply (slot 0x{:02X})",
                        self.device_index
                    )));
                }
                Err(err) => return Err(err.into()),
            };

            if let Some(reply) =
                match_reply(&buf[..read_len], self.device_index, feature_index, function)
            {
                return Ok(reply);
            }
        }

        Err(CoreError::Protocol(
            "no matching HID++ reply received within read window".to_owned(),
        ))
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

        // 3. Firmware version (0x0003)
        if self.resolve_feature(feature::DEVICE_FW_VERSION)?.is_some() {
            let count_resp = self.call_feature(feature::DEVICE_FW_VERSION, 0, &[], false)?;
            if count_resp[0] > 0 {
                let entity = self.call_feature(feature::DEVICE_FW_VERSION, 1, &[0], false)?;
                let prefix = String::from_utf8_lossy(&entity[1..4])
                    .trim_matches('\0')
                    .to_owned();
                let build = u16::from_be_bytes([entity[6], entity[7]]);
                self.info.firmware_version = Some(format!(
                    "{prefix} {:02X}.{:02X}.{build:04X}",
                    entity[4], entity[5]
                ));
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
        if self.resolve_feature(feature::ONBOARD_PROFILES)?.is_some() {
            let info_resp = self.call_feature(feature::ONBOARD_PROFILES, 0, &[], false)?;
            let slot_count = info_resp[3].max(1);
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

        let active_resp = self.call_feature(feature::ONBOARD_PROFILES, 4, &[], false)?;
        let sector = u16::from_be_bytes([active_resp[0], active_resp[1]]);
        let slot_ix = u8::try_from(sector.saturating_sub(1)).unwrap_or(0);
        let active_slot = ProfileSlot::new(slot_ix.min(caps.slot_count.saturating_sub(1)));

        let profiles = (0..caps.slot_count)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();

        Ok(ProfileSnapshot::new(active_slot, mode, profiles))
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        let caps = self.profile_capabilities();
        caps.validate_slot(slot)?;
        let sector = u16::from(slot.index()) + 1;
        self.call_feature(feature::ONBOARD_PROFILES, 3, &sector.to_be_bytes(), true)?;
        Ok(())
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
        let mut frame = vec![0u8; LONG_REPORT_LEN];
        frame[0] = crate::protocol::REPORT_ID_LONG;
        frame[1] = dev_idx;
        frame[2] = feat_idx;
        frame[3] = ((func & 0x0F) << 4) | crate::protocol::SOFTWARE_ID;
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
        //    OnboardProfiles.Info(): 5 profiles
        mock.push_input_response(make_reply(1, 0x07, 0, &[0x01, 0x04, 0x01, 0x05]));
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
        // Profile state: OnboardMemory (1), active sector 2 (slot index 1)
        mock.push_input_response(make_reply(1, 0x07, 2, &[0x01]));
        mock.push_input_response(make_reply(1, 0x07, 4, &[0x00, 0x02]));

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
}
