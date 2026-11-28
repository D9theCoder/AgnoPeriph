# AgnoPeriph — Engineering & Agent Guidelines (`CLAUDE.md`)

This document is the authoritative contract for all work in **AgnoPeriph**. Every contributor and coding agent must follow these guidelines strictly unless the user explicitly overrides them. It is the Claude-oriented variant of the project guidelines: it covers architecture, implementation, verification, and UI rules, with driver work driven only by published wire formats. See `GEMINI.md` for the fuller variant.

This file is the authoritative project guideline for Claude. `GEMINI.md` is the Gemini CLI variant and is not part of Claude's context unless explicitly requested.

---

## 1. Mission & Scope
**AgnoPeriph** is a lightweight, unified, brand-agnostic Windows desktop peripheral manager written entirely in **Rust**, with a modular USB HID backend and a native GPU-accelerated frontend powered by **`gpui-kit`** (`0.11.0`).

Target peripherals (11 protocol driver crates):
1. **Logitech** — HID++ 1.0 / 2.0 mice and keyboards (wired, Lightspeed, Bolt, Unifying).
2. **Lamzu** — Atlantis series (CompX Report-8) and Maya / Aurora codec.
3. **ATK / VGN / VXE** — DuckBread (`0xFF60`) & GTech (`0xFF01`) Hall Effect keyboards (ATK RS7, ATK68/75, etc.) and `0xFF02` mice.
4. **MCHOSE** — M-HUB V2 (`0x11`/`0x12`), V3 (`0x4D`), and A5 Gen1 (`0x2023`) mice.
5. **Razer** — 90-byte HID Feature mice (Viper, DeathAdder, Basilisk V3, Naga, Orochi).
6. **Pulsar** — Report-8 Flash & XS-1 mice (X2, Xlite, X3).
7. **Finalmouse** — 64-byte Telemetry HID mice (UltralightX, Starlight X).
8. **WLmouse** — CompX 64-byte Feature mice (Beast X, Sword X, Strider).
9. **Endgame Gear** — `0xA0`/`0xA1` config-blob mice (OP1/XM2 8K & 4K Wireless).
10. **Wooting** — ARM / Hall Effect keyboards (`VID 0x31E3`, 60HE/80HE/Two HE/UwU).
11. **QMK / VIA** — VIA Raw HID keyboards (`0xFF60:0x0061`: Keychron, Lemokey, NuPhy, MonsGeek, Drop, etc.).

---

## 2. Non-Negotiable Safety Guardrails
1. **Native Windows Only (`x86_64-pc-windows-msvc`):**
   - Always build, test, and run natively on Windows. Never run inside WSL.
2. **No Firmware Modification or Flashing:**
   - Never dump, patch, or flash device firmware, bootloaders, or DFU/IAP update modes.
3. **Host-Side Runtime USB HID Only:**
   - Strictly use standard host-to-device USB HID `SET_REPORT`, `GET_REPORT`, and interrupt transfers (via `hidapi`) that mirror official runtime configuration commands.
4. **Read-Only First & Safe-by-Default Testing:**
   - All live transport writes must pass through `agnoperiph_transport::GuardedTransport` with a `PacketValidator` enforcing exact packet length, allowed Report IDs, and checksum verification before any byte is sent to hardware.
   - Maintain a global `--dry-run` mode that still validates and logs outgoing packets (hex + metadata) without transmitting writes to physical devices.
   - For every new driver or device, verify **read-only** queries (device info, battery, current DPI/actuation) against live hardware before testing any write command.
   - Never transmit guessed or undocumented opcodes to live hardware; every opcode must be backed by a documented open-source protocol implementation or official vendor documentation.
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
| `crates/drivers/driver-logitech` | `agnoperiph-driver-logitech` | Logitech HID++ 1.0 / 2.0 mice & keyboards. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-lamzu` | `agnoperiph-driver-lamzu` | Lamzu Atlantis (CompX) & Aurora mice. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-atk` | `agnoperiph-driver-atk` | ATK / VGN / VXE Hall Effect keyboards (`0xFF60`, `0xFF01`) & mice (`0xFF02`). | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-mchose` | `agnoperiph-driver-mchose` | MCHOSE M-HUB V2/V3 & A5 Gen1 mice. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-razer` | `agnoperiph-driver-razer` | Razer 90-byte HID Feature mice & multi-zone RGB. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-pulsar` | `agnoperiph-driver-pulsar` | Pulsar Report-8 Flash & XS-1 mice. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-finalmouse` | `agnoperiph-driver-finalmouse` | Finalmouse UltralightX / Starlight X 64-byte HID mice. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-wlmouse` | `agnoperiph-driver-wlmouse` | WLmouse CompX 64-byte Feature mice. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-endgame-gear` | `agnoperiph-driver-endgame-gear` | Endgame Gear OP1/XM2 8K & 4K Wireless mice. | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-wooting` | `agnoperiph-driver-wooting` | Wooting Hall Effect keyboards (`VID 0x31E3`). | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/drivers/driver-via` | `agnoperiph-driver-via` | QMK / VIA Raw HID keyboards (`0xFF60:0x0061`). | `agnoperiph-core`, `agnoperiph-transport`, `thiserror`, `tracing`, `hex`. |
| `crates/cli` | `agnoperiph-cli` | Headless CLI (`list`, `probe`, `get`, `set`, `--dry-run`, `--trace-hex`, `--json`). | `agnoperiph-core`, `agnoperiph-transport`, drivers, `clap`, `anyhow`, `serde_json`, `tracing-subscriber`. `#![forbid(unsafe_code)]`. |
| `crates/ui` | `agnoperiph` | Native Windows GPU frontend (active workspace member). | **`gpui-kit = "0.11.0"`** (single UI dependency), `agnoperiph-core`, `agnoperiph-transport`, drivers. |

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

