//! Shared autoupdate helpers: release parsing, version comparison and install paths.

use crate::hidden;
use crate::monitor_config::UpdatesSettings;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Structured markers live outside `pending\` so the launch sweep cannot eat
/// them. The script writes all three files (ok, failed, error) here.
pub fn markers_dir() -> PathBuf {
    crate::monitor_config::data_dir().join("updates")
}

/// The swap script's report to the next manager launch. `failed` is `(tag,
/// reason)`; a failed marker wins over an ok marker in `apply_markers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markers {
    pub ok: Option<String>,
    pub failed: Option<(String, String)>,
}

fn ok_marker(dir: &Path) -> PathBuf {
    dir.join("update-ok.txt")
}

fn failed_marker(dir: &Path) -> PathBuf {
    dir.join("failed-version.txt")
}

fn update_error_marker(dir: &Path) -> PathBuf {
    dir.join("update-error.txt")
}

/// Missing or empty files are simply absent markers. `failed-version.txt` is
/// `tag\nreason` (written by the generated script).
pub fn read_markers(dir: &Path) -> Markers {
    let ok = std::fs::read_to_string(ok_marker(dir))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let failed = std::fs::read_to_string(failed_marker(dir)).ok().and_then(|s| {
        let (tag, reason) = s.split_once('\n').unwrap_or((&s, ""));
        let tag = tag.trim();
        (!tag.is_empty()).then(|| (tag.to_owned(), reason.trim().to_owned()))
    });
    Markers { ok, failed }
}

pub fn clear_markers(dir: &Path) {
    let _ = std::fs::remove_file(ok_marker(dir));
    let _ = std::fs::remove_file(failed_marker(dir));
}

pub fn write_ok(dir: &Path, tag: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{e} creating {}", dir.display()))?;
    std::fs::write(ok_marker(dir), format!("{tag}\n"))
        .map_err(|e| format!("{e} writing {}", ok_marker(dir).display()))
}

pub fn write_failed(dir: &Path, tag: &str, reason: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{e} creating {}", dir.display()))?;
    std::fs::write(failed_marker(dir), format!("{tag}\n{reason}\n"))
        .map_err(|e| format!("{e} writing {}", failed_marker(dir).display()))
}

pub fn read_update_error(dir: &Path) -> Option<String> {
    std::fs::read_to_string(update_error_marker(dir))
        .ok()
        .map(|s| s.trim().to_owned())
}

pub fn clear_update_error(dir: &Path) {
    let _ = std::fs::remove_file(update_error_marker(dir));
}

/// Apply the script's report to the persisted updates table. A failed tag is
/// authoritative: it suppresses the ok marker, records the failure time and
/// drops a matching installed tag so the floor cannot be raised by a rollback.
pub fn apply_markers(updates: &mut UpdatesSettings, markers: &Markers, now_secs: u64) {
    if let Some((tag, _reason)) = &markers.failed {
        updates.last_failed_version = Some(tag.clone());
        updates.last_failed_at = Some(now_secs);
        if updates.last_installed_version.as_deref() == Some(tag.as_str()) {
            updates.last_installed_version = None;
        }
    } else if let Some(tag) = &markers.ok {
        updates.last_installed_version = Some(tag.clone());
        updates.last_failed_version = None;
        updates.last_failed_at = None;
    }
}

/// True while the persisted 24 h snooze is still in the future. A timestamp is
/// used instead of an `Instant` so the snooze survives a restart.
pub fn snoozed(snoozed_until: Option<u64>, now: u64) -> bool {
    snoozed_until.is_some_and(|until| until > now)
}

/// A manual request always runs; an automatic one runs only when enabled and a
/// full day has elapsed. `saturating_duration_since` keeps a clock wobble from
/// panicking or accidentally making a future timestamp look due.
pub fn auto_check_due(enabled: bool, manual: bool, last: Instant, now: Instant) -> bool {
    manual || (enabled && now.saturating_duration_since(last) >= Duration::from_secs(24 * 3600))
}

/// The failed tag is retried automatically one day after it was recorded.
pub fn failed_tag_expired(last_failed_at: Option<u64>, now_secs: u64) -> bool {
    last_failed_at.is_some_and(|at| now_secs.saturating_sub(at) >= 24 * 3600)
}

/// The version floor always applies (no downgrades, even by manual request).
/// Suppression and snooze hold back automatic checks only.
pub fn should_offer(
    tag: &str,
    floor: &str,
    suppressed: Option<&str>,
    snoozed: bool,
    manual: bool,
) -> bool {
    version_is_newer(tag, floor)
        && (manual || (!snoozed && !suppressed.is_some_and(|s| s == tag)))
}

