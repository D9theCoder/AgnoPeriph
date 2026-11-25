//! Cross-brand Game Profiles, custom preset naming, software profile storage,
//! Windows installed game discovery (Steam, Epic, Riot, Uninstall registry),
//! and lightweight active-process monitoring.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use agnoperiph_core::{
    DeviceId, KeyboardConfigSnapshot, ProfileSlot, RgbZoneState, TravelHundredthsMm,
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
    Installed,
    Manual,
}

impl fmt::Display for GameSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Steam => f.write_str("Steam"),
            Self::Epic => f.write_str("Epic Games"),
            Self::Riot => f.write_str("Riot Games"),
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
#[non_exhaustive]
pub struct SavedKeyboardPreset {
    pub global_actuation: Option<TravelHundredthsMm>,
    pub top_deadzone: Option<TravelHundredthsMm>,
    pub bottom_deadzone: Option<TravelHundredthsMm>,
    pub rgb_zones: Vec<RgbZoneState>,
    pub keyboard_config: Option<KeyboardConfigSnapshot>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AppConfig {
    pub auto_switch_enabled: bool,
    /// Custom user-assigned names for any device's profile slots: `device_id -> slot -> label`.
    pub preset_names: BTreeMap<String, BTreeMap<u8, String>>,
    /// Host-managed software profiles for devices without multi-slot hardware memory.
    pub software_profiles: BTreeMap<String, DeviceSoftwareProfiles>,
    /// Automatic game-triggered cross-brand profiles.
    pub game_profiles: Vec<GameProfile>,
    /// Fallback per-device profile slots restored when no configured game is running.
    pub default_targets: BTreeMap<String, u8>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_switch_enabled: true,
            preset_names: BTreeMap::new(),
            software_profiles: BTreeMap::new(),
            game_profiles: Vec::new(),
            default_targets: BTreeMap::new(),
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

/// Discovers installed Windows games across Steam, Epic Games, Riot Games, and Uninstall registry.
///
/// Mirrors the local manifest/registry discovery strategy used by NVIDIA App / Playnite
/// without requiring administrator elevation.
pub fn discover_installed_games() -> Vec<DetectedGame> {
    let mut found: Vec<DetectedGame> = Vec::new();
    let mut seen_exes: HashSet<String> = HashSet::new();

    let mut push_unique = |game: DetectedGame| {
        let key = normalize_exe_name(&game.exe_name);
        let is_exe = Path::new(&key)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        if !key.is_empty() && is_exe && seen_exes.insert(key) {
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
    for game in discover_registry_games() {
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
    ];
    IGNORED_SUBSTRINGS
        .iter()
        .any(|needle| file_name_lower.contains(needle))
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
    let known = [
        (
            "VALORANT",
            "VALORANT-Win64-Shipping.exe",
            [
                r"C:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe",
                r"D:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe",
            ],
        ),
        (
            "League of Legends",
            "League of Legends.exe",
            [
                r"C:\Riot Games\League of Legends\Game\League of Legends.exe",
                r"D:\Riot Games\League of Legends\Game\League of Legends.exe",
            ],
        ),
    ];
    for (name, exe_name, paths) in known {
        for candidate in paths {
            let p = Path::new(candidate);
            if p.exists() {
                games.push(DetectedGame::new(
                    name,
                    exe_name,
                    candidate,
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
