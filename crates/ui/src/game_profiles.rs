//! Cross-brand Game Profiles, custom preset naming, software profile storage,
//! Windows installed game discovery (Steam, Epic, Riot, Uninstall registry),
//! and lightweight active-process monitoring.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use agnoperiph_core::{
    DeviceId, DksBinding, KeyHallEffectConfig, KeyboardConfigSnapshot, ModTapBinding, ProfileSlot,
    RapidSwitchPair, RgbZoneState, SocdPair, ToggleKeyBinding, TravelHundredthsMm,
};
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessesToUpdate, System};

/// Number of default software profile slots provisioned for devices without multi-slot hardware profiles.
pub const DEFAULT_SOFTWARE_PROFILE_SLOTS: u8 = 4;

/// Storefront / source where an installed game was discovered on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GameSource {
    Steam,
    Epic,
    Riot,
    HoyoPlay,
    Gog,
    Xbox,
    GameBar,
    Installed,
    Manual,
}

impl fmt::Display for GameSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Steam => f.write_str("Steam"),
            Self::Epic => f.write_str("Epic Games"),
            Self::Riot => f.write_str("Riot Games"),
            Self::HoyoPlay => f.write_str("HoYoPlay"),
            Self::Gog => f.write_str("GOG"),
            Self::Xbox => f.write_str("Xbox"),
            Self::GameBar => f.write_str("Detected"),
            Self::Installed => f.write_str("Windows"),
            Self::Manual => f.write_str("Manual"),
        }
    }
}

/// One installed Windows game or application detected by the local scanner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DetectedGame {
    pub name: String,
    pub exe_name: String,
    pub exe_path: String,
    pub source: GameSource,
}

impl DetectedGame {
    pub fn new(
        name: impl Into<String>,
        exe_name: impl Into<String>,
        exe_path: impl Into<String>,
        source: GameSource,
    ) -> Self {
        Self {
            name: name.into(),
            exe_name: exe_name.into(),
            exe_path: exe_path.into(),
            source,
        }
    }
}

/// One cross-brand automatic Game Profile mapping an executable to per-device preset slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GameProfile {
    pub id: String,
    pub name: String,
    /// Executable filename matched case-insensitively (e.g. `VALORANT-Win64-Shipping.exe`).
    pub exe_name: String,
    /// Optional full executable path for display.
    pub exe_path: String,
    pub enabled: bool,
    /// Maps `DeviceId::as_str()` to target 0-based profile slot index.
    pub device_targets: BTreeMap<String, u8>,
}

impl GameProfile {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        exe_name: impl Into<String>,
        exe_path: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            exe_name: exe_name.into(),
            exe_path: exe_path.into(),
            enabled: true,
            device_targets: BTreeMap::new(),
        }
    }

    pub fn normalized_exe(&self) -> String {
        normalize_exe_name(&self.exe_name)
    }
}

/// Snapshot of device settings saved inside a host-managed software profile slot
/// (used for devices like ATK RS7 whose firmware only has a single onboard slot).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SavedKeyboardPreset {
    /// `true` once Hall Effect state (per-key tuning & paired/advanced keys) has been saved here.
    pub has_hall_effect: bool,
    pub global_actuation: Option<TravelHundredthsMm>,
    pub top_deadzone: Option<TravelHundredthsMm>,
    pub bottom_deadzone: Option<TravelHundredthsMm>,
    /// Per-key actuation and Rapid Trigger configuration across the keyboard matrix.
    pub key_configs: Vec<KeyHallEffectConfig>,
    pub socd_pairs: Vec<SocdPair>,
    pub rapid_switch_pairs: Vec<RapidSwitchPair>,
    pub dks_bindings: Vec<DksBinding>,
    pub mod_tap_bindings: Vec<ModTapBinding>,
    pub toggle_bindings: Vec<ToggleKeyBinding>,
    pub rgb_zones: Vec<RgbZoneState>,
    pub keyboard_config: Option<KeyboardConfigSnapshot>,
}

impl SavedKeyboardPreset {
    /// Returns `true` when nothing has been saved into this preset yet.
    pub fn is_empty(&self) -> bool {
        !self.has_hall_effect
            && self.global_actuation.is_none()
            && self.top_deadzone.is_none()
            && self.bottom_deadzone.is_none()
            && self.key_configs.is_empty()
            && self.socd_pairs.is_empty()
            && self.rapid_switch_pairs.is_empty()
            && self.dks_bindings.is_empty()
            && self.mod_tap_bindings.is_empty()
            && self.toggle_bindings.is_empty()
            && self.rgb_zones.is_empty()
            && self.keyboard_config.is_none()
    }
}

