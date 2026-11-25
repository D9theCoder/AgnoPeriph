# Razer USB HID Control Protocol Specification

**Status:** Verified from open-source blueprints (`OpenMouse-Project/mouse-protocol` `src/razer/codec.ts`, `src/razer/devices.ts`, `src/drivers/razer/hid.ts`, corroborated with `openrazer/openrazer`).

## 1. USB HID Identity & Transport Profiles

- **Vendor ID:** `0x1532` (Razer Inc.)
- **Report ID:** `0x00` (unnumbered 90-byte HID Feature report; `hidapi` prepends byte `0x00` on TX for a 91-byte buffer)
- **Fixed Packet Size:** **90 bytes** payload (`[status, tx_id, rem_hi, rem_lo, proto_type, data_size, cmd_class, cmd_id, args[0..80], crc, reserved]`)
- **Control Interface:** Generic Desktop Mouse (`Usage Page 0x0001`, `Usage ID 0x0002`) or vendor collection (`>= 0xFF00`) for legacy/standard models.

### Representative Product Profiles (Wired vs. Wireless Separated)

| Model | PID | Connection Kind | Transaction ID | Polling Family | Max DPI | Battery | Lift-Off | Lighting |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Viper V3 Pro (Wired) | `0x00C0` | `WiredUsb` | `0x1F` | Legacy 1K | 35,000 | Yes | Yes | None |
| Viper V3 Pro (Wireless) | `0x00C1` | `WirelessDongle24Ghz` | `0x1F` | Extended 8K (commit `0x1F`) | 35,000 | Yes | Yes | None |
| Viper V2 Pro (Wired) | `0x00A5` | `WiredUsb` | `0x1F` | Legacy 1K | 30,000 | Yes | Yes | None |
| Viper V2 Pro (Wireless) | `0x00A6` | `WirelessDongle24Ghz` | `0x1F` | Extended 1K | 30,000 | Yes | Yes | None |
| DeathAdder V3 Pro (Wired) | `0x00B6` / `0x00C2` | `WiredUsb` | `0x1F` | Legacy 1K | 30,000 | Yes | No | None |
| DeathAdder V3 Pro (Wireless) | `0x00B7` / `0x00C3` | `WirelessDongle24Ghz` | `0x1F` | Legacy 1K (`0x00B7`) / Ext (`0x00C3`) | 30,000 | Yes | No | None |
| DeathAdder V3 (Wired) | `0x00B2` | `WiredUsb` | `0x1F` | Extended 8K (commit `0x1F`) | 30,000 | No | No | None |
| DeathAdder V4 Pro (Wired/Wireless) | `0x00BE` / `0x00BF` | `WiredUsb` / `WirelessDongle24Ghz` | `0x1F` | Extended 8K | 45,000 | Yes | No | None |
| Viper V3 HyperSpeed | `0x00B8` | `WirelessDongle24Ghz` | `0x1F` | Legacy 1K | 30,000 | Yes | Yes | None |
| Viper 8KHz | `0x0091` | `WiredUsb` | `0xFF` | Extended 8K | 20,000 | No | No | None |
| HyperPolling Wireless Dongle | `0x00B3` | `WirelessDongle24Ghz` | `0x1F` | Extended 8K (commit `0xFF`) | 30,000 | Yes | No | None |
| Basilisk V3 / V3 35K | `0x0099` / `0x00CB` | `WiredUsb` | `0x1F` | Legacy 1K | 26,000 / 35,000 | No | No | Extended Matrix |
| Basilisk V3 Pro (Wired/Wireless) | `0x00AA` / `0x00AB` | `WiredUsb` / `WirelessDongle24Ghz` | `0x1F` | Legacy 1K / Extended 1K | 30,000 | Yes | No | None |
| Naga V2 Pro (Wired/Wireless) | `0x00A7` / `0x00A8` | `WiredUsb` / `WirelessDongle24Ghz` | `0x1F` | Legacy 1K / Extended 1K | 30,000 | Yes | No | None |
| Orochi V2 | `0x0094` | `WirelessDongle24Ghz` | `0x1F` | Legacy 1K | 18,000 | Yes | No | None |
| DeathAdder V2 | `0x0084` | `WiredUsb` | `0x3F` | Legacy 1K (NOSTORE `0x00`) | 20,000 | No | No | None |
| DeathAdder Essential | `0x006E` / `0x0071` / `0x0098` | `WiredUsb` | `0x3F` | Legacy 1K | 6,400 | No | No | None |
| Diamondback Chroma | `0x004C` | `WiredUsb` | `0xFF` | Legacy 1K | 16,000 | No | No | Standard Matrix |

## 2. Framing & Checksum Algorithm