/// Fetch and parse the latest release from `url` (injectable for the loopback
/// test). The two headers are required by the GitHub API; a release that is not
/// newer, or is suppressed/snoozed, is reported as `Ok(None)`.
pub fn check_latest_at(
    url: &str,
    floor: &str,
    suppressed: Option<&str>,
    snoozed: bool,
    manual: bool,
) -> Result<Option<Release>, String> {
    let agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .header("User-Agent", format!("QuantDock/{}", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(code) => format!("HTTP {code} from {url}"),
            other => other.to_string(),
        })?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{e} reading {url}"))?;
    let release = parse_release(&body)?;
    Ok(should_offer(&release.tag, floor, suppressed, snoozed, manual).then_some(release))
}

pub fn check_latest(
    floor: &str,
    suppressed: Option<&str>,
    snoozed: bool,
    manual: bool,
) -> Result<Option<Release>, String> {
    check_latest_at(RELEASES_API, floor, suppressed, snoozed, manual)
}

/// Background failures stay quiet; a manual check always reports its outcome.
pub fn should_report_check_failure(manual: bool) -> bool {
    manual
}

/// Consume the swap script's markers into a persisted `updates` table. This is
/// the only mark-consumer in the launch path: it reuses `apply_markers`, so the
/// user's toggle and snooze are never touched. Returns the marker tag that is
/// newer than the floor — the one that survives the launch sweep.
pub fn launch_update_state(
    updates: &mut UpdatesSettings,
    markers: &Markers,
    floor: &str,
    now_secs: u64,
) -> Option<String> {
    apply_markers(updates, markers, now_secs);
    markers
        .failed
        .as_ref()
        .map(|(tag, _)| tag.clone())
        .or_else(|| markers.ok.clone())
        .filter(|tag| version_is_newer(tag, floor))
}

const OVERLAY_GUARD: &str = r#"set "MON_PID=@MON@"
tasklist /FI "PID eq @MON@" /NH | findstr /C:"@MON@" >nul
if errorlevel 1 goto overlay_gone
echo Update aborted: overlay monitor.exe (PID @MON@) is still running.>"%MARKERS%\update-error.txt"
start "" "%INSTALL%\quantdock.exe"
exit /b 1
:overlay_gone"#;

/// Hidden swap script. It waits for the manager PID, optionally checks that
/// the exact overlay PID is gone, swaps both exes with a guarded rename/move,
/// writes `update-ok.txt` before starting the new exe, and rolls back with an
/// error marker on every failure path.
const SWAP_SCRIPT: &str = r#"@echo off
setlocal
set "INSTALL=@INSTALL@"
set "PENDING=@PENDING@"
set "MARKERS=@MARKERS@"
set "QD_PID=@PID@"

rem Wait for the manager to exit, bounded to 60 s.
set /a QD_WAIT=0
:wait_manager
tasklist /FI "PID eq %QD_PID%" /NH | findstr /C:"%QD_PID%" >nul
if errorlevel 1 goto manager_gone
set /a QD_WAIT+=1
if %QD_WAIT% GEQ 60 goto fail_no_swap
ping -n 2 127.0.0.1 >nul
goto wait_manager

:manager_gone
@OVERLAY_GUARD@
rem Let Windows release the just-exited exe's file locks.
ping -n 3 127.0.0.1 >nul

set "TAG="
for /f "usebackq delims=" %%i in ("%PENDING%\version.txt") do set "TAG=%%i"
if not defined TAG goto fail_no_swap

if exist "%INSTALL%\quantdock.exe.old" del /f /q "%INSTALL%\quantdock.exe.old"
if errorlevel 1 goto fail_no_swap
ren "%INSTALL%\quantdock.exe" quantdock.exe.old
if errorlevel 1 goto rollback
if exist "%INSTALL%\monitor.exe.old" del /f /q "%INSTALL%\monitor.exe.old"
if errorlevel 1 goto rollback
ren "%INSTALL%\monitor.exe" monitor.exe.old
if errorlevel 1 goto rollback

move /y "%PENDING%\quantdock.exe" "%INSTALL%\quantdock.exe"
if errorlevel 1 goto rollback
move /y "%PENDING%\monitor.exe" "%INSTALL%\monitor.exe"
if errorlevel 1 goto rollback

rem The ok marker must exist before the new manager starts and reads it.
echo %TAG%>"%MARKERS%\update-ok.txt"

