# UI redesign mockup

**Status:** final design direction for the `crates/ui` visual refresh. Approved on 2026-11-27. Implementation is deferred.

Open `mockup.html` in a browser. It is an interactive HTML prototype of the 1240×820 app window, built against mock data. It is not production code. It shows the target look and behaviour, not GPUI APIs.

The overall direction follows Wootility: a calm, tasteful app with one clear main object per screen, and nothing like the extra features G Hub piles on.

## Colour

Every colour has one job. Colour never carries meaning on its own; each state also has a shape, a ring, a fill level or text.

| Role | Dark theme | Light theme | Used for |
| --- | --- | --- | --- |
| Accent (default: Cyan) | `#47B4FF`, dark text `#04121F` on top | `#0067B3`, white text on top | Selected keys, selected segments and tabs, switches that are on, slider fill, the "N keys selected" count, the Apply button. Nothing decorative. |
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

- **Saira:** device name, all-caps panel titles (for example KEY TESTER, RAPID TRIGGER), keycap legends, and large mm/DPI readouts with tabular figures.
- **Figtree:** everything else.
- **Caps:** reserved for panel titles, not ordinary labels.

## Layout and components

- **Shell:** title bar, a device sidebar with a line icon for each device and its connection and battery, and the main area.
  - App settings (close to tray, start with Windows, low-battery alerts, theme, accent, updates) live in a Settings view, reached from the sidebar footer.
  - Game profiles are reached from the sidebar footer too.
- **Device header:** the device name with brand, connection, battery and firmware chips. Below it, the preset or onboard-profile switcher, then the tabs.
- **Keyboard:** drawn as the main element, inside a physical case. The case's bottom edge holds the legend. What the keys show depends on the tab:
  - **Triggers:** a key shows a number only where it differs from the most common actuation. A thin bar shows the actuation depth on every key. A dot marks Rapid Trigger and a link icon marks paired keys.
  - **Lighting:** each key shows its LED colour. Selected keys get an outline.
  - **On every tab:** a key with an unapplied change gets an orange keycap edge.
- **Paired keys inspector:** a type selector (SOCD and the other types). The SOCD view lists the configured pair, a "When both are held" choice, the pair's actuation, and its Rapid Trigger switch.
- **Key tester:** a vertical travel gauge with mm ticks and hatched dead zones. The actuation point is drawn as a glowing "crossbar". Pressing a real key animates the gauge and fills the same key on the board.
- **Tool panels:** Actuation (large readout, slider with a mm ruler, Use for all keys, dead zones) and Rapid Trigger (switch, press and lift sensitivity).
- **Empty states:** a panel that can't be used yet explains why and offers the next step (for example "Select all keys"). It never shows greyed-out dead values.
- **Lighting:** a Lamp panel next to the board (zone, brightness, speed), then the Effect list and the Color panel.
  - Effects are a three-column list. Each effect has a line icon, and names are never truncated.
  - The Color panel has a colour square, a hue strip, swatches and a hex field, and paints the selected keys.
- **Mouse:** DPI stages as tiles with large numbers. The active stage is in the accent colour. A doubling ruler (100 to 25.6K) shows where each stage sits. Reporting and Sensor panels list each setting with a one-line description.
- **Unsaved changes:** a bar docked at the bottom with the change count, the areas changed, the dry-run state, Undo, Discard and Apply. Apply is the only primary button.
- **Motion:** only in response to the user (key tester, switches). It respects reduced motion.

## Not mocked up yet

- Keybinds, Macros and Advanced tabs. Keybinds should highlight remapped keys, using the same per-tab approach as the other tabs.
- Mouse Buttons and Lighting tabs.
- Wooting 60HE and Keychron Q1 pages.

## Needs sign-off before implementation

- **Effect names:** sentence case, "Close" → "Off", and "Single Color Constant" → "Single color".
- **Effect icons:** guessed from the names. Check them against the real animations.
- **SOCD mode labels:** "Last pressed wins", "Neither key sends", "A always wins", "D always wins".
- **Paired-key type names:** guessed from the current icons. The counts in the mockup are mock values.
- **Invented values:** the Rapid Trigger slider range (0.02–1.00 mm), the mouse tab names, and the Settings descriptions.
- **Removed from today's UI:** the "Instant Release" label, the Key 1 / Key 2 pickers (replaced by Add), the "was …" notes, and the profile line under each sidebar device.

## GPUI feasibility notes

Checked against gpui-fast 0.1.19:

- **Custom fonts:** `add_fonts` exists.
- **Tabular figures:** `FontFeatures` exists.
- **Letter spacing and font width:** no API was found. The tracked caps titles and the narrow keycap legends need static width cuts of Saira, or another workaround.
- **Glow on the actuation line:** not checked yet.
- **Theme tokens:** all colours above belong in the theme or token layer, as the design guide requires. Views must not use raw hex values. An accent preset should then just be a different token set.

Saira and Figtree are Google Fonts under the SIL Open Font License, so both can be bundled.

## Screenshots

The page reads its starting state from the URL hash. Combine any of these flags:

- **View:** `lighting`, `mouse`, `settings`, `games`
- **Theme:** `light` (default is dark)
- **Accent:** `violet`, `green`, `pink`, `gold` (default is cyan)
- **Selection:** `empty` starts with no keys selected
- **Unsaved changes:** `dirty` starts with one pending change
- **Capture mode:** `shot` removes the window frame

To render one with headless Edge:

```powershell
$edge = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
$page = "file:///D:/Coding/AgnoPeriph/docs/design/ui-redesign/mockup.html#triggers-light-shot"
& $edge --headless=new --disable-gpu --hide-scrollbars --window-size=1240,820 --force-device-scale-factor=1.5 --virtual-time-budget=6000 --screenshot="$env:TEMP\triggers-light.png" $page
```
