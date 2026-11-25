# MCHOSE Mouse HID Protocol Specification (V2, V3 & A5 Gen1)

**Status:** Verified from published open-source protocol codecs (`OpenMouse-Project/mouse-protocol` `src/mchose/*` & `src/drivers/mchose/*`, `alexfrih/mchose-linux`).

---

## 1. USB HID Identity & Transport Profiles

MCHOSE gaming mice span three distinct wire protocol generations:

| Family | VID | PIDs (Wired / Receiver) | Usage Page | Usage ID | Report Type & IDs | Packet Size |
| --- | --- | --- | --- | --- | --- | --- |
| **V2 (M-HUB Feature)** | `0x3837` (MCHOSE) / `0x5253` (RealTek OEM) | Wired: `0x4018` (A7 V2 Pro), `0x4023` (A7 V2 Pro+), `0x4019` (A7 V2 Ultra), `0x4021` (A7 V2 Ultra+), `0x4015` (L7 Pro+), `0x5253:0x1011` (A7 Ultra), `0x5253:0x00B0` (L7 Pro)<br>Receivers: `0x100B` (2.4G), `0x100A` (BT), `0x1020` (8K, `0x3837` & `0x5253`) | `0xFF01` | `0x0001` | Feature `0x11` (short, 20 tokens) & `0x12` (long, 64 tokens) | 65 bytes (`[report_id, body[64]]`) |
| **V3 (`0x4D` Output/Input)** | `0x3837` | Wired: `0x4035` (A5 V3 Pro), `0x4026` (A5 V3 Ultra+), `0x4034` (A5 V3 Ultra+ 3955), `0x4027` (K7 V2 Pro+), `0x4028` (K7 V2 Ultra+), `0x4030` (A7 V3), `0x4031` (A7 V3 Pro), `0x4032` (A7 V3 Pro+), `0x4033` (A7 V3 Ultra+), `0x4037` (K5 Pro), `0x4038` (K5 Ultra), `0x4036` (R7 Ultra), `0x402A` (V7), `0x4029` (G3 V3)<br>Receivers: `0x1014` (2.4G), `0x1016` (1K), `0x1018` (8K) | `0xFF01` | `0x0001` | Output / Input `0x4D` (`'M'`) | 64 bytes (`[0x4D, body[63]]`) |
| **A5 Gen1 (XVI Feature)** | `0x2023` | Wired: `0xF019` (A5 Pro Max)<br>Receivers: `0xF013` (1K), `0xF015` (4K) | `0xFFFF` | Any | Feature `0x00` | 65 bytes (`[0x00, body[64]]`) |

> **MagDock Excluded (`0x3837:0x1012`, `UP=0xFF00`):** MCHOSE MagDock uses a WCH dock-lighting protocol on `0xFF00` and is excluded from mouse matching.
> **Factory Reset Blocked:** V3 command `0x0105` (factory reset) is explicitly blocked in `GuardedTransport`'s `PacketValidator`.

---

## 2. Framing, Endianness & Checksums

### 2.1 MCHOSE V2 (`0x11` / `0x12` Bit-Inverted Feature Reports)
- **Transport:** `SET_FEATURE` / `GET_FEATURE` with 65-byte buffer `[report_id, body[0..64]]`.
- **Encoding:** Every spelled token byte `body[0..token_count]` is **bit-inverted (`byte ^ 0xFF`)**; remaining bytes stay `0x00`.
  - Short channel (`report_id = 0x11`): `token_count = 20`.
  - Long channel (`report_id = 0x12`): `token_count = 64`.
