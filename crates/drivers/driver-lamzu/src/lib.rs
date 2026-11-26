//! Lamzu peripheral driver (Atlantis CompX Report-8 + extensible Aurora codec seam).

#![forbid(unsafe_code)]

pub mod atlantis;
pub mod aurora;
pub mod crystall;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    BatteryMonitor, BatteryStatus, Capabilities, CoreError, CoreResult, DebounceMs, DeviceId,
    DeviceInfo, DeviceMatchRule, DpiStage, DriverDescriptor, LiftOffDistance, MacroCapabilities,
    MacroController, MacroSlot, Peripheral, PointerCapabilities, PointerDevice, PointerSnapshot,
    PollingRateHz, ProfileCapabilities, ProfileInfo, ProfileManager, ProfileMode, ProfileSlot,
    ProfileSnapshot, VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::atlantis::{
    ATLANTIS_MAX_PAYLOAD, ATLANTIS_PACKET_LEN, ATLANTIS_PRODUCTS, AtlantisProductProfile,
    AtlantisRateFamily, AtlantisReply, Command, LAMZU_ATLANTIS_USAGE_ID, LAMZU_ATLANTIS_USAGE_PAGE,
    LAMZU_ATLANTIS_VID, atlantis_validator, decode_battery, decode_dpi_color, decode_dpi_stage,
    decode_firmware, decode_lod, decode_polling_rate, decode_reply, encode_dpi_stage, encode_lod,
    encode_polling_rate, encode_request, flash, is_checksum_valid, lookup_product, seal_field,
};

const READ_TIMEOUT: Duration = Duration::from_millis(600);
const MAX_READ_ATTEMPTS: usize = 6;

/// Returns true for firmware images that store the packed macro slots (`v2.x` and later).
fn firmware_supports_macros(version: &str) -> bool {
    version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse::<u16>().ok())
        .is_some_and(|major| major >= 2)
}

/// Returns the declarative `DriverDescriptor` for registering Lamzu Atlantis wired and dongle PIDs.
pub fn driver_descriptor() -> DriverDescriptor {
    let rules = ATLANTIS_PRODUCTS
        .iter()
        .map(|profile| {
            DeviceMatchRule::new(LAMZU_ATLANTIS_VID, profile.connection, profile.model_name)
                .with_pid(profile.pid)
                .with_usage_page(LAMZU_ATLANTIS_USAGE_PAGE)
                .with_usage_id(LAMZU_ATLANTIS_USAGE_ID)
        })
        .collect();

    DriverDescriptor::new("lamzu-atlantis", VendorKind::Lamzu, rules)
}

/// Active Lamzu Atlantis peripheral backed by a checksum-validated [`GuardedTransport`].
pub struct LamzuAtlantisDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    profile: AtlantisProductProfile,
    info: DeviceInfo,
    capabilities: Capabilities,
    pointer_caps: PointerCapabilities,
    profile_caps: ProfileCapabilities,
}

