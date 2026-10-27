use crate::{ContentBlock, Message};

/// Split a `data:<media-type>;base64,<payload>` URL.
///
/// Anthropic's Messages API models an image as a tagged `source` rather than a
/// URL, so the native route has to take the data URL back apart. Returns
/// `None` for `http(s)` URLs and for anything malformed, which the caller
/// renders as a remote source or a visible degradation respectively.
#[must_use]
pub fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (header, payload) = rest.split_once(',')?;
    let media_type = header.strip_suffix(";base64")?;
    if media_type.is_empty() || payload.is_empty() {
        return None;
    }
    Some((media_type, payload))
}

/// Whether a URL is one a provider can fetch for itself.
#[must_use]
pub fn is_remote_image_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

pub const ELIDED_IMAGE_NOTE: &str = "[Image not attached to this request: this model accepts only a limited number of images per request, so the oldest are left out. It remains in the session; do not guess what it showed.]";

pub const ELIDED_TOOL_IMAGE_NOTE: &str = "[Tool image not attached to this request: this model accepts only a limited number of images per request, so the oldest are left out; do not guess what it showed. Take a fresh observation (for example a new screenshot) if you need the current state; do not repeat an action to recover its image.]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageWindow {
    pub total: usize,
    pub elided: usize,
    pub cap: usize,
}

fn block_carries_image(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::ImageUrl { .. } => true,
        ContentBlock::ToolResult { content_blocks, .. } => {
            content_blocks.as_ref().is_some_and(|blocks| {
                blocks.iter().any(|value| {
                    value.get("type").and_then(serde_json::Value::as_str) == Some("image")
                })
            })
        }
        _ => false,
    }
}

#[must_use]
pub fn count_request_images(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| block_carries_image(block))
        .count()
}

#[must_use]
pub fn images_to_elide(total: usize, cap: usize) -> usize {
    if total <= cap {
        return 0;
    }
    let chunk = (cap / 4).max(1);
    ((total - cap).div_ceil(chunk) * chunk).min(total)
}

pub fn cap_request_images(messages: &mut [Message], cap: usize) -> ImageWindow {
    let total = count_request_images(messages);
    let mut window = cap_request_images_with_notes(
        messages,
        total - images_to_elide(total, cap),
        ELIDED_IMAGE_NOTE,
        ELIDED_TOOL_IMAGE_NOTE,
    );
    window.cap = cap;
    window
}

pub fn cap_request_images_with_notes(
    messages: &mut [Message],
    cap: usize,
    image_note: &str,
    tool_note: &str,
) -> ImageWindow {
    let total = count_request_images(messages);
    let elided = total.saturating_sub(cap);
    let mut remaining = elided;
    for block in messages.iter_mut().flat_map(|message| &mut message.content) {
        if remaining == 0 {
            break;
        }
        if !block_carries_image(block) {
            continue;
        }
        match block {
            ContentBlock::ImageUrl { .. } => {
                *block = ContentBlock::Text {
                    text: image_note.to_string(),
                    cache_control: None,
                };
            }
            ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } => {
                if let Some(blocks) = content_blocks.as_mut() {
                    blocks.retain(|value| {
                        value.get("type").and_then(serde_json::Value::as_str) != Some("image")
                    });
                    if blocks.is_empty() {
                        *content_blocks = None;
                    }
                }
                *content = format!("{content}\n{tool_note}");
            }
            _ => continue,
        }
        remaining -= 1;
    }
    ImageWindow { total, elided, cap }
}

#[must_use]
pub fn tool_result_text_with_omission(content: &str, omitted: usize) -> String {
    if omitted == 0 {
        return content.to_string();
    }
    format!(
        "{content}\n[{omitted} tool-result image block(s) omitted: invalid, unsupported, oversized, or additional.]"
    )
}
