//! QMK / VIA Raw HID keyboard driver supporting Keychron, Lemokey, NuPhy, MonsGeek, Drop & QMK boards.

#![forbid(unsafe_code)]

pub mod protocol;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agnoperiph_core::{
    Brightness, Capabilities, ConnectionKind, CoreError, CoreResult, DeviceId, DeviceInfo,
    DeviceMatchRule, DriverDescriptor, KeyCode, KeyIndex, KeymapCapabilities, KeymapController,
    KeymapSnapshot, MacroCapabilities, MacroController, MacroSlot, Peripheral, RgbCapabilities,
    RgbController, RgbEffect, RgbEffectKind, RgbSnapshot, RgbZoneId, RgbZoneInfo, RgbZoneState,
    VendorKind,
};
use agnoperiph_transport::{GuardedTransport, HidTransport, PacketSink, TransportError};

use crate::protocol::{
    EXCLUDED_KEYCHRON_MOUSE_PIDS, VIA_MAX_CHUNK, VIA_PAYLOAD_LEN, VIA_TX_LEN, VIA_USAGE_ID,
    VIA_USAGE_PAGE, VIA_VENDORS, channel, cmd, decode_rgb_effect, decode_via_macros,
    encode_rgb_effect, encode_via_macros, qmk_hs_to_rgb, req_custom_get, req_custom_save,
    req_custom_set, req_firmware_version, req_keymap_get_buffer, req_keymap_reset,
    req_keymap_set_keycode, req_layer_count, req_macro_buffer_size, req_macro_count,
    req_macro_get_buffer, req_macro_set_buffer, req_protocol_version, rgb_matrix_presets,
    rgb_to_qmk_hs, rgb_val, unwrap_rx, vendor_for_vid, via_validator, wrap_tx,
};

const READ_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_READ_ATTEMPTS: usize = 6;
const DEFAULT_ROWS: u8 = 6;
const DEFAULT_COLS: u8 = 16;

/// Declarative `DriverDescriptor` for QMK / VIA Raw HID keyboards (`0xFF60:0x0061`).
pub fn driver_descriptor() -> DriverDescriptor {
    let rules = VIA_VENDORS
        .iter()
        .map(|&(vid, _, hint)| {
            DeviceMatchRule::new(vid, ConnectionKind::WiredUsb, hint)
                .with_usage_page(VIA_USAGE_PAGE)
                .with_usage_id(VIA_USAGE_ID)
        })
        .collect();
    DriverDescriptor::new("qmk-via", VendorKind::Keychron, rules)
}

/// Active QMK / VIA keyboard backed by a validated [`GuardedTransport`].
pub struct ViaKeyboardDevice<T: HidTransport> {
    transport: GuardedTransport<T>,
    info: DeviceInfo,
    capabilities: Capabilities,
    protocol_version: u16,
    rows: u8,
    cols: u8,
    layer_count: u8,
    macro_caps: Option<MacroCapabilities>,
    rgb_channel: Option<u8>,
}

impl<T: HidTransport> fmt::Debug for ViaKeyboardDevice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViaKeyboardDevice")
            .field("info", &self.info)
            .field("protocol_version", &self.protocol_version)
            .field("layer_count", &self.layer_count)
            .field("rgb_channel", &self.rgb_channel)
            .finish_non_exhaustive()
    }
}

impl<T: HidTransport> ViaKeyboardDevice<T> {
    /// Connects to a QMK/VIA `0xFF60:0x0061` Raw HID interface and probes protocol version,
    /// layer count, macro storage, and QMK RGB Matrix / RGBLight channels.
    pub fn open(
        transport: T,
        dry_run: bool,
        sink: Option<Arc<dyn PacketSink>>,
    ) -> CoreResult<Self> {
        let desc = transport.descriptor().clone();
        if desc.vid == 0x3434 && EXCLUDED_KEYCHRON_MOUSE_PIDS.contains(&desc.pid) {
            return Err(CoreError::Validation(format!(
                "Keychron PID 0x{:04X} is a mouse/receiver, not a VIA keyboard",
                desc.pid
            )));
        }

        let mut guarded = GuardedTransport::new(transport)
            .with_validator(Arc::new(via_validator()))
            .with_dry_run(dry_run);
        if let Some(sink) = sink {
            guarded = guarded.with_sink(sink);
        }

        let (vendor, default_name) = vendor_for_vid(desc.vid);
        let model_name = desc
            .product
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default_name.to_owned());

