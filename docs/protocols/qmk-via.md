# QMK / VIA Raw HID Keyboard Protocol Specification (Keychron, Lemokey, NuPhy, MonsGeek, Drop, QMK)

**Status:** Verified directly from canonical upstream QMK firmware source (`qmk/qmk_firmware` `quantum/via.h`, `quantum/via.c`).

## 1. USB HID Identity & Transport Profiles

- **Usage Page:** `0xFF60` (`RAW_USAGE_PAGE`)
- **Usage ID:** `0x0061` (`RAW_USAGE_ID`)
- **Report ID:** `0x00` (unnumbered 32-byte Raw HID reports; `hidapi` writes 33 bytes `[0x00, payload[0..32]]` and reads 32 bytes)
- **Supported Vendor Families on `0xFF60:0x0061`:**
  - **Keychron:** `VID 0x3434` (Q / Q Pro / Q Max / V / V Max / K Pro / K Max / HE keyboards; excludes mouse PIDs `0x0440`, `0xD026`, `0xD029`)
  - **Lemokey (Keychron Gaming):** `VID 0x362D`
  - **NuPhy:** `VID 0x19F5`
  - **MonsGeek / Akko (QMK):** `VID 0x3151` (on `0xFF60:0x0061`)
  - **Glorious GMMK (QMK):** `VID 0x320F` (on `0xFF60:0x0061`)
  - **Drop:** `VID 0x04D8` (on `0xFF60:0x0061`)
  - **QMK Community (`0xFEED`):** `VID 0xFEED` (on `0xFF60:0x0061`)

## 2. Packet Framing

```text
TX (33 bytes to hidapi): [0x00 (Report ID), command_id, command_data[0..31]]
RX (32 bytes):           [command_id (or 0xFF=id_unhandled), command_data[0..31]]
```
No wire checksum is used by QMK Raw HID; the firmware echoes `command_id` on success or replaces byte 0 with `0xFF` (`id_unhandled`) if unsupported. All multi-byte integers are **big-endian**.

**Safety Gate:** `id_eeprom_reset` (`0x0A`) and `id_bootloader_jump` (`0x0B`) are strictly **blocked** by `PacketValidator`.

**Worked Example (`id_get_protocol_version = 0x01`):**
- TX (33 bytes): `00 01 00 00 ... 00`
- RX (32 bytes): `01 00 0C 00 ... 00` → VIA Protocol v12 (`0x000C`) ✓

## 3. Read Commands (`quantum/via.h` / `quantum/via.c`)

| Command ID | Name | Request Bytes `[1..]` | Response Bytes `[1..]` |
| --- | --- | --- | --- |
| `0x01` | `id_get_protocol_version` | `[]` | `[ver_hi, ver_lo]` (e.g. `0x000B`=v11, `0x000C`=v12, `0x000D`=v13) |
| `0x02` | `id_get_keyboard_value` | `[0x04]` (`id_firmware_version`) | `[0x04, v3, v2, v1, v0]` (u32 BE) |
| `0x04` | `id_dynamic_keymap_get_keycode` | `[layer, row, col]` | `[layer, row, col, kc_hi, kc_lo]` |
| `0x08` | `id_custom_get_value` | `[channel, value_id]` | `[channel, value_id, value_data...]` |
| `0x0C` | `id_dynamic_keymap_macro_get_count` | `[]` | `[macro_count]` |
| `0x0D` | `id_dynamic_keymap_macro_get_buffer_size` | `[]` | `[size_hi, size_lo]` (u16 BE) |
| `0x0E` | `id_dynamic_keymap_macro_get_buffer` | `[off_hi, off_lo, size (<=28)]` | `[off_hi, off_lo, size, data[0..size]]` |
| `0x11` | `id_dynamic_keymap_get_layer_count` | `[]` | `[layer_count]` |
| `0x12` | `id_dynamic_keymap_get_buffer` | `[off_hi, off_lo, size (<=28)]` | `[off_hi, off_lo, size, data[0..size]]` |

### Custom Lighting Channels (`0x07` set / `0x08` get / `0x09` save)
- Channel `0x03` (`id_qmk_rgb_matrix_channel`) and Channel `0x02` (`id_qmk_rgblight_channel`):
  - Value `1` = Brightness (`0..=255`)
  - Value `2` = Effect (`0` = Off, `1` = Solid/Static, `>=2` = Firmware animation effect ID)
  - Value `3` = Effect Speed (`0..=255`)
  - Value `4` = Color (`[hue_0_255, sat_0_255]` QMK HSV)

## 4. Write Commands

| Command ID | Name | Request Bytes `[1..]` | Effect |
| --- | --- | --- | --- |
| `0x05` | `id_dynamic_keymap_set_keycode` | `[layer, row, col, kc_hi, kc_lo]` | Updates one key binding in EEPROM |
| `0x06` | `id_dynamic_keymap_reset` | `[]` | Resets dynamic keymap to firmware defaults |
| `0x07` | `id_custom_set_value` | `[channel, value_id, data...]` | Updates live RGB Matrix / RGBLight value |
| `0x09` | `id_custom_save` | `[channel]` | Persists channel state to EEPROM |
| `0x0F` | `id_dynamic_keymap_macro_set_buffer` | `[off_hi, off_lo, size (<=28), data...]` | Writes macro buffer chunk (NUL-delimited slots) |
| `0x10` | `id_dynamic_keymap_macro_reset` | `[]` | Clears all macros |

## 5. Evidence Sources
- `qmk/qmk_firmware`: `quantum/via.h` (`VIA_PROTOCOL_VERSION`, `enum via_command_id`, `enum via_channel_id`, `enum via_qmk_rgb_matrix_value`), `quantum/via.c` (`raw_hid_receive`, `via_qmk_rgb_matrix_command`, `via_qmk_rgblight_command`).