set "NEW_PID="
for /f "usebackq delims=" %%p in (`powershell -NoProfile -Command "$p = Start-Process -PassThru -FilePath '%INSTALL%\quantdock.exe'; Write-Output $p.Id"`) do set "NEW_PID=%%p"
if not defined NEW_PID goto rollback

set /a START_WAIT=0
:check_start
set "VERDICT="
for /f "usebackq delims=" %%v in (`powershell -NoProfile -Command "$p = Get-Process -Id %NEW_PID% -ErrorAction SilentlyContinue; if ($null -eq $p) { Write-Output GONE } elseif ($p.HasExited) { Write-Output EXITED; Write-Output $p.ExitCode } else { Write-Output RUNNING }"`) do set "VERDICT=%%v"
if "%VERDICT%"=="RUNNING" goto swap_success
set /a START_WAIT+=1
if %START_WAIT% GEQ 15 goto rollback
ping -n 2 127.0.0.1 >nul
goto check_start

:swap_success
del /f /q "%MARKERS%\failed-version.txt" "%MARKERS%\update-error.txt"
del /f /q "%INSTALL%\quantdock.exe.old" "%INSTALL%\monitor.exe.old"
del /f /q "%PENDING%\quantdock.exe" "%PENDING%\monitor.exe" "%PENDING%\version.txt"
exit /b 0

:rollback
set "REASON=new quantdock.exe did not stay running after the swap"
echo Update failed: %REASON%>"%MARKERS%\update-error.txt"
echo %TAG%>"%MARKERS%\failed-version.txt"
echo %REASON%>>"%MARKERS%\failed-version.txt"
if exist "%INSTALL%\quantdock.exe.old" (
if exist "%INSTALL%\quantdock.exe" del /f /q "%INSTALL%\quantdock.exe"
ren "%INSTALL%\quantdock.exe.old" quantdock.exe
)
if exist "%INSTALL%\monitor.exe.old" (
if exist "%INSTALL%\monitor.exe" del /f /q "%INSTALL%\monitor.exe"
ren "%INSTALL%\monitor.exe.old" monitor.exe
)
del /f /q "%PENDING%\quantdock.exe" "%PENDING%\monitor.exe" "%PENDING%\version.txt"
start "" "%INSTALL%\quantdock.exe"
exit /b 1

:fail_no_swap
echo Update aborted before swapping; staged set left in place.>"%MARKERS%\update-error.txt"
tasklist /FI "PID eq %QD_PID%" /NH | findstr /C:"%QD_PID%" >nul
if errorlevel 1 start "" "%INSTALL%\quantdock.exe"
exit /b 1
"#;

pub fn generate_script(
    install: &Path,
    pending: &Path,
    markers: &Path,
    pid: u32,
    monitor_pid: Option<u32>,
) -> String {
    let overlay_guard = match monitor_pid {
        Some(mon) => OVERLAY_GUARD.replace("@MON@", &mon.to_string()),
        None => String::new(),
    };
    SWAP_SCRIPT
        .replace("@INSTALL@", &install.display().to_string())
        .replace("@PENDING@", &pending.display().to_string())
        .replace("@MARKERS@", &markers.display().to_string())
        .replace("@PID@", &pid.to_string())
        .replace("@OVERLAY_GUARD@", &overlay_guard)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
}

