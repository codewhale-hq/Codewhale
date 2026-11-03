//! `read_media` tool — safe, first-class image reading and preprocessing.
//!
//! Provides bounded decoding, decompression-bomb protection, active route
//! vision-capability checks, crop region extraction, resolution detail modes,
//! and typed receipt metadata without leaking credentials.
//!
//! Delivery is byte-budget-first: after an optional crop and the detail-mode
//! edge cap, the image descends an encoding ladder (JPEG quality steps for
//! photo-like content, PNG for alpha-bearing or flat/line-art content, then
//! longest-edge halving) until the payload fits the budget. Results carry a
//! delivery note stating exactly how the image was delivered (untouched /
//! downsampled / crop / full) with zoom guidance, and the pre-compression
//! original is persisted in the content-addressed [`codewhale_runtime::media_originals`]
//! store so a later crop read can pull the full-resolution source. When no
//! ladder result fits the budget the tool fails closed — nothing is sent —
//! with the exact conversion command to retry with.

#[cfg(test)]
use image::ImageReader;
#[cfg(test)]
use std::io::Cursor;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use codewhale_config::route::CapabilityState;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, GenericImageView, ImageEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, RichToolResult, ToolCapability, ToolContext, ToolError, ToolResult,
    ToolResultContentBlock, ToolSpec, optional_str, required_str, type_mismatch,
};

/// Maximum source image size before decoding (20 MiB).
pub const MAX_SOURCE_IMAGE_BYTES: usize = 20 * 1024 * 1024;

use crate::image_attach::decode_and_guard_image;

/// Maximum inline image payload admitted on the wire (5 MiB).
pub const MAX_WIRE_IMAGE_BYTES: usize = crate::image_attach::MAX_IMAGE_BYTES;

/// Delivery byte budget for a default read (`detail` `low`/`auto`), 256 KiB.
///
/// Matches the reference budget in kimi-code's `READ_IMAGE_BYTE_BUDGET`: after
/// base64 inflation this is ~350 KB on the wire — far under the 5 MiB provider
/// ceiling — and it keeps routine reads from spending megabytes of context on
/// PNG screenshots when a JPEG ladder step fits. `high`/`original` detail and
/// crop (deliberate zoom) reads keep the full [`MAX_WIRE_IMAGE_BYTES`] budget.
pub const READ_IMAGE_BYTE_BUDGET: usize = 256 * 1024;

/// JPEG quality steps tried highest-first at each ladder rung (mirrors
/// kimi-code's `JPEG_QUALITY_STEPS`).
const JPEG_QUALITY_LADDER: [u8; 4] = [80, 60, 40, 20];

/// Longest-edge floor for ladder halving (px); below this the ladder fails
/// closed rather than delivering an unreadable thumbnail.
const MIN_DELIVERY_EDGE_PX: u32 = 256;

/// Flat/line-art images keep PNG rungs down to this longest edge before the
/// ladder falls back to JPEG (mirrors kimi-code's `PNG_RESCALE_FLOOR_PX`).
const PNG_EDGE_FLOOR_PX: u32 = 1000;

/// Opaque images whose colorfulness sigma (spread of the `r-g` and
/// `(r+g)/2-b` opponent channels, measured on a ≤128 px thumbnail) falls below
/// this threshold are treated as flat/line-art and stay PNG; above it the
/// image is photo-like and takes the JPEG ladder. Solid fills and line art
/// score ~0; ordinary photos score well above. Heuristic, deliberately simple.
const FLAT_COLORFULNESS_THRESHOLD: f64 = 12.0;

/// Resolution/detail preference for image processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailMode {
    #[default]
    Auto,
    Low,
    High,
    Original,
}

impl DetailMode {
    fn from_str_opt(s: Option<&str>) -> Result<Self, ToolError> {
        match s {
            None | Some("auto") => Ok(Self::Auto),
            Some("low") => Ok(Self::Low),
            Some("high") => Ok(Self::High),
            Some("original") | Some("full") => Ok(Self::Original),
            Some(other) => Err(ToolError::invalid_input(format!(
                "invalid detail mode '{other}'; expected 'auto', 'low', 'high', or 'original'"
            ))),
        }
    }

    fn max_dimension(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Auto => 2048,
            Self::High | Self::Original => 4096,
        }
    }

    /// Delivery byte budget for a full (non-crop) read at this detail level.
    /// Crop reads are the deliberate zoom action and always get the full wire
    /// budget, matching kimi-code's `cropImageForModel` default.
    fn byte_budget(self) -> usize {
        match self {
            Self::Low | Self::Auto => READ_IMAGE_BYTE_BUDGET,
            Self::High | Self::Original => MAX_WIRE_IMAGE_BYTES,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::High => "high",
            Self::Original => "original",
        }
    }
}

/// Optional bounding box for cropping an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropRegion {
    fn parse_from_value(value: Option<&Value>) -> Result<Option<Self>, ToolError> {
        let Some(val) = value else {
            return Ok(None);
        };
        if val.is_null() {
            return Ok(None);
        }
        let obj = val
            .as_object()
            .ok_or_else(|| type_mismatch("crop", val, "an object with x, y, width, and height"))?;

        let extract_u32 = |field: &str| -> Result<u32, ToolError> {
            let num_val = obj.get(field).ok_or_else(|| {
                ToolError::invalid_input(format!("crop missing required field '{field}'"))
            })?;
            if let Some(n) = num_val.as_u64() {
                u32::try_from(n).map_err(|_| {
                    ToolError::invalid_input(format!(
                        "crop field '{field}' is out of range for u32"
                    ))
                })
            } else if let Some(n) = num_val.as_i64() {
                if n < 0 {
                    return Err(ToolError::invalid_input(format!(
                        "crop field '{field}' must be non-negative; got {n}"
                    )));
                }
                u32::try_from(n).map_err(|_| {
                    ToolError::invalid_input(format!(
                        "crop field '{field}' is out of range for u32"
                    ))
                })
            } else {
                Err(type_mismatch(
                    &format!("crop.{field}"),
                    num_val,
                    "an integer",
                ))
            }
        };

        let x = extract_u32("x")?;
        let y = extract_u32("y")?;
        let width = extract_u32("width")?;
        let height = extract_u32("height")?;

        if width == 0 || height == 0 {
            return Err(ToolError::invalid_input(
                "crop width and height must be greater than 0",
            ));
        }

        Ok(Some(Self {
            x,
            y,
            width,
            height,
        }))
    }
}

/// The first-class `read_media` tool.
pub struct ReadMediaTool;

impl Default for ReadMediaTool {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ToolSpec for ReadMediaTool {
    fn name(&self) -> &'static str {
        "read_media"
    }

    fn description(&self) -> &'static str {
        "Read an image file (PNG, JPEG, GIF, WebP) into context for multimodal/vision inspection, with optional crop region and detail level. Safe, bounded decode with pixel and byte guards."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file (relative to workspace or absolute). PNG, JPEG, GIF, and WebP are supported."
                },
                "crop": {
                    "type": "object",
                    "description": "Optional bounding box to crop [x, y, width, height] in pixel coordinates.",
                    "properties": {
                        "x": {
                            "type": "integer",
                            "description": "Left coordinate (X) of the crop region in pixels (0-based)."
                        },
                        "y": {
                            "type": "integer",
                            "description": "Top coordinate (Y) of the crop region in pixels (0-based)."
                        },
                        "width": {
                            "type": "integer",
                            "description": "Width of the crop region in pixels (must be > 0)."
                        },
                        "height": {
                            "type": "integer",
                            "description": "Height of the crop region in pixels (must be > 0)."
                        }
                    },
                    "required": ["x", "y", "width", "height"]
                },
                "detail": {
                    "type": "string",
                    "enum": ["auto", "low", "high", "original"],
                    "description": "Resolution/detail preference. 'auto' (default) downscales large images to max 2048px; 'low' to max 1024px; 'high' / 'original' preserves resolution up to max 4096px within the 5 MiB payload limit."
                }
            },
            "required": ["path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Sandboxable]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    fn defer_loading(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        self.execute_rich(input, context)
            .await
            .map(RichToolResult::into_result)
    }

    async fn execute_rich(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        execute_read_media(input, context).await
    }
}