        let stable_id = DeviceId::new(format!(
            "via:{:04X}:{:04X}:{}",
            desc.vid,
            desc.pid,
            desc.serial_number.as_deref().unwrap_or(&desc.path)
        ));

        let mut info = DeviceInfo::new(
            stable_id,
            vendor,
            model_name,
            ConnectionKind::WiredUsb,
            desc.vid,
            desc.pid,
        );
        if let Some(serial) = desc.serial_number {
            info = info.with_serial_number(serial);
        }

        let mut dev = Self {
            transport: guarded,
            info,
            capabilities: Capabilities::empty().with_keymap(true),
            protocol_version: 0,
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            layer_count: 4,
            macro_caps: None,
            rgb_channel: None,
        };

        // Mandatory read-only handshake: `id_get_protocol_version` (`0x01`).
        let ver_reply = dev.send_query(req_protocol_version())?;
        dev.protocol_version = u16::from_be_bytes([ver_reply[1], ver_reply[2]]);
        if dev.protocol_version == 0 {
            return Err(CoreError::Protocol(
                "device returned VIA protocol version 0".to_owned(),
            ));
        }

        let fw_str = dev
            .send_query(req_firmware_version())
            .ok()
            .map(|r| u32::from_be_bytes([r[2], r[3], r[4], r[5]]))
            .filter(|&v| v != 0)
            .map_or_else(
                || format!("VIA v{}", dev.protocol_version),
                |v| format!("VIA v{} (fw 0x{v:08X})", dev.protocol_version),
            );
        dev.info.firmware_version = Some(fw_str);

        if let Ok(lc) = dev.send_query(req_layer_count())
            && lc[1] > 0
        {
            dev.layer_count = lc[1].min(16);
        }

        if let (Ok(mc), Ok(ms)) = (
            dev.send_query(req_macro_count()),
            dev.send_query(req_macro_buffer_size()),
        ) {
            let count = mc[1];
            let buf_size = u16::from_be_bytes([ms[1], ms[2]]);
            if count > 0 && buf_size > 0 {
                dev.macro_caps = Some(MacroCapabilities::new(count, buf_size));
                dev.capabilities = dev.capabilities.with_macros(true);
            }
        }

        // Probe QMK RGB Matrix (channel 3), falling back to QMK RGBLight (channel 2).
        for chan in [channel::QMK_RGB_MATRIX, channel::QMK_RGBLIGHT] {
            if dev
                .send_query(req_custom_get(chan, rgb_val::EFFECT))
                .is_ok()
            {
                dev.rgb_channel = Some(chan);
                dev.capabilities = dev.capabilities.with_rgb(true);
                break;
            }
        }

