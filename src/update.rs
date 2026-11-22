//! Shared autoupdate helpers: release parsing, version comparison and install paths.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
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

/// Staging root: a dedicated folder outside the install directory, so a failed
/// stage never leaves a final exe beside the running ones.
pub fn pending_dir() -> PathBuf {
    crate::monitor_config::data_dir().join("updates").join("pending")
}

/// Stream one asset into `<name>.part` while hashing it; return the bare hex digest.
fn download_hashed(url: &str, part: &Path) -> Result<String, String> {
    let mut response = crate::hf::download_agent().get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code} from {url}"),
        other => other.to_string(),
    })?;
    let mut file =
        std::fs::File::create(part).map_err(|e| format!("{e} creating {}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    {
        let mut reader = response.body_mut().as_reader();
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("{e} from {url}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| format!("{e} writing {}", part.display()))?;
        }
    }
    file.flush()
        .map_err(|e| format!("{e} writing {}", part.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download and verify both assets in `pending`. Final names appear only after
/// both digests match; the tag marker is written last. Any failure removes every
/// staged path this call created, never anything else in the folder.
pub fn stage(release: &Release, pending: &Path) -> Result<(), String> {
    std::fs::create_dir_all(pending)
        .map_err(|e| format!("{e} creating {}", pending.display()))?;
    let parts: Vec<PathBuf> = release
        .assets
        .iter()
        .map(|a| pending.join(format!("{}.part", a.name)))
        .collect();
    let finals: Vec<PathBuf> = release.assets.iter().map(|a| pending.join(&a.name)).collect();
    let marker_part = pending.join("version.txt.part");
    let marker = pending.join("version.txt");

    let result = (|| -> Result<(), String> {
        for (asset, part) in release.assets.iter().zip(&parts) {
            let actual = download_hashed(&asset.url, part)?;
            if !actual.eq_ignore_ascii_case(&asset.sha256) {
                return Err(format!(
                    "{} digest mismatch: expected {}, got {actual}",
                    asset.name, asset.sha256
                ));
            }
        }
        // Nothing final exists until every byte of both assets was verified.
        for (part, final_path) in parts.iter().zip(&finals) {
            std::fs::rename(part, final_path)
                .map_err(|e| format!("{e} staging {}", final_path.display()))?;
        }
        // The marker is the commit point. Write it beside its target first so a
        // crash mid-write cannot leave a readable-but-truncated marker behind.
        std::fs::write(&marker_part, &release.tag)
            .map_err(|e| format!("{e} writing {}", marker_part.display()))?;
        std::fs::rename(&marker_part, &marker)
            .map_err(|e| format!("{e} writing {}", marker.display()))?;
        Ok(())
    })();

    if result.is_err() {
        for path in parts.iter().chain(&finals).chain([&marker_part, &marker]) {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

/// A kept set is useful only when its marker names exactly `tag` and both exes
/// are present. A stray marker or half set is never mistaken for a complete one.
pub fn staged_complete(pending: &Path, tag: &str) -> bool {
    let Ok(marker) = std::fs::read_to_string(pending.join("version.txt")) else {
        return false;
    };
    marker.trim() == tag.trim()
        && pending.join("quantdock.exe").is_file()
        && pending.join("monitor.exe").is_file()
}

/// Drop every partial and final file unless a complete set newer than `floor`
/// remains. Returns true only when a usable set was kept.
pub fn sweep_pending(pending: &Path, floor: &str) -> bool {
    let usable = std::fs::read_to_string(pending.join("version.txt"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|tag| version_is_newer(tag, floor))
        .is_some_and(|tag| staged_complete(pending, &tag));
    if usable {
        return true;
    }
    if let Ok(entries) = std::fs::read_dir(pending) {
        for entry in entries.flatten() {
            let path = entry.path();
            let removed = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            let _ = removed;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

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

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "quantdock_update_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Serve `bodies` in order on a loopback socket. `watch`, when set, flags a
    /// final staged exe or the marker appearing while a body is still being served.
    fn serve(
        bodies: Vec<Vec<u8>>,
        watch: Option<(PathBuf, Arc<AtomicBool>)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.ends_with(b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                if let Some((dir, flag)) = &watch {
                    if dir.join("quantdock.exe").exists()
                        || dir.join("monitor.exe").exists()
                        || dir.join("version.txt").exists()
                    {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (url, handle)
    }

    fn asset(name: &str, url: &str, body: &[u8]) -> Asset {
        Asset {
            name: name.to_owned(),
            url: url.to_owned(),
            sha256: format!("{:x}", Sha256::digest(body)),
        }
    }

    #[test]
    fn stage_writes_both_files_and_marker_last() {
        let pending = tmp("stage_ok");
        // Larger than the read buffer, so hashing spans several bounded reads.
        let qd = vec![0x51u8; 200 * 1024];
        let mon = vec![0x4du8; 130 * 1024];
        let premature = Arc::new(AtomicBool::new(false));
        let (url, server) = serve(
            vec![qd.clone(), mon.clone()],
            Some((pending.clone(), premature.clone())),
        );
        let release = Release {
            tag: "v9.9.9".to_owned(),
            notes: String::new(),
            assets: vec![
                asset("quantdock.exe", &format!("{url}/quantdock.exe"), &qd),
                asset("monitor.exe", &format!("{url}/monitor.exe"), &mon),
            ],
        };

        stage(&release, &pending).unwrap();
        server.join().unwrap();

        assert!(
            !premature.load(Ordering::SeqCst),
            "a final exe or marker appeared before both assets were served"
        );
        assert_eq!(std::fs::read(pending.join("quantdock.exe")).unwrap(), qd);
        assert_eq!(std::fs::read(pending.join("monitor.exe")).unwrap(), mon);
        assert_eq!(
            std::fs::read_to_string(pending.join("version.txt")).unwrap(),
            "v9.9.9"
        );
        assert!(!pending.join("quantdock.exe.part").exists());
        assert!(!pending.join("monitor.exe.part").exists());
        assert!(pending_dir().ends_with("pending"));
    }

    #[test]
    fn stage_rejects_bad_digest_and_cleans_up() {
        let pending = tmp("stage_bad");
        let qd = b"good quantdock".to_vec();
        let mon = b"tampered monitor".to_vec();
        let (url, server) = serve(vec![qd.clone(), mon], None);
        let release = Release {
            tag: "v9.9.8".to_owned(),
            notes: String::new(),
            assets: vec![
                asset("quantdock.exe", &format!("{url}/quantdock.exe"), &qd),
                Asset {
                    name: "monitor.exe".to_owned(),
                    url: format!("{url}/monitor.exe"),
                    sha256: "0".repeat(64),
                },
            ],
        };
        // An unrelated pre-existing file is not a staged file and must survive.
        std::fs::write(pending.join("keep.txt"), b"keep").unwrap();

        let err = stage(&release, &pending).unwrap_err();
        server.join().unwrap();

        assert!(
            err.contains("monitor.exe"),
            "error should name the bad asset: {err}"
        );
        for name in [
            "quantdock.exe",
            "monitor.exe",
            "quantdock.exe.part",
            "monitor.exe.part",
            "version.txt",
        ] {
            assert!(!pending.join(name).exists(), "{name} survived failed stage");
        }
        assert_eq!(std::fs::read(pending.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn staged_complete_requires_marker_and_both_exes() {
        let pending = tmp("complete");
        std::fs::write(pending.join("quantdock.exe"), b"q").unwrap();
        std::fs::write(pending.join("monitor.exe"), b"m").unwrap();
        std::fs::write(pending.join("version.txt"), "v1.2.3\n").unwrap();

        assert!(staged_complete(&pending, "v1.2.3"));
        assert!(!staged_complete(&pending, "v1.2.4"));

        std::fs::remove_file(pending.join("version.txt")).unwrap();
        assert!(!staged_complete(&pending, "v1.2.3"));

        std::fs::write(pending.join("version.txt"), "v1.2.3").unwrap();
        std::fs::remove_file(pending.join("monitor.exe")).unwrap();
        assert!(!staged_complete(&pending, "v1.2.3"));
    }

    #[test]
    fn sweep_keeps_newer_complete_set_and_drops_others() {
        let base = tmp("sweep");
        let set = |name: &str, marker: Option<&str>, exes: usize| -> PathBuf {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            if exes > 0 {
                std::fs::write(dir.join("quantdock.exe"), b"q").unwrap();
            }
            if exes > 1 {
                std::fs::write(dir.join("monitor.exe"), b"m").unwrap();
            }
            if let Some(tag) = marker {
                std::fs::write(dir.join("version.txt"), tag).unwrap();
            }
            dir
        };

        let good = set("good", Some("v0.3.0"), 2);
        assert!(sweep_pending(&good, "0.2.0"), "usable set was swept");
        assert!(good.join("version.txt").exists());

        for (name, marker, exes) in [
            ("old", Some("v0.2.0"), 2),
            ("garbage", Some("not-a-tag"), 2),
            ("no_marker", None, 2),
            ("one_exe", Some("v9.0.0"), 1),
            ("part_only", None, 0),
        ] {
            let dir = set(name, marker, exes);
            if name == "part_only" {
                std::fs::write(dir.join("quantdock.exe.part"), b"x").unwrap();
            }
            assert!(!sweep_pending(&dir, "0.2.0"), "{name} was kept");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "{name} not emptied");
        }
    }
}
