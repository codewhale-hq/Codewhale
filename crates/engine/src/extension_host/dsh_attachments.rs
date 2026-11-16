use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use codewhale_models::image_window::parse_data_url;
use codewhale_models::{ContentBlock, Message};
use codewhale_runtime::hashing::sha256_hex;
use image::{DynamicImage, GenericImageView, ImageError, ImageReader};
use serde::Deserialize;
use serde_json::{Value, json};

use super::core_call::HeldImage;
use super::dsh_broker::{
    BrokerCall, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, decode,
};
use super::protocol::DshService;
use crate::image_attach::{
    ACCEPTED_IMAGE_MEDIA_TYPES, ATTACH_MAX_EDGE_PX, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION,
    MAX_IMAGE_PIXELS, decode_and_guard_image, decode_limits, detect_rejected_format,
    prepare_tool_image_bytes, sniff_media_type, stored_tool_image_payload,
};
use crate::tools::read_media::fit_and_encode;

const MAX_ATTACHMENT_ID_LEN: usize = 7 + 64;
const MAX_NAME_LEN: usize = 128;

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", rename_all = "snake_case")]
enum AttachmentsMethod {
    Validate(ImageBytesInput),
    Save(ImageBytesInput),
    Read(ReadInput),
    ReadRequest(ReadRequestInput),
}