/// One host-managed software profile slot for a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SoftwareProfileSlot {
    pub slot: u8,
    pub name: String,
    pub saved: SavedKeyboardPreset,
}

impl SoftwareProfileSlot {
    pub fn new(slot: u8, name: impl Into<String>) -> Self {
        Self {
            slot,
            name: name.into(),
            saved: SavedKeyboardPreset::default(),
        }
    }
}

/// Collection of host-managed software profiles for one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DeviceSoftwareProfiles {
    pub active_slot: u8,
    pub slots: Vec<SoftwareProfileSlot>,
}

impl Default for DeviceSoftwareProfiles {
    fn default() -> Self {
        Self {
            active_slot: 0,
            slots: vec![
                SoftwareProfileSlot::new(0, "Default"),
                SoftwareProfileSlot::new(1, "FPS / Gaming"),
                SoftwareProfileSlot::new(2, "Typing"),
                SoftwareProfileSlot::new(3, "Custom"),
            ],
        }
    }
}

/// Persistent AgnoPeriph user configuration (custom preset names, software profiles, game profiles).
///
/// Missing fields fall back to their defaults so configs written by older builds still load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct AppConfig {
    pub auto_switch_enabled: bool,
    /// Show a Windows toast notification when opening or closing a game switches presets.
    pub notify_on_game_switch: bool,
    /// Custom user-assigned names for any device's profile slots: `device_id -> slot -> label`.
    pub preset_names: BTreeMap<String, BTreeMap<u8, String>>,
    /// Host-managed software profiles for devices without multi-slot hardware memory.
    pub software_profiles: BTreeMap<String, DeviceSoftwareProfiles>,
    /// Automatic game-triggered cross-brand profiles.
    pub game_profiles: Vec<GameProfile>,
    /// Fallback per-device profile slots restored when no configured game is running.
    pub default_targets: BTreeMap<String, u8>,
    /// Closing the window keeps AgnoPeriph running in the notification area.
    pub close_to_tray: bool,
    /// Launch AgnoPeriph when the user signs in to Windows.
    pub start_with_windows: bool,
    /// Show a Windows notification when a wireless device's battery runs low.
    #[serde(default = "default_true")]
    pub low_battery_alerts: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_switch_enabled: true,
            notify_on_game_switch: true,
            preset_names: BTreeMap::new(),
            software_profiles: BTreeMap::new(),
            game_profiles: Vec::new(),
            default_targets: BTreeMap::new(),
            close_to_tray: true,
            start_with_windows: false,
            low_battery_alerts: true,
        }
    }
}

