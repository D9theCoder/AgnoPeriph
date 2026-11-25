# Pulsar USB HID Protocol Specification (Report-8 Flash & XS-1)

**Status:** Verified from open-source blueprints (`OpenMouse-Project/mouse-protocol` `src/pulsar/index.ts`, `src/drivers/pulsar/pulsar-hid.ts`).

## 1. USB HID Identity & Transport Profiles

- **Vendor ID:** `0x3710` (Pulsar Gaming Gears)
- **Report-8 Flash Family** (X2, X2V2, X2H, X2A, X2 CrazyLight, Xlite V3, etc.):
  - **Usage Page:** `0xFF02` (vendor configuration collection with 16-byte Report ID `0x08` input & output reports)
  - **Report ID:** `0x08`
  - **Fixed Packet Size (including Report ID):** **17 bytes** (`[0x08, cmd, status, addr_hi, addr_lo, len, payload[0..10], checksum]`)
- **XS-1 Family** (Pulsar X3 series, wired PIDs `0x3404`, `0x3409`, `0x3410`; wireless PIDs `0x5402`, `0x5403`):
  - **Usage Page:** `0xFFFF`, **Usage ID:** `0x0001`
  - **Report ID:** `0x00`, **Fixed Packet Size:** 64 bytes with u16LE sum checksum of bytes `0..62` at bytes `62..63`.

## 2. Report-8 Framing & Checksum Algorithm

```text
Byte  0     : 0x08 (Report ID)
Byte  1     : Command ID
Byte  2     : Error / status (0x00 on requests; 0x00 = OK on responses)
Bytes 3..4  : Flash address (u16 big-endian, 0x0000 for non-flash commands)
Byte  5     : Payload length (0..=10)
Bytes 6..15 : Payload (zero-padded to 10 bytes)
Byte  16    : Checksum
```

### Checksum Formula (Packet & Sealed Flash Fields)
```text
checksum = (0x55u8).wrapping_sub(sum(preceding_bytes))
```
For the 17-byte packet including Report ID `0x08`, `(sum(bytes[0..17]) & 0xFF) == 0x55`.

**Worked Example (`BatteryLevel`, cmd `0x04`, addr `0x0000`, len `0`):**
- Bytes `0..16`: `08 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00`
- Sum: `0x08 + 0x04 = 0x0C` → Checksum: `(0x55 - 0x0C) & 0xFF = 0x49` ✓

### Pulsar `0x3710` DPI Encoding (`dpiEx` High-Range Formula)
Each DPI stage occupies 4 bytes `[low, low, flags, csum]` at `12 + stage * 4`:
- `10..=10,000` DPI (step 10): `raw = dpi / 10 - 1`, `dpi_ex = 0x00`
- `10,050..=30,000` DPI (step 50): `raw = (dpi - 10_050) / 50`, `dpi_ex = 0x22`
- `30,100..=32,000` DPI (step 100): `raw = (dpi / 2 - 10_050) / 50`, `dpi_ex = 0x33`
- `high = (raw >> 8) & 0x03`, `flags = (high << 2) | (high << 6) | dpi_ex | (dpi_ex << 4)`
- `csum = (0x55 - (low + low + flags)) & 0xFF`
- Decoding: `raw = low + (((flags & 0x0C) >> 2) << 8)`; `dpi = (raw + 1) * 10`; if `(flags & 0x02) != 0` then `dpi = dpi * 5 + 10_000`; if `(flags & 0x01) != 0` then `dpi *= 2`.

**Worked Example (800 DPI):** `raw = 79 (0x4F)`, `high = 0`, `flags = 0x00`, `csum = (0x55 - 0x9E) & 0xFF = 0xB7` → `[0x4F, 0x4F, 0x00, 0xB7]`. Decode: `(79 + 1) * 10 = 800` ✓.

## 3. Read Commands

| Command ID | Name | Payload | Response Payload |
| --- | --- | --- | --- |
| `0x01` | `EncryptionData` | 8-byte challenge | `[..., cid (byte 9), mid (byte 10), type (byte 11), dongle_type (byte 12)]` (indices after Report ID) |
| `0x03` | `DeviceOnline` | `[1]` (enter) / `[0]` (exit) | `payload[0]=online (1)`, `payload[4]=busy (1)` |
| `0x04` | `BatteryLevel` | `[]` | `[percent (0..=100), charging (0/1), mv_hi, mv_lo]` |
| `0x08` | `ReadFlashData` | `addr`, `len (1..=10)` | Requested flash bytes |
| `0x0E` | `GetCurrentConfig` | `[]` | `[profile_index]` (`0..=3`) |
| `0x12` | `ReadVersionId` | `[]` | `[major, minor_bcd]` |
| `0x1D` | `GetDongleVersion` | `[]` | `[major, minor_bcd]` |

`type` byte in `EncryptionData` determines connection & max polling rate:
- `0` → Wireless 1000 Hz, `1` → Wireless 4000 Hz, `2` → Wired 1000 Hz, `3` → Wired 8000 Hz, `4` → Wireless 2000 Hz, `5` → Wireless 8000 Hz.

## 4. Write Commands & Flash Map

| Command ID | Name | Effect |
| --- | --- | --- |
| `0x07` | `WriteFlashData` | Writes sealed flash bytes at `addr` |
| `0x0F` | `SetCurrentConfig` | Switches active onboard profile slot (`0..=3`) |

| Flash Address | Data Len (+1 csum) | Field | Encoding |
| --- | --- | --- | --- |
| `0` | `1 + 1` | Polling Rate | `<=1000`: `1000/rate` (`0x08`=125, `0x04`=250, `0x02`=500, `0x01`=1000); `>1000`: `rate/2000*16` (`0x10`=2000, `0x20`=4000, `0x40`=8000) |
| `2` | `1 + 1` | DPI Stage Count | `1..=8` |
| `4` | `1 + 1` | Active DPI Stage | `0..stage_count` |
| `10` | `1 + 1` | Lift-Off Distance | `3` = Low (0.7 mm), `1` = Medium (1.0 mm), `2` = High (2.0 mm) |
| `12 + i*4` | `3 + 1` | DPI Stage `i` (`0..8`) | Pulsar `dpiEx` 4-byte record |
| `169` | `1 + 1` | Debounce (ms) | `0..=15` |
| `171` | `1 + 1` | Motion Sync | `0` = off, `1` = on |
| `175` | `1 + 1` | Angle Snapping | `0` = off, `1` = on |
| `177` | `1 + 1` | Ripple Control | `0` = off, `1` = on |

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/pulsar/index.ts`, `src/drivers/pulsar/pulsar-hid.ts`): Hardware-verified on Pulsar X2 CrazyLight (`VID 0x3710`, `CID 0x57`, `MID 0x04`) against `bbb.pulsar.gg`.
