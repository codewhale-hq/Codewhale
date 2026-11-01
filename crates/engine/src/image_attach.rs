//! Turning a local image file into a wire-ready image content block.
//!
//! CodeWhale's message model has been multimodal for a long time —
//! [`ContentBlock::ImageUrl`] round-trips
//! through session persistence, compaction and all three wire builders. What it
//! never had was a *faucet*: nothing outside `#[cfg(any(test, feature = "test-support"))]` ever constructed
//! one, so a user who attached a screenshot got the literal text
//! `[Attached image: /path/to/shot.png]` and a model that correctly concluded it
//! could not see the picture.
//!
//! This module is that faucet. It reads a file, proves it is an image the
//! providers actually accept, holds it to a size budget, and emits a
//! `data:` URL.
//!
//! # Two stages, because the two failure kinds differ
//!
//! [`expand_attachment_blocks`] runs when the message is built and decides the
//! *permanent* questions: does this file exist, is it really an image, is it
//! small enough. Those answers cannot change, so they are baked into history.
//!
//! [`strip_images_when_unsupported`] runs per outbound request and decides the
//! one *contingent* question: can the model this request is going to actually
//! see images. Routes change mid-session, so answering that at build time
//! would mean attaching a screenshot under a text-only model and losing it
//! permanently, even after switching to a vision model. History keeps the
//! image; each request is normalized against its own route.
//!
//! # Provider neutrality
//!
//! There is exactly one internal representation — a `data:<media-type>;base64,…`
//! URL on `ContentBlock::ImageUrl` — and each wire builder projects it:
//!
//! | wire format | shape |
//! |---|---|
//! | Chat Completions | `{"type":"image_url","image_url":{"url":"data:…"}}` |
//! | Responses | `{"type":"input_image","image_url":"data:…"}` |
//! | Anthropic Messages | `{"type":"image","source":{"type":"base64","media_type":…,"data":…}}` |
//!
//! The Anthropic split lives in [`parse_data_url`], which
//! `client::anthropic` calls. Anthropic is the reason the accepted-format list
//! below is not simply "whatever the `image` crate can decode": it accepts only
//! PNG/JPEG/GIF/WebP, and so, therefore, do we. Refusing a BMP here with a
//! readable message beats letting one through to a provider-side 400.

use anyhow::{Result, bail};
use codewhale_protocol::runtime::{
    MAX_RUNTIME_IMAGE_BYTES, MAX_RUNTIME_IMAGE_TOTAL_BYTES, MAX_RUNTIME_IMAGES, RuntimeImageInput,
};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder, ImageReader, Limits};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::model_profile::SupportState;
use codewhale_models::image_window::{parse_data_url, tool_result_text_with_omission};
use codewhale_models::{ContentBlock, ImageUrlContent};

/// Largest source image accepted, in bytes, before base64 expansion.
///
/// Base64 inflates by 4/3, so this admits roughly 6.7 MB of request body per
/// image. The number is Anthropic's documented per-image ceiling; keeping the
/// tightest provider limit as the shared limit is what makes a "CodeWhale
/// accepted it" verdict portable across routes.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Maximum width or height admitted for an input image (8192 px).
pub const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Maximum total pixels admitted before decoding is aborted (~33.5 megapixels).
pub const MAX_IMAGE_PIXELS: u64 = 33_554_432;

/// Memory allocation limit for image decoding (64 MiB).
pub const MAX_DECODE_ALLOC_BYTES: u64 = 64 * 1024 * 1024;

pub fn decode_and_guard_image(bytes: &[u8]) -> Result<(DynamicImage, u32, u32)> {
    let limits = || {
        let mut limits = Limits::default();
        limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
        limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
        limits
    };
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| anyhow::anyhow!("invalid image header or decompression bomb guard"))?;
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
    {
        bail!("image dimensions exceed the decompression bomb guard; downscale or crop first");
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(limits());
    let decoded = reader
        .decode()
        .map_err(|_| anyhow::anyhow!("invalid image content or decode allocation limit"))?;
    Ok((decoded, width, height))
}

/// Validate untrusted inline input before route selection or durable admission.
/// Return the existing provider-neutral history representation; no file is opened.
pub fn prepare_runtime_images(images: &[RuntimeImageInput]) -> Result<Vec<ContentBlock>> {
    if images.len() > MAX_RUNTIME_IMAGES {
        bail!("images exceed the {MAX_RUNTIME_IMAGES} attachment limit");
    }
    prepare_images_with_limit(
        images,
        MAX_RUNTIME_IMAGE_BYTES,
        Some(MAX_RUNTIME_IMAGE_TOTAL_BYTES),
    )
}

/// Internal Engine/history input retains the established local 5 MiB ceiling.
/// Network callers must first pass `prepare_runtime_images` (4 MiB per image,
/// 10 images and 5 MiB total). Local history never had those aggregate/count
/// limits; impose only its existing per-image bound and bounded full decode.
pub fn prepare_stored_images(images: &[RuntimeImageInput]) -> Result<Vec<ContentBlock>> {
    prepare_images_with_limit(images, MAX_IMAGE_BYTES, None)
}

fn prepare_images_with_limit(
    images: &[RuntimeImageInput],
    per_image_limit: usize,
    total_limit: Option<usize>,
) -> Result<Vec<ContentBlock>> {
    let mut total = 0usize;
    images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            if image.data_base64.len() > per_image_limit.div_ceil(3) * 4 {
                bail!(
                    "image {} exceeds the {} MiB limit",
                    index + 1,
                    per_image_limit / (1024 * 1024)
                );
            }
            let bytes = STANDARD
                .decode(&image.data_base64)
                .map_err(|_| anyhow::anyhow!("image {} has invalid base64", index + 1))?;
            if bytes.len() > per_image_limit {
                bail!(
                    "image {} exceeds the {} MiB limit",
                    index + 1,
                    per_image_limit / (1024 * 1024)
                );
            }
            total = total.saturating_add(bytes.len());
            if total_limit.is_some_and(|limit| total > limit) {
                bail!("images exceed the 5 MiB total limit");
            }
            let attached = encode_image_bytes(&bytes, &format!("image {}", index + 1))?;
            if image.mime != attached.media_type {
                bail!("image {} MIME does not match its content", index + 1);
            }
            decode_and_guard_image(&bytes)?;
            // Standard padded base64 is the one replay representation.
            if STANDARD.encode(&bytes) != image.data_base64 {
                bail!("image {} base64 is not canonical", index + 1);
            }
            Ok(attached.content_block())
        })
        .collect()
}