impl AppConfig {
    /// Returns the default Windows configuration file path (`%APPDATA%\AgnoPeriph\config.json`).
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("AgnoPeriph").join("config.json"))
    }

    /// Loads config from disk, returning [`Self::default`] when absent or unreadable.
    pub fn load_or_default(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Saves config cleanly to disk.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        }
        let body =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))?;
        fs::write(path, body).map_err(|e| format!("write config: {e}"))
    }

    /// Returns the display name for one device profile slot, honoring user renames.
    pub fn preset_name(&self, device: &DeviceId, slot: ProfileSlot) -> String {
        if let Some(name) = self
            .preset_names
            .get(device.as_str())
            .and_then(|map| map.get(&slot.index()))
            .filter(|s| !s.trim().is_empty())
        {
            return name.clone();
        }
        if let Some(sw) = self.software_profiles.get(device.as_str())
            && let Some(entry) = sw.slots.iter().find(|s| s.slot == slot.index())
            && !entry.name.trim().is_empty()
        {
            return entry.name.clone();
        }
        slot.to_string()
    }

    /// Renames a device's profile slot (for either hardware or software profiles).
    pub fn set_preset_name(
        &mut self,
        device: &DeviceId,
        slot: ProfileSlot,
        name: impl Into<String>,
    ) {
        let trimmed = name.into().trim().to_owned();
        if trimmed.is_empty() {
            if let Some(map) = self.preset_names.get_mut(device.as_str()) {
                map.remove(&slot.index());
            }
        } else {
            self.preset_names
                .entry(device.as_str().to_owned())
                .or_default()
                .insert(slot.index(), trimmed.clone());
            if let Some(sw) = self.software_profiles.get_mut(device.as_str())
                && let Some(entry) = sw.slots.iter_mut().find(|s| s.slot == slot.index())
            {
                entry.name = trimmed;
            }
        }
    }

    /// Ensures host-managed software profiles exist for `device` and returns a mutable reference.
    pub fn ensure_software_profiles(&mut self, device: &DeviceId) -> &mut DeviceSoftwareProfiles {
        self.software_profiles
            .entry(device.as_str().to_owned())
            .or_default()
    }

    /// Adds or updates a Game Profile.
    pub fn upsert_game_profile(&mut self, profile: GameProfile) {
        if let Some(existing) = self.game_profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            self.game_profiles.push(profile);
        }
    }

    /// Returns the Game Profile with `id`.
    pub fn game_profile(&self, id: &str) -> Option<&GameProfile> {
        self.game_profiles.iter().find(|p| p.id == id)
    }

    /// Returns a mutable Game Profile with `id`.
    pub fn game_profile_mut(&mut self, id: &str) -> Option<&mut GameProfile> {
        self.game_profiles.iter_mut().find(|p| p.id == id)
    }

    /// Returns an id derived from `name` that no existing Game Profile uses.
    pub fn unique_game_profile_id(&self, name: &str) -> String {
        let base: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_owned();
        let base = if base.is_empty() {
            "game".to_owned()
        } else {
            base
        };
        let mut candidate = base.clone();
        let mut n = 2;
        while self.game_profile(&candidate).is_some() {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        candidate
    }

    /// Deletes a Game Profile by id.
    pub fn remove_game_profile(&mut self, id: &str) {
        self.game_profiles.retain(|p| p.id != id);
    }

    /// Renames a Game Profile by id.
    pub fn rename_game_profile(&mut self, id: &str, name: impl Into<String>) {
        let trimmed = name.into().trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        if let Some(profile) = self.game_profiles.iter_mut().find(|p| p.id == id) {
            profile.name = trimmed;
        }
    }

    /// Finds the first enabled game profile whose executable is present in `running_exes`.
    pub fn match_running_game<'a>(
        &'a self,
        running_exes: &HashSet<String>,
    ) -> Option<&'a GameProfile> {
        if !self.auto_switch_enabled {
            return None;
        }
        self.game_profiles
            .iter()
            .find(|profile| profile.enabled && running_exes.contains(&profile.normalized_exe()))
    }
}

/// Normalizes an executable path or filename to a lowercased `.exe` filename.
pub fn normalize_exe_name(input: &str) -> String {
    let trimmed = input.trim().trim_matches('"');
    let leaf = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed);
    leaf.to_ascii_lowercase()
}

/// Lightweight native Windows process watcher using `sysinfo` (no subprocess spawning).
pub struct ActiveProcessWatcher {
    system: System,
}

impl fmt::Debug for ActiveProcessWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveProcessWatcher")
            .finish_non_exhaustive()
    }
}

impl Default for ActiveProcessWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ActiveProcessWatcher {
    pub fn new() -> Self {
        Self {
            system: System::new(),
        }
    }

    /// Refreshes the running process table and returns lowercased executable filenames.
    pub fn running_exes(&mut self) -> HashSet<String> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        self.system
            .processes()
            .values()
            .filter_map(|proc| proc.name().to_str())
            .map(str::to_ascii_lowercase)
            .collect()
    }
}

/// Discovers installed Windows games across Steam, Epic Games, Riot Games, HoYoPlay /
/// HoYoverse, GOG, Xbox, Windows GameConfigStore, Uninstall registry, and common game folders.
///
/// Mirrors the multi-source local discovery strategy used by NVIDIA App without requiring
/// administrator elevation.
pub fn discover_installed_games() -> Vec<DetectedGame> {
    let mut found: Vec<DetectedGame> = Vec::new();
    let mut seen_exes: HashSet<String> = HashSet::new();

    let mut push_unique = |game: DetectedGame| {
        let key = normalize_exe_name(&game.exe_name);
        let is_exe = Path::new(&key)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        if !key.is_empty() && is_exe && !is_helper_exe(&key) && seen_exes.insert(key) {
            found.push(game);
        }
    };

    for game in discover_steam_games() {
        push_unique(game);
    }
    for game in discover_epic_games() {
        push_unique(game);
    }
    for game in discover_riot_games() {
        push_unique(game);
    }
    for game in discover_hoyoplay_games() {
        push_unique(game);
    }
    for game in discover_gog_games() {
        push_unique(game);
    }
    for game in discover_xbox_games() {
        push_unique(game);
    }
    for game in discover_game_config_store_games() {
        push_unique(game);
    }
    for game in discover_registry_games() {
        push_unique(game);
    }
    for game in discover_common_game_folders() {
        push_unique(game);
    }

    found.sort_by_key(|a| a.name.to_lowercase());
    found
}