### 4.1 Driver Reliability & Unowned-Hardware Hygiene
When implementing or maintaining drivers for hardware you cannot test physically, follow these rules:
1. **Port the full state machine, not just the wire bytes:** Audit mature reference daemons (`Solaar`, `libratbag`, `OpenMouse`, `OpenRGB`) for preconditions (e.g., switching Host mode → Onboard mode before selecting a profile), directory/ROM fallbacks when writeable flash is empty or uninitialized, disabled-slot filtering, transient error retries (`Busy`/NAK), and multi-entity descriptor selection (prefer main application firmware over bootloader entities).
2. **Enforce Windows HID queue hygiene on every exchange:** Long-lived Windows HID handles accumulate unsolicited notifications (battery, wireless status, DPI/profile events) in the OS input buffer. Always drain stale input reports (`drain_input_reports()`) before transmitting a request, read with a wall-clock deadline plus a generous non-matching packet cap (not a tiny fixed retry count like `6`–`8`), and prefer shared request/reply helpers in `agnoperiph-transport` over hand-rolled per-driver loops.
3. **Test hostile hardware states in `MockHidTransport`:** Every driver test suite must go beyond happy-path replies and cover:
   - Stale OS buffer frames (`push_stale_input_report`) plus mid-flight unsolicited notification bursts.
   - Factory-fresh / unconfigured states (Host mode, unwritten `0x00`/`0xFF` flash, ROM fallbacks, disabled profile slots).
   - Transient device errors (`Busy`/NAK) and non-matching frames.
   - Multi-entity responses (e.g., bootloader entity listed before main firmware) and split Windows HID collections.
4. **Stay conservative & make field issues replayable:** When a mutating command cannot be verified on physical hardware, gate it strictly on read-only capabilities (e.g., disable uninitialized profile slots in the UI rather than blindly rewriting flash) and preserve full TX/RX hex traces via `PacketSink` so real-device bug reports can be replayed directly in `MockHidTransport` regression tests.

---

## 5. Protocol Documentation & Reference Blueprints
Driver work is driven only by published wire formats and documented protocol references.

### Mandatory Evidence Spec (`docs/protocols/<device>.md`)
Every newly implemented protocol (starting with `docs/protocols/atk-rs7.md`) must be documented **before** Rust implementation with:
1. Exact VID, PID (wired vs. dongle), Usage Page, Usage ID, Report ID, and fixed packet size.
2. Header bytes, endianness, padding, and exact checksum/CRC algorithm with a worked example.
3. Separate tables for **Read** commands vs. **Write** commands.
4. An **Evidence** section citing the upstream reference source (repository, file, and symbol) or vendor documentation that proves every byte offset. Never guess or interpolate undocumented offsets.

### Open-Source Reference Blueprints
Study and port documented wire formats from:
- ATK & Lamzu (TS/WebHID): `https://github.com/OpenMouse-Project/mouse-protocol`
- Lamzu Atlantis: `https://github.com/LeadSun/lamzu-cfg`
- Lamzu CompX vs. Aurora: `https://github.com/qu1ncky/BatteryTray`
- ATK Keyboards (`0xFF01`): `https://github.com/bsdrop/atk68`
- Logitech HID++ 1.0/2.0: `https://github.com/libratbag/libratbag`, `https://github.com/pwr-Solaar/Solaar`, `https://github.com/Gianlz/pxmouse`

If a wire format is not covered by these references, stop and ask the user for documentation rather than probing live hardware.

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

1. **Run:** `cargo test -p agnoperiph --test visual -- --ignored --nocapture` (or `.\scripts\dev.ps1 visual [filter]`). It renders the production `AgnoPeriphApp` against `MockPeripheralService` at 1240×820 with dev tools off, in light and dark themes, and writes PNGs to `target/ui-screenshots/` (`<view>-<theme>.png`, 2× scale).
   - **Filter views during iteration:** Set `AGNO_VISUAL=<comma-separated substrings>` (e.g., `.\scripts\dev.ps1 visual triggers,mouse-sensor`) to render only the affected views.
   - **Baseline pixel diffing:** Run `.\scripts\dev.ps1 visual-baseline` (`AGNO_VISUAL_UPDATE_BASELINE=1`) to save baselines in `target/ui-screenshots-baseline/`; subsequent runs print `%` pixel diff vs. baseline (and fail if `AGNO_VISUAL_MAX_DIFF` is set and exceeded).
   - **Human live preview:** Humans can run `.\scripts\dev.ps1 mock` (`cargo run -p agnoperiph -- --mock`) or `.\scripts\dev.ps1 watch-mock` to preview UI changes in a real window without touching live HID hardware.
