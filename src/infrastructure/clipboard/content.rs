use super::*;

pub(super) fn payload_kind(payload: &ClipboardPayload) -> ClipboardContentKind {
    match payload {
        ClipboardPayload::Text(_) => ClipboardContentKind::Text,
        ClipboardPayload::RichText { .. } => ClipboardContentKind::Html,
        ClipboardPayload::Image { .. } => ClipboardContentKind::Image,
        ClipboardPayload::Files(_) => ClipboardContentKind::Files,
    }
}

pub(super) fn sync_payload(record: &ClipboardSyncRecord) -> Result<ClipboardPayload> {
    match record.kind {
        ClipboardContentKind::Text => record
            .text
            .clone()
            .map(ClipboardPayload::Text)
            .ok_or_else(|| Error::Clipboard("synchronized text payload is missing".into())),
        ClipboardContentKind::Html => record
            .html
            .clone()
            .map(|html| ClipboardPayload::RichText {
                html,
                plain_text: record.text.clone().unwrap_or_default(),
                rtf: record.rtf.clone(),
            })
            .ok_or_else(|| Error::Clipboard("synchronized rich-text payload is missing".into())),
        ClipboardContentKind::Image => record
            .image_png
            .clone()
            .map(|png| ClipboardPayload::Image {
                png,
                width: record.width.unwrap_or_default(),
                height: record.height.unwrap_or_default(),
            })
            .ok_or_else(|| Error::Clipboard("synchronized image payload is missing".into())),
        ClipboardContentKind::Files => Err(Error::Clipboard(
            "file paths cannot be synchronized as clipboard content".into(),
        )),
    }
}

pub(super) fn hide_local_source_device(summary: &mut ClipboardSummary, local_device_id: &str) {
    if summary.source_device_id.as_deref() == Some(local_device_id) {
        summary.source_device_name = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(super) enum NativeClipboardOrigin {
    Local,
    Handoff,
    ArcRelay,
}

impl ClipboardFingerprints {
    fn matches(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash || self.semantic_hash == other.semantic_hash
    }
}

impl ClipboardCaptureState {
    pub(super) fn remember_write(&mut self, fingerprints: ClipboardFingerprints, now: Instant) {
        self.current = Some(fingerprints.clone());
        while self.writes.len() >= MAX_PENDING_SUPPRESSIONS {
            self.writes.pop_front();
        }
        self.writes.push_back((fingerprints, now));
    }

    pub(super) fn accepts(
        &mut self,
        fingerprints: &ClipboardFingerprints,
        origin: NativeClipboardOrigin,
        native_markers: bool,
        now: Instant,
    ) -> bool {
        self.writes
            .retain(|(_, at)| now.duration_since(*at) <= SUPPRESSION_TTL);
        match origin {
            NativeClipboardOrigin::ArcRelay => false,
            NativeClipboardOrigin::Handoff => !self
                .current
                .as_ref()
                .is_some_and(|last| last.matches(fingerprints)),
            // A native application copy clears our marker. Even the same content
            // copied immediately after a remote write is a real new copy.
            NativeClipboardOrigin::Local => {
                native_markers
                    || !self
                        .writes
                        .iter()
                        .any(|(written, _)| written.matches(fingerprints))
            }
        }
    }
}

pub(super) fn lock_capture_state(
    capture_state: &SharedCaptureState,
) -> Result<std::sync::MutexGuard<'_, ClipboardCaptureState>> {
    capture_state
        .lock()
        .map_err(|_| Error::Clipboard("clipboard capture lock poisoned".into()))
}

pub(super) fn capture_payload(context: &ClipboardContext) -> Result<Option<ClipboardPayload>> {
    if context.has(ContentFormat::Files) {
        if let Ok(files) = context.get_files() {
            if !files.is_empty() {
                if files.len() == 1 {
                    if let Some(image) = image_payload_from_file(&files[0])? {
                        return Ok(Some(image));
                    }
                }
                return Ok(Some(ClipboardPayload::Files(files)));
            }
        }
    }
    if context.has(ContentFormat::Image) {
        if let Ok(image) = context.get_image() {
            let (width, height) = image.get_size();
            let png = image
                .to_png()
                .map_err(|error| Error::Clipboard(format!("encode clipboard image: {error}")))?
                .get_bytes()
                .to_vec();
            if png.len() > MAX_CAPTURED_IMAGE_BYTES {
                return Err(Error::Clipboard(format!(
                    "clipboard image exceeds {} MiB",
                    MAX_CAPTURED_IMAGE_BYTES / 1024 / 1024
                )));
            }
            return Ok(Some(ClipboardPayload::Image { png, width, height }));
        }
    }
    let html = context
        .has(ContentFormat::Html)
        .then(|| context.get_html().ok())
        .flatten()
        .filter(|value| !value.is_empty());
    let rtf = context
        .has(ContentFormat::Rtf)
        .then(|| context.get_rich_text().ok())
        .flatten()
        .filter(|value| !value.is_empty());
    if html.is_some() || rtf.is_some() {
        let plain_text = context
            .get_text()
            .ok()
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| html.as_deref().map(strip_html).unwrap_or_default());
        let html = html.unwrap_or_else(|| plain_text_html(&plain_text));
        return Ok(Some(ClipboardPayload::RichText {
            html,
            plain_text,
            rtf,
        }));
    }
    match context.get_text() {
        Ok(text) if !text.is_empty() => Ok(Some(ClipboardPayload::Text(text))),
        Ok(_) => Ok(None),
        Err(error) => Err(Error::Clipboard(format!("read clipboard: {error}"))),
    }
}

