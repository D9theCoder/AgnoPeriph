//! App update checks: reads the latest GitHub release and offers its download page.
//!
//! The check runs once on startup when the app options enable it, and on demand from
//! the sidebar. An available release shows a notice above the workspace; anything else
//! (no release, offline, GitHub error) stays quiet unless the user asked.

use gpui_kit::component::{
    IconName, Sizable, WindowExt,
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

/// A published release newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    version: String,
    url: String,
}

impl UpdateInfo {
    pub fn new(version: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            url: url.into(),
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn summary(&self) -> String {
        format!(
            "AgnoPeriph {} is available. You’re on {CURRENT_VERSION}.",
            self.version
        )
    }
}

/// Where release information comes from. Blocking: called on the background executor.
pub trait ReleaseSource: Send + Sync {
    fn latest(&self) -> Result<Option<UpdateInfo>, String>;
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
        .filter(|url| url.starts_with("https://"))
        .unwrap_or(RELEASES_PAGE);
    Ok(Some(UpdateInfo::new(
        tag.trim().trim_start_matches(['v', 'V']),
        url,
    )))
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

/// Update-check state owned by the app.
#[derive(Debug, Default)]
pub(crate) struct UpdateUi {
    available: Option<UpdateInfo>,
    is_checking: bool,
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

    /// Asks the release source for the newest version, off the UI thread.
    pub(crate) fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.updates.is_checking {
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
}

/// The update notice above the workspace, when a newer release is waiting.
pub(crate) fn render_notice(
    app: &AgnoPeriphApp,
    cx: &mut Context<AgnoPeriphApp>,
) -> Option<impl IntoElement + use<>> {
    let info = app.updates.notice()?.clone();
    let url = info.url().to_owned();
    Some(
        h_flex()
            .gap_3()
            .items_start()
            .child(
                Alert::info("update-banner", info.summary())
                    .title("Update available")
                    .flex_1(),
            )
            .child(
                Button::new("update-download")
                    .outline()
                    .small()
                    .icon(IconName::ExternalLink)
                    .label("Download")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_update(&url, cx);
                    })),
            )
            .child(
                Button::new("update-dismiss")
                    .ghost()
                    .small()
                    .label("Later")
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_update(cx))),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::{RELEASES_PAGE, is_newer, latest_release};

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
}
