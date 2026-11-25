//! Operating-system integration seams: sign-in auto start and configuration persistence.
//!
//! Every side effect outside the HID backend goes through [`SystemIntegration`] so headless
//! tests never touch the registry or `%APPDATA%`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::game_profiles::AppConfig;

/// Registry key holding per-user sign-in launch entries.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name AgnoPeriph registers under [`RUN_KEY`].
pub const RUN_VALUE: &str = "AgnoPeriph";
/// Command-line flag that starts AgnoPeriph hidden in the notification area.
pub const MINIMIZED_FLAG: &str = "--minimized";
/// Windows AppUserModelID registered under `HKCU\Software\Classes\AppUserModelId` for toasts.
pub const TOAST_APP_ID: &str = "AgnoPeriph.App";

/// Builds the title and body for a Windows notification after a Game Profile switches presets.
pub fn format_switch_notification(
    game_name: Option<&str>,
    switches: &[(String, String)],
) -> (String, String) {
    let title = match game_name {
        Some(name) => format!("{name} — Presets switched"),
        None => "Desktop / Default — Presets restored".to_owned(),
    };
    let body = switches
        .iter()
        .map(|(device, preset)| format!("{device} → {preset}"))
        .collect::<Vec<_>>()
        .join("\n");
    (title, body)
}

/// Builds the `Run` command line for `exe`, quoting the path and appending
/// [`MINIMIZED_FLAG`] when the app should start in the notification area.
pub fn run_command_line(exe: &Path, minimized: bool) -> String {
    let quoted = format!("\"{}\"", exe.display());
    if minimized {
        format!("{quoted} {MINIMIZED_FLAG}")
    } else {
        quoted
    }
}

/// Side effects the app performs on the host system.
pub trait SystemIntegration: Send + Sync {
    /// Registers or removes the sign-in launch entry.
    fn set_start_with_windows(&self, enabled: bool, minimized: bool) -> Result<(), String>;

    /// Returns whether the sign-in launch entry is currently registered.
    fn is_start_with_windows_enabled(&self) -> bool;

    /// Persists the configuration. Called from a background executor.
    fn save_config(&self, config: &AppConfig) -> Result<(), String>;

    /// Displays a native desktop notification. Called from a background executor.
    fn send_notification(&self, title: &str, body: &str) -> Result<(), String>;
}

/// No-op integration for tests and previews: nothing leaves the process.
#[derive(Debug, Default)]
pub struct DetachedSystem;

impl SystemIntegration for DetachedSystem {
    fn set_start_with_windows(&self, _enabled: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }

    fn is_start_with_windows_enabled(&self) -> bool {
        false
    }

    fn save_config(&self, _config: &AppConfig) -> Result<(), String> {
        Ok(())
    }

    fn send_notification(&self, _title: &str, _body: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Windows integration: `HKCU\...\Run` for auto start, JSON config persistence, and WinRT toasts.
pub struct WindowsSystem {
    config_path: Option<PathBuf>,
    toast_icon_path: OnceLock<Option<PathBuf>>,
    toast_registered: OnceLock<bool>,
}

impl fmt::Debug for WindowsSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsSystem")
            .field("config_path", &self.config_path)
            .finish_non_exhaustive()
    }
}

impl WindowsSystem {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        Self {
            config_path,
            toast_icon_path: OnceLock::new(),
            toast_registered: OnceLock::new(),
        }
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    #[cfg(windows)]
    fn ensure_toast_icon(&self) -> Option<&Path> {
        self.toast_icon_path
            .get_or_init(|| {
                let dir = self
                    .config_path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .or_else(|| {
                        std::env::var_os("APPDATA")
                            .map(PathBuf::from)
                            .map(|base| base.join("AgnoPeriph"))
                    })?;
                std::fs::create_dir_all(&dir).ok()?;
                let icon_path = dir.join("logo.png");
                if !icon_path.exists() {
                    std::fs::write(&icon_path, include_bytes!("../assets/logo_64.png")).ok()?;
                }
                Some(icon_path)
            })
            .as_deref()
    }

    #[cfg(windows)]
    fn ensure_toast_aumid(&self) -> bool {
        *self.toast_registered.get_or_init(|| {
            use winreg::RegKey;
            use winreg::enums::HKEY_CURRENT_USER;

            let key_path = format!(r"Software\Classes\AppUserModelId\{TOAST_APP_ID}");
            let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(&key_path) else {
                return false;
            };
            if key.set_value("DisplayName", &"AgnoPeriph").is_err() {
                return false;
            }
            let _ = key.set_value("IconBackgroundColor", &"0");
            if let Some(icon) = self.ensure_toast_icon() {
                let _ = key.set_value("IconUri", &icon.display().to_string());
            }
            true
        })
    }
}