/// Fire-and-forget: the script waits for this process's PID, so waiting here
/// would deadlock the exit. Drop the child handle before quitting the manager.
pub fn spawn_script(script: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new("cmd");
    hidden(&mut cmd);
    cmd.arg("/C")
        .arg(script)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{e} launching {}", script.display()))
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
    use std::time::{Duration, Instant};

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

    #[test]
    fn read_markers_reads_both_files() {
        let dir = tmp("markers");
        std::fs::write(dir.join("update-ok.txt"), "v1.2.3\n").unwrap();
        std::fs::write(
            dir.join("failed-version.txt"),
            "v1.2.4\nnew exe exited before the ready poll\n",
        )
        .unwrap();
        let m = read_markers(&dir);
        assert_eq!(m.ok.as_deref(), Some("v1.2.3"));
        assert_eq!(
            m.failed.as_ref().map(|(t, r)| (t.as_str(), r.as_str())),
            Some(("v1.2.4", "new exe exited before the ready poll"))
        );

        // Missing files are tolerated, and the writer/clearer round-trip.
        let empty = tmp("markers_empty");
        let m = read_markers(&empty);
        assert!(m.ok.is_none() && m.failed.is_none());
        write_ok(&empty, "v2.0.0").unwrap();
        write_failed(&empty, "v2.0.1", "crashed").unwrap();
        let m = read_markers(&empty);
        assert_eq!(m.ok.as_deref(), Some("v2.0.0"));
        assert_eq!(
            m.failed.as_ref().map(|(t, r)| (t.as_str(), r.as_str())),
            Some(("v2.0.1", "crashed"))
        );
        clear_markers(&empty);
        let m = read_markers(&empty);
        assert!(m.ok.is_none() && m.failed.is_none());
        assert!(markers_dir().ends_with("updates"));
    }

    #[test]
    fn update_error_is_read_then_cleared_on_dismiss() {
        let dir = tmp("update_error");
        assert!(read_update_error(&dir).is_none());
        std::fs::write(dir.join("update-error.txt"), "swap failed: move error").unwrap();
        assert_eq!(
            read_update_error(&dir).as_deref(),
            Some("swap failed: move error")
        );
        clear_update_error(&dir);
        assert!(read_update_error(&dir).is_none());
        assert!(!dir.join("update-error.txt").exists());
    }

    #[test]
    fn apply_markers_failed_wins_and_clears_floor() {
        // A rollback for the same tag as the last installed one must drop the
        // floor so the release can be offered again.
        let mut updates = UpdatesSettings {
            last_installed_version: Some("v1.2.3".to_owned()),
            ..Default::default()
        };
        let markers = Markers {
            ok: Some("v1.2.3".to_owned()),
            failed: Some(("v1.2.3".to_owned(), "crashed".to_owned())),
        };
        apply_markers(&mut updates, &markers, 1_700_000_000);
        assert_eq!(updates.last_failed_version.as_deref(), Some("v1.2.3"));
        assert_eq!(updates.last_failed_at, Some(1_700_000_000));
        assert_eq!(updates.last_installed_version, None);
    }

    #[test]
    fn apply_markers_ok_sets_installed_and_clears_failed() {
        let mut updates = UpdatesSettings {
            last_failed_version: Some("v1.0.0".to_owned()),
            last_failed_at: Some(42),
            ..Default::default()
        };
        let markers = Markers {
            ok: Some("v1.1.0".to_owned()),
            failed: None,
        };
        apply_markers(&mut updates, &markers, 99);
        assert_eq!(updates.last_installed_version.as_deref(), Some("v1.1.0"));
        assert_eq!(updates.last_failed_version, None);
        assert_eq!(updates.last_failed_at, None);
    }

    fn script(monitor_pid: Option<u32>) -> String {
        generate_script(
            Path::new(r"C:\Tools\QuantDock"),
            Path::new(r"C:\AppData\QuantDock\updates\pending"),
            Path::new(r"C:\AppData\QuantDock\updates"),
            4321,
            monitor_pid,
        )
        .replace("\r\n", "\n")
    }

    #[test]
    fn script_checks_every_move_and_rolls_back() {
        let s = script(None);
        assert!(s.contains(r"C:\Tools\QuantDock"), "literal install path");
        assert!(s.contains(r"C:\AppData\QuantDock\updates\pending"), "literal pending path");
        assert!(s.contains(r#"set "QD_PID=4321""#), "literal manager PID");
        assert!(s.contains("GEQ 60"), "bounded 60 s PID wait");
        assert!(s.contains("ping -n 3 127.0.0.1"), "2 s settle");

        // Every rename/move is immediately guarded, and all four guards jump
        // to the rollback label rather than silently continuing.
        for cmd in [
            r#"ren "%INSTALL%\quantdock.exe" quantdock.exe.old"#,
            r#"ren "%INSTALL%\monitor.exe" monitor.exe.old"#,
            r#"move /y "%PENDING%\quantdock.exe" "%INSTALL%\quantdock.exe""#,
            r#"move /y "%PENDING%\monitor.exe" "%INSTALL%\monitor.exe""#,
        ] {
            assert!(
                s.contains(&format!("{cmd}\nif errorlevel 1 goto rollback")),
                "unguarded swap step: {cmd}"
            );
        }
        let rollback = s.find(":rollback").unwrap();
        let body = &s[rollback..];
        let restore = body.find(r#"ren "%INSTALL%\quantdock.exe.old" quantdock.exe"#).unwrap();
        let relaunch = body.find(r#"start "" "%INSTALL%\quantdock.exe""#).unwrap();
        assert!(restore < relaunch, "backups must be restored before relaunching");
    }

    #[test]
    fn script_writes_ok_marker_before_start() {
        let s = script(None);
        let ok = s.find(r#"echo %TAG%>"%MARKERS%\update-ok.txt""#).unwrap();
        let start = s.find("Start-Process -PassThru").unwrap();
        assert!(ok < start, "update-ok.txt must exist before the new exe starts");
        assert!(s.contains("Get-Process -Id"));
        assert!(s.contains("$p.HasExited"));
        assert!(s.contains("del /f /q \"%INSTALL%\\quantdock.exe.old\""));
    }

    #[test]
    fn script_aborts_when_overlay_pid_is_given() {
        let with = script(Some(7777));
        assert!(with.contains("tasklist /FI \"PID eq 7777\""));
        let abort = with.find("overlay monitor.exe (PID 7777) is still running").unwrap();
        let relaunch = with.find(r#"start "" "%INSTALL%\quantdock.exe""#).unwrap();
        let first_ren = with.find(r#"ren "%INSTALL%\quantdock.exe""#).unwrap();
        assert!(abort < relaunch && relaunch < first_ren, "abort must relaunch old exe without swapping");

        let without = script(None);
        assert!(!without.contains("PID eq 7777"));
        assert!(!without.contains("MON_PID"));
    }

    #[test]
    fn script_writes_update_error_on_failure() {
        let s = script(None);
        let rollback = s.find(":rollback").unwrap();
        let body = &s[rollback..];
        assert!(body.contains("update-error.txt"), "rollback writes update-error.txt");
        assert!(body.contains("failed-version.txt"), "rollback writes the failed marker");
    }

    #[test]
    fn script_rollback_never_deletes_an_unguarded_exe() {
        let s = script(None);
        let rollback = s.find(":rollback").unwrap();
        let body = &s[rollback..];
        // A live exe may only be deleted when its own .old backup exists and is
        // about to be renamed back; otherwise a failed monitor step would leave
        // a half-updated install with no overlay binary at all.
        for (exe, old) in [
            ("quantdock.exe", "quantdock.exe.old"),
            ("monitor.exe", "monitor.exe.old"),
        ] {
            let guarded = format!(
                "if exist \"%INSTALL%\\{old}\" (\n\
                 if exist \"%INSTALL%\\{exe}\" del /f /q \"%INSTALL%\\{exe}\"\n\
                 ren \"%INSTALL%\\{old}\" {exe}\n\
                 )"
            );
            assert!(
                body.contains(&guarded),
                "rollback must gate the {exe} delete on {old} existing"
            );
        }
    }

    #[test]
    fn script_success_clears_stale_failure_markers() {
        let s = script(None);
        let success = s.find(":swap_success").unwrap();
        let rollback = s.find(":rollback").unwrap();
        let body = &s[success..rollback];
        assert!(
            body.contains(r#"del /f /q "%MARKERS%\failed-version.txt" "%MARKERS%\update-error.txt""#),
            "a successful retry must clear a previous failed/error marker"
        );
    }

    #[test]
    fn spawn_script_uses_hidden_nonblocking_cmd() {
        let src = include_str!("update.rs");
        let tail = src.split("pub fn spawn_script").nth(1).unwrap();
        let body = &tail[..tail.find("\n}").unwrap()];
        assert!(body.contains("hidden("), "child must be hidden");
        assert!(body.contains(".spawn()"), "spawn must be non-blocking");
        assert!(!body.contains(".status()"), "must not wait for the script");
        assert!(!body.contains(".wait()"), "must not wait for the script");
        assert!(body.contains("cmd") && body.contains("/C"));
    }

    /// One tiny real PE stands in for both exes. Its behaviour is chosen by
    /// `QD_SWAP_DUMMY_MODE`, which the script's cmd -> PowerShell -> Start-Process
    /// chain inherits; a trailing tag lets the tests tell old from new bytes.
    fn dummy_exe(dir: &Path) -> Vec<u8> {
        let src = dir.join("dummy.rs");
        std::fs::write(
            &src,
            r#"#![windows_subsystem = "windows"]
fn main() {
    match std::env::var("QD_SWAP_DUMMY_MODE").as_deref() {
        Ok("stay") => std::thread::sleep(std::time::Duration::from_secs(25)),
        Ok("fail") => std::process::exit(1),
        _ => {}
    }
}
"#,
        )
        .unwrap();
        let exe = dir.join("dummy.exe");
        let mut c = std::process::Command::new("rustc");
        hidden(&mut c);
        let built = c
            .arg("--edition")
            .arg("2021")
            .arg("-o")
            .arg(&exe)
            .arg(&src)
            .status()
            .unwrap();
        assert!(built.success(), "rustc could not build the dummy exe");
        let bytes = std::fs::read(&exe).unwrap();
        let _ = std::fs::remove_file(&exe);
        bytes
    }

    struct SwapFolders {
        root: PathBuf,
        install: PathBuf,
        pending: PathBuf,
        markers: PathBuf,
        pid: u32,
    }

    impl SwapFolders {
        fn new() -> Self {
            let root = tmp("swap");
            let install = root.join("install");
            let pending = root.join("pending");
            let markers = root.join("markers");
            for d in [&install, &pending, &markers] {
                std::fs::create_dir_all(d).unwrap();
            }
            let base = dummy_exe(&root);
            let put = |dir: &Path, name: &str, tag: &[u8]| {
                let mut bytes = base.clone();
                bytes.extend_from_slice(tag);
                std::fs::write(dir.join(name), bytes).unwrap();
            };
            put(&install, "quantdock.exe", b"OLDQD");
            put(&install, "monitor.exe", b"OLDMON");
            put(&pending, "quantdock.exe", b"NEWQD");
            put(&pending, "monitor.exe", b"NEWMON");
            std::fs::write(pending.join("version.txt"), "v9.9.9").unwrap();

            // The script must see this PID as already gone and skip its wait.
            let mut c = std::process::Command::new("cmd");
            hidden(&mut c);
            let mut child = c.arg("/C").arg("exit 0").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();

            Self { root, install, pending, markers, pid }
        }

        fn run(&self, mode: &str) -> std::process::ExitStatus {
            let script = self.root.join("swap.cmd");
            std::fs::write(
                &script,
                generate_script(&self.install, &self.pending, &self.markers, self.pid, None),
            )
            .unwrap();
            let mut c = std::process::Command::new("cmd");
            hidden(&mut c);
            c.arg("/C").arg(&script).env("QD_SWAP_DUMMY_MODE", mode);
            let mut child = c.spawn().unwrap();
            let start = Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    return status;
                }
                if start.elapsed() > Duration::from_secs(40) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("swap script did not finish within 40 s");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        fn installed(&self, name: &str) -> Vec<u8> {
            std::fs::read(self.install.join(name)).unwrap()
        }
    }

    impl Drop for SwapFolders {
        fn drop(&mut self) {
            // Only kill a process whose exe lives in this test's temp folder;
            // never touch a real installed QuantDock.
            let path = self.install.join("quantdock.exe").display().to_string();
            let kill = format!(
                "Get-CimInstance Win32_Process -Filter \"Name='quantdock.exe'\" | Where-Object {{ $_.ExecutablePath -eq '{path}' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}"
            );
            let mut c = std::process::Command::new("powershell");
            hidden(&mut c);
            let _ = c.args(["-NoProfile", "-Command", &kill]).status();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    #[ignore = "runs a generated .cmd against temp folders"]
    fn script_swaps_both_exes_and_writes_ok_marker() {
        let f = SwapFolders::new();
        assert!(f.run("stay").success(), "a healthy new exe must swap cleanly");
        assert!(f.installed("quantdock.exe").ends_with(b"NEWQD"));
        assert!(f.installed("monitor.exe").ends_with(b"NEWMON"));
        assert!(!f.install.join("quantdock.exe.old").exists());
        assert!(!f.install.join("monitor.exe.old").exists());
        assert!(!f.pending.join("version.txt").exists());
        assert!(!f.pending.join("quantdock.exe").exists());
        let m = read_markers(&f.markers);
        assert_eq!(m.ok.as_deref(), Some("v9.9.9"));
        assert!(m.failed.is_none());
        assert!(read_update_error(&f.markers).is_none());
    }

    #[test]
    #[ignore = "runs a generated .cmd against temp folders"]
    fn script_rolls_back_and_writes_failed_marker() {
        let f = SwapFolders::new();
        assert!(!f.run("fail").success(), "a non-zero new exe must roll back");
        assert!(f.installed("quantdock.exe").ends_with(b"OLDQD"));
        assert!(f.installed("monitor.exe").ends_with(b"OLDMON"));
        assert!(!f.install.join("quantdock.exe.old").exists());
        assert!(!f.install.join("monitor.exe.old").exists());
        assert!(!f.pending.join("version.txt").exists());
        let m = read_markers(&f.markers);
        assert_eq!(
            m.failed.as_ref().map(|(t, r)| (t.as_str(), r.as_str())),
            Some(("v9.9.9", "new quantdock.exe did not stay running after the swap"))
        );
        assert!(read_update_error(&f.markers).is_some());
    }

    #[test]
    #[ignore = "runs a generated .cmd against temp folders"]
    fn script_rolls_back_when_new_process_vanishes() {
        let f = SwapFolders::new();
        assert!(
            !f.run("pass").success(),
            "a zero-exit new exe that dies immediately must still roll back"
        );
        assert!(f.installed("quantdock.exe").ends_with(b"OLDQD"));
        let m = read_markers(&f.markers);
        assert_eq!(m.failed.as_ref().map(|(t, _)| t.as_str()), Some("v9.9.9"));
    }

    const DAY: u64 = 24 * 3600;

    #[test]
    fn snooze_is_active_until_expiry() {
        // Q3: the snooze is a persisted timestamp, so it survives a restart.
        let set_at = 1_700_000_000;
        assert!(snoozed(Some(set_at + DAY), set_at));
        assert!(snoozed(Some(set_at + DAY), set_at + DAY - 1));
        // At and after expiry it no longer suppresses anything.
        assert!(!snoozed(Some(set_at + DAY), set_at + DAY));
        assert!(!snoozed(Some(set_at), set_at + DAY));
        assert!(!snoozed(None, set_at + DAY));
    }

    #[test]
    fn should_offer_honours_floor_suppression_snooze_and_manual() {
        // A genuinely newer tag on a quiet, awake install is offered.
        assert!(should_offer("v0.3.0", "0.2.0", None, false, false));
        // The floor applies to manual checks too: no downgrade or equal version.
        assert!(!should_offer("v0.2.0", "0.2.0", None, false, true));
        assert!(!should_offer("v0.1.9", "0.2.0", None, false, true));
        assert!(!should_offer("v0.3", "0.2.0", None, false, true));
        // The failed tag suppresses only the automatic cycle, only when it matches.
        assert!(!should_offer("v0.3.0", "0.2.0", Some("v0.3.0"), false, false));
        assert!(should_offer("v0.3.0", "0.2.0", Some("v0.2.9"), false, false));
        assert!(should_offer("v0.3.0", "0.2.0", Some("v0.3.0"), false, true));
        // The snooze suppresses only the automatic cycle.
        assert!(!should_offer("v0.3.0", "0.2.0", None, true, false));
        assert!(should_offer("v0.3.0", "0.2.0", None, true, true));
    }

    #[test]
    fn background_checks_skip_when_disabled() {
        let now = Instant::now();
        let stale = now - Duration::from_secs(25 * 3600);
        assert!(!auto_check_due(false, false, stale, now));
        assert!(!auto_check_due(false, false, now, now));
        // A manual request always runs, even with the toggle off and a fresh check.
        assert!(auto_check_due(false, true, now, now));
    }

    #[test]
    fn launch_check_is_due_with_stale_timestamp() {
        let now = Instant::now();
        let stale = now - Duration::from_secs(24 * 3600);
        assert!(auto_check_due(true, false, stale, now));
        assert!(!auto_check_due(true, false, now, now));
        assert!(!auto_check_due(true, false, now - Duration::from_secs(23 * 3600), now));
    }

    #[test]
    fn failed_tag_expires_after_twenty_four_hours() {
        // Q1: the next automatic cycle clears the persisted failure after a day.
        let failed = 1_700_000_000;
        assert!(!failed_tag_expired(None, failed + 10 * DAY));
        assert!(!failed_tag_expired(Some(failed), failed + DAY - 1));
        assert!(failed_tag_expired(Some(failed), failed + DAY));
        assert!(failed_tag_expired(Some(failed), failed + DAY + 1));
    }

    #[test]
    fn should_report_check_failure_only_for_manual() {
        // Acceptance 7: background failures stay quiet, manual ones always report.
        assert!(should_report_check_failure(true));
        assert!(!should_report_check_failure(false));
    }

    #[test]
    fn check_latest_sends_required_headers() {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (head_tx, head_rx) = mpsc::channel();
        let body = json_with(good_assets()).into_bytes();
        let server = std::thread::spawn(move || {
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
            head_tx.send(String::from_utf8_lossy(&head).into_owned()).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        // Manual with both blockers set: the request still goes out and the
        // release is returned, proving manual bypasses suppression and snooze.
        let release = check_latest_at(&url, "0.2.0", Some("v0.3.0"), true, true)
            .unwrap()
            .expect("a newer release should be offered on a manual check");
        assert_eq!(release.tag, "v0.3.0");
        server.join().unwrap();

        let head = head_rx.recv().unwrap().to_ascii_lowercase();
        assert!(head.starts_with("get / "), "expected a GET request: {head}");
        assert!(
            head.contains(&format!("user-agent: quantdock/{}", env!("CARGO_PKG_VERSION"))),
            "missing QuantDock User-Agent: {head}"
        );
        assert!(
            head.contains("accept: application/vnd.github+json"),
            "missing GitHub Accept header: {head}"
        );
    }

    #[test]
    fn launch_state_consumes_markers_before_sweep() {
        let now = 1_700_000_000;
        // A failed marker wins over ok, stamps the time and drops a matching
        // installed tag so the floor cannot be raised by a rollback.
        let mut updates = UpdatesSettings {
            last_installed_version: Some("v0.3.0".to_owned()),
            ..Default::default()
        };
        let markers = Markers {
            ok: Some("v0.3.0".to_owned()),
            failed: Some(("v0.3.0".to_owned(), "crashed".to_owned())),
        };
        let kept = launch_update_state(&mut updates, &markers, "0.2.0", now);
        assert_eq!(kept.as_deref(), Some("v0.3.0"));
        assert_eq!(updates.last_failed_version.as_deref(), Some("v0.3.0"));
        assert_eq!(updates.last_failed_at, Some(now));
        assert_eq!(updates.last_installed_version, None);

        // An ok marker installs its tag and clears the failure fields.
        let mut updates = UpdatesSettings {
            last_failed_version: Some("v0.2.9".to_owned()),
            last_failed_at: Some(1),
            ..Default::default()
        };
        let markers = Markers {
            ok: Some("v0.3.0".to_owned()),
            failed: None,
        };
        let kept = launch_update_state(&mut updates, &markers, "0.2.0", now);
        assert_eq!(kept.as_deref(), Some("v0.3.0"));
        assert_eq!(updates.last_installed_version.as_deref(), Some("v0.3.0"));
        assert_eq!(updates.last_failed_version, None);
        assert_eq!(updates.last_failed_at, None);

        // A marker not newer than the floor survives no sweep.
        let mut updates = UpdatesSettings::default();
        let markers = Markers {
            ok: Some("v0.2.0".to_owned()),
            failed: None,
        };
        assert_eq!(launch_update_state(&mut updates, &markers, "0.2.0", now), None);
        assert_eq!(updates.last_installed_version.as_deref(), Some("v0.2.0"));

        // No markers: no fields change and nothing is kept.
        let mut updates = UpdatesSettings::default();
        let none = Markers { ok: None, failed: None };
        assert_eq!(launch_update_state(&mut updates, &none, "0.2.0", now), None);
        assert!(updates.check_automatically);
        assert_eq!(updates.snoozed_until, None);
        assert_eq!(updates.last_failed_version, None);
        assert_eq!(updates.last_failed_at, None);
        assert_eq!(updates.last_installed_version, None);
    }

    #[test]
    fn launch_state_preserves_snooze_and_toggle() {
        // Y1: the pure mark-consumer must only touch marker fields.
        let mut updates = UpdatesSettings {
            check_automatically: false,
            snoozed_until: Some(1_700_999_999),
            ..Default::default()
        };
        let markers = Markers {
            ok: None,
            failed: Some(("v0.4.0".to_owned(), "crashed".to_owned())),
        };
        launch_update_state(&mut updates, &markers, "0.2.0", 1_700_000_000);
        assert!(!updates.check_automatically, "automatic-check toggle was reset");
        assert_eq!(updates.snoozed_until, Some(1_700_999_999), "snooze was reset");
    }

    #[test]
    fn release_workflow_checks_tag_against_version() {
        // Acceptance 9: the workflow is the guard that a published tag agrees
        // with the package version (the shipped v0.2.1 tag did not).
        let wf = include_str!("../.github/workflows/release.yml");
        assert!(
            wf.contains("github.ref_name"),
            "release workflow must compare the pushed tag"
        );
        assert!(
            wf.contains("cargo metadata"),
            "release workflow must read the package version from cargo metadata"
        );
        assert!(
            wf.contains("cargo test --all-targets script -- --ignored"),
            "release workflow must run the ignored swap gate on windows-latest"
        );
    }

    #[test]
    fn docs_describe_update_flow_and_manual_fallback() {
        // Acceptance 9: the flow is documented with the writable-folder
        // requirement and the still-supported manual download fallback.
        let readme = include_str!("../README.md");
        let install = include_str!("../INSTALL.md");
        assert!(
            readme.contains("Check for updates"),
            "README must document the update action"
        );
        assert!(
            readme.contains("writable"),
            "README must state the install folder must be writable"
        );
        assert!(
            install.contains("manual download"),
            "INSTALL must note manual download still works"
        );
    }
}