/// Reuse durable canonical bytes for retry, never reread a path or URL.
pub fn runtime_images_from_blocks(blocks: &[ContentBlock]) -> Result<Vec<RuntimeImageInput>> {
    let mut images = Vec::new();
    for block in blocks {
        if let ContentBlock::ImageUrl { image_url } = block {
            if image_url.url.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 + 32 {
                bail!("stored image exceeds the attachment limit");
            }
            let (mime, data) = parse_data_url(&image_url.url)
                .ok_or_else(|| anyhow::anyhow!("stored image requires canonical inline content"))?;
            images.push(RuntimeImageInput {
                mime: mime.to_string(),
                data_base64: data.to_string(),
            });
        }
    }
    prepare_stored_images(&images)?;
    Ok(images)
}

/// Validate new image-bearing durable records without rewriting their block order.
/// Legacy schema 2 history continues to use its original interpretation.
pub fn validate_stored_image_content(blocks: &[ContentBlock]) -> Result<()> {
    if blocks.iter().any(|block| {
        !matches!(
            block,
            ContentBlock::Text { .. } | ContentBlock::ImageUrl { .. }
        )
    }) {
        bail!("invalid persisted user image content kind");
    }
    if runtime_images_from_blocks(blocks)?.is_empty() {
        bail!("persisted image input must contain an image");
    }
    Ok(())
}

/// Why a file could not be attached as an image.
///
/// Every variant renders to a sentence naming the file and the reason. These
/// strings reach both the user (as a command error) and the model (as an
/// in-band notice), so they say what to do next rather than only what failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageAttachError {
    /// The file could not be read at all.
    Unreadable { path: String, reason: String },
    /// The file is zero bytes.
    Empty { path: String },
    /// Over `limit` bytes: [`MAX_IMAGE_BYTES`] for already-encoded bytes, the
    /// larger source bound when attach-time downscaling applies.
    TooLarge {
        path: String,
        bytes: usize,
        limit: usize,
    },
    /// Magic bytes identify a format no provider in the set accepts.
    UnsupportedFormat { path: String, detected: String },
    /// Magic bytes match nothing we recognize as an image.
    NotAnImage { path: String },
}

impl std::fmt::Display for ImageAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, reason } => {
                write!(f, "Cannot attach {path}: {reason}")
            }
            Self::Empty { path } => {
                write!(f, "Cannot attach {path}: the file is empty")
            }
            Self::TooLarge { path, bytes, limit } => write!(
                f,
                "Cannot attach {path}: {} exceeds the {} per-image limit. \
                 Downscale or crop it first.",
                human_bytes(*bytes),
                human_bytes(*limit),
            ),
            Self::UnsupportedFormat { path, detected } => write!(
                f,
                "Cannot attach {path}: {detected} is not accepted by vision \
                 models. Convert it to PNG, JPEG, GIF or WebP.",
            ),
            Self::NotAnImage { path } => write!(
                f,
                "Cannot attach {path}: the file is not a PNG, JPEG, GIF or \
                 WebP image (its contents do not match any of those formats).",
            ),
        }
    }
}

impl std::error::Error for ImageAttachError {}

pub fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// An image that is ready to go on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedImage {
    /// e.g. `"image/png"`.
    pub media_type: &'static str,
    /// `data:<media_type>;base64,<payload>`.
    pub data_url: String,
    /// Size of the source file, before base64.
    pub source_bytes: usize,
}

/// Validated image content returned by lowercase `read`.
pub struct PreparedToolImage {
    pub block: Option<codewhale_tools::ToolResultContentBlock>,
    pub note: String,
}

/// Prepare one provider-neutral tool-result image using Codewhale's existing
/// cross-provider format and size policy. Failure is a visible text receipt,
/// not a failed file read.
#[must_use]
pub fn prepare_tool_image_bytes(bytes: &[u8], mime_type: &str) -> PreparedToolImage {
    let mime_type = mime_type.split(';').next().unwrap_or(mime_type).trim();
    let budget = crate::tools::read_media::READ_IMAGE_BYTE_BUDGET;
    if bytes.len() <= crate::tools::read_media::MAX_SOURCE_IMAGE_BYTES
        && sniff_media_type(bytes) == Some(mime_type)
        && decode_and_guard_image(bytes).is_ok()
        && let Ok(fitted) = fit_image_bytes(bytes, Path::new("image"), budget)
        && let Some((fitted_mime, payload)) = parse_data_url(&fitted.data_url)
    {
        let note = if fitted.source_bytes == bytes.len() && fitted_mime == mime_type {
            format!("Read image file [{mime_type}]")
        } else {
            format!(
                "Read image file [{mime_type}] (re-encoded from {} to a {} {fitted_mime} within the {} default delivery budget)",
                human_bytes(bytes.len()),
                human_bytes(fitted.source_bytes),
                human_bytes(budget),
            )
        };
        return PreparedToolImage {
            block: Some(codewhale_tools::ToolResultContentBlock::Image {
                mime_type: fitted_mime.to_string(),
                data: payload.to_string(),
            }),
            note,
        };
    }
    PreparedToolImage {
        block: None,
        note: format!(
            "Read image file [{mime_type}]\n[Image omitted: unsupported, invalid, above the source limit, or unable to fit the {} default delivery budget. Use read_media with a crop or higher detail if needed.]",
            human_bytes(budget),
        ),
    }
}

