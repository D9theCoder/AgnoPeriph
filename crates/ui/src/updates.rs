//! App update checks: reads the latest GitHub release, downloads its Windows installer,
//! and launches it in place (falling back to the release page when no installer asset exists).
//!
//! The check runs once on startup when the app options enable it, and on demand from
//! the sidebar. An available release shows a notice above the workspace; anything else
//! (no release, offline, GitHub error) stays quiet unless the user asked.

use std::io::Read;
use std::path::{Path, PathBuf};

use gpui_kit::component::{
    Disableable, Icon, IconName, Sizable, WindowExt,
    alert::Alert,
    button::{Button, ButtonVariants},
    h_flex,
    notification::Notification,
};
use gpui_kit::*;
use serde_json::Value;

use crate::app::AgnoPeriphApp;

/// Version of this build, compared against the latest release tag.
///
/// Release builds are stamped with the workflow's `APP_VERSION` (for example `0.1.0.2`);
/// a local build falls back to the workspace version.
pub const CURRENT_VERSION: &str = env!("AGNO_APP_VERSION");
/// GitHub endpoint that returns the newest published release.
pub const RELEASES_API: &str = "https://api.github.com/repos/D9theCoder/AgnoPeriph/releases/latest";
/// Human-facing release list, used when a release carries no usable link.
pub const RELEASES_PAGE: &str = "https://github.com/D9theCoder/AgnoPeriph/releases/latest";
/// Default filename for the published Windows installer asset.
pub const DEFAULT_INSTALLER_NAME: &str = "AgnoPeriph-Setup-x64.exe";

/// A published release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    version: String,
    url: String,
    installer_name: Option<String>,
    installer_url: Option<String>,
}

impl UpdateInfo {
    pub fn new(version: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            url: url.into(),
            installer_name: None,
            installer_url: None,
        }
    }

    /// Attaches a direct Windows installer asset to download and run in-app.
    pub fn with_installer(
        mut self,
        name: impl Into<String>,
        download_url: impl Into<String>,
    ) -> Self {
        self.installer_name = Some(name.into());
        self.installer_url = Some(download_url.into());
        self
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn installer_name(&self) -> Option<&str> {
        self.installer_name.as_deref()
    }

    pub fn installer_url(&self) -> Option<&str> {
        self.installer_url.as_deref()
    }

    pub fn has_installer(&self) -> bool {
        self.installer_url.is_some()
    }

    pub fn summary(&self) -> String {
        format!(
            "AgnoPeriph {} is available. You’re on {CURRENT_VERSION}.",
            self.version
        )
    }
}

/// Where release metadata and installer downloads come from.
/// Blocking: called on the background executor.
pub trait ReleaseSource: Send + Sync {
    fn latest(&self) -> Result<Option<UpdateInfo>, String>;

    /// Downloads the installer at `url` into `destination`.
    fn download_installer(&self, url: &str, destination: &Path) -> Result<(), String> {
        let _ = (url, destination);
        Err("this release source does not support installer downloads".to_owned())
    }
}

/// GitHub's latest-release API, read with the `curl.exe` that ships with Windows.
#[derive(Debug, Clone)]
pub struct GithubReleases {
    endpoint: String,
}

impl Default for GithubReleases {
    fn default() -> Self {
        Self::new(RELEASES_API)
    }
}

impl GithubReleases {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

impl ReleaseSource for GithubReleases {
    fn latest(&self) -> Result<Option<UpdateInfo>, String> {
        let body = fetch(&self.endpoint)?;
        latest_release(&body, CURRENT_VERSION)
    }

