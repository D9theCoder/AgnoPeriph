# UI redesign mockup

**Status:** final design direction for the `crates/ui` visual refresh. Approved on 2026-11-27; implemented, merged into `main` (`85221bb`), and pushed the same day (verified against mock data).

Open `mockup.html` in a browser. It is an interactive HTML prototype of the 1240×820 app window, built against mock data. It covers every tab of all six mock devices: three keyboards in their real layouts and three mice. It is not production code. It shows the target look and behaviour, not GPUI APIs.

The overall direction follows Wootility: a calm, tasteful app with one clear main object per screen, and nothing like the extra features G Hub piles on.

## Colour

Every colour has one job. Colour never carries meaning on its own; each state also has a shape, a ring, a fill level or text.

| Role | Dark theme | Light theme | Used for |
| --- | --- | --- | --- |
| Accent (default: Cyan) | `#47B4FF`, dark text `#04121F` on top | `#0067B3`, white text on top | Selected keys, selected segments and tabs, switches that are on, slider fill, the "N keys selected" count, the Apply button, the Rapid Trigger dot, remapped keys and changed button actions. Nothing decorative. |
| Attention (orange) | `#FF8A3D` | `#A84A00` | "Changed but not applied": the orange keycap edge, the dot in the unsaved-changes bar, low battery. |
| Danger (red) | `#FF7A7A` | `#CF3434` | Destructive actions only, for example Remove. |
| LED colours | device data | device data | Keys on the Lighting tab. These are never restyled by the theme or the accent. |

The cyan accent comes from the glowing crossbar in the app logo.

**Accent presets.** Settings → Appearance offers five accents:

| Preset | Dark theme | Light theme |
| --- | --- | --- |
| Cyan (default) | `#47B4FF` | `#0067B3` |
| Violet | `#A98BFF` | `#6A3FD0` |
| Green | `#3DDC84` | `#0E7342` |
| Pink | `#FF6FB5` | `#B4165F` |
| Gold | `#F5C542` | `#8A6200` |

- Each light-theme colour reaches at least 4.5:1 contrast as text on the light background.
- There is no free colour picker.
- Orange and red are not offered, because they already have meanings.

**Surfaces.** Regions are separated by background tone rather than borders.

| Surface | Dark theme | Light theme |
| --- | --- | --- |
| Base | `#15171C` | `#ECEEF2` |
| Sidebar | `#191C21` | `#F4F5F8` |
| Panels | `#1D2026` | `#FFFFFF` |
| Controls on panels | `#262A31` | `#E1E4EA` |
| Keyboard case | `#0E1013` | aluminium `#CFD4DC` |

## Type

- **Saira:** device name, panel titles, keycap legends, and large mm/DPI readouts with tabular figures.
- **Figtree:** everything else.
- **Panel titles:** sentence case (for example "Key tester", "Rapid Trigger"), Saira SemiBold at 16 px with no extra letter spacing. This replaces the mockup's tracked all-caps titles.

## Layout and components

- **Shell:** title bar, a device sidebar with a line icon for each device and its connection and battery, and the main area.
  - App settings (close to tray, start with Windows, low-battery alerts, theme, accent, updates) live in a Settings view, reached from the sidebar footer.
  - Game profiles are reached from the sidebar footer too.
- **Device header:** the device name with brand, connection, battery and firmware chips. Below it, the preset or onboard-profile switcher (only on devices that have one), then the tabs. A device with a single tab shows no tab bar.
- **Tabs come from capabilities.** A device shows only the tabs its driver supports (see the table below). No tab is ever shown greyed out.
- **Unsaved changes:** a bar docked at the bottom with the change count, the areas changed, the dry-run state, Undo, Discard and Apply. Apply is the only primary button. Every device keeps its own pending changes.
- **Empty states:** a panel that can't be used yet explains why and offers the next step (for example "Select all keys"). It never shows greyed-out dead values.
- **Motion:** only in response to the user (key tester, switches). It respects reduced motion.

### Keyboards

The keyboard is drawn in its real physical layout, inside a case, as the main element of every keyboard tab. The case's bottom edge holds the legend. Each keyboard has its own layout table:

| Keyboard | Layout | Notes |
| --- | --- | --- |
| ATK RS7 | 75% | F-row gap, Del, nav column (Ins, PgUp, PgDn) |
| Keychron Q1 | 75% with knob | Follows the 6 × 16 VIA matrix: nav column PgUp, PgDn, Home; round knob top right |
| Wooting 60HE | 60% ANSI | No F row, so no gap above the number row |

What the keys show depends on the tab. On every tab, a key with an unapplied change gets an orange keycap edge.

- **Triggers:**
  - A key shows a number only where it differs from the most common actuation ("Unmarked keys use 1.20 mm").
  - A thin bar along the bottom of each key shows its actuation depth. A Rapid Trigger key ends that bar with an accent dot.
  - A key with an advanced function wears a round badge on its top-right corner with that function's icon. These are the app's own icons from `crates/ui/assets/icons` (RS, SOCD, DKS, MT, TGL). The badge sits on the corner, not on the keycap face, so it never covers a legend such as "Esc" or a value. On a selected key the badge turns accent.
  - The legend shows the icons of the types in use, labelled "Paired". Hovering it names them.
  - Hovering a pair in the inspector outlines its keys on the board.
