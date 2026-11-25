# AgnoPeriph — Engineering & Agent Guidelines (`GEMINI.md`)

This document is the authoritative contract for all work in **AgnoPeriph**. Every contributor and coding agent must follow these guidelines strictly unless the user explicitly overrides them. A Claude-specific variant lives in `CLAUDE.md`; the protocol-discovery rules below are Gemini-only.

---

## 1. Mission & Scope
**AgnoPeriph** is a lightweight, unified, brand-agnostic Windows desktop peripheral manager written entirely in **Rust**, with a modular USB HID backend and a native GPU-accelerated frontend powered by **`gpui-kit`** (`0.11.0`).

Target peripherals:
1. **Logitech** — HID++ 1.0 / 2.0 mice and keyboards (wired, Lightspeed, Bolt, Unifying).
2. **Lamzu** — Atlantis series (CompX protocol), extensible to newer Aurora models.
3. **ATK** — ATK RS7 Hall Effect keyboard (per-key actuation, Rapid Trigger, DKS, SOCD, calibration status) and ATK mice where applicable.

---

## 2. Non-Negotiable Safety Guardrails
1. **Native Windows Only (`x86_64-pc-windows-msvc`):**
   - Always build, test, and run natively on Windows. Never run inside WSL.
2. **NO Firmware Reverse Engineering or Modification:**
   - Never dump, disassemble, decompile, patch, or flash device firmware, bootloaders, or DFU/IAP update modes.
3. **Host-Side Runtime USB HID Only:**
   - Strictly use standard host-to-device USB HID `SET_REPORT`, `GET_REPORT`, and interrupt transfers (via `hidapi`) that mirror official runtime configuration commands.
4. **Read-Only First & Safe-by-Default Testing:**
   - All live transport writes must pass through `agnoperiph_transport::GuardedTransport` with a `PacketValidator` enforcing exact packet length, allowed Report IDs, and checksum verification before any byte is sent to hardware.
   - Maintain a global `--dry-run` mode that still validates and logs outgoing packets (hex + metadata) without transmitting writes to physical devices.
   - For every new driver or device, verify **read-only** queries (device info, battery, current DPI/actuation) against live hardware before testing any write command.
   - Never transmit guessed or undocumented opcodes to live hardware; every opcode must be backed by verified reference repos, recovered WebHID JS, or USBPcap captures.
5. **Hardware Isolation & Vendor App Hygiene:**
   - Implement and unit-test every packet encoder/decoder against `MockHidTransport` before running against physical peripherals.
   - Remind the user to fully exit all official vendor applications (**G Hub**, **ATK HUB**, **Lamzu software**, including system tray background processes) before any live HID session.
   - Treat wired USB cables and 2.4 GHz wireless receiver dongles as distinct transport profiles (`ConnectionKind::WiredUsb` vs `ConnectionKind::WirelessDongle24Ghz`) with separate PIDs and framing prefixes.

---

## 3. Cargo Workspace & Architecture Boundaries

Dependencies point strictly downward and remain acyclic:

```text
crates/ui (Phase 4)  ──┐
crates/cli           ──┼──▶ crates/drivers/* ──▶ crates/transport ──▶ crates/core
                       └───────────────────────▶ crates/transport & crates/core
```