2. **HTML-first prototyping (medium-to-big frontend changes only):** For new screens, complex multi-panel layouts, or comparing design options, prototype the layout in an inline HTML/CSS preview first so the user can approve or tweak spacing and hierarchy in seconds before writing GPUI Rust code. Skip this for small tweaks (padding, labels, single controls) and go straight to Rust + filtered `tests/visual.rs`.
3. **View:** open each PNG that your change affects with the image Read tool and review it against the design guides and any reference screenshots. Check text, truncation, overlap, clipping at the window edge, alignment and both themes.
4. **Extend:** add views or states in `crates/ui/tests/visual.rs` by driving the UI first, with the same helpers as `tests/ui.rs` (`click`, `open_tab`, `set_theme`), then calling `save(cx, handle, "name")`. Use the mock only.
5. **Report:** say which screenshots you viewed and what you fixed. If the renderer is unavailable (no GPU adapter, CI), say the visual check was skipped. Do not claim it passed.

How it works, so nobody has to rediscover it:
- **Renderer:** gpui supplies no headless renderer on Windows (`current_headless_renderer()` returns `None`). `tests/visual.rs` therefore builds a `HeadlessAppContext` with `gpui_wgpu::WgpuHeadlessRenderer` from the dev-dependency `gpui-fast-wgpu` (lib name `gpui_wgpu`), pinned to the exact `gpui-fast` version `gpui-kit` uses (`=0.1.19`). The context loads `agnoperiph::icons::AppAssets`, the same asset source as the app, so custom and extra icons render. This dev-dependency (plus `image` and `anyhow`) is the one allowed exception to "single UI dependency". It is test-only and must never move into `[dependencies]`. When upgrading `gpui-kit`, bump the pin to match, or the build fails with "multiple versions of crate `gpui`".
- **Text:** `gpui_kit::platform::current_platform(true)` returns a **no-op** text system on Windows. Text lays out at the right size but every glyph is empty, so screenshots show boxes with no text. Use `current_platform(false).text_system()` (real DirectWrite), wrapped to force grayscale glyphs as the test does.
- **Cost:** the first build compiles `wgpu`, which takes several minutes at the workspace's dependency `opt-level = 1`. Later builds reuse the cache. The test is `#[ignore]`d so `cargo test --workspace` stays GPU-free and fast.

---

## 7. Phased Workflow & Verification Gate
Work one phase at a time and wait for user confirmation between phases:
1. **Phase 1:** Workspace scaffold, `crates/core` traits + models, `crates/transport` (`hidapi`, `GuardedTransport`, `MockHidTransport`), `agnoperiph-cli list` & `probe`.
2. **Phase 2:** `driver-logitech` & `driver-lamzu` (mock-first tests → read-only live probe → safe write validation).
3. **Phase 3:** ATK RS7 protocol implementation from documented references, `docs/protocols/atk-rs7.md`, `driver-atk` with fixture tests, live CLI validation.
4. **Phase 4:** `crates/ui` with `gpui-kit` (device sidebar, capability-driven mouse & ATK RS7 Hall Effect editors, collapsible live HID packet inspector).

During active iteration, run targeted crate tests (`cargo test -p <crate>` or `.\scripts\dev.ps1 test <crate>`) rather than the entire workspace on every edit.

Before completing any phase or milestone, run and verify (or `.\scripts\dev.ps1 check`):
```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
For UI changes, also run and view the screenshots (§6.1):
```powershell
cargo test -p agnoperiph --test visual -- --ignored --nocapture
```

### 7.1 Target Directory & Build Cache Hygiene
- `Cargo.toml` sets `[profile.dev] debug = "line-tables-only"` for dependencies, workspace crates and test binaries. It keeps Windows `.pdb`/`.rlib` sizes manageable while panics and backtraces keep file and line numbers.
- Keep incremental compilation on (the dev default). The GPUI edit/test loop depends on it.
- The dev profile builds dependencies and the `agnoperiph` UI crate at `opt-level = 1`. At `opt-level = 0`, GPUI render functions need more than the 1 MiB Windows main-thread stack, and debug builds crash at launch with `thread 'main' has overflowed its stack`. Do not lower it. `heaviest_screens_render_within_the_main_thread_stack` (`crates/ui/tests/ui.rs`) reruns the heaviest screens on a 512 KiB stack to guard it.
- **Never touch or delete `target/release/`**, and never run a bare `cargo clean` (it wipes release builds).
- Keep local release builds rare. CI publishes the installer.
- To reclaim disk space:
  ```powershell
  .\scripts\dev.ps1 prune       # deletes only target\debug\incremental and prints the space freed
  .\scripts\dev.ps1 sweep 14    # weekly: removes artifacts older than 14 days (needs cargo-sweep)
  .\scripts\dev.ps1 clean-dev   # bigger hammer: cargo clean --profile dev
  ```
  `sweep` does not install `cargo-sweep`. Install it yourself with `cargo install cargo-sweep`.
