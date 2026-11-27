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
| `0x1B04` | `ReprogControlsV4` | `0` / `1` / `2` | `[]` / `[pos]` / `[cid_hi, cid_lo]` | Count / `[cid_hi, cid_lo, tid_hi, tid_lo, flags, pos, group, group_mask]` / `[cid_hi, cid_lo, flags_lo, mapped_hi, mapped_lo, flags_hi]` |
| `0x40A0` | `FnInversion` | `0` | `[]` | `[state]` (`params[0] & 0x0F`: `0`=F1–F12, `1`=Media/Shortcuts) |
| `0x40A2` | `NewFnInversion` | `0` | `[]` | `[state, default_state]` (`0`=Off, `1`=On) |
| `0x40A3` | `K375sFnInversion` | `0` | `[host_ix]` (`0xFF`=current) | `[host_ix, state, default_state, caps]` |
| `0x4522` | `DisableKeysByUsage` | `0` | `[]` | `[max_usages]` (Game Mode / key-lock capacity) |
| `0x8060` | `ReportRate` | `0` / `1` | `[]` | Bitmask (bit `ms-1` for `1..=8 ms`) / `[current_ms]` |
| `0x8070` | `ColorLEDEffects` | `0` / `1` / `2` / `14` | `[]` / `[zone]` / `[zone, ord]` / `[zone]` | Info (`p[0]=zone_count`, `p[3..5]&1`=readback) / Zone info (`p[1..3]=loc`, `p[3]=eff_count`) / Effect info (`p[2..4]=eff_id`, `p[6..8]=period`) / Active RAM effect (`[zone, ord, p0..p9]`) |
| `0x8071` | `RGBEffects` | `0` | `[0xFF, 0xFF]` / `[cl, 0xFF]` / `[cl, ord]` | Cluster count (`p[2]`) / Cluster info (`p[2..4]=loc`, `p[4]=eff_count`) / Effect info (`p[2..4]=eff_id`, `p[6..8]=period`) |
| `0x8100` | `OnboardProfiles` | `0` / `2` / `4` / `5` | `[]` / `[]` / `[]` / `[mem_type, page, off_hi, off_lo]` | Info (`r[3]=profile_count`, `r[4]=profile_count_oob`, `r[7..9]=sector_size`) / `[mode]` (`1`=Onboard, `2`=Host) / `[active_mem_type, active_page]` / 16 bytes of sector memory (`0x0000` writeable directory or `0x0100` ROM directory: 4-byte `[mem_type, page, enabled, reserved]` entries terminated by `0xFFFF`) |

## 4. Write Commands