impl SystemIntegration for WindowsSystem {
    fn set_start_with_windows(&self, enabled: bool, minimized: bool) -> Result<(), String> {
        #[cfg(windows)]
        {
            use winreg::RegKey;
            use winreg::enums::HKEY_CURRENT_USER;
            let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
                .create_subkey(RUN_KEY)
                .map_err(|e| format!("open the Run key: {e}"))?;
            if enabled {
                let exe =
                    std::env::current_exe().map_err(|e| format!("locate AgnoPeriph.exe: {e}"))?;
                key.set_value(RUN_VALUE, &run_command_line(&exe, minimized))
                    .map_err(|e| format!("write the Run entry: {e}"))
            } else {
                match key.delete_value(RUN_VALUE) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(format!("remove the Run entry: {e}")),
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (enabled, minimized);
            Err("Starting with the system is only available on Windows".to_owned())
        }
    }

    fn is_start_with_windows_enabled(&self) -> bool {
        #[cfg(windows)]
        {
            use winreg::RegKey;
            use winreg::enums::HKEY_CURRENT_USER;
            RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey(RUN_KEY)
                .and_then(|key| key.get_value::<String, _>(RUN_VALUE))
                .is_ok()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    fn save_config(&self, config: &AppConfig) -> Result<(), String> {
        match &self.config_path {
            Some(path) => config.save_to(path),
            None => Ok(()),
        }
    }

    fn send_notification(&self, title: &str, body: &str) -> Result<(), String> {
        #[cfg(windows)]
        {
            use tauri_winrt_notification::{Duration, IconCrop, Toast};

            let icon = self.ensure_toast_icon();
            let build = |app_id: &str| {
                let mut toast = Toast::new(app_id)
                    .title(title)
                    .duration(Duration::Short)
                    .sound(None);
                let mut lines = body.lines();
                if let Some(first) = lines.next() {
                    toast = toast.text1(first);
                }
                let rest: Vec<&str> = lines.collect();
                if !rest.is_empty() {
                    toast = toast.text2(&rest.join(" · "));
                }
                if let Some(icon_path) = icon {
                    toast = toast.icon(icon_path, IconCrop::Square, "AgnoPeriph");
                }
                toast
            };

            if self.ensure_toast_aumid() && build(TOAST_APP_ID).show().is_ok() {
                return Ok(());
            }
            build(Toast::POWERSHELL_APP_ID)
                .show()
                .map_err(|e| format!("show Windows notification: {e}"))
        }
        #[cfg(not(windows))]
        {
            let _ = (title, body);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_line_quotes_the_path_and_adds_the_tray_flag() {
        let exe = Path::new(r"C:\Program Files\AgnoPeriph\agnoperiph.exe");
        assert_eq!(
            run_command_line(exe, false),
            r#""C:\Program Files\AgnoPeriph\agnoperiph.exe""#
        );
        assert_eq!(
            run_command_line(exe, true),
            r#""C:\Program Files\AgnoPeriph\agnoperiph.exe" --minimized"#
        );
    }

    #[test]
    fn detached_system_never_reports_auto_start() {
        let system = DetachedSystem;
        assert!(system.set_start_with_windows(true, true).is_ok());
        assert!(!system.is_start_with_windows_enabled());
        assert!(system.save_config(&AppConfig::default()).is_ok());
        assert!(system.send_notification("Title", "Body").is_ok());
    }

    #[test]
    fn older_configs_without_new_fields_still_load() {
        let raw = r#"{"auto_switch_enabled": false, "game_profiles": []}"#;
        let config: AppConfig = serde_json::from_str(raw).unwrap();
        assert!(!config.auto_switch_enabled);
        assert!(config.notify_on_game_switch);
        assert!(config.close_to_tray);
        assert!(!config.start_with_windows);
        assert!(config.low_battery_alerts);
    }

    #[test]
    fn formats_switch_notification_for_game_and_desktop() {
        let switches = vec![
            ("Lamzu Atlantis".to_owned(), "FPS".to_owned()),
            ("ATK RS7".to_owned(), "Competitive".to_owned()),
        ];
        let (title, body) = format_switch_notification(Some("VALORANT"), &switches);
        assert_eq!(title, "VALORANT — Presets switched");
        assert_eq!(body, "Lamzu Atlantis → FPS\nATK RS7 → Competitive");

        let (title, body) = format_switch_notification(None, &switches[..1]);
        assert_eq!(title, "Desktop / Default — Presets restored");
        assert_eq!(body, "Lamzu Atlantis → FPS");
    }
}
