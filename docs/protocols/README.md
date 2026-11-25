# AgnoPeriph Protocol Specifications

This directory stores evidence-backed host-side USB HID wire specifications serving as the single source of truth for driver implementations under `crates/drivers/`.

## Mandatory Specification Sections

Before implementing Rust driver code for any newly discovered protocol or command, the corresponding `docs/protocols/<device>.md` file MUST document:

1. **USB HID Identity:** Exact VID, PID (wired USB vs. 2.4 GHz wireless dongle listed separately), Usage Page, Usage ID, Report ID, and fixed packet size.
2. **Framing & Checksum:** Header bytes, byte order (endianness), padding, and exact checksum/CRC formula with a worked byte-by-byte example.
3. **Command Tables:** Separate tables for **Read** commands vs. **Write** commands.
4. **Evidence Block:** Exact recovered WebHID/Electron JavaScript snippet, upstream open-source reference, and/or raw captured USBPcap hex frames (request + response) proving every byte offset. Never guess or interpolate undocumented offsets.

## Specifications
- `logitech-hidpp.md` — Logitech HID++ 1.0 / 2.0 mice & keyboards
- `lamzu-compx.md` — Lamzu Atlantis CompX / Aurora mice
- `atk-rs7.md` — ATK RS7 Hall Effect keyboard
- `razer-hid.md` — Razer 90-byte Feature Report mice (Viper, DeathAdder, Basilisk, Naga, Orochi)
- `pulsar-hid.md` — Pulsar Report-8 Flash & XS-1 mice (X2, X2V2, X2H, X2 CrazyLight, Xlite, X3)
- `finalmouse-hid.md` — Finalmouse UltralightX & Starlight X mice
- `wlmouse-hid.md` — WLmouse CompX 64-byte Feature Report mice (Beast X, Beast Mini, Sword X, Strider)
- `endgame-gear-hid.md` — Endgame Gear OP1 / XM2 8K & 4K Wireless mice
- `wooting-hid.md` — Wooting Hall Effect analog keyboards (60HE, 60HE+, 80HE, Two HE, UwU)
- `qmk-via.md` — QMK / VIA Raw HID keyboards (Keychron, Lemokey, NuPhy, MonsGeek, Drop, Glorious QMK)