        Ok(dev)
    }

    pub const fn transport(&self) -> &GuardedTransport<T> {
        &self.transport
    }

    pub const fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    fn send_query(&mut self, payload: [u8; VIA_PAYLOAD_LEN]) -> CoreResult<[u8; VIA_PAYLOAD_LEN]> {
        let tx = wrap_tx(payload);
        self.transport.send_query_output_report(&tx)?;
        self.read_matching_reply(payload[0])
    }

    fn send_write(&mut self, payload: [u8; VIA_PAYLOAD_LEN]) -> CoreResult<[u8; VIA_PAYLOAD_LEN]> {
        let tx = wrap_tx(payload);
        self.transport.send_output_report(&tx)?;
        if self.transport.is_dry_run() {
            return Ok(payload);
        }
        self.read_matching_reply(payload[0])
    }

    fn read_matching_reply(&mut self, expected_cmd: u8) -> CoreResult<[u8; VIA_PAYLOAD_LEN]> {
        let mut buf = [0u8; VIA_TX_LEN];
        for _ in 0..MAX_READ_ATTEMPTS {
            let n = match self.transport.read_input_report(&mut buf, READ_TIMEOUT) {
                Ok(n) => n,
                Err(TransportError::Timeout { .. }) => {
                    return Err(CoreError::NotConnected(format!(
                        "timed out waiting for VIA reply to 0x{expected_cmd:02X}"
                    )));
                }
                Err(err) => return Err(err.into()),
            };
            let Some(rx) = unwrap_rx(&buf[..n]) else {
                continue;
            };
            if rx[0] == cmd::UNHANDLED {
                return Err(CoreError::UnsupportedCapability("via_command_unhandled"));
            }
            if rx[0] == expected_cmd {
                return Ok(rx);
            }
        }
        Err(CoreError::Protocol(format!(
            "no matching VIA reply for command 0x{expected_cmd:02X}"
        )))
    }
}

impl<T: HidTransport> Peripheral for ViaKeyboardDevice<T> {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn refresh(&mut self) -> CoreResult<()> {
        let ver = self.send_query(req_protocol_version())?;
        self.protocol_version = u16::from_be_bytes([ver[1], ver[2]]);
        Ok(())
    }

    fn as_keymap(&self) -> Option<&dyn KeymapController> {
        Some(self)
    }

    fn as_keymap_mut(&mut self) -> Option<&mut dyn KeymapController> {
        Some(self)
    }

    fn as_macros(&self) -> Option<&dyn MacroController> {
        self.macro_caps.is_some().then_some(self)
    }

    fn as_macros_mut(&mut self) -> Option<&mut dyn MacroController> {
        self.macro_caps.is_some().then_some(self)
    }

    fn as_rgb(&self) -> Option<&dyn RgbController> {
        self.rgb_channel.is_some().then_some(self)
    }

    fn as_rgb_mut(&mut self) -> Option<&mut dyn RgbController> {
        self.rgb_channel.is_some().then_some(self)
    }
}

impl<T: HidTransport> KeymapController for ViaKeyboardDevice<T> {
    fn keymap_capabilities(&self) -> KeymapCapabilities {
        let names = (0..self.layer_count)
            .map(|ix| format!("Layer {ix}"))
            .collect();
        KeymapCapabilities::new(names, self.rows, self.cols, true)
    }

    fn read_keymap(&mut self) -> CoreResult<KeymapSnapshot> {
        let keys_per_layer = usize::from(self.rows) * usize::from(self.cols);
        let total_bytes = usize::from(self.layer_count) * keys_per_layer * 2;
        let mut raw = Vec::with_capacity(total_bytes);
        let mut offset = 0usize;
        while offset < total_bytes {
            let chunk_len = (total_bytes - offset).min(VIA_MAX_CHUNK);
            let off_u16 = u16::try_from(offset).unwrap_or(u16::MAX);
            let sz_u8 = u8::try_from(chunk_len).unwrap_or(28);
            let reply = self.send_query(req_keymap_get_buffer(off_u16, sz_u8))?;
            raw.extend_from_slice(&reply[4..4 + chunk_len]);
            offset += chunk_len;
        }

        let mut layers = Vec::with_capacity(usize::from(self.layer_count));
        for layer_ix in 0..usize::from(self.layer_count) {
            let mut layer = Vec::with_capacity(keys_per_layer);
            for key_ix in 0..keys_per_layer {
                let pos = (layer_ix * keys_per_layer + key_ix) * 2;
                let class = raw.get(pos).copied().unwrap_or(0);
                let id = raw.get(pos + 1).copied().unwrap_or(0);
                layer.push(KeyCode::new(class, id));
            }
            layers.push(layer);
        }
        Ok(KeymapSnapshot::new(layers, 0))
    }