    fn download_installer(&self, url: &str, destination: &Path) -> Result<(), String> {
        download_file(url, destination)
    }
}

#[cfg(windows)]
fn fetch(endpoint: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    let output = Command::new("curl")
        .args([
            "-sSL",
            "--max-time",
            "8",
            "-A",
            concat!("AgnoPeriph/", env!("CARGO_PKG_VERSION")),
            "-H",
            "Accept: application/vnd.github+json",
            endpoint,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        // Release builds are GUI apps: the child must not flash a console window.
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|err| format!("start curl: {err}"))?;
    if !output.status.success() {
        return Err(format!("curl exited with {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|_| "the reply was not UTF-8".to_owned())
}

#[cfg(not(windows))]
fn fetch(_endpoint: &str) -> Result<String, String> {
    Err("update checks run on Windows only".to_owned())
}

#[cfg(windows)]
fn download_file(url: &str, destination: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    if !is_valid_https_url(url) {
        return Err("installer link must use https".to_owned());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("prepare the update folder: {err}"))?;
    }

    let part_path = part_download_path(destination);
    let _ = std::fs::remove_file(&part_path);

    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "15",
            "--max-time",
            "300",
            "-A",
            concat!("AgnoPeriph/", env!("CARGO_PKG_VERSION")),
            "-o",
        ])
        .arg(&part_path)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // Release builds are GUI apps: the child must not flash a console window.
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|err| format!("start curl: {err}"))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&part_path);
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return if detail.is_empty() {
            Err(format!("download failed ({})", output.status))
        } else {
            Err(format!("download failed: {detail}"))
        };
    }

    if let Err(err) = verify_pe_executable(&part_path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(err);
    }

    let _ = std::fs::remove_file(destination);
    std::fs::rename(&part_path, destination).map_err(|err| {
        let _ = std::fs::remove_file(&part_path);
        format!("finalize the downloaded installer: {err}")
    })
}

#[cfg(not(windows))]
fn download_file(_url: &str, _destination: &Path) -> Result<(), String> {
    Err("installing updates runs on Windows only".to_owned())
}

/// Destination path inside `%TEMP%\AgnoPeriph-Update` for a versioned installer download.
pub fn installer_download_path(version: &str, asset_name: &str) -> PathBuf {
    let safe_version = sanitize_filename_part(version);
    let stem = Path::new(asset_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_filename_part)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "AgnoPeriph-Setup-x64".to_owned());
    let file_name = if safe_version.is_empty() {
        format!("{stem}.exe")
    } else {
        format!("{stem}-{safe_version}.exe")
    };
    std::env::temp_dir()
        .join("AgnoPeriph-Update")
        .join(file_name)
}

fn part_download_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().map_or_else(
        || std::ffi::OsString::from(DEFAULT_INSTALLER_NAME),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".part");
    destination.with_file_name(name)
}

fn sanitize_filename_part(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>()
        .trim_matches('.')
        .to_owned()
}

/// Verifies that a downloaded file starts with the Windows PE `MZ` header.
pub fn verify_pe_executable(path: &Path) -> Result<(), String> {
    let mut file =
        std::fs::File::open(path).map_err(|err| format!("read the downloaded installer: {err}"))?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)
        .map_err(|_| "downloaded installer was empty or incomplete".to_owned())?;
    if &magic != b"MZ" {
        return Err("downloaded file is not a valid Windows installer".to_owned());
    }
    Ok(())
}

fn is_valid_https_url(url: &str) -> bool {
    url.starts_with("https://") && !url.chars().any(char::is_whitespace)
}

fn is_safe_exe_asset_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains("..")
        && Path::new(trimmed)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

fn find_installer_asset(value: &Value) -> Option<(String, String)> {
    let assets = value.get("assets")?.as_array()?;
    let mut fallback: Option<(String, String)> = None;
    for asset in assets {
        let Some(name) = asset.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !is_safe_exe_asset_name(name) {
            continue;
        }
        let Some(download_url) = asset
            .get("browser_download_url")
            .and_then(Value::as_str)
            .filter(|url| is_valid_https_url(url))
        else {
            continue;
        };
        if name.eq_ignore_ascii_case(DEFAULT_INSTALLER_NAME) {
            return Some((name.to_owned(), download_url.to_owned()));
        }
        if fallback.is_none() {
            fallback = Some((name.to_owned(), download_url.to_owned()));
        }
    }
    fallback
}

