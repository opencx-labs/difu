//! PR image discovery and bounded downloads. No repository code is executed.
use crate::{
    model::{PrDetail, PrKey},
    process::{self, Cancel},
    storage::Storage,
};
use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::{
    fs,
    io::{Cursor, Read, Write},
    net::IpAddr,
    ops::Range,
    path::Path,
    process::Command,
    time::Duration,
};
use ureq::unversioned::{
    resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
    transport::{DefaultConnector, NextTimeout},
};
use url::Url;

const MAX_BYTES: usize = 10 * 1024 * 1024;
const CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Request {
    pub source: String,
    pub label: String,
    pub key: PrKey,
    pub head: String,
}
impl Request {
    pub fn new(source: String, label: String, pr: &PrDetail) -> Self {
        Self {
            source,
            label,
            key: pr.key.clone(),
            head: pr.head.clone(),
        }
    }
    pub fn browser_url(&self) -> Result<Url> {
        if let Ok(url) = Url::parse(&self.source) {
            ensure!(
                matches!(url.scheme(), "http" | "https"),
                "Unsupported image URL scheme"
            );
            return Ok(url);
        }
        if self.source.starts_with('/') {
            return Url::parse("https://github.com/")?
                .join(&self.source)
                .context("Invalid image URL");
        }
        self.key.validate()?;
        let path = Url::parse("https://repository.invalid/")?.join(&self.source)?;
        let mut url = Url::parse("https://github.com/")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid image URL"))?
            .extend([
                self.key.owner.as_str(),
                self.key.repo.as_str(),
                "blob",
                self.head.as_str(),
            ]);
        let prefix = url.as_str().to_owned();
        Url::parse(&format!("{prefix}{}", path.path())).context("Invalid repository image URL")
    }
    pub fn download_url(&self) -> Result<Url> {
        let mut url = self.browser_url()?;
        if url.host_str() == Some("github.com") {
            let path = url.path().replacen("/blob/", "/raw/", 1);
            url.set_path(&path);
        }
        validate_url(&url)?;
        Ok(url)
    }
    pub fn id(&self) -> String {
        crate::storage::hash(format!("{}:{}:{}", self.key.id(), self.head, self.source))
    }
}

#[derive(Debug)]
pub struct Reference {
    pub range: Range<usize>,
    pub source: String,
    pub label: String,
}

/// Parse image syntax rather than mistaking examples inside code blocks for images.
pub fn references(source: &str) -> Vec<Reference> {
    let mut images = Vec::new();
    let mut current: Option<Reference> = None;
    for (event, range) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                current = Some(Reference {
                    range,
                    source: dest_url.into_string(),
                    label: String::new(),
                });
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                if let Some(image) = &mut current {
                    image.label.push_str(&text);
                }
            }
            Event::End(TagEnd::Image) => {
                if let Some(mut image) = current.take() {
                    image.range.end = image.range.end.max(range.end);
                    images.push(image);
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                // HTML event ranges may contain prose as well as images. Replace only
                // individual img tags, retaining surrounding details/table markup.
                let Ok(selector) = scraper::Selector::parse("img") else {
                    continue;
                };
                let mut rest = html.as_ref();
                let mut offset = 0;
                while let Some(start) = rest.find('<') {
                    let Some(tag_start) = rest.get(start..) else {
                        break;
                    };
                    let mut quote = None;
                    let mut end = None;
                    for (i, ch) in tag_start.char_indices().skip(1) {
                        if quote == Some(ch) {
                            quote = None;
                        } else if quote.is_none() && matches!(ch, '\'' | '"') {
                            quote = Some(ch);
                        } else if quote.is_none() && ch == '>' {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    let Some(end) = end else { break };
                    let Some(tag) = tag_start.get(..end) else {
                        break;
                    };
                    let fragment = scraper::Html::parse_fragment(tag);
                    if let Some(element) = fragment.select(&selector).next()
                        && let Some(src) = element.value().attr("src")
                    {
                        images.push(Reference {
                            range: (range.start + offset + start)
                                ..(range.start + offset + start + end),
                            source: src.into(),
                            label: element.value().attr("alt").unwrap_or("Image").into(),
                        });
                    }
                    offset += start + end;
                    let Some(next) = rest.get(start + end..) else {
                        break;
                    };
                    rest = next;
                }
            }
            _ => {}
        }
    }
    images.sort_by_key(|image| image.range.start);
    images
}