    fn set_key_binding(&mut self, layer: u8, key: KeyIndex, code: KeyCode) -> CoreResult<()> {
        if layer >= self.layer_count {
            return Err(CoreError::OutOfRange {
                field: "keymap_layer",
                actual: layer.to_string(),
                expected: format!("0..{}", self.layer_count),
            });
        }
        let max_keys = u16::from(self.rows) * u16::from(self.cols);
        if key.value() >= max_keys {
            return Err(CoreError::OutOfRange {
                field: "key_index",
                actual: key.value().to_string(),
                expected: format!("0..{max_keys}"),
            });
        }
        let row = u8::try_from(key.value() / u16::from(self.cols)).unwrap_or(0);
        let col = u8::try_from(key.value() % u16::from(self.cols)).unwrap_or(0);
        self.send_write(req_keymap_set_keycode(layer, row, col, code))?;
        Ok(())
    }

    fn reset_keymap(&mut self) -> CoreResult<()> {
        self.send_write(req_keymap_reset())?;
        Ok(())
    }
}

impl<T: HidTransport> MacroController for ViaKeyboardDevice<T> {
    fn macro_capabilities(&self) -> MacroCapabilities {
        self.macro_caps
            .unwrap_or_else(|| MacroCapabilities::new(0, 0))
    }

    fn read_macros(&mut self) -> CoreResult<Vec<MacroSlot>> {
        let Some(caps) = self.macro_caps else {
            return Err(CoreError::UnsupportedCapability("macros"));
        };
        let total = usize::from(caps.max_buffer_bytes);
        let mut raw = Vec::with_capacity(total);
        let mut offset = 0usize;
        while offset < total {
            let chunk_len = (total - offset).min(VIA_MAX_CHUNK);
            let off_u16 = u16::try_from(offset).unwrap_or(u16::MAX);
            let sz_u8 = u8::try_from(chunk_len).unwrap_or(28);
            let reply = self.send_query(req_macro_get_buffer(off_u16, sz_u8))?;
            raw.extend_from_slice(&reply[4..4 + chunk_len]);
            offset += chunk_len;
        }
        Ok(decode_via_macros(&raw, caps.max_slots))
    }

    fn write_macros(&mut self, macros: &[MacroSlot]) -> CoreResult<()> {
        let Some(caps) = self.macro_caps else {
            return Err(CoreError::UnsupportedCapability("macros"));
        };
        if macros.len() > usize::from(caps.max_slots) {
            return Err(CoreError::OutOfRange {
                field: "macros",
                actual: macros.len().to_string(),
                expected: format!("0..={}", caps.max_slots),
            });
        }
        let encoded = encode_via_macros(macros, usize::from(caps.max_buffer_bytes));
        for (ix, chunk) in encoded.chunks(VIA_MAX_CHUNK).enumerate() {
            let offset = u16::try_from(ix * VIA_MAX_CHUNK).unwrap_or(u16::MAX);
            self.send_write(req_macro_set_buffer(offset, chunk))?;
        }
        Ok(())
    }
}

impl<T: HidTransport> RgbController for ViaKeyboardDevice<T> {
    fn rgb_capabilities(&self) -> RgbCapabilities {
        let name = if self.rgb_channel == Some(channel::QMK_RGBLIGHT) {
            "Underglow"
        } else {
            "RGB Matrix"
        };
        let zone = RgbZoneInfo::new(RgbZoneId::new(0), name).with_presets(rgb_matrix_presets());
        RgbCapabilities::new(
            vec![zone],
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
        )
    }

