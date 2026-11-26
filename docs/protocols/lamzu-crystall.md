# LAMZU Crystal Wireless — packed flash image & onboard macros

Protocol family: CompX Report-8 (`0x08`), 17-byte reports, `0x55`-sum flash checksum.
This document covers the **packed 6912-byte flash window** used by the official
`LAMZU CRYSTAL WIRELESS` desktop tool (assembly `Mouse Drive Beta`, app version
`2.1.0.43`) and, in particular, its macro storage.

> The wire image is **not** the 10428-byte .NET `FlashDataMap` struct. Offsets from
> `Marshal.OffsetOf`/`Marshal.SizeOf` (`keys@74`, `macroKey@956`, 592-byte slots)
> describe the in-memory struct only. The device returns a packed image whose key
> and macro offsets differ; using the struct offsets produces checksum failures.
> The older Atlantis Mini map in `lamzu-compx.md` (`lamzu-cfg`: macros at `768`,
> 384-byte slots) is a different firmware family — do not mix the two.

## 1. Identity

| Field | Value |
| --- | --- |
| VID | `0x3554` |
| Wired PID | `0xF50F` |
| Dongle PIDs | `0xF50D`, `0xF510`, `0xF517` |
| Interface | HID interface 1, collection 5 |
| Report ID | `0x08` |
| Packet size | 17 bytes (1 report id + 4 header + 1 length + 10 payload + 1 checksum) |
| Opcodes | `0x04` battery, `0x07` write flash, `0x08` read flash, `0x0E` read profile, `0x0F` write profile |
| Checksum | `(0x55 - sum(bytes)) & 0xFF` over the field before the checksum byte |

Worked checksum example, stock left-click binding `01 01 00 53`:
`0x01 + 0x01 + 0x00 = 0x02`, `0x55 - 0x02 = 0x53`.

## 2. Packed flash window

The vendor tool reads the whole configuration with one `ReadFlashData (0x00)
Len(6912)` request. Offsets below are from the start of that window.

| Offset | Size | Field |
| --- | --- | --- |
| `0x00` | 6 × 2 | `reportRate`, `maxDPI`, `currentDPI`, `xSpindown`, `ySpindown`, `silenceHeight` (1 value byte + checksum each) |
| `0x0C` | 8 × 4 | DPI stage values: 3 bytes + checksum each |
| `0x2C` | 8 × 4 | DPI stage colors: RGB + checksum each |
| `0x4C` | 4 × 2 | `DPILed`: `mode`, `brightness`, `breath_speed`, `enable` |
| `0x54` | 12 | unused in this window |
| `0x60` | 16 × 4 | `KeyFunMap[16]`: `[action_id, param1, param2, checksum]` |
| `0xA0` | 8 | `LedBar`: 6 value bytes + checksum, then 1 value byte + checksum at `0xA7` |
| `0xA8` | 88 | unused in this window |
| `0x100` | 16 × 32 | `ShortCutKey[16]` |
| `0x300` | 16 × 384 | `MacroKey[16]` |
| `0x1B00` | — | end of window |

Every field is sealed independently with one trailing `0x55` checksum byte; a
`ReadFlash`/`WriteFlash` request may split a field into ≤10-byte payloads, and the
checksum covers the whole field, not each transfer.

The remaining `MouseConfig` bytes (debounce, motion sync, sleep) are not carried in
this window; they use separate reads. Do not map them from the struct offsets.

## 3. Macro storage

```text
MacroKey (384 bytes, last 1 byte unused)
  0   u8        nameLength            1..=30
  1   u8[30]    name                  ASCII, not sealed
  31  u8        contextCount          0..=70
  32  Context[70] * 5 bytes
  ... u8        checksum              at 32 + 5 * contextCount
```

```text
Context (5 bytes)
  0   u8        flags    bits 7..6 state: 0x80 down, 0x40 up, 0x00 none
                         bits 3..0 type:  0 mod, 1 keyboard, 2 media,
                                          3 power, 4 mouse button, 5 MoveXY
  1   u8[2]     value    little-endian u16 (keyboard usage, modifier mask, ...)
  3   u8[2]     delay    big-endian u16 milliseconds (firmware caps at 65535)
```

The delay is the only multi-byte field in the image stored big-endian. The
checksum covers the count byte plus every context byte: `sum(bytes[31..32+5n]) +
checksum == 0x55`.

Trigger: `KeyFunMap[i] = [0x06, i, repeat]` binds key slot `i` to macro slot `i`.
`repeat` is the replay count; the official UI stores it from `Macro Cycle Times`
and treats `0` as `1`. The editor supports 16 slots (`ShowMaxMacroKey` uses
index 15).

### Value semantics by `type`

| type | value |
| --- | --- |
| `0` Modifier | HID modifier bitmask (`0x02` left shift, `0x20` right shift, `0x01` left ctrl, `0x10` right ctrl, `0x04`/`0x40` alt, `0x08`/`0x80` GUI) |
| `1` Keyboard | USB HID keyboard usage (`0x04` = `A`, `0x1E` = `1`) |
| `2` Media | USB HID consumer usage |
| `3` Power | ACPI power usage |
| `4` Mouse button | `value[0]` bitmask (`0x01` left, `0x02` right, `0x04` middle, `0x08` back, `0x10` forward) |
| `5` MoveXY | `value[0]` = X delta, `value[1]` = Y delta; stored as a down/up pair with state `none` |