/// Reads a GitHub release reply and returns it when it is newer than `current`.
///
/// A reply without `tag_name` (the 404 body when the repository has no releases yet)
/// means there is nothing to offer, not an error.
pub fn latest_release(json: &str, current: &str) -> Result<Option<UpdateInfo>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("read the release reply: {err}"))?;
    let Some(tag) = value.get("tag_name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let flagged = |field: &str| value.get(field).and_then(Value::as_bool) == Some(true);
    if flagged("draft") || flagged("prerelease") || !is_newer(tag, current) {
        return Ok(None);
    }
    let url = value
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| is_valid_https_url(url))
        .unwrap_or(RELEASES_PAGE);
    let mut info = UpdateInfo::new(tag.trim().trim_start_matches(['v', 'V']), url);
    if let Some((name, download_url)) = find_installer_asset(&value) {
        info = info.with_installer(name, download_url);
    }
    Ok(Some(info))
}

/// Whether release tag `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_parts(candidate), version_parts(current)) {
        (Some(next), Some(now)) => next > now,
        _ => false,
    }
}

fn version_parts(raw: &str) -> Option<Vec<u64>> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let trimmed = trimmed.split('+').next()?;
    let (core, build) = match trimmed.split_once('-') {
        Some((core, build)) => (core, Some(build)),
        None => (trimmed, None),
    };
    let mut parts = Vec::new();
    for part in core.split('.') {
        parts.push(part.parse::<u64>().ok()?);
    }
    if let Some(build) = build {
        // Release tags are `v<version>-<run number>`, so a numeric suffix is a build
        // number, not a prerelease. Text suffixes (`-beta`) are never offered.
        parts.push(build.parse::<u64>().ok()?);
    }
    while parts.last() == Some(&0) {
        parts.pop();
    }
    (!parts.is_empty()).then_some(parts)
}

/// Update-check and installer state owned by the app.
#[derive(Debug, Default)]
pub(crate) struct UpdateUi {
    available: Option<UpdateInfo>,
    is_checking: bool,
    is_installing: bool,
    dismissed: bool,
    /// The user asked for this check, so its outcome is reported even when it finds nothing.
    manual: bool,
}

impl UpdateUi {
    fn notice(&self) -> Option<&UpdateInfo> {
        self.available.as_ref().filter(|_| !self.dismissed)
    }
}

/// Marks the "up to date" / "couldn’t check" notifications so a new check replaces the old one.
struct UpdateCheckNotice;

impl AgnoPeriphApp {
    /// The newer release waiting for the user, if any.
    pub fn update_available(&self) -> Option<&UpdateInfo> {
        self.updates.notice()
    }

    pub fn is_checking_updates(&self) -> bool {
        self.updates.is_checking
    }

    pub fn is_installing_update(&self) -> bool {
        self.updates.is_installing
    }

