# ATK / VGN / VXE Mouse Protocol Specification (`0x08` EEPROM Framing)

**Status:** Verified from published open-source protocol codecs (`OpenMouse-Project/mouse-protocol` `src/atk/index.ts`, `src/vgn/index.ts`, `src/drivers/atk/products.ts`, `src/drivers/atk/hid.ts`, `src/drivers/vgn/hid.ts`, `src/drivers/atk/device-catalog.generated.ts`).

---

## 1. USB HID Identity & Transport Profiles

ATK, VXE, and VGN gaming mice share a 16-byte Output/Input HID configuration protocol on Report ID `0x08` (`17` bytes TX on Windows `hidapi` including Report ID `0x08`) over Usage Page `0xFF02`, Usage ID `0x0002`.

| Brand / Family | VID | Representative PIDs | Connection | Sensor Family | Max Polling |
| --- | --- | --- | --- | --- | --- |
| **ATK F1 / X1 / Z1 / U2 / A9 / Y9 / Mad R** | `0x373B` | `0x1014` (F1 Pro), `0x1015` (F1 Pro Max), `0x1016` (X1 Pro), `0x1017` (X1 Pro Max), `0x1018` (Z1 Pro), `0x1019` (Z1 Pro Max), `0x102E` (F1 Ultimate), `0x102F` (X1 Ultra), `0x1030` (Fierce X), `0x103C` (F1 Ultra), `0x103D` (X1 Ultimate), `0x103E` (Mad R Major), `0x103F` (Mad R), `0x1045` (F1 Extreme), `0x104A` (Z1 Ultra), `0x104B` (Mad R+), `0x104C` (Mad R Major+), `0x1086` (U2 Mini), `0x1087` (U2 Ultimate), `0x10AC` (U2 Ultra), `0x10C9` (NearLink Dongle), `0x10D8` (X1), `0x10E9` (X1 Extreme), `0x10EC` (U2 Pro), `0x1115` (A9 Plus NK), `0x1117` (A9 Pro Max), `0x1119` (A9 Ultra), `0x112D` (A9 Pro) | Wired / 2.4 GHz / NearLink | `PAW3950Ultra` (10–42K), `PAW3950` (50–36K), `PAW3395` / `PAW3395Ultra` (100–30K), `PAW3395SE` (200–18K), `PAW3311` (200–15K), `PAW3955Master` (50–40K) | 1K–8K Hz |
| **VXE R1 (COMPX / Beken)** | `0x3554` / `0x373B` | `0x3554:0xF58A` (R1 Pro Max 4K Receiver), `0x3554:0xF58C` (R1 Pro / Pro Max Wired), `0x3554:0xF58E` (R1 Receiver), `0x3554:0xF58F` (R1 / R1 SE / SE+ Wired), `0x373B:0x1085` (R1 SE/SE+ 1K Dongle), `0x373B:0x1065`/`0x1066` (R1 NK) | Wired / 2.4 GHz Dongle | `PAW3395` / `PAW3395SE` | 1K–4K Hz |
| **ATK Dragonfly F1 (COMPX)** | `0x3554` | `0xF502` (F1 Pro), `0xF506` (F1 MOBA / Pro Max), `0xF50A` (F1S) | Wired / 2.4 GHz Dongle | `PAW3395` | 4K Hz |
| **VGN Dragonfly F2 Master+** | `0x3554` | `0xFB56` (8K Receiver), `0xFB57` (Wired) | Wired / 2.4 GHz Dongle | `PAW3395` / 26K (`50..=26000` step 50) | 8K Hz |

> **Safety Gate:** Bootloader / upgrade commands (`0x09` restoreFactory, `0x0D` enterUsbUpgradeMode) are explicitly rejected by `GuardedTransport`'s `PacketValidator`.

---

## 2. Framing, Checksum & Encodings

### Wire Framing (17-byte `hidapi` TX / 16- or 17-byte RX)
```text
TX: [0x08 (Report ID), cmd, 0x00, addr_hi, addr_lo, len, d0..d9, checksum]
```
- **Frame Checksum (`payload[15]`):** `(0x55 - ((0x08 + sum(payload[0..15])) & 0xFF)) & 0xFF` so `(0x08 + sum(payload[0..16])) & 0xFF == 0x55`.
- **Scalar Pair Checksum (in EEPROM):** Each single-byte setting is stored as 2 bytes `[val, (0x55 - val) & 0xFF]`.
- **Worked Example (Read Battery `cmd = 0x04`):**
  - `payload[0..15] = [0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]`
  - `sum = 0x08 + 0x04 = 0x0C` → `checksum = (0x55 - 0x0C) & 0xFF = 0x49`
  - TX (17 bytes): `08 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00 49`