impl BrokerMethods for AttachmentsMethod {
    const NAMES: &'static [&'static str] = &["validate", "save", "read", "read_request"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageBytesInput {
    data_base64: String,
    media_type: String,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentRefWire {
    attachment_id: String,
    media_type: Option<String>,
    bytes: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    attachment: AttachmentRefWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequestInput {
    attachment: AttachmentRefWire,
    width: Option<u32>,
    height: Option<u32>,
    max_bytes: Option<usize>,
}

struct StoredImage {
    attachment_id: String,
    media_type: String,
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

struct ValidatedImage {
    bytes: Vec<u8>,
    media_type: &'static str,
    width: u32,
    height: u32,
}

struct CodedError {
    code: &'static str,
    message: String,
}

fn refuse(code: &str, message: impl std::fmt::Display) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Attachments,
        message: format!("[{code}] {message}"),
    }
}

fn invalid_input(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::InvalidInput {
        service: DshService::Attachments,
        message: message.into(),
    }
}

fn refused_with_record(
    call: &BrokerCall,
    method: &str,
    code: &str,
    message: impl std::fmt::Display,
) -> BrokerRefusal {
    let _ = call.context.record(json!({
        "method": method,
        "outcome": "refused",
        "reason": code,
    }));
    refuse(code, message)
}

fn valid_attachment_id(id: &str) -> bool {
    id.len() == MAX_ATTACHMENT_ID_LEN
        && id.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

fn check_name(name: Option<&str>) -> Result<Option<String>, BrokerRefusal> {
    let Some(name) = name else {
        return Ok(None);
    };
    if name.is_empty()
        || name.len() > MAX_NAME_LEN
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains(':')
        || name.chars().any(|ch| ch.is_control())
    {
        return Err(invalid_input(
            "attachment name must be a non-empty basename with no path separators or control characters",
        ));
    }
    Ok(Some(name.to_string()))
}

fn accepted_media_type(raw: &str) -> Option<&'static str> {
    ACCEPTED_IMAGE_MEDIA_TYPES
        .iter()
        .copied()
        .find(|&candidate| candidate == raw)
}

fn validate_raw_image(
    data_base64: &str,
    declared_media_type: &str,
) -> Result<ValidatedImage, CodedError> {
    let Some(declared) = accepted_media_type(declared_media_type) else {
        return Err(CodedError {
            code: "UNSUPPORTED_IMAGE_TYPE",
            message: format!("unsupported image media type: {declared_media_type}"),
        });
    };
    let max_b64 = MAX_IMAGE_BYTES.div_ceil(3) * 4;
    if data_base64.len() > max_b64 {
        return Err(CodedError {
            code: "IMAGE_TOO_LARGE",
            message: format!("image exceeds the {MAX_IMAGE_BYTES} byte limit"),
        });
    }
    let bytes = STANDARD.decode(data_base64).map_err(|_| CodedError {
        code: "INVALID_IMAGE_BASE64",
        message: "invalid base64 image payload".to_string(),
    })?;
    if bytes.is_empty() {
        return Err(CodedError {
            code: "INVALID_IMAGE",
            message: "image payload is empty".to_string(),
        });
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(CodedError {
            code: "IMAGE_TOO_LARGE",
            message: format!("image exceeds the {MAX_IMAGE_BYTES} byte limit"),
        });
    }
    if let Some(rejected) = detect_rejected_format(&bytes) {
        return Err(CodedError {
            code: "UNSUPPORTED_IMAGE_TYPE",
            message: format!("{rejected} is not supported; use PNG, JPEG, GIF, or WebP"),
        });
    }
    let Some(sniffed) = sniff_media_type(&bytes) else {
        return Err(CodedError {
            code: "INVALID_IMAGE",
            message: "image bytes do not match PNG, JPEG, GIF, or WebP".to_string(),
        });
    };
    if sniffed != declared {
        return Err(CodedError {
            code: "IMAGE_TYPE_MISMATCH",
            message: format!(
                "declared mediaType {declared} does not match image content {sniffed}"
            ),
        });
    }
    let mut reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| CodedError {
            code: "INVALID_IMAGE",
            message: "could not inspect image header".to_string(),
        })?;
    reader.limits(decode_limits());
    let (width, height) = reader.into_dimensions().map_err(|error| match error {
        ImageError::Limits(_) => CodedError {
            code: "IMAGE_DIMENSION_TOO_LARGE",
            message: format!("image dimensions exceed the {MAX_IMAGE_DIMENSION}px limit"),
        },
        _ => CodedError {
            code: "INVALID_IMAGE",
            message: "corrupt or truncated image header".to_string(),
        },
    })?;
    if width == 0 || height == 0 {
        return Err(CodedError {
            code: "INVALID_IMAGE",
            message: "image has zero width or height".to_string(),
        });
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(CodedError {
            code: "IMAGE_DIMENSION_TOO_LARGE",
            message: format!(
                "image dimensions {width}x{height} exceed the {MAX_IMAGE_DIMENSION}px limit"
            ),
        });
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(CodedError {
            code: "IMAGE_TOO_MANY_PIXELS",
            message: format!("image pixels exceed the {MAX_IMAGE_PIXELS} pixel limit"),
        });
    }
    let (decoded, dec_width, dec_height) =
        decode_and_guard_image(&bytes).map_err(|_| CodedError {
            code: "INVALID_IMAGE",
            message: "invalid or corrupted image payload".to_string(),
        })?;
    let (actual_width, actual_height) = decoded.dimensions();
    if dec_width == 0
        || dec_height == 0
        || actual_width == 0
        || actual_height == 0
        || actual_width > MAX_IMAGE_DIMENSION
        || actual_height > MAX_IMAGE_DIMENSION
    {
        return Err(CodedError {
            code: "IMAGE_DIMENSION_TOO_LARGE",
            message: format!("image dimensions exceed the {MAX_IMAGE_DIMENSION}px limit"),
        });
    }
    if u64::from(actual_width) * u64::from(actual_height) > MAX_IMAGE_PIXELS {
        return Err(CodedError {
            code: "IMAGE_TOO_MANY_PIXELS",
            message: format!("image pixels exceed the {MAX_IMAGE_PIXELS} pixel limit"),
        });
    }
    Ok(ValidatedImage {
        bytes,
        media_type: sniffed,
        width: actual_width,
        height: actual_height,
    })
}

fn prepare_for_save(
    data_base64: &str,
    declared_media_type: &str,
) -> Result<(Vec<u8>, &'static str, u32, u32, String), CodedError> {
    let validated = validate_raw_image(data_base64, declared_media_type)?;
    let prepared = prepare_tool_image_bytes(&validated.bytes, validated.media_type);
    let Some(codewhale_tools::ToolResultContentBlock::Image { mime_type, data }) = prepared.block
    else {
        return Err(CodedError {
            code: "IMAGE_TOO_LARGE",
            message: "image could not fit the cross-provider tool delivery budget".to_string(),
        });
    };
    let stored_bytes = STANDARD.decode(&data).map_err(|_| CodedError {
        code: "INVALID_IMAGE",
        message: "prepared image encoding failed".to_string(),
    })?;
    if stored_bytes.len() > MAX_IMAGE_BYTES {
        return Err(CodedError {
            code: "IMAGE_TOO_LARGE",
            message: format!("prepared image exceeds the {MAX_IMAGE_BYTES} byte limit"),
        });
    }
    let Some(stored_mime) = accepted_media_type(&mime_type) else {
        return Err(CodedError {
            code: "UNSUPPORTED_IMAGE_TYPE",
            message: format!("prepared image type {mime_type} is unsupported"),
        });
    };
    if sniff_media_type(&stored_bytes) != Some(stored_mime) {
        return Err(CodedError {
            code: "INVALID_IMAGE",
            message: "prepared image bytes failed media verification".to_string(),
        });
    }
    let (decoded, _, _) = decode_and_guard_image(&stored_bytes).map_err(|_| CodedError {
        code: "INVALID_IMAGE",
        message: "prepared image failed decode verification".to_string(),
    })?;
    let (width, height) = decoded.dimensions();
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(CodedError {
            code: "IMAGE_DIMENSION_TOO_LARGE",
            message: format!("prepared image dimensions exceed the {MAX_IMAGE_DIMENSION}px limit"),
        });
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(CodedError {
            code: "IMAGE_TOO_MANY_PIXELS",
            message: format!("prepared image pixels exceed the {MAX_IMAGE_PIXELS} pixel limit"),
        });
    }
    Ok((stored_bytes, stored_mime, width, height, data))
}