/// Execute the `read_media` tool logic.
pub async fn execute_read_media(
    input: Value,
    context: &ToolContext,
) -> Result<RichToolResult, ToolError> {
    // 0. Check cancellation early (before any path resolution, capability checks, or I/O)
    if context
        .cancel_token
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(ToolError::cancelled("Operation aborted"));
    }

    let path_str = required_str(&input, "path")?;
    let detail_str = optional_str(&input, "detail")?;
    let detail_mode = DetailMode::from_str_opt(detail_str)?;
    let crop_region = CropRegion::parse_from_value(input.get("crop"))?;

    // 1. Check active route vision capability
    if context.route_capabilities.image_input == CapabilityState::Unsupported {
        return Err(ToolError::execution_failed(
            "read_media: the active model route does not support image input. Switch to a route marked vision-capable with /model, or configure the route's image_input capability, then try again.",
        ));
    }

    // 2. Resolve path and protect credentials (including symlink/canonicalization escapes)
    // S1/F2: the deny-list check on the caller's raw spelling runs BEFORE
    // `resolve_path` — resolution canonicalizes a workspace symlink to the
    // secret's real location, and an error raised only afterwards would name
    // that location. `read_guard::check` canonicalizes internally, so the raw
    // spelling still matches by target; the resolved check below remains as
    // defense in depth.
    crate::tools::file::enforce_read_denylist(std::path::Path::new(path_str), "read_media")?;
    let file_path = match context.resolve_path(path_str) {
        Ok(path) => path,
        Err(primary_err) => {
            // Read-back fallback for tool-owned stored originals: the store
            // only holds image bytes that already passed the guards above,
            // named by content hash, so admitting it widens nothing.
            let stored = context
                .runtime
                .media_originals_dir
                .as_deref()
                .and_then(|dir| {
                    codewhale_runtime::media_originals::resolve_stored_original(
                        path_str,
                        &context.workspace,
                        dir,
                    )
                });
            match stored {
                Some(path) => path,
                // An image the user attached from outside the workspace
                // (a dropped screenshot in a temp directory) stays viewable.
                None if matches!(primary_err, ToolError::PathEscape { .. }) => {
                    crate::tools::file::user_attached_image_read_path(
                        context,
                        path_str,
                        "read_media",
                    )
                    .await?
                    .ok_or(primary_err)?
                }
                None => return Err(primary_err),
            }
        }
    };
    if crate::tools::file::is_codewhale_credential_path(&file_path) {
        return Err(ToolError::permission_denied(
            "read_media cannot read Codewhale configuration or credential-store files; use `codewhale config list` or `codewhale auth status` for safe inspection",
        ));
    }
    crate::tools::file::enforce_read_denylist(&file_path, "read_media")?;

    // Check cancellation immediately before dispatching blocking I/O and decode work
    if context
        .cancel_token
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(ToolError::cancelled("Operation aborted"));
    }

    let file_path_clone = file_path.clone();
    let originals_dir = context.runtime.media_originals_dir.clone();
    let processed = tokio::task::spawn_blocking(move || {
        process_media_file(&file_path_clone, crop_region, detail_mode, originals_dir)
    })
    .await
    .map_err(|join_err| {
        ToolError::execution_failed(format!("read_media task failed: {join_err}"))
    })??;

    // Check cancellation after await
    if context
        .cancel_token
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(ToolError::cancelled("Operation aborted"));
    }

    // 10. Construct receipt and typed metadata without credentials
    context.note_file_read(&file_path);

    let crop_summary = if let Some(crop) = crop_region {
        format!(
            "region [x: {}, y: {}, w: {}, h: {}]",
            crop.x, crop.y, crop.width, crop.height
        )
    } else {
        "none".to_string()
    };

    let delivery_note = build_delivery_note(&processed, crop_region);

    let mut content_text = format!(
        "Read media file: {path_str} [{delivered_mime}]\n\
         Original dimensions: {orig_width}x{orig_height} ({mime_type})\n\
         Processed dimensions: {final_width}x{final_height}\n\
         Crop: {crop_summary}\n\
         Detail: {}\n\
         Delivery: {delivery_note}\n\
         Size: {} (source) -> {} (wire payload)",
        detail_mode.as_str(),
        human_bytes(processed.source_bytes),
        human_bytes(processed.encoded_bytes),
        delivered_mime = processed.delivered_mime,
        orig_width = processed.orig_width,
        orig_height = processed.orig_height,
        mime_type = processed.source_mime,
        final_width = processed.final_width,
        final_height = processed.final_height,
    );
    if context.route_capabilities.image_input == CapabilityState::Unknown {
        content_text.push_str(
            "\nRoute image support is unverified; if the model cannot see this image, use image_ocr.",
        );
    }

    let metadata_json = json!({
        "path": path_str,
        "mime_type": processed.delivered_mime,
        "original_width": processed.orig_width,
        "original_height": processed.orig_height,
        "original_mime_type": processed.source_mime,
        "width": processed.final_width,
        "height": processed.final_height,
        "cropped": processed.crop_applied,
        "crop": crop_region.map(|c| json!({
            "x": c.x,
            "y": c.y,
            "width": c.width,
            "height": c.height
        })),
        "detail": detail_mode.as_str(),
        "delivery": processed.delivery.as_str(),
        "original_path": processed
            .original_path
            .as_ref()
            .map(|p| p.display().to_string()),
        "source_bytes": processed.source_bytes,
        "encoded_bytes": processed.encoded_bytes
    });

    Ok(RichToolResult::with_content_blocks(
        ToolResult::success(content_text).with_metadata(metadata_json),
        vec![ToolResultContentBlock::Image {
            mime_type: processed.delivered_mime.to_string(),
            data: processed.base64_payload,
        }],
    ))
}

/// The delivery-mode sentence of the result note: exactly how the image was
/// delivered, plus zoom/offset guidance. Compression is never silent.
fn build_delivery_note(processed: &ProcessedMedia, crop_region: Option<CropRegion>) -> String {
    let delivered_size = human_bytes(processed.encoded_bytes);
    match processed.delivery {
        DeliveryMode::Untouched => format!(
            "untouched — delivered at native resolution {}x{} ({}, {}); no downsampling applied.",
            processed.final_width, processed.final_height, processed.delivered_mime, delivered_size
        ),
        DeliveryMode::Full => format!(
            "full — shown at native resolution {}x{} ({}, {}); no downscaling applied.",
            processed.final_width, processed.final_height, processed.delivered_mime, delivered_size
        ),
        DeliveryMode::Downsampled { resolution_lost } => {
            let how = if resolution_lost {
                format!(
                    "downsampled to {}x{}",
                    processed.final_width, processed.final_height
                )
            } else {
                format!(
                    "re-encoded at native resolution {}x{}",
                    processed.final_width, processed.final_height
                )
            };
            let mut note = format!(
                "{how} ({}, {delivered_size}) to fit model limits; fine detail may be lost. \
                 To inspect fine detail, call read_media again with the crop parameter \
                 (original-image pixel coordinates) to view a region at full fidelity.",
                processed.delivered_mime
            );
            if let Some(original_path) = &processed.original_path {
                note.push_str(&format!(
                    " The uncompressed original is preserved at \"{}\".",
                    original_path.display()
                ));
            }
            note
        }
        DeliveryMode::Crop { resized } => {
            let region = crop_region.expect("crop delivery implies a crop region");
            format!(
                "crop (x={}, y={}, width={}, height={}) of the original image{} ({}, {}). \
                 To output coordinates in original-image pixels, locate them within this crop \
                 and add the region offset (x={}, y={}).",
                region.x,
                region.y,
                region.width,
                region.height,
                if resized {
                    format!(
                        ", downsampled to {}x{}",
                        processed.final_width, processed.final_height
                    )
                } else {
                    " at native resolution".to_string()
                },
                processed.delivered_mime,
                delivered_size,
                region.x,
                region.y
            )
        }
    }
}