fn is_helper_exe(file_name_lower: &str) -> bool {
    const IGNORED_SUBSTRINGS: &[&str] = &[
        "unins",
        "uninstall",
        "crash",
        "report",
        "redist",
        "vcredist",
        "dxsetup",
        "easyanticheat",
        "battleye",
        "updater",
        "setup",
        "installer",
        "cefprocess",
        "webhelper",
        "vc_redist",
        "ue4prereq",
        "ueprereq",
        "touchup",
        "activation",
        "launcher",
        "zfgamebrowser",
        "hyp_",
        "unitycrashhandler",
        "qtwebengine",
        "bootstrapper",
        "benchmark",
        "configtool",
        "settings",
        "helper",
        "service",
        "agent",
        "daemon",
        "overlay",
    ];
    const IGNORED_EXACT: &[&str] = &[
        "explorer.exe",
        "cmd.exe",
        "powershell.exe",
        "pwsh.exe",
        "conhost.exe",
        "chrome.exe",
        "msedge.exe",
        "firefox.exe",
        "brave.exe",
        "opera.exe",
        "discord.exe",
        "spotify.exe",
        "steam.exe",
        "epicgameslauncher.exe",
        "riotclientservices.exe",
        "riotclientux.exe",
        "hyp.exe",
        "agnoperiph.exe",
        "obs64.exe",
        "code.exe",
        "notepad.exe",
        "taskmgr.exe",
    ];
    IGNORED_EXACT.contains(&file_name_lower)
        || IGNORED_SUBSTRINGS
            .iter()
            .any(|needle| file_name_lower.contains(needle))
}

fn present_drive_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for letter in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if root.exists() {
            roots.push(root);
        }
    }
    roots
}

/// Maps well-known game executable names or install paths to clean display titles.
pub fn friendly_game_name(exe_path: &Path) -> String {
    let exe_lower = exe_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let full_lower = exe_path.to_string_lossy().to_ascii_lowercase();

    match exe_lower.as_str() {
        "genshinimpact.exe" | "yuanshen.exe" => return "Genshin Impact".to_owned(),
        "starrail.exe" => return "Honkai: Star Rail".to_owned(),
        "zenlesszonezero.exe" => return "Zenless Zone Zero".to_owned(),
        "bh3.exe" => return "Honkai Impact 3rd".to_owned(),
        "valorant-win64-shipping.exe" => return "VALORANT".to_owned(),
        "league of legends.exe" => return "League of Legends".to_owned(),
        "cs2.exe" => return "Counter-Strike 2".to_owned(),
        "r5apex.exe" | "r5apex_dx12.exe" => return "Apex Legends".to_owned(),
        "overwatch.exe" => return "Overwatch 2".to_owned(),
        "fortniteclient-win64-shipping.exe" => return "Fortnite".to_owned(),
        "minecraft.windows.exe" => return "Minecraft".to_owned(),
        "eldenring.exe" => return "ELDEN RING".to_owned(),
        "cyberpunk2077.exe" => return "Cyberpunk 2077".to_owned(),
        "osu!.exe" => return "osu!".to_owned(),
        _ => {}
    }

    if full_lower.contains("wuthering waves") {
        return "Wuthering Waves".to_owned();
    }
    if full_lower.contains("genshin impact") {
        return "Genshin Impact".to_owned();
    }
    if full_lower.contains("star rail") {
        return "Honkai: Star Rail".to_owned();
    }
    if full_lower.contains("zenless") {
        return "Zenless Zone Zero".to_owned();
    }

    // For Unreal Engine binaries (`*-Win64-Shipping.exe`) or generic `game.exe`/`client.exe`,
    // climb past `Win64`/`Binaries`/`Client`/`Games` to find the real game folder name.
    let generic_exe = exe_lower.ends_with("-win64-shipping.exe")
        || exe_lower.ends_with("-win64-test.exe")
        || matches!(
            exe_lower.as_str(),
            "client.exe" | "game.exe" | "main.exe" | "start.exe" | "app.exe" | "shipping.exe"
        );
    if generic_exe {
        for ancestor in exe_path.ancestors().skip(1) {
            let Some(dir_name) = ancestor.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let lower = dir_name.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "win64"
                    | "win32"
                    | "binaries"
                    | "bin"
                    | "client"
                    | "game"
                    | "games"
                    | "live"
                    | "shipping"
                    | "x64"
                    | "x86"
                    | "program files"
                    | "program files (x86)"
                    | "steamapps"
                    | "common"
                    | "epic games"
                    | "riot games"
                    | "xboxgames"
                    | "content"
            ) {
                continue;
            }
            if !dir_name.trim().is_empty() {
                return dir_name.trim().to_owned();
            }
        }
    }

    exe_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Game")
        .replace(['_', '-'], " ")
        .trim()
        .to_owned()
}