fn valid_tool_image(mime_type: &str, data: &str) -> bool {
    matches!(
        mime_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) && data.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4
        && STANDARD.decode(data).is_ok_and(|bytes| {
            bytes.len() <= MAX_IMAGE_BYTES
                && sniff_media_type(&bytes) == Some(mime_type)
                && decode_and_guard_image(&bytes).is_ok()
        })
}

/// Enforce the same one-image limit at the tool execution boundary so plugin
/// or future rich tools cannot create unbounded history.
#[must_use]
pub fn bound_rich_tool_result(
    mut rich: crate::tools::spec::RichToolResult,
) -> crate::tools::spec::RichToolResult {
    let mut kept = Vec::with_capacity(1);
    let mut omitted = 0usize;
    for block in rich.content_blocks.drain(..) {
        let codewhale_tools::ToolResultContentBlock::Image { mime_type, data } = &block;
        if kept.is_empty() && valid_tool_image(mime_type, data) {
            kept.push(block);
        } else {
            omitted += 1;
        }
    }
    rich.result.content = tool_result_text_with_omission(&rich.result.content, omitted);
    rich.content_blocks = kept;
    rich
}

/// Borrow the first valid inline image and count everything omitted.
#[must_use]
pub fn provider_tool_result_image_refs(
    blocks: Option<&[serde_json::Value]>,
) -> (Option<(&str, &str)>, usize) {
    let mut image = None;
    let mut omitted = 0usize;
    for block in blocks.unwrap_or_default() {
        let fields = block
            .get("type")
            .and_then(serde_json::Value::as_str)
            .filter(|kind| *kind == "image")
            .and_then(|_| {
                block
                    .get("mime_type")
                    .and_then(serde_json::Value::as_str)
                    .zip(block.get("data").and_then(serde_json::Value::as_str))
            });
        if image.is_none()
            && let Some((mime_type, data)) = fields
            && valid_tool_image(mime_type, data)
        {
            image = Some((mime_type, data));
        } else {
            omitted += 1;
        }
    }
    (image, omitted)
}

/// Copy/export projection that retains metadata but never inline base64.
#[must_use]
pub fn safe_tool_result_content_blocks(
    blocks: Option<&[serde_json::Value]>,
) -> Option<Vec<serde_json::Value>> {
    blocks.map(|blocks| {
        blocks
            .iter()
            .map(|block| {
                if block.get("type").and_then(serde_json::Value::as_str) == Some("image") {
                    serde_json::json!({
                        "type": "image",
                        "mime_type": block.get("mime_type").and_then(serde_json::Value::as_str).unwrap_or("application/octet-stream"),
                        "omission_code": "inline_or_local_image_payload",
                        "omitted_base64_bytes": block.get("data").and_then(serde_json::Value::as_str).map_or(0, str::len),
                    })
                } else {
                    block.clone()
                }
            })
            .collect()
    })
}

#[must_use]
pub fn safe_tool_result_message_projection(
    messages: &[codewhale_models::Message],
) -> Vec<codewhale_models::Message> {
    let mut projected = messages.to_vec();
    for message in &mut projected {
        for block in &mut message.content {
            if let ContentBlock::ToolResult { content_blocks, .. } = block {
                *content_blocks = safe_tool_result_content_blocks(content_blocks.as_deref());
            }
        }
    }
    projected
}

impl AttachedImage {
    /// The content block this image becomes in a message.
    #[must_use]
    pub fn content_block(&self) -> ContentBlock {
        ContentBlock::ImageUrl {
            image_url: ImageUrlContent {
                url: self.data_url.clone(),
            },
        }
    }
}

/// Identify an image format from its leading bytes.
///
/// Extension sniffing is not enough here: the extension is attacker- and
/// typo-controlled, while what the provider validates is the payload. A
/// `.png` holding JPEG bytes must be declared `image/jpeg` or the request is
/// rejected with a media-type mismatch that reads like a CodeWhale bug.
///
/// Returns `None` for anything that is not one of the four accepted formats;
/// [`detect_rejected_format`] names the near-misses so the error can be
/// specific.
#[must_use]
pub fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Name a format we can recognize but deliberately refuse.
///
/// These are real images, so `NotAnImage` would be a lie and would send the
/// user looking for a corrupt file. Naming the format points at the fix
/// (convert it) instead.
#[must_use]
pub fn detect_rejected_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"BM") {
        return Some("BMP");
    }
    if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
        return Some("TIFF");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"\0\0\0") && &bytes[4..8] == b"ftyp" {
        return Some("HEIC/AVIF");
    }
    if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        return Some("SVG");
    }
    if bytes.starts_with(b"%PDF") {
        return Some("PDF");
    }
    None
}

/// Validate and encode raw bytes that were read from `path`.
///
/// Split from [`attach_image_from_path`] so the whole policy — order of
/// checks, limits, format verdicts — is testable without touching a
/// filesystem.
pub fn encode_image_bytes(bytes: &[u8], path: &str) -> Result<AttachedImage, ImageAttachError> {
    if bytes.is_empty() {
        return Err(ImageAttachError::Empty {
            path: path.to_string(),
        });
    }
    // Size is checked before format so a huge file is rejected on the cheap
    // fact rather than after we have decided what it is.
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(ImageAttachError::TooLarge {
            path: path.to_string(),
            bytes: bytes.len(),
            limit: MAX_IMAGE_BYTES,
        });
    }
    let Some(media_type) = sniff_media_type(bytes) else {
        return Err(format_error(bytes, path));
    };
    let payload = STANDARD.encode(bytes);
    Ok(AttachedImage {
        media_type,
        data_url: format!("data:{media_type};base64,{payload}"),
        source_bytes: bytes.len(),
    })
}

fn format_error(bytes: &[u8], path: &str) -> ImageAttachError {
    match detect_rejected_format(bytes) {
        Some(detected) => ImageAttachError::UnsupportedFormat {
            path: path.to_string(),
            detected: detected.to_string(),
        },
        None => ImageAttachError::NotAnImage {
            path: path.to_string(),
        },
    }
}

