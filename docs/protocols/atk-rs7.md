# ATK RS7 Hall Effect Keyboard ("DuckBread") Protocol Specification

**Status:** Verified via WebHID JavaScript reconstruction of official **ATK HUB v3.2.33** (`https://hub.atk.pro/` → `api-NHsRvjW6.js` and `demoMode.store-x0QG9XUG.js`) and live Windows HID enumeration (`agnoperiph-cli list`).

---

## 1. USB HID Identity & Transport Profile

| Field | Value | Source |
| --- | --- | --- |
| **Vendor Name** | Shenzhen Yizhita Technology / ATK (`solutionProvider: DUCKBREAD`) | Live `hidapi` + `demoMode.store-x0QG9XUG.js` |
| **VID** | `0x373B` (`14139`) | Live `agnoperiph-cli list` + `demoMode.store-x0QG9XUG.js` |
| **PID (Wired USB)** | `0x1044` (`4164`, `ATK RS7 RGB`, Serial `ATK RS HE`) | Live `agnoperiph-cli list` + `demoMode.store-x0QG9XUG.js` |
| **Connection Kind** | `WiredUsb` (`connectType: 2`, wired-only model) | `demoMode.store-x0QG9XUG.js` |
| **Usage Page** | `0xFF60` (`65376`) | Live `agnoperiph-cli list` + `demoMode.store-x0QG9XUG.js` |
| **Usage ID** | `0x0061` (`97`) | Live `agnoperiph-cli list` + `demoMode.store-x0QG9XUG.js` |
| **Interface #** | `1` | Live `agnoperiph-cli list` |
| **Report ID** | `0x00` (unnumbered Raw HID report) | `api-NHsRvjW6.js` (`transferForResultAsync(..., 0)`) |
| **Packet Size** | **32 bytes** payload (`33` bytes on Windows `hidapi` TX with leading `0x00` Report ID) | `api-NHsRvjW6.js` (`I.SIZE = 32`, `I.baseOffset = 1`) |
| **Key Matrix** | `6 rows × 16 cols` = `96` matrix slots (`4` layers) | `demoMode.store-x0QG9XUG.js` (`ah = {layer:4, row:6, col:16}`) |

> **Bootloader PID Excluded (`0x373B:0x2002`, `UP=0x00FF`, `U=0x0001`):** AgnoPeriph never opens bootloader interfaces and explicitly hard-blocks `id_bootloader_jump (0x0B)`, `id_eeprom_reset (0x0A)`, `iap (0x98)`, and `resetAll (0x15)` inside `GuardedTransport`'s `PacketValidator`.

---

## 2. Framing, Endianness, Travel Math & Checksum

### Wire Framing
- **TX (`hidapi::write`):** 33 bytes = `[0x00 (Report ID), p[0], p[1], ..., p[31]]`
- **RX (`hidapi::read_timeout`):** 32 bytes `[p[0..32]]` (or 33 bytes if leading Report ID `0x00` is returned; responses echo `p[0] == request_cmd_id`).
- **Padding:** Unused trailing bytes are zero-filled to 32 payload bytes.
- **Checksum:** None (standard USB HID 32-byte QMK/VIA-derived Raw HID framing).
- **Endianness:** **Big-Endian (`>u16`)** — JS `DataView.getUint16(offset)` / `setUint16(offset, val)` defaults to big-endian (`littleEndian = false`).

### Travel Distance Unit Conversion (Fixed-Point 1:1)
All actuation, Rapid Trigger, deadzone, and SOCD trigger distances are encoded as **`u16` big-endian hundredths of a millimeter (`0.01 mm` units)**:
- `0.10 mm` → `10` (`0x00 0x0A`)
- `1.50 mm` → `150` (`0x00 0x96`)
- `3.40 mm` → `340` (`0x01 0x54`)

Hardware bounds for **ATK RS7** (`TRAVEL_INFO_MAP["ATK RS7"]`):
- **Actuation:** `0.10 mm..=3.40 mm` (`10..=340`), step `0.02 mm` (normal) or `0.01 mm` (extreme / `gameMode == 1`).
- **Rapid Trigger:** `0.02 mm..=3.40 mm` (`2..=340`), step `0.02 mm` (normal) or `0.01 mm` (extreme).

### Worked Packet Example (Read Single-Key Actuation for Row 2, Col 1 — `A` key)
- Payload bytes `p[0..32]`:
  - `p[0] = 0x02` (`id_get_keyboard_value`)
  - `p[1] = 0x96` (`customId = 150`)
  - `p[2] = 0x03` (`oneTogTh = 3`)
  - `p[3] = 0x02` (`row = 2`)
  - `p[4] = 0x01` (`col = 1`)
  - `p[5..32] = 0x00`
- Windows `hidapi` 33-byte TX buffer:
  `00 02 96 03 02 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00`
- Expected reply `p[5..7]`: `0x00 0x96` → `150` → **`1.50 mm`**.

---

## 3. Read Commands (Read-Only Queries)

All byte offsets below are 0-based indices into the 32-byte payload `p[0..32]` (`baseOffset = 1`).