fn discover_hoyoplay_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    let known: &[(&str, &str, &[&str])] = &[
        (
            "Genshin Impact",
            "GenshinImpact.exe",
            &[
                r"Program Files\Genshin Impact\Genshin Impact game\GenshinImpact.exe",
                r"Program Files\HoYoPlay\games\Genshin Impact game\GenshinImpact.exe",
                r"HoYoPlay\games\Genshin Impact game\GenshinImpact.exe",
                r"Games\Genshin Impact\Genshin Impact game\GenshinImpact.exe",
                r"Genshin Impact\Genshin Impact game\GenshinImpact.exe",
            ],
        ),
        (
            "Honkai: Star Rail",
            "StarRail.exe",
            &[
                r"Program Files\Star Rail\Games\StarRail.exe",
                r"Program Files\Star Rail\Game\StarRail.exe",
                r"Program Files\HoYoPlay\games\Star Rail Games\StarRail.exe",
                r"HoYoPlay\games\Star Rail Games\StarRail.exe",
                r"Games\Star Rail\Games\StarRail.exe",
                r"Star Rail\Games\StarRail.exe",
            ],
        ),
        (
            "Zenless Zone Zero",
            "ZenlessZoneZero.exe",
            &[
                r"Program Files\ZenlessZoneZero Game\ZenlessZoneZero.exe",
                r"Program Files\HoYoPlay\games\ZenlessZoneZero Game\ZenlessZoneZero.exe",
                r"HoYoPlay\games\ZenlessZoneZero Game\ZenlessZoneZero.exe",
                r"Games\ZenlessZoneZero Game\ZenlessZoneZero.exe",
                r"ZenlessZoneZero Game\ZenlessZoneZero.exe",
            ],
        ),
        (
            "Honkai Impact 3rd",
            "BH3.exe",
            &[
                r"Program Files\Honkai Impact 3rd\Games\BH3.exe",
                r"Program Files\HoYoPlay\games\Honkai Impact 3rd\Games\BH3.exe",
                r"HoYoPlay\games\Honkai Impact 3rd\Games\BH3.exe",
                r"Games\Honkai Impact 3rd\Games\BH3.exe",
            ],
        ),
    ];

    let roots = present_drive_roots();
    for &(name, exe_name, rel_paths) in known {
        'next_game: for root in &roots {
            for rel in rel_paths {
                let candidate = root.join(rel);
                if candidate.is_file() {
                    games.push(DetectedGame::new(
                        name,
                        exe_name,
                        candidate.to_string_lossy().into_owned(),
                        GameSource::HoyoPlay,
                    ));
                    break 'next_game;
                }
            }
        }
    }

    // Also scan any `HoYoPlay\games` directory across drives for additional titles/custom folders.
    for root in &roots {
        for sub in [r"Program Files\HoYoPlay\games", r"HoYoPlay\games"] {
            let dir = root.join(sub);
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && let Some(exe) = find_primary_exe(&path)
                    && let Some(exe_name) = exe.file_name().and_then(|s| s.to_str())
                {
                    games.push(DetectedGame::new(
                        friendly_game_name(&exe),
                        exe_name,
                        exe.to_string_lossy().into_owned(),
                        GameSource::HoyoPlay,
                    ));
                }
            }
        }
    }

    games
}

fn discover_gog_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_LOCAL_MACHINE;
        for path in [
            r"SOFTWARE\WOW6432Node\GOG.com\Games",
            r"SOFTWARE\GOG.com\Games",
        ] {
            let Ok(root) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) else {
                continue;
            };
            for key_name in root.enum_keys().flatten() {
                let Ok(sub) = root.open_subkey(&key_name) else {
                    continue;
                };
                let name = sub
                    .get_value::<String, _>("gameName")
                    .or_else(|_| sub.get_value::<String, _>("GAMENAME"));
                let exe = sub.get_value::<String, _>("exe");
                if let (Ok(name), Ok(exe)) = (name, exe) {
                    let p = PathBuf::from(&exe);
                    if p.is_file()
                        && let Some(exe_name) = p.file_name().and_then(|s| s.to_str())
                    {
                        games.push(DetectedGame::new(name, exe_name, exe, GameSource::Gog));
                    }
                }
            }
        }
    }
    games
}