pub(super) fn plain_text_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!("<pre>{escaped}</pre>")
}

pub(super) fn image_payload_from_file(path: &str) -> Result<Option<ClipboardPayload>> {
    let path_ref = Path::new(path);
    let extension = path_ref
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("jpg" | "jpeg" | "jpe" | "png")) {
        return Ok(None);
    }
    let metadata = match std::fs::metadata(path_ref) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_CAPTURED_IMAGE_BYTES as u64 => {
            metadata
        }
        Ok(_) | Err(_) => return Ok(None),
    };
    let _ = metadata;
    let image = match clipboard_rs::RustImageData::from_path(path) {
        Ok(image) => image,
        Err(error) => {
            warn!(%error, "failed to decode copied image file");
            return Ok(None);
        }
    };
    let (width, height) = image.get_size();
    let png = image
        .to_png()
        .map_err(|error| Error::Clipboard(format!("encode copied image file: {error}")))?
        .get_bytes()
        .to_vec();
    if png.len() > MAX_CAPTURED_IMAGE_BYTES {
        return Ok(None);
    }
    Ok(Some(ClipboardPayload::Image { png, width, height }))
}

pub(super) fn detect_source_application(
    context: &ClipboardContext,
    source_provider: &dyn WindowManagerRepository,
) -> Option<String> {
    #[cfg(target_os = "macos")]
    if context.available_formats().is_ok_and(|formats| {
        formats
            .iter()
            .any(|format| format == "com.apple.is-remote-clipboard")
    }) {
        return Some("Apple Handoff".into());
    }
    source_provider
        .focused_app_name_now()
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "macos")]
pub(super) fn write_payload(
    _context: &ClipboardContext,
    payload: ClipboardPayload,
    local_only: bool,
) -> Result<()> {
    macos::write_payload(payload, local_only)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn write_payload(
    context: &ClipboardContext,
    payload: ClipboardPayload,
    _local_only: bool,
) -> Result<()> {
    let result = match payload {
        ClipboardPayload::Text(text) => context.set_text(text),
        ClipboardPayload::RichText {
            html,
            plain_text,
            rtf,
        } => {
            let mut contents = vec![ClipboardContent::Text(plain_text)];
            if !html.is_empty() {
                contents.push(ClipboardContent::Html(html));
            }
            if let Some(rtf) = rtf.filter(|value| !value.is_empty()) {
                contents.push(ClipboardContent::Rtf(rtf));
            }
            context.set(contents)
        }
        ClipboardPayload::Image { png, .. } => {
            let image = clipboard_rs::RustImageData::from_bytes(&png)
                .map_err(|error| Error::Clipboard(format!("decode stored image: {error}")))?;
            context.set_image(image)
        }
        ClipboardPayload::Files(paths) => context.set_files(paths),
    };
    result.map_err(|error| Error::Clipboard(format!("restore clipboard record: {error}")))
}

pub(super) fn summarize(
    payload: &ClipboardPayload,
    source_app: Option<String>,
) -> ClipboardSummary {
    let captured_at = Utc::now();
    let (
        kind,
        preview_source,
        size_bytes,
        character_count,
        item_count,
        width,
        height,
        text_syntax,
        available,
    ): (_, Cow<'_, str>, _, _, _, _, _, _, _) = match payload {
        ClipboardPayload::Text(text) => (
            ClipboardContentKind::Text,
            Cow::Borrowed(text),
            text.len() as u64,
            Some(text.chars().count().min(u64::MAX as usize) as u64),
            1,
            None,
            None,
            detect_text_syntax_bounded(text),
            true,
        ),
        ClipboardPayload::RichText {
            html,
            plain_text,
            rtf,
        } => (
            ClipboardContentKind::Html,
            if plain_text.is_empty() {
                Cow::Owned(strip_html(html))
            } else {
                Cow::Borrowed(plain_text)
            },
            html.len()
                .saturating_add(plain_text.len())
                .saturating_add(rtf.as_ref().map_or(0, String::len)) as u64,
            Some(plain_text.chars().count().min(u64::MAX as usize) as u64),
            1,
            None,
            None,
            detect_text_syntax_bounded(plain_text),
            true,
        ),
        ClipboardPayload::Image { png, width, height } => (
            ClipboardContentKind::Image,
            Cow::Owned(format!("Image · {width} × {height}")),
            png.len() as u64,
            None,
            1,
            Some(*width),
            Some(*height),
            ClipboardTextSyntax::Plain,
            true,
        ),
        ClipboardPayload::Files(paths) => {
            let names = paths
                .iter()
                .filter_map(|path| Path::new(path).file_name())
                .map(|name| name.to_string_lossy())
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            let mut size_bytes = 0u64;
            let mut available = true;
            for path in paths {
                match std::fs::metadata(path) {
                    Ok(metadata) => size_bytes = size_bytes.saturating_add(metadata.len()),
                    Err(_) => available = false,
                }
            }
            (
                ClipboardContentKind::Files,
                Cow::Owned(if paths.len() == 1 {
                    names
                } else {
                    format!("{} files · {names}", paths.len())
                }),
                size_bytes,
                None,
                paths.len().min(u32::MAX as usize) as u32,
                None,
                None,
                ClipboardTextSyntax::Plain,
                available,
            )
        }
    };
    let sensitive = matches!(
        kind,
        ClipboardContentKind::Text | ClipboardContentKind::Html
    ) && looks_sensitive(preview_source.as_ref());
    ClipboardSummary {
        id: 0,
        kind,
        preview: if sensitive {
            "••••••••".into()
        } else {
            bounded_preview(preview_source.as_ref())
        },
        source_app,
        first_captured_at: captured_at,
        captured_at,
        last_used_at: None,
        size_bytes,
        character_count,
        item_count,
        width,
        height,
        sensitive,
        favorite: false,
        labels: Vec::new(),
        copy_count: 1,
        available,
        sync_id: String::new(),
        source_device_id: None,
        source_device_name: None,
        text_syntax,
    }
}

pub(super) fn bounded_preview(value: &str) -> String {
    let mut preview = String::with_capacity(MAX_PREVIEW_CHARS + 3);
    let mut emitted = 0usize;
    let mut pending_space = false;
    let mut truncated = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending_space |= emitted > 0;
            continue;
        }
        if pending_space {
            if emitted == MAX_PREVIEW_CHARS {
                truncated = true;
                break;
            }
            preview.push(' ');
            emitted += 1;
            pending_space = false;
        }
        if emitted == MAX_PREVIEW_CHARS {
            truncated = true;
            break;
        }
        preview.push(character);
        emitted += 1;
    }
    if truncated {
        preview.push('…');
    }
    if preview.is_empty() {
        "Empty content".into()
    } else {
        preview
    }
}

