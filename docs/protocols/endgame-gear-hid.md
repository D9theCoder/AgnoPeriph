# Endgame Gear OP1 / XM2 8K & 4K Wireless USB HID Protocol Specification

**Status:** Verified from open-source blueprints (`OpenMouse-Project/mouse-protocol` `src/endgame-gear/op1.ts`, `src/drivers/endgame/egg-op1-hid.ts`).

## 1. USB HID Identity & Transport Profiles

- **Vendor ID:** `0x3367` (Endgame Gear)
- **Feature Reports:**
  - `0xA0`: Configuration blob (**1041 bytes** including Report ID `0xA0`)
  - `0xA1`: Command & status channel (**64 bytes** including Report ID `0xA1`)
- **Vendor Collection:** Usage Page `>= 0xFF00` exposing feature reports `0xA0` and `0xA1`.

| Model | PID | Connection Kind | Config Family | Sensor | CPI Range | Max Polling |
| --- | --- | --- | --- | --- | --- | --- |
| OP1 8K | `0x1964` | `WiredUsb` | `v1` | PAW3395 | 50..26,000 (step 50) | 8000 Hz |
| XM2 8K | `0x1966` | `WiredUsb` | `v1` | PAW3395 | 50..26,000 (step 50) | 8000 Hz |
| OP1 8K Purple Frost | `0x1976` | `WiredUsb` | `v1` | PAW3950 | 50..30,000 (step 50) | 8000 Hz |
| OP1 8K v2 | `0x1978` | `WiredUsb` | `v2` | PAW3950 | 10..30,000 (step 10/50) | 8000 Hz |
| XM2 8K v2 | `0x1980` | `WiredUsb` | `v2` | PAW3950 | 10..30,000 (step 10/50) | 8000 Hz |
| OP1w 4K (Wired) | `0x1972` | `WiredUsb` | `v1` (4K) | PAW3395 | 50..26,000 (step 50) | 4000 Hz |
| XM2w 4K (Wired) | `0x1968` | `WiredUsb` | `v1` (4K) | PAW3395 | 50..26,000 (step 50) | 4000 Hz |
| OP1w 4K v2 (Wired) | `0x1984` | `WiredUsb` | `v2` (4K) | PAW3950 | 10..30,000 (step 10/50) | 4000 Hz |
| XM2w 4K v2 (Wired) | `0x1982` | `WiredUsb` | `v2` (4K) | PAW3950 | 10..30,000 (step 10/50) | 4000 Hz |
| OP1w/XM2w 4K Dongle | `0x1970` | `WirelessDongle24Ghz` | `v2` (4K, probed via `0x0E`) | PAW3950/3395 | 10..30,000 | 4000 Hz |

## 2. Framing & Config Blob Offsets

### Command Frame (`Report ID 0xA1`, 64 bytes)
```text
Byte  0     : 0xA1 (Report ID)
Byte  1     : Operation (0x02=Firmware, 0x0E=MouseInfo, 0x11=Store, 0x12=Load, 0x14=WriteSensor, 0x15=WritePower, 0x16=WriteButtons)
Bytes 2..6  : Optional block-write header ([0x0F, declared_len, 0x00, 0x00, chunk_idx])
Bytes 16..  : Optional block-write payload
```
Status replies return `Byte 1`: `0x01`=OK, `0x03`=Busy, `0x07`=Rejected, `0x08`=RF Sleep.

**Worked Example (`LoadConfig`, report `0xA1`, op `0x12`):**
- TX: `A1 12 00 ... 00` (64 bytes)
- RX (`GET_REPORT 0xA0`): `A0 01 ...` (1041 bytes; `bytes[1] == 0x01` OK) ✓

### Config Blob Offsets (`Report ID 0xA0`, index 0 = `0xA0`)

| Offset | Size | Field | Encoding |
| --- | --- | --- | --- |
| `21` | 1 | Polling Divider | `8000 / rate_hz` (`1`=8000, `2`=4000, `4`=2000, `8`=1000, `16`=500, `32`=250, `64`=125; `0x80`=1000 Hz power-save on 4K) |
| `25` | 1 | Lift-Off Distance | `v1`: `0`=0.7mm, `1`=1.0mm, `2`=2.0mm; `v2`: `0..=10` = `0.7..=1.7` mm; `4K v1`: `1`=1.0mm, `2`=2.0mm |
| `26` | 1 | Angle Snapping | `0` = off, `1` = on |
| `27` | 1 | Ripple Control | `0` = off, `1` = on |
| `28` | 1 | Motion Sync | `0` = off, `1` = on |
| `30` | 1 | CPI Stage Count | `1..=4` |
| `51 + i*5` | 5 | CPI Stage `i` (`0..4`) | `[split (0\|1), x_lo, x_hi, y_lo, y_hi]` (u16 LE) |
| `127` | 1 | Glass Mode | `0` = off, `1` = on |

## 3. Read Commands
- `0x02` (`Firmware`): `SET_REPORT(0xA1, [0x02])` → `GET_REPORT(0xA1)` BCD `[patch, major]` at offset `17`/`16`/`18` → `V{major}.{patch:02}`.
- `0x12` (`Load`): `SET_REPORT(0xA1, [0x12])` → `GET_REPORT(0xA0)` (1041-byte configuration).
- `0x0E` (`MouseInfo`, 4K dongle): returns paired mouse VID/PID (u16 LE) at bytes `16..20`.

## 4. Write Commands
- **Wired 8K (`!wireless4k`):** Mutate config blob in memory, set `blob[1] = 0x11` (`Store`), `SET_REPORT(0xA0, blob)`, then poll `GET_REPORT(0xA1)` for status `0x01` (OK).
- **4K Wireless (`wireless4k`):** Send `0x14` (`WriteSensor`, 28 bytes) via `SET_REPORT(0xA1)` with header `[0x0F, 28, 0, 0, 0]` and payload `[at(0x07..=0x0B), at(0x01), at(0x0E), at(0x0D), config[16+0x23..16+0x37]]`, where `at(off) = config[16 + off]`.

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/endgame-gear/op1.ts`, `src/drivers/endgame/egg-op1-hid.ts`).
