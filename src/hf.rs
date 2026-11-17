//! HuggingFace GGUF download support: repo/URL parsing, tree listing, streaming
//! downloads, TOML generation and the stranded-partial sweep.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const API: &str = "https://huggingface.co/api/models";
const FILES: &str = "https://huggingface.co";

/// One GGUF in a repo's file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfFile {
    pub path: String,
    pub size: u64,
}

/// One download the dialog asked for.
#[derive(Debug, Clone)]
pub struct DownloadSpec {
    pub repo: String,
    pub branch: String,
    /// Path inside the repo; kept so a nested repo mirrors its own layout.
    pub path: String,
    /// Size from the listing, used until the response reports a length.
    pub size: u64,
}

/// What the download thread reports to the dialog.
#[derive(Debug)]
pub enum Progress {
    /// Emitted by the dialog while its own `list_ggufs` call runs.
    Listing,
    Downloading { bytes: u64, total: u64 },
    Done { folder: PathBuf, toml: PathBuf },
    Failed(String),
    Cancelled,
}

// listing only: timeout_global, never for the download body.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .build()
            .new_agent()
    })
}

/// Accept `owner/name` or any huggingface.co URL for a repo or a file in it.
/// Returns `(owner/name, branch)`; the branch defaults to `main`.
pub fn parse_repo(input: &str) -> Option<(String, String)> {
    let path = input.trim().split(['?', '#']).next().unwrap_or_default();
    let (is_url, rest) = match path
        .strip_prefix("https://")
        .or_else(|| path.strip_prefix("http://"))
    {
        Some(rest) => (true, rest),
        None => (false, path),
    };
    let mut segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    // A URL's first segment is always the host. A bare id may carry a dot in the
    // owner, so only treat one as a host when a repo path follows it.
    if is_url || (segs.len() >= 3 && segs[0].contains('.')) {
        // A bare `https://` has no host: garbage in, `None` out, never a panic.
        let host = segs.first().copied()?;
        segs.remove(0);
        if host != "huggingface.co" && !host.ends_with(".huggingface.co") {
            return None;
        }
    }
    let (owner, name) = (segs.first()?, segs.get(1)?);
    let branch = match segs.get(2).copied() {
        Some("tree" | "blob" | "resolve") => segs.get(3).copied().unwrap_or("main"),
        _ => "main",
    };
    Some((format!("{owner}/{name}"), branch.to_owned()))
}

/// Keep the `.gguf` files from a tree-API response, sorted by path.
pub fn ggufs_from_tree(json: &serde_json::Value) -> Result<Vec<HfFile>, String> {
    let entries = json.as_array().ok_or("unexpected tree response")?;
    let mut files: Vec<HfFile> = entries
        .iter()
        .filter(|e| e["type"] == "file")
        .filter_map(|e| {
            let path = e["path"].as_str()?;
            path.to_ascii_lowercase()
                .ends_with(".gguf")
                .then(|| HfFile {
                    path: path.to_owned(),
                    size: e["size"].as_u64().unwrap_or(0),
                })
        })
        .collect();
    // Case-folded on purpose: the brief expects `a.gguf` before `B.GGUF`.
    files.sort_by_cached_key(|f| f.path.to_ascii_lowercase());
    if files.is_empty() {
        return Err("no .gguf files".to_owned());
    }
    Ok(files)
}

/// List the GGUFs in `repo` at `branch`, recursively.
pub fn list_ggufs(repo: &str, branch: &str) -> Result<Vec<HfFile>, String> {
    // shortcut: tree listing reads one page (HF caps at 1000 entries); add Link-header cursor looping only when a >1000-entry repo shows up.
    let url = format!("{API}/{repo}/tree/{branch}?recursive=true");
    let mut resp = agent().get(&url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code} from {url}"),
        other => other.to_string(),
    })?;
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{e} from {url}"))?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("{e} from {url}"))?;
    ggufs_from_tree(&json).map_err(|e| format!("{e} in {repo}"))
}