### Polling Rate Codes (EEPROM `0x0000`)
| Wire Byte | Polling Rate |
| --- | --- |
| `0x08` | `125 Hz` |
| `0x04` | `250 Hz` |
| `0x02` | `500 Hz` |
| `0x01` | `1000 Hz` |
| `0x10` | `2000 Hz` |
| `0x20` | `4000 Hz` |
| `0x40` | `8000 Hz` |

### DPI Stage Encoding (4 bytes `[x_lo, y_lo, mode, chk]` at `0x000C + stage * 4`, sum = `0x55`)
- **Step50 (`PAW3395` / `PAW3950` / `VGN F2`):**
  - `doubled = dpi > 30000`, `count = dpi / (doubled ? 100 : 50) - 1`
  - `byte = count & 0xFF`, `nibble = (((count >> 8) & 0x03) << 2) | (doubled ? 1 : 0)`
  - `mode = (y_nibble << 4) | x_nibble`, `chk = (0x55 - x_byte - y_byte - mode) & 0xFF`.
- **Ultra (`PAW3950Ultra`):**
  - `value <= 10000`: `code = value / 10 - 1`, `nibble = ((code >> 8) << 2) | doubled`
  - `value > 10000`: `code = (value - 10050) / 50`, `nibble = ((code >> 8) << 2) | 2 | doubled`.
- **Stepped (`PAW3311` / `PAW3315`):** `count = (dpi - min) / step`, `nibble = ((count >> 8) & 0x03) << 2`.

---

## 3. Read & Write Commands

| Command | ID | Payload `p[2..5]` | Response / Effect |
| --- | --- | --- | --- |
| **Online Status** | `0x03` | `[0, 0, 0]` | `p[5]`: online flag |
| **Battery** | `0x04` | `[0, 0, 0]` | `p[5]=percent`, `p[6]=charging (1)`, `p[7..9]=voltage_mv (>u16)` |
| **Write EEPROM** | `0x07` | `[addr_hi, addr_lo, len]` + `p[5..5+len]` | Writes `1..=10` bytes to configuration EEPROM |
| **Read EEPROM** | `0x08` | `[addr_hi, addr_lo, len]` | Returns `len` (`1..=10`) bytes at `p[5..5+len]` |
| **Get Current Profile** | `0x0E` | `[0, 0, 0]` | `p[5]=profile_index (0..3)` |
| **Set Current Profile** | `0x0F` | `[0, 0, 1]` + `p[5]=profile` | Switches onboard profile (`0..=3`) |
| **Get Mouse CID/MID** | `0x10` | `[0, 0, 0]` | `p[5]=cid`, `p[6]=mid` (model disambiguation) |
| **Firmware Version** | `0x12` | `[0, 0, 0]` | Mouse firmware version bytes |

### EEPROM Register Map
| Address | Length | Field |
| --- | --- | --- |
| `0x0000` | 2 | Polling rate scalar pair |
| `0x0002` | 2 | DPI stage count scalar pair (`1..=8`) |
| `0x0004` | 2 | Active DPI stage scalar pair (`0..count`) |
| `0x000A` | 2 | Lift-Off Distance scalar pair (`3`=Low/0.7mm, `1`=Medium/1.0mm, `2`=High/2.0mm on VGN/X1; or `(mm*10)-6` on A9) |
| `0x000C` | `4 × N` | DPI stage table (`N` up to 6 or 8 stages) |
| `0x00A9` | 2 | Switch click debounce (ms) scalar pair |
| `0x00AB` | 2 | Motion Sync (`0`/`1`) scalar pair |
| `0x00AD` | 2 | Auto-sleep timeout (`× 10 s`) scalar pair |
| `0x00AF` | 2 | Angle Snapping / Linear Correction (`0`/`1`) scalar pair |
| `0x00B1` | 2 | Ripple Control (`0`/`1`) scalar pair |

---

## 4. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/atk/index.ts`, `src/vgn/index.ts`, `src/drivers/atk/products.ts`, `src/drivers/atk/hid.ts`, `src/drivers/vgn/hid.ts`, `src/drivers/atk/device-catalog.generated.ts`).