fn discover_xbox_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    for root in present_drive_roots() {
        let xbox_dir = root.join("XboxGames");
        let Ok(entries) = fs::read_dir(&xbox_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("Xbox Game")
                .to_owned();
            let content = dir.join("Content");
            let search_root = if content.is_dir() { content } else { dir };
            if let Some(exe) = find_primary_exe(&search_root)
                && let Some(exe_name) = exe.file_name().and_then(|s| s.to_str())
            {
                games.push(DetectedGame::new(
                    name,
                    exe_name,
                    exe.to_string_lossy().into_owned(),
                    GameSource::Xbox,
                ));
            }
        }
    }
    games
}

/// Reads Windows' `GameConfigStore` (`HKCU\System\GameConfigStore\Children`), which automatically
/// records every DirectX/Vulkan game executable launched on the machine (including HoYoverse,
/// Kuro Games, indie, and standalone installs that don't register through Steam/Epic).
fn discover_game_config_store_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;

        let Ok(root) =
            RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"System\GameConfigStore\Children")
        else {
            return games;
        };
        for key_name in root.enum_keys().flatten() {
            let Ok(sub) = root.open_subkey(&key_name) else {
                continue;
            };
            let Ok(raw_path) = sub.get_value::<String, _>("MatchedExeFullPath") else {
                continue;
            };
            let clean = raw_path.trim().trim_matches('"');
            let path = Path::new(clean);
            if !path.is_file() {
                continue;
            }
            let lower = clean.to_ascii_lowercase();
            // Skip Windows system binaries and standard desktop app directories.
            if lower.contains("\\windows\\")
                || lower.contains("\\appdata\\")
                || lower.contains("\\programdata\\")
                || lower.contains("\\nvidia corporation\\")
                || lower.contains("\\amd\\")
                || lower.contains("\\microsoft ")
            {
                continue;
            }
            let Some(exe_name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if is_helper_exe(&exe_name.to_ascii_lowercase()) {
                continue;
            }
            let source = if lower.contains("genshin")
                || lower.contains("star rail")
                || lower.contains("zenless")
                || lower.contains("hoyoplay")
                || lower.contains("mihoyo")
            {
                GameSource::HoyoPlay
            } else {
                GameSource::GameBar
            };
            games.push(DetectedGame::new(
                friendly_game_name(path),
                exe_name,
                clean,
                source,
            ));
        }
    }
    games
}

/// Scans standard NVIDIA-App-style game library folders across all fixed drives.
fn discover_common_game_folders() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    let library_subs = [
        "Games",
        "Epic Games",
        "Riot Games",
        r"GOG Galaxy\Games",
        r"Ubisoft\Ubisoft Game Launcher\games",
        "EA Games",
        "Origin Games",
    ];
    for root in present_drive_roots() {
        for sub in library_subs {
            let dir = root.join(sub);
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let game_dir = entry.path();
                if !game_dir.is_dir() {
                    continue;
                }
                let Some(folder_name) = game_dir.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let folder_lower = folder_name.to_ascii_lowercase();
                if folder_lower.contains("riot client") || folder_lower.contains("launcher") {
                    continue;
                }
                if let Some(exe) = find_primary_exe(&game_dir)
                    && let Some(exe_name) = exe.file_name().and_then(|s| s.to_str())
                {
                    let name = friendly_game_name(&exe);
                    let display = if name.eq_ignore_ascii_case("client") || name.is_empty() {
                        folder_name.to_owned()
                    } else {
                        name
                    };
                    games.push(DetectedGame::new(
                        display,
                        exe_name,
                        exe.to_string_lossy().into_owned(),
                        GameSource::Installed,
                    ));
                }
            }
        }
    }
    games
}

fn find_primary_exe(dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();
    collect_exes(dir, 0, 3, &mut candidates);
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn collect_exes(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<(u64, PathBuf)>) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                let lower = name.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "_commonredist" | "directx" | "redist" | "support" | "__installer" | "engine"
                ) {
                    continue;
                }
            }
            collect_exes(&path, depth + 1, max_depth, out);
        } else if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
            && let Some(name) = path.file_name().and_then(|s| s.to_str())
        {
            let lower = name.to_ascii_lowercase();
            if !is_helper_exe(&lower)
                && let Ok(meta) = entry.metadata()
            {
                out.push((meta.len(), path));
            }
        }
    }
}

/// Parses Steam `libraryfolders.vdf` to extract all Steam library root paths.
pub fn parse_steam_library_paths(vdf_content: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in vdf_content.lines() {
        let tokens: Vec<&str> = line.split('"').collect();
        if tokens.len() >= 4 && tokens[1].eq_ignore_ascii_case("path") {
            let unescaped = tokens[3].replace("\\\\", "\\");
            if !unescaped.is_empty() {
                paths.push(PathBuf::from(unescaped));
            }
        }
    }
    paths
}