| Crate | Package Name | Responsibility | Allowed Dependencies |
| --- | --- | --- | --- |
| `crates/core` | `agnoperiph-core` | Brand-agnostic hardware traits (`Peripheral`, `PointerDevice`, `HallEffectKeyboard`, `RgbController`, `BatteryMonitor`, `ProfileManager`), strongly typed validated models, `DeviceRegistry`. | Pure Rust only (`serde`, `thiserror`). **Zero** `hidapi`, OS, or UI dependencies. `#![forbid(unsafe_code)]`. |
| `crates/transport` | `agnoperiph-transport` | `HidTransport`, `HidEnumerator`, `GuardedTransport` (`--dry-run`, `PacketValidator`, `PacketSink`), `HidapiTransport`, deterministic `MockHidTransport`. | `agnoperiph-core`, `hidapi`, `thiserror`, `serde`, `tracing`, `hex`. |
| `crates/drivers/driver-logitech` | `agnoperiph-driver-logitech` | Logitech HID++ 1.0 / 2.0 protocol implementation (Phase 2). | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-lamzu` | `agnoperiph-driver-lamzu` | Lamzu Atlantis (CompX) & Aurora protocol implementation (Phase 2). | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-atk` | `agnoperiph-driver-atk` | ATK HUB / RS7 Hall Effect keyboard protocol implementation (Phase 3). | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/cli` | `agnoperiph-cli` | Headless CLI (`list`, `probe`, `get`, `set`, `--dry-run`, `--trace-hex`, `--json`). | `agnoperiph-core`, `agnoperiph-transport`, drivers, `clap`, `anyhow`, `serde_json`, `tracing-subscriber`. `#![forbid(unsafe_code)]`. |
| `crates/ui` | `agnoperiph` | Native Windows GPU frontend (Phase 4). Deferred from workspace compilation during Phases 1–3 to keep driver builds fast. | **`gpui-kit = "0.11.0"`** (single UI dependency), `agnoperiph-core`, `agnoperiph-transport`, drivers. |

---

## 4. Rust & Core Domain Conventions
- **Edition & Toolchain:** Rust Edition `2024`, native `x86_64-pc-windows-msvc`.
- **Error Handling:** Libraries (`core`, `transport`, `drivers/*`) use typed `thiserror` errors (`CoreError`, `TransportError`). Only `crates/cli` may use `anyhow` at the binary boundary.
- **Public API Discipline:**
  - Public data structs carry `#[non_exhaustive]` so fields can evolve without breaking downstream crates.
  - Validate domain invariants at construction time (`DpiValue`, `DebounceMs`, `TravelHundredthsMm`, `Brightness`, `ProfileSlot`) so invalid values never reach wire encoders.
  - Use fixed-point `TravelHundredthsMm(u16)` (`10` = `0.10 mm`, `400` = `4.00 mm`) for Hall Effect travel math—never floats across wire boundaries.
  - Boolean readers use `is_*` or `has_*`; avoid `can_*`. Spell the readonly state as `readonly` (one word).
  - Use `DeviceId` (stable across sessions/frames) as the domain identity and GPUI `ElementId` seed—never mutable list indices.
- **Capability-Driven Composition:**
  - UI and CLI code must render and dispatch purely via `Peripheral::capabilities()` and `as_pointer()`, `as_hall_effect()`, `as_rgb()`, `as_battery()`, `as_profiles()`. Never hardcode vendor-specific UI branches when a capability trait expresses the feature.

---

## 5. Protocol Discovery & `morluto/rea` MCP Rules
Before writing any Rust driver code for newly discovered or undocumented commands, use the **`rea` MCP server** (`reverse-engineer-anything` skill) strictly within the allowed lanes below:

### Allowed REA MCP Lanes & Tools ONLY
1. **WebHID JavaScript Reconstruction (Preferred):** `javascript-applications.md`, `browser-observation`, `website-script-export`, `javascript-artifact-reconstruction` (`list_browser_targets`, `inspect_web_page`, `analyze_web_bundle`, `observe_web_session`, `export_web_scripts`, `analyze_javascript_application`, `trace_javascript_semantics`, `recover_javascript_sources`) on official WebHID configurators (e.g., ATK HUB WebHID, newer Lamzu web apps) to locate `sendReport` / `sendFeatureReport` / `inputreport` handlers, command IDs, report IDs, packet lengths, checksum formulas, and `mm`-to-byte conversion math.
2. **USBPcap / Wireshark Analysis:** `pcap-analysis` and `packet-captures` (`inspect_packet_fields`, `inspect_packet_capture`, `inspect_pcap_capture`) on user-supplied `.pcapng` captures to filter outgoing `SET_REPORT` / `URB_INTERRUPT out` frames for the target `Bus.Device` and diff payloads across setting changes.
3. **Electron Observation (Fallback Only):** `electron-observation` (`list_electron_targets`, `inspect_electron_page`, `capture_electron_scenario`) if a feature only exists in an Electron desktop client's JS layer.