impl<T: HidTransport> fmt::Debug for LamzuAtlantisDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LamzuAtlantisDevice")
            .field("profile", &self.profile)
            .field("info", &self.info)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> LamzuAtlantisDevice<T> {
    /// Connects to a Lamzu Atlantis wired or wireless receiver HID interface,
    /// enforces strict Report-8 17-byte + checksum validation, and reads firmware info.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid != LAMZU_ATLANTIS_VID {
            return Err(CoreError::Validation(format!(
                "expected Lamzu Atlantis VID 0x{LAMZU_ATLANTIS_VID:04X}, got 0x{:04X}",
                desc.vid
            )));
        }
        let profile = lookup_product(desc.pid).ok_or_else(|| {
            CoreError::Validation(format!("unsupported Lamzu Atlantis PID 0x{:04X}", desc.pid))
        })?;

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(atlantis_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let stable_id = DeviceId::new(format!(
            "lamzu:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let supported_rates = match profile.rate_family {
            AtlantisRateFamily::Wired => vec![
                PollingRateHz::Hz125,
                PollingRateHz::Hz250,
                PollingRateHz::Hz500,
                PollingRateHz::Hz1000,
            ],
            AtlantisRateFamily::Pulsar4k => vec![
                PollingRateHz::Hz125,
                PollingRateHz::Hz250,
                PollingRateHz::Hz500,
                PollingRateHz::Hz1000,
                PollingRateHz::Hz2000,
                PollingRateHz::Hz4000,
            ],
        };

        let pointer_caps = PointerCapabilities::new(
            flash::MIN_DPI,
            flash::MAX_DPI,
            flash::DPI_STEP,
            flash::MAX_DPI_STAGES,
        )
        .with_xy_independent_dpi(true)
        .with_polling_rates(supported_rates)
        .with_lods(vec![
            LiftOffDistance::TenthsMm(10),
            LiftOffDistance::TenthsMm(20),
        ])
        .with_debounce_range_ms(0, flash::MAX_DEBOUNCE_MS)
        .with_motion_sync(true)
        .with_angle_snapping(true)
        .with_ripple_control(true);

        let mut info = DeviceInfo::new(
            stable_id,
            VendorKind::Lamzu,
            profile.model_name,
            profile.connection,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            profile,
            info,
            capabilities: Capabilities::empty()
                .with_pointer(true)
                .with_battery(true)
                .with_profiles(true),
            pointer_caps,
            profile_caps: ProfileCapabilities::new(flash::PROFILE_COUNT, false, false),
        };

        // Read-only firmware query during open (non-fatal if dongle reports sleeping mouse).
        if let Ok(reply) = dev.send_command(Command::ReadFirmwareVersion, 0, &[], true) {
            dev.info.firmware_version = Some(decode_firmware(&reply.payload));
        }
        // The packed macro slots only exist from the v2 profile image onward; older
        // firmwares use the compact map and must not receive these writes.
        if dev
            .info
            .firmware_version
            .as_deref()
            .is_some_and(firmware_supports_macros)
        {
            dev.capabilities = dev.capabilities.with_macros(true);
        }

        Ok(dev)
    }

    /// Returns the underlying [`GuardedTransport`] for inspecting traces or mock recordings.
    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    fn send_command(
        &mut self,
        cmd: Command,
        address: u16,
        payload: &[u8],
        ignore_len: bool,
    ) -> CoreResult<AtlantisReply> {
        let req = encode_request(cmd, address, payload)?;
        let is_mutating = matches!(cmd, Command::WriteFlash | Command::WriteActiveProfile);

        if !is_mutating || !self.transport.is_dry_run() {
            let _ = self.transport.drain_input_reports();
        }

        if is_mutating {
            self.transport.send_output_report(&req)?;
            if self.transport.is_dry_run() {
                return Ok(AtlantisReply {
                    command: cmd,
                    error: 0,
                    address,
                    declared_len: payload.len(),
                    payload: [0u8; ATLANTIS_MAX_PAYLOAD],
                });
            }
        } else {
            self.transport.send_query_output_report(&req)?;
        }

        let mut buf = [0u8; ATLANTIS_PACKET_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let read_len = match self.transport.read_input_report(&mut buf, READ_TIMEOUT) {
                Ok(n) => n,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for Lamzu Atlantis reply to {cmd:?}"
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            if let Some(reply) = decode_reply(&buf[..read_len])
                && reply.command == cmd
                && (ignore_len || reply.declared_len == payload.len())
            {
                if reply.error != 0 {
                    return Err(CoreError::Protocol(format!(
                        "Lamzu Atlantis returned error status 0x{:02X} for {cmd:?}",
                        reply.error
                    )));
                }
                return Ok(reply);
            }
        }

        Err(CoreError::Protocol(format!(
            "no matching Lamzu Atlantis reply for {cmd:?}"
        )))
    }

    fn read_flash_raw(&mut self, address: u16, len: usize) -> CoreResult<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let chunk_len = (len - out.len()).min(ATLANTIS_MAX_PAYLOAD);
            let offset = u16::try_from(out.len()).unwrap_or(0);
            let zeros = vec![0u8; chunk_len];
            let reply = self.send_command(
                Command::ReadFlash,
                address.saturating_add(offset),
                &zeros,
                false,
            )?;
            out.extend_from_slice(&reply.payload[..chunk_len]);
        }
        Ok(out)
    }

    fn write_flash_raw(&mut self, mut address: u16, data: &[u8]) -> CoreResult<()> {
        for chunk in data.chunks(ATLANTIS_MAX_PAYLOAD) {
            self.send_command(Command::WriteFlash, address, chunk, false)?;
            address = address.saturating_add(u16::try_from(chunk.len()).unwrap_or(0));
        }
        Ok(())
    }

    fn read_flash_checked(&mut self, address: u16, data_len: usize) -> CoreResult<Vec<u8>> {
        let mut raw = self.read_flash_raw(address, data_len + 1)?;
        if !is_checksum_valid(&raw) {
            return Err(CoreError::Protocol(format!(
                "Lamzu flash field checksum mismatch at address 0x{address:04X}"
            )));
        }
        raw.pop();
        Ok(raw)
    }

    fn write_flash_checked(&mut self, address: u16, values: &[u8]) -> CoreResult<()> {
        let sealed = seal_field(values);
        self.write_flash_raw(address, &sealed)
    }

    fn read_flash_u8(&mut self, address: u16) -> CoreResult<u8> {
        Ok(self.read_flash_checked(address, 1)?[0])
    }

    fn write_flash_u8(&mut self, address: u16, value: u8) -> CoreResult<()> {
        self.write_flash_checked(address, &[value])
    }
}