| Command | Request `p[0..8]` | Response Payload |
| --- | --- | --- |
| **Firmware Version** (`fa.get`) | `[0x02, 0x96, 0x00, ...]` | `p[5]` (major hex), `p[6]` (minor hex, 2 digits) → `"{:x}.{:02x}"` |
| **Active Layer** (`sr.get`) | `[0x02, 0x96, 0x13, ...]` | `p[3]` mapped `{1:0, 2:0, 4:2, 8:2}` |
| **Game / Extreme Mode** (`_e.get`) | `[0x02, 0x96, 0x1E, ...]` | `p[3]` (`0` = normal 0.02 mm step, `1` = extreme 0.01 mm step) |
| **Polling Rate** (`Zs.get`) | `[0x02, 0x96, 0x22, ...]` | `p[3]` (`1`=1000Hz, `2`=2000Hz, `4`=4000Hz, `8`=8000Hz) |
| **Single-Key Actuation** (`Ps.get`) | `[0x02, 0x96, 0x03, row, col]` | `p[5..7]` `>u16` hundredths mm |
| **Bulk Actuation Buffer** (`Gs.get`) | `[0x02, 0x96, 0x0D, 0, 0, off_hi, off_lo, count]` | `p[8 .. 8 + count*2]`: `count` (`<=12`) `>u16` hundredths mm |
| **Single-Key Rapid Trigger** (`Hs.get`) | `[0x02, 0x96, 0x07, row, col]` | `p[5]` `on` (`0`/`1`), `p[6..8]` `release` `>u16`, `p[8..10]` `press` `>u16` |
| **Bulk Rapid Trigger Buffer** (`js.get`) | `[0x02, 0x96, 0x0E, 0, 0, off_hi, off_lo, count]` | `p[8 .. 8 + count*5]`: `count` (`<=4`) × `[on, rel_hi, rel_lo, press_hi, press_lo]` |
| **Global Deadzone** (`Xs.get`) | `[0x02, 0x96, 0x0B]` | `p[3..5]` `top` `>u16`, `p[5..7]` `bottom` `>u16` |
| **DKS Slot** (`Wt.get`) | `[0x02, 0x96, 0x0F, 0, 0, 0x01, idx]` (`idx: 0..20`) | `p[7..23]`: 4 actions × 4 bytes `[status_hi, status_lo, keyClass, keyId]` |
| **SOCD / RS Slot** (`rs.get`) | `[0x02, 0x96, 0x20, 0, 0, 0x01, idx, 0, idx]` (`idx: 0..20`) | `p[7]=mode`, `p[8]=id`, `p[9..11]=point (>u16)`, `p[11]=withGlobal`, `p[12]=rt`, `p[13..17]=[k1_r, k1_c, k2_r, k2_c]`, `p[17]=layer` |
| **Calibration Init Status** (`Qt.getInitStatusBuffer`) | `[0x02, 0x96, 0x1F, 0, 0, off_hi, off_lo, count]` | `p[8 .. 8 + count]`: `count` (`<=24`) bytes (`1` = calibrated, `0` = uncalibrated) |
| **Backlight RGB** (`Ys.get`) | `[0x08, 0x41]` | `p[2]=effect`, `p[4]=speed`, `p[5..8]=[r, g, b]`, `p[8]=brightness (0..=100)` |
| **Logo Light** (`Js.get`) | `[0x02, 0x96, 0x21]` | `p[3]=mode`, `p[4]=colorful`, `p[5]=brightness`, `p[6]=speed`, `p[7..10]=[r, g, b]` |

### SOCD / RS Mode Enum (`G`)
| Wire Byte | Name | Mapped `SocdMode` |
| --- | --- | --- |
| `0` | `NONE` | Inactive slot |
| `1` | `RS` (Rappy Snappy) | *(non-SOCD)* |
| `2` | `SOCD` | `SocdMode::LastInputPriority` |
| `3` | `SOCD_KEY1` | `SocdMode::KeyAPriority` |
| `4` | `SOCD_KEY2` | `SocdMode::KeyBPriority` |
| `5` | `SOCD_BALANCE` | `SocdMode::Neutral` |
| `6` | `OKS` | *(non-SOCD)* |

---

## 4. Write Commands (Guarded by `GuardedTransport` + `--dry-run`)