    fn read_rgb_state(&mut self) -> CoreResult<RgbSnapshot> {
        let Some(chan) = self.rgb_channel else {
            return Err(CoreError::UnsupportedCapability("rgb"));
        };
        let bright_raw = self
            .send_query(req_custom_get(chan, rgb_val::BRIGHTNESS))?
            .get(3)
            .copied()
            .unwrap_or(255);
        let pct = u8::try_from((u16::from(bright_raw) * 100 + 127) / 255).unwrap_or(100);
        let brightness = Brightness::new(pct.min(100))?;

        let mode = self
            .send_query(req_custom_get(chan, rgb_val::EFFECT))?
            .get(3)
            .copied()
            .unwrap_or(0);
        let speed = self
            .send_query(req_custom_get(chan, rgb_val::EFFECT_SPEED))
            .ok()
            .and_then(|r| r.get(3).copied())
            .unwrap_or(128);
        let color = self
            .send_query(req_custom_get(chan, rgb_val::COLOR))
            .ok()
            .map_or(agnoperiph_core::RgbColor::WHITE, |r| {
                qmk_hs_to_rgb(r[3], r[4])
            });

        let effect = decode_rgb_effect(mode, color, speed);
        Ok(RgbSnapshot::new(vec![RgbZoneState::new(
            RgbZoneId::new(0),
            effect,
            brightness,
        )]))
    }

    fn set_zone_effect(
        &mut self,
        _zone: RgbZoneId,
        effect: RgbEffect,
        brightness: Brightness,
    ) -> CoreResult<()> {
        let Some(chan) = self.rgb_channel else {
            return Err(CoreError::UnsupportedCapability("rgb"));
        };
        let (mode, color, speed) = encode_rgb_effect(effect);
        self.send_write(req_custom_set(chan, rgb_val::EFFECT, &[mode]))?;
        let bright_raw =
            u8::try_from((u16::from(brightness.percent()) * 255 + 50) / 100).unwrap_or(255);
        self.send_write(req_custom_set(chan, rgb_val::BRIGHTNESS, &[bright_raw]))?;
        if let Some(speed) = speed {
            self.send_write(req_custom_set(chan, rgb_val::EFFECT_SPEED, &[speed]))?;
        }
        if let Some(color) = color {
            let (h, s) = rgb_to_qmk_hs(color);
            self.send_write(req_custom_set(chan, rgb_val::COLOR, &[h, s]))?;
        }
        self.send_write(req_custom_save(chan))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agnoperiph_core::{MacroActionKind, MacroStep, RgbColor};
    use agnoperiph_transport::{
        HidDeviceDescriptor, MemoryPacketSink, MockHidTransport, PacketOutcome, PacketValidator,
        ReportKind,
    };

    fn keychron_q1_desc() -> HidDeviceDescriptor {
        HidDeviceDescriptor::new(
            0x3434,
            0x0610,
            VIA_USAGE_PAGE,
            VIA_USAGE_ID,
            1,
            "mock://keychron/q1-pro",
        )
        .with_strings(
            Some("Keychron".to_owned()),
            Some("Keychron Q1 Pro".to_owned()),
            Some("KC-Q1-001".to_owned()),
        )
    }

    fn via_reply(bytes: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; VIA_PAYLOAD_LEN];
        p[..bytes.len()].copy_from_slice(bytes);
        p
    }

    #[test]
    fn validator_blocks_eeprom_reset_and_bootloader_jump() {
        let v = via_validator();
        let ok_tx = wrap_tx(req_protocol_version());
        assert!(v.validate_outgoing(ReportKind::Output, &ok_tx).is_ok());

        for bad_cmd in [cmd::FORBIDDEN_EEPROM_RESET, cmd::FORBIDDEN_BOOTLOADER_JUMP] {
            let mut bad = [0u8; VIA_PAYLOAD_LEN];
            bad[0] = bad_cmd;
            assert!(
                v.validate_outgoing(ReportKind::Output, &wrap_tx(bad))
                    .is_err()
            );
        }
    }

