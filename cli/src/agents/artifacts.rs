//! Explicit HTML deliverables, kept as paths in the session workspace. A
//! server session's artifact is downloaded into this computer's cache to be
//! previewed (`Request::ReadArtifact`).
use crate::storage::Storage;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// The largest artifact a server sends to another computer: its base64 reply
/// stays well inside the WebSocket message cap.
pub const DOWNLOAD_LIMIT: u64 = 32 << 20;
/// Downloaded copies of server sessions' artifacts, under the cache.
const COPIES: &str = "server-artifacts";

pub const TOOL: &str = "difu_present_artifact";
pub const INSTRUCTIONS: &str = "Prefer self-contained HTML rather than Markdown for reports, visual explanations, and other artifacts intended for viewing. After creating an HTML artifact, call difu_present_artifact with its workspace-relative path and a short title. Register deliverables only, not ordinary application HTML source files. The user can open the artifact in difu or a browser and refresh manually after changes.";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub title: String,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
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
        workspace: Some(root.clone()),
        title: crate::model::clean(title).chars().take(160).collect(),
    };
    if let Some(existing) = session
        .artifacts
        .iter_mut()
        .find(|a| a.path == path && a.workspace.as_ref().is_none_or(|w| w == &root))
    {
        *existing = artifact;
    } else {
        session.artifacts.push(artifact);
    }
    Ok(())
}
/// A registered artifact's contents, for a terminal app on another computer.
/// Only presented artifacts can be read, with the same checks as opening one.
pub fn read(session: &super::Session, path: &Path, workspace: Option<&Path>) -> Result<Vec<u8>> {
    let artifact = session
        .artifacts
        .iter()
        .find(|artifact| artifact.path == path && artifact.workspace.as_deref() == workspace)
        .context("That artifact is no longer presented in this session")?;
    let file = resolve(
        artifact
            .workspace
            .as_deref()
            .or(session.workspace.as_deref())
            .context("No workspace")?,
        &artifact.path,
    )?;
    let size = fs::metadata(&file)?.len();
    ensure!(
        size <= DOWNLOAD_LIMIT,
        "{} is {:.1} MB; artifacts open from a server up to {} MB",
        artifact.path.display(),
        size as f64 / f64::from(1u32 << 20),
        DOWNLOAD_LIMIT >> 20
    );
    Ok(fs::read(file)?)
}

fn copies(storage: &Storage, session: &str) -> Result<PathBuf> {
    ensure!(
        crate::cloud::federation::split(session).is_some(),
        "Only server sessions' artifacts are downloaded"
    );
    Ok(storage
        .cache
        .join(COPIES)
        .join(super::media::directory_name(session)?))
}

/// Where this computer keeps its copy of a server session's artifact: per
/// session and artifact, under the artifact's own file name.
pub fn local_copy(storage: &Storage, session: &str, artifact: &Artifact) -> Result<PathBuf> {
    let name = artifact
        .path
        .file_name()
        .context("Artifact has no file name")?;
    let key = crate::storage::hash(format!(
        "{}\0{}",
        artifact
            .workspace
            .as_deref()
            .unwrap_or(Path::new(""))
            .display(),
        artifact.path.display()
    ));
    Ok(copies(storage, session)?.join(key).join(name))
}

/// Replace the local copy of a server session's artifact with `data` (base64,
/// as `Reply::ArtifactFile` carries it).
pub fn save_copy(
    storage: &Storage,
    session: &str,
    artifact: &Artifact,
    data: &str,
) -> Result<PathBuf> {
    let bytes = STANDARD
        .decode(data)
        .context("The server sent an invalid artifact")?;
    let path = local_copy(storage, session, artifact)?;
    let directory = path.parent().context("Missing artifact directory")?;
    fs::create_dir_all(directory)?;
    let root = storage.cache.join(COPIES);
    for ancestor in directory.ancestors().take_while(|a| a.starts_with(&root)) {
        fs::set_permissions(ancestor, fs::Permissions::from_mode(0o700))?;
    }
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&bytes)?;
    file.as_file_mut().sync_all()?;
    file.persist(&path)?;
    Ok(path)
}

