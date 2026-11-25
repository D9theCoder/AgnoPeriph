# Finalmouse UltralightX & Starlight X USB HID Protocol Specification

**Status:** Verified from open-source blueprints (`OpenMouse-Project/mouse-protocol` `src/finalmouse/index.ts`, `src/drivers/finalmouse/hid.ts`, derived from Finalmouse `xpanel` WebHID).

## 1. USB HID Identity & Transport Profiles

- **Vendor ID:** `0x361D` (Finalmouse)
- **Usage Page:** `0xFF00` (ULX pins `Usage ID 0x0001`; Starlight X matches `Usage Page 0xFF00`)
- **Fixed Packet Size (including Report ID):** **64 bytes** (`1` byte Report ID + `63` byte payload)

| Transport Profile | VID | PID | Connection Kind | TX Report ID | RX Report ID | Dongle TX / RX | Polling Rates |
| --- | --- | --- | --- | --- | --- | --- | --- |
| UltralightX Dongle | `0x361D` | `0x0100` | `WirelessDongle24Ghz` | `0x04` | `0x05` | `0x02` / `0x03` | 500, 1000, 2000, 4000, 8000 Hz |
| UltralightX Wired | `0x361D` | `0x0102` | `WiredUsb` | `0x04` | `0x05` | `0x02` / `0x03` | 500, 1000, 2000, 4000, 8000 Hz |
| Starlight X Dongle | `0x361D` | `0x0300` / `0x0301` | `WirelessDongle24Ghz` | `0x01` | `0x02` | `0x01` / `0x02` | Fixed PerfectPolling |
| Starlight X Wired | `0x361D` | `0x0310` / `0x0311` | `WiredUsb` | `0x01` | `0x02` | `0x01` / `0x02` | Fixed PerfectPolling |

## 2. Packet Framing

### Host → Device (64 bytes total)
```text
Byte  0     : Report ID (0x04 ULX mouse, 0x02 ULX dongle, 0x01 SLX)
Byte  1     : Inner length = 2 + payload.len()
Byte  2     : 0x80 | command_id  (command_id in 0..=0x7F)
Byte  3     : payload.len()      (0..=60)
Bytes 4..63 : Payload bytes (zero-padded)
```

### Device → Host (64 bytes total, or 63 bytes when Report ID stripped)
```text
Byte  0     : Report ID (0x05 ULX mouse, 0x03 ULX dongle, 0x02 SLX)
Byte  1     : Inner length
Byte  2     : Command / telemetry ID (without 0x80 bit)
Byte  3     : Payload length
Bytes 4..   : Payload bytes (little-endian integers, ASCII strings)
```

**Worked Example (`SetDpi` = 800 = `0x0320` LE on ULX, report `0x04`, cmd `16 = 0x10`):**
- `04 04 90 02 20 03 00 ... 00` (64 bytes total)
- `Byte 1 = 2 + 2 = 0x04`, `Byte 2 = 0x80 | 0x10 = 0x90`, `Byte 3 = 0x02`, `Bytes 4..6 = 0x20 0x03` ✓

## 3. Read / Telemetry Commands

Sending a command with `payload.len() == 0` queries its value; `wakeAll (96)` triggers a burst of status telemetry frames.

| Command / Telemetry ID | Name | Payload (RX) |
| --- | --- | --- |
| `96` (`0x60`) | `WakeAll` (TX query) | Triggers telemetry burst |
| `10` (`0x0A`) | `DongleInfo` (on dongle report) | ASCII dongle USB firmware |
| `3` / `16` | `Dpi` | `u16` LE (50..=26,000 DPI) |
| `4` / `17` | `PollingRate` | `u16` LE Hz |
| `5` | `BatteryVoltage` | `u16` LE millivolts (mapped via xpanel voltage curve `3.00V..4.38V`) |
| `12` | `MouseFirmware` | ASCII firmware string |
| `14` | `MouseSerial` | ASCII serial number |
| `18` | `MotionSync` | `u8` (`0` = off, `1` = on) |
| `21` | `LiftOffDistance` | `u8` (`1` = 1.0 mm, `2` = 2.0 mm) |
| `25` | `PawLod` (SLX) | `u8` (`1`=0.7mm, `2`=1.0mm, `3`=2.0mm, `7..=17`=`raw/10` mm) |
| `36` | `LinkState` | `u8` (`0` = disconnected, `1` = linked) |
| `37` | `ChargingState` | `u8` (`1` = charging) |
| `38` (`0x26`) | `BatterySoc` | `[soc_0_100, mv_lo, mv_hi]` |
| `57` | `Profile` (SLX) | `[active_0based, count]` |

## 4. Write Commands

| Command ID | Name | Payload (TX) | Effect |
| --- | --- | --- | --- |
| `16` (`0x10`) | `SetDpi` | `u16` LE (`50..=26000`) | Sets active DPI |
| `17` (`0x11`) | `SetPollingRate` | `u16` LE (`500..=8000`) | Sets ULX polling rate |
| `18` (`0x12`) | `SetMotionSync` | `[0\|1]` | Toggles Motion Sync |
| `21` (`0x15`) | `SetLiftOffDistance` | `[1\|2]` | Sets ULX LOD (1 mm / 2 mm) |
| `25` (`0x19`) | `SetPawLod` (SLX) | `[raw]` | Sets SLX PAW LOD |
| `57` (`0x39`) | `SetProfile` (SLX) | `[index]` | Selects SLX active profile (`0..count`) |

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/finalmouse/index.ts`, `src/drivers/finalmouse/hid.ts`, `src/drivers/finalmouse/protocol.test.ts`).