/// Folder to download into: the repo's name part, suffixed `-2`, `-3`, … when that
/// folder already holds anything or is named `old` (discovery skips it forever).
pub fn folder_name(spec: &DownloadSpec, root: &Path) -> String {
    let base = match spec.repo.rsplit('/').next().unwrap_or_default() {
        "" | "." | ".." => "model",
        name => name,
    };
    // Only a folder with no entry at all is reusable: merging into a nested-only
    // folder would let a second quant overwrite the first quant's `.toml`.
    let free = |name: &str| !name.eq_ignore_ascii_case("old") && is_empty(&root.join(name));
    let mut name = base.to_owned();
    let mut n = 2;
    while !free(&name) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

fn is_empty(dir: &Path) -> bool {
    // Never call an unreadable folder reusable: merging into a real model folder
    // could overwrite its `.toml`.
    !dir.exists()
        || std::fs::read_dir(dir)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
}

/// Build the download agent. `timeout_global` and `timeout_recv_body` are
/// total-body budgets and would abort a healthy multi-GB transfer; a per-read
/// idle timeout is what keeps a stalled connection from wedging the thread.
fn download_agent() -> ureq::Agent {
    ureq::config::Config::builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
}

/// Stream the GGUF into `<root>\<folder>\<repo path>` on a background thread.
pub fn start_download(
    spec: DownloadSpec,
    root: &Path,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Progress>,
) {
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let folder = folder_name(&spec, &root);
        let msg = match fetch(&spec, &root, &folder, &cancel, &tx) {
            Ok((folder, toml)) => Progress::Done { folder, toml },
            Err(Stop::Cancelled) => Progress::Cancelled,
            Err(Stop::Failed(e)) => Progress::Failed(e),
        };
        let _ = tx.send(msg);
    });
}

#[derive(Debug)]
enum Stop {
    Failed(String),
    Cancelled,
}

fn fetch(
    spec: &DownloadSpec,
    root: &Path,
    folder: &str,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<Progress>,
) -> Result<(PathBuf, PathBuf), Stop> {
    let model_dir = root.join(folder);
    let target = model_dir.join(safe_rel(&spec.path)?);
    let part = part_path(&target);
    // Must be read before anything is created, so cleanup owns only what we made.
    let created = missing_dirs(target.parent().unwrap_or(&model_dir), root);

    if let Err(e) = copy(spec, &target, cancel, tx) {
        cleanup(&part, &created);
        return Err(e);
    }
    match write_model_toml(&model_dir, &spec.path) {
        Ok(toml) => Ok((model_dir, toml)),
        Err(e) => {
            // A model folder with no `.toml` is invisible to discovery and would
            // force every retry into a `-2` folder.
            let _ = std::fs::remove_file(&target);
            cleanup(&part, &created);
            Err(Stop::Failed(e))
        }
    }
}

fn copy(
    spec: &DownloadSpec,
    target: &Path,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<Progress>,
) -> Result<(), Stop> {
    let url = format!("{FILES}/{}/resolve/{}/{}", spec.repo, spec.branch, spec.path);
    let part = part_path(target);
    let dir = target.parent().unwrap_or(Path::new("."));
    // Checked before any filesystem work so a pre-set flag creates nothing.
    if cancel.load(Ordering::Relaxed) {
        return Err(Stop::Cancelled);
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| Stop::Failed(format!("{e} creating {}", dir.display())))?;

    let mut resp = download_agent().get(&url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => Stop::Failed(format!("HTTP {code} from {url}")),
        other => Stop::Failed(format!("{other} from {url}")),
    })?;
    let total = resp.body().content_length().unwrap_or(spec.size);
    let mut file = std::fs::File::create(&part)
        .map_err(|e| Stop::Failed(format!("{e} creating {}", part.display())))?;
    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    let mut sent = 0u64;
    // Start the bar even when the file is under one MiB and no chunk message fires.
    let _ = tx.send(Progress::Downloading { bytes: 0, total });
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| Stop::Failed(format!("{e} from {url}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| Stop::Failed(format!("{e} writing {}", part.display())))?;
        bytes += n as u64;
        // One message per MiB keeps the dialog channel from flooding.
        if bytes - sent >= 1 << 20 {
            sent = bytes;
            let _ = tx.send(Progress::Downloading { bytes, total });
        }
    }
    file.flush()
        .map_err(|e| Stop::Failed(format!("{e} writing {}", part.display())))?;
    drop(file);
    if bytes < total {
        return Err(Stop::Failed(format!(
            "short body: {bytes} of {total} bytes from {url}"
        )));
    }
    let _ = tx.send(Progress::Downloading { bytes, total });
    finalize_part(&part, target).map_err(Stop::Failed)
}

