use crate::model::{Model, PORT};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Model id the server advertises (the alias).
    pub served_id: String,
    /// GGUF the server actually loaded, if it will tell us.
    pub model_path: Option<String>,
    /// Context size the server is actually using.
    pub ctx: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
struct Props {
    #[serde(default)]
    model_path: String,
    #[serde(default)]
    default_generation_settings: GenSettings,
}

#[derive(Deserialize, Default, Clone)]
struct GenSettings {
    #[serde(default)]
    n_ctx: Option<u32>,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// One shared agent (connection pooling) with timeouts short enough that a wedged
/// server cannot stall the UI for long. The status call runs on the UI thread.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_millis(1200)))
            .timeout_connect(Some(Duration::from_millis(400)))
            .build()
            .new_agent()
    })
}

fn get_json<T: serde::de::DeserializeOwned>(path: &str) -> Option<T> {
    let url = format!("http://127.0.0.1:{PORT}{path}");
    let mut resp = agent().get(&url).call().ok()?;
    resp.body_mut().read_json().ok()
}

/// `/props` carries the whole chat template, so it is cached: it only changes when a
/// different model loads. Polling it every tick was what made the process grow.
fn props_cached() -> Option<Props> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, Option<Props>)>> = Mutex::new(None);
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some((at, cached)) = &*guard {
        if at.elapsed() < Duration::from_secs(10) {
            return cached.clone();
        }
    }
    let fresh: Option<Props> = get_json("/props");
    *guard = Some((Instant::now(), fresh.clone()));
    fresh
}

/// What the server is currently serving. `None` = nothing listening.
pub fn status() -> Option<Status> {
    let models: ModelsResponse = get_json("/v1/models")?;
    let served_id = models.data.first()?.id.clone();

    // /props gives the real GGUF path and context size, so the UI can show what is
    // *actually* loaded rather than trusting the alias.
    // A typed struct instead of a Value tree: /props carries the whole chat template,
    // and building a tree of it every poll was pure churn.
    let props = props_cached();
    let model_path = props
        .as_ref()
        .map(|p| p.model_path.clone())
        .filter(|p| !p.is_empty());
    let ctx = props
        .as_ref()
        .and_then(|p| p.default_generation_settings.n_ctx);

    Some(Status {
        served_id,
        model_path,
        ctx,
    })
}

pub fn is_running() -> bool {
    status().is_some()
}

/// Kill any running llama-server.
pub fn unload() {
    let _ = crate::hidden(&mut std::process::Command::new("taskkill"))
        .args(["/F", "/IM", "llama-server.exe"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Locate llama-server.exe: env override, then the LM Studio backend, then PATH.
pub fn find_llama_server() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("QUANTDOCK_LLAMA") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }

    // C:\Users\<user>\.lmstudio\extensions\backends\llama.cpp-<ver>\llama-server.exe
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let backends = PathBuf::from(home)
            .join(".lmstudio")
            .join("extensions")
            .join("backends");
        if let Ok(entries) = std::fs::read_dir(&backends) {
            let mut best: Option<(Vec<u32>, PathBuf)> = None;
            for e in entries.flatten() {
                let dir = e.path();
                let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(version) = name.strip_prefix("llama.cpp-") else {
                    continue;
                };
                let exe = dir.join("llama-server.exe");
                if !exe.is_file() {
                    continue;
                }
                let key: Vec<u32> = version
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse().unwrap_or(0))
                    .collect();
                if best.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                    best = Some((key, exe));
                }
            }
            if let Some((_, exe)) = best {
                return Some(exe);
            }
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let candidate = PathBuf::from(dir).join("llama-server.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Start llama-server with the model's own args, replacing any running server.
pub fn load(model: &Model) -> Result<(), String> {
    let exe = find_llama_server().ok_or("llama-server.exe not found")?;
    if !model.gguf_path.is_file() {
        return Err(format!("GGUF missing: {}", model.gguf_path.display()));
    }

    unload();
    std::thread::sleep(Duration::from_secs(2));

    // llama-server refuses to start on a bad flag or an OOM. Keep its output so a
    // failure is reportable instead of looking like a successful launch.
    let log_path = model.folder.join("llama-server.log");
    let log = std::fs::File::create(&log_path).map_err(|e| format!("log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("log: {e}"))?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(model.server_args())
        .current_dir(&model.folder)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;

    // A rejected flag kills it within a second; give it a moment to prove it survives.
    std::thread::sleep(Duration::from_millis(1500));
    if let Ok(Some(code)) = child.try_wait() {
        return Err(match last_error_line(&log_path) {
            Some(line) => format!("llama-server failed: {line}"),
            None => format!("llama-server exited ({code}) — see {}", log_path.display()),
        });
    }
    Ok(())
}

/// Last error-ish line from the server log, for surfacing in the UI.
fn last_error_line(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .find(|l| l.to_ascii_lowercase().contains("error"))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().to_owned())
}
