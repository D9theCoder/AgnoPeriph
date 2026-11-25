# AgnoPeriph Protocol Specifications

This directory stores evidence-backed host-side USB HID wire specifications serving as the single source of truth for driver implementations under `crates/drivers/`.

## Mandatory Specification Sections

Before implementing Rust driver code for any newly discovered protocol or command (especially in Phase 3 for the ATK RS7), the corresponding `docs/protocols/<device>.md` file MUST document:

1. **USB HID Identity:** Exact VID, PID (wired USB vs. 2.4 GHz wireless dongle listed separately), Usage Page, Usage ID, Report ID, and fixed packet size.
2. **Framing & Checksum:** Header bytes, byte order (endianness), padding, and exact checksum/CRC formula with a worked byte-by-byte example.
3. **Command Tables:** Separate tables for **Read** commands vs. **Write** commands.
4. **Evidence Block:** Exact recovered WebHID/Electron JavaScript snippet and/or raw captured USBPcap hex frames (request + response) proving every byte offset. Never guess or interpolate undocumented offsets.

## Planned Specifications
- `logitech-hidpp.md` — Phase 2 (Logitech HID++ 1.0 / 2.0)
- `lamzu-compx.md` — Phase 2 (Lamzu Atlantis CompX / Aurora)
- `atk-rs7.md` — Phase 3 (ATK RS7 Hall Effect keyboard)