/// The HF path relative to the model folder; refuse anything that could escape it.
fn safe_rel(path: &str) -> Result<PathBuf, Stop> {
    let rel = Path::new(path);
    if path.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(Stop::Failed(format!("unsafe file path {path}")));
    }
    Ok(rel.to_path_buf())
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// The ancestors of `dir` that do not exist yet, deepest first, never above `stop`.
fn missing_dirs(dir: &Path, stop: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dir.ancestors() {
        if dir == stop || dir.exists() {
            break;
        }
        out.push(dir.to_path_buf());
    }
    out
}

/// Remove the partial file, then only the directories the download created.
/// `remove_dir` refuses a non-empty folder, so nothing else can be lost.
fn cleanup(part: &Path, created: &[PathBuf]) {
    let _ = std::fs::remove_file(part);
    for dir in created {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Check the GGUF magic, then move the part file into place. A gated or moved repo
/// answers HTTP 200 with an HTML login page, so the first bytes must say `GGUF`.
fn finalize_part(part: &Path, target: &Path) -> Result<(), String> {
    let mut head = [0u8; 4];
    let ok = std::fs::File::open(part)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == *b"GGUF";
    if !ok {
        let _ = std::fs::remove_file(part);
        return Err(format!("{} is not a GGUF file", target.display()));
    }
    std::fs::rename(part, target).map_err(|e| format!("{e} moving {}", part.display()))
}

/// Write `<folder>\<folder>.toml` with the four keys discovery needs.
fn write_model_toml(folder: &Path, gguf: &str) -> Result<PathBuf, String> {
    let name = Path::new(gguf)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut doc = toml_edit::DocumentMut::new();
    doc["name"] = toml_edit::value(name);
    doc["gguf"] = toml_edit::value(gguf);
    doc["alias"] = toml_edit::value("local");
    doc["args"] = toml_edit::value("");
    let stem = folder.file_name().unwrap_or_default().to_string_lossy();
    let path = folder.join(format!("{stem}.toml"));
    // `write` truncates before writing, so a mid-write failure leaves a partial
    // `.toml` that would block the folder cleanup: drop it before reporting.
    std::fs::write(&path, doc.to_string()).map_err(|e| {
        let _ = std::fs::remove_file(&path);
        format!("{e} writing {}", path.display())
    })?;
    Ok(path)
}

/// Delete model folders that hold nothing but `*.gguf.part` files, at any depth.
/// Returns the folders removed. A folder with any real entry is never touched.
///
/// Startup-only: call this once before any download starts. Running it while a
/// download is in flight could delete that download's live `.part` folder.
pub fn sweep_partials(root: &Path) -> Vec<PathBuf> {
    let mut swept = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return swept;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) && only_partials(&path) {
            if std::fs::remove_dir_all(&path).is_ok() {
                swept.push(path);
            }
        }
    }
    swept
}