    #[test]
    fn reads_keychron_via_keymap_macros_and_rgb_and_suppresses_dry_run() {
        let mut mock = MockHidTransport::new(keychron_q1_desc());
        // Open: protocol v12, firmware 0x00010203, 2 layers, 2 macros (32 bytes), RGB Matrix present
        mock.push_input_response(via_reply(&[cmd::GET_PROTOCOL_VERSION, 0x00, 0x0C]));
        mock.push_input_response(via_reply(&[
            cmd::GET_KEYBOARD_VALUE,
            0x04,
            0x00,
            0x01,
            0x02,
            0x03,
        ]));
        mock.push_input_response(via_reply(&[cmd::DYNAMIC_KEYMAP_GET_LAYER_COUNT, 2]));
        mock.push_input_response(via_reply(&[cmd::MACRO_GET_COUNT, 2]));
        mock.push_input_response(via_reply(&[cmd::MACRO_GET_BUFFER_SIZE, 0x00, 0x10]));
        mock.push_input_response(via_reply(&[
            cmd::CUSTOM_GET_VALUE,
            channel::QMK_RGB_MATRIX,
            rgb_val::EFFECT,
            1,
        ]));

        // RGB read: brightness 204 (80%), effect 1 (Static), speed 128, hue 0 sat 255 (Red)
        mock.push_input_response(via_reply(&[
            cmd::CUSTOM_GET_VALUE,
            channel::QMK_RGB_MATRIX,
            rgb_val::BRIGHTNESS,
            204,
        ]));
        mock.push_input_response(via_reply(&[
            cmd::CUSTOM_GET_VALUE,
            channel::QMK_RGB_MATRIX,
            rgb_val::EFFECT,
            1,
        ]));
        mock.push_input_response(via_reply(&[
            cmd::CUSTOM_GET_VALUE,
            channel::QMK_RGB_MATRIX,
            rgb_val::EFFECT_SPEED,
            128,
        ]));
        mock.push_input_response(via_reply(&[
            cmd::CUSTOM_GET_VALUE,
            channel::QMK_RGB_MATRIX,
            rgb_val::COLOR,
            0,
            255,
        ]));

        // Macro buffer read (16 bytes): slot 0 = Tap A (0x04), slot 1 = empty
        mock.push_input_response(via_reply(&[
            cmd::MACRO_GET_BUFFER,
            0,
            0,
            16,
            0x01,
            0x01,
            0x04,
            0x00,
            0x00,
        ]));

        let sink = Arc::new(MemoryPacketSink::unbounded());
        let mut dev = ViaKeyboardDevice::open(mock, true, Some(sink.clone())).unwrap();
        assert_eq!(dev.info().vendor, VendorKind::Keychron);
        assert_eq!(dev.info().model_name, "Keychron Q1 Pro");
        assert_eq!(dev.protocol_version(), 12);
        assert!(dev.capabilities().has_keymap());
        assert!(dev.capabilities().has_macros());
        assert!(dev.capabilities().has_rgb());

        let rgb = dev.read_rgb_state().unwrap();
        assert_eq!(rgb.zones[0].brightness.percent(), 80);
        assert_eq!(
            rgb.zones[0].effect,
            RgbEffect::Static {
                color: RgbColor::new(255, 0, 0)
            }
        );

        let macros = dev.read_macros().unwrap();
        assert_eq!(macros.len(), 2);
        assert_eq!(macros[0].steps.len(), 1);
        assert_eq!(macros[0].steps[0].hid_keycode, 0x04);

        // Mutating writes under dry-run pass validation and are suppressed.
        dev.set_key_binding(0, KeyIndex::new(5), KeyCode::standard(0x04))
            .unwrap();
        dev.write_macros(&[MacroSlot::new(
            0,
            "M1",
            vec![MacroStep::new(MacroActionKind::KeyTap, 0x05, 0)],
        )])
        .unwrap();
        dev.set_zone_effect(
            RgbZoneId::new(0),
            RgbEffect::Static {
                color: RgbColor::new(0, 255, 0),
            },
            Brightness::new(90).unwrap(),
        )
        .unwrap();

        let suppressed = sink
            .snapshot()
            .into_iter()
            .filter(|e| e.outcome == PacketOutcome::DryRunSuppressed)
            .count();
        // 1 keybind + 1 macro chunk + 4 RGB writes (effect, brightness, color, save) = 6
        assert_eq!(suppressed, 6);
    }
}