/// Longest edge an attached image is sent at. A Retina screenshot is 3–6k px
/// and often over [`MAX_IMAGE_BYTES`] as PNG; larger images are downscaled
/// and re-encoded when attached rather than refused.
pub const ATTACH_MAX_EDGE_PX: u32 = 2048;

/// Read, validate and encode an image file.
///
/// Images over [`ATTACH_MAX_EDGE_PX`] or [`MAX_IMAGE_BYTES`] are decoded under
/// the decompression-bomb guard, fitted to the edge and re-encoded on
/// `read_media`'s budget ladder (PNG for flat or alpha content, JPEG for
/// photos). Known limit: an animated GIF that needs downscaling keeps only
/// its first frame. Sources above `read_media`'s source bound are refused.
pub fn attach_image_from_path(path: &Path) -> Result<AttachedImage, ImageAttachError> {
    let display = path.display().to_string();
    let source_limit = crate::tools::read_media::MAX_SOURCE_IMAGE_BYTES;
    // Check the size from metadata first so a multi-gigabyte file is refused
    // without being read into memory.
    if let Ok(meta) = std::fs::metadata(path) {
        let len = meta.len();
        if len > source_limit as u64 {
            return Err(ImageAttachError::TooLarge {
                path: display,
                bytes: usize::try_from(len).unwrap_or(usize::MAX),
                limit: source_limit,
            });
        }
    }
    let bytes = std::fs::read(path).map_err(|error| ImageAttachError::Unreadable {
        path: display.clone(),
        reason: error.to_string(),
    })?;
    if bytes.len() > source_limit {
        return Err(ImageAttachError::TooLarge {
            path: display,
            bytes: bytes.len(),
            limit: source_limit,
        });
    }
    fit_image_bytes(&bytes, path, MAX_IMAGE_BYTES)
}

fn fit_image_bytes(
    bytes: &[u8],
    path: &Path,
    byte_budget: usize,
) -> Result<AttachedImage, ImageAttachError> {
    let display = path.display().to_string();
    let oversized_edge = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some_and(|(width, height)| width.max(height) > ATTACH_MAX_EDGE_PX);
    if bytes.len() <= byte_budget && !oversized_edge {
        return encode_image_bytes(bytes, &display);
    }
    if sniff_media_type(bytes).is_none() {
        return Err(format_error(bytes, &display));
    }
    let unreadable = |reason: String| ImageAttachError::Unreadable {
        path: display.clone(),
        reason,
    };
    let (image, _, _) =
        decode_and_guard_image(bytes).map_err(|error| unreadable(error.to_string()))?;
    let (encoded, _) =
        crate::tools::read_media::fit_and_encode(&image, ATTACH_MAX_EDGE_PX, byte_budget, path)
            .map_err(|error| unreadable(error.to_string()))?;
    encode_image_bytes(&encoded, &display)
}

/// Upper bound on a paste considered as a list of dropped file paths.
const MAX_PASTED_PATHS_BYTES: usize = 16 * 1024;

/// The local image files a pasted string names, or `None` when the paste is
/// anything else and belongs in the composer as text.
///
/// Terminals deliver a drag-and-drop as a paste of the file's path, in
/// whatever spelling the terminal prefers: Terminal.app and iTerm2
/// shell-escape (`/var/folders/…/Screenshot\ 2026-10-04\ at\ 22.25.47.png`),
/// others quote, some hand over a `file://` URL, and several files arrive as
/// one space-separated line. Without this, that path reached the model as
/// prose and the model had no way to look at the picture.
///
/// The paste converts only when *every* path in it is absolute and names an
/// existing file whose bytes are PNG, JPEG, GIF or WebP, so a sentence that
/// merely mentions a path, a relative name, or a pasted non-image stays text.
///
/// Known limits: a path typed or pasted inside prose is not converted (the
/// drop is the gesture, not the mention); on Windows a paste is read as one
/// path verbatim, without shell unescaping.
#[must_use]
pub fn pasted_image_paths(text: &str) -> Option<Vec<PathBuf>> {
    pasted_paths_matching(text, |path| sniff_image_file(path).is_some())
}

/// A local image path leading a submitted message, and the text after it.
///
/// The paste-time check ([`pasted_image_paths`]) only sees a paste that is
/// nothing but paths. A drop can still reach the composer as typed keys, or
/// be followed by the question on the same line before Enter. So at submit a
/// message that *starts* with an existing image path (escaped, quoted, a
/// `file://` URL, or unescaped with spaces) attaches it and keeps the rest as
/// the prompt — the same rule Hermes Agent's `_detect_file_drop` applies.
#[must_use]
pub fn leading_dropped_image(text: &str) -> Option<(PathBuf, String)> {
    leading_path_matching(text, |path| sniff_image_file(path).is_some())
}

fn leading_path_matching(
    text: &str,
    is_image: impl Fn(&Path) -> bool,
) -> Option<(PathBuf, String)> {
    let text = text.trim_start();
    let (line, after) = text.split_once('\n').unwrap_or((text, ""));
    let line = line.trim_end();
    if line.is_empty() || line.len() > MAX_PASTED_PATHS_BYTES {
        return None;
    }
    let bare = line.trim_start_matches(['"', '\'']);
    if !(bare.starts_with('/') || bare.starts_with("file://") || Path::new(bare).is_absolute()) {
        return None;
    }
    let accept = |raw: &str, unescape: bool| {
        let path = pasted_path(raw, unescape);
        (path.is_absolute() && is_image(&path)).then_some(path)
    };
    let found = (|| {
        // A quoted path ends at its closing quote.
        if let Some(quote) = line.chars().next().filter(|c| *c == '"' || *c == '\'')
            && let Some(end) = line[1..].find(quote)
        {
            let end = end + 2;
            return accept(&line[..end], false).map(|path| (path, end));
        }
        // A shell-escaped path ends at its first unescaped space.
        let mut escaped = false;
        let token_end = line
            .char_indices()
            .find(|&(_, ch)| {
                let stop = ch == ' ' && !escaped;
                escaped = ch == '\\' && !escaped;
                stop
            })
            .map_or(line.len(), |(index, _)| index);
        if let Some(path) = accept(&line[..token_end], true) {
            return Some((path, token_end));
        }
        // An unescaped path with spaces: the longest prefix that is a file.
        let mut cuts: Vec<usize> = line.match_indices(' ').map(|(index, _)| index).collect();
        cuts.push(line.len());
        cuts.into_iter()
            .rev()
            .filter(|&cut| cut > token_end)
            .find_map(|cut| accept(&line[..cut], false).map(|path| (path, cut)))
    })()?;
    let (path, end) = found;
    let rest = format!("{}\n{after}", &line[end..]);
    Some((path, rest.trim().to_string()))
}