### Forbidden REA Lanes
- **NEVER** load or invoke Ghidra, IDA, Hopper, Cutter, Frida, Android/iOS, native binary disassembly, or firmware analysis tools or references.

### Mandatory Evidence Spec (`docs/protocols/<device>.md`)
Every newly discovered protocol (starting with `docs/protocols/atk-rs7.md`) must be documented **before** Rust implementation with:
1. Exact VID, PID (wired vs. dongle), Usage Page, Usage ID, Report ID, and fixed packet size.
2. Header bytes, endianness, padding, and exact checksum/CRC algorithm with a worked example.
3. Separate tables for **Read** commands vs. **Write** commands.
4. An **Evidence** section citing the exact recovered JS snippet and/or raw captured hex frames (request + response) proving every byte offset. Never guess or interpolate undocumented offsets.

### Open-Source Reference Blueprints
Study and port documented wire formats from:
- ATK & Lamzu (TS/WebHID): `https://github.com/OpenMouse-Project/mouse-protocol`
- Lamzu Atlantis: `https://github.com/LeadSun/lamzu-cfg`
- Lamzu CompX vs. Aurora: `https://github.com/qu1ncky/BatteryTray`
- ATK Keyboards (`0xFF01`): `https://github.com/bsdrop/atk68`
- Logitech HID++ 1.0/2.0: `https://github.com/libratbag/libratbag`, `https://github.com/pwr-Solaar/Solaar`, `https://github.com/Gianlz/pxmouse`

---

## 6. GPUI Kit (`crates/ui`) Rules (Phase 4)
Before designing or writing any UI code, **read both normative guides in full**:
- **`gpui-kit`** skill (`references/coding-guides.md`, `references/usage.md`, `references/conventions.md`)
- **`gpui-kit-design-guides`** skill (`references/design-guides.md` — visual hierarchy, spacing, density, desktop conventions, interaction states, overlays, data-heavy views, interface copy, and the Design Review Checklist)

- **Single UI Dependency:** Depend only on `gpui-kit = { version = "0.11.0", features = ["gpui-fast"] }`. Import GPUI via `use gpui_kit::*;` and layers via `gpui_kit::component`, `gpui_kit::base`, `gpui_kit::assets`, `gpui_kit::platform`. Never add separate `gpui` or `gpui-component` crate entries. (Sole exception: the test-only `gpui-fast-wgpu` dev-dependency for screenshots, §6.1.)
- **Bootstrap:** Call `gpui_kit::init(cx)` first inside `gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(...)` and open windows via `gpui_kit::open_window(...)` so `Root` mounts dialog/sheet/notification/tooltip layers properly.
- **Never Invent APIs:** Always verify component constructors and methods against `gpui-kit` docs/source before writing code.
- **State & Rendering:**
  - Use `RenderOnce` for stateless presentational controls and `Entity<T>` for retained state, focus, subscriptions, or async HID tasks.
  - Keep `render` pure and side-effect-free; never perform HID I/O or call `cx.notify()` unconditionally inside `render`.
  - Run blocking `hidapi` calls on a background executor/worker thread; attach request revisions/identities so stale responses never overwrite newer state.
  - Store subscription handles (`_subscriptions`) on the owning entity.
- **Identity & Layout:**
  - Use stable domain-derived `ElementId`s (e.g., `DeviceId`, `KeyIndex`, stage/profile IDs)—never mutable list indices.
  - Use `cx.theme()` semantic tokens and GPUI rem-based scale helpers (`p_2()`, `gap_3()`, `text_sm()`)—no raw hex/RGB literals or arbitrary `px(...)` nudges in UI views.
  - Remember `h_flex()` centers on the cross axis; use `h_flex().items_stretch()` for multi-column desktop layouts (sidebar + main content + inspector).
  - Use `Button` (with `ghost`/`outline` when quiet) for all in-app commands; reserve `Link` strictly for external URLs.
  - Use `AlertDialog` only for consequential/destructive actions (never reversible toggles).