- **Reply decoding:** `command = raw[1] ^ 0xFF`, `payload[i] = raw[2 + i] ^ 0xFF`.
- **Endianness:** Multi-byte integers (VID, PID, DPI stages) are **Little-Endian (`<u16`)**.
- **Checksum:** None (handled by USB HID feature transfer).
- **Worked Example (Read Battery `0x11 0x06`):**
  - Tokens: `[0x06, 0x00, ... (20 bytes)]`
  - Wire TX: `[0x11, 0xF9, 0xFF, 0xFF, ...(18×0xFF), 0x00, ...(44×0x00)]`
  - Wire RX (`0x11, 0xF9, ~p0, ~p1, ...`): un-inverting `raw[1]` yields `0x06`, `payload[9] = battery_pct`, `payload[10] = charging`.

### 2.2 MCHOSE V3 (`0x4D` Output/Input Reports with XOR Checksum)
- **Transport:** `Output` / `Input` report `0x4D` (`64` bytes total: `[0x4D, body[0..63]]`).
- **Frame Layout (`body[0..63]`):**
  - `body[0]`: `0x01` (protocol version)
  - `body[1]`: `flags` (`0x01` = trailing XOR checksum present)
  - `body[2]`: `length` (data byte count, `0..=55`)
  - `body[3..5]`: `command` (`<u16` little-endian)
  - `body[5]`: `biz_code` (`0x00`)
  - `body[6]`: `sequence` (`0x00`)
  - `body[7..7+len]`: `data`
  - `body[7+len]`: `checksum = XOR(body[1..=6+len])`
- **Worked Example (Read Settings `0x0002`, `len = 0`):**
  - `body[0..8] = [0x01, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, xor]`
  - `xor = 0x01 ^ 0x00 ^ 0x02 ^ 0x00 ^ 0x00 ^ 0x00 = 0x03`
  - Wire TX: `[0x4D, 0x01, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x03, 0x00...]`

### 2.3 MCHOSE A5 Gen1 (`0x2023` XVI Feature Reports)
- **Transport:** `SET_FEATURE` / `GET_FEATURE` with 65-byte buffer `[0x00, body[0..64]]`.
- **Request Layout (`body[0..64]`):** `body[2] = route (0x02)`, `body[3] = len`, `body[4] = page`, `body[5] = cmd`, `body[6..] = data`.
- **Reply Layout:** `body[0] == 0xA1`, `body[4] == page`, `body[5] == cmd`, `body[6..] = data` (Big-Endian `>u16` DPI).

---

## 3. Read Commands

### V2 Read Commands (`0x11` / `0x12`)
| Command | Report ID | Tokens | Response Payload (after un-inverting) |
| --- | --- | --- | --- |
| **Identity** | `0x11` | `[0x03]` | `p[0]=bonded`, `p[1..3]=vid (<u16)`, `p[3..5]=pid (<u16)`, `p[5]=connected`, `p[6]=game_mode` |
| **Firmware Version** | `0x11` | `[0x04]` | `p[0]=len`, `p[1..1+len]=ASCII` (e.g. `"5.46.2.4"`) |
| **Battery & Model** | `0x11` | `[0x06]` | `p[0..2]=vid`, `p[2..4]=mouse_pid`, `p[4..8]=fw`, `p[8]=flags` (`(f>>4)&1`=connected), `p[9]=pct`, `p[10]=charging` |
| **Config Blob** | `0x12` | `[0x67]` | `p[0]=profile`, `p[1]=wired(rate<<4\|dpi)`, `p[2]=wireless(rate<<4\|dpi)`, `p[3]=rsv`, `p[4..16]=6×u16 LE DPI`, `p[16]=stage_count`, `p[17]=sensor` (bits `0..1`=LOD, bit `2`=ripple, bit `3`=angle_snap, bit `4`=motion_sync, bits `6..7`=mode), `p[18]=debounce_ms`, `p[19]=sleep_min`, `p[49]=angle_tuning (i8)` |
| **Profile Name** | `0x12` | `[0x68, ix]` | `p[0]=ix`, `p[1..]=NUL-terminated UTF-8` |