fn pasted_paths_matching(text: &str, is_image: impl Fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_PASTED_PATHS_BYTES {
        return None;
    }
    let accept = |path: PathBuf| (path.is_absolute() && is_image(&path)).then_some(path);
    // The whole paste as one path first: a dropped file whose name has
    // spaces is one path even when the terminal did not escape it.
    if let Some(path) = accept(pasted_path(text, true)) {
        return Some(vec![path]);
    }
    if cfg!(windows) {
        return None;
    }
    // Several dropped files: shlex has already removed quotes and escapes.
    let paths = shlex::split(text)?
        .iter()
        .map(|token| accept(pasted_path(token, false)))
        .collect::<Option<Vec<_>>>()?;
    (!paths.is_empty()).then_some(paths)
}

/// One pasted path spelling (quoted, shell-escaped, or a `file://` URL) as
/// a filesystem path. `unescape` is false for a token shlex already split.
fn pasted_path(raw: &str, unescape: bool) -> PathBuf {
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|quote| {
            raw.strip_prefix(*quote)
                .and_then(|rest| rest.strip_suffix(*quote))
        })
        .unwrap_or(raw);
    if let Ok(url) = url::Url::parse(unquoted)
        && url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        return path;
    }
    if cfg!(windows) || !unescape {
        return PathBuf::from(unquoted);
    }
    let mut unescaped = String::with_capacity(unquoted.len());
    let mut chars = unquoted.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.next()
        {
            unescaped.push(next);
        } else {
            unescaped.push(ch);
        }
    }
    PathBuf::from(unescaped)
}

/// The accepted image format of a regular file, judged by its leading bytes.
fn sniff_image_file(path: &Path) -> Option<&'static str> {
    use std::io::Read as _;
    // stat before open: opening a FIFO (or other special file) a paste named
    // blocks until a writer appears, which would freeze the composer.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut head = Vec::with_capacity(16);
    file.take(16).read_to_end(&mut head).ok()?;
    sniff_media_type(&head)
}

/// Paths of the images the user attached in this session's own prompts.
///
/// Reads only `[Attached image: …]` lines in user-role text — the logged
/// record of what the user put in the composer — and never tool output or
/// model text, so a tool result cannot widen what a read may open. Pure
/// string work; [`resolve_user_attached_image`] does the filesystem half.
#[must_use]
pub fn user_attached_image_references(messages: &[codewhale_models::Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == codewhale_models::Role::User)
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .flat_map(codewhale_core::media_attachment_references)
        .filter(|reference| reference.kind == "image")
        .map(|reference| reference.path)
        .collect()
}

/// Admit `raw` for a read-only image tool when it is exactly a file the user
/// attached (see [`user_attached_image_references`]).
///
/// The workspace boundary keeps the model from wandering the disk; a
/// screenshot the user dropped into the composer from a temp directory is not
/// wandering, and refusing it is what sent a model to OCR and computer-use
/// screenshots instead of looking. Admission stays narrow: the exact file
/// (compared after canonicalization, so `/var` and `/private/var` agree), not
/// its directory, and only while its bytes are an accepted image format.
/// Callers still apply the credential and read deny-list checks.
#[must_use]
pub fn resolve_user_attached_image(references: &[String], raw: &str) -> Option<PathBuf> {
    let requested = Path::new(raw);
    if !requested.is_absolute() {
        return None;
    }
    let requested = std::fs::canonicalize(requested).ok()?;
    let attached = references
        .iter()
        .any(|reference| std::fs::canonicalize(reference).is_ok_and(|path| path == requested));
    (attached && sniff_image_file(&requested).is_some()).then_some(requested)
}

/// Image blocks sent from the latest user prompt onward: this turn's
/// attachments and tool-result images, not ones replayed from history.
#[must_use]
pub fn images_since_last_user_prompt(messages: &[codewhale_models::Message]) -> usize {
    let is_prompt = |message: &codewhale_models::Message| {
        message.role == codewhale_models::Role::User
            && message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { .. } | ContentBlock::ImageUrl { .. }
                )
            })
    };
    let start = messages.iter().rposition(is_prompt).unwrap_or(0);
    messages[start..]
        .iter()
        .flat_map(|message| &message.content)
        .map(|block| match block {
            ContentBlock::ImageUrl { .. } => 1,
            ContentBlock::ToolResult { content_blocks, .. } => {
                content_blocks.as_ref().map_or(0, Vec::len)
            }
            _ => 0,
        })
        .sum()
}

/// The outcome of expanding a user turn's attachment placeholders.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExpandedAttachments {
    /// Image blocks to append to the user message, in placeholder order.
    pub blocks: Vec<ContentBlock>,
    /// One line per attachment that could not be sent. These are shown to the
    /// user and also handed to the model, because a model that is not told an
    /// image was dropped will confidently discuss it from the filename.
    pub notices: Vec<String>,
}

