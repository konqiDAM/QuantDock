//! Shared autoupdate helpers: release parsing, version comparison and install paths.

use std::path::{Component, Path, PathBuf};

pub const RELEASES_API: &str =
    "https://api.github.com/repos/konqiDAM/QuantDock/releases/latest";

/// One of the two release exes. `sha256` is the bare hex digest (the API's
/// `sha256:` prefix is stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub notes: String,
    pub assets: Vec<Asset>,
}

/// Require the two exact release assets; a missing, extra or malformed asset
/// rejects the whole release so an update is never partly applied.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("malformed release JSON: {e}"))?;
    let tag = value["tag_name"]
        .as_str()
        .ok_or("release has no tag_name")?
        .to_owned();
    let notes = value["body"].as_str().unwrap_or_default().to_owned();
    let entries = value["assets"]
        .as_array()
        .ok_or("release has no assets")?;
    if entries.len() != 2 {
        return Err(format!("expected exactly 2 release assets, got {}", entries.len()));
    }
    let mut assets = Vec::with_capacity(2);
    for entry in entries {
        let name = entry["name"].as_str().ok_or("asset has no name")?;
        if name != "quantdock.exe" && name != "monitor.exe" {
            return Err(format!("unexpected release asset: {name}"));
        }
        let url = entry["browser_download_url"]
            .as_str()
            .ok_or("asset has no download URL")?
            .to_owned();
        let digest = entry["digest"].as_str().ok_or("asset has no digest")?;
        let sha256 = digest
            .strip_prefix("sha256:")
            .filter(|hex| !hex.is_empty())
            .ok_or("asset digest is not sha256")?
            .to_owned();
        assets.push(Asset { name: name.to_owned(), url, sha256 });
    }
    // Both names must be present, not just two well-formed assets.
    if assets.iter().filter(|a| a.name == "quantdock.exe").count() != 1
        || assets.iter().filter(|a| a.name == "monitor.exe").count() != 1
    {
        return Err("release must contain quantdock.exe and monitor.exe".to_owned());
    }
    Ok(Release { tag, notes, assets })
}

/// A release tag is exactly `v` + three decimal segments. No floor rules here:
/// a plain `0.2.0` is not a tag.
pub fn valid_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let mut count = 0;
    for seg in rest.split('.') {
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        count += 1;
        if count > 3 {
            return false;
        }
    }
    count == 3
}

/// Parse either a tag (`v0.2.1`) or a floor (`0.2.1`) into three numbers.
fn triple(s: &str) -> Option<[u32; 3]> {
    let rest = s.strip_prefix('v').unwrap_or(s);
    let mut segs = rest.split('.');
    let out = [
        segs.next()?.parse::<u32>().ok()?,
        segs.next()?.parse::<u32>().ok()?,
        segs.next()?.parse::<u32>().ok()?,
    ];
    (segs.next().is_none()).then_some(out)
}

pub fn version_is_newer(tag: &str, floor: &str) -> bool {
    if !valid_tag(tag) {
        return false;
    }
    match (triple(tag), triple(floor)) {
        (Some(t), Some(f)) => t > f,
        _ => false,
    }
}

/// The persisted installed tag wins over the package version: the shipped
/// `v0.2.1` tag disagrees with `Cargo.toml`, so without this the release would
/// be offered forever. A bad persisted value falls back to the package version.
pub fn version_floor(last_installed: Option<&str>) -> String {
    last_installed
        .filter(|t| triple(t).is_some())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .to_owned()
}

/// A binary under `target\debug` or `target\release` is a build artifact and
/// must never be overwritten by an update.
pub fn is_dev_path(path: &Path) -> bool {
    let names: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    names.windows(2).any(|w| {
        w[0].to_string_lossy().eq_ignore_ascii_case("target")
            && (w[1].to_string_lossy().eq_ignore_ascii_case("debug")
                || w[1].to_string_lossy().eq_ignore_ascii_case("release"))
    })
}

pub fn install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate current exe: {e}"))?;
    if is_dev_path(&exe) {
        return Err("development builds cannot update themselves".to_owned());
    }
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate install folder".to_owned())
}