/// True when the tree holds at least one file and every file is a `*.gguf.part`.
fn only_partials(dir: &Path) -> bool {
    let mut files = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return false;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => {
                    if !entry
                        .file_name()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".gguf.part")
                    {
                        return false;
                    }
                    files += 1;
                }
                // A link or junction could point anywhere: leave the folder alone.
                _ => return false,
            }
        }
    }
    files > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_plain_id() {
        assert_eq!(
            parse_repo("owner/name"),
            Some(("owner/name".to_owned(), "main".to_owned()))
        );
    }

    #[test]
    fn parse_repo_url_tree_main() {
        assert_eq!(
            parse_repo("https://huggingface.co/o/n"),
            Some(("o/n".to_owned(), "main".to_owned()))
        );
        assert_eq!(
            parse_repo("https://huggingface.co/o/n/tree/main"),
            Some(("o/n".to_owned(), "main".to_owned()))
        );
    }

    #[test]
    fn parse_repo_url_tree_branch() {
        assert_eq!(
            parse_repo("https://huggingface.co/o/n/tree/dev?filename=x.gguf"),
            Some(("o/n".to_owned(), "dev".to_owned()))
        );
    }

    #[test]
    fn parse_repo_url_blob() {
        assert_eq!(
            parse_repo("https://huggingface.co/o/n/blob/main/a.gguf"),
            Some(("o/n".to_owned(), "main".to_owned()))
        );
    }

    #[test]
    fn parse_repo_url_resolve_branch() {
        assert_eq!(
            parse_repo("https://huggingface.co/o/n/resolve/dev/a.gguf"),
            Some(("o/n".to_owned(), "dev".to_owned()))
        );
    }

    #[test]
    fn parse_repo_garbage_is_none() {
        assert_eq!(parse_repo(""), None);
        assert_eq!(parse_repo("a"), None);
        assert_eq!(parse_repo("https://example.com/x"), None);
        assert_eq!(parse_repo("https://"), None);
        assert_eq!(parse_repo("https:///"), None);
    }

    #[test]
    fn tree_json_keeps_only_gguf_files() {
        let json = serde_json::json!([
            {"type": "file", "path": "a.gguf", "size": 10},
            {"type": "file", "path": "readme.md", "size": 1},
            {"type": "directory", "path": "sub"},
            {"type": "directory", "path": "x.gguf"},
            {"type": "file", "path": "no-size.gguf"},
            {"type": "file", "path": "B.GGUF", "size": 20},
        ]);
        assert_eq!(
            ggufs_from_tree(&json).unwrap(),
            vec![
                HfFile {
                    path: "a.gguf".to_owned(),
                    size: 10
                },
                HfFile {
                    path: "B.GGUF".to_owned(),
                    size: 20
                },
                HfFile {
                    path: "no-size.gguf".to_owned(),
                    size: 0
                },
            ]
        );
    }

    #[test]
    fn tree_json_no_ggufs_is_error() {
        let json = serde_json::json!([{"type": "file", "path": "readme.md", "size": 1}]);
        let err = ggufs_from_tree(&json).unwrap_err();
        assert!(err.contains("no .gguf"), "unexpected error: {err}");
    }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "quantdock_hf_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spec(repo: &str, path: &str) -> DownloadSpec {
        DownloadSpec {
            repo: repo.to_owned(),
            branch: "main".to_owned(),
            path: path.to_owned(),
            size: 1,
        }
    }

    fn wait(rx: &mpsc::Receiver<Progress>) -> Progress {
        loop {
            match rx
                .recv_timeout(Duration::from_secs(10))
                .expect("download thread ended without a verdict")
            {
                Progress::Listing | Progress::Downloading { .. } => {}
                other => return other,
            }
        }
    }

    #[test]
    fn folder_name_appends_suffix_when_folder_exists_and_has_files() {
        let root = tmp("suffix_files");
        std::fs::create_dir_all(root.join("X")).unwrap();
        std::fs::write(root.join("X").join("f.gguf"), b"x").unwrap();
        assert_eq!(folder_name(&spec("o/X", "f.gguf"), &root), "X-2");
        std::fs::create_dir_all(root.join("X-2")).unwrap();
        std::fs::write(root.join("X-2").join("f.gguf"), b"x").unwrap();
        assert_eq!(folder_name(&spec("o/X", "f.gguf"), &root), "X-3");
    }

    #[test]
    fn folder_name_suffixes_a_nested_only_folder() {
        let root = tmp("suffix_nested");
        std::fs::create_dir_all(root.join("X").join("sub")).unwrap();
        std::fs::write(root.join("X").join("sub").join("x.gguf"), b"x").unwrap();
        assert_eq!(folder_name(&spec("o/X", "sub/x.gguf"), &root), "X-2");
    }

    #[test]
    fn folder_name_reuses_empty_folder() {
        let root = tmp("reuse_empty");
        std::fs::create_dir_all(root.join("X")).unwrap();
        assert_eq!(folder_name(&spec("o/X", "f.gguf"), &root), "X");
        assert_eq!(folder_name(&spec("o/X", "f.gguf"), &root.join("missing")), "X");
    }

    #[test]
    fn folder_name_suffixes_old() {
        let root = tmp("suffix_old");
        assert_eq!(folder_name(&spec("o/old", "f.gguf"), &root), "old-2");
        assert_eq!(folder_name(&spec("o/Old", "f.gguf"), &root), "Old-2");
        std::fs::create_dir_all(root.join("old-2")).unwrap();
        std::fs::write(root.join("old-2").join("f.gguf"), b"x").unwrap();
        assert_eq!(folder_name(&spec("o/old", "f.gguf"), &root), "old-3");
    }

    #[test]
    fn write_toml_has_the_four_keys() {
        let root = tmp("toml");
        let folder = root.join("X");
        std::fs::create_dir_all(&folder).unwrap();
        let toml = write_model_toml(&folder, "a.gguf").unwrap();
        assert_eq!(toml, folder.join("X.toml"));
        let cfg: crate::model::ModelConfig =
            toml_edit::de::from_str(&std::fs::read_to_string(&toml).unwrap()).unwrap();
        assert_eq!(cfg.name, "a");
        assert_eq!(cfg.gguf, "a.gguf");
        assert_eq!(cfg.alias, "local");
        assert_eq!(cfg.args, "");
    }

    #[test]
    fn safe_rel_rejects_escapes() {
        for bad in ["../x", "/abs", "", r"..\x", r"C:\x"] {
            assert!(safe_rel(bad).is_err(), "accepted unsafe path {bad:?}");
        }
        assert_eq!(safe_rel("sub/a.gguf").unwrap(), PathBuf::from("sub/a.gguf"));
    }

    #[test]
    fn gguf_magic_check_rejects_html() {
        let root = tmp("magic_html");
        let part = root.join("a.gguf.part");
        let target = root.join("a.gguf");
        std::fs::write(&part, b"<html>login</html>").unwrap();
        let err = finalize_part(&part, &target).unwrap_err();
        assert!(err.contains("GGUF"), "unexpected error: {err}");
        assert!(!part.exists(), ".part survived the magic check");
        assert!(!target.exists(), "rejected body was renamed into place");
    }

    #[test]
    fn finalize_part_accepts_gguf() {
        let root = tmp("magic_ok");
        let part = root.join("a.gguf.part");
        let target = root.join("a.gguf");
        std::fs::write(&part, b"GGUF\x03\x00\x00\x00").unwrap();
        finalize_part(&part, &target).unwrap();
        assert!(target.exists());
        assert!(!part.exists());
    }

    #[test]
    fn cancel_cleans_the_part_file() {
        let root = tmp("cancel_early");
        let cancel = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel();
        start_download(spec("o/X", "a.gguf"), &root, cancel, tx);
        assert!(matches!(wait(&rx), Progress::Cancelled));
        assert!(!root.join("X").exists(), "created folder left behind");
    }

    #[test]
    fn cancel_removes_created_dirs_bottom_up() {
        let root = tmp("cancel_nested");
        let cancel = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel();
        start_download(spec("o/X", "sub/a.gguf"), &root, cancel, tx);
        assert!(matches!(wait(&rx), Progress::Cancelled));
        assert!(!root.join("X").exists(), "created folder left behind");

        // A pre-existing empty folder is reused and must survive the cleanup.
        let reused = tmp("cancel_nested_reused");
        std::fs::create_dir_all(reused.join("X")).unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel();
        start_download(spec("o/X", "sub/a.gguf"), &reused, cancel, tx);
        assert!(matches!(wait(&rx), Progress::Cancelled));
        assert!(reused.join("X").exists(), "pre-existing folder was removed");
        assert!(!reused.join("X").join("sub").exists(), "created subfolder left behind");

        // The model root is the caller's anchor: a pre-set cancel creates nothing at
        // all, so cleanup never has to remove it.
        let missing_root = tmp("cancel_rootless").join("models");
        let (tx, rx) = mpsc::channel();
        start_download(
            spec("o/X", "sub/a.gguf"),
            &missing_root,
            Arc::new(AtomicBool::new(true)),
            tx,
        );
        assert!(matches!(wait(&rx), Progress::Cancelled));
        assert!(!missing_root.exists(), "cancel touched the filesystem");
    }

    #[test]
    fn sweep_deletes_only_part_only_folders() {
        let root = tmp("sweep");
        std::fs::create_dir_all(root.join("folder1")).unwrap();
        std::fs::write(root.join("folder1").join("a.gguf.part"), b"x").unwrap();
        std::fs::create_dir_all(root.join("folder1b").join("sub")).unwrap();
        std::fs::write(root.join("folder1b").join("sub").join("b.gguf.part"), b"x").unwrap();
        std::fs::create_dir_all(root.join("folder2")).unwrap();
        std::fs::write(root.join("folder2").join("b.gguf.part"), b"x").unwrap();
        std::fs::write(root.join("folder2").join("c.toml"), b"x").unwrap();
        std::fs::create_dir_all(root.join("folder3")).unwrap();

        let swept = sweep_partials(&root);

        assert_eq!(swept.len(), 2, "unexpected sweep: {swept:?}");
        assert!(!root.join("folder1").exists(), "part-only folder kept");
        assert!(!root.join("folder1b").exists(), "nested part-only folder kept");
        assert!(root.join("folder2").join("c.toml").exists(), "real folder touched");
        assert!(root.join("folder3").exists(), "empty folder touched");
    }
}