/// Build the image blocks for a user turn from its `[Attached image: …]` lines.
///
/// This is the ingest half of the composer's placeholder design: the buffer
/// holds a path-bearing text line (which survives editing, history and session
/// reload for free), and the bytes are read here, once, as the message is
/// built.
///
/// Only *permanent* failures are decided here — a file that is missing,
/// oversized, or not an image will still be all of those things next turn, so
/// baking the verdict into history costs nothing. Whether the *model* can see
/// images is deliberately not decided here; that is contingent on the active
/// route and is re-decided per request by
/// [`strip_images_when_unsupported`].
///
/// Each image is bracketed by text tags naming its path. Without them a turn
/// carrying three screenshots gives the model three anonymous images in a row
/// and no way to say which is which.
///
/// Never returns an error: a turn with one bad attachment should still be
/// sent, with the failure stated in-band rather than swallowed.
#[must_use]
pub fn expand_attachment_blocks(text: &str) -> ExpandedAttachments {
    let references = codewhale_core::media_attachment_references(text);
    let mut out = ExpandedAttachments::default();
    for reference in references {
        if reference.kind != "image" {
            // Video and any future kind: left as the text reference it
            // already was. Silently ignoring it here is not a drop — the
            // path is still in the prompt, exactly as before this module
            // existed.
            continue;
        }
        match attach_image_from_path(Path::new(&reference.path)) {
            Ok(image) => {
                out.blocks
                    .push(tag_block(&format!("<image path=\"{}\">", reference.path)));
                out.blocks.push(image.content_block());
                out.blocks.push(tag_block("</image>"));
            }
            Err(error) => out.notices.push(error.to_string()),
        }
    }
    out
}

fn tag_block(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

/// True for the `<image path="…">` / `</image>` text blocks that
/// [`expand_attachment_blocks`] brackets an attachment with.
///
/// They exist so the model can tell three screenshots apart, not because a
/// person typed them: the turn record keeps the prompt block that named
/// `[Attached image: …]` and nothing else. Readers that rebuild "what the user
/// said" from a saved transcript skip them, so a prompt that carried a
/// screenshot still matches the turn that recorded it.
#[must_use]
pub fn is_attachment_tag_block(block: &ContentBlock) -> bool {
    let ContentBlock::Text { text, .. } = block else {
        return false;
    };
    let text = text.trim();
    text == "</image>" || (text.starts_with("<image path=\"") && text.ends_with("\">"))
}

/// Replace every image in a request with text when the route cannot see them.
///
/// Capability is a property of the *route*, not of the attachment, and the
/// route changes freely mid-session. Deciding this when the message is built
/// would burn the answer into history: attach a screenshot while a text-only
/// model is selected, switch to a vision model, and the image would be gone
/// for good. So history always keeps the real image and each outbound request
/// is normalized against the model it is actually going to.
///
/// Only a known `Unsupported` strips. Most routes report `Unknown` because
/// models.dev has no modality data for them, and treating unknown as "no"
/// would make the feature dead on arrival for exactly the self-hosted and
/// custom routes that most need it — so `Unknown` sends the image and lets the
/// provider be the authority.
///
/// The image is replaced in place rather than removed, so the model is told
/// why it is looking at a gap instead of being left to invent one.
pub fn strip_images_when_unsupported(
    messages: &mut [codewhale_models::Message],
    vision: SupportState,
    model: &str,
) -> usize {
    if vision != SupportState::Unsupported {
        return 0;
    }
    let mut stripped = 0;
    for message in messages.iter_mut() {
        for block in &mut message.content {
            match block {
                ContentBlock::ImageUrl { .. } => {
                    *block = ContentBlock::Text {
                        text: format!(
                            "[image content omitted: the active model ({model}) does \
                             not accept image input. Use the image_ocr tool to read \
                             text from it, or switch to a vision-capable model with \
                             /model.]"
                        ),
                        cache_control: None,
                    };
                    stripped += 1;
                }
                ContentBlock::ToolResult {
                    content,
                    content_blocks,
                    ..
                } => {
                    let count = content_blocks.as_ref().map_or(0, Vec::len);
                    if count > 0 {
                        *content_blocks = None;
                        *content = format!(
                            "{content}\n[{count} image block(s) omitted: the active model ({model}) does not accept image input. Use the image_ocr tool to read text from it, or switch to a vision-capable model with /model.]"
                        );
                        stripped += count;
                    }
                }
                _ => {}
            }
        }
    }
    stripped
}

/// Total inline-image byte budget for one compaction retry that follows a
/// provider body-size rejection.
///
/// An HTTP 413 boundary caps the request *body*, and the token-side context
/// budget that governs compaction cannot see it: an image that costs a flat
/// token estimate can still spend megabytes of base64. Nothing consults this
/// budget on the happy path — it exists only to recover a summary call that
/// has already been refused, where the cheapest useful move is to re-encode
/// the inline images under a cap at or below the smallest provider body limits
/// seen in practice and retry.
pub const COMPACTION_IMAGE_TOTAL_BUDGET_BYTES: usize = 2 * 1024 * 1024;

/// Smallest per-image share of that budget, so an image-heavy session still
/// re-encodes each image instead of dividing the budget down to nothing.
const COMPACTION_IMAGE_MIN_BUDGET_BYTES: usize = 96 * 1024;

/// Longest edge a re-encoded inline image keeps, in pixels.
const COMPACTION_IMAGE_MAX_EDGE: u32 = 1024;

/// Longest edge the shrink ladder descends to before giving up on an image.
const COMPACTION_IMAGE_MIN_EDGE: u32 = 128;

/// JPEG quality for re-encoded images that carry no meaningful alpha.
const COMPACTION_IMAGE_JPEG_QUALITY: u8 = 80;

/// What one inline-image shrink pass changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShrunkInlineImages {
    /// Images that were re-encoded smaller.
    pub images: usize,
    /// Inline images the request carried, rewritten or not.
    ///
    /// `images == 0` alone cannot tell "nothing to do because every image
    /// already fits" from "there were no images"; a request-size ladder must
    /// not conflate the two, because only the second makes the next rung
    /// pointless.
    pub images_seen: usize,
    /// Decoded bytes of those images before the pass.
    pub bytes_before: usize,
    /// Decoded bytes of those images after the pass.
    pub bytes_after: usize,
}