- **Testing:** Verify UI interactions using `#[gpui_kit::test]` and `VisualTestContext` against real production views.
- **Engine:** `gpui-kit` here runs on **`gpui-fast`** (Longbridge's GPUI fork with scroll layers and retained-subtree reuse), enabled by the workspace `gpui-fast` feature. `gpui_kit::*` re-exports `gpui_fast`, so read `gpui-fast-*` sources when checking framework behavior, not `gpui-pre-*` (still compiled as a gpui-kit dependency, but not re-exported). For smooth scrolling, move a `ScrollHandle` offset and `cx.notify(view)` the owning view; never call `window.refresh()`, which dirties the whole window and defeats scroll layers.

### 6.1 Visual Verification (Headless Screenshots)
Behavior tests (`tests/ui.rs`) cannot show what the UI looks like. Any change with a visible surface (layout, spacing, color, theme, copy) must also be checked by rendering screenshots and **actually viewing them** before the work is reported done. Never launch the real binary for this: it enumerates live HID devices.

1. **Run:** `cargo test -p agnoperiph --test visual -- --ignored --nocapture`. It renders the production `AgnoPeriphApp` against `MockPeripheralService` at 1240×820 with dev tools off, in light and dark themes, and writes PNGs to `target/ui-screenshots/` (`<view>-<theme>.png`, 2× scale).
2. **View:** open each PNG that your change affects with the image Read tool and review it against the design guides and any reference screenshots. Check text, truncation, overlap, clipping at the window edge, alignment and both themes.
3. **Extend:** add views or states in `crates/ui/tests/visual.rs` by driving the UI first, with the same helpers as `tests/ui.rs` (`click`, `open_tab`, `set_theme`), then calling `save(cx, handle, "name")`. Use the mock only.
4. **Report:** say which screenshots you viewed and what you fixed. If the renderer is unavailable (no GPU adapter, CI), say the visual check was skipped. Do not claim it passed.

How it works, so nobody has to rediscover it:
- **Renderer:** gpui supplies no headless renderer on Windows (`current_headless_renderer()` returns `None`). `tests/visual.rs` therefore builds a `HeadlessAppContext` with `gpui_wgpu::WgpuHeadlessRenderer` from the dev-dependency `gpui-fast-wgpu` (lib name `gpui_wgpu`), pinned to the exact `gpui-fast` version `gpui-kit` uses (`=0.1.19`). The context loads `agnoperiph::icons::AppAssets`, the same asset source as the app, so custom and extra icons render. This dev-dependency (plus `image` and `anyhow`) is the one allowed exception to "single UI dependency". It is test-only and must never move into `[dependencies]`. When upgrading `gpui-kit`, bump the pin to match, or the build fails with "multiple versions of crate `gpui`".
- **Text:** `gpui_kit::platform::current_platform(true)` returns a **no-op** text system on Windows. Text lays out at the right size but every glyph is empty, so screenshots show boxes with no text. Use `current_platform(false).text_system()` (real DirectWrite), wrapped to force grayscale glyphs as the test does.
- **Cost:** the first build compiles `wgpu`, which takes several minutes at the workspace's dependency `opt-level = 3`. Later builds reuse the cache. The test is `#[ignore]`d so `cargo test --workspace` stays GPU-free and fast.

---

## 7. Phased Workflow & Verification Gate
Work one phase at a time and wait for user confirmation between phases:
1. **Phase 1:** Workspace scaffold, `crates/core` traits + models, `crates/transport` (`hidapi`, `GuardedTransport`, `MockHidTransport`), `agnoperiph-cli list` & `probe`.
2. **Phase 2:** `driver-logitech` & `driver-lamzu` (mock-first tests → read-only live probe → safe write validation).
3. **Phase 3:** ATK RS7 protocol discovery via `rea`, `docs/protocols/atk-rs7.md`, `driver-atk` with fixture tests, live CLI validation.
4. **Phase 4:** `crates/ui` with `gpui-kit` (device sidebar, capability-driven mouse & ATK RS7 Hall Effect editors, collapsible live HID packet inspector).

Before completing any phase or milestone, run and verify:
```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
For UI changes, also run and view the screenshots (§6.1):
```powershell
cargo test -p agnoperiph --test visual -- --ignored --nocapture
```