/// Download a server session's artifact into its local copy, so the artifact
/// pane and browsers open it like a local session's. Each open and refresh
/// downloads again, so the copy follows changes on the server.
pub fn download(storage: &Storage, session: &str, artifact: &Artifact) -> Result<PathBuf> {
    let reply = super::client::request(
        storage,
        super::Request::ReadArtifact {
            id: session.to_owned(),
            path: artifact.path.clone(),
            workspace: artifact.workspace.clone(),
        },
    )?;
    let super::Reply::ArtifactFile(data) = reply else {
        anyhow::bail!("Unexpected reply while downloading the artifact");
    };
    save_copy(storage, session, artifact, &data)
}

/// Remove a deleted server session's downloaded artifacts.
pub fn forget_copies(storage: &Storage, session: &str) -> Result<()> {
    match fs::remove_dir_all(copies(storage, session)?) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
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

    #[test]
    fn only_presented_artifacts_are_read_and_large_ones_are_refused() -> Result<()> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().canonicalize()?;
        std::fs::write(workspace.join("report.html"), "<h1>Report</h1>")?;
        std::fs::write(workspace.join("other.html"), "not presented")?;
        let mut session = super::super::Session::new(
            "1791-abc".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: workspace.clone(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.workspace = Some(workspace.clone());
        register(
            &mut session,
            &json!({"path": "report.html", "title": "Report"}),
        )?;
        let presented = session.artifacts.first().context("artifact")?.clone();
        let read_back =
            |path: &str| read(&session, Path::new(path), presented.workspace.as_deref());
        assert_eq!(read_back("report.html")?, b"<h1>Report</h1>");
        assert!(read_back("other.html").is_err());
        assert!(
            read(
                &session,
                Path::new("report.html"),
                Some(Path::new("/elsewhere"))
            )
            .is_err()
        );
        std::fs::File::options()
            .write(true)
            .open(workspace.join("report.html"))?
            .set_len(DOWNLOAD_LIMIT + 1)?;
        let error = read_back("report.html").err().context("too large")?;
        assert!(format!("{error:#}").contains("up to 32 MB"), "{error:#}");
        Ok(())
    }

    #[test]
    fn server_artifact_copies_are_private_replaced_and_forgotten() -> Result<()> {
        let root = tempfile::tempdir()?;
        let storage = Storage {
            config: root.path().join("config.json"),
            cache: root.path().join("cache"),
        };
        let artifact = Artifact {
            path: "out/report.html".into(),
            title: "Report".into(),
            workspace: Some("/srv/difu/agents/worktrees/1".into()),
        };
        let copy = save_copy(&storage, "box:1", &artifact, &STANDARD.encode("first"))?;
        assert_eq!(copy, local_copy(&storage, "box:1", &artifact)?);
        assert!(copy.starts_with(storage.cache.join("server-artifacts/box@1")));
        assert_eq!(
            copy.file_name().and_then(|n| n.to_str()),
            Some("report.html")
        );
        assert_eq!(fs::read_to_string(&copy)?, "first");
        assert_eq!(fs::metadata(&copy)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(storage.cache.join("server-artifacts/box@1"))?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        save_copy(&storage, "box:1", &artifact, &STANDARD.encode("second"))?;
        assert_eq!(fs::read_to_string(&copy)?, "second");
        // The same file name in another workspace keeps its own copy.
        let elsewhere = Artifact {
            workspace: Some("/srv/difu/agents/worktrees/2".into()),
            ..artifact.clone()
        };
        assert_ne!(local_copy(&storage, "box:1", &elsewhere)?, copy);
        // Local sessions and unsafe ids have no downloaded copies.
        for session in ["1791-abc", "box:../x", "box:"] {
            assert!(
                local_copy(&storage, session, &artifact).is_err(),
                "{session}"
            );
        }
        assert!(save_copy(&storage, "box:1", &artifact, "not base64!").is_err());
        forget_copies(&storage, "box:1")?;
        assert!(!copy.exists());
        forget_copies(&storage, "box:1")?;
        Ok(())
    }
}