The vendor recorder only emits keyboard and mouse-button events; media events
(`type = 2`) are skipped during recording. Delays are clamped to a 10 ms minimum
while recording.

## 4. Writer contract (this repository)

`crates/drivers/driver-lamzu/src/crystall.rs` implements the packed layout and
codec. The driver only authors `type = 1` keyboard events.

- Reading: every macro record is read, including slots with no macro trigger (the
  vendor editor lists all 16 records regardless of bindings). A slot whose contexts
  contain any other type (MoveXY included) stays visible by name with no steps and
  `MacroSlot::is_readonly` set; the rest of the slots still load. Header bytes above
  the valid range (`0xFF` on erased flash) are treated as empty, not clamped to the
  maximum.
- Writing: a slot with unsupported current events is left untouched when the
  incoming slot is unchanged (same name, no steps); otherwise the write is refused
  with `CoreError::UnsupportedCapability("lamzu_macro_advanced_events")` instead
  of silently dropping data. Every slot is checked before the first write, so a
  refused slot never leaves a save half applied. The UI blocks edits to read-only
  slots, and `validate_change` rejects them before they reach the driver.
- A macro longer than 70 contexts (a tap counts as two) is refused with
  `CoreError::OutOfRange`, never truncated.
- Writes are ordered contexts → name → trigger, and are read back and compared
  unless `--dry-run` is active.
- The trigger is only written when key slot `i` already resolves to a macro
  (`KeyFunMap[i][0] == 0x06`) or is disabled (`00 00 00`). Saving a macro must not
  silently turn left click, back, DPI, or any other working button into a macro
  trigger; those bindings are left untouched, so their slot content is stored but
  never fires until the user assigns it in the official tool.

## 5. Evidence

- `Mouse Drive Beta.exe`, assembly `Mouse Drive Beta` `1.0.0.4`, .NET Framework
  4.8, SHA-256 `3cd7bed1ee95fa78cd168c71e36c854ed8ec35d2a20addde2ba54f03af40d751`
  (path `C:\Program Files (x86)\LAMZU CRYSTAL WIRELESS\Mouse Drive Beta.exe`).
- Managed `DriverLib.DataParser::ProtocolParser(byte[] data, ref FlashDataMap)`
  allocates `Marshal.SizeOf<FlashDataMap>()` (10428) and calls the native
  `CS_ProtocolDataParser` with the 6912-byte read.
- `hidusb.dll` (embedded as `costura64.hidusb.dll`, extracted by Costura to
  `%TEMP%\Costura\<hash>\64\hidusb.dll`, SHA-256
  `abbc0a8175fe91471ae6e082abacd69137ac725f22079104478e6ff0b7a9e6e1`):
  - `CS_ProtocolDataParser` (`0x180007710`) tail-jumps to `0x1800074A0`, which parses
    the 6912-byte input into the struct:
    - DPI values at source `0x0C + i*4`, colors at `0x2C + i*4` (3 bytes + checksum).
    - `DpiLed` at `0x4C` (four value+checksum pairs).
    - `KeyFunMap` source `0x60 + i*4`, output stride 3.
    - `ShortCutKey` source `0x100 + i*0x20`, output `shortCutKey@124`, stride 52.
    - `MacroKey` source `0x300 + i*0x180`, output `macroKey@956`, stride 592.
  - `BufferToMacroKey` (`0x180009970`) checks the context checksum at
    `data[32 + 5*count]`, unpacks `type = flags & 0x0F`, state from `flags & 0xC0`
    (`0x80` down, `0x40` up), `value = ctx[1] | ctx[2] << 8`, and
    `delay = ctx[3] << 8 | ctx[4]` (big-endian).
- Live `Device6_log.txt` (2026-11-26 09:35) `ReadFlashData (0x00) Len(6912)`:
  `0x60: 01 01 00 53 01 02 00 52 01 04 00 50 01 08 00 4C` confirms the key map
  at `0x60` with a per-slot checksum. The same dump has a slot at `0xA80`
  (`0x300 + 5 * 0x180`): `0A` + `spam click`, count `02`, contexts
  `84 01 00 00 0A | 44 01 00 00 0A`, checksum `75`. Its key slot 5 holds the stock
  `[02 01 00]` binding, so the macro record exists without a trigger.
- Cached image `%LOCALAPPDATA%\LAMZU\LAMZU CRYSTAL WIRELESS\Device_Info\Device_CID11_MID6.bin`
  is the 10428-byte struct image. Its `macroKey@956 + 5*592` slot decodes to the
  same macro (`state 0/1, type 4, value 1, delay 10`), which ties the packed record
  above to the struct-backed editor.
- The struct's `MacroContext` fields are `keyState: u8`, `type: u8`, `value: u8[]`,
  `delay: u32`; the flash record compresses `value` to two bytes and `delay` to a
  big-endian u16.

## 6. Open points

- The packed window ends at `0x1B00`; `MacroKey[15]` occupies `0x1980..0x1B00`.
  Slots are fully inside the observed window, so no macro slot is unverified.
- `ShortCutKey` uses 3-byte contexts (no delay); this driver does not write them.
- The remaining `MouseConfig` bytes are read through other commands and were not
  re-validated against this firmware; the struct offsets in `lamzu-compx.md` are
  not valid flash addresses for this image.