| Feature ID | Feature Name | Func | Params | Effect |
| --- | --- | --- | --- | --- |
| `0x1B04` | `ReprogControlsV4.SetReporting` | `3` | `[cid_hi, cid_lo, 0x00, target_hi, target_lo]` | Remaps a reprogrammable control to a legal `target` CID (`0x0000` restores default) |
| `0x2201` | `AdjustableDPI.SetSensorDpi` | `3` | `[sensor, dpi_hi, dpi_lo]` | Sets live sensor DPI |
| `0x40A0` | `FnInversion.SetState` | `1` | `[state]` | Sets Fn-key inversion (`0`=Off, `1`=On) |
| `0x40A2` | `NewFnInversion.SetState` | `1` | `[state]` | Sets global Fn-key inversion (`0`=Off, `1`=On) |
| `0x40A3` | `K375sFnInversion.SetState` | `1` | `[host_ix, state]` | Sets per-host Fn-key inversion (`host_ix = 0xFF` for current host) |
| `0x4522` | `DisableKeysByUsage` | `1` / `2` / `3` | `[usages[0..16]]` / `[usages[0..16]]` / `[]` | `fn 1` disables listed 8-bit HID key usages (e.g. `[0xE3, 0xE7, 0x65]` for Game Mode / Win Lock); `fn 2` re-enables listed usages; `fn 3` re-enables all keys |
| `0x8060` | `ReportRate.SetReportRate` | `2` | `[interval_ms]` | Sets polling interval (`1`=1000Hz, `2`=500Hz, `4`=250Hz, `8`=125Hz) |
| `0x8070` | `ColorLEDEffects.SetZoneEffect` | `3` | `[zone, effect_ord, p0..p9, 0, 0, 0, 0]` | Sets RAM effect on zone (`0x00`=Off, `0x01`=Static `[r,g,b]`, `0x03`=Cycling `[0..0,p5=period_hi,p6=period_lo,p7=intensity]`, `0x04`=Wave `[0..0,p5=period_hi,p6=period_lo]`, `0x0A`=Breathing `[r,g,b,p3=period_hi,p4=period_lo,0,p6=intensity]`) |
| `0x8071` | `RGBEffects.SetClusterEffect` | `5` / `1` | `[0x01, 0x01]` / `[cluster, effect_ord, p0..p9, 0x01, 0, 0, 0]` | Claims SW RGB control (`fn 5`, re-issued before writes as sleep clears it) and sets cluster effect (`0x00`=Off, `0x01`=Static `[r,g,b,0x02]`, `0x03`/`0x15`=Cycling, `0x04`/`0x16`=Wave, `0x0A`=Breathing). For `0x15`/`0x16`: `p1=saturation`, `p6..p7=period_be`, `p8=intensity`, `p9=direction` (`1`=Right, `2`=Down, `6`=Left, `7`=Up). Prefer `0x16` over `0x04` when both exist. |
| `0x8100` | `OnboardProfiles.SetMode` | `1` | `[mode]` | `1` = Onboard Memory, `2` = Host Controlled |
| `0x8100` | `OnboardProfiles.SetActiveProfile` | `3` | `[mem_type, page]` | Selects an enabled onboard profile from the active directory (`0x00` writeable or `0x01` ROM); requires Onboard Memory mode (`func 1` = `1`), otherwise firmware rejects with `0x02` (`INVALID_ARGUMENT`) |

### Keyboard Feature Detail Notes (Solaar `hidpp20.py` / `special_keys.py`)
- **`0x1B04` `ReprogControlsV4` flags & group mask:** The 16-bit capability flags combine response byte `4` (low) and byte `8` (high); bit `0x10` indicates a reprogrammable control. Group `N` (`N >= 1`) corresponds to bit `1 << (N - 1)` in `group_mask`. A target CID is legal when it is `0x0000` (reset to default), the control's own CID, or a control whose group bit is set in the source control's `group_mask`.
- **`0x8070`/`0x8071` Zone Locations (`LEDZoneLocations`):** `0x0001` Primary, `0x0002` Logo, `0x0003` Left Side, `0x0004` Right Side, `0x0005` Combined, `0x0006` Primary + Logo, `0x0007` Macro Keys, `0x0008` Media Keys, `0x0009` Indicators, `0x000A` Function Keys, `0x000B` Backlight.
- **`0x4522` `DisableKeysByUsage`:** No readback command exists in HID++ 2.0; if `fn 0` reports capacity `0`, drivers cap requests at 16 usages (one Long HID++ payload).

## 5. Evidence Sources
- `Gianlz/pxmouse` (`hidpp.py` lines 13–242): `SHORT=0x10`, `LONG=0x11`, feature IDs, `dpi_list` step-marker decoding (`v >> 13 == 0b111`), `rate_list` bitmask, `OnboardProfiles` functions `0..4`.
- `qu1ncky/BatteryTray` (`src/providers/logitech.rs`): Windows split short/long collection behavior, webcam PID exclusion `0x0800..=0x09FF`, `UnifiedBattery` (`0x1004`), `BatteryStatus` (`0x1000`), `BatteryVoltage` (`0x1001`) + Solaar 13-point mV-to-percentage discharge curve.
- `pwr-Solaar/Solaar` (`lib/logitech_receiver/hidpp20.py`, `hidpp20_constants.py`, `special_keys.py`, `settings_templates.py`, `settings.py`), `libratbag/libratbag` (`src/hidpp20.h`, `src/hidpp20.c`), `OpenMouse-Project/keyboard-protocol` (`src/logitech/index.ts`, `controls.ts`, `src/drivers/logitech/hid.ts`), and `OpenLogi` (`x40a0`, `x40a2`, `x40a3`, `x4522`): keyboard features `0x8070`, `0x8071`, `0x40A0`, `0x40A2`, `0x40A3`, `0x4522`, and `0x1B04`.
