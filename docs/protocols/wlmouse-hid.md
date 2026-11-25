# WLmouse USB HID Protocol Specification (CompX Page/Command 64-Byte)

**Status:** Verified from open-source blueprints (`OpenMouse-Project/mouse-protocol` `src/compx/codec.ts`, `src/wlmouse/index.ts`, `src/drivers/wlmouse/hid.ts`).

## 1. USB HID Identity & Transport Profiles

- **Vendor ID:** `0x36A7` (WLmouse)
- **Report ID:** `0x00` (64-byte HID Feature report; `hidapi` prepends `0x00` for a 65-byte buffer)
- **Fixed Packet Size:** **64 bytes** (`[status, 0x00, target, length, page, command, args[0..58]]`)
- **Excluded PID:** `0xA887` (Beast X 4K uses a distinct Report-4 protocol and is explicitly excluded from this table).

| Model | Wireless PID | Wired PID | Max Polling |
| --- | --- | --- | --- |
| WLmouse Beast G | `0xA860` | `0xA861` | 8000 Hz |
| WLmouse Huan | `0xA863` | `0xA864` | 8000 Hz |
| WLmouse Beast Miao | `0xA866` | `0xA867` | 8000 Hz |
| WLmouse Beast Mini Pro | `0xA868` | `0xA869` | 8000 Hz |
| WLmouse Beast X Pro | `0xA870` | `0xA871` | 8000 Hz |
| WLmouse Strider | `0xA872` | `0xA873` | 8000 Hz |
| WLmouse Ying | `0xA874` | `0xA875` | 8000 Hz |
| WLmouse Sword X | `0xA878` | `0xA879` | 8000 Hz |
| WLmouse Beast Max | `0xA880` | `0xA881` | 8000 Hz |
| WLmouse 1K Receiver | `0xA882` | — | 1000 Hz |
| WLmouse Beast X | `0xA883` | `0xA884` | 8000 Hz |
| WLmouse Beast Mini | `0xA885` | `0xA886` | 8000 Hz |

## 2. Framing

```text
Byte  0     : Status (0x00=Request, 0xA0=Pending, 0xA1=OK, 0xA2=Unsupported, 0xA3=Busy)
Byte  1     : Reserved (0x00)
Byte  2     : Target (0x00=Dongle, 0x01=Pairing, 0x02=Mouse)
Byte  3     : Declared payload length (0..=58)
Byte  4     : Page (0x00=Device, 0x01=Profile, 0x03=Buttons)
Byte  5     : Command (bit 7 = 1 for read, bit 7 = 0 for write)
Bytes 6..63 : Arguments / response payload (zero-padded)
```

**Worked Example (`ReadBattery`, target `0x02`, len `0x02`, page `0x00`, cmd `0x83`):**
- `00 00 02 02 00 83 00 ... 00` (64 bytes; 65 bytes with leading `0x00` Report ID for `hidapi`)
- Reply: `A1 00 02 02 00 83 [charging] [percent] 00 ...` ✓

## 3. Read Commands

| Target | Page | Cmd | Len | Args | Response Payload |
| --- | --- | --- | --- | --- | --- |
| `0x02` | `0x00` | `0x81` | `0x10` | `[]` | Firmware (`payload[2].payload[3]`) |
| `0x02` | `0x00` | `0x83` | `0x02` | `[]` | `[charging (0/1), percent (0..=100)]` |
| `0x02` | `0x00` | `0x85` | `0x01` | `[]` | `[active_profile_1based]` (`1..=4`) |
| `0x02` | `0x00` | `0x88` | `0x02` | `[profile]` | `[profile, debounce_ms]` (`0..=15`) |
| `0x02` | `0x01` | `0x80` | `0x02` | `[profile]` | `[profile, polling_code]` (`0x08`=125, `0x04`=250, `0x02`=500, `0x01`/`0x10`=1000, `0x20`=2000, `0x40`=4000, `0x80`=8000) |
| `0x02` | `0x01` | `0x81` | `0x0A` | `[profile, 6]` | `[profile, count, count × (x_hi, x_lo, y_hi, y_lo)]` |
| `0x02` | `0x01` | `0x82` | `0x02` | `[profile]` | `[profile, active_stage_1based]` |
| `0x02` | `0x01` | `0x84` | `0x02` | `[profile]` | `[profile, angle_snapping (0/1)]` |
| `0x02` | `0x01` | `0x88` | `0x02` | `[profile]` | `[profile, lod]` (`0x87`=0.7mm, `0x01`=1.0mm, `0x02`=2.0mm) |
| `0x02` | `0x01` | `0x89` | `0x02` | `[profile]` | `[profile, motion_sync (0/1)]` |
| `0x02` | `0x01` | `0x8A` | `0x02` | `[profile]` | `[profile, ripple_control (0/1)]` |

## 4. Write Commands

| Target | Page | Cmd | Args | Effect |
| --- | --- | --- | --- | --- |
| `0x02` | `0x00` | `0x08` | `[profile, debounce_ms]` | Sets click debounce (`0..=15` ms) |
| `0x02` | `0x01` | `0x00` | `[profile, polling_code]` | Sets polling rate |
| `0x02` | `0x01` | `0x01` | `[profile, count, stages...]` | Writes DPI stages (`50..=30000`, step 50, up to 6 stages) |
| `0x02` | `0x01` | `0x02` | `[profile, stage_1based]` | Selects active DPI stage |
| `0x02` | `0x01` | `0x04` | `[profile, 0\|1]` | Sets angle snapping |
| `0x02` | `0x01` | `0x08` | `[profile, lod_code]` | Sets LOD (`0x87`=0.7mm, `0x01`=1.0mm, `0x02`=2.0mm) |
| `0x02` | `0x01` | `0x09` | `[profile, 0\|1]` | Sets Motion Sync |
| `0x02` | `0x01` | `0x0A` | `[profile, 0\|1]` | Sets Ripple Control |

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/compx/codec.ts`, `src/wlmouse/index.ts`, `src/drivers/wlmouse/hid.ts`, `src/drivers/vendors.ts`).