/// How the delivered payload relates to the source image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryMode {
    /// Native resolution, first ladder step, `detail` `low`/`auto`.
    Untouched,
    /// Native resolution, first ladder step, `detail` `high`/`original`.
    Full,
    /// Resolution and/or encoding quality was reduced to fit the budget.
    Downsampled { resolution_lost: bool },
    /// A crop region was delivered; `resized` when the crop itself had to
    /// shrink to fit the budget.
    Crop { resized: bool },
}

impl DeliveryMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Untouched => "untouched",
            Self::Full => "full",
            Self::Downsampled { .. } => "downsampled",
            Self::Crop { .. } => "crop",
        }
    }
}

struct ProcessedMedia {
    base64_payload: String,
    delivered_mime: &'static str,
    orig_width: u32,
    orig_height: u32,
    source_mime: &'static str,
    final_width: u32,
    final_height: u32,
    crop_applied: bool,
    delivery: DeliveryMode,
    source_bytes: usize,
    encoded_bytes: usize,
    original_path: Option<std::path::PathBuf>,
}

fn process_media_file(
    file_path: &std::path::Path,
    crop_region: Option<CropRegion>,
    detail_mode: DetailMode,
    originals_dir: Option<std::path::PathBuf>,
) -> Result<ProcessedMedia, ToolError> {
    // 3. Inspect metadata and check file bounds
    if !file_path.exists() {
        return Err(ToolError::execution_failed(format!(
            "read_media: image file does not exist: {}",
            file_path.display()
        )));
    }

    let meta = std::fs::metadata(file_path).map_err(|e| {
        ToolError::execution_failed(format!(
            "read_media: failed to inspect {}: {e}",
            file_path.display()
        ))
    })?;

    if meta.is_dir() {
        return Err(ToolError::execution_failed(format!(
            "read_media: path is a directory, not an image file: {}",
            file_path.display()
        )));
    }

    let file_len = meta.len();
    if file_len == 0 {
        return Err(ToolError::execution_failed(format!(
            "read_media: image file is empty (0 bytes): {}",
            file_path.display()
        )));
    }

    if file_len > MAX_SOURCE_IMAGE_BYTES as u64 {
        return Err(ToolError::execution_failed(format!(
            "read_media: image file size ({}) exceeds the maximum source limit of {}. Downscale or crop the file first.",
            human_bytes(file_len as usize),
            human_bytes(MAX_SOURCE_IMAGE_BYTES)
        )));
    }

    // 4. Read source bytes
    let raw_bytes = std::fs::read(file_path).map_err(|e| {
        ToolError::execution_failed(format!(
            "read_media: failed to read {}: {e}",
            file_path.display()
        ))
    })?;

    if raw_bytes.is_empty() {
        return Err(ToolError::execution_failed(format!(
            "read_media: image file is empty (0 bytes): {}",
            file_path.display()
        )));
    }

    if raw_bytes.len() > MAX_SOURCE_IMAGE_BYTES {
        return Err(ToolError::execution_failed(format!(
            "read_media: image file size ({}) exceeds the maximum source limit of {}. Downscale or crop the file first.",
            human_bytes(raw_bytes.len()),
            human_bytes(MAX_SOURCE_IMAGE_BYTES)
        )));
    }

    // 5. Sniff format and guard against non-images or rejected formats
    let sniffed_mime = crate::image_attach::sniff_media_type(&raw_bytes);
    let mime_type = match sniffed_mime {
        Some(m) => m,
        None => {
            if let Some(rejected) = crate::image_attach::detect_rejected_format(&raw_bytes) {
                return Err(ToolError::execution_failed(format!(
                    "read_media: {rejected} format is not directly supported by vision models. Convert it to PNG, JPEG, GIF, or WebP first."
                )));
            }
            return Err(ToolError::execution_failed(format!(
                "read_media: file is not a recognized or supported image format (expected PNG, JPEG, GIF, or WebP): {}",
                file_path.display()
            )));
        }
    };

    // 6. Bounded decoding with decompression-bomb guards
    let (processed_image, orig_width, orig_height) = decode_and_guard_image(&raw_bytes)
        .map_err(|error| ToolError::execution_failed(format!("read_media: {error}")))?;

    // 7. Apply crop if requested
    let (cropped_image, crop_applied) = if let Some(crop) = crop_region {
        let crop_right = crop.x.checked_add(crop.width);
        let crop_bottom = crop.y.checked_add(crop.height);
        if crop_right.is_none_or(|right| right > orig_width)
            || crop_bottom.is_none_or(|bottom| bottom > orig_height)
        {
            return Err(ToolError::invalid_input(format!(
                "read_media: crop region [x: {}, y: {}, width: {}, height: {}] is out of bounds for image dimensions {}x{}",
                crop.x, crop.y, crop.width, crop.height, orig_width, orig_height
            )));
        }
        let cropped =
            image::imageops::crop_imm(&processed_image, crop.x, crop.y, crop.width, crop.height)
                .to_image();
        (DynamicImage::ImageRgba8(cropped), true)
    } else {
        (processed_image, false)
    };

    // 8. Apply detail resolution resizing
    let (current_w, current_h) = cropped_image.dimensions();
    let max_target = detail_mode.max_dimension();
    let detail_resized = current_w > max_target || current_h > max_target;
    let fitted_image = if detail_resized {
        cropped_image.resize(max_target, max_target, FilterType::Lanczos3)
    } else {
        cropped_image
    };

    // 9. Byte-budget-first encoding ladder: classify the content, then descend
    // quality/edge rungs until the payload fits the delivery budget. Fails
    // closed (nothing sent) with a conversion recipe when nothing fits.
    let policy = classify_image(&fitted_image);
    let budget = if crop_applied {
        MAX_WIRE_IMAGE_BYTES
    } else {
        detail_mode.byte_budget()
    };
    let outcome = encode_within_budget(&fitted_image, policy, budget, file_path)?;

    // 10. Persist the pre-compression original whenever the delivered copy is
    // degraded, so a later crop read can pull the full-resolution source.
    // Best effort: persistence never blocks delivery.
    let degraded = detail_resized || outcome.resized || outcome.quality_reduced;
    let original_path = if degraded && !crop_applied {
        originals_dir.and_then(|dir| {
            codewhale_runtime::media_originals::persist_original_image(&raw_bytes, mime_type, &dir)
        })
    } else {
        None
    };

    let delivery = if crop_applied {
        DeliveryMode::Crop { resized: degraded }
    } else if degraded {
        DeliveryMode::Downsampled {
            resolution_lost: detail_resized || outcome.resized,
        }
    } else {
        match detail_mode {
            DetailMode::High | DetailMode::Original => DeliveryMode::Full,
            DetailMode::Low | DetailMode::Auto => DeliveryMode::Untouched,
        }
    };

    let source_bytes = raw_bytes.len();
    let encoded_bytes = outcome.bytes.len();
    let base64_payload = BASE64.encode(&outcome.bytes);

    Ok(ProcessedMedia {
        base64_payload,
        delivered_mime: outcome.mime,
        orig_width,
        orig_height,
        source_mime: mime_type,
        final_width: outcome.width,
        final_height: outcome.height,
        crop_applied,
        delivery,
        source_bytes,
        encoded_bytes,
        original_path,
    })
}

