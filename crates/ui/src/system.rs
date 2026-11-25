//! Operating-system integration seams: sign-in auto start and configuration persistence.
//!
//! Every side effect outside the HID backend goes through [`SystemIntegration`] so headless
//! tests never touch the registry or `%APPDATA%`.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::game_profiles::AppConfig;

/// Registry key holding per-user sign-in launch entries.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name AgnoPeriph registers under [`RUN_KEY`].
pub const RUN_VALUE: &str = "AgnoPeriph";
/// Command-line flag that starts AgnoPeriph hidden in the notification area.
pub const MINIMIZED_FLAG: &str = "--minimized";

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
}

/// Windows integration: `HKCU\...\Run` for auto start and a JSON file for configuration.
pub struct WindowsSystem {
    config_path: Option<PathBuf>,
}

impl fmt::Debug for WindowsSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowsSystem")
            .field("config_path", &self.config_path)
            .finish()
    }
}

impl WindowsSystem {
    pub fn new(config_path: Option<PathBuf>) -> Self {
        Self { config_path }
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
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
    }

    #[test]
    fn older_configs_without_new_fields_still_load() {
        let raw = r#"{"auto_switch_enabled": false, "game_profiles": []}"#;
        let config: AppConfig = serde_json::from_str(raw).unwrap();
        assert!(!config.auto_switch_enabled);
        assert!(config.close_to_tray);
        assert!(!config.start_with_windows);
    }
}