/// Parses a Steam `appmanifest_*.acf` file into `(display_name, installdir)`.
pub fn parse_steam_appmanifest(acf_content: &str) -> Option<(String, String)> {
    let mut name = None;
    let mut installdir = None;
    for line in acf_content.lines() {
        let tokens: Vec<&str> = line.split('"').collect();
        if tokens.len() >= 4 {
            if tokens[1].eq_ignore_ascii_case("name") {
                name = Some(tokens[3].to_owned());
            } else if tokens[1].eq_ignore_ascii_case("installdir") {
                installdir = Some(tokens[3].to_owned());
            }
        }
    }
    Some((name?, installdir?))
}

fn discover_steam_games() -> Vec<DetectedGame> {
    let mut steam_roots = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam")
            && let Ok(path) = key.get_value::<String, _>("SteamPath")
        {
            steam_roots.push(PathBuf::from(path));
        }
    }
    for fallback in [
        r"C:\Program Files (x86)\Steam",
        r"C:\Program Files\Steam",
        r"D:\Steam",
        r"D:\SteamLibrary",
    ] {
        let p = PathBuf::from(fallback);
        if p.exists() && !steam_roots.contains(&p) {
            steam_roots.push(p);
        }
    }

    let mut libraries = steam_roots.clone();
    for root in &steam_roots {
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(content) = fs::read_to_string(vdf) {
            for lib in parse_steam_library_paths(&content) {
                if !libraries.contains(&lib) {
                    libraries.push(lib);
                }
            }
        }
    }

    let mut games = Vec::new();
    for lib in libraries {
        let steamapps = lib.join("steamapps");
        let Ok(entries) = fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let is_acf = Path::new(fname)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("acf"));
            if !(fname.starts_with("appmanifest_") && is_acf) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Some((name, installdir)) = parse_steam_appmanifest(&content) else {
                continue;
            };
            if name
                .to_ascii_lowercase()
                .contains("steamworks common redist")
                || name.to_ascii_lowercase().contains("proton")
            {
                continue;
            }
            let game_dir = steamapps.join("common").join(&installdir);
            if let Some(exe) = find_primary_exe(&game_dir)
                && let Some(exe_name) = exe.file_name().and_then(|s| s.to_str())
            {
                games.push(DetectedGame::new(
                    name,
                    exe_name,
                    exe.to_string_lossy().into_owned(),
                    GameSource::Steam,
                ));
            }
        }
    }
    games
}

#[derive(Deserialize)]
struct EpicManifest {
    #[serde(rename = "DisplayName")]
    display_name: Option<String>,
    #[serde(rename = "InstallLocation")]
    install_location: Option<String>,
    #[serde(rename = "LaunchExecutable")]
    launch_executable: Option<String>,
}

/// Parses an Epic Games Launcher `.item` manifest JSON into a [`DetectedGame`].
pub fn parse_epic_manifest(json_str: &str) -> Option<DetectedGame> {
    let item: EpicManifest = serde_json::from_str(json_str).ok()?;
    let name = item.display_name?;
    let install = PathBuf::from(item.install_location?);
    let launch = item.launch_executable?;
    let full = install.join(&launch);
    let exe_name = full.file_name()?.to_str()?.to_owned();
    Some(DetectedGame::new(
        name,
        exe_name,
        full.to_string_lossy().into_owned(),
        GameSource::Epic,
    ))
}

fn discover_epic_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    let Some(program_data) = std::env::var_os("ProgramData") else {
        return games;
    };
    let manifests = PathBuf::from(program_data)
        .join("Epic")
        .join("EpicGamesLauncher")
        .join("Data")
        .join("Manifests");
    let Ok(entries) = fs::read_dir(manifests) else {
        return games;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("item")
            && let Ok(raw) = fs::read_to_string(&path)
            && let Some(game) = parse_epic_manifest(&raw)
        {
            games.push(game);
        }
    }
    games
}

fn discover_riot_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    let roots = present_drive_roots();
    let known: &[(&str, &str, &str)] = &[
        (
            "VALORANT",
            "VALORANT-Win64-Shipping.exe",
            r"Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe",
        ),
        (
            "League of Legends",
            "League of Legends.exe",
            r"Riot Games\League of Legends\Game\League of Legends.exe",
        ),
    ];
    for &(name, exe_name, rel) in known {
        for root in &roots {
            let p = root.join(rel);
            if p.is_file() {
                games.push(DetectedGame::new(
                    name,
                    exe_name,
                    p.to_string_lossy().into_owned(),
                    GameSource::Riot,
                ));
                break;
            }
        }
    }
    games
}