/// Probe a folder by creating and deleting a uniquely named file in it.
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(
        "quantdock-write-probe-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(&probe).is_ok()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn json_with(assets: serde_json::Value) -> String {
        serde_json::json!({
            "tag_name": "v0.3.0",
            "body": "New things",
            "assets": assets,
        })
        .to_string()
    }

    fn good_assets() -> serde_json::Value {
        serde_json::json!([
            {"name": "quantdock.exe", "browser_download_url": "https://x/quantdock.exe", "digest": "sha256:aaaa"},
            {"name": "monitor.exe", "browser_download_url": "https://x/monitor.exe", "digest": "sha256:bbbb"},
        ])
    }

    #[test]
    fn parse_release_reads_both_assets_and_digests() {
        let release = parse_release(&json_with(good_assets())).unwrap();
        assert_eq!(release.tag, "v0.3.0");
        assert_eq!(release.notes, "New things");
        assert_eq!(release.assets.len(), 2);
        let exe = release
            .assets
            .iter()
            .find(|a| a.name == "quantdock.exe")
            .unwrap();
        assert_eq!(exe.url, "https://x/quantdock.exe");
        assert_eq!(exe.sha256, "aaaa");
        let mon = release.assets.iter().find(|a| a.name == "monitor.exe").unwrap();
        assert_eq!(mon.url, "https://x/monitor.exe");
        assert_eq!(mon.sha256, "bbbb");
    }

    #[test]
    fn parse_release_rejects_wrong_assets() {
        // A wrong name in place of an expected one is an error, not a partial result.
        let wrong = serde_json::json!([
            {"name": "quantdock.exe", "browser_download_url": "https://x/quantdock.exe", "digest": "sha256:aaaa"},
            {"name": "other.exe", "browser_download_url": "https://x/other.exe", "digest": "sha256:bbbb"},
        ]);
        assert!(parse_release(&json_with(wrong)).is_err());

        // A missing expected asset is also an error.
        let missing = serde_json::json!([
            {"name": "quantdock.exe", "browser_download_url": "https://x/quantdock.exe", "digest": "sha256:aaaa"},
        ]);
        assert!(parse_release(&json_with(missing)).is_err());

        // No third asset is allowed: the release is exactly the two exes.
        let extra = serde_json::json!([
            {"name": "quantdock.exe", "browser_download_url": "https://x/quantdock.exe", "digest": "sha256:aaaa"},
            {"name": "monitor.exe", "browser_download_url": "https://x/monitor.exe", "digest": "sha256:bbbb"},
            {"name": "extra.zip", "browser_download_url": "https://x/extra.zip", "digest": "sha256:cccc"},
        ]);
        assert!(parse_release(&json_with(extra)).is_err());

        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn parse_release_rejects_missing_digest() {
        let no_digest = serde_json::json!([
            {"name": "quantdock.exe", "browser_download_url": "https://x/quantdock.exe"},
            {"name": "monitor.exe", "browser_download_url": "https://x/monitor.exe", "digest": "sha256:bbbb"},
        ]);
        assert!(parse_release(&json_with(no_digest)).is_err());

        let wrong_prefix = serde_json::json!([
            {"name": "quantdock.exe", "browser_download_url": "https://x/quantdock.exe", "digest": "md5:aaaa"},
            {"name": "monitor.exe", "browser_download_url": "https://x/monitor.exe", "digest": "sha256:bbbb"},
        ]);
        assert!(parse_release(&json_with(wrong_prefix)).is_err());

        let no_url = serde_json::json!([
            {"name": "quantdock.exe", "digest": "sha256:aaaa"},
            {"name": "monitor.exe", "browser_download_url": "https://x/monitor.exe", "digest": "sha256:bbbb"},
        ]);
        assert!(parse_release(&json_with(no_url)).is_err());
    }

    #[test]
    fn version_compare_grammar_and_order() {
        assert!(valid_tag("v0.3.0"));
        assert!(!valid_tag("0.3.0"));
        assert!(!valid_tag("v0.3"));
        assert!(!valid_tag("v0.3.1.2"));
        assert!(!valid_tag("v0.3."));
        assert!(!valid_tag("v0.3.a"));
        assert!(!valid_tag("V0.3.0"));

        assert!(version_is_newer("v0.3.0", "0.2.0"));
        assert!(version_is_newer("v0.3.0", "v0.2.9"));
        assert!(version_is_newer("v0.2.1", "0.2.0"));
        assert!(!version_is_newer("v0.2.0", "0.2.0"));
        assert!(!version_is_newer("v0.1.9", "0.2.0"));
        // Unparseable tag or floor never offers an update.
        assert!(!version_is_newer("v0.3", "0.2.0"));
        assert!(!version_is_newer("0.3.0", "0.2.0"));
        assert!(!version_is_newer("v0.3.0", "0.3"));
        assert!(!version_is_newer("v0.3.0", "garbage"));
    }

    #[test]
    fn version_floor_prefers_installed_tag() {
        // The shipped v0.2.1 tag disagrees with Cargo.toml (0.2.0), so the
        // persisted installed tag must win as the floor.
        assert_eq!(version_floor(Some("v0.2.1")), "v0.2.1");
        assert_eq!(version_floor(Some("0.2.1")), "0.2.1");
        assert_eq!(version_floor(Some("nonsense")), env!("CARGO_PKG_VERSION"));
        assert_eq!(version_floor(None), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn is_dev_path_detects_target_profiles() {
        assert!(is_dev_path(Path::new(
            r"C:\x\target\release\quantdock.exe"
        )));
        assert!(is_dev_path(Path::new(r"C:\x\target\debug\monitor.exe")));
        assert!(is_dev_path(Path::new("target/release/quantdock.exe")));
        assert!(!is_dev_path(Path::new(
            r"C:\Tools\QuantDock\quantdock.exe"
        )));
        assert!(!is_dev_path(Path::new(
            r"C:\x\target\bin\quantdock.exe"
        )));
    }

    #[test]
    fn writable_probes_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("qd-writable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(writable(&dir));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir(&dir).unwrap();
        assert!(!writable(&dir.join("missing")));
    }
}