```text
Byte  0      : Status (0x00 on request; 0x01=Busy, 0x02=OK, 0x03=Failure, 0x04=Timeout, 0x05=Unsupported)
Byte  1      : Transaction ID (per-product: 0x1F, 0x3F, or 0xFF)
Bytes 2..3   : Remaining packets (0x00, 0x00)
Byte  4      : Protocol type (0x00)
Byte  5      : Data size (0..=80)
Byte  6      : Command class
Byte  7      : Command ID (bit 7 = 1 for read/getter, bit 7 = 0 for write/setter)
Bytes 8..87  : Command arguments (80 bytes, zero-padded)
Byte  88     : XOR Checksum
Byte  89     : Reserved (0x00)
```

### Checksum Formula
```text
crc = packet[2] ^ packet[3] ^ ... ^ packet[87]
```

**Worked Example (`ReadFirmware`, class `0x00`, cmd `0x81`, data_size `0x02`, tx `0x1F`):**
- Bytes `0..8`: `00 1F 00 00 00 02 00 81`
- Bytes `8..88`: all `0x00`
- XOR of bytes `2..88`: `0x00 ^ 0x00 ^ 0x00 ^ 0x02 ^ 0x00 ^ 0x81 = 0x83`
- Byte `88` = `0x83`, Byte `89` = `0x00` ✓

## 3. Read Commands

| Class | Cmd ID | Data Size | Args | Response Args |
| --- | --- | --- | --- | --- |
| `0x00` | `0x81` | `0x02` | `[]` | `[major, minor]` → `v{major}.{minor}` |
| `0x00` | `0x82` | `0x16` | `[]` | NUL-terminated ASCII serial string |
| `0x07` | `0x80` | `0x02` | `[]` | `[0x00, raw_0_255]` → `round(raw * 100 / 255)` % |
| `0x07` | `0x84` | `0x02` | `[]` | `[0x00, charging]` (`1` = Charging, `0` = Discharging) |
| `0x04` | `0x85` | `0x07` | `[storage]` | `[storage, x_hi, x_lo, y_hi, y_lo, 0, 0]` |
| `0x04` | `0x86` | `0x26` | `[storage]` | `[storage, active_1based, count, 5 × (idx, x_hi, x_lo, y_hi, y_lo, 0, 0)]` |
| `0x00` | `0x85` | `0x01` | `[]` | `[divisor]` → `1000 / divisor` Hz |
| `0x00` | `0xC0` | `0x02` | `[0x00]` | `[0x00, divisor]` → `8000 / divisor` Hz |
| `0x0B` | `0x85` | `0x05` | `[]` | `[0, 0, tracking (0=Low,1=Med,2=High), lift_off-1, landing-1]` |
| `0x03` | `0x83` | `0x03` | `[0x01, 0x05]` | `[0x01, 0x05, raw_0_255]` (Standard Matrix backlight brightness) |
| `0x0F` | `0x84` | `0x03` | `[0x01, led]` | `[0x01, led, raw_0_255]` (Extended Matrix LED brightness) |

## 4. Write Commands

| Class | Cmd ID | Data Size | Args | Effect |
| --- | --- | --- | --- | --- |
| `0x04` | `0x05` | `0x07` | `[storage, x_hi, x_lo, y_hi, y_lo, 0, 0]` | Sets active X/Y DPI |
| `0x04` | `0x06` | `0x26` | `[storage, active_1based, count, stages...]` | Writes complete DPI stage table (1..=5 stages) |
| `0x00` | `0x05` | `0x01` | `[1000 / hz]` | Sets legacy polling rate (125/500/1000 Hz) |
| `0x00` | `0x40` | `0x02` | `[selector, 8000 / hz]` | Sets extended polling rate (`selector=0x00`, optional commit `0x01`) |
| `0x0B` | `0x0B` | `0x04` | `[0x00, 0x04, 0x01, level]` | Sets symmetric tracking distance (`0`=Low, `1`=Medium, `2`=High) |
| `0x03` | `0x0A` | `1..=8` | `[effect, ...]` | Sets Standard Matrix lighting effect (`0`=Off, `1`=Wave, `2`=Reactive, `3`=Breathing, `4`=Spectrum, `6`=Static) |
| `0x03` | `0x03` | `0x03` | `[0x01, 0x05, val]` | Sets Standard Matrix brightness (`0..=255`) |
| `0x0F` | `0x02` | `6..=12` | `[0x01, led, effect, ...]` | Sets Extended Matrix lighting (`led`: `0x00`=All, `0x01`=Scroll, `0x04`=Logo) |
| `0x0F` | `0x04` | `0x03` | `[0x01, led, val]` | Sets Extended Matrix LED brightness (`0..=255`) |

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol`: `src/razer/codec.ts`, `src/razer/devices.ts`, `src/drivers/razer/hid.ts` (hardware-verified on Viper V3 Pro `0x00C0`/`0x00C1`, Viper V2 Pro `0x00A5`/`0x00A6`, DeathAdder V3 Pro `0x00B6`/`0x00B7`, Viper V3 HyperSpeed `0x00B8`, Naga V2 Pro `0x00A7`/`0x00A8`, Diamondback Chroma `0x004C`).
- `openrazer/openrazer`: `driver/razercommon.h`, `driver/razermouse_driver.c`.