/// Re-encode every inline image in `messages` under a total byte budget.
///
/// Covers both carriers an outbound request can hold: an `image_url` block,
/// and the stored tool-result shape (`{"type":"image","mime_type","data"}`)
/// that the wire projection reads back out via
/// [`provider_tool_result_image_refs`].
///
/// `images == 0` means nothing was rewritten — either there were no inline
/// images, or each was already under its share of the budget. Check
/// [`ShrunkInlineImages::images_seen`] to tell those apart: a caller that is
/// still looking at a body-size rejection must climb to the next rung rather
/// than resend the same bytes, and when images are present but nothing was
/// rewritten, the next rung is the only one that can still change the payload.
pub fn shrink_images_for_request(messages: &mut [codewhale_models::Message]) -> ShrunkInlineImages {
    shrink_images_for_request_with_budget(messages, COMPACTION_IMAGE_TOTAL_BUDGET_BYTES)
}

/// Budget-parameterized core of [`shrink_images_for_request`], kept separate
/// so tests can drive it with small budgets and small images.
pub fn shrink_images_for_request_with_budget(
    messages: &mut [codewhale_models::Message],
    budget: usize,
) -> ShrunkInlineImages {
    let sizes = inline_image_sizes(messages);
    let total: usize = sizes.iter().sum();
    let images_seen = sizes.len();
    if total <= budget {
        // Images may be present and simply already fit: report them as seen
        // without rewriting, so a caller can tell "nothing to shrink" from
        // "nothing there".
        return ShrunkInlineImages {
            images_seen,
            ..ShrunkInlineImages::default()
        };
    }
    // A share of the budget per image, floored so a few large images still come
    // back readable. With many images the floor can push the total past
    // `budget`; the caller's next rung (replace with notes) covers that case,
    // not a tighter share here.
    let per_image = (budget / sizes.len()).max(COMPACTION_IMAGE_MIN_BUDGET_BYTES);
    let mut outcome = ShrunkInlineImages::default();
    for message in messages.iter_mut() {
        for block in &mut message.content {
            match block {
                ContentBlock::ImageUrl { image_url } => {
                    if let Some((url, shrunk)) = shrink_data_url(&image_url.url, per_image) {
                        image_url.url = url;
                        outcome.images += 1;
                        outcome.bytes_before += shrunk.before;
                        outcome.bytes_after += shrunk.after;
                    }
                }
                ContentBlock::ToolResult { content_blocks, .. } => {
                    let Some(blocks) = content_blocks.as_mut() else {
                        continue;
                    };
                    for value in blocks.iter_mut() {
                        if let Some(shrunk) = shrink_stored_tool_image(value, per_image) {
                            outcome.images += 1;
                            outcome.bytes_before += shrunk.before;
                            outcome.bytes_after += shrunk.after;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    outcome.images_seen = images_seen;
    outcome
}

/// Replace every inline image with a text note that keeps the image's
/// existence visible without its bytes.
///
/// This is the last rung of the request-size ladder. A summary pass that still
/// exceeds the provider's body cap after re-encoding has nothing left to give
/// but the pixels; the note keeps the fact that a tool or the user supplied an
/// image — and how large it was — so the handoff can still say so instead of
/// silently dropping the detail. Session history keeps the real image; this
/// only rewrites the outbound copy.
///
/// Returns the number of images replaced.
pub fn replace_images_with_placeholders(
    messages: &mut [codewhale_models::Message],
    reason: &str,
) -> usize {
    let mut replaced = 0;
    for message in messages.iter_mut() {
        for block in &mut message.content {
            match block {
                ContentBlock::ImageUrl { image_url } => {
                    let bytes = parse_data_url(&image_url.url)
                        .map(|(_, payload)| decoded_len_estimate(payload));
                    *block = ContentBlock::Text {
                        text: image_placeholder_note(1, bytes, reason),
                        cache_control: None,
                    };
                    replaced += 1;
                }
                ContentBlock::ToolResult {
                    content,
                    content_blocks,
                    ..
                } => {
                    let Some(blocks) = content_blocks.as_ref() else {
                        continue;
                    };
                    let mut count = 0usize;
                    let mut bytes = 0usize;
                    for value in blocks {
                        if let Some((_, payload)) = stored_tool_image_payload(value) {
                            count += 1;
                            bytes += decoded_len_estimate(payload);
                        }
                    }
                    if count == 0 {
                        continue;
                    }
                    // Same shape as `strip_images_when_unsupported`: the
                    // wire projection reads tool images back out of
                    // `content_blocks` and would count a text block placed
                    // there as an omitted image.
                    *content_blocks = None;
                    *content = format!(
                        "{content}\n{}",
                        image_placeholder_note(count, Some(bytes), reason)
                    );
                    replaced += count;
                }
                _ => {}
            }
        }
    }
    replaced
}

/// The in-band note that replaces an inline image byte payload for one
/// summary pass. It tells the summarizer what was there and what to do
/// about it (refer to it as an image; never invent its contents), rather than
/// leaving a silent gap.
fn image_placeholder_note(count: usize, bytes: Option<usize>, reason: &str) -> String {
    let size = bytes.map_or(String::new(), |bytes| format!(" (~{})", human_bytes(bytes)));
    format!(
        "[{count} image(s){size} omitted from this summary pass: {reason}. \
         The image(s) remain in the session. Refer to them only as images the \
         conversation included; do not describe or guess what they showed.]"
    )
}

/// Base64 length to decoded length: four characters carry three bytes.
fn decoded_len_estimate(payload: &str) -> usize {
    payload.len() / 4 * 3
}

/// Decoded-size estimates for every inline image, in request order.
fn inline_image_sizes(messages: &[codewhale_models::Message]) -> Vec<usize> {
    let mut sizes = Vec::new();
    for message in messages {
        for block in &message.content {
            match block {
                ContentBlock::ImageUrl { image_url } => {
                    if let Some((_, payload)) = parse_data_url(&image_url.url) {
                        sizes.push(decoded_len_estimate(payload));
                    }
                }
                ContentBlock::ToolResult { content_blocks, .. } => {
                    if let Some(blocks) = content_blocks.as_ref() {
                        for value in blocks {
                            if let Some((_, payload)) = stored_tool_image_payload(value) {
                                sizes.push(decoded_len_estimate(payload));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    sizes
}

/// The `(mime_type, data)` pair of a stored tool-result image block, if the
/// block is one.
fn stored_tool_image_payload(value: &serde_json::Value) -> Option<(&str, &str)> {
    if value.get("type").and_then(serde_json::Value::as_str) != Some("image") {
        return None;
    }
    let mime_type = value.get("mime_type").and_then(serde_json::Value::as_str)?;
    let data = value.get("data").and_then(serde_json::Value::as_str)?;
    Some((mime_type, data))
}

/// One rewritten image, in decoded bytes.
#[derive(Debug, Clone, Copy)]
struct ShrunkPayload {
    before: usize,
    after: usize,
}

/// Re-encode a `data:` URL image under `budget`, returning the replacement
/// URL plus the byte delta.
fn shrink_data_url(url: &str, budget: usize) -> Option<(String, ShrunkPayload)> {
    let (_, payload) = parse_data_url(url)?;
    let (mime, data, shrunk) = shrink_base64_image(payload, budget)?;
    Some((format!("data:{mime};base64,{data}"), shrunk))
}

/// Re-encode one stored tool-result image block in place.
fn shrink_stored_tool_image(value: &mut serde_json::Value, budget: usize) -> Option<ShrunkPayload> {
    let (_, payload) = stored_tool_image_payload(value)?;
    let (mime, data, shrunk) = shrink_base64_image(payload, budget)?;
    let object = value.as_object_mut()?;
    object.insert("mime_type".to_string(), serde_json::Value::String(mime));
    object.insert("data".to_string(), serde_json::Value::String(data));
    Some(shrunk)
}

/// Re-encode one base64 inline image to fit `budget` decoded bytes.
///
/// `None` means the image already fits, cannot be decoded, or the re-encode
/// came back no smaller — so a caller's "did anything change" tally stays
/// honest about what a retry would actually send.
fn shrink_base64_image(data: &str, budget: usize) -> Option<(String, String, ShrunkPayload)> {
    let bytes = STANDARD.decode(data).ok()?;
    if bytes.len() <= budget {
        return None;
    }
    let (decoded, _, _) = decode_and_guard_image(&bytes).ok()?;
    let (mime, encoded) = reencode_within_budget(decoded, budget)?;
    if encoded.len() >= bytes.len() {
        return None;
    }
    Some((
        mime.to_string(),
        STANDARD.encode(&encoded),
        ShrunkPayload {
            before: bytes.len(),
            after: encoded.len(),
        },
    ))
}

/// Encode `image` down a short ladder until it fits `budget`.
///
/// Rung order: longest edge capped at [`COMPACTION_IMAGE_MAX_EDGE`], then
/// halved until [`COMPACTION_IMAGE_MIN_EDGE`]. Alpha-bearing images stay PNG
/// (JPEG would flatten transparency); everything else becomes JPEG, which is
/// what actually makes screenshots and artwork small. The smallest rung wins
/// even when nothing fits `budget`, because a smaller-than-before payload is
/// still progress for the retry.
fn reencode_within_budget(image: DynamicImage, budget: usize) -> Option<(&'static str, Vec<u8>)> {
    let mut current = image;
    let (mut width, mut height) = current.dimensions();
    if width.max(height) > COMPACTION_IMAGE_MAX_EDGE {
        let scale = f64::from(COMPACTION_IMAGE_MAX_EDGE) / f64::from(width.max(height));
        let scaled_width = ((f64::from(width) * scale).round() as u32).max(1);
        let scaled_height = ((f64::from(height) * scale).round() as u32).max(1);
        current = current.resize(scaled_width, scaled_height, FilterType::Lanczos3);
        (width, height) = current.dimensions();
    }
    let mut smallest: Option<(&'static str, Vec<u8>)> = None;
    loop {
        if let Some(candidate) = encode_inline_image(&current)
            && smallest
                .as_ref()
                .is_none_or(|(_, best)| candidate.1.len() < best.len())
        {
            smallest = Some(candidate);
        }
        if smallest
            .as_ref()
            .is_some_and(|(_, bytes)| bytes.len() <= budget)
        {
            break;
        }
        if width.max(height) <= COMPACTION_IMAGE_MIN_EDGE {
            break;
        }
        (width, height) = ((width / 2).max(1), (height / 2).max(1));
        current = current.resize(width, height, FilterType::Lanczos3);
    }
    smallest
}

/// One encoder pass: PNG when alpha matters, JPEG otherwise.
fn encode_inline_image(image: &DynamicImage) -> Option<(&'static str, Vec<u8>)> {
    let (width, height) = image.dimensions();
    let mut bytes = Vec::new();
    if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        PngEncoder::new_with_quality(&mut bytes, CompressionType::Best, PngFilter::Adaptive)
            .write_image(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
            .ok()?;
        Some(("image/png", bytes))
    } else {
        let rgb = image.to_rgb8();
        JpegEncoder::new_with_quality(&mut bytes, COMPACTION_IMAGE_JPEG_QUALITY)
            .write_image(rgb.as_raw(), width, height, ExtendedColorType::Rgb8)
            .ok()?;
        Some(("image/jpeg", bytes))
    }
}

/// Render dropped-attachment notices as a block the model will read.
///
/// Wrapped in a tag rather than appended as bare prose so the model can tell
/// the difference between the user saying something and the harness reporting
/// on itself.
#[must_use]
pub fn notice_block(notices: &[String]) -> Option<ContentBlock> {
    if notices.is_empty() {
        return None;
    }
    let body = notices.join("\n");
    Some(ContentBlock::Text {
        text: format!(
            "<attachment_notice>\n{body}\nDo not describe these images from \
             memory or from their filenames; ask the user to re-share them.\n\
             </attachment_notice>"
        ),
        cache_control: None,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
#[path = "image_attach/tests.rs"]
mod tests;
