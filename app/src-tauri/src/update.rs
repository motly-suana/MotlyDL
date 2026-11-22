//! Tells the user about a newer MotlyDL release on GitHub, and installs it.
//!
//! On Windows the installer is downloaded and run in place; elsewhere the
//! download opens in the browser, since a .dmg or Linux package needs the
//! user's hands anyway.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use motlydl_engine::Client;

const LATEST: &str = "https://api.github.com/repos/motly-suana/MotlyDL/releases/latest";

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub version: String,
    /// The release page, with the full notes.
    pub page: String,
    /// This platform's installer, when the release has one.
    pub asset: Option<String>,
}

fn parts(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

/// Whether `remote` is a later version than `local` (both like "0.3.1").
pub fn is_newer(remote: &str, local: &str) -> bool {
    let (r, l) = (parts(remote), parts(local));
    !r.is_empty() && r > l
}

/// This platform's installer among the release's assets.
fn pick_asset(names: &[(String, String)]) -> Option<String> {
    if cfg!(windows) {
        return pick_windows(names, cfg!(target_arch = "aarch64"));
    }
    if cfg!(target_os = "macos") {
        return names.iter().find(|(name, _)| name.ends_with(".dmg")).map(|(_, url)| url.clone());
    }
    None // Linux: .deb, .rpm or AppImage; let the user choose on the page.
}

/// The Windows installer for this CPU. An ARM64 PC takes the ARM64 one; every
/// other PC takes the x64 one and never the ARM64 one (Windows refuses to run it).
fn pick_windows(names: &[(String, String)], arm: bool) -> Option<String> {
    let installers = names.iter().filter(|(n, _)| n.ends_with("-setup.exe") || n.ends_with("_installer.exe"));
    let for_arm = |n: &str| n.contains("arm64");
    let mut found = installers.clone().find(|(n, _)| for_arm(n) == arm);
    // An ARM64 PC with no ARM64 installer in the release runs the x64 one.
    if found.is_none() && arm {
        found = installers.clone().next();
    }
    found.map(|(_, url)| url.clone())
}

/// The latest release, if it is newer than this build.
pub async fn check(client: &Client) -> Result<Option<Update>> {
    let body = client
        .get(LATEST)
        .header("User-Agent", "MotlyDL")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let rel: Value = serde_json::from_str(&body).context("GitHub sent an unexpected reply")?;
    let tag = rel["tag_name"].as_str().unwrap_or_default();
    if rel["draft"].as_bool() == Some(true) || rel["prerelease"].as_bool() == Some(true) || !is_newer(tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let assets: Vec<(String, String)> = rel["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string())))
        .collect();
    Ok(Some(Update {
        version: tag.trim_start_matches('v').to_string(),
        page: rel["html_url"].as_str().unwrap_or("https://github.com/motly-suana/MotlyDL/releases/latest").to_string(),
        asset: pick_asset(&assets),
    }))
}

/// Downloads the Windows installer and starts it. The caller then quits the
/// app so the installer can replace it; `/P` shows only a progress bar, `/UPDATE` keeps
/// the user's shortcuts, and `/R` starts MotlyDL again afterwards.
#[cfg(windows)]
pub async fn run_installer(client: &Client, url: &str) -> Result<()> {
    if !url.starts_with("https://github.com/motly-suana/MotlyDL/releases/download/") {
        bail!("refusing to install from {url}");
    }
    let bytes = client.get(url).header("User-Agent", "MotlyDL").send().await?.error_for_status()?.bytes().await?;
    let path = std::env::temp_dir().join(url.rsplit('/').next().unwrap_or("MotlyDL-setup.exe"));
    std::fs::write(&path, &bytes).with_context(|| format!("could not save {}", path.display()))?;
    std::process::Command::new(&path).args(["/P", "/R", "/UPDATE"]).spawn().context("could not start the installer")?;
    Ok(())
}

#[cfg(not(windows))]
pub async fn run_installer(_client: &Client, _url: &str) -> Result<()> {
    bail!("installing in place is only supported on Windows")
}

#[cfg(test)]
mod tests {
    use super::{is_newer, pick_windows};

    #[test]
    fn each_pc_gets_its_own_windows_installer() {
        let names = |list: &[&str]| list.iter().map(|n| (n.to_string(), format!("https://x/{n}"))).collect::<Vec<_>>();
        // Alphabetical, as GitHub lists them: arm64 comes before x64.
        let both = names(&["MotlyDL_1_arm64-setup.exe", "MotlyDL_1_arm64_en-US.msi", "MotlyDL_1_x64-setup.exe", "MotlyDL_1_x64_en-US.msi"]);
        assert_eq!(pick_windows(&both, false).as_deref(), Some("https://x/MotlyDL_1_x64-setup.exe"), "x64 must never get the ARM64 file");
        assert_eq!(pick_windows(&both, true).as_deref(), Some("https://x/MotlyDL_1_arm64-setup.exe"));
        // The ARM64 installer renamed away from "-setup.exe" so old updaters skip it.
        let renamed = names(&["MotlyDL_1_arm64_installer.exe", "MotlyDL_1_x64-setup.exe"]);
        assert_eq!(pick_windows(&renamed, false).as_deref(), Some("https://x/MotlyDL_1_x64-setup.exe"));
        assert_eq!(pick_windows(&renamed, true).as_deref(), Some("https://x/MotlyDL_1_arm64_installer.exe"));
        // Only an x64 installer: an ARM64 PC runs it under emulation; nothing at all: None.
        let only_x64 = names(&["MotlyDL_1_x64-setup.exe"]);
        assert_eq!(pick_windows(&only_x64, true).as_deref(), Some("https://x/MotlyDL_1_x64-setup.exe"));
        assert_eq!(pick_windows(&names(&["MotlyDL_1_arm64_installer.exe"]), false), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.3.2", "0.3.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.0"));
        assert!(!is_newer("v0.3.1", "0.3.1"));
        assert!(!is_newer("v0.3.0", "0.3.1"));
        assert!(!is_newer("nightly", "0.3.1"));
    }
}