### V3 Read Commands (`0x4D`)
| Command | ID (LE) | Data | Response Data |
| --- | --- | --- | --- |
| **Device Info & Battery** | `0x0900` | `[]` | `d[0..2]=vid`, `d[2..4]=pid`, `d[4]=profile_count`, `d[10]=connected`, `d[11]=charging`, `d[12]=battery_pct`, `d[13]=game_mode` |
| **Firmware Version** | `0x0901` | `[0x00]` | Version bytes (target byte `0x00` required; may be rejected with cmd `0x0000` on some units) |
| **Settings Block** | `0x0002` | `[]` | `d[0]=profile`, `d[1]=wired(rate_slot<<4\|dpi_ix)`, `d[2]=wireless(rate_slot<<4\|dpi_ix)`, `d[3]=sleep_min`, `d[4]=sleep_mode`, `d[5]=sensor` (bits `0..1`=mode, bit `2`=ripple, bit `3`=angle_snap, bit `4`=motion_sync, bits `5..6`=LOD, bit `7`=glass), `d[6]=angle_tuning (i8)`, `d[7]=left_debounce_ms`, `d[8]=right_debounce_ms`, `d[9..]=extra (preserve verbatim)` |
| **DPI Table** | `0x0003` | `[profile, axis]` | `d[0]=profile`, `d[1]=axis (0=X,1=Y)`, `d[2]=stage_count`, `d[3]=active_stage`, `d[4]=has_separate_y`, `d[5..17]=6×u16 LE DPI` |
| **Lift-Off (5-step models)** | `0x0009` | `[profile]` | `d[0]=lod_index` |

- **V3 Polling Rate Wire Mapping:** Device slot `1` is skipped (`option 0 -> slot 0` = 125 Hz, `option 1 -> slot 2` = 500 Hz, `option 2 -> slot 3` = 1000 Hz, `option 3 -> slot 4` = 2000 Hz, `option 4 -> slot 5` = 4000 Hz, `option 5 -> slot 6` = 8000 Hz).

### A5 Gen1 Read Commands (`0x2023`, Feature Report `0x00`, `route = 0x02`)
| Command | `len` | `page` | `cmd` | Response Data |
| --- | --- | --- | --- | --- |
| **Firmware Version** | `16` | `0` | `0x81` | Firmware ASCII/version bytes |
| **Battery** | `2` | `0` | `0x83` | `[charging, pct]` |
| **Active Profile** | `1` | `0` | `0x85` | `[profile_ix]` (`0..=2`) |
| **Sleep Timer** | `2` | `0` | `0x87` | Sleep configuration bytes |
| **Debounce** | `2` | `0` | `0x88` | `[debounce_ms, ...]` (`0..=15`) |
| **Polling Rate** | `1` | `1` | `0x80` | `[rate_code]` |
| **DPI Table** | `10` | `1` | `0x81` | `[active_stage, stage_count, 6×u16 BE DPI]` |
| **Active DPI Stage** | `2` | `1` | `0x82` | `[stage_ix, ...]` |
| **Angle Snapping** | `1` | `1` | `0x84` | `[0/1]` |
| **Lift-Off Distance** | `1` | `1` | `0x88` | `[lod_code]` |
| **Motion Sync** | `1` | `1` | `0x89` | `[0/1]` |
| **Ripple Control** | `1` | `1` | `0x8A` | `[0/1]` |

---

## 4. Write Commands

### V2 Write Commands (`0x11` / `0x12`)
| Command | Report ID | Tokens | Effect |
| --- | --- | --- | --- |
| **Write Config Blob** | `0x12` | `[0x57, <full 0x67 read payload with edited offsets>]` | Updates active DPI stage, polling rate index, 6×DPI stages, stage count, debounce while preserving untouched bytes |
| **Set Active Profile** | `0x11` | `[0x58, profile_ix]` | Switches active onboard profile (`0..=2`) |
| **Set Performance** | `0x11` | `[0x42, lod_ix, ripple(0/1/2), linear(0/1/2), motion_sync(0/1/2), 0, 0, mode(0..3), rot_en, rot_val]` | Updates LOD and sensor flags (`0`=leave unchanged, `1`=on, `2`=off) |
| **Set Auto-Sleep** | `0x11` | `[0x0A, enabled(0/1), minutes]` | Updates auto-sleep timer |

