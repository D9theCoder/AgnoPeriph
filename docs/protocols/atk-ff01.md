# ATK68 / ATK75 (`0xFF01` GTech) Hall Effect Keyboard Protocol Specification

**Status:** Verified from published open-source implementation `bsdrop/atk68` (`PROTOCOL.md`, `src/device.rs`, `src/actuation.rs`, `src/light.rs`, `src/tweaks.rs`), hardware-verified on ATK68 firmware 1.25.

---

## 1. USB HID Identity & Transport Profile

- **Usage Page:** `0xFF01` (vendor configuration interface)
- **Report ID:** `0x00` (unnumbered 64-byte Output/Input reports; `65` bytes TX on Windows `hidapi` with leading `0x00`)
- **Feature Reports:** Not used (`GET_FEATURE` times out)
- **Checksum:** None

### Verified Model Table (`bsdrop/atk68` `src/device.rs` `MODELS`)
| VID | PID | Model | Actuation Step |
| --- | --- | --- | --- |
| `0x1A81` | `0x207E` | ATK68 | `0.10 mm` (`10`) |
| `0x1A81` | `0x2075` | ATK75 G | `0.10 mm` (`10`) |
| `0x1A81` | `0x2082` | ATK75 L | `0.10 mm` (`10`) |
| `0x1A81` | `0x2083` | ATK68 L | `0.02 mm` (`2`) |
| `0x373B` | `0x1005` | ATK68 Air | `0.02 mm` (`2`) |
| `0x373B` | `0x2029` | ATK68 V2 | `0.10 mm` (`10`) |
| `0x373B` | `0x1259` | ATK68 V4 | `0.10 mm` (`10`) |
| `0x373B` | `0x21B8` | ATK68 V4 | `0.10 mm` (`10`) |
| `0x373B` | `0x21C6` | ATK68 RX | `0.10 mm` (`10`) |
| `0x373B` | `0x2169` | ATK EDGE75 | `0.10 mm` (`10`) |
| `0x373B` | `0x21BC` | EDGE 63HE Ultimate | `0.10 mm` (`10`) |
| `0x373B` | `0x2188` | ATK75 V2 | `0.10 mm` (`10`) |
| `0x373B` | `0x21C9` | ATK RS7 Turbo | `0.10 mm` (`10`) |
| `0x373B` | `0x1177` | ATK x QK Hex80 | `0.10 mm` (`10`) |

> **Safety Gate:** `ResetStorage` (`0x22`) and bootloader jump (`0xAA`) are explicitly blocked in `GuardedTransport`'s `PacketValidator`.

---

## 2. Wire Framing & Worked Example

- **TX (`65` bytes on Windows `hidapi`):** `[0x00 (Report ID), cmd, len, body[0..62]]`
- **RX (`64` bytes):** `[cmd, len, result_code (0x00=OK, 0xFF=error), data[0..len]]`
  - Unsolicited async notifications arrive with `rx[0] == 0x48` (`DataReported`) and are skipped while waiting for `cmd`.
- **Worked Example (Get Global Actuation `0x43`):**
  - TX: `[0x00, 0x43, 0x00, 0x00, ... (zero-padded to 65 bytes)]`
  - RX: `[0x43, 0x03, 0x00, 0x0A, 0x02, 0x02, ...]` → `point = 10` (`1.00 mm` at `0.10 mm` step), `rt_down = 2` (`0.20 mm`), `rt_up = 2` (`0.20 mm`).

---

## 3. Read Commands

| Command | ID | Request Body `[len, ...]` | Reply Data (`rx[3..3+len]`) |
| --- | --- | --- | --- |
| **GetKeyboardInfo** | `0x12` | `[0, 0]` | `[vid_hi, vid_lo, pid_hi, pid_lo, _, _, ver_maj, ver_min]` (`"{d[6]}.{:02x}"`) |
| **GetReportRate** | `0x23` | `[0, 0]` | `[index]` (`0..=6` → `125, 250, 500, 1000, 2000, 4000, 8000 Hz`) |
| **GetDeviceProfile** | `0x2B` | `[0, 0]` | `[profile_index]` (`0..=3`) |
| **GetAllActuation** | `0x43` | `[0, 0]` | `[point, down, up]` (units of `model.step`, max `4.00 mm`) |
| **GetLightConfig** | `0x45` | `[0, 0]` | `[enabled, mode, brightness (0..=8), speed (0..=4), r, g, b, colorful]` |
| **GetOneFastTrigger** | `0x54` | `[2, row, col]` | `[trigger]` (`result_code 0xFF` for unused matrix slot) |

### Lighting Mode IDs (`0x45` / `0x44`)
`0`=Spectrum Cycle, `1`=Static, `2`=Wave L→R, `3`=Breathing, `4`=Snake, `5`=Reactive, `6`=Rotate, `7`=Wave-to-Centre, `8`=Fountain, `9`=Laser, `10`=Custom.

---

## 4. Write Commands

| Command | ID | Request Body `[len, ...]` | Effect |
| --- | --- | --- | --- |
| **SaveStorage** | `0x21` | `[0, 0]` | Commits RAM settings to onboard flash |
| **SetReportRate** | `0x24` | `[1, index]` | Sets USB polling rate (`0..=6`) |
| **SetDeviceProfile** | `0x2C` | `[1, index]` | Selects active onboard profile |
| **SetAllActuation** | `0x42` | `[3, point, down, up]` | Sets global actuation point and Rapid Trigger press/release |
| **SetLightConfig** | `0x44` | `[8, enabled, mode, brightness, speed, r, g, b, 0, colorful]` | Sets main backlight state |
| **SetOneFastTrigger** | `0x55` | `[3, row, col, value]` | Sets per-key Rapid Trigger sensitivity |

---

## 5. Evidence Sources
- `bsdrop/atk68`: `PROTOCOL.md`, `src/device.rs` (`MODELS`, `op`, `exchange`, `REPORT_RATE_HZ`), `src/actuation.rs`, `src/light.rs`.