- **Paired keys inspector (RS7):** a type selector whose menu shows each type's icon, name, one-line summary and count. The selected type shows its keys and settings:
  - SOCD: the key pair, "When both are held", the pair's actuation and its Rapid Trigger switch.
  - Dynamic keystroke: the four travel stages and the action on each.
  - Rapid switching, Mod-tap and Toggle: the keys and the type's summary.
- **Key tester:** a vertical travel gauge with mm ticks and hatched dead zones. Its scale follows the keyboard's travel (3.4 mm on the RS7, 4.0 mm on the Wooting). The actuation point is drawn as a glowing "crossbar". Pressing a real key animates the gauge and fills the same key on the board. On keyboards without paired keys, the tester takes the inspector's place.
- **Tool panels:** Actuation (large readout, slider with a mm ruler, Use for all keys, dead zones where the keyboard has them) and Rapid Trigger (switch, press and lift sensitivity, with the range printed under the sliders).
- **Keybinds:** the board shows each key's binding on the chosen layer.
  - A remapped key gets an accent tint, its new label, and the original legend small at the bottom.
  - On Fn and Mac layers, keys that fall through to the base layer are dimmed.
  - Layers are a segmented control under the board. One key is selected at a time.
  - The Key inspector shows "Caps → Ctrl" as two keycaps, a sentence that says what the key sends, and every remap on the layer as clickable chips.
  - The Assign panel has a search field and categories (Basic, Media, Mouse, Macro, Layer), plus Disable and Restore default.
- **Lighting:** a Lamp (RS7) or Backlight panel next to the board (zones only where the device has them, brightness, speed), then the Effect list and the Color panel.
  - Effects are a three-column list. Each effect has a line icon, and names are never truncated. Each device lists only its own effects.
  - The Color panel has a colour square, a hue strip, swatches and a hex field, and paints the selected keys when the effect is Customization.
- **Macros:** a slot list (M1 to M16, with names and step counts) and a memory meter, next to a step editor.
  - Steps read as sentences: "Press Shift", "Tap End", "Wait 20 ms". Each step can be removed, and delays have a stepper.
  - Record, Clear, + Key and + Delay sit around the steps.
  - The memory meter shows the shared budget (512 bytes on the RS7, 1024 on the Q1) or the per-slot budget (384 bytes on the Atlantis).
  - The foot of the editor says where to assign the macro.
- **Advanced (RS7):** four panels: Key layout, Function keys, Key conflict, and Precision and polling. Every switch has the same one-line description as today's tooltips. Turning on Extreme mode changes every actuation and Rapid Trigger step to 0.01 mm.

### Mice

- **Sensor:** DPI stages as tiles with large numbers. The active stage is in the accent colour. On mice with stage LEDs (Atlantis), each tile shows its colour dot. A doubling ruler (100 to 25.6K) shows where each stage sits. Reporting and Sensor panels list only the settings the mouse has, each with a one-line description.
- **Buttons:** a top-down drawing of the mouse with numbered buttons, next to the list of assignments. Selecting a button in either place highlights it in both. A changed button shows its new action in the accent colour. The Assign panel is the same as on the Keybinds tab, starting on the Mouse category.
- **Lighting:** the mouse drawing shows each zone in its current colour (the underglow, scroll wheel and logo on the Basilisk). Clicking a zone or the zone segment picks it. The Lamp panel takes the zone's name. The Effect and Color panels below are the same as on keyboards.

### Tabs per device

| Device | Tabs | Profile row |
| --- | --- | --- |
| ATK RS7 | Triggers & paired keys, Keybinds, Lighting, Macros, Advanced | Preset |
| Keychron Q1 | Keybinds, Lighting, Macros | none |
| Wooting 60HE | Triggers, Lighting | Onboard profile |
| Lamzu Atlantis | Sensor, Buttons, Macros | Onboard profile |
| Razer Basilisk V3 | Sensor, Lighting | none |
| Pulsar X2 | Sensor only (no tab bar) | Onboard profile |

## Decisions

These were open questions before. Each is now decided, from the driver and mock code where it has the answer.

- **Rapid Trigger range:** 0.02–3.40 mm on the RS7 (`RS7_RT_MIN` and `RS7_RT_MAX`) and 0.10–2.35 mm on the Wooting. Sliders and steppers move in the keyboard's step (0.02 mm, or 0.01 mm in Extreme mode on the RS7; 0.05 mm on the Wooting).
- **Actuation range:** 0.10–3.40 mm on the RS7 and 0.10–4.00 mm on the Wooting.
- **SOCD mode labels:** kept: "Last pressed wins", "Neither key sends", and "[key] always wins", which takes the pair's own key names. They describe what the code does.
- **Paired-key type names:** Rapid switching, SOCD, Dynamic keystroke, Mod-tap, Toggle. The summaries are the ones in `hall_effect_panel.rs`.
  - "Instant Release" is renamed SOCD, because that is the term players search for.
  - "Hold / Click" and "Toggle Switch" become Mod-tap and Toggle.
