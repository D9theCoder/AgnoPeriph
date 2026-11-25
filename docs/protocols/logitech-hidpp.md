# Logitech HID++ 1.0 / 2.0 Protocol Specification

**Status:** Verified from open-source blueprints (`libratbag`, `pwr-Solaar/Solaar`, `Gianlz/pxmouse`, `qu1ncky/BatteryTray`).

## 1. USB HID Identity & Transport Profiles

| Field | Wired USB | 2.4 GHz Receiver (Lightspeed / Bolt / Unifying) |
| --- | --- | --- |
| **VID** | `0x046D` | `0x046D` |
| **PID** | Non-receiver, **excluding webcams `0x0800..=0x09FF`** | `0xC500..=0xC5FF` (e.g., `0xC547`, `0xC539`, `0xC52B`, `0xC548`) |
| **Usage Page** | `0xFF00` (vendor HID++) | `0xFF00` (vendor HID++) |
| **Device Index** | `0xFF` (fallback `0x00` for HID++ 1.0) | `0x01..=0x06` (receiver paired slots) |
| **Report IDs & Lengths** | `0x10` (Short: **7 bytes**), `0x11` (Long: **20 bytes**) | `0x10` (Short: **7 bytes**), `0x11` (Long: **20 bytes**) |

> **Safety Note:** Never open Logitech webcam PIDs (`0x0800..=0x09FF`) as HID++ devices; sending vendor reports can lock up webcam firmware (`qu1ncky/BatteryTray` `logitech.rs`).

> **Windows Split Collection Note:** Windows splits a Logitech HID++ interface into separate Short (`0x10`, 7-byte) and Long (`0x11`, 20-byte) HID collections. All HID++ 2.0 requests may be sent over the Long (`0x11`, 20-byte) collection.

## 2. Packet Framing & Validation

HID++ uses fixed-size reports with no trailing checksum byte (integrity is handled by USB HID framing):

```text
Short Report (7 bytes):  [0x10, dev_index, feat_idx, (func << 4) | sw_id, p0, p1, p2]
Long  Report (20 bytes): [0x11, dev_index, feat_idx, (func << 4) | sw_id, p0..p15]
```

- **Endianness:** Multi-byte integers in HID++ 2.0 feature payloads are **Big-Endian** (`>u16`).
- **Software ID (`sw_id`):** Non-zero 4-bit tag (`0x0C` in AgnoPeriph; distinct from G HUB `0x0D` and Solaar `0x0B`) echoed in replies so unsolicited notifications (`sw_id == 0`) are ignored.
- **Error Frames:**
  - HID++ 2.0 error: `resp[2] == 0xFF`, `resp[3] == feat_idx`, `resp[4] == (func << 4) | sw_id`, `resp[5] == error_code` (`0x08` = Busy).
  - HID++ 1.0 error: `resp[2] == 0x8F`, `resp[5] == error_code` (`0x01` = Invalid SubID / HID++ 1.0 device, `0x08` = Empty slot, `0x04`/`0x09` = Offline/Asleep).

## 3. Read Commands

| Feature ID | Feature Name | Func | Params | Response Format |
| --- | --- | --- | --- | --- |
| `0x0000` | `Root.GetFeature` | `0` | `[fid_hi, fid_lo]` | `[feat_idx, flags, version]` (`0` = unsupported) |
| `0x0000` | `Root.Ping` | `1` | `[0x00, 0x00, marker]` | `[proto_major, proto_minor, marker]` |
| `0x0003` | `DeviceFWVersion` | `0` / `1` | `[]` / `[entity_ix]` | Count / `[type, prefix[3], major, minor, build_hi, build_lo]` |
| `0x0005` | `DeviceNameType` | `0` / `1` / `2` | `[]` / `[offset]` / `[]` | Name length / up to 16 UTF-8 bytes / device type enum |
| `0x1004` | `UnifiedBattery` | `1` | `[]` | `[soc_pct, level_bits, status, ext_power]` |
| `0x1000` | `BatteryStatus` | `0` | `[]` | `[pct, next_level, status]` |
| `0x1001` | `BatteryVoltage` | `0` | `[]` | `[mv_hi, mv_lo, flags]` (`flags & 0x80`: external power; `& 0x01`: full) |
| `0x2201` | `AdjustableDPI` | `1` / `2` | `[sensor]` | Supported DPI list/step markers (`v >> 13 == 0b111` → step `v & 0x1FFF`) / `[sensor, cur_hi, cur_lo, def_hi, def_lo]` |
| `0x8060` | `ReportRate` | `0` / `1` | `[]` | Bitmask (bit `ms-1` for `1..=8 ms`) / `[current_ms]` |
| `0x8100` | `OnboardProfiles` | `0` / `2` / `4` | `[]` | Info (`r[3]=profile_count`, `r[7..9]=sector_size`) / `[mode]` (`1`=Onboard, `2`=Host) / `[active_sector_hi, lo]` |

## 4. Write Commands

| Feature ID | Feature Name | Func | Params | Effect |
| --- | --- | --- | --- | --- |
| `0x2201` | `AdjustableDPI.SetSensorDpi` | `3` | `[sensor, dpi_hi, dpi_lo]` | Sets live sensor DPI |
| `0x8060` | `ReportRate.SetReportRate` | `2` | `[interval_ms]` | Sets polling interval (`1`=1000Hz, `2`=500Hz, `4`=250Hz, `8`=125Hz) |
| `0x8100` | `OnboardProfiles.SetMode` | `1` | `[mode]` | `1` = Onboard Memory, `2` = Host Controlled |
| `0x8100` | `OnboardProfiles.SetActiveProfile` | `3` | `[sector_hi, sector_lo]` | Selects active onboard profile sector (`1..=profile_count`) |

## 5. Evidence Sources
- `Gianlz/pxmouse` (`hidpp.py` lines 13–242): `SHORT=0x10`, `LONG=0x11`, feature IDs, `dpi_list` step-marker decoding (`v >> 13 == 0b111`), `rate_list` bitmask, `OnboardProfiles` functions `0..4`.
- `qu1ncky/BatteryTray` (`src/providers/logitech.rs`): Windows split short/long collection behavior, webcam PID exclusion `0x0800..=0x09FF`, `UnifiedBattery` (`0x1004`), `BatteryStatus` (`0x1000`), `BatteryVoltage` (`0x1001`) + Solaar 13-point mV-to-percentage discharge curve.