impl<T: HidTransport> Peripheral for LamzuAtlantisDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let reply = self.send_command(Command::ReadFirmwareVersion, 0, &[], true)?;
        self.info.firmware_version = Some(decode_firmware(&reply.payload));
        Ok(())
    }

    fn as_pointer(&self) -> Option<&dyn PointerDevice> {
        Some(self)
    }

    fn as_pointer_mut(&mut self) -> Option<&mut dyn PointerDevice> {
        Some(self)
    }

    fn as_battery(&self) -> Option<&dyn BatteryMonitor> {
        Some(self)
    }

    fn as_battery_mut(&mut self) -> Option<&mut dyn BatteryMonitor> {
        Some(self)
    }

    fn as_profiles(&self) -> Option<&dyn ProfileManager> {
        Some(self)
    }

    fn as_profiles_mut(&mut self) -> Option<&mut dyn ProfileManager> {
        Some(self)
    }

    fn as_macros(&self) -> Option<&dyn MacroController> {
        if self.capabilities.has_macros() {
            Some(self)
        } else {
            None
        }
    }

    fn as_macros_mut(&mut self) -> Option<&mut dyn MacroController> {
        if self.capabilities.has_macros() {
            Some(self)
        } else {
            None
        }
    }
}

/// A macro record as stored on the device, with erased header bytes read as empty.
struct StoredMacro {
    address: u16,
    name: Vec<u8>,
    /// Count byte followed by the contexts, without the checksum.
    contexts: Vec<u8>,
}

impl StoredMacro {
    fn count_address(&self) -> u16 {
        self.address + crystall::layout::CONTEXT_COUNT_OFFSET
    }

