use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PORT: u16 = 8080;

/// The per-model config file, e.g. `C:\aiModels\Qwen3.8-27B-R3-mix\Qwen3.8-27B-R3-mix.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    /// GGUF filename, relative to the model's own folder.
    pub gguf: String,
    /// Served model id. Shared by a group of models -> one OpenCode entry.
    pub alias: String,
    /// Raw llama-server flags, single line. The launcher adds -m / -a / --port.
    #[serde(default)]
    pub args: String,
    /// Optional per-model llama-server exe (absolute path). Absent -> default chain.
    #[serde(default)]
    pub server: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub cfg: ModelConfig,
    pub folder: PathBuf,
    pub toml_path: PathBuf,
    pub gguf_path: PathBuf,
    /// GGUF size, read once at discovery so drawing the list costs no syscalls.
    pub size: u64,
}

impl Model {
    pub fn size_bytes(&self) -> u64 {
        self.size
    }

    /// Exact llama-server command line, matching the flags documented in `INSTALL.md`.
    pub fn server_args(&self) -> Vec<String> {
        let mut v = vec![
            "-m".to_owned(),
            self.gguf_path.display().to_string(),
            "-a".to_owned(),
            self.cfg.alias.clone(),
        ];
        v.extend(self.cfg.args.split_whitespace().map(str::to_owned));
        v.push("--port".to_owned());
        v.push(PORT.to_string());
        v
    }

    /// Write one key back to the .toml, preserving comments and layout.
    pub fn set_key(&self, key: &str, value: &str) -> Result<(), String> {
        let text = std::fs::read_to_string(&self.toml_path).map_err(|e| e.to_string())?;
        let mut doc: toml_edit::DocumentMut =
            text.parse::<toml_edit::DocumentMut>().map_err(|e| e.to_string())?;
        doc[key] = toml_edit::value(value);
        std::fs::write(&self.toml_path, doc.to_string()).map_err(|e| e.to_string())
    }
}

/// Scan `<root>\<Model Folder>\<Model Folder>.toml` for every model.
pub fn discover(root: &Path) -> Vec<Model> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else {
        return out;
    };
    for dir in dirs.flatten() {
        if !dir.path().is_dir() {
            continue;
        }
        let folder = dir.path();
        let folder_name = folder.file_name().unwrap_or_default().to_string_lossy().to_string();
        // `old` holds the legacy launchers, not models.
        if folder_name.eq_ignore_ascii_case("old") {
            continue;
        }
        let toml_path = folder.join(format!("{folder_name}.toml"));
        let Ok(text) = std::fs::read_to_string(&toml_path) else {
            continue;
        };
        let Ok(cfg) = toml_edit::de::from_str::<ModelConfig>(&text) else {
            continue;
        };
        let gguf_path = folder.join(&cfg.gguf);
        let size = std::fs::metadata(&gguf_path).map(|m| m.len()).unwrap_or(0);
        out.push(Model {
            cfg,
            folder,
            toml_path,
            gguf_path,
            size,
        });
    }
    out.sort_by(|a, b| a.cfg.name.cmp(&b.cfg.name));
    out
}

/// The model the server is actually serving: matched by GGUF file name when the
/// server tells us, otherwise by alias.
pub fn loaded<'a>(models: &'a [Model], st: &crate::server::Status) -> Option<&'a Model> {
    if let Some(p) = &st.model_path {
        let want = std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase());
        if let Some(want) = want {
            let hit = models.iter().find(|m| {
                m.gguf_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .is_some_and(|n| n == want)
            });
            if hit.is_some() {
                return hit;
            }
        }
    }
    models.iter().find(|m| m.cfg.alias == st.served_id)
}

pub fn models_root() -> PathBuf {
    std::env::var_os("QUANTDOCK_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\aiModels"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_model(contents: &str) -> (PathBuf, Model) {
        let dir = std::env::temp_dir().join(format!(
            "quantdock_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let toml_path = dir.join("m.toml");
        std::fs::write(&toml_path, contents).unwrap();
        let cfg: ModelConfig = toml_edit::de::from_str(contents).unwrap();
        let m = Model {
            cfg,
            folder: dir.clone(),
            toml_path: toml_path.clone(),
            gguf_path: dir.join("m.gguf"),
            size: 0,
        };
        (dir, m)
    }

    #[test]
    fn set_key_keeps_comments_and_other_keys() {
        let (_dir, m) = tmp_model(
            "# keep me\nname = \"m\"\ngguf = \"m.gguf\"\nalias = \"local\"\nserver = \"C:/aiModels/_runtime/unsloth-b12588/llama-server.exe\"\n\n# args comment\nargs = \"-a 1\"\n",
        );
        m.set_key("args", "-b 2").unwrap();
        let out = std::fs::read_to_string(&m.toml_path).unwrap();
        assert!(out.contains("# keep me"), "top comment lost");
        assert!(out.contains("# args comment"), "inline comment lost");
        assert!(out.contains("-b 2"), "new value missing");
        assert!(!out.contains("-a 1"), "old value still present");
        assert!(out.contains("alias = \"local\""), "other key changed");
        assert!(out.contains("server = \"C:/aiModels/_runtime/unsloth-b12588/llama-server.exe\""), "server key lost");
    }

    #[test]
    fn server_key_deserializes() {
        let (_dir, m) = tmp_model(
            "name = \"m\"\ngguf = \"m.gguf\"\nalias = \"local\"\nserver = \"C:/aiModels/_runtime/unsloth-b12588/llama-server.exe\"\n",
        );
        assert_eq!(
            m.cfg.server.as_deref(),
            Some("C:/aiModels/_runtime/unsloth-b12588/llama-server.exe")
        );
    }

    #[test]
    fn server_args_match_the_documented_flags() {
        let (_dir, m) = tmp_model("name = \"m\"\ngguf = \"m.gguf\"\nalias = \"local\"\nargs = \"-c 1024 --temp 0.7\"\n");
        assert_eq!(
            m.server_args(),
            vec![
                "-m",
                &m.gguf_path.display().to_string(),
                "-a",
                "local",
                "-c",
                "1024",
                "--temp",
                "0.7",
                "--port",
                "8080",
            ]
        );
    }

    #[test]
    fn discover_finds_models_and_skips_old() {
        let dir = std::env::temp_dir().join(format!(
            "quantdock_disc_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("Model A")).unwrap();
        std::fs::create_dir_all(dir.join("old")).unwrap();
        std::fs::write(
            dir.join("Model A").join("Model A.toml"),
            "name = \"Model A\"\ngguf = \"a.gguf\"\nalias = \"local\"\nargs = \"\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("old").join("old.toml"),
            "name = \"old\"\ngguf = \"o.gguf\"\nalias = \"local\"\nargs = \"\"\n",
        )
        .unwrap();

        let found = discover(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cfg.name, "Model A");
        assert_eq!(found[0].cfg.alias, "local");
    }
}