fn validate_url(url: &Url) -> Result<()> {
    ensure!(
        url.scheme() == "https",
        "Only HTTPS image downloads are supported"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Image URLs containing credentials are not supported"
    );
    ensure!(url.host_str().is_some(), "Image URL has no host");
    Ok(())
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_unspecified()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (18..=19).contains(&b))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4() {
                return public_ip(IpAddr::V4(v4));
            }
            let [a, b, _, _, _, _, _, _] = ip.segments();
            a & 0xe000 == 0x2000 && !(a == 0x2001 && b == 0x0db8)
        }
    }
}
#[derive(Debug)]
struct PublicResolver;
impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> std::result::Result<ResolvedSocketAddrs, ureq::Error> {
        let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
        if addresses.iter().any(|address| !public_ip(address.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
}
fn github_auth_host(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("github.com" | "api.github.com" | "raw.githubusercontent.com")
    )
}
fn github_token(cancel: &Cancel) -> Result<String> {
    let output = process::run_limited(
        Command::new("gh").args(["auth", "token", "--hostname", "github.com"]),
        cancel,
        4096,
    )?;
    ensure!(
        output.code == 0,
        "GitHub authentication is unavailable for this image"
    );
    let token =
        String::from_utf8(output.stdout).context("Invalid GitHub authentication response")?;
    ensure!(
        !token.trim().is_empty(),
        "GitHub authentication is unavailable for this image"
    );
    Ok(token.trim().into())
}
fn download(request: &Request, cancel: &Cancel) -> Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .timeout_connect(Some(Duration::from_secs(5)))
        .build();
    let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), PublicResolver);
    let mut url = request.download_url()?;
    let mut token = None;
    for _ in 0..8 {
        cancel.check()?;
        validate_url(&url)?;
        let mut get = agent
            .get(url.as_str())
            .header("User-Agent", "difu image preview");
        if github_auth_host(&url)
            && let Some(token) = &token
        {
            get = get.header("Authorization", format!("Bearer {token}"));
        }
        if url.host_str() == Some("api.github.com") {
            get = get.header("Accept", "application/vnd.github.raw+json");
        }
        // Do not include redirect URLs, signed query parameters, or tokens in errors.
        let mut response = get.call().map_err(|_| anyhow::anyhow!("Image download failed or timed out; the host may be unavailable or resolve to a non-public address"))?;
        let status = response.status().as_u16();
        if matches!(status, 401 | 403 | 404) && github_auth_host(&url) && token.is_none() {
            token = Some(github_token(cancel)?);
            continue;
        }
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|s| s.to_str().ok())
                .context("Image redirect has no destination")?;
            url = url.join(location).context("Invalid image redirect")?;
            continue;
        }
        ensure!(
            response.status().is_success(),
            "Image download returned HTTP {status}; repository access or a browser login may be required"
        );
        let mut bytes = Vec::new();
        let mut reader = response.body_mut().as_reader();
        let mut chunk = [0u8; 16384];
        loop {
            cancel.check()?;
            let count = reader
                .read(&mut chunk)
                .context("Could not read image data")?;
            if count == 0 {
                break;
            }
            ensure!(
                bytes.len().saturating_add(count) <= MAX_BYTES,
                "Image exceeds the 10 MiB preview limit"
            );
            if let Some(data) = chunk.get(..count) {
                bytes.extend_from_slice(data);
            }
        }
        return Ok(bytes);
    }
    bail!("Too many image redirects")
}

pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    ensure!(
        bytes.len() <= MAX_BYTES,
        "Image exceeds the 10 MiB preview limit"
    );
    let format = image::guess_format(bytes).context("Image format was not recognized")?;
    ensure!(
        matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
        ),
        "This image format is not supported; use Open in browser"
    );
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .context("Could not decode image within preview limits")?;
    ensure!(image.width() > 0 && image.height() > 0, "Image is empty");
    Ok(image)
}

pub fn load(storage: &Storage, request: &Request, cancel: &Cancel) -> Result<DynamicImage> {
    cached_image(storage, request, cancel, || download(request, cancel))
}
fn cached_image(
    storage: &Storage,
    request: &Request,
    cancel: &Cancel,
    fetch: impl FnOnce() -> Result<Vec<u8>>,
) -> Result<DynamicImage> {
    cancel.check()?;
    let directory = storage.cache.join("images");
    let path = directory.join(format!("{}.image", request.id()));
    if path.exists() {
        let mut bytes = Vec::new();
        fs::File::open(&path)?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        return decode(&bytes);
    }
    let bytes = fetch()?;
    let image = decode(&bytes)?;
    cancel.check()?;
    fs::create_dir_all(&directory)?;
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    file.write_all(&bytes)?;
    file.persist(&path).context("Could not cache image")?;
    prune_cache(&directory, &path)?;
    Ok(image)
}
fn prune_cache(directory: &Path, keep: &Path) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(hash) = name.strip_suffix(".image") else {
            continue;
        };
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_file() {
            entries.push((metadata.modified()?, metadata.len(), entry.path()));
        }
    }
    entries.sort_by_key(|(modified, _, _)| *modified);
    let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
    for (_, size, path) in entries {
        if total <= CACHE_BYTES {
            break;
        }
        if path != keep {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}

use crate::{
    app::{Action, App, Message, Modal},
    ui::{ACCENT, BG, DIM, TEXT, TextRow, link, prose, text},
};
use ratatui::{
    Frame,
    layout::{Rect, Size},
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph},
};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    sliced::{SignedPosition, SlicedImage, SlicedProtocol},
};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone)]
pub struct PreviewRow {
    pub request: Request,
    pub offset: u16,
    pub height: u16,
    pub width: u16,
    pub inset: u16,
}
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct RenderKey {
    id: String,
    width: u16,
    height: u16,
}
#[derive(Default)]
pub struct State {
    picker: Option<Picker>,
    entries: HashMap<RenderKey, Result<SlicedProtocol, String>>,
    order: VecDeque<RenderKey>,
    pending: HashSet<RenderKey>,
}
impl State {
    pub fn detect(&mut self) {
        self.picker = Picker::from_query_stdio()
            .ok()
            .filter(|p| p.protocol_type() != ProtocolType::Halfblocks);
    }
    pub fn supported(&self) -> bool {
        self.picker.is_some()
    }
    pub fn receive(&mut self, key: RenderKey, output: Result<SlicedProtocol, String>) {
        self.pending.remove(&key);
        self.entries.insert(key.clone(), output);
        self.order.retain(|k| k != &key);
        self.order.push_back(key);
        while self.order.len() > 16 {
            if let Some(key) = self.order.pop_front() {
                self.entries.remove(&key);
            }
        }
    }
}