    fn name_text(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

impl<T: HidTransport> LamzuAtlantisDevice<T> {
    fn read_stored_macro(&mut self, index: u8) -> CoreResult<StoredMacro> {
        let address = crystall::macro_slot_address(index)?;
        let name_len = crystall::header_len(
            self.read_flash_raw(address, 1)?[0],
            crystall::layout::NAME_MAX,
        );
        let name = if name_len == 0 {
            Vec::new()
        } else {
            self.read_flash_raw(address + crystall::layout::NAME_OFFSET, name_len)?
        };
        let count_address = address + crystall::layout::CONTEXT_COUNT_OFFSET;
        let count = crystall::header_len(
            self.read_flash_raw(count_address, 1)?[0],
            crystall::layout::CONTEXT_MAX,
        );
        let contexts = if count == 0 {
            vec![0u8]
        } else {
            self.read_flash_checked(count_address, 1 + count * crystall::layout::CONTEXT_LEN)?
        };
        Ok(StoredMacro {
            address,
            name,
            contexts,
        })
    }

    fn read_key_binding(&mut self, index: u8) -> CoreResult<[u8; 3]> {
        let raw = self.read_flash_checked(crystall::key_slot_address(index)?, 3)?;
        Ok([raw[0], raw[1], raw[2]])
    }
}

impl<T: HidTransport> MacroController for LamzuAtlantisDevice<T> {
    fn macro_capabilities(&self) -> MacroCapabilities {
        MacroCapabilities::new(
            crystall::layout::MACRO_SLOT_COUNT,
            u16::from(crystall::layout::MACRO_SLOT_COUNT) * crystall::layout::MACRO_SLOT_STRIDE,
        )
        .with_repeat_support(true)
        .with_mouse_buttons(true)
        .with_media_keys(true)
        .with_pointer_move(true)
    }

    fn read_macros(&mut self) -> CoreResult<Vec<MacroSlot>> {
        let mut slots = Vec::with_capacity(usize::from(crystall::layout::MACRO_SLOT_COUNT));
        for index in 0..crystall::layout::MACRO_SLOT_COUNT {
            let binding = self.read_key_binding(index)?;
            let repeat = crystall::binding_repeat(index, &binding).unwrap_or(1);
            // Macro records exist independently of their trigger, so every slot is read.
            let stored = self.read_stored_macro(index)?;
            let mut name_block = vec![u8::try_from(stored.name.len()).unwrap_or(0)];
            name_block.extend_from_slice(&stored.name);
            match crystall::decode_slot(index, &name_block, &stored.contexts, repeat) {
                Ok(slot) => slots.push(slot),
                // Keep the slot visible by name; an event this driver cannot re-encode
                // (mouse buttons, media keys) must not hide every other macro.
                Err(CoreError::UnsupportedCapability(_)) => slots.push(
                    MacroSlot::new(index, stored.name_text(), Vec::new())
                        .with_repeat(repeat)
                        .with_readonly(true),
                ),
                Err(err) => return Err(err),
            }
        }
        Ok(slots)
    }

    fn write_macros(&mut self, macros: &[MacroSlot]) -> CoreResult<()> {
        // Every slot is checked before the first write, so a refused slot never leaves
        // the slots before it saved and the ones after it unsaved.
        let mut planned = Vec::with_capacity(macros.len());
        for slot in macros {
            let stored = self.read_stored_macro(slot.index)?;
            if crystall::has_unsupported_context(&stored.contexts) {
                let is_unchanged = slot.steps.is_empty() && slot.name == stored.name_text();
                if is_unchanged {
                    continue;
                }
                return Err(CoreError::UnsupportedCapability(
                    "lamzu_macro_advanced_events",
                ));
            }
            let contexts = if slot.steps.is_empty() {
                vec![0u8]
            } else {
                crystall::encode_contexts(&slot.steps)?
            };
            planned.push((slot, stored, contexts));
        }

        for (slot, stored, contexts) in planned {
            let index = slot.index;
            let count_address = stored.count_address();
            if slot.steps.is_empty() {
                if stored.contexts != contexts {
                    self.write_flash_checked(count_address, &contexts)?;
                }
                if !stored.name.is_empty() {
                    self.write_flash_raw(stored.address, &[0])?;
                }
                continue;
            }

            if contexts != stored.contexts {
                self.write_flash_checked(count_address, &contexts)?;
            }
            let name = crystall::encode_name(&slot.name);
            if name[1..] != stored.name[..] {
                self.write_flash_raw(stored.address, &name)?;
            }

            let binding = crystall::macro_binding(index, slot.repeat);
            let current_binding = self.read_key_binding(index)?;
            // Never take over a button that already does something else (left click,
            // back, DPI, ...). Only refresh an existing macro binding or claim a slot
            // whose key function is disabled.
            let is_macro_trigger = crystall::binding_repeat(index, &current_binding).is_some();
            let is_disabled = current_binding == [0, 0, 0];
            if current_binding != binding && (is_macro_trigger || is_disabled) {
                self.write_flash_checked(crystall::key_slot_address(index)?, &binding)?;
            }

            if !self.transport.is_dry_run() {
                let readback = self.read_flash_checked(count_address, contexts.len())?;
                if readback != contexts {
                    return Err(CoreError::Protocol(format!(
                        "Lamzu macro slot {index} read-back mismatch"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl<T: HidTransport> BatteryMonitor for LamzuAtlantisDevice<T> {
    fn read_battery(&mut self) -> CoreResult<BatteryStatus> {
        let reply = self.send_command(Command::ReadBattery, 0, &[], true)?;
        Ok(decode_battery(&reply.payload))
    }
}

impl<T: HidTransport> PointerDevice for LamzuAtlantisDevice<T> {
    fn pointer_capabilities(&self) -> PointerCapabilities {
        self.pointer_caps.clone()
    }

    fn read_pointer_state(&mut self) -> CoreResult<PointerSnapshot> {
        let raw_rate = self.read_flash_u8(flash::POLL_RATE)?;
        let polling_rate = decode_polling_rate(raw_rate, self.profile.rate_family)?;

        let stage_count = self.read_flash_u8(flash::DPI_STAGE_COUNT)?;
        if !(1..=flash::MAX_DPI_STAGES).contains(&stage_count) {
            return Err(CoreError::Protocol(format!(
                "invalid Atlantis DPI stage count {stage_count}"
            )));
        }
        let active_stage = self.read_flash_u8(flash::ACTIVE_DPI_STAGE)?;

        let mut stages = Vec::with_capacity(usize::from(stage_count));
        for ix in 0..stage_count {
            let offset = u16::from(ix) * flash::DPI_STAGE_STRIDE;
            let raw_dpi = self.read_flash_raw(flash::DPI_STAGES + offset, 4)?;
            let mut stage = decode_dpi_stage(ix, &raw_dpi)?;
            if let Ok(raw_color) = self.read_flash_raw(flash::DPI_COLORS + offset, 4)
                && let Ok(color) = decode_dpi_color(&raw_color)
            {
                stage = stage.with_indicator_color(color);
            }
            stages.push(stage);
        }

        let lod = decode_lod(self.read_flash_u8(flash::LIFT_OFF_DISTANCE)?)?;
        let debounce = DebounceMs::new(self.read_flash_u8(flash::DEBOUNCE_MS)?);
        let motion_sync = self.read_flash_u8(flash::MOTION_SYNC)? != 0;
        let angle_snapping = self.read_flash_u8(flash::ANGLE_SNAPPING)? != 0;
        let ripple_control = self.read_flash_u8(flash::RIPPLE_CONTROL)? != 0;

        Ok(PointerSnapshot::new(active_stage, stages, polling_rate)
            .with_lod(lod)
            .with_debounce_ms(debounce)
            .with_motion_sync(motion_sync)
            .with_angle_snapping(angle_snapping)
            .with_ripple_control(ripple_control))
    }

    fn set_dpi_stages(&mut self, stages: &[DpiStage], active_ix: u8) -> CoreResult<()> {
        self.pointer_caps.validate_dpi_stages(stages, active_ix)?;
        let count = u8::try_from(stages.len()).unwrap_or(1);
        self.write_flash_u8(flash::DPI_STAGE_COUNT, count)?;
        for (ix, stage) in stages.iter().enumerate() {
            let offset = u16::try_from(ix).unwrap_or(0) * flash::DPI_STAGE_STRIDE;
            let encoded = encode_dpi_stage(stage)?;
            self.write_flash_raw(flash::DPI_STAGES + offset, &encoded)?;
            if let Some(color) = stage.indicator_color {
                self.write_flash_checked(flash::DPI_COLORS + offset, &[color.r, color.g, color.b])?;
            }
        }
        self.write_flash_u8(flash::ACTIVE_DPI_STAGE, active_ix)
    }

    fn set_active_dpi_stage(&mut self, stage_ix: u8) -> CoreResult<()> {
        if stage_ix >= flash::MAX_DPI_STAGES {
            return Err(CoreError::OutOfRange {
                field: "active_dpi_stage",
                actual: stage_ix.to_string(),
                expected: format!("0..{}", flash::MAX_DPI_STAGES),
            });
        }
        self.write_flash_u8(flash::ACTIVE_DPI_STAGE, stage_ix)
    }

    fn set_polling_rate(&mut self, rate: PollingRateHz) -> CoreResult<()> {
        self.pointer_caps.validate_polling_rate(rate)?;
        let raw = encode_polling_rate(rate, self.profile.rate_family)?;
        self.write_flash_u8(flash::POLL_RATE, raw)
    }

    fn set_lod(&mut self, lod: LiftOffDistance) -> CoreResult<()> {
        self.pointer_caps.validate_lod(lod)?;
        let raw = encode_lod(lod)?;
        self.write_flash_u8(flash::LIFT_OFF_DISTANCE, raw)
    }

    fn set_debounce_ms(&mut self, debounce: DebounceMs) -> CoreResult<()> {
        self.pointer_caps.validate_debounce(debounce)?;
        self.write_flash_u8(flash::DEBOUNCE_MS, debounce.value())
    }

    fn set_motion_sync(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_flash_u8(flash::MOTION_SYNC, u8::from(enabled))
    }

    fn set_angle_snapping(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_flash_u8(flash::ANGLE_SNAPPING, u8::from(enabled))
    }

    fn set_ripple_control(&mut self, enabled: bool) -> CoreResult<()> {
        self.write_flash_u8(flash::RIPPLE_CONTROL, u8::from(enabled))
    }
}

impl<T: HidTransport> ProfileManager for LamzuAtlantisDevice<T> {
    fn profile_capabilities(&self) -> ProfileCapabilities {
        self.profile_caps.clone()
    }

    fn read_profile_state(&mut self) -> CoreResult<ProfileSnapshot> {
        let reply = self.send_command(Command::ReadActiveProfile, 0, &[], true)?;
        let active = ProfileSlot::new(reply.payload[0]);
        self.profile_caps.validate_slot(active)?;
        let profiles = (0..self.profile_caps.slot_count)
            .map(|ix| ProfileInfo::new(ProfileSlot::new(ix), true))
            .collect();
        Ok(ProfileSnapshot::new(
            active,
            ProfileMode::OnboardMemory,
            profiles,
        ))
    }

    fn set_active_profile(&mut self, slot: ProfileSlot) -> CoreResult<()> {
        self.profile_caps.validate_slot(slot)?;
        self.send_command(Command::WriteActiveProfile, 0, &[slot.index()], false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::{ChargingState, DpiValue, MacroActionKind, MacroStep, RgbColor};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome,
    };

    fn reply_frame(cmd: Command, addr: u16, declared_len: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; ATLANTIS_PACKET_LEN];
        frame[0] = crate::atlantis::ATLANTIS_REPORT_ID;
        frame[1] = cmd as u8;
        frame[2] = 0x00;
        let [hi, lo] = addr.to_be_bytes();
        frame[3] = hi;
        frame[4] = lo;
        frame[5] = declared_len;
        frame[6..6 + payload.len()].copy_from_slice(payload);
        frame[16] = crate::atlantis::compute_checksum(&frame[..16]);
        frame
    }

    fn atlantis_4k_descriptor() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            LAMZU_ATLANTIS_VID,
            0xF510,
            LAMZU_ATLANTIS_USAGE_PAGE,
            LAMZU_ATLANTIS_USAGE_ID,
            1,
            "mock://lamzu/atlantis-4k",
        )
    }

    #[test]
    fn checksum_worked_example_and_dpi_stage_roundtrip() {
        // Worked example from docs/protocols/lamzu-compx.md
        let batt_req = encode_request(Command::ReadBattery, 0, &[]).unwrap();
        assert_eq!(batt_req[0], 0x08);
        assert_eq!(batt_req[1], 0x04);
        assert_eq!(batt_req[16], 0x49);
        assert!(is_checksum_valid(&batt_req));

        // 1600 DPI X / 3200 DPI Y round-trip across 10-bit flags byte
        let stage =
            DpiStage::new(0, DpiValue::new(1600).unwrap()).with_y(DpiValue::new(3200).unwrap());
        let encoded = encode_dpi_stage(&stage).unwrap();
        assert!(is_checksum_valid(&encoded));
        let decoded = decode_dpi_stage(0, &encoded).unwrap();
        assert_eq!(decoded.x.value(), 1600);
        assert_eq!(decoded.effective_y().value(), 3200);

        // High DPI > 12800 uses upper 2 bits in flags byte (e.g., 26000 DPI => raw 519 = 0x207)
        let high = DpiStage::new(1, DpiValue::new(26_000).unwrap());
        let high_enc = encode_dpi_stage(&high).unwrap();
        let high_dec = decode_dpi_stage(1, &high_enc).unwrap();
        assert_eq!(high_dec.x.value(), 26_000);
    }

    #[test]
    fn reads_full_atlantis_pointer_battery_and_profile_state() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        // Open firmware version -> v1.24
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x01, 0x24],
        ));
        // Battery -> 95%, charging, 4239 mV (0x108F)
        mock.push_input_response(reply_frame(
            Command::ReadBattery,
            0,
            2,
            &[95, 1, 0x10, 0x8F],
        ));
        // Polling rate -> 0x20 (4000 Hz on Pulsar4k receiver)
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::POLL_RATE,
            2,
            &seal_field(&[0x20]),
        ));
        // Stage count -> 1, Active stage -> 0
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DPI_STAGE_COUNT,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::ACTIVE_DPI_STAGE,
            2,
            &seal_field(&[0]),
        ));
        // Stage 0 -> 800 DPI + cyan indicator LED
        let stage0 = encode_dpi_stage(&DpiStage::new(0, DpiValue::new(800).unwrap())).unwrap();
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DPI_STAGES,
            4,
            &stage0,
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DPI_COLORS,
            4,
            &seal_field(&[0x00, 0xFF, 0xCC]),
        ));
        // LOD 1mm, Debounce 2ms, MotionSync on, AngleSnapping off, Ripple off
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::LIFT_OFF_DISTANCE,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::DEBOUNCE_MS,
            2,
            &seal_field(&[2]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::MOTION_SYNC,
            2,
            &seal_field(&[1]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::ANGLE_SNAPPING,
            2,
            &seal_field(&[0]),
        ));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            flash::RIPPLE_CONTROL,
            2,
            &seal_field(&[0]),
        ));
        // Active profile -> slot 2
        mock.push_input_response(reply_frame(Command::ReadActiveProfile, 0, 1, &[2]));

        let mut dev = LamzuAtlantisDevice::open(mock, false, None).unwrap();
        assert_eq!(dev.info().firmware_version.as_deref(), Some("v1.24"));

        let batt = dev.read_battery().unwrap();
        assert_eq!(batt.percentage, Some(95));
        assert_eq!(batt.voltage_mv, Some(4239));
        assert_eq!(batt.state, ChargingState::Charging);

        let snap = dev.read_pointer_state().unwrap();
        assert_eq!(snap.polling_rate, PollingRateHz::Hz4000);
        assert_eq!(snap.dpi_stages.len(), 1);
        assert_eq!(snap.dpi_stages[0].x.value(), 800);
        assert_eq!(
            snap.dpi_stages[0].indicator_color,
            Some(RgbColor::new(0x00, 0xFF, 0xCC))
        );
        assert_eq!(snap.lod, Some(LiftOffDistance::TenthsMm(10)));
        assert_eq!(snap.debounce_ms, Some(DebounceMs::new(2)));
        assert_eq!(snap.motion_sync, Some(true));

        let prof = dev.read_profile_state().unwrap();
        assert_eq!(prof.active_slot, ProfileSlot::new(2));
    }

    #[test]
    fn dry_run_validates_and_suppresses_flash_writes() {
        let mock = MockHidTransport::new(atlantis_4k_descriptor());
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.set_polling_rate(PollingRateHz::Hz2000).unwrap();
        dev.set_lod(LiftOffDistance::TenthsMm(20)).unwrap();
        dev.set_debounce_ms(DebounceMs::new(4)).unwrap();
        dev.set_motion_sync(true).unwrap();
        dev.set_active_profile(ProfileSlot::new(1)).unwrap();

        // Only the read-only firmware query during open() reached the mock transport;
        // zero mutating flash/profile writes were transmitted.
        assert_eq!(dev.transport().inner().recorded_writes().len(), 1);
        assert_eq!(
            dev.transport().inner().recorded_writes()[0].bytes[1],
            Command::ReadFirmwareVersion as u8
        );

        // Every write was validated and logged as DryRunSuppressed.
        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        assert!(suppressed >= 5);
    }

    #[test]
    fn reads_unbound_macro_slots_as_empty_entries() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        for _ in 0..crystall::layout::MACRO_SLOT_COUNT {
            mock.push_input_response(reply_frame(
                Command::ReadFlash,
                0,
                4,
                &[0x01, 0x01, 0x00, 0x53],
            ));
            mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0xFF]));
            mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0xFF]));
        }
        let mut dev = LamzuAtlantisDevice::open(mock, false, None).unwrap();

        let slots = dev.read_macros().unwrap();
        assert_eq!(slots.len(), 16);
        assert!(slots.iter().all(|slot| slot.steps.is_empty()));
        assert!(slots.iter().all(|slot| slot.repeat == 1));
        assert!(dev.capabilities().has_macros());
    }

    #[test]
    fn reads_the_captured_crystal_wire_slot_without_hiding_other_slots() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        for index in 0..crystall::layout::MACRO_SLOT_COUNT {
            let (binding, checksum) = if index == 5 {
                // Stock DPI binding, not a macro trigger: the record below is unbound.
                ([0x02, 0x01, 0x00], 0x52)
            } else {
                ([0x00, 0x00, 0x00], 0x55)
            };
            mock.push_input_response(reply_frame(
                Command::ReadFlash,
                0,
                4,
                &[binding[0], binding[1], binding[2], checksum],
            ));
            if index == 5 {
                mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x0A]));
                mock.push_input_response(reply_frame(Command::ReadFlash, 0, 10, b"spam click"));
                mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x02]));
                mock.push_input_response(reply_frame(
                    Command::ReadFlash,
                    0,
                    10,
                    &[0x02, 0x84, 0x01, 0x00, 0x00, 0x0A, 0x44, 0x01, 0x00, 0x00],
                ));
                mock.push_input_response(reply_frame(Command::ReadFlash, 0, 2, &[0x0A, 0x75]));
            } else {
                mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0xFF]));
                mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0xFF]));
            }
        }
        let mut dev = LamzuAtlantisDevice::open(mock, false, None).unwrap();

        let slots = dev.read_macros().unwrap();
        assert_eq!(slots[5].name, "spam click");
        assert!(
            !slots[5].is_readonly,
            "captured mouse-button macro loads as editable steps"
        );
        assert_eq!(
            slots[5].steps,
            vec![
                MacroStep::mouse(
                    MacroActionKind::MouseDown,
                    agnoperiph_core::MouseButton::Left,
                    10
                ),
                MacroStep::mouse(
                    MacroActionKind::MouseUp,
                    agnoperiph_core::MouseButton::Left,
                    10
                ),
            ]
        );
        assert_eq!(slots.iter().filter(|slot| slot.is_readonly).count(), 0);
        assert!(
            slots
                .iter()
                .enumerate()
                .all(|(ix, slot)| ix == 5 || slot.steps.is_empty())
        );
    }

    #[test]
    fn unchanged_unsupported_macro_is_left_untouched_on_save() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        push_unsupported_power_record(&mut mock);
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.write_macros(&[MacroSlot::new(5, "power key", Vec::new())])
            .unwrap();

        assert!(
            sink.snapshot()
                .iter()
                .all(|event| event.bytes[1] != Command::WriteFlash as u8),
            "an unsupported ACPI power macro must survive a save that did not edit it"
        );
    }

    #[test]
    fn erased_slots_are_not_mistaken_for_full_macros() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0xFF]));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0xFF]));
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.write_macros(&[MacroSlot::new(0, "", Vec::new())])
            .unwrap();

        assert!(
            sink.snapshot()
                .iter()
                .all(|event| event.bytes[1] != Command::WriteFlash as u8),
            "erased 0xFF header bytes must not read as 70 contexts"
        );
    }

    #[test]
    fn macro_capability_requires_the_v2_profile_image() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x01, 0x24],
        ));
        let dev = LamzuAtlantisDevice::open(mock, false, None).unwrap();
        assert!(!dev.capabilities().has_macros());
        assert!(dev.as_macros().is_none());

        for (payload, expected) in [
            ([0x02, 0x00], true),
            ([0x02, 0x17], true),
            ([0x03, 0x01], true),
            ([0x01, 0x24], false),
            ([0x00, 0xFF], false),
        ] {
            let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
            mock.push_input_response(reply_frame(Command::ReadFirmwareVersion, 0, 2, &payload));
            let dev = LamzuAtlantisDevice::open(mock, false, None).unwrap();
            assert_eq!(dev.capabilities().has_macros(), expected, "{payload:?}");
        }
    }

    /// Read replies for an unsupported ACPI Power (`type = 3`) record in slot 5.
    fn push_unsupported_power_record(mock: &mut MockHidTransport) {
        let contexts = [0x01, 0x83, 0x81, 0x00, 0x00, 0x0A];
        let sealed = seal_field(&contexts);
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x09]));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 9, b"power key"));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x01]));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            0,
            u8::try_from(sealed.len()).unwrap_or(0),
            &sealed,
        ));
    }

    fn flash_writes(sink: &MemoryPacketSink) -> Vec<FlashWrite> {
        sink.snapshot()
            .into_iter()
            .filter(|event| event.bytes[1] == Command::WriteFlash as u8)
            .map(|event| FlashWrite {
                address: u16::from_be_bytes([event.bytes[3], event.bytes[4]]),
                len: event.bytes[5],
                payload: event.bytes[6..].to_vec(),
            })
            .collect()
    }

    fn burst_steps() -> Vec<MacroStep> {
        vec![
            MacroStep::new(MacroActionKind::KeyDown, 0x04, 10),
            MacroStep::new(MacroActionKind::KeyUp, 0x04, 10),
        ]
    }

    #[test]
    fn editing_another_slot_never_writes_an_unsupported_macro() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        // Slot 0 is empty, slot 5 holds an unsupported ACPI Power event; trigger read follows plan.
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x00]));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x00]));
        push_unsupported_power_record(&mut mock);
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            0,
            4,
            &[0x01, 0x01, 0x00, 0x53],
        ));
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.write_macros(&[
            MacroSlot::new(0, "burst", burst_steps()),
            MacroSlot::new(5, "power key", Vec::new()).with_readonly(true),
        ])
        .unwrap();

        let slot5 = crystall::macro_slot_address(5).unwrap();
        let slot5_end = slot5 + crystall::layout::MACRO_SLOT_STRIDE;
        let key5 = crystall::key_slot_address(5).unwrap();
        let writes = flash_writes(&sink);
        assert!(!writes.is_empty(), "slot 0 is saved");
        assert!(
            writes
                .iter()
                .all(|w| !(slot5..slot5_end).contains(&w.address) && w.address != key5),
            "slot 5 record and trigger stay untouched"
        );
    }

    #[test]
    fn editing_an_unsupported_macro_is_refused_before_any_write() {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x00]));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x00]));
        push_unsupported_power_record(&mut mock);
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        let err = dev
            .write_macros(&[
                MacroSlot::new(0, "burst", burst_steps()),
                MacroSlot::new(5, "power key", burst_steps()),
            ])
            .unwrap_err();

        assert!(matches!(err, CoreError::UnsupportedCapability(_)));
        assert!(
            flash_writes(&sink).is_empty(),
            "slot 0 must not be saved when slot 5 is refused"
        );
    }

    struct FlashWrite {
        address: u16,
        len: u8,
        payload: Vec<u8>,
    }

    fn macro_write_frames(binding: [u8; 3], checksum: u8) -> Vec<FlashWrite> {
        let mut mock = MockHidTransport::new(atlantis_4k_descriptor());
        mock.push_input_response(reply_frame(
            Command::ReadFirmwareVersion,
            0,
            2,
            &[0x02, 0x17],
        ));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x00]));
        mock.push_input_response(reply_frame(Command::ReadFlash, 0, 1, &[0x00]));
        mock.push_input_response(reply_frame(
            Command::ReadFlash,
            0,
            4,
            &[binding[0], binding[1], binding[2], checksum],
        ));
        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = LamzuAtlantisDevice::open(mock, true, Some(sink.clone())).unwrap();

        dev.write_macros(&[MacroSlot::new(0, "burst", burst_steps()).with_repeat(3)])
            .unwrap();

        sink.snapshot()
            .into_iter()
            .filter(|event| event.outcome == PacketOutcome::DryRunSuppressed)
            .filter(|event| event.bytes[1] == Command::WriteFlash as u8)
            .map(|event| FlashWrite {
                address: u16::from_be_bytes([event.bytes[3], event.bytes[4]]),
                len: event.bytes[5],
                payload: event.bytes[6..].to_vec(),
            })
            .collect()
    }

    #[test]
    fn macro_writes_content_without_taking_over_a_normal_button() {
        // Stock left click; saving a macro must not rebind it.
        let writes = macro_write_frames([0x01, 0x01, 0x00], 0x53);
        let addresses: Vec<_> = writes.iter().map(|w| (w.address, w.len)).collect();
        assert_eq!(addresses, vec![(799, 10), (809, 2), (768, 6)]);
        assert_eq!(&writes[2].payload[..6], b"\x05burst");
    }

    #[test]
    fn macro_writes_refresh_an_existing_macro_binding() {
        // Already bound to macro 0 with repeat 2 (0x06 + 0x00 + 0x02 sums below 0x55).
        let writes = macro_write_frames([0x06, 0x00, 0x02], 0x4D);
        let addresses: Vec<_> = writes.iter().map(|w| (w.address, w.len)).collect();
        assert_eq!(addresses, vec![(799, 10), (809, 2), (768, 6), (96, 4)]);
        assert_eq!(&writes[3].payload[..3], &[0x06, 0x00, 0x03]);
    }

    #[test]
    fn aurora_codec_encodes_and_decodes_cleanly() {
        let req = aurora::encode_page_request(0x01, 0x04, 0x02, 0x08, &[0xAA, 0xBB]);
        assert_eq!(req.len(), 64);
        assert_eq!(&req[..8], &[0x00, 0x00, 0x01, 0x04, 0x02, 0x08, 0xAA, 0xBB]);

        let dpi_payload = [0x00, 0x02, 0x03, 0x20, 0x03, 0x20, 0x06, 0x40, 0x06, 0x40];
        let stages = aurora::decode_dpi_stages(&dpi_payload).unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].x.value(), 800);
        assert_eq!(stages[1].x.value(), 1600);
        assert_eq!(
            aurora::decode_lift_off(0x87),
            Some(LiftOffDistance::TenthsMm(7))
        );
        assert_eq!(
            aurora::decode_firmware(&[0, 0, 2, 14]).as_deref(),
            Some("v2.14")
        );
    }
}