### V3 Write Commands (`0x4D`)
| Command | ID (LE) | Data | Effect |
| --- | --- | --- | --- |
| **Write Settings Block** | `0x0102` | Full `[profile, wired, wireless, sleep, sleep_mode, sensor, angle, left_db, right_db, extra...]` | Replaces settings block (must echo freshly read `extra` bytes) |
| **Write DPI Table** | `0x0103` | `[profile, axis, sep_y, stage_count, active_stage, 6×u16 LE DPI]` | Replaces 6-stage DPI table for axis |
| **Write Single DPI Stage** | `0x0104` | `[profile, stage, axis, dpi_lo, dpi_hi]` | Updates one DPI stage |
| **Write Lift-Off (5-step)** | `0x0109` | `[profile, lod_index]` | Updates 5-step LOD index |

### A5 Gen1 Write Commands (`0x2023`, Feature Report `0x00`, `route = 0x02`)
| Command | `len` | `page` | `cmd` | Effect |
| --- | --- | --- | --- | --- |
| **Set Profile** | `1` | `0` | `0x05` | Switches active profile (`0..=2`) |
| **Set Debounce** | `2` | `0` | `0x08` | Sets switch debounce (`0..=15` ms) |
| **Set Polling Rate** | `1` | `1` | `0x00` | Sets polling rate code |
| **Set DPI Table** | `2 + 4×stages` | `1` | `0x01` | Writes encoded Big-Endian DPI table |
| **Set Active Stage** | `2` | `1` | `0x02` | Selects active DPI stage |
| **Set Angle Snapping** | `1` | `1` | `0x04` | Toggles angle snapping (`0`/`1`) |
| **Set Lift-Off Distance** | `1` | `1` | `0x08` | Sets LOD code |
| **Set Motion Sync** | `1` | `1` | `0x09` | Toggles Motion Sync (`0`/`1`) |
| **Set Ripple Control** | `1` | `1` | `0x0A` | Toggles Ripple Control (`0`/`1`) |

> **Hardware Validation Notes:**
> - Per upstream `OpenMouse-Project/mouse-protocol`, V3 write commands (`0x0102`–`0x0109`) and receiver PID `0xF015` (A5 4K) have not yet been live-tested on physical units; always validate with `--dry-run` and read-only `probe` first.
> - Over V3 wireless receivers, `0x0900` may report a generic model ID (e.g. `0x4026`), so USB product-string classification is preferred when present.

---

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/mchose/index.ts`, `src/drivers/mchose/hid.ts`): V2 bit-inverted feature reports `0x11`/`0x12`, command IDs `0x03/0x04/0x06/0x67/0x57/0x58/0x42/0x0A/0x68`, sensor bitmask layout, product table & polling lists.
- `OpenMouse-Project/mouse-protocol` (`src/mchose/v3.ts`, `src/drivers/mchose/v3-hid.ts`): V3 `0x4D` framing, XOR checksum, LE command IDs `0x0002/0x0003/0x0009/0x0900/0x0102/0x0103/0x0104/0x0109`, asymmetric rate slot mapping, swapped sensor bits, and `extra` byte preservation.
- `OpenMouse-Project/mouse-protocol` (`src/mchose/a5-gen1.ts`, `src/drivers/mchose/a5-gen1-hid.ts`): VID `0x2023` A5 Pro Max XVI feature protocol.
- `alexfrih/mchose-linux`: Hardware verification of VID `0x5253` L7 Pro (`0x00B0`) and `0x1020` 8K receiver.