pub(crate) fn rows(source: &str, width: usize, pr: &PrDetail, supported: bool) -> Vec<TextRow> {
    let mut rows = Vec::new();
    let mut end = 0;
    for reference in references(source) {
        if reference.range.start < end {
            continue;
        }
        if let Some(before) = source.get(end..reference.range.start) {
            rows.extend(prose(before, width));
        }
        end = reference.range.end;
        let label = if reference.label.trim().is_empty() {
            "Image".into()
        } else {
            crate::model::clean(&reference.label)
        };
        let request = Request::new(reference.source, label.clone(), pr);
        if let Ok(url) = request.browser_url()
            && matches!(
                std::path::Path::new(url.path())
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("svg" | "svgz")
            )
        {
            rows.push(link(
                format!("↗ {label} · SVG image · open in browser"),
                Action::Link(url.into()),
            ));
            continue;
        }
        for mut row in prose(&format!("Image: {label} · click to enlarge"), width) {
            row.action = Some(Action::Image(request.clone()));
            rows.push(row);
        }
        if supported {
            for offset in 0..24 {
                rows.push(TextRow {
                    image: Some(PreviewRow {
                        request: request.clone(),
                        offset,
                        height: 24,
                        width: u16::try_from(width).unwrap_or(u16::MAX),
                        inset: 0,
                    }),
                    action: Some(Action::Image(request.clone())),
                    ..Default::default()
                });
            }
        } else {
            rows.extend(prose(
                "Inline images are unavailable in this terminal.",
                width,
            ));
        }
        if let Ok(url) = request.browser_url() {
            rows.push(link("↗ Open image in browser", Action::Link(url.into())));
        } else {
            rows.extend(prose("This image has an unsupported URL.", width));
        }
    }
    if let Some(after) = source.get(end..) {
        rows.extend(prose(after, width));
    }
    rows
}

fn request_render(app: &mut App, request: &Request, width: u16, height: u16) -> RenderKey {
    let key = RenderKey {
        id: request.id(),
        width,
        height,
    };
    if app.images.entries.contains_key(&key) {
        app.images.order.retain(|k| k != &key);
        app.images.order.push_back(key.clone());
        return key;
    }
    if app.images.pending.contains(&key) || app.images.pending.len() >= 2 {
        return key;
    }
    let Some(picker) = app.images.picker.clone() else {
        return key;
    };
    app.images.pending.insert(key.clone());
    let request = request.clone();
    let storage = app.storage.clone();
    let job_key = key.clone();
    let failure_key = key.clone();
    let cancel = app.spawn(move |tx, cancel| {
        let output = (|| -> Result<SlicedProtocol> {
            let image = load(&storage, &request, &cancel)?;
            cancel.check()?;
            SlicedProtocol::new(&picker, image, Some(Size::new(width, height)))
                .context("Could not render image")
        })()
        .map_err(|error| format!("{error:#}"));
        let _ = tx.send(Message::Image(job_key, output));
    });
    if cancel.check().is_err() {
        app.images
            .receive(failure_key, Err("Could not start image loader".into()));
    }
    key
}
fn paint_image(
    frame: &mut Frame,
    app: &mut App,
    request: &Request,
    rect: Rect,
    size: Size,
    offset: u16,
) {
    if rect.is_empty() {
        return;
    }
    let key = request_render(app, request, size.width, size.height);
    match app.images.entries.get(&key) {
        Some(Ok(image)) => frame.render_widget(
            SlicedImage::new(image, SignedPosition::from((0, -(offset as i16)))),
            rect,
        ),
        output => {
            let message = match output {
                Some(Err(error)) => format!(
                    "{} · Open image in browser below",
                    crate::model::clean(error)
                ),
                _ if !app.images.supported() => {
                    "Inline images are unavailable in this terminal. Use Open in browser.".into()
                }
                _ => "Loading image…".into(),
            };
            frame.render_widget(
                Paragraph::new(message)
                    .style(Style::default().fg(DIM))
                    .wrap(ratatui::widgets::Wrap { trim: false }),
                rect,
            );
        }
    }
}