- **Effect names:** sentence case, "Close" → "Off", and "Single Color Constant" → "Single color". Generic RGB effects use one shared set of names on every brand: Off, Single color, Breathing, Spectrum cycle, Wave, Reactive.
- **Effect icons:** drawn from the effect names. They are a starting point. Compare them against the real animations when implementing.
- **Mouse tabs:** named after capabilities (Sensor, Buttons, Lighting, Macros), as listed in the table above.
- **Settings and Advanced descriptions:** reuse the existing tooltip text from `keyboard_config_panel.rs`. The app settings keep the mockup's sentences.
- **Removed from today's UI:**
  - the Key 1 / Key 2 pickers, replaced by Add;
  - the "was …" notes;
  - the profile line under each sidebar device.
- **Device ranges in the mock:** DPI ranges come from the drivers (Pulsar 10–32,000, Atlantis 50–26,000, Basilisk up to 26,000), as do the polling lists and the four onboard profiles on the Pulsar and the Atlantis.

**Mock values, not taken from code:** the Basilisk minimum DPI (100), the Atlantis stage LED colours, the RS7 polling default (1K), the default DPI stages, the remaps, macros and DKS actions, and the paired-key counts.

**Known close call:** with the Gold accent, a selected key with an unapplied change shows a gold fill next to an orange edge. The two still differ in shape (fill versus edge), but check them on real screens.

## What the design assumes from the backend

The mockup shows the target. These parts need driver or model work before the UI can show them for real. They are noted here, not done.

- **Physical layouts:** the UI needs a layout table (key position and width) per keyboard model. Today the Wooting and Keychron pages fall back to a row and column matrix.
- **Wooting writes:** the Wooting page is drawn as editable. Check that the driver can apply actuation and Rapid Trigger before enabling it.
- **Atlantis buttons:** the driver reads key bindings (`read_key_binding`). The Buttons tab also needs a write path and a list of the actions the firmware accepts.
- **Atlantis DPI stage colours:** shown as dots on the tiles. Confirm that the driver reads them.
- **Basilisk zones:** the zone drawing assumes the three Razer zones (Mouse, Scroll Wheel, Logo) that the mock reports.

## GPUI feasibility notes

Checked against gpui-fast 0.1.19:

- **Custom fonts:** `add_fonts` exists.
- **Tabular figures:** `FontFeatures` exists.
- **Letter spacing and font width:** no API was found. The narrow keycap legends need a static width cut of Saira, or another workaround.
- **Glow on the actuation line and the mouse underglow:** `BoxShadow` has `blur_radius`, so a blurred, coloured shadow on a thin bar gives the glow.
- **Type icons on keycaps:** `advanced_svg()` in `icons.rs` already renders these SVGs tinted, so the keycap corner can reuse it.
- **Mouse drawing:** needs a custom element or a bundled SVG per mouse shape, with hit areas per button.
- **Theme tokens:** all colours above belong in the theme or token layer, as the design guide requires. Views must not use raw hex values. An accent preset should then just be a different token set.

Saira and Figtree are Google Fonts under the SIL Open Font License, so both can be bundled.

## Screenshots

The page reads its starting state from the URL hash. Combine any of these flags with `-`:

- **Device:** `rs7` (default), `q1`, `wooting`, `atlantis`, `basilisk`, `x2` (`mouse` is an alias for `x2`)
- **Tab:** `triggers`, `keybinds`, `lighting`, `macros`, `advanced`, `sensor`, `buttons`. A tab the device lacks is ignored.
- **Other views:** `settings`, `games`
- **Theme:** `light` (default is dark)
- **Accent:** `violet`, `green`, `pink`, `gold` (default is cyan)
- **Paired keys:** `rs`, `socd`, `dks`, `mt`, `tgl` pick the inspector type; `menu` opens the type menu
- **Keybinds:** `fn` opens the first Fn layer
- **Mouse lighting zone:** `scroll`, `logo`
- **Selection:** `empty` starts with no keys selected
- **Unsaved changes:** `dirty` starts with one pending change on the current tab
- **Capture mode:** `shot` removes the window frame

For example, `#q1-keybinds-fn-light` or `#atlantis-buttons`.

To render one with headless Edge:

```powershell
$edge = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
$page = "file:///D:/Coding/AgnoPeriph/docs/design/ui-redesign/mockup.html#triggers-light-shot"
& $edge --headless=new --disable-gpu --hide-scrollbars --window-size=1240,820 --force-device-scale-factor=1.5 --virtual-time-budget=6000 --user-data-dir="$env:TEMP\edge-shot" --screenshot="$env:TEMP\triggers-light.png" $page
```

Run it from PowerShell. From Git Bash, Edge exits without writing a file.