fn verify_stored_bytes(attachment_id: String, b64: &str) -> Option<(StoredImage, DynamicImage)> {
    if b64.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4 {
        return None;
    }
    let bytes = STANDARD.decode(b64).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let id = format!("sha256:{}", sha256_hex(&bytes));
    if id != attachment_id {
        return None;
    }
    let media_type = sniff_media_type(&bytes)?;
    let (decoded, _, _) = decode_and_guard_image(&bytes).ok()?;
    let (width, height) = decoded.dimensions();
    Some((
        StoredImage {
            attachment_id: id,
            media_type: media_type.to_string(),
            bytes,
            width,
            height,
        },
        decoded,
    ))
}

fn resolve_from_conversation_blocking(
    held: Option<HeldImage>,
    messages: Option<&[Message]>,
    attachment_id: &str,
) -> Option<(StoredImage, DynamicImage)> {
    if let Some(held) = held
        && let Some(found) = verify_stored_bytes(held.attachment_id, &held.data_base64)
    {
        return Some(found);
    }
    let messages = messages?;
    for message in messages {
        for block in &message.content {
            match block {
                ContentBlock::ImageUrl { image_url } => {
                    if let Some((_, payload)) = parse_data_url(&image_url.url)
                        && let Some(found) = verify_stored_bytes(attachment_id.to_string(), payload)
                    {
                        return Some(found);
                    }
                }
                ContentBlock::ToolResult {
                    content_blocks: Some(blocks),
                    ..
                } => {
                    for value in blocks {
                        if let Some((_, payload)) = stored_tool_image_payload(value)
                            && let Some(found) =
                                verify_stored_bytes(attachment_id.to_string(), payload)
                        {
                            return Some(found);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn check_ref_fields(wire: &AttachmentRefWire, stored: &StoredImage) -> Result<(), CodedError> {
    if let Some(ref media_type) = wire.media_type
        && media_type != &stored.media_type
    {
        return Err(CodedError {
            code: "INVALID_ATTACHMENT_REF",
            message: "attachment reference mediaType does not match stored image".to_string(),
        });
    }
    if let Some(bytes) = wire.bytes
        && bytes != stored.bytes.len() as u64
    {
        return Err(CodedError {
            code: "INVALID_ATTACHMENT_REF",
            message: "attachment reference byte size does not match stored image".to_string(),
        });
    }
    if let Some(width) = wire.width
        && width != stored.width
    {
        return Err(CodedError {
            code: "INVALID_ATTACHMENT_REF",
            message: "attachment reference width does not match stored image".to_string(),
        });
    }
    if let Some(height) = wire.height
        && height != stored.height
    {
        return Err(CodedError {
            code: "INVALID_ATTACHMENT_REF",
            message: "attachment reference height does not match stored image".to_string(),
        });
    }
    Ok(())
}

fn ref_json(stored: &StoredImage) -> Value {
    json!({
        "attachment_id": stored.attachment_id,
        "media_type": stored.media_type,
        "bytes": stored.bytes.len(),
        "width": stored.width,
        "height": stored.height,
    })
}

fn target_edge_for_box(
    orig_w: u32,
    orig_h: u32,
    wanted_w: Option<u32>,
    wanted_h: Option<u32>,
) -> u32 {
    let edge_for_w = |tw: u32| {
        if orig_w <= orig_h {
            (u64::from(tw) * u64::from(orig_h) / u64::from(orig_w).max(1))
                .clamp(1, u64::from(MAX_IMAGE_DIMENSION)) as u32
        } else {
            tw
        }
    };
    let edge_for_h = |th: u32| {
        if orig_h < orig_w {
            (u64::from(th) * u64::from(orig_w) / u64::from(orig_h).max(1))
                .clamp(1, u64::from(MAX_IMAGE_DIMENSION)) as u32
        } else {
            th
        }
    };
    match (wanted_w, wanted_h) {
        (Some(tw), Some(th)) => edge_for_w(tw).min(edge_for_h(th)),
        (Some(tw), None) => {
            if orig_w <= tw {
                orig_w.max(orig_h)
            } else {
                edge_for_w(tw)
            }
        }
        (None, Some(th)) => {
            if orig_h <= th {
                orig_w.max(orig_h)
            } else {
                edge_for_h(th)
            }
        }
        (None, None) => orig_w.max(orig_h).min(ATTACH_MAX_EDGE_PX),
    }
}

async fn handle_validate(
    call: &BrokerCall,
    input: ImageBytesInput,
) -> Result<Value, BrokerRefusal> {
    check_name(input.name.as_deref())?;
    let target = accepted_media_type(&input.media_type);
    call.context
        .act("validate", BrokerEffect::Read, target)
        .await?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    let validated = tokio::task::spawn_blocking(move || {
        validate_raw_image(&input.data_base64, &input.media_type)
    })
    .await
    .map_err(|_| refuse("ATTACHMENT_READ_FAILED", "image validation task ended"))?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    match validated {
        Ok(image) => {
            let _ = call.context.record(json!({
                "method": "validate",
                "outcome": "valid",
                "media_type": image.media_type,
                "bytes": image.bytes.len(),
                "width": image.width,
                "height": image.height,
            }));
            Ok(json!({
                "ok": true,
                "media_type": image.media_type,
                "bytes": image.bytes.len(),
                "width": image.width,
                "height": image.height,
            }))
        }
        Err(error) => Err(refused_with_record(
            call,
            "validate",
            error.code,
            error.message,
        )),
    }
}

async fn handle_save(call: &BrokerCall, input: ImageBytesInput) -> Result<Value, BrokerRefusal> {
    check_name(input.name.as_deref())?;
    let target = accepted_media_type(&input.media_type);
    call.context.act("save", BrokerEffect::Read, target).await?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    if let Err(sealed) = call.context.can_hold_image() {
        if sealed {
            return Err(refused_with_record(
                call,
                "save",
                "CANCELLED",
                "tool invocation has already ended",
            ));
        }
        return Err(refused_with_record(
            call,
            "save",
            "TOO_MANY_IMAGES",
            "this Codewhale keeps one image per tool result",
        ));
    }
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_for_save(&input.data_base64, &input.media_type)
    })
    .await
    .map_err(|_| refuse("ATTACHMENT_READ_FAILED", "image save preparation failed"))?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    let (stored_bytes, stored_mime, width, height, stored_b64) = match prepared {
        Ok(parts) => parts,
        Err(error) => {
            return Err(refused_with_record(call, "save", error.code, error.message));
        }
    };
    let attachment_id = format!("sha256:{}", sha256_hex(&stored_bytes));
    let held = HeldImage {
        attachment_id: attachment_id.clone(),
        media_type: stored_mime.to_string(),
        bytes: stored_bytes.len(),
        width,
        height,
        data_base64: stored_b64,
    };
    if let Err(sealed) = call.context.hold_image(held) {
        if sealed {
            return Err(refused_with_record(
                call,
                "save",
                "CANCELLED",
                "tool invocation has already ended",
            ));
        }
        return Err(refused_with_record(
            call,
            "save",
            "TOO_MANY_IMAGES",
            "this Codewhale keeps one image per tool result",
        ));
    }
    let _ = call.context.record(json!({
        "method": "save",
        "outcome": "held",
        "attachment_id": attachment_id,
        "media_type": stored_mime,
        "bytes": stored_bytes.len(),
        "width": width,
        "height": height,
    }));
    Ok(json!({
        "attachment": {
            "attachment_id": attachment_id,
            "media_type": stored_mime,
            "bytes": stored_bytes.len(),
            "width": width,
            "height": height,
        }
    }))
}

async fn handle_read(call: &BrokerCall, input: ReadInput) -> Result<Value, BrokerRefusal> {
    if !valid_attachment_id(&input.attachment.attachment_id) {
        return Err(refuse("INVALID_ATTACHMENT_REF", "invalid attachment id"));
    }
    call.context
        .act(
            "read",
            BrokerEffect::Read,
            Some(&input.attachment.attachment_id),
        )
        .await?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    let held = call.context.held_image(&input.attachment.attachment_id);
    let messages = call
        .context
        .tool()
        .execution
        .session_objects
        .as_ref()
        .map(|snapshot| Arc::clone(&snapshot.messages));
    let attachment_ref = input.attachment;
    let resolved = tokio::task::spawn_blocking(move || {
        let (stored, _) = resolve_from_conversation_blocking(
            held,
            messages.as_deref().map(Vec::as_slice),
            &attachment_ref.attachment_id,
        )
        .ok_or_else(|| CodedError {
            code: "ATTACHMENT_NOT_FOUND",
            message: "attachment not found in this conversation".to_string(),
        })?;
        check_ref_fields(&attachment_ref, &stored)?;
        let data_base64 = STANDARD.encode(&stored.bytes);
        Ok::<_, CodedError>((stored, data_base64))
    })
    .await
    .map_err(|_| refuse("ATTACHMENT_READ_FAILED", "attachment read task ended"))?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    let (stored, data_base64) = match resolved {
        Ok(resolved) => resolved,
        Err(error) => {
            return Err(refused_with_record(call, "read", error.code, error.message));
        }
    };
    let _ = call.context.record(json!({
        "method": "read",
        "outcome": "read",
        "attachment_id": stored.attachment_id,
        "media_type": stored.media_type,
        "bytes": stored.bytes.len(),
        "width": stored.width,
        "height": stored.height,
    }));
    Ok(json!({
        "data_base64": data_base64,
        "attachment": ref_json(&stored),
    }))
}

async fn handle_read_request(
    call: &BrokerCall,
    input: ReadRequestInput,
) -> Result<Value, BrokerRefusal> {
    if !valid_attachment_id(&input.attachment.attachment_id) {
        return Err(refuse("INVALID_ATTACHMENT_REF", "invalid attachment id"));
    }
    if input
        .width
        .is_some_and(|w| w == 0 || w > MAX_IMAGE_DIMENSION)
        || input
            .height
            .is_some_and(|h| h == 0 || h > MAX_IMAGE_DIMENSION)
    {
        return Err(refuse(
            "IMAGE_DIMENSION_TOO_LARGE",
            format!("requested dimensions must be between 1 and {MAX_IMAGE_DIMENSION}"),
        ));
    }
    if input
        .max_bytes
        .is_some_and(|max| max == 0 || max > MAX_IMAGE_BYTES)
    {
        return Err(refuse(
            "IMAGE_TOO_LARGE",
            format!("requested maxBytes must be between 1 and {MAX_IMAGE_BYTES}"),
        ));
    }
    call.context
        .act(
            "read_request",
            BrokerEffect::Read,
            Some(&input.attachment.attachment_id),
        )
        .await?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    let held = call.context.held_image(&input.attachment.attachment_id);
    let messages = call
        .context
        .tool()
        .execution
        .session_objects
        .as_ref()
        .map(|snapshot| Arc::clone(&snapshot.messages));
    let budget = input.max_bytes.unwrap_or(MAX_IMAGE_BYTES);
    let wanted_w = input.width;
    let wanted_h = input.height;
    let attachment_ref = input.attachment;
    let fitted = tokio::task::spawn_blocking(move || {
        let (stored, decoded) = resolve_from_conversation_blocking(
            held,
            messages.as_deref().map(Vec::as_slice),
            &attachment_ref.attachment_id,
        )
        .ok_or_else(|| CodedError {
            code: "ATTACHMENT_NOT_FOUND",
            message: "attachment not found in this conversation".to_string(),
        })?;
        check_ref_fields(&attachment_ref, &stored)?;
        let max_edge = target_edge_for_box(stored.width, stored.height, wanted_w, wanted_h);
        let (encoded, mime) = fit_and_encode(&decoded, max_edge, budget, Path::new("attachment"))
            .map_err(|_| CodedError {
            code: "IMAGE_TOO_LARGE",
            message: "image could not fit the requested size budget".to_string(),
        })?;
        let mut reader = ImageReader::new(Cursor::new(&encoded))
            .with_guessed_format()
            .map_err(|_| CodedError {
                code: "ATTACHMENT_READ_FAILED",
                message: "fitted image header could not be inspected".to_string(),
            })?;
        reader.limits(decode_limits());
        let (out_w, out_h) = reader.into_dimensions().map_err(|_| CodedError {
            code: "ATTACHMENT_READ_FAILED",
            message: "fitted image header could not be inspected".to_string(),
        })?;
        let variant_id = format!("sha256:{}", sha256_hex(&encoded));
        let data_base64 = STANDARD.encode(&encoded);
        Ok::<_, CodedError>((
            stored.attachment_id,
            variant_id,
            data_base64,
            mime,
            encoded.len(),
            out_w,
            out_h,
        ))
    })
    .await
    .map_err(|_| refuse("ATTACHMENT_READ_FAILED", "image resize task ended"))?;
    if call.context.cancel().is_cancelled() {
        return Err(refuse(
            "CANCELLED",
            "the attachments broker call was cancelled",
        ));
    }
    let (attachment_id, variant_id, data_base64, mime, byte_len, out_w, out_h) = match fitted {
        Ok(result) => result,
        Err(error) => {
            return Err(refused_with_record(
                call,
                "read_request",
                error.code,
                error.message,
            ));
        }
    };
    let _ = call.context.record(json!({
        "method": "read_request",
        "outcome": "served",
        "attachment_id": attachment_id,
        "variant_id": variant_id,
        "media_type": mime,
        "bytes": byte_len,
        "width": out_w,
        "height": out_h,
    }));
    Ok(json!({
        "variant_id": variant_id,
        "data_base64": data_base64,
        "media_type": mime,
    }))
}

pub fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        if call.context.cancel().is_cancelled() {
            return Err(refuse(
                "CANCELLED",
                "the attachments broker call was cancelled",
            ));
        }
        let method = decode::<AttachmentsMethod>(&call)?;
        match method {
            AttachmentsMethod::Validate(input) => handle_validate(&call, input).await,
            AttachmentsMethod::Save(input) => handle_save(&call, input).await,
            AttachmentsMethod::Read(input) => handle_read(&call, input).await,
            AttachmentsMethod::ReadRequest(input) => handle_read_request(&call, input).await,
        }
    })
}

#[cfg(test)]
#[path = "dsh_attachments_tests.rs"]
mod tests;
