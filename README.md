# AgnoPeriph

**One lightweight, native Windows app for all your gaming mice and keyboards.**  
Replace half a dozen bloated, telemetry-heavy vendor suites with a single GPU-accelerated Rust desktop manager and headless CLI.

[![Platform: Windows x64](https://img.shields.io/badge/platform-Windows%20x86__64-0078D4)](#quick-start)
[![Latest Windows Release](https://img.shields.io/github/v/release/D9theCoder/AgnoPeriph?label=download%20.exe&color=2EA043)](https://github.com/D9theCoder/AgnoPeriph/releases/latest)
[![Built with Rust](https://img.shields.io/badge/Rust-Edition%202024-orange)](#architecture)
[![UI: gpui-kit](https://img.shields.io/badge/UI-gpui--kit%20%2F%20gpui--fast-6E56CF)](#why-agnoperiph)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green)](#license)

---

## Why AgnoPeriph?

Modern desks mix brands — a Logitech or Pulsar mouse, an ATK or Wooting Hall Effect keyboard, a Keychron QMK board — and each vendor expects its own always-on background service. **AgnoPeriph** talks directly to your hardware over standard host-side Windows USB HID:

- **Native & fast** — Written end-to-end in Rust with a GPU-accelerated [`gpui-kit`](https://crates.io/crates/gpui-kit) (`gpui-fast`) interface. Zero Electron, zero Chromium, minimal RAM.
- **Capability-driven UI & staged editing** — Controls appear automatically based on what each peripheral actually supports (DPI stages, 8K polling, lift-off distance, Hall Effect per-key actuation, Rapid Trigger & live travel tester, SOCD/DKS/Mod-Tap, QMK keymaps & macros, multi-zone RGB, Fn-inversion, Game Mode, onboard profiles, battery telemetry). Unsaved edits stage behind a unified Apply / Undo (`Ctrl+Z`) / Discard bar with switch guards, validated JSON profile import/export, quick presets, and keyboard-first shortcuts (`Ctrl+1..9`, `Ctrl+Tab`, `Ctrl+S`, `Ctrl+/`).
- **Game Profiles, Tray Quick-Menu & Battery Alerts** — Auto-switch peripheral settings when you launch a game, check live battery % and switch profiles from the Windows system tray, receive non-spammy low-battery Windows toast notifications, and optionally start with Windows.
- **Safety-first USB HID** — Every outgoing write passes through `GuardedTransport` + a per-protocol `PacketValidator` enforcing exact packet length, Report IDs, checksums, and hard blocks on bootloader/DFU/EEPROM-reset commands. Includes a global `--dry-run` mode and live hex packet tracing.
- **Headless CLI included** — Script and inspect devices with `agnoperiph-cli` (`list`, `probe`, `set`, `--dry-run`, `--trace-hex`, `--json`).

---

## Supported Hardware & Firmware

AgnoPeriph targets documented host-side runtime USB HID protocols only — **no firmware modification or flashing**. Every wire format is documented in [`docs/protocols/`](docs/protocols/README.md).

### Mice

| Brand | Firmware / Protocol | Supported Models & Families | Connection | Supported Features |
| --- | --- | --- | --- | --- |
| **Logitech** | HID++ 1.0 / 2.0 ([spec](docs/protocols/logitech-hidpp.md)) | G-series mice (G Pro X Superlight, G502, etc.) | Wired USB, Lightspeed, Bolt, Unifying | DPI stages, polling rate, onboard profiles, battery & voltage curve |
| **Lamzu** | CompX Flash & Aurora ([spec](docs/protocols/lamzu-compx.md)) | Atlantis series (1K / 4K), Maya / Aurora codec | Wired USB, 2.4 GHz dongle | DPI stages, polling rate, LOD, debounce, Motion Sync, Angle Snapping, Ripple Control, battery, profiles |
| **Razer** | 90-byte HID Feature ([spec](docs/protocols/razer-hid.md)) | Viper (V2/V3 Pro, 8KHz), DeathAdder (V2/V3), Basilisk V3, Naga, Orochi | Wired USB, HyperSpeed dongle | X/Y DPI stages, polling rate (up to 8K), LOD, battery, Standard & Extended Matrix multi-zone RGB |
| **Pulsar** | Report-8 Flash & XS-1 ([spec](docs/protocols/pulsar-hid.md)) | X2, X2V2, X2H, X2 CrazyLight, Xlite, X3 (XS-1 codec) | Wired USB, 2.4 GHz dongle | DPI stages (10/50/100 steps), polling rate (up to 8K), LOD (Low/Med/High), debounce, Motion Sync, Angle Snapping, Ripple Control, battery, profiles |
| **Finalmouse** | 64-byte Telemetry HID ([spec](docs/protocols/finalmouse-hid.md)) | UltralightX (ULX), Starlight X | Wired USB, 2.4 GHz dongle | DPI, polling rate (up to 8K), PAW LOD (0.7–2.0 mm), Motion Sync, battery curve, profiles |
| **WLmouse** | CompX 64-byte Feature ([spec](docs/protocols/wlmouse-hid.md)) | Beast X / Max / Pro, Beast Mini, Sword X, Strider | Wired USB, 2.4 GHz dongle | X/Y DPI stages, polling rate (up to 8K), LOD (0.7/1.0/2.0 mm), debounce, Motion Sync, Angle Snapping, Ripple Control, battery, profiles |
| **Endgame Gear** | `0xA0`/`0xA1` Config Blob ([spec](docs/protocols/endgame-gear-hid.md)) | OP1 8K, OP1w 4K, XM2 8K, XM2w 4K | Wired USB, 4K dongle | CPI stages, polling rate (up to 8K), LOD (0.7–2.0 mm), Motion Sync, Angle Snapping, Ripple Control |
| **MCHOSE** | M-HUB V2 (`0x11`/`0x12`), V3 (`0x4D`), A5 Gen1 (`0x2023`) ([spec](docs/protocols/mchose.md)) | A7 / A7 V2 / A7 V3, L7, A5 / A5 V3, K7, K5, R7, V7, G3 V3 | Wired USB, 1K / 2.4 GHz / 4K / 8K receivers | DPI stages, polling rate (up to 8K), LOD, debounce, Motion Sync, Angle Snapping, Ripple Control, battery, onboard profiles |
| **ATK / VGN / VXE** | `0xFF02` Report-`0x08` EEPROM ([spec](docs/protocols/atk-mice.md)) | ATK F1 / X1 / Z1 / A9, VXE R1 / Mad R, VGN Dragonfly F2 Master+ | Wired USB, 1K / 8K dongles | X/Y DPI stages, polling rate (up to 8K), LOD, debounce, Motion Sync, Angle Snapping, Ripple Control, battery, profiles |

### Keyboards

| Brand / Ecosystem | Firmware / Protocol | Supported Models & Families | Connection | Supported Features |
| --- | --- | --- | --- | --- |
| **ATK / VXE** | DuckBread `0xFF60` ([RS7 spec](docs/protocols/atk-rs7.md)) & GTech `0xFF01` ([FF01 spec](docs/protocols/atk-ff01.md)) | ATK RS7 RGB (`0xFF60`); ATK68 / ATK75 / VXE / Mad60 / Mad68 family (`0xFF01`) | Wired USB | Per-key actuation, Rapid Trigger, live key-travel tester, top/bottom deadzones, DKS, SOCD, Rapid Switch, Mod-Tap, Toggle Key, calibration, keymap, macros, RGB + logo light, polling rate, onboard profiles |
| **Logitech** | HID++ 2.0 ([spec](docs/protocols/logitech-hidpp.md)) | HID++ 2.0 keyboards (G-series, MX / K-series) | Wired USB, Lightspeed, Bolt, Unifying | Multi-zone RGB (`0x8070`/`0x8071`), Fn-inversion (`0x40A0`/`0x40A2`/`0x40A3`), Game Mode / Win Lock (`0x4522`), key remapping (`0x1B04`), polling rate, battery |
| **Wooting** | Wooting ARM / HE `0xFF55` & `0x1337` ([spec](docs/protocols/wooting-hid.md)) | 60HE, 60HE+, 80HE, Two HE, UwU | Wired USB | Firmware/layout detection, onboard profile readout, per-key actuation readout *(read-only writes for safety)*, RGB control (`wooting-rgb-sdk` commands) |
| **QMK / VIA** | VIA Raw HID `0xFF60:0x0061` ([spec](docs/protocols/qmk-via.md)) | Keychron, Lemokey, NuPhy, MonsGeek / Akko, Glorious GMMK QMK, Drop, standard QMK boards | Wired USB | Dynamic multi-layer keymap read/write/reset, VIA v3 macros, QMK RGB Matrix & RGBLight effects/brightness/color |

> **Note:** Wired cables and 2.4 GHz wireless dongles are modeled as distinct transport profiles with separate PIDs and packet framing.

---

## Quick Start

> **Important before connecting live hardware:** Fully exit all official vendor apps (**G Hub**, **Razer Synapse**, **ATK HUB**, **M-HUB**, **Lamzu**, **Pulsar**, **Wootility**, **Endgame Gear**, etc. — including background tray icons) so they do not contend for exclusive HID interfaces.

### Download Installer (Recommended)
Grab the latest Windows x64 installer (`.exe`) directly from **[GitHub Releases](https://github.com/D9theCoder/AgnoPeriph/releases/latest)**:

- **[Download AgnoPeriph-Setup-x64.exe](https://github.com/D9theCoder/AgnoPeriph/releases/latest/download/AgnoPeriph-Setup-x64.exe)** — installs both the desktop app (`agnoperiph.exe`) and headless CLI (`agnoperiph-cli.exe`).

### Build from Source
#### Requirements
- **Windows 10 / 11 (`x86_64-pc-windows-msvc`)** — native build only (do not run inside WSL).
- **Rust toolchain** (Edition 2024 / latest stable `rustup`).

### Launch the Desktop App
```powershell
cargo run -p agnoperiph --release
```

### Use the Headless CLI
```powershell
# 1. List connected Windows HID interfaces (vendor control pages marked with '*')
cargo run -p agnoperiph-cli -- list --vendor-pages-only

# 2. Read-only probe of all matched peripherals
cargo run -p agnoperiph-cli -- probe

# 3. Validate and inspect outgoing HID packets without writing to hardware
cargo run -p agnoperiph-cli -- --dry-run --trace-hex set --vid 0x046D --pid 0xC547 --dpi 1600 --polling-rate 1000
```

### Run Verification & Tests
```powershell
.\scripts\dev.ps1 check
# or manually:
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## Architecture

Dependencies flow strictly downward and remain acyclic:

```text
crates/ui  (agnoperiph)      ──┐
crates/cli (agnoperiph-cli)  ──┼──▶ crates/drivers/* ──▶ crates/transport ──▶ crates/core
                               └────────────────────────▶ crates/transport & crates/core
```

- **`crates/core`** — Pure Rust (`#![forbid(unsafe_code)]`, zero OS/HID/UI deps). Brand-agnostic hardware traits (`Peripheral`, `PointerDevice`, `HallEffectKeyboard`, `KeymapController`, `MacroController`, `RgbController`, `BatteryMonitor`, `ProfileManager`, `KeyboardConfigController`), fixed-point validated domain types (`TravelHundredthsMm`, `DpiValue`, `PollingRateHz`, `ExportedProfile`), and `DeviceRegistry`.
- **`crates/transport`** — `HidTransport`, `HidEnumerator`, `HidapiTransport`, deterministic `MockHidTransport`, and `GuardedTransport` (`PacketValidator`, `--dry-run`, `PacketSink`).
- **`crates/drivers/*`** — Self-contained protocol crates (`driver-logitech`, `driver-lamzu`, `driver-atk`, `driver-mchose`, `driver-razer`, `driver-pulsar`, `driver-finalmouse`, `driver-wlmouse`, `driver-endgame-gear`, `driver-wooting`, `driver-via`), each unit-tested against `MockHidTransport`.
- **`crates/cli`** — Headless diagnostics and configuration CLI.
- **`crates/ui`** — Native Windows GPU-accelerated desktop app built on `gpui-kit` (`gpui-fast`).

---

## Acknowledgments

Protocol documentation and wire compatibility build on open-source community work from [`OpenMouse-Project/mouse-protocol`](https://github.com/OpenMouse-Project/mouse-protocol), [`OpenMouse-Project/keyboard-protocol`](https://github.com/OpenMouse-Project/keyboard-protocol), [`libratbag`](https://github.com/libratbag/libratbag), [`Solaar`](https://github.com/pwr-Solaar/Solaar), [`OpenLogi`](https://openlogi.org/), [`alexfrih/mchose-linux`](https://github.com/alexfrih/mchose-linux), [`qmk/qmk_firmware`](https://github.com/qmk/qmk_firmware), [`WootingKb/wooting-rgb-sdk`](https://github.com/WootingKb/wooting-rgb-sdk), [`LeadSun/lamzu-cfg`](https://github.com/LeadSun/lamzu-cfg), [`qu1ncky/BatteryTray`](https://github.com/qu1ncky/BatteryTray), and [`bsdrop/atk68`](https://github.com/bsdrop/atk68).

## License

Dual-licensed under either of:

- **MIT License**
- **Apache License, Version 2.0**

at your option.
