//! HuggingFace GGUF download support: repo/URL parsing and tree listing.

use std::sync::OnceLock;
use std::time::Duration;

const API: &str = "https://huggingface.co/api/models";

/// One GGUF in a repo's file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfFile {
    pub path: String,
    pub size: u64,
}

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
    files.sort_by_key(|f| f.path.to_ascii_lowercase());
    if files.is_empty() {
        return Err("no .gguf files".to_owned());
    }
    Ok(files)
}

/// List the GGUFs in `repo` at `branch`, recursively.
pub fn list_ggufs(repo: &str, branch: &str) -> Result<Vec<HfFile>, String> {
    let url = format!("{API}/{repo}/tree/{branch}?recursive=true");
    let mut resp = agent().get(&url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code} from {url}"),
        other => other.to_string(),
    })?;
    let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    ggufs_from_tree(&json).map_err(|e| format!("{e} in {repo}"))
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
            ]
        );
    }

    #[test]
    fn tree_json_no_ggufs_is_error() {
        let json = serde_json::json!([{"type": "file", "path": "readme.md", "size": 1}]);
        let err = ggufs_from_tree(&json).unwrap_err();
        assert!(err.contains("no .gguf"), "unexpected error: {err}");
    }
}