pub(super) fn sanitize_html_preview(html: &str) -> Option<String> {
    if html.is_empty() || html.len() > MAX_SAFE_HTML_SOURCE_BYTES {
        return None;
    }
    let fragment = html
        .chars()
        .take(MAX_SAFE_HTML_INPUT_CHARS)
        .collect::<String>();
    let tags = [
        "p",
        "br",
        "div",
        "span",
        "strong",
        "b",
        "em",
        "i",
        "u",
        "s",
        "del",
        "mark",
        "small",
        "sub",
        "sup",
        "blockquote",
        "pre",
        "code",
        "ul",
        "ol",
        "li",
        "dl",
        "dt",
        "dd",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "th",
        "td",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "a",
    ]
    .into_iter()
    .collect();
    let attributes = ["title"].into_iter().collect();
    let mut builder = ammonia::Builder::default();
    builder
        .tags(tags)
        .generic_attributes(attributes)
        .tag_attributes(std::collections::HashMap::new())
        .strip_comments(true);
    let sanitized = builder.clean(&fragment).to_string();
    (!sanitized.trim().is_empty()).then_some(sanitized)
}

pub(super) fn strip_html(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub(super) fn looks_sensitive(value: &str) -> bool {
    [
        "password",
        "passwd",
        "passcode",
        "api_key",
        "api-key",
        "access_token",
        "refresh_token",
        "secret=",
        "secret:",
        "authorization: bearer",
        "private key",
    ]
    .iter()
    .any(|needle| {
        value
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
    })
}

pub(super) fn detect_text_syntax_bounded(value: &str) -> ClipboardTextSyntax {
    if value.len() <= MAX_TEXT_SYNTAX_DETECTION_BYTES {
        return detect_text_syntax(value);
    }
    let mut end = MAX_TEXT_SYNTAX_DETECTION_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    detect_text_syntax(&value[..end])
}

pub(super) fn content_hash(payload: &ClipboardPayload) -> String {
    let mut digest = Xxh3::new();
    match payload {
        ClipboardPayload::Text(text) => {
            digest.update(b"text\0");
            digest.update(text.as_bytes());
        }
        ClipboardPayload::RichText {
            html,
            plain_text,
            rtf,
        } => {
            digest.update(b"rich-text\0");
            digest.update(html.as_bytes());
            digest.update(b"\0text\0");
            digest.update(plain_text.as_bytes());
            if let Some(rtf) = rtf {
                digest.update(b"\0rtf\0");
                digest.update(rtf.as_bytes());
            }
        }
        ClipboardPayload::Image { png, .. } => {
            digest.update(b"image\0");
            digest.update(png);
        }
        ClipboardPayload::Files(paths) => {
            digest.update(b"files\0");
            for path in paths {
                digest.update(path.as_bytes());
                digest.update(&[0]);
            }
        }
    }
    format!("{:032x}", digest.digest128())
}

pub(super) fn clipboard_fingerprints(payload: &ClipboardPayload) -> ClipboardFingerprints {
    let content_hash = content_hash(payload);
    let semantic_hash = match payload {
        ClipboardPayload::Text(_) | ClipboardPayload::RichText { .. } => semantic_hash(payload),
        ClipboardPayload::Image { .. } => semantic_hash(payload),
        ClipboardPayload::Files(_) => content_hash.clone(),
    };
    ClipboardFingerprints {
        content_hash,
        semantic_hash,
    }
}

/// A format-tolerant fingerprint for clipboard writes performed by ArcRelay.
/// macOS may normalize HTML or RTF while placing the same visible text on the
/// pasteboard, so loop suppression cannot rely on the serialized rich-text
/// payload being byte-for-byte identical when the watcher reads it back.
pub(super) fn semantic_hash(payload: &ClipboardPayload) -> String {
    let mut digest = Xxh3::new();
    match payload {
        ClipboardPayload::Text(text) => {
            digest.update(b"textual\0");
            update_normalized_text(&mut digest, text);
        }
        ClipboardPayload::RichText {
            html, plain_text, ..
        } => {
            digest.update(b"textual\0");
            if plain_text.is_empty() {
                update_normalized_text(&mut digest, &strip_html(html));
            } else {
                update_normalized_text(&mut digest, plain_text);
            }
        }
        ClipboardPayload::Image { png, .. } => {
            digest.update(b"image\0");
            // PNG metadata/compression can change during native pasteboard conversion.
            // Compare decoded pixels, but retain a byte fallback for invalid input.
            if let Ok(image) = image::load_from_memory_with_format(png, image::ImageFormat::Png) {
                let rgba = image.into_rgba8();
                digest.update(&rgba.width().to_le_bytes());
                digest.update(&rgba.height().to_le_bytes());
                digest.update(rgba.as_raw());
            } else {
                digest.update(png);
            }
        }
        ClipboardPayload::Files(paths) => {
            digest.update(b"files\0");
            for path in paths {
                digest.update(path.as_bytes());
                digest.update(&[0]);
            }
        }
    }
    format!("{:032x}", digest.digest128())
}

pub(super) fn update_normalized_text(digest: &mut Xxh3, text: &str) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    digest.update(normalized.as_bytes());
}
