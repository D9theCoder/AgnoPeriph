# Wooting Analog Keyboard USB HID Protocol Specification

**Status:** Verified from official open-source SDK (`WootingKb/wooting-rgb-sdk` `src/wooting-usb.h`, `src/wooting-usb.c`) and `OpenMouse-Project/mouse-protocol` (`src/wooting/index.ts`, `src/drivers/wooting/hid.ts`).

## 1. USB HID Identity & Transport Profiles

- **Vendor ID:** `0x31E3` (Wooting Technologies B.V. — ARM / Hall Effect generation)
- **Config Interface:** `Usage Page 0xFF55`, `Usage ID 0x0001` (v3 / ARM modern) or `Usage Page 0x1337` (legacy v1/v2)
- **Feature Command Packet Size:** **8 bytes** (`[report_index, magic0, magic1, command_id, param3, param2, param1, param0]`)

| Model | Base PID (+alt `0x0`/`0x1`/`0x2`) | Rows × Cols | Device Type |
| --- | --- | --- | --- |
| Wooting Two HE | `0x1220..=0x1222` | 6 × 21 | Full-size HE |
| Wooting Two HE (ARM) | `0x1230..=0x1232` | 6 × 21 | Full-size HE (ARM) |
| Wooting 60HE | `0x1300..=0x1302` | 6 × 14 | 60% HE |
| Wooting 60HE (ARM) | `0x1310..=0x1312` | 6 × 14 | 60% HE (ARM) |
| Wooting 60HE+ | `0x1320..=0x1322` | 6 × 14 | 60% HE+ |
| Wooting 60HE V2 | `0x1340..=0x1342` | 6 × 14 | 60% HE V2 |
| Wooting 80HE | `0x1400..=0x1402` | 6 × 17 | 80% HE |
| Wooting UwU / UwU RGB | `0x1500..=0x1502` / `0x1510..=0x1512` | 5 × 7 | 3-key Keypad |

## 2. Command Framing

### Host → Device (8-byte HID Feature Report)
```text
Byte 0 : Report index (0x00 single-report, 0x01 multi-report)
Byte 1 : Magic0 (0xD0 single-report, 0xD1 multi-report)
Byte 2 : Magic1 (0xDA)
Byte 3 : Command ID
Byte 4 : param3 (reversed parameter order per wooting-usb.c)
Byte 5 : param2
Byte 6 : param1
Byte 7 : param0
```

### Device → Host (Input Report Reply)
```text
Byte 0   : Magic0 (0xD0 or 0xD1)
Byte 1   : Magic1 (0xDA)
Byte 2   : Echoed Command ID
Byte 3   : Status (0x88 = OK)
Bytes 4..5 : Payload length (u16 LE) or command-specific header
Bytes 6..  : Command-specific payload
```

**Worked Example (`GetVersion`, cmd `0x01`, multi-report):**
- TX Feature Report (8 bytes): `01 D1 DA 01 00 00 00 00`
- Hardware-verified RX (Wooting 60HE+): `D1 DA 01 88 03 00 02 0D 00 ...` → `v2.13.0` (`major=2, minor=13, patch=0`) ✓

## 3. Read Commands (Verified)

| Command ID | Name | Params | Response Payload |
| --- | --- | --- | --- |
| `0x01` | `GetVersion` | `0, 0, 0, 0` | `bytes[6..9] = [major, minor, patch]` → `v{major}.{minor}.{patch}` |
| `0x03` | `GetSerial` | `0, 0, 0, 0` | Serial metadata bytes |
| `0x0B` | `GetCurrentKeyboardProfileIndex` | `0, 0, 0, 0` | Active onboard profile index at `bytes[4]` (`0..=3`) |
| `0x13` | `GetDeviceConfig` | `0, 0, 0, 0` | Layout enum at offset `9` (`0`=ANSI, `1`=ISO, `2`=JIS, `3`=ANSI Split, `4`=ISO Split) |
| `0x27` | `GetKeyboardProfile` | `p3=0xFF, p2=0xFF, p1=0, p0=0` | Protobuf body (length at `bytes[4..6]` LE): nested message field 1 → field 1 = actuation (`raw = 16384 + mm * 4096`, `0x4000`=0.00mm, `0x8000`=4.00mm) |
| `0x33` | `GetSettings` | `0, 0, 0, 0` | Global settings protobuf (nested field 1: field 1 = actuation raw, field 2 = RT sensitivity raw) |

## 4. Write Commands (Verified from Official `wooting-rgb-sdk`)

> **Safety Gate:** Destructive/bootloader commands (`0x02` reset_to_bootloader, `0x15` keys_off, `0x19` do_soft_reset) are strictly **blocked** by `PacketValidator`. Profile/actuation write opcodes are not transmitted because they are not verified in the public SDK; `HallEffectKeyboard` exposes read-only actuation inspection and returns `UnsupportedCapability` on mutating actuation calls.

| Command ID | Name | Params (`p0, p1, p2, p3`) | Effect |
| --- | --- | --- | --- |
| `0x1E` (`30`) | `SingleColor` | `p3=matrix_id, p2=rgb565_lo, p1=rgb565_hi, p0=0` (or SDK order) | Sets one LED color directly (RGB565) |
| `0x20` (`32`) | `ResetAll` | `0, 0, 0, 0` | Restores hardware RGB profile |
| `0x21` (`33`) | `ColorInit` | `0, 0, 0, 0` | Initializes direct RGB array mode |

## 5. Evidence Sources
- `WootingKb/wooting-rgb-sdk`: `src/wooting-usb.h`, `src/wooting-usb.c` (VID `0x31E3`, PID table, `WOOTING_COMMAND_SIZE=8`, `0xD0`/`0xD1` `0xDA` magic, `WOOTING_DEVICE_CONFIG_COMMAND=19`, RGB commands `30..33`).
- `OpenMouse-Project/mouse-protocol`: `src/wooting/index.ts`, `src/drivers/wooting/hid.ts` (hardware-confirmed 60HE+ `v2.13.0`, `get_keyboard_profile` `0x27` protobuf formula `raw = 16384 + mm * 4096`).