/// Paint each visible image once, clipping at both viewport edges. The row
/// metadata keeps image positions aligned with card borders and timeline insets.
pub(crate) fn draw_inline(
    frame: &mut Frame,
    app: &mut App,
    document: &crate::ui::Document,
    area: Rect,
) {
    if app.modal.is_some() {
        return;
    }
    let mut y = 0;
    while y < area.height {
        let Some(preview) = document
            .rows
            .get(app.scroll + usize::from(y))
            .and_then(|r| r.right.image.as_ref())
        else {
            y += 1;
            continue;
        };
        let height = preview
            .height
            .saturating_sub(preview.offset)
            .min(area.height - y);
        let rect = Rect::new(
            area.x.saturating_add(preview.inset),
            area.y + y,
            preview.width.min(area.width.saturating_sub(preview.inset)),
            height,
        );
        paint_image(
            frame,
            app,
            &preview.request,
            rect,
            Size::new(preview.width, preview.height),
            preview.offset,
        );
        app.hits
            .push((rect, Action::Image(preview.request.clone())));
        y = y.saturating_add(height.max(1));
    }
}
pub(crate) fn draw_modal(frame: &mut Frame, app: &mut App) {
    let Some(Modal::Image(request)) = &app.modal else {
        return;
    };
    let request = request.clone();
    let area = frame.area();
    let rect = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", crate::model::clean(&request.label)))
            .border_style(Style::default().fg(ACCENT))
            .style(Style::default().bg(BG)),
        rect,
    );
    app.hits.clear();
    let inner = Rect::new(
        rect.x + 2,
        rect.y + 1,
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(4),
    );
    paint_image(
        frame,
        app,
        &request,
        inner,
        Size::new(inner.width, inner.height),
        0,
    );
    let footer = Rect::new(inner.x, rect.bottom().saturating_sub(2), inner.width, 1);
    let row = request
        .browser_url()
        .map(|url| link("o Open in browser · Esc Back", Action::Link(url.into())))
        .unwrap_or_else(|_| text("Unsupported image URL · Esc Back", TEXT));
    crate::ui::paint(frame, footer, &row, app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{Review, View},
        model::PrSummary,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::{sync::Arc, time::Instant};
    fn pr() -> PrDetail {
        PrDetail {
            key: PrKey {
                owner: "example".into(),
                repo: "repo".into(),
                number: 1,
            },
            title: "Images".into(),
            body: "Before ![Screenshot](docs/image.png) after.".into(),
            author: "author".into(),
            head: "abc123".into(),
            base: "base".into(),
            head_branch: "feature".into(),
            base_branch: "main".into(),
            state: "open".into(),
            additions: 1,
            deletions: 1,
            changed_files: 1,
        }
    }
    fn png() -> Result<Vec<u8>> {
        let mut bytes = Cursor::new(Vec::new());
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(100, 100, |x, y| {
            image::Rgba([x as u8, y as u8, 200, 255])
        }));
        image.write_to(&mut bytes, ImageFormat::Png)?;
        Ok(bytes.into_inner())
    }
    #[test]
    fn markdown_and_html_images_preserve_offsets_and_ignore_code() -> Result<()> {
        let source = "before ![alt **bold**](image.png) after\n![ref][pic]\n\n[pic]: https://example.com/a.png\n\n<img src=\"a.png?x=1&amp;y=2\" alt='A > B'> tail\n\n```md\n![example](fake.png)\n```\n`![inline](fake.png)`";
        let refs = references(source);
        assert_eq!(refs.len(), 3);
        let first = refs.first().context("first image")?;
        assert_eq!(first.label, "alt bold");
        assert_eq!(
            source.get(first.range.clone()),
            Some("![alt **bold**](image.png)")
        );
        let html = refs.last().context("html image")?;
        assert_eq!(html.source, "a.png?x=1&y=2");
        assert_eq!(html.label, "A > B");
        assert!(
            source
                .get(html.range.clone())
                .is_some_and(|s| s.starts_with("<img ") && s.ends_with('>'))
        );
        assert!(references("<!-- <img src='hidden.png'> -->").is_empty());
        Ok(())
    }
    #[test]
    fn preview_html_picture_and_comments_do_not_leak_markup() {
        let source = r#"Intro **bold**.

<!-- greptile_comment -->
<!-- greptile_summary -->

<h2><a href="https://example.com/retrigger"><picture><source media="(prefers-color-scheme: dark)" srcset="https://example.com/dark.svg"><source media="(prefers-color-scheme: light)" srcset="https://example.com/light.svg"><img src="https://example.com/badge.svg" alt="Retrigger"></picture></a></h2>

<details><summary>Summary</summary>

Useful summary text.

</details>"#;
        let output = rows(source, 80, &pr(), true);
        assert!(output.iter().all(|row| row.image.is_none()));
        assert!(output.iter().any(|row| matches!(&row.action, Some(Action::Link(url)) if url == "https://example.com/badge.svg")));
        let rendered = output
            .iter()
            .map(|r| {
                r.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("greptile_comment"), "{rendered}");
        assert!(
            !rendered.contains("<source") && !rendered.contains("srcset="),
            "{rendered}"
        );
        assert!(rendered.contains("Summary") && rendered.contains("Useful summary text."));
    }
    #[test]
    fn urls_and_credentials_remain_scoped() -> Result<()> {
        let request = Request::new("docs/screen%20shot.png".into(), "Screen".into(), &pr());
        assert_eq!(
            request.download_url()?.as_str(),
            "https://github.com/example/repo/raw/abc123/docs/screen%20shot.png"
        );
        for source in [
            "file:///etc/passwd",
            "data:image/png;base64,AAA",
            "https://token@github.com/a",
            "http://example.com/a",
        ] {
            assert!(
                Request::new(source.into(), "x".into(), &pr())
                    .download_url()
                    .is_err()
            );
        }
        for host in [
            "https://github.com.attacker.example/a",
            "https://user-images.githubusercontent.com/a",
            "https://example.com/a",
        ] {
            assert!(!github_auth_host(&Url::parse(host)?));
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
        ] {
            assert!(!public_ip(ip.parse()?));
        }
        assert!(public_ip("8.8.8.8".parse()?));
        Ok(())
    }
    #[test]
    fn cache_reuses_valid_image_and_rejects_invalid_or_cancelled_data() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config"),
            cache: dir.path().join("cache"),
        };
        let request = Request::new("https://example.com/a.png".into(), "A".into(), &pr());
        let image = cached_image(&storage, &request, &Cancel::default(), png)?;
        assert_eq!(image.width(), 100);
        let image = cached_image(&storage, &request, &Cancel::default(), || {
            bail!("must not fetch a cached image")
        })?;
        assert_eq!(image.height(), 100);
        assert!(decode(b"<html>login required</html>").is_err());
        let cancel = Cancel::default();
        cancel.cancel();
        assert!(cached_image(&storage, &request, &cancel, png).is_err());
        assert!(decode(b"BM000000000000000000000000000000").is_err());
        Ok(())
    }
    #[test]
    fn supported_formats_decode_and_animated_gif_uses_first_frame() -> Result<()> {
        let image = DynamicImage::new_rgb8(8, 6);
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, format)?;
            let decoded = decode(&bytes.into_inner())?;
            assert_eq!((decoded.width(), decoded.height()), (8, 6));
        }
        let red = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        let blue = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255]));
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder.encode_frames([image::Frame::new(red), image::Frame::new(blue)])?;
        }
        let decoded = decode(&bytes)?.to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
        Ok(())
    }
    #[test]
    fn image_modal_preserves_review_position_and_unsupported_terminal_has_browser_action()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config"),
                cache: dir.path().join("cache"),
            },
            Default::default(),
        );
        let pr = pr();
        let fallback = rows(&pr.body, 80, &pr, false);
        assert!(!fallback.iter().any(|r| r.image.is_some()));
        assert!(
            fallback.iter().any(
                |r| matches!(&r.action,Some(Action::Link(url)) if url.contains("/blob/abc123/"))
            )
        );
        assert!(
            fallback
                .iter()
                .flat_map(|r| &r.spans)
                .any(|s| s.content.contains("unavailable"))
        );
        app.scroll = 47;
        app.workflow.cursor = Some(52);
        app.view = View::Guide;
        app.action(Action::Image(Request::new(
            "image.png".into(),
            "Image".into(),
            &pr,
        )));
        let mut terminal = Terminal::new(TestBackend::new(90, 30))?;
        terminal.draw(|frame| draw_modal(frame, &mut app))?;
        assert!(app.images.pending.is_empty());
        app.key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.modal.is_none());
        assert_eq!(app.scroll, 47);
        assert_eq!(app.workflow.cursor, Some(52));
        Ok(())
    }
    #[test]
    fn background_cache_loading_and_clipped_preview_keep_card_geometry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut app = App::new(
            Storage {
                config: dir.path().join("config"),
                cache: dir.path().join("cache"),
            },
            Default::default(),
        );
        // Halfblocks provides deterministic pixels in TestBackend; production
        // detection deliberately uses the browser fallback for this protocol.
        app.images.picker = Some(Picker::halfblocks());
        let pr = pr();
        let request = Request::new("docs/image.png".into(), "Screenshot".into(), &pr);
        cached_image(&app.storage, &request, &Cancel::default(), png)?;
        let key = request_render(&mut app, &request, 80, 12);
        assert_eq!(app.images.pending.len(), 1);
        request_render(&mut app, &request, 80, 12);
        assert_eq!(app.images.pending.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.images.entries.contains_key(&key) && Instant::now() < deadline {
            app.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(app.images.entries.get(&key), Some(Ok(_))));
        let review = Review {
            detail: Some(Arc::new(pr.clone())),
            timeline: vec![crate::model::TimelineItem {
                date: "2026-09-17T00:00:00Z".into(),
                author: "reviewer".into(),
                kind: "commented".into(),
                body: "<img alt='Comment image' src='docs/image.png'>".into(),
                url: String::new(),
            }],
            ..Default::default()
        };
        let rows = crate::overview::rows_with_images(&review, 84, true);
        let previews: Vec<_> = rows.iter().filter_map(|r| r.image.as_ref()).collect();
        assert_eq!(previews.len(), 48);
        assert!(
            previews
                .iter()
                .take(24)
                .all(|p| p.inset == 2 && p.width == 80)
        );
        assert!(
            previews
                .iter()
                .skip(24)
                .all(|p| p.inset == 4 && p.width == 78)
        );
        app.inbox.push(PrSummary {
            key: pr.key.clone(),
            title: pr.title.clone(),
            author: pr.author.clone(),
            updated: String::new(),
            created: String::new(),
            stats: None,
            stats_error: false,
            draft: false,
        });
        app.reviews.insert(pr.key.id(), review);
        app.scroll = rows
            .iter()
            .position(|r| r.image.is_some())
            .context("image row")?
            + 3;
        let document = crate::ui::Document {
            navigation: vec![],
            epoch: 0,
            width: 84,
            horizontal: 0,
            rows: rows
                .into_iter()
                .map(|right| crate::ui::Row {
                    right,
                    ..Default::default()
                })
                .collect(),
            sections: vec![],
            files: vec![],
            guide_columns: false,
            left_width: 0,
        };
        let mut terminal = Terminal::new(TestBackend::new(90, 10))?;
        terminal.draw(|frame| draw_inline(frame, &mut app, &document, Rect::new(1, 1, 84, 5)))?;
        assert!(
            app.hits
                .iter()
                .any(|(rect, action)| *rect == Rect::new(3, 1, 80, 5)
                    && matches!(action, Action::Image(_)))
        );
        assert_eq!(
            terminal.backend().buffer().cell((0, 0)).map(|c| c.symbol()),
            Some(" ")
        );
        app.shutdown();
        Ok(())
    }
}