    /// Asks the release source for the newest version, off the UI thread.
    pub(crate) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.updates.is_checking || self.updates.is_installing {
            return;
        }
        self.updates.is_checking = true;
        self.updates.manual = manual;
        let source = self.releases.clone();
        cx.spawn(async move |this, cx| {
            let reply = cx.background_spawn(async move { source.latest() }).await;
            let _ = this.update(cx, |this, cx| this.finish_update_check(reply, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish_update_check(
        &mut self,
        reply: Result<Option<UpdateInfo>, String>,
        cx: &mut Context<Self>,
    ) {
        self.updates.is_checking = false;
        let manual = std::mem::take(&mut self.updates.manual);
        match reply {
            Ok(Some(info)) => {
                self.updates.available = Some(info);
                self.updates.dismissed = false;
            }
            Ok(None) => {
                self.updates.available = None;
                if manual {
                    self.notify_update_check(
                        Notification::success(format!(
                            "AgnoPeriph {CURRENT_VERSION} is up to date."
                        )),
                        cx,
                    );
                }
            }
            Err(err) => {
                if manual {
                    self.notify_update_check(
                        Notification::error(format!("Couldn’t check for updates: {err}.")),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }

    fn notify_update_check(&self, note: Notification, cx: &mut Context<Self>) {
        let Some(handle) = self.window_handle else {
            return;
        };
        let note = note.id::<UpdateCheckNotice>();
        cx.spawn(async move |_, cx| {
            let _ = cx.update_window(handle, |_, window, cx| window.push_notification(note, cx));
        })
        .detach();
    }

    fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        if self.updates.is_installing {
            return;
        }
        self.updates.dismissed = true;
        cx.notify();
    }

    fn open_update(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Err(err) = self.system.open_url(url) {
            self.show_banner(
                "Couldn’t open the download page".to_owned(),
                format!("{err}."),
            );
        }
        cx.notify();
    }

    /// Downloads the release installer off the UI thread and launches it to update in place.
    fn install_update(&mut self, cx: &mut Context<Self>) {
        if self.updates.is_installing {
            return;
        }
        let Some(info) = self.updates.available.clone() else {
            return;
        };
        let Some(installer_url) = info.installer_url().map(str::to_owned) else {
            self.open_update(info.url(), cx);
            return;
        };
        let asset_name = info.installer_name().unwrap_or(DEFAULT_INSTALLER_NAME);
        let destination = installer_download_path(info.version(), asset_name);
        let source = self.releases.clone();
        let system = self.system.clone();

        self.updates.is_installing = true;
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    source.download_installer(&installer_url, &destination)?;
                    system.launch_installer(&destination)
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_update_install(outcome, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish_update_install(&mut self, outcome: Result<bool, String>, cx: &mut Context<Self>) {
        self.updates.is_installing = false;
        match outcome {
            Ok(should_quit) => {
                if should_quit {
                    cx.quit();
                }
            }
            Err(err) => {
                self.show_banner("Couldn’t install the update".to_owned(), format!("{err}."));
            }
        }
        cx.notify();
    }
}

/// The update notice above the workspace, when a newer release is waiting.
pub(crate) fn render_notice(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> Option<impl IntoElement + use<>> {
    let info = app.updates.notice()?.clone();
    let is_installing = app.updates.is_installing;
    let has_installer = info.has_installer();
    let url = info.url().to_owned();
    let message = if is_installing {
        format!(
            "Downloading AgnoPeriph {} installer… AgnoPeriph will close to apply the update.",
            info.version()
        )
    } else {
        info.summary()
    };
    let action_label = if is_installing {
        "Downloading…"
    } else if has_installer {
        "Download & install"
    } else {
        "Download"
    };
    let action_icon = if has_installer {
        Icon::new(gpui_kit::assets::IconName::Download)
    } else {
        Icon::new(IconName::ExternalLink)
    };

    Some(
        h_flex()
            .gap_3()
            .items_start()
            .child(
                Alert::info("update-banner", message)
                    .title("Update available")
                    .flex_1(),
            )
            .child(
                Button::new("update-download")
                    .outline()
                    .small()
                    .icon(action_icon)
                    .label(action_label)
                    .loading(is_installing)
                    .disabled(is_installing)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if has_installer {
                            this.install_update(cx);
                        } else {
                            this.open_update(&url, cx);
                        }
                    })),
            )
            .child(
                Button::new("update-dismiss")
                    .ghost()
                    .small()
                    .label("Later")
                    .disabled(is_installing)
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_update(cx))),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        RELEASES_PAGE, installer_download_path, is_newer, latest_release, verify_pe_executable,
    };

    #[test]
    fn compares_dotted_versions_ignoring_the_v_prefix() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn release_build_numbers_are_part_of_the_version() {
        assert!(is_newer("v0.1.0-2", "0.1.0"));
        assert!(is_newer("v0.1.0-10", "0.1.0.2"));
        assert!(!is_newer("v0.1.0-2", "0.1.0.2"));
        assert!(!is_newer("v0.1.0-2", "0.1.0-2"));
        assert!(!is_newer("v0.2.0-beta", "0.1.0"));
        assert!(!is_newer("v0.2.0-rc1", "0.1.0"));
    }

    #[test]
    fn reads_a_newer_release_and_its_https_page() {
        let json = r#"{
            "tag_name": "v0.2.0",
            "html_url": "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v0.2.0",
            "draft": false,
            "prerelease": false
        }"#;
        let release = latest_release(json, "0.1.0").unwrap().unwrap();
        assert_eq!(release.version(), "0.2.0");
        assert_eq!(
            release.url(),
            "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v0.2.0"
        );
        assert!(!release.has_installer());
    }

    #[test]
    fn reads_the_windows_installer_asset_when_present() {
        let json = r#"{
            "tag_name": "v0.1.0-42",
            "html_url": "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v0.1.0-42",
            "draft": false,
            "prerelease": false,
            "assets": [
                {
                    "name": "checksums.txt",
                    "browser_download_url": "https://github.com/D9theCoder/AgnoPeriph/releases/download/v0.1.0-42/checksums.txt"
                },
                {
                    "name": "other-tool.exe",
                    "browser_download_url": "https://github.com/D9theCoder/AgnoPeriph/releases/download/v0.1.0-42/other-tool.exe"
                },
                {
                    "name": "AgnoPeriph-Setup-x64.exe",
                    "browser_download_url": "https://github.com/D9theCoder/AgnoPeriph/releases/download/v0.1.0-42/AgnoPeriph-Setup-x64.exe"
                }
            ]
        }"#;
        let release = latest_release(json, "0.1.0").unwrap().unwrap();
        assert!(release.has_installer());
        assert_eq!(release.installer_name(), Some("AgnoPeriph-Setup-x64.exe"));
        assert_eq!(
            release.installer_url(),
            Some(
                "https://github.com/D9theCoder/AgnoPeriph/releases/download/v0.1.0-42/AgnoPeriph-Setup-x64.exe"
            )
        );
    }

    #[test]
    fn ignores_non_https_and_path_traversal_installer_assets() {
        let json = r#"{
            "tag_name": "v0.2.0",
            "html_url": "https://github.com/D9theCoder/AgnoPeriph/releases/tag/v0.2.0",
            "assets": [
                {
                    "name": "AgnoPeriph-Setup-x64.exe",
                    "browser_download_url": "http://example.com/AgnoPeriph-Setup-x64.exe"
                },
                {
                    "name": "..\\evil.exe",
                    "browser_download_url": "https://example.com/evil.exe"
                }
            ]
        }"#;
        let release = latest_release(json, "0.1.0").unwrap().unwrap();
        assert!(!release.has_installer());
    }

    #[test]
    fn skips_drafts_prereleases_and_current_releases() {
        let draft = r#"{"tag_name": "v0.3.0", "draft": true}"#;
        let beta = r#"{"tag_name": "v0.3.0", "prerelease": true}"#;
        let current = r#"{"tag_name": "v0.1.0"}"#;
        for json in [draft, beta, current] {
            assert_eq!(latest_release(json, "0.1.0").unwrap(), None);
        }
    }

    #[test]
    fn a_repository_without_releases_reports_nothing() {
        let json = r#"{"message": "Not Found"}"#;
        assert_eq!(latest_release(json, "0.1.0").unwrap(), None);
    }

    #[test]
    fn non_https_pages_fall_back_to_the_release_list() {
        let json = r#"{"tag_name": "v0.2.0", "html_url": "http://example.com/release"}"#;
        let release = latest_release(json, "0.1.0").unwrap().unwrap();
        assert_eq!(release.url(), RELEASES_PAGE);
    }

    #[test]
    fn a_broken_reply_is_an_error_not_an_update() {
        assert!(latest_release("<html>bad gateway</html>", "0.1.0").is_err());
    }

    #[test]
    fn installer_download_path_sanitizes_version_and_asset_name() {
        let path = installer_download_path("0.1.0-42/../bad", "AgnoPeriph-Setup-x64.exe");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("AgnoPeriph-Setup-x64-"));
        assert!(
            std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        );
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
    }

    #[test]
    fn verifies_pe_header_before_launching() {
        let dir = std::env::temp_dir().join("agnoperiph-pe-test");
        std::fs::create_dir_all(&dir).unwrap();
        let valid = dir.join("valid.exe");
        let invalid = dir.join("invalid.exe");
        std::fs::write(&valid, b"MZ\x90\x00test").unwrap();
        std::fs::write(&invalid, b"<html>404</html>").unwrap();

        assert!(verify_pe_executable(&valid).is_ok());
        assert!(verify_pe_executable(&invalid).is_err());

        let _ = std::fs::remove_file(&valid);
        let _ = std::fs::remove_file(&invalid);
    }
}
