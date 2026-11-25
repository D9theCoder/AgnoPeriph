# Lamzu Atlantis (CompX Report-8) & Aurora Protocol Specification

**Status:** Verified from open-source blueprints (`OpenMouse-Project/mouse-protocol` `src/lamzu/atlantis.ts` & `src/compx/codec.ts`, `LeadSun/lamzu-cfg` `src/atlantis.rs` & `src/atlantis/hid.rs`).

## 1. USB HID Identity & Transport Profiles

### Lamzu Atlantis Generation (CompX Report-8 Flash Protocol)

| Transport Profile | VID | PID | Connection Kind | Polling Rate Family | Supported Rates |
| --- | --- | --- | --- | --- | --- |
| Atlantis Wired USB | `0x3554` | `0xF50F` | `WiredUsb` | `Wired` | 125, 250, 500, 1000 Hz |
| Atlantis 1K Receiver | `0x3554` | `0xF50D` | `WirelessDongle24Ghz` | `Wired` | 125, 250, 500, 1000 Hz |
| Atlantis 4K Receiver | `0x3554` | `0xF510` | `WirelessDongle24Ghz` | `Pulsar4k` | 125, 250, 500, 1000, 2000, 4000 Hz |

- **Usage Page:** `0xFF02`
- **Usage ID:** `0x0002`
- **Report ID:** `0x08`
- **Fixed Packet Size (including Report ID):** **17 bytes** (`[0x08, cmd, error, addr_hi, addr_lo, payload_len, payload[0..10], checksum]`)

### Lamzu Aurora / Newer CompX Generation (Extensible Seam)
- Newer models (`0x373E`, `0x37B0`) use a 64-byte feature-report page/command protocol (`Report ID 0x00`, 6-byte header `[status, 0x00, target, length, page, command]`, status codes `0x00` request, `0xA0` pending, `0xA1` ok, `0xA2` unsupported, `0xA3` busy). Supported as a dedicated codec module in `driver-lamzu` for future Aurora models.

## 2. Atlantis Framing & Checksum Algorithm

Every 17-byte HID report has the following layout (big-endian 16-bit flash address):

```text
Byte  0     : 0x08 (Report ID)
Byte  1     : Command ID
Byte  2     : Error / status (0x00 on requests; 0x00 = OK on responses)
Bytes 3..4  : Flash address (u16 big-endian, 0x0000 for non-flash commands)
Byte  5     : Payload length (0..=10)
Bytes 6..15 : Payload (zero-padded to 10 bytes)
Byte  16    : Checksum
```

### Checksum Formula (Both Packet & Sealed Flash Fields)
Both the 17-byte packet and every sealed flash field obey the invariant:
```text
(sum(all_bytes_including_checksum) & 0xFF) == 0x55
checksum = (0x55u8).wrapping_sub(sum(preceding_bytes))
```

**Worked Example (`ReadBattery`, cmd `0x04`, addr `0x0000`, len `0`):**
- Bytes `0..16`: `08 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00`
- Sum of bytes `0..16`: `0x08 + 0x04 = 0x0C`
- Byte `16` (checksum): `(0x55 - 0x0C) & 0xFF = 0x49`
- Full 17-byte frame: `08 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00 49`
- Verification: `(0x08 + 0x04 + 0x49) & 0xFF = 0x55` ✓

## 3. Read Commands

| Command ID | Name | Address | Payload Len | Response Payload |
| --- | --- | --- | --- | --- |
| `0x04` | `ReadBattery` | `0x0000` | `0` | `[percent (0..=100), charging (0/1), mv_hi, mv_lo]` (4 meaningful bytes even when header len=2) |
| `0x08` | `ReadFlash` | `addr` | `1..=10` | Requested flash bytes (fields include trailing `0x55`-sum checksum byte) |
| `0x0E` | `ReadActiveProfile` | `0x0000` | `0` | `[profile_index]` (`0..=3`) |
| `0x12` | `ReadFirmwareVersion` | `0x0000` | `0` | `[major, minor_bcd]` (e.g., `0x01 0x24` → `v1.24`) |

## 4. Write Commands & Flash Map

| Command ID | Name | Address | Payload Len | Effect |
| --- | --- | --- | --- | --- |
| `0x07` | `WriteFlash` | `addr` | `1..=10` | Writes sealed flash bytes (must append `(0x55 - sum) & 0xFF`) |
| `0x0F` | `WriteActiveProfile` | `0x0000` | `1` | `[profile_index]` (`0..=3`) |

### Verified Flash Field Offsets (Active Profile)

| Address | Data Len (+1 csum) | Field | Encoding |
| --- | --- | --- | --- |
| `0` | `1 + 1` | Polling Rate | Wired: `0x08`=125, `0x04`=250, `0x02`=500, `0x01`=1000. 4K (`Pulsar4k`): `0x08`=125, `0x04`=250, `0x02`=500, `0x01`=1000, `0x10`=2000, `0x20`=4000, `0x40`=8000 |
| `2` | `1 + 1` | DPI Stage Count | `1..=8` |
| `4` | `1 + 1` | Active DPI Stage | `0..stage_count` |
| `10` | `1 + 1` | Lift-Off Distance | `1` = 1.0 mm, `2` = 2.0 mm |
| `12 + i*4` | `3 + 1` | DPI Stage `i` (`0..8`) | `[x_lo, y_lo, ((x_hi & 3) << 2) \| ((y_hi & 3) << 6)]`, where `val = dpi / 50 - 1` (50..=26,000 DPI, step 50) |
| `44 + i*4` | `3 + 1` | DPI Stage `i` LED Color | `[r, g, b]` |
| `169` | `1 + 1` | Debounce (ms) | `0..=15` |
| `171` | `1 + 1` | Motion Sync | `0` = off, `1` = on |
| `175` | `1 + 1` | Angle Snapping | `0` = off, `1` = on |
| `177` | `1 + 1` | Ripple Control | `0` = off, `1` = on |

## 5. Evidence Sources
- `OpenMouse-Project/mouse-protocol` (`src/lamzu/atlantis.ts` & `src/compx/codec.ts`): Hardware-verified on Lamzu Atlantis Mini 4K (`0x3554:0xF50F`, fw `1.24`), `0xF50D` 1K receiver, `0xF510` 4K receiver (`Pulsar4k` rate family), 10-bit DPI axis flags, `[percent, charging, mv_hi, mv_lo]` battery payload.
- `LeadSun/lamzu-cfg` (`src/atlantis.rs`, `src/atlantis/hid.rs`, `src/device.rs`): Flash address table, `0x08` 17-byte packet framing, `0x55`-sum checksum (`0u8.wrapping_sub(171u8.wrapping_add(sum))`).