/// How the encoding ladder should treat an image's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncodePolicy {
    /// Meaningful alpha present: PNG only. JPEG would silently destroy the
    /// alpha channel, so the ladder fails closed rather than falling back.
    RequireAlphaPng,
    /// Opaque and flat (solid fills, line art, UI screenshots): PNG rungs
    /// first — flat content compresses well losslessly and JPEG rings on hard
    /// edges — then the JPEG ladder below the PNG edge floor.
    PreferPng,
    /// Opaque and photo-like: the JPEG quality ladder immediately. Photos as
    /// PNG waste an order of magnitude of context bytes.
    PreferJpeg,
}

impl EncodePolicy {
    fn prefers_png(self) -> bool {
        !matches!(self, Self::PreferJpeg)
    }
}

/// Content classification for the ladder: alpha first (correctness), then a
/// colorfulness heuristic on a small thumbnail (cost).
fn classify_image(image: &DynamicImage) -> EncodePolicy {
    if has_meaningful_alpha(image) {
        return EncodePolicy::RequireAlphaPng;
    }
    if colorfulness_sigma(image) < FLAT_COLORFULNESS_THRESHOLD {
        EncodePolicy::PreferPng
    } else {
        EncodePolicy::PreferJpeg
    }
}

/// Whether any pixel is actually transparent — not merely whether the decoded
/// layout has an alpha channel (an opaque RGBA PNG decodes to `Rgba8` too).
fn has_meaningful_alpha(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageRgba8(buf) => buf.pixels().any(|p| p.0[3] != u8::MAX),
        DynamicImage::ImageLumaA8(buf) => buf.pixels().any(|p| p.0[1] != u8::MAX),
        DynamicImage::ImageRgba16(buf) => buf.pixels().any(|p| p.0[3] != u16::MAX),
        DynamicImage::ImageLumaA16(buf) => buf.pixels().any(|p| p.0[1] != u16::MAX),
        _ => false,
    }
}

/// Spread of the opponent color channels (Hasler–Süsstrunk sigma terms only —
/// the mean terms would inflate saturated flat fills), measured on a ≤128 px
/// thumbnail for speed. Solid fills and line art score ~0.
fn colorfulness_sigma(image: &DynamicImage) -> f64 {
    let thumb = image.thumbnail(128, 128);
    let rgb = thumb.to_rgb8();
    let n = (rgb.len() / 3) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let (mut sum_rg, mut sum_yb, mut sum_rg2, mut sum_yb2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    // An `Rgb<u8>` buffer is exactly 3 bytes per pixel, so the remainder is
    // always empty; `as_chunks` makes the pixel stride a compile-time fact.
    let (pixels, remainder) = rgb.as_raw().as_chunks::<3>();
    debug_assert!(
        remainder.is_empty(),
        "RGB8 buffer length must be a multiple of 3"
    );
    for &[r, g, b] in pixels {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        let rg = r - g;
        let yb = (r + g) / 2.0 - b;
        sum_rg += rg;
        sum_yb += yb;
        sum_rg2 += rg * rg;
        sum_yb2 += yb * yb;
    }
    let var_rg = (sum_rg2 / n - (sum_rg / n).powi(2)).max(0.0);
    let var_yb = (sum_yb2 / n - (sum_yb / n).powi(2)).max(0.0);
    (var_rg + var_yb).sqrt()
}

/// One encoded ladder candidate.
struct EncodedImage {
    bytes: Vec<u8>,
    mime: &'static str,
    width: u32,
    height: u32,
}

/// The ladder's winning candidate plus how it got there.
#[derive(Debug)]
struct LadderOutcome {
    bytes: Vec<u8>,
    mime: &'static str,
    width: u32,
    height: u32,
    /// The candidate's dimensions differ from the ladder's input image.
    resized: bool,
    /// A JPEG quality step below the ladder's first (80) was needed.
    quality_reduced: bool,
}

fn encode_png_best(image: &DynamicImage) -> Result<EncodedImage, ToolError> {
    let (width, height) = image.dimensions();
    let mut bytes: Vec<u8> = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut bytes, CompressionType::Best, PngFilter::Adaptive);
    if image.color().has_alpha() {
        let rgba = image.to_rgba8();
        encoder
            .write_image(rgba.as_raw(), width, height, ExtendedColorType::Rgba8)
            .map_err(|e| {
                ToolError::execution_failed(format!("read_media: failed to encode PNG: {e}"))
            })?;
    } else {
        let rgb = image.to_rgb8();
        encoder
            .write_image(rgb.as_raw(), width, height, ExtendedColorType::Rgb8)
            .map_err(|e| {
                ToolError::execution_failed(format!("read_media: failed to encode PNG: {e}"))
            })?;
    }
    Ok(EncodedImage {
        bytes,
        mime: "image/png",
        width,
        height,
    })
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<EncodedImage, ToolError> {
    let (width, height) = image.dimensions();
    // Only reached for images without meaningful alpha, so dropping the
    // channel changes nothing visible.
    let rgb = image.to_rgb8();
    let mut bytes: Vec<u8> = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, quality)
        .write_image(rgb.as_raw(), width, height, ExtendedColorType::Rgb8)
        .map_err(|e| {
            ToolError::execution_failed(format!("read_media: failed to encode JPEG: {e}"))
        })?;
    Ok(EncodedImage {
        bytes,
        mime: "image/jpeg",
        width,
        height,
    })
}

/// Half-size Lanczos copy, or `None` once the longest edge is at the floor.
fn halved(image: &DynamicImage) -> Option<DynamicImage> {
    let (w, h) = image.dimensions();
    if w.max(h) <= MIN_DELIVERY_EDGE_PX {
        return None;
    }
    Some(image.resize((w / 2).max(1), (h / 2).max(1), FilterType::Lanczos3))
}

fn track_smallest(smallest: &mut Option<EncodedImage>, candidate: EncodedImage) {
    if smallest
        .as_ref()
        .is_none_or(|s| candidate.bytes.len() < s.bytes.len())
    {
        *smallest = Some(candidate);
    }
}

