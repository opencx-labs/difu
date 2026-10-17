//! Explicitly pasted media: private session copies, never uploads before Send.
use crate::storage::Storage;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Image,
    Video,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub label: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub hash: String,
}
impl Attachment {
    pub fn token(&self) -> String {
        format!("[{}]", self.label)
    }
}
pub enum Paste {
    Text(String),
    Attachments(Vec<Attachment>),
}
fn kind(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" => Some(Kind::Image),
        "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" => Some(Kind::Video),
        _ => None,
    }
}
pub fn directory(storage: &Storage, session: &str) -> Result<PathBuf> {
    ensure!(
        !session.is_empty()
            && session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "Invalid attachment session"
    );
    let root = super::server::home(storage)?.join("attachments");
    let dir = root.join(session);
    for path in [&root, &dir] {
        fs::create_dir_all(path)?;
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "Attachment directory is a symlink"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}
fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(buffer.get(..count).context("Invalid read size")?);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn persist(storage: &Storage, session: &str, source: &Path, kind: Kind) -> Result<Attachment> {
    let input = fs::File::open(source).context("Cannot open attachment")?;
    ensure!(
        input.metadata()?.is_file(),
        "Attachment must be a regular file"
    );
    let original = input.metadata()?;
    let extension = source.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let mut copy = tempfile::Builder::new()
        .prefix("media-")
        .suffix(&format!(".{extension}"))
        .tempfile_in(directory(storage, session)?)?;
    let mut input = input;
    std::io::copy(&mut input, copy.as_file_mut())?;
    let current = input.metadata()?;
    ensure!(
        original.len() == current.len() && original.modified()? == current.modified()?,
        "Attachment changed while copying; paste it again"
    );
    copy.as_file_mut().sync_all()?;
    if kind == Kind::Image {
        let reader = image::ImageReader::open(copy.path())?.with_guessed_format()?;
        ensure!(
            matches!(
                reader.format(),
                Some(
                    image::ImageFormat::Png
                        | image::ImageFormat::Jpeg
                        | image::ImageFormat::WebP
                        | image::ImageFormat::Gif
                )
            ),
            "Unsupported image format; use PNG, JPEG, WebP or GIF"
        );
        let (w, h) = reader
            .into_dimensions()
            .context("Cannot read attachment image")?;
        ensure!(w > 0 && h > 0, "Image is empty");
    }
    let digest = hash(copy.path())?;
    let (_, path) = copy.keep()?;
    Ok(Attachment {
        label: String::new(),
        path,
        kind,
        hash: digest,
    })
}
pub fn validate(storage: &Storage, session: &str, attachment: &Attachment) -> Result<()> {
    let dir = directory(storage, session)?.canonicalize()?;
    let path = attachment
        .path
        .canonicalize()
        .context("Attachment is missing; paste it again")?;
    ensure!(
        path.parent() == Some(dir.as_path()) && path.is_file(),
        "Attachment does not belong to this session"
    );
    ensure!(
        hash(&path)? == attachment.hash,
        "Attachment changed; paste it again"
    );
    Ok(())
}
/// Parse terminal drop quoting without ever executing a shell.
pub fn paths(text: &str, cwd: &Path) -> Option<Vec<PathBuf>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let path = |value: &str| -> PathBuf {
        if let Ok(url) = url::Url::parse(value)
            && url.scheme() == "file"
            && let Ok(path) = url.to_file_path()
        {
            return path;
        }
        if let Some(rest) = value.strip_prefix("~/")
            && let Some(home) = dirs::home_dir()
        {
            return home.join(rest);
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let direct = path(text);
    if direct.is_file() && kind(&direct).is_some() {
        return Some(vec![direct]);
    }
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escape = false;
    for ch in text.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escape = true;
            continue;
        }
        if let Some(end) = quote {
            if ch == end {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if escape || quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    let paths = tokens.into_iter().map(|s| path(&s)).collect::<Vec<_>>();
    (!paths.is_empty() && paths.iter().all(|p| p.is_file() && kind(p).is_some())).then_some(paths)
}
pub fn files(storage: &Storage, session: &str, paths: Vec<PathBuf>) -> Result<Paste> {
    let mut attachments = Vec::new();
    for path in paths {
        let kind = kind(&path).context("Unsupported attachment; choose an image or video file")?;
        attachments.push(persist(storage, session, &path, kind)?);
    }
    Ok(Paste::Attachments(attachments))
}
pub fn clipboard(storage: &Storage, session: &str, cwd: &Path) -> Result<Paste> {
    let mut clipboard = arboard::Clipboard::new().context("Cannot access the desktop clipboard")?;
    if let Ok(paths) = clipboard.get().file_list()
        && !paths.is_empty()
    {
        return files(storage, session, paths);
    }
    if let Ok(pixels) = clipboard.get_image() {
        let width = u32::try_from(pixels.width)?;
        let height = u32::try_from(pixels.height)?;
        let image = image::RgbaImage::from_raw(width, height, pixels.bytes.into_owned())
            .context("Clipboard returned invalid image pixels")?;
        let mut file = tempfile::Builder::new()
            .prefix("clipboard-")
            .suffix(".png")
            .tempfile_in(directory(storage, session)?)?;
        image::DynamicImage::ImageRgba8(image)
            .write_to(file.as_file_mut(), image::ImageFormat::Png)?;
        file.flush()?;
        let digest = hash(file.path())?;
        let (_, path) = file.keep()?;
        return Ok(Paste::Attachments(vec![Attachment {
            label: String::new(),
            path,
            kind: Kind::Image,
            hash: digest,
        }]));
    }
    let text = clipboard
        .get_text()
        .context("Clipboard contains no supported image, file path, or text")?;
    if let Some(paths) = paths(&text, cwd) {
        files(storage, session, paths)
    } else {
        Ok(Paste::Text(text))
    }
}
pub fn cleanup(storage: &Storage, session: &str) -> Result<()> {
    let directory = directory(storage, session)?;
    fs::remove_dir_all(directory).context("Could not remove session attachments")
}

pub fn load_draft(storage: &Storage, session: &str) -> Result<Vec<Attachment>> {
    let path = directory(storage, session)?.join("draft.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub fn save_draft(storage: &Storage, session: &str, attachments: &[Attachment]) -> Result<()> {
    crate::storage::atomic_json(
        &directory(storage, session)?.join("draft.json"),
        &attachments,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copied_media_survives_original_changes_and_remains_private() -> Result<()> {
        let root = tempfile::tempdir()?;
        let storage = Storage {
            config: root.path().join("config.json"),
            cache: root.path().join("cache"),
        };
        let source = root.path().join("my image.png");
        image::RgbaImage::new(2, 2).save(&source)?;
        let parsed =
            paths(&format!("'{}'", source.display()), root.path()).context("quoted path")?;
        let Paste::Attachments(mut copies) = files(&storage, "session-1", parsed)? else {
            anyhow::bail!("expected media");
        };
        let a = copies.first_mut().context("copy")?;
        a.label = "image 1".into();
        assert_eq!(fs::metadata(&a.path)?.permissions().mode() & 0o777, 0o600);
        fs::write(&source, b"changed original")?;
        validate(&storage, "session-1", a)?;
        assert!(validate(&storage, "session-2", a).is_err());
        save_draft(&storage, "session-1", &copies)?;
        assert_eq!(load_draft(&storage, "session-1")?, copies);
        let a = copies.first().context("copy")?;
        fs::write(&a.path, b"changed stored attachment")?;
        assert!(validate(&storage, "session-1", a).is_err());
        assert!(paths("some prose about image.png", root.path()).is_none());
        assert!(directory(&storage, "../outside").is_err());
        Ok(())
    }
}