| Command | Request `p[0..32]` | Effect |
| --- | --- | --- |
| **Single-Key Actuation** (`Ps.create`) | `[0x03, 0x96, 0x03, row, col, apc_hi, apc_lo]` | Updates actuation point for `(row, col)` |
| **Bulk Actuation Buffer** (`Gs.create`) | `[0x03, 0x96, 0x0D, 0, 0, off_hi, off_lo, count, flashOp, data[0..22]]` | Writes `11` keys per chunk (`flashOp`: `1`=erase on first chunk, `0`=normal, `2`=write on last chunk; 200 ms delay after erase/write) |
| **Single-Key Rapid Trigger** (`Hs.create`) | `[0x03, 0x96, 0x07, row, col, on, rel_hi, rel_lo, press_hi, press_lo]` | Updates Rapid Trigger for `(row, col)` |
| **Global Deadzone** (`Xs.create`) | `[0x03, 0x96, 0x0B, top_hi, top_lo, bot_hi, bot_lo]` | Updates top & bottom deadzones |
| **DKS Slot** (`Wt.create`) | `[0x03, 0x96, 0x0F, 0, 0, 0x03, idx, actions[16]]` | Writes 4-stage DKS action table for slot `idx` (`0..20`) |
| **SOCD / RS Slot** (`rs.create`) | `[0x03, 0x96, 0x20, 0, 0, 0x03, idx, mode, id, pt_hi, pt_lo, withGlobal, rt, k1_r, k1_c, k2_r, k2_c, layer]` | Writes or clears (`mode=0`) one SOCD pair slot (`0..20`) |
| **Backlight RGB** (`Ys.create`) | `[0x07, 0x41, effect, 0, speed, r, g, b, brightness]` | Sets main key backlight effect, speed, RGB color, and brightness (`0..=100`) |
| **Logo Light** (`Js.create`) | `[0x03, 0x96, 0x21, mode, colorful, brightness, speed, r, g, b]` | Sets ATK RS7 logo light zone |

---

## 5. Evidence (Verbatim Recovered JS Snippets from `ATK HUB v3.2.33`)

### Device Filter & Matrix (`demoMode.store-x0QG9XUG.js`)
```js
var ah = { layer: 4, row: 6, col: 16, connectType: 2, name: "ATK RS7", title: "ATK RS7", firmwareMark: "duck", supportLogoLight: true };
// filters: [{ vendorId: 14139 (0x373B), productId: 4164 (0x1044), usagePage: 65376 (0xFF60), usage: 97 (0x0061) }]
```

### Packet Base, Command Enums & Travel Bounds (`api-NHsRvjW6.js`)
```js
I.SIZE = 32; I.baseOffset = 1;
U = { id_get_keyboard_value: 2, id_set_keyboard_value: 3, id_lighting_set_value: 7, id_lighting_get_value: 8 };
_ = { customId: 150, oneTogTh: 3, oneRt: 7, deadBand: 11, bufferTogTh: 13, bufferRt: 14, dks: 15,
      lightInfo: 65, calibrationStart: 24, calibrationFinish: 25, gameMode: 30, calibration: 31, rs: 32, boxLight: 33, reportRate: 34 };
G = { NONE: 0, RS: 1, SOCD: 2, SOCD_KEY1: 3, SOCD_KEY2: 4, SOCD_BALANCE: 5, OKS: 6 };
TRAVEL_INFO_MAP["ATK RS7"] = {
  normal:  { travelMax: 3.4, travelMin: 0.1, travelStep: 0.02, rtMax: 3.4, rtMin: 0.02, rtStep: 0.02 },
  extreme: { travelMax: 3.4, travelMin: 0.1, travelStep: 0.01, rtMax: 3.4, rtMin: 0.02, rtStep: 0.01 }
};
```

### Actuation, Rapid Trigger, DKS, SOCD, Calibration & RGB Codecs (`api-NHsRvjW6.js`)
```js
// Single-key actuation (Ps): p[0]=2|3, p[1]=150, p[2]=3, p[3]=row, p[4]=col, p[5..7]=apc*100 (u16 BE)
// Bulk actuation (Gs): p[0]=2|3, p[1]=150, p[2]=13, p[5..7]=offset (u16 BE), p[7]=size, get: p[8..8+size*2], set: p[8]=flashOp, p[9..]=data
// Single-key RT (Hs): p[0]=2|3, p[1]=150, p[2]=7, p[3]=row, p[4]=col, p[5]=on, p[6..8]=release*100, p[8..10]=press*100
// Bulk RT (js): p[0]=2|3, p[1]=150, p[2]=14, p[5..7]=offset, p[7]=size(4), get: p[8..8+size*5]
// Deadzone (Xs): p[0]=2|3, p[1]=150, p[2]=11, p[3..5]=top*100, p[5..7]=bottom*100
// DKS (Wt): p[0]=2|3, p[1]=150, p[2]=15, p[5]=1(get)|3(set), p[6]=index, p[7..23]=4 * [status_u16_be, keyClass, keyId]
// SOCD/RS (rs): p[0]=2|3, p[1]=150, p[2]=32, p[5]=1|3, p[6]=index, p[7]=mode, p[8]=id, p[9..11]=point*100, p[11]=withGlobal, p[12]=rt, p[13..17]=k1r,k1c,k2r,k2c, p[17]=layer
// Calibration status (Qt.getInitStatusBuffer): p[0]=2, p[1]=150, p[2]=31, p[5..7]=offset, p[7]=size(24), p[8..8+size]
// Backlight RGB (Ys): p[0]=8|7, p[1]=65, p[2]=effect, p[4]=speed, p[5..8]=r,g,b, p[8]=brightness
// Logo Light (Js): p[0]=2|3, p[1]=150, p[2]=33, p[3]=mode, p[4]=colorful, p[5]=brightness, p[6]=speed, p[7..10]=r,g,b
```
