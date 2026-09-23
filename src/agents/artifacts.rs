//! Explicit HTML deliverables, kept as paths in the session workspace.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const TOOL: &str = "difu_present_artifact";
pub const INSTRUCTIONS: &str = "Prefer self-contained HTML rather than Markdown for reports, visual explanations, and other artifacts intended for viewing. After creating an HTML artifact, call difu_present_artifact with its workspace-relative path and a short title. Register deliverables only, not ordinary application HTML source files. The user can open the artifact in difu or a browser and refresh manually after changes.";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub title: String,
}
pub fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Present a completed local HTML deliverable in difu. The file must already exist inside the current workspace.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"title":{"type":"string"}},"required":["path","title"],"additionalProperties":false}})
}
pub fn resolve(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let file = root
        .join(path)
        .canonicalize()
        .context("Artifact file does not exist")?;
    ensure!(
        file.starts_with(&root),
        "Artifact must be inside this session workspace"
    );
    ensure!(file.is_file(), "Artifact must be a regular file");
    ensure!(
        file.extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("html") || x.eq_ignore_ascii_case("htm")),
        "Artifact must be an HTML file"
    );
    Ok(file)
}
pub fn register(session: &mut super::Session, args: &Value) -> Result<()> {
    let root = session
        .workspace
        .as_deref()
        .context("No session workspace")?
        .canonicalize()?;
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .context("Missing artifact path")?;
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .context("Missing artifact title")?;
    ensure!(!title.trim().is_empty(), "Artifact title cannot be empty");
    let path = resolve(&root, Path::new(path))?
        .strip_prefix(&root)?
        .to_owned();
    let artifact = Artifact {
        path: path.clone(),
        title: crate::model::clean(title).chars().take(160).collect(),
    };
    if let Some(existing) = session.artifacts.iter_mut().find(|a| a.path == path) {
        *existing = artifact;
    } else {
        session.artifacts.push(artifact);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_html_and_rejects_escaping_symlinks() -> Result<()> {
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::write(root.path().join("report.html"), "<h1>Report</h1>")?;
        std::fs::write(outside.path().join("secret.html"), "private")?;
        std::os::unix::fs::symlink(
            outside.path().join("secret.html"),
            root.path().join("escape.html"),
        )?;
        assert!(resolve(root.path(), Path::new("report.html")).is_ok());
        assert!(resolve(root.path(), Path::new("escape.html")).is_err());
        assert!(resolve(root.path(), Path::new("missing.html")).is_err());
        Ok(())
    }
}