/// Descend the encoding ladder until a rung fits `budget`.
///
/// Rung order (mirrors kimi-code's `encodeWithinBudget`): PNG-preferring
/// images try best-compression PNG at the current size, then halved PNG rungs
/// down to [`PNG_EDGE_FLOOR_PX`], then the JPEG quality ladder; photo-like
/// images start at the JPEG ladder; alpha-bearing images only ever see PNG
/// rungs. JPEG rungs repeat the quality ladder at each halved edge down to
/// [`MIN_DELIVERY_EDGE_PX`]. The first fitting rung wins (highest fidelity
/// within budget). When nothing fits, fails closed — nothing is sent — with
/// the exact conversion command to retry with.
fn encode_within_budget(
    image: &DynamicImage,
    policy: EncodePolicy,
    budget: usize,
    path: &std::path::Path,
) -> Result<LadderOutcome, ToolError> {
    let (start_w, start_h) = image.dimensions();
    let mut smallest: Option<EncodedImage> = None;
    let mut candidate = image.clone();

    if policy.prefers_png() {
        let png_floor = match policy {
            EncodePolicy::RequireAlphaPng => MIN_DELIVERY_EDGE_PX,
            _ => PNG_EDGE_FLOOR_PX,
        };
        loop {
            let encoded = encode_png_best(&candidate)?;
            if encoded.bytes.len() <= budget {
                let resized = encoded.width != start_w || encoded.height != start_h;
                return Ok(LadderOutcome {
                    bytes: encoded.bytes,
                    mime: encoded.mime,
                    width: encoded.width,
                    height: encoded.height,
                    resized,
                    quality_reduced: false,
                });
            }
            let longest = encoded.width.max(encoded.height);
            track_smallest(&mut smallest, encoded);
            if longest <= png_floor {
                break;
            }
            let Some(next) = halved(&candidate) else {
                break;
            };
            candidate = next;
        }
        if policy == EncodePolicy::RequireAlphaPng {
            return Err(over_budget_error(path, budget, smallest));
        }
    }

    loop {
        for (step, quality) in JPEG_QUALITY_LADDER.iter().enumerate() {
            let encoded = encode_jpeg(&candidate, *quality)?;
            if encoded.bytes.len() <= budget {
                let resized = encoded.width != start_w || encoded.height != start_h;
                return Ok(LadderOutcome {
                    bytes: encoded.bytes,
                    mime: encoded.mime,
                    width: encoded.width,
                    height: encoded.height,
                    resized,
                    quality_reduced: step > 0,
                });
            }
            track_smallest(&mut smallest, encoded);
        }
        let Some(next) = halved(&candidate) else {
            break;
        };
        candidate = next;
    }
    Err(over_budget_error(path, budget, smallest))
}

/// Fit `image` to `max_edge` and encode it within `budget` on the same ladder
/// `read_media` delivers with. Attach-time ingest (`image_attach`) shares it,
/// so a pasted Retina screenshot is normalized exactly like a tool read.
pub fn fit_and_encode(
    image: &DynamicImage,
    max_edge: u32,
    budget: usize,
    path: &std::path::Path,
) -> Result<(Vec<u8>, &'static str), ToolError> {
    let (width, height) = image.dimensions();
    let fitted = if width.max(height) > max_edge {
        image.resize(max_edge, max_edge, FilterType::Lanczos3)
    } else {
        image.clone()
    };
    let outcome = encode_within_budget(&fitted, classify_image(&fitted), budget, path)?;
    Ok((outcome.bytes, outcome.mime))
}

/// Fail-closed error naming the exact conversion recipe to retry with.
fn over_budget_error(
    path: &std::path::Path,
    budget: usize,
    smallest: Option<EncodedImage>,
) -> ToolError {
    let smallest_desc = match &smallest {
        Some(s) => format!(
            "smallest deliverable: {} at {}x{}",
            human_bytes(s.bytes.len()),
            s.width,
            s.height
        ),
        None => "no encodable result".to_string(),
    };
    ToolError::execution_failed(format!(
        "read_media: {path} could not be compressed under the {budget} delivery budget ({smallest_desc}). \
         Nothing was sent to the model; do not retry the same file unchanged. \
         Create a smaller copy and read that instead: \
         `sips -Z 1024 \"{path}\" --out /tmp/codewhale-smaller.png` (macOS) or \
         `magick \"{path}\" -resize 1024x1024 /tmp/codewhale-smaller.png` (ImageMagick); \
         for a photo-like image, JPEG output (`--out /tmp/codewhale-smaller.jpg` / `-quality 70`) \
         compresses further. Then call read_media on the smaller copy.",
        path = path.display(),
        budget = human_bytes(budget),
    ))
}

fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use image::ImageFormat;
    use tempfile::tempdir;

    fn create_test_png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Png).unwrap();
        cursor.into_inner()
    }

    fn create_test_jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([120, 200, 50]));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Jpeg).unwrap();
        cursor.into_inner()
    }

    #[tokio::test]
    async fn read_media_spec_metadata_and_capabilities() {
        let tool = ReadMediaTool;
        assert_eq!(tool.name(), "read_media");
        assert!(tool.capabilities().contains(&ToolCapability::ReadOnly));
        assert!(tool.capabilities().contains(&ToolCapability::Sandboxable));
        assert!(tool.supports_parallel());
        assert!(tool.defer_loading());
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
    }

    #[tokio::test]
    async fn read_media_success_png_roundtrip() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("test.png");
        let png_data = create_test_png(100, 50, [255, 0, 0, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let input = json!({
            "path": "test.png",
            "detail": "auto"
        });

        let rich = tool.execute_rich(input, &ctx).await.unwrap();
        assert!(rich.success);
        assert_eq!(rich.content_blocks.len(), 1);
        let ToolResultContentBlock::Image { mime_type, data } = &rich.content_blocks[0];
        assert_eq!(mime_type, "image/png");
        assert!(!data.is_empty());

        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["path"], "test.png");
        assert_eq!(meta["original_width"], 100);
        assert_eq!(meta["original_height"], 50);
        assert_eq!(meta["width"], 100);
        assert_eq!(meta["height"], 50);
        assert_eq!(meta["cropped"], false);
        assert_eq!(meta["detail"], "auto");
    }

    #[tokio::test]
    async fn read_media_success_jpeg_decoded_and_reencoded_to_png() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("photo.jpg");
        let jpeg_data = create_test_jpeg(80, 60);
        std::fs::write(&img_path, &jpeg_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let input = json!({
            "path": "photo.jpg",
            "detail": "low"
        });

        let rich = tool.execute_rich(input, &ctx).await.unwrap();
        assert!(rich.success);
        let ToolResultContentBlock::Image { mime_type, data } = &rich.content_blocks[0];
        assert_eq!(mime_type, "image/png");
        assert!(!data.is_empty());

        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["original_width"], 80);
        assert_eq!(meta["original_height"], 60);
        assert_eq!(meta["original_mime_type"], "image/jpeg");
    }

    #[tokio::test]
    async fn read_media_crop_region_bounds_and_execution() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("grid.png");
        let png_data = create_test_png(200, 100, [0, 255, 0, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;

        // 1. Valid crop
        let valid_input = json!({
            "path": "grid.png",
            "crop": {
                "x": 10,
                "y": 20,
                "width": 50,
                "height": 40
            }
        });
        let rich = tool.execute_rich(valid_input, &ctx).await.unwrap();
        assert!(rich.success);
        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["cropped"], true);
        assert_eq!(meta["width"], 50);
        assert_eq!(meta["height"], 40);

        // 2. Out-of-bounds crop
        let oob_input = json!({
            "path": "grid.png",
            "crop": {
                "x": 180,
                "y": 20,
                "width": 50,
                "height": 40
            }
        });
        let err = tool.execute_rich(oob_input, &ctx).await.unwrap_err();
        assert!(err.to_string().contains("out of bounds"));

        // 3. Zero-dimension crop
        let zero_input = json!({
            "path": "grid.png",
            "crop": {
                "x": 10,
                "y": 10,
                "width": 0,
                "height": 10
            }
        });
        let err = tool.execute_rich(zero_input, &ctx).await.unwrap_err();
        assert!(err.to_string().contains("greater than 0"));

        // Adversarial coordinates must be rejected rather than wrapping in
        // release builds or panicking in debug builds.
        for overflow_input in [
            json!({
                "path": "grid.png",
                "crop": {
                    "x": u32::MAX,
                    "y": 0,
                    "width": 2,
                    "height": 1
                }
            }),
            json!({
                "path": "grid.png",
                "crop": {
                    "x": 0,
                    "y": u32::MAX,
                    "width": 1,
                    "height": 2
                }
            }),
        ] {
            let err = tool.execute_rich(overflow_input, &ctx).await.unwrap_err();
            assert!(err.to_string().contains("out of bounds"), "{err}");
        }
    }

    #[tokio::test]
    async fn read_media_detail_modes_and_resizing() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("large.png");
        let png_data = create_test_png(3000, 1500, [0, 0, 255, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;
        let tool = ReadMediaTool;

        // Low detail -> max 1024
        let low_input = json!({ "path": "large.png", "detail": "low" });
        let rich_low = tool.execute_rich(low_input, &ctx).await.unwrap();
        let meta_low = rich_low.metadata.as_ref().unwrap();
        assert_eq!(meta_low["width"], 1024);
        assert_eq!(meta_low["height"], 512);

        // Auto detail -> max 2048
        let auto_input = json!({ "path": "large.png", "detail": "auto" });
        let rich_auto = tool.execute_rich(auto_input, &ctx).await.unwrap();
        let meta_auto = rich_auto.metadata.as_ref().unwrap();
        assert_eq!(meta_auto["width"], 2048);
        assert_eq!(meta_auto["height"], 1024);

        // Original detail -> within 4096, keeps 3000x1500
        let orig_input = json!({ "path": "large.png", "detail": "original" });
        let rich_orig = tool.execute_rich(orig_input, &ctx).await.unwrap();
        let meta_orig = rich_orig.metadata.as_ref().unwrap();
        assert_eq!(meta_orig["width"], 3000);
        assert_eq!(meta_orig["height"], 1500);
    }

    #[tokio::test]
    async fn read_media_route_capability_unsupported_fails_actionable() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("img.png");
        let png_data = create_test_png(10, 10, [1, 2, 3, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Unsupported;

        let tool = ReadMediaTool;
        let input = json!({ "path": "img.png" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("active model route does not support image input"));
        assert!(err_msg.contains("/model"));
        assert!(err_msg.contains("image_input"));
        assert!(!err_msg.contains("deepseek-v4-pro"));
    }

    #[tokio::test]
    async fn read_media_missing_file_fails_actionable() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let input = json!({ "path": "nonexistent.png" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        assert!(err.to_string().contains("image file does not exist"));
    }

    #[tokio::test]
    async fn read_media_empty_file_fails_actionable() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("empty.png");
        std::fs::write(&img_path, b"").unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let input = json!({ "path": "empty.png" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        assert!(err.to_string().contains("image file is empty"));
    }

    #[tokio::test]
    async fn read_media_corrupted_or_non_image_fails_actionable() {
        let dir = tempdir().unwrap();
        let bad_path = dir.path().join("corrupted.png");
        std::fs::write(&bad_path, b"not a real image payload at all").unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let input = json!({ "path": "corrupted.png" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("not a recognized or supported image format")
        );
    }

    #[tokio::test]
    async fn read_media_rejected_format_svg_fails_with_conversion_hint() {
        let dir = tempdir().unwrap();
        let svg_path = dir.path().join("vector.svg");
        std::fs::write(&svg_path, b"<svg><circle r='10'/></svg>").unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let input = json!({ "path": "vector.svg" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("SVG format is not directly supported by vision models"));
        assert!(msg.contains("Convert it to PNG, JPEG, GIF, or WebP"));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn read_media_credential_path_is_denied() {
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let _config_path = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

        std::fs::write(tmp.path().join("config.toml"), "api_key = \"secret\"\n")
            .expect("write config");

        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let tool = ReadMediaTool;
        let input = json!({ "path": "config.toml" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot read Codewhale configuration or credential-store"),
            "{}",
            err
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[cfg(unix)]
    #[tokio::test]
    async fn read_media_credential_symlink_escape_is_denied() {
        let _env_lock = crate::test_support::lock_test_env();
        let home_tmp = tempdir().expect("home tempdir");
        let ws_tmp = tempdir().expect("workspace tempdir");
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home_tmp.path());
        let _config_path = crate::test_support::EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = crate::test_support::EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

        let real_config = home_tmp.path().join("config.toml");
        std::fs::write(&real_config, "api_key = \"secret_in_home\"\n").expect("write config");

        // Create a symlink in the workspace pointing to the credentials
        let symlink_path = ws_tmp.path().join("fake_image.png");
        std::os::unix::fs::symlink(&real_config, &symlink_path).expect("create symlink");

        {
            // Case 1: Default context (no symlink follow outside workspace) -> path escape error
            let ctx_default = ToolContext::new(ws_tmp.path().to_path_buf());
            let tool = ReadMediaTool;
            let input = json!({ "path": "fake_image.png" });
            let err_default = tool
                .execute_rich(input.clone(), &ctx_default)
                .await
                .unwrap_err();
            let msg_default = err_default.to_string();
            assert!(
                msg_default.contains("escapes workspace") || msg_default.contains("credential"),
                "default policy must reject symlink outside workspace: {msg_default}"
            );

            // Case 2: follow_symlinks enabled -> credential guard must still deny read
            let ctx_follow =
                ToolContext::new(ws_tmp.path().to_path_buf()).with_follow_symlinks(true);
            let err_follow = tool.execute_rich(input, &ctx_follow).await.unwrap_err();
            assert!(
                err_follow
                    .to_string()
                    .contains("cannot read Codewhale configuration or credential-store"),
                "follow_symlinks policy must catch canonical credential path: {}",
                err_follow
            );
        }
    }

    #[tokio::test]
    async fn read_media_path_escape_is_rejected() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let input = json!({ "path": "../../etc/shadow" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        assert!(
            err.to_string().contains("escapes workspace") || err.to_string().contains("permission"),
            "{}",
            err
        );
    }

    #[tokio::test]
    async fn read_media_respects_cancel_token() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("cancel.png");
        let png_data = create_test_png(10, 10, [1, 2, 3, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();
        let mut ctx = ToolContext::new(dir.path()).with_cancel_token(cancel_token);
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let err = tool
            .execute_rich(json!({ "path": "cancel.png" }), &ctx)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("aborted") || msg.contains("cancel"), "{msg}");
    }

    #[tokio::test]
    async fn read_media_cancellation_before_dispatch_fails_without_read() {
        let dir = tempdir().unwrap();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();
        let mut ctx = ToolContext::new(dir.path()).with_cancel_token(cancel_token);
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        // A non-existent file path would normally fail with "image file does not exist",
        // but when cancelled before dispatch, it must abort with Cancelled without touching disk.
        let input = json!({ "path": "nonexistent_before_dispatch.png" });
        let err = tool.execute_rich(input, &ctx).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("aborted") || msg.contains("cancel"),
            "expected cancellation error before dispatch, got: {msg}"
        );
        assert!(
            !msg.contains("does not exist"),
            "should not reach filesystem checks when cancelled before dispatch"
        );
    }

    #[tokio::test]
    async fn read_media_provider_request_wiring_integration() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("wire.png");
        let png_data = create_test_png(40, 40, [10, 20, 30, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "wire.png" }), &ctx)
            .await
            .unwrap();

        let ToolResultContentBlock::Image { mime_type, data } = &rich.content_blocks[0];

        // 1. Check Anthropic tool result wiring
        let anthropic_content = crate::client::anthropic_tool_result_content_for_test(
            &rich.content,
            Some(&[json!({
                "type": "image",
                "mime_type": mime_type,
                "data": data
            })]),
        );
        let blocks = anthropic_content
            .as_array()
            .expect("anthropic content array");
        assert!(
            blocks
                .iter()
                .any(|b| b["type"] == "image" && b["source"]["media_type"] == "image/png")
        );

        // 2. Check OpenAI Responses tool output wiring
        let responses_content = crate::client::responses_tool_output_for_test(
            &rich.content,
            Some(&[json!({
                "type": "image",
                "mime_type": mime_type,
                "data": data
            })]),
        );
        let resp_blocks = responses_content
            .as_array()
            .expect("responses content array");
        assert!(resp_blocks.iter().any(|b| b["type"] == "input_image"));

        // 3. Check Chat Completions provider request body wiring
        let messages = vec![
            codewhale_models::Message {
                role: Role::Assistant,
                content: vec![codewhale_models::ContentBlock::ToolUse {
                    execution_id: None,
                    id: "call_read_media".to_string(),
                    name: "read_media".to_string(),
                    input: json!({ "path": "wire.png" }),
                    caller: None,
                    thought_signature: None,
                }],
            },
            codewhale_models::Message {
                role: Role::User,
                content: vec![codewhale_models::ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "call_read_media".to_string(),
                    content: rich.content.clone(),
                    is_error: None,
                    content_blocks: Some(vec![json!({
                        "type": "image",
                        "mime_type": mime_type,
                        "data": data
                    })]),
                }],
            },
        ];
        let chat_msgs = crate::client::chat_messages_for_test(&messages);
        let tool_msg = chat_msgs
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_read_media")
            .expect("tool response message");
        assert!(
            tool_msg["content"]
                .as_str()
                .unwrap()
                .contains("Read media file")
        );
        let follow_up_user = chat_msgs
            .iter()
            .find(|m| m["role"] == "user" && m["content"].is_array())
            .expect("follow-up user message carrying tool image");
        let parts = follow_up_user["content"].as_array().unwrap();
        assert!(
            parts.iter().any(|part| {
                part["type"] == "image_url"
                    && part["image_url"]["url"]
                        .as_str()
                        .is_some_and(|u| u.starts_with("data:image/png;base64,"))
            }),
            "expected image_url part in chat completions follow-up message: {parts:?}"
        );

        // 4. Check Chat Completions provider tool result refs helper
        let blocks = [json!({
            "type": "image",
            "mime_type": mime_type,
            "data": data
        })];
        let (image_ref, omitted) =
            crate::image_attach::provider_tool_result_image_refs(Some(&blocks));
        assert_eq!(omitted, 0);
        assert!(image_ref.is_some());
        let (sniffed_mime, payload) = image_ref.unwrap();
        assert_eq!(sniffed_mime, "image/png");
        assert_eq!(payload, data);
    }

    #[tokio::test]
    async fn read_media_decompression_bomb_rejected() {
        let dir = tempdir().unwrap();
        let bomb_path = dir.path().join("bomb.png");

        // Construct a synthetic PNG header with 10,000 x 10,000 dimensions (100 megapixels > 33.5 megapixel guard)
        // PNG signature + IHDR chunk (length 13, type IHDR, width, height, bit depth 8, color type 6, etc.)
        let mut fake_png = Vec::new();
        fake_png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        fake_png.extend_from_slice(&13_u32.to_be_bytes()); // IHDR length
        fake_png.extend_from_slice(b"IHDR");
        fake_png.extend_from_slice(&10_000_u32.to_be_bytes()); // width: 10,000
        fake_png.extend_from_slice(&10_000_u32.to_be_bytes()); // height: 10,000
        fake_png.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        fake_png.extend_from_slice(&[0, 0, 0, 0]); // CRC (dummy)
        std::fs::write(&bomb_path, &fake_png).unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let err = tool
            .execute_rich(json!({ "path": "bomb.png" }), &ctx)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("decompression bomb")
                || msg.contains("exceed safe limits")
                || msg.contains("guard triggered"),
            "{}",
            msg
        );
    }

    #[tokio::test]
    async fn read_media_gif_format_supported() {
        let dir = tempdir().unwrap();
        let gif_path = dir.path().join("anim.gif");
        let img = image::RgbaImage::from_pixel(30, 30, image::Rgba([100, 150, 200, 255]));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Gif).unwrap();
        std::fs::write(&gif_path, cursor.into_inner()).unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "anim.gif" }), &ctx)
            .await
            .unwrap();
        assert!(rich.success);
        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["original_mime_type"], "image/gif");
        assert_eq!(meta["mime_type"], "image/png");
    }

    #[tokio::test]
    async fn read_media_webp_format_supported() {
        let dir = tempdir().unwrap();
        let webp_path = dir.path().join("image.webp");
        let webp_bytes = BASE64
            .decode("UklGRkoAAABXRUJQVlA4WAoAAAAQAAAAAAAAAAAAQUxQSAwAAAARBxAR/Q9ERP8DAABWUDggGAAAADABAJ0BKgEAAQABABwlpAADcAD+/gbQAA==")
            .expect("valid webp");
        std::fs::write(&webp_path, webp_bytes).unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "image.webp" }), &ctx)
            .await
            .unwrap();
        assert!(rich.success);
        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["original_mime_type"], "image/webp");
        assert_eq!(meta["mime_type"], "image/png");
    }

    #[tokio::test]
    async fn read_media_supported_and_unknown_routes_admitted() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("check.png");
        let png_data = create_test_png(20, 20, [50, 50, 50, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let tool = ReadMediaTool;

        // 1. Supported route
        let mut ctx_sup = ToolContext::new(dir.path());
        ctx_sup.route_capabilities.image_input = CapabilityState::Supported;
        let res_sup = tool
            .execute_rich(json!({ "path": "check.png" }), &ctx_sup)
            .await;
        assert!(res_sup.is_ok());

        // Unknown deliberately matches the established attachment contract:
        // custom/self-hosted routes frequently lack modality metadata, so
        // only a known Unsupported verdict blocks this explicit user action
        // and the provider remains authoritative.
        let mut ctx_unk = ToolContext::new(dir.path());
        ctx_unk.route_capabilities.image_input = CapabilityState::Unknown;
        let res_unk = tool
            .execute_rich(json!({ "path": "check.png" }), &ctx_unk)
            .await;
        let rich_unk = res_unk.unwrap();
        assert!(rich_unk.content.contains(
            "Route image support is unverified; if the model cannot see this image, use image_ocr."
        ));
    }

    #[tokio::test]
    async fn read_media_receipt_contains_no_credentials() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("safe.png");
        let png_data = create_test_png(15, 15, [255, 255, 255, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let ctx = ToolContext::new(dir.path());
        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "safe.png" }), &ctx)
            .await
            .unwrap();

        let meta = rich.metadata.as_ref().unwrap();
        let meta_str = meta.to_string();
        assert!(!meta_str.contains("key"));
        assert!(!meta_str.contains("secret"));
        assert!(!meta_str.contains("token"));
        assert!(!meta_str.contains("password"));
        assert!(!meta_str.contains("auth"));
    }

    /// Deterministic photo-like noise image (high colorfulness, hard to
    /// compress), so the ladder takes the JPEG path without any network or
    /// fixture dependency.
    fn create_noise_png(width: u32, height: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(width, height);
        let mut state: u32 = 0x1234_5678;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        };
        for px in img.pixels_mut() {
            *px = image::Rgb([next(), next(), next()]);
        }
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Png).unwrap();
        cursor.into_inner()
    }

    fn decode_payload(data: &str) -> (u32, u32, Vec<u8>) {
        let bytes = BASE64.decode(data).expect("valid base64 payload");
        let img = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .unwrap()
            .decode()
            .expect("delivered payload decodes");
        (img.width(), img.height(), bytes)
    }

    #[tokio::test]
    async fn read_media_photo_over_budget_arrives_under_budget_as_jpeg() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("photo.png");
        let png_data = create_noise_png(1600, 1200);
        assert!(
            png_data.len() > READ_IMAGE_BYTE_BUDGET,
            "test image must start over the default read budget"
        );
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "photo.png", "detail": "auto" }), &ctx)
            .await
            .unwrap();
        assert!(rich.success);

        let ToolResultContentBlock::Image { mime_type, data } = &rich.content_blocks[0];
        assert_eq!(
            mime_type, "image/jpeg",
            "photo-like content must go out as JPEG, not PNG"
        );
        let payload_len = BASE64.decode(data).unwrap().len();
        assert!(
            payload_len <= READ_IMAGE_BYTE_BUDGET,
            "delivered payload {payload_len} must fit the {}-byte read budget",
            READ_IMAGE_BYTE_BUDGET
        );

        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["mime_type"], "image/jpeg");
        assert_eq!(meta["original_width"], 1600);
        assert_eq!(meta["original_height"], 1200);
        assert_eq!(
            meta["delivery"], "downsampled",
            "a quality- or resolution-reduced delivery is never silent"
        );
        assert!(rich.content.contains("fine detail may be lost"));
        assert!(
            rich.content
                .contains("crop parameter (original-image pixel coordinates)")
        );
    }

    #[tokio::test]
    async fn read_media_small_image_passes_untouched() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("small.png");
        let png_data = create_test_png(120, 80, [10, 120, 200, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "small.png" }), &ctx)
            .await
            .unwrap();
        assert!(rich.success);

        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["delivery"], "untouched");
        assert_eq!(meta["mime_type"], "image/png");
        assert_eq!(meta["width"], 120);
        assert_eq!(meta["height"], 80);
        assert!(
            meta["original_path"].is_null(),
            "untouched reads keep no stored copy"
        );
        assert!(rich.content.contains("untouched"));
        assert!(rich.content.contains("no downsampling applied"));
    }

    #[tokio::test]
    async fn read_media_alpha_image_stays_png() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("alpha.png");
        // Colorful RGB with a real alpha gradient: alpha must survive.
        let mut img = image::RgbaImage::new(640, 480);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x % 256) as u8, 200, 90, if x < 320 { 255 } else { 100 }]);
        }
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Png).unwrap();
        std::fs::write(&img_path, cursor.into_inner()).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "alpha.png" }), &ctx)
            .await
            .unwrap();
        assert!(rich.success);

        let ToolResultContentBlock::Image { mime_type, data } = &rich.content_blocks[0];
        assert_eq!(
            mime_type, "image/png",
            "meaningful alpha must never be flattened to JPEG"
        );
        let (w, h, bytes) = decode_payload(data);
        assert_eq!((w, h), (640, 480));
        let decoded = ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap()
            .to_rgba8();
        assert!(
            decoded.pixels().any(|p| p.0[3] == 100),
            "alpha channel must survive delivery"
        );
    }

    #[tokio::test]
    async fn read_media_delivery_note_reports_mode_dims_and_zoom_guidance() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("big.png");
        let png_data = create_test_png(3000, 1500, [0, 0, 255, 255]);
        std::fs::write(&img_path, &png_data).unwrap();

        let mut ctx = ToolContext::new(dir.path());
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "big.png", "detail": "auto" }), &ctx)
            .await
            .unwrap();
        assert!(rich.success);

        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["delivery"], "downsampled");
        assert_eq!(meta["width"], 2048);
        assert_eq!(meta["height"], 1024);

        let note = &rich.content;
        assert!(note.contains("Original dimensions: 3000x1500"), "{note}");
        assert!(note.contains("downsampled to 2048x1024"), "{note}");
        assert!(note.contains("fine detail may be lost"), "{note}");
        assert!(
            note.contains(
                "call read_media again with the crop parameter (original-image pixel coordinates)"
            ),
            "downsampled note must guide zoom via crop: {note}"
        );
    }

    #[tokio::test]
    async fn read_media_crop_after_downsample_pulls_full_res_stored_original() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("media-originals");
        let img_path = dir.path().join("scene.png");
        // 2600x1300 photo-like source: auto detail downsamples to 2048x1024.
        std::fs::write(&img_path, create_noise_png(2600, 1300)).unwrap();

        let mut ctx = ToolContext::new(dir.path()).with_runtime_services(
            crate::tools::spec::RuntimeToolServices {
                media_originals_dir: Some(store_dir.clone()),
                ..crate::tools::spec::RuntimeToolServices::default()
            },
        );
        ctx.route_capabilities.image_input = CapabilityState::Supported;

        let tool = ReadMediaTool;
        let rich = tool
            .execute_rich(json!({ "path": "scene.png", "detail": "auto" }), &ctx)
            .await
            .unwrap();
        let meta = rich.metadata.as_ref().unwrap();
        assert_eq!(meta["delivery"], "downsampled");
        let delivered_width = meta["width"].as_u64().unwrap();
        assert!(
            delivered_width <= 2048,
            "auto detail caps at 2048px; noise may ladder further: {delivered_width}"
        );
        assert_eq!(meta["original_width"], 2600);
        assert_eq!(meta["original_height"], 1300);

        // The pre-compression original is persisted, content-addressed.
        let original_path = meta["original_path"]
            .as_str()
            .expect("downsampled reads persist the original")
            .to_string();
        assert!(original_path.starts_with(store_dir.to_str().unwrap()));
        let stored = std::fs::read(&original_path).expect("stored original bytes");
        let stored_img = ImageReader::new(Cursor::new(&stored))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(
            (stored_img.width(), stored_img.height()),
            (2600, 1300),
            "the store holds the full-resolution source, not the downsampled copy"
        );

        // A crop that is out of bounds for the downsampled 2048-wide copy but
        // valid for the 2600-wide original must succeed at the requested
        // pixel size — proof the crop pulls the full-res stored source.
        let crop = tool
            .execute_rich(
                json!({
                    "path": original_path,
                    "crop": { "x": 2200, "y": 900, "width": 300, "height": 300 }
                }),
                &ctx,
            )
            .await
            .unwrap();
        let crop_meta = crop.metadata.as_ref().unwrap();
        assert_eq!(crop_meta["delivery"], "crop");
        assert_eq!(crop_meta["width"], 300);
        assert_eq!(crop_meta["height"], 300);
        assert_eq!(crop_meta["original_width"], 2600);
        assert!(
            crop.content.contains("region offset (x=2200, y=900)"),
            "crop note must carry the offset guidance: {}",
            crop.content
        );
    }

    #[tokio::test]
    async fn read_media_over_budget_failure_names_conversion_recipe() {
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("noise.png");
        std::fs::write(&img_path, create_noise_png(512, 512)).unwrap();
        let raw = std::fs::read(&img_path).unwrap();
        let (image, _, _) = decode_and_guard_image(&raw).unwrap();

        // A 1024-byte budget is unreachable even at the 256px/q20 floor.
        let err =
            encode_within_budget(&image, EncodePolicy::PreferJpeg, 1024, &img_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Nothing was sent"), "{msg}");
        assert!(msg.contains("sips -Z 1024"), "recipe names sips: {msg}");
        assert!(msg.contains("magick"), "recipe names ImageMagick: {msg}");
        assert!(msg.contains("smaller copy"), "{msg}");

        // Alpha-bearing images fail closed the same way rather than
        // falling back to JPEG.
        let err = encode_within_budget(&image, EncodePolicy::RequireAlphaPng, 1024, &img_path)
            .unwrap_err();
        assert!(err.to_string().contains("Nothing was sent"), "{err}");
    }
}