fn discover_registry_games() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

        let hives = [
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_CURRENT_USER,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ];

        for (hive, subkey_path) in hives {
            let Ok(root) = RegKey::predef(hive).open_subkey(subkey_path) else {
                continue;
            };
            for key_name in root.enum_keys().flatten() {
                let Ok(sub) = root.open_subkey(&key_name) else {
                    continue;
                };
                let Ok(display_name) = sub.get_value::<String, _>("DisplayName") else {
                    continue;
                };
                let Ok(icon) = sub.get_value::<String, _>("DisplayIcon") else {
                    continue;
                };
                let clean_icon = icon
                    .split(',')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                let icon_path = Path::new(clean_icon);
                if !icon_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                {
                    continue;
                }
                let lower_path = clean_icon.to_ascii_lowercase();
                // Keep discovery focused on games/launchers rather than OS/driver utilities.
                let looks_like_game = lower_path.contains("\\games\\")
                    || lower_path.contains("\\riot games\\")
                    || lower_path.contains("\\gog galaxy\\games\\")
                    || lower_path.contains("\\ubisoft\\")
                    || lower_path.contains("\\ea games\\")
                    || lower_path.contains("\\blizzard\\")
                    || lower_path.contains("\\xboxgames\\");
                if !looks_like_game || !icon_path.exists() {
                    continue;
                }
                if let Some(exe_name) = icon_path.file_name().and_then(|s| s.to_str())
                    && !is_helper_exe(&exe_name.to_ascii_lowercase())
                {
                    games.push(DetectedGame::new(
                        display_name,
                        exe_name,
                        clean_icon,
                        GameSource::Installed,
                    ));
                }
            }
        }
    }
    games
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steam_vdf_and_acf_and_epic_manifest() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path"		"C:\\Program Files (x86)\\Steam"
    }
    "1"
    {
        "path"		"D:\\SteamLibrary"
    }
}
"#;
        let libs = parse_steam_library_paths(vdf);
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[1], PathBuf::from(r"D:\SteamLibrary"));

        let acf = r#"
"AppState"
{
    "appid"		"730"
    "name"		"Counter-Strike 2"
    "installdir"		"Counter-Strike Global Offensive"
}
"#;
        assert_eq!(
            parse_steam_appmanifest(acf),
            Some((
                "Counter-Strike 2".to_owned(),
                "Counter-Strike Global Offensive".to_owned()
            ))
        );

        let epic = r#"{
            "DisplayName": "Fortnite",
            "InstallLocation": "C:\\Program Files\\Epic Games\\Fortnite",
            "LaunchExecutable": "FortniteGame/Binaries/Win64/FortniteClient-Win64-Shipping.exe"
        }"#;
        let parsed = parse_epic_manifest(epic).unwrap();
        assert_eq!(parsed.name, "Fortnite");
        assert_eq!(parsed.exe_name, "FortniteClient-Win64-Shipping.exe");
        assert_eq!(parsed.source, GameSource::Epic);
    }

    #[test]
    fn config_renames_presets_and_matches_running_games() {
        let mut cfg = AppConfig::default();
        let mouse = DeviceId::new("lamzu:3554:F50D:mock");
        let kb = DeviceId::new("atk:373B:1044:ATK RS HE");

        cfg.set_preset_name(&mouse, ProfileSlot::new(1), "Valorant 1600");
        cfg.set_preset_name(&kb, ProfileSlot::new(2), "Valorant RT");
        assert_eq!(
            cfg.preset_name(&mouse, ProfileSlot::new(1)),
            "Valorant 1600"
        );
        assert_eq!(cfg.preset_name(&kb, ProfileSlot::new(2)), "Valorant RT");

        let mut gp = GameProfile::new(
            "valorant",
            "VALORANT",
            "VALORANT-Win64-Shipping.exe",
            r"C:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe",
        );
        gp.device_targets.insert(mouse.as_str().to_owned(), 1);
        gp.device_targets.insert(kb.as_str().to_owned(), 2);
        cfg.upsert_game_profile(gp);

        cfg.rename_game_profile("valorant", "Valorant Ranked");
        assert_eq!(cfg.game_profiles[0].name, "Valorant Ranked");

        let running = HashSet::from(["valorant-win64-shipping.exe".to_owned()]);
        let matched = cfg.match_running_game(&running).unwrap();
        assert_eq!(matched.id, "valorant");
        assert_eq!(matched.device_targets.get(mouse.as_str()), Some(&1));
        assert_eq!(matched.device_targets.get(kb.as_str()), Some(&2));

        // JSON roundtrip
        let json = serde_json::to_string(&cfg).unwrap();
        let roundtrip: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, cfg);
    }
}
