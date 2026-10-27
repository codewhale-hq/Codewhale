use super::*;
use codewhale_models::Role;
use codewhale_models::image_window::is_remote_image_url;

/// A 1x1 PNG, as bytes rather than a fixture file so the encoding tests
/// have no filesystem dependency.
pub(crate) const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[test]
fn sniffs_every_accepted_format_from_magic_bytes() {
    assert_eq!(sniff_media_type(PNG_1X1), Some("image/png"));
    assert_eq!(
        sniff_media_type(&[0xff, 0xd8, 0xff, 0xe0, 0x00]),
        Some("image/jpeg")
    );
    assert_eq!(sniff_media_type(b"GIF89a....."), Some("image/gif"));
    assert_eq!(sniff_media_type(b"GIF87a....."), Some("image/gif"));
    assert_eq!(
        sniff_media_type(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
        Some("image/webp")
    );
}

/// Noise defeats compression, so the shrink ladder has to re-encode for real.
fn noise_payload(width: u32, height: u32) -> Vec<u8> {
    use image::ImageEncoder as _;
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb([
            (x.wrapping_mul(31) ^ y.wrapping_mul(17)) as u8,
            (x.wrapping_mul(7) ^ y.wrapping_mul(29)) as u8,
            (x.wrapping_add(y).wrapping_mul(13)) as u8,
        ]);
    }
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut bytes,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(
        pixels.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )
    .expect("encode fixture png");
    bytes
}

fn image_blocks_fixture() -> Vec<codewhale_models::Message> {
    let payload = STANDARD.encode(noise_payload(320, 320));
    vec![
        codewhale_models::Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "look at this".to_string(),
                    cache_control: None,
                },
                ContentBlock::ImageUrl {
                    image_url: ImageUrlContent {
                        url: format!("data:image/png;base64,{payload}"),
                    },
                },
            ],
        },
        codewhale_models::Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call_1".to_string(),
                content: "Read image file [image/png]".to_string(),
                is_error: None,
                content_blocks: Some(vec![serde_json::json!({
                    "type": "image",
                    "mime_type": "image/png",
                    "data": payload,
                })]),
            }],
        },
    ]
}

#[test]
fn compaction_shrink_rewrites_both_image_carriers_under_budget() {
    let budget = 64 * 1024;
    let mut messages = image_blocks_fixture();
    let outcome = shrink_images_for_request_with_budget(&mut messages, budget);
    assert_eq!(outcome.images, 2, "both carriers are rewritten");
    assert!(outcome.bytes_after < outcome.bytes_before);

    let ContentBlock::ImageUrl { image_url } = &messages[0].content[1] else {
        panic!("image block survives the shrink");
    };
    let (mime, payload) = parse_data_url(&image_url.url).expect("data url");
    assert!(matches!(mime, "image/png" | "image/jpeg"), "{mime}");
    let bytes = STANDARD.decode(payload).expect("base64");
    assert!(bytes.len() <= budget, "{} <= {budget}", bytes.len());
    assert_eq!(sniff_media_type(&bytes), Some(mime));

    let ContentBlock::ToolResult { content_blocks, .. } = &messages[1].content[0] else {
        panic!("tool result survives the shrink");
    };
    let block = &content_blocks.as_ref().expect("blocks")[0];
    assert_eq!(block["type"], "image");
    let nested = STANDARD
        .decode(block["data"].as_str().expect("data"))
        .expect("nested base64");
    assert!(nested.len() <= budget);
    assert_eq!(
        sniff_media_type(&nested),
        block["mime_type"].as_str(),
        "the mime must match the rewritten bytes"
    );
}

#[test]
fn compaction_shrink_leaves_an_under_budget_history_untouched() {
    let mut messages = image_blocks_fixture();
    let before = messages.clone();
    let outcome = shrink_images_for_request_with_budget(&mut messages, 32 * 1024 * 1024);
    assert_eq!(outcome.images, 0, "a fitting request is never rewritten");
    assert_eq!(
        outcome.images_seen, 2,
        "the request still carried images, and the ladder must be able to tell"
    );
    assert_eq!(messages, before);
}

#[test]
fn compaction_placeholder_keeps_the_image_visible_as_text() {
    let mut messages = image_blocks_fixture();
    let replaced = replace_images_with_placeholders(
        &mut messages,
        "the summary request exceeded the provider's request-body limit (HTTP 413)",
    );
    assert_eq!(replaced, 2);

    let ContentBlock::Text { text, .. } = &messages[0].content[1] else {
        panic!("image_url becomes a note");
    };
    assert!(text.contains("omitted from this summary pass"), "{text}");
    assert!(text.contains("do not describe"), "{text}");

    let ContentBlock::ToolResult {
        content,
        content_blocks,
        ..
    } = &messages[1].content[0]
    else {
        panic!("tool result survives");
    };
    assert!(content_blocks.is_none(), "no base64 may remain");
    assert!(content.contains("1 image(s)"), "{content}");
    assert!(content.contains("Read image file [image/png]"), "{content}");
}

#[test]
fn sniffing_ignores_the_extension_and_believes_the_bytes() {
    // A JPEG named .png must be declared image/jpeg, or the provider
    // rejects the media-type mismatch.
    let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x11, 0x22];
    let attached = encode_image_bytes(&jpeg, "screenshot.png").expect("attach");
    assert_eq!(attached.media_type, "image/jpeg");
    assert!(attached.data_url.starts_with("data:image/jpeg;base64,"));
}

#[test]
fn rich_tool_images_reject_corruption_mime_spoofing_and_decode_bombs() {
    use crate::tools::spec::{RichToolResult, ToolResult};
    use codewhale_tools::ToolResultContentBlock;
    let mut oversized_header = PNG_1X1.to_vec();
    oversized_header[16..20].copy_from_slice(&(MAX_IMAGE_DIMENSION + 1).to_be_bytes());
    for (mime, bytes) in [
        ("image/png", &PNG_1X1[..8]),
        ("image/jpeg", PNG_1X1),
        ("image/png", oversized_header.as_slice()),
    ] {
        assert!(prepare_tool_image_bytes(bytes, mime).block.is_none());
        let rich = bound_rich_tool_result(RichToolResult::with_content_blocks(
            ToolResult::success("capture receipt"),
            vec![ToolResultContentBlock::Image {
                mime_type: mime.into(),
                data: STANDARD.encode(bytes),
            }],
        ));
        assert!(rich.result.success);
        assert!(rich.content_blocks.is_empty());
        assert!(rich.result.content.starts_with("capture receipt"));
        assert!(
            rich.result
                .content
                .contains("1 tool-result image block(s) omitted")
        );
        let stored = vec![
            serde_json::json!({"type":"image","mime_type":mime,"data":STANDARD.encode(bytes)}),
        ];
        let (image, omitted) = provider_tool_result_image_refs(Some(&stored));
        assert!(
            image.is_none(),
            "restored history must not bypass validation"
        );
        assert_eq!(omitted, 1);
    }
}

#[test]
fn lowercase_read_image_preparation_is_typed_and_bounded() {
    let prepared = prepare_tool_image_bytes(PNG_1X1, "image/png");
    assert_eq!(prepared.note, "Read image file [image/png]");
    let codewhale_tools::ToolResultContentBlock::Image { mime_type, data } =
        prepared.block.expect("typed image");
    assert_eq!(mime_type, "image/png");
    assert_eq!(STANDARD.decode(data).expect("base64"), PNG_1X1);

    let omitted = prepare_tool_image_bytes(b"BMnot-a-safe-bitmap", "image/bmp");
    assert!(omitted.block.is_none());
    assert!(omitted.note.contains("Image omitted"), "{}", omitted.note);
}

#[test]
fn blind_route_removes_nested_tool_result_image() {
    let mut messages = vec![codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "call-image".to_string(),
            content: "Read image file [image/png]".to_string(),
            is_error: None,
            content_blocks: Some(vec![serde_json::json!({
                "type": "image",
                "mime_type": "image/png",
                "data": "QUJD",
            })]),
        }],
    }];

    assert_eq!(
        strip_images_when_unsupported(&mut messages, SupportState::Unsupported, "text-only",),
        1
    );
    let ContentBlock::ToolResult {
        content,
        content_blocks,
        ..
    } = &messages[0].content[0]
    else {
        panic!("tool result")
    };
    assert!(content_blocks.is_none());
    assert!(content.contains("text-only"), "{content}");
}

#[test]
fn copy_projection_never_contains_inline_image_bytes() {
    const SENTINEL: &str = "U0VOU0lUSVZFX0JBU0U2NA==";
    let projected = safe_tool_result_content_blocks(Some(&[serde_json::json!({
        "type": "image",
        "mime_type": "image/png",
        "data": SENTINEL,
    })]))
    .expect("projection");
    let encoded = serde_json::to_string(&projected).expect("json");
    assert!(!encoded.contains(SENTINEL), "{encoded}");
    assert!(
        encoded.contains("inline_or_local_image_payload"),
        "{encoded}"
    );
}

#[test]
fn riff_that_is_not_webp_is_not_an_image() {
    // A WAV file is also RIFF. Matching on "RIFF" alone would attach audio.
    assert_eq!(sniff_media_type(b"RIFF\x00\x00\x00\x00WAVEfmt "), None);
}

#[test]
fn encodes_a_png_to_a_data_url_that_round_trips() {
    let attached = encode_image_bytes(PNG_1X1, "shot.png").expect("attach");
    assert_eq!(attached.media_type, "image/png");
    assert_eq!(attached.source_bytes, PNG_1X1.len());

    let (media_type, payload) = parse_data_url(&attached.data_url).expect("parse");
    assert_eq!(media_type, "image/png");
    assert_eq!(STANDARD.decode(payload).expect("decode"), PNG_1X1);
}

#[test]
fn rejects_a_file_over_the_size_limit() {
    let oversized = vec![0u8; MAX_IMAGE_BYTES + 1];
    let error = encode_image_bytes(&oversized, "huge.png").expect_err("must reject");
    assert!(
        matches!(error, ImageAttachError::TooLarge { .. }),
        "got {error:?}"
    );
    let rendered = error.to_string();
    assert!(rendered.contains("5.0 MB"), "{rendered}");
    assert!(rendered.contains("huge.png"), "{rendered}");
}

#[test]
fn accepts_a_file_exactly_at_the_size_limit() {
    // The boundary is inclusive; an off-by-one here would reject images
    // the providers accept.
    let mut at_limit = PNG_1X1.to_vec();
    at_limit.resize(MAX_IMAGE_BYTES, 0);
    assert!(encode_image_bytes(&at_limit, "edge.png").is_ok());
}

#[test]
fn rejects_a_real_image_in_an_unsupported_format_by_name() {
    for (bytes, name) in [
        (b"BM\x00\x00\x00\x00".as_slice(), "BMP"),
        (b"II\x2a\x00extra".as_slice(), "TIFF"),
        (b"MM\x00\x2aextra".as_slice(), "TIFF"),
        (b"<svg xmlns=".as_slice(), "SVG"),
        (b"%PDF-1.7".as_slice(), "PDF"),
    ] {
        let error = encode_image_bytes(bytes, "f").expect_err("must reject");
        match error {
            ImageAttachError::UnsupportedFormat { detected, .. } => {
                assert_eq!(detected, name);
            }
            other => panic!("expected UnsupportedFormat for {name}, got {other:?}"),
        }
    }
}

#[test]
fn rejects_a_file_that_is_not_an_image_at_all() {
    let error = encode_image_bytes(b"#!/bin/sh\necho hi\n", "script.png").expect_err("must reject");
    assert!(
        matches!(error, ImageAttachError::NotAnImage { .. }),
        "got {error:?}"
    );
}

#[test]
fn rejects_an_empty_file() {
    let error = encode_image_bytes(b"", "empty.png").expect_err("must reject");
    assert!(
        matches!(error, ImageAttachError::Empty { .. }),
        "got {error:?}"
    );
}

#[test]
fn parses_and_rejects_data_urls() {
    assert_eq!(
        parse_data_url("data:image/png;base64,QUJD"),
        Some(("image/png", "QUJD"))
    );
    // Not base64-tagged: Anthropic has no shape for a raw data URL.
    assert_eq!(parse_data_url("data:image/png,QUJD"), None);
    // Remote URLs are a different source type, not a malformed data URL.
    assert_eq!(parse_data_url("https://example.com/a.png"), None);
    // Degenerate forms must not produce an empty base64 payload that the
    // provider would reject with an opaque error.
    assert_eq!(parse_data_url("data:;base64,QUJD"), None);
    assert_eq!(parse_data_url("data:image/png;base64,"), None);
    assert_eq!(parse_data_url("data:image/png;base64"), None);
}

#[test]
fn classifies_remote_urls() {
    assert!(is_remote_image_url("https://example.com/a.png"));
    assert!(is_remote_image_url("http://example.com/a.png"));
    assert!(!is_remote_image_url("data:image/png;base64,QUJD"));
    assert!(!is_remote_image_url("file:///tmp/a.png"));
}

fn message_with_image(url: &str) -> codewhale_models::Message {
    codewhale_models::Message {
        role: Role::User,
        content: vec![
            ContentBlock::ImageUrl {
                image_url: ImageUrlContent {
                    url: url.to_string(),
                },
            },
            ContentBlock::Text {
                text: "what is this?".to_string(),
                cache_control: None,
            },
        ],
    }
}

fn write_png(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, PNG_1X1).expect("write fixture");
    path
}

#[test]
fn expands_a_placeholder_into_an_image_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_png(dir.path(), "shot.png");
    let text = format!("look at this\n[Attached image: {}]", path.display());

    let expanded = expand_attachment_blocks(&text);

    assert!(expanded.notices.is_empty(), "{expanded:?}");
    // Bracketed: open tag naming the path, the image, close tag.
    assert_eq!(expanded.blocks.len(), 3, "{expanded:?}");
    match &expanded.blocks[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.starts_with("<image path=\""), "{text}");
            assert!(text.contains("shot.png"), "{text}");
        }
        other => panic!("expected an opening tag, got {other:?}"),
    }
    match &expanded.blocks[1] {
        ContentBlock::ImageUrl { image_url } => {
            assert!(image_url.url.starts_with("data:image/png;base64,"));
        }
        other => panic!("expected an image block, got {other:?}"),
    }
    assert_eq!(
        expanded.blocks[2],
        ContentBlock::Text {
            text: "</image>".to_string(),
            cache_control: None
        }
    );
}

#[test]
fn expands_multiple_placeholders_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = write_png(dir.path(), "one.png");
    let second = write_png(dir.path(), "two.png");
    std::fs::write(&second, [0xff, 0xd8, 0xff, 0xe0, 0x01]).expect("write jpeg");
    let text = format!(
        "[Attached image: {}]\nand\n[Attached image: {}]",
        first.display(),
        second.display()
    );

    let expanded = expand_attachment_blocks(&text);

    let media: Vec<_> = expanded
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ImageUrl { image_url } => Some(
                parse_data_url(&image_url.url)
                    .expect("data url")
                    .0
                    .to_string(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(media, vec!["image/png", "image/jpeg"]);

    // Each image carries its own path tag, so the model can tell two
    // screenshots in one turn apart.
    let tags: Vec<_> = expanded
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } if text.starts_with("<image path=") => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(tags.len(), 2, "{tags:?}");
    assert!(tags[0].contains("one.png"), "{tags:?}");
    assert!(tags[1].contains("two.png"), "{tags:?}");
}

#[test]
fn ingest_does_not_consult_model_capability() {
    // Capability is a route property and is re-decided per request. If
    // ingest started gating on it, attaching under a text-only model would
    // destroy the image for the rest of the session.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_png(dir.path(), "shot.png");
    let text = format!("[Attached image: {}]", path.display());

    let expanded = expand_attachment_blocks(&text);

    assert_eq!(expanded.blocks.len(), 3);
    assert!(expanded.notices.is_empty());
}

#[test]
fn a_blind_route_gets_text_in_place_of_every_image() {
    let mut messages = vec![
        message_with_image("data:image/png;base64,QUJD"),
        codewhale_models::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "sure".to_string(),
                cache_control: None,
            }],
        },
    ];

    let stripped =
        strip_images_when_unsupported(&mut messages, SupportState::Unsupported, "deepseek-chat");

    assert_eq!(stripped, 1);
    assert!(
        !messages[0]
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ImageUrl { .. })),
        "no image may survive to a route that cannot read one"
    );
    match &messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("deepseek-chat"), "{text}");
            assert!(text.contains("/model"), "{text}");
            assert!(text.contains("omitted"), "{text}");
        }
        other => panic!("expected replacement text, got {other:?}"),
    }
}

#[test]
fn a_supported_or_unknown_route_keeps_its_images() {
    // Unknown is the common case: models.dev has no modality data for most
    // routes. Stripping there would make the feature dead on arrival for
    // self-hosted and custom providers.
    for vision in [SupportState::Supported, SupportState::Unknown] {
        let mut messages = vec![message_with_image("data:image/png;base64,QUJD")];

        let stripped = strip_images_when_unsupported(&mut messages, vision, "some-model");

        assert_eq!(stripped, 0, "{vision:?} must not strip");
        assert!(
            messages[0]
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ImageUrl { .. })),
            "{vision:?} must keep the image"
        );
    }
}

#[test]
fn offline_seed_route_for_a_vision_model_keeps_the_image() {
    // #6396: the bundled seed lists Claude as text-only. An offline cold
    // start resolves the route from that seed alone; the image must still go.
    let route = codewhale_config::route::RouteResolver::new()
        .resolve(&codewhale_config::route::RouteRequest {
            explicit_provider: Some(codewhale_config::ProviderKind::Anthropic),
            model_selector: Some(codewhale_config::route::LogicalModelRef::from(
                "claude-opus-5",
            )),
            saved_provider_model: None,
            base_url_override: None,
            limit_overrides: Vec::new(),
            wire_override: None,
        })
        .expect("bundled Anthropic route resolves offline");
    let mut messages = vec![message_with_image("data:image/png;base64,QUJD")];

    let stripped = strip_images_when_unsupported(
        &mut messages,
        route.capabilities().image_input,
        "claude-opus-5",
    );

    assert_eq!(stripped, 0);
}

#[test]
fn stripping_replaces_every_image_across_every_message() {
    let mut messages = vec![
        message_with_image("data:image/png;base64,AAAA"),
        message_with_image("data:image/jpeg;base64,BBBB"),
    ];

    let stripped = strip_images_when_unsupported(&mut messages, SupportState::Unsupported, "blind");

    assert_eq!(
        stripped, 2,
        "a per-message early return would miss the second"
    );
}

#[test]
fn a_missing_file_becomes_a_notice_not_a_dropped_turn() {
    let text = "[Attached image: /nonexistent/definitely-not-here.png]";

    let expanded = expand_attachment_blocks(text);

    assert!(expanded.blocks.is_empty());
    assert_eq!(expanded.notices.len(), 1);
    assert!(
        expanded.notices[0].contains("definitely-not-here.png"),
        "{:?}",
        expanded.notices
    );
}

#[test]
fn one_bad_attachment_does_not_suppress_a_good_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = write_png(dir.path(), "good.png");
    let text = format!(
        "[Attached image: /nope/missing.png]\n[Attached image: {}]",
        good.display()
    );

    let expanded = expand_attachment_blocks(&text);

    assert_eq!(expanded.blocks.len(), 3);
    assert_eq!(expanded.notices.len(), 1);
}

#[test]
fn video_attachments_are_left_as_text() {
    let text = "[Attached video: /tmp/clip.mp4]";

    let expanded = expand_attachment_blocks(text);

    assert!(expanded.blocks.is_empty(), "{expanded:?}");
    assert!(expanded.notices.is_empty(), "{expanded:?}");
}

#[test]
fn text_with_no_attachments_produces_nothing() {
    let expanded = expand_attachment_blocks("just a normal question");
    assert!(expanded.blocks.is_empty());
    assert!(expanded.notices.is_empty());
}

#[test]
fn notice_block_names_the_failure_and_forbids_guessing() {
    assert_eq!(notice_block(&[]), None);
    let block =
        notice_block(&["Cannot attach a.png: the file is empty".to_string()]).expect("block");
    match block {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("<attachment_notice>"), "{text}");
            assert!(text.contains("a.png"), "{text}");
            assert!(text.contains("Do not describe"), "{text}");
        }
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn attach_from_path_reports_an_unreadable_file() {
    let error = attach_image_from_path(Path::new("/nonexistent/x.png")).expect_err("must fail");
    assert!(
        matches!(error, ImageAttachError::Unreadable { .. }),
        "got {error:?}"
    );
}

pub(crate) fn runtime_image_fixture(color: u8) -> codewhale_protocol::runtime::RuntimeImageInput {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([color, 31, 99, 255]),
    ));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    codewhale_protocol::runtime::RuntimeImageInput {
        mime: "image/png".into(),
        data_base64: STANDARD.encode(bytes.into_inner()),
    }
}

#[test]
fn runtime_image_validation_preserves_exact_bytes_and_rejects_corruption() {
    let input = runtime_image_fixture(7);
    let blocks = prepare_runtime_images(std::slice::from_ref(&input)).unwrap();
    assert_eq!(
        runtime_images_from_blocks(&blocks).unwrap().as_slice(),
        std::slice::from_ref(&input)
    );
    for bad in [
        codewhale_protocol::runtime::RuntimeImageInput {
            mime: "image/jpeg".into(),
            ..input.clone()
        },
        codewhale_protocol::runtime::RuntimeImageInput {
            data_base64: "not base64".into(),
            ..input.clone()
        },
        codewhale_protocol::runtime::RuntimeImageInput {
            data_base64: String::new(),
            ..input.clone()
        },
        // Existing signature sniffing alone accepted this truncated PNG.
        codewhale_protocol::runtime::RuntimeImageInput {
            data_base64: STANDARD.encode(b"\x89PNG\r\n\x1a\n"),
            ..input.clone()
        },
    ] {
        assert!(prepare_runtime_images(&[bad]).is_err());
    }
}

#[test]
fn runtime_image_validation_bounds_count_encoded_size_and_decode_dimensions() {
    let input = runtime_image_fixture(7);
    assert!(prepare_runtime_images(&vec![input.clone(); 11]).is_err());
    assert!(
        prepare_runtime_images(&[codewhale_protocol::runtime::RuntimeImageInput {
            data_base64: "A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1),
            ..input
        }])
        .is_err()
    );
    let wide = image::DynamicImage::ImageRgba8(image::RgbaImage::new(MAX_IMAGE_DIMENSION + 1, 1));
    let mut bytes = std::io::Cursor::new(Vec::new());
    wide.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    let input = codewhale_protocol::runtime::RuntimeImageInput {
        mime: "image/png".into(),
        data_base64: STANDARD.encode(bytes.into_inner()),
    };
    assert!(prepare_runtime_images(&[input]).is_err());
}

pub(crate) fn runtime_image_fixture_bytes(
    size: usize,
) -> codewhale_protocol::runtime::RuntimeImageInput {
    let mut image = runtime_image_fixture(42);
    let mut bytes = STANDARD.decode(&image.data_base64).unwrap();
    bytes.resize(size, 0);
    image.data_base64 = STANDARD.encode(bytes);
    image
}

#[test]
fn runtime_image_network_four_mib_and_historical_five_mib_bounds_are_distinct() {
    let at_network_limit = runtime_image_fixture_bytes(MAX_RUNTIME_IMAGE_BYTES);
    assert!(prepare_runtime_images(&[at_network_limit]).is_ok());
    let historical = runtime_image_fixture_bytes(MAX_RUNTIME_IMAGE_BYTES + 1);
    assert!(prepare_runtime_images(std::slice::from_ref(&historical)).is_err());
    let stored = prepare_stored_images(std::slice::from_ref(&historical)).unwrap();
    assert_eq!(runtime_images_from_blocks(&stored).unwrap(), [historical]);
    assert!(prepare_stored_images(&[runtime_image_fixture_bytes(MAX_IMAGE_BYTES)]).is_ok());
    assert!(prepare_stored_images(&[runtime_image_fixture_bytes(MAX_IMAGE_BYTES + 1)]).is_err());
    let three_mib = runtime_image_fixture_bytes(3 * 1024 * 1024);
    assert!(prepare_runtime_images(&[three_mib.clone(), three_mib.clone()]).is_err());
    assert!(prepare_stored_images(&[three_mib.clone(), three_mib]).is_ok());
    assert!(prepare_stored_images(&vec![runtime_image_fixture(1); 11]).is_ok());
}

fn noise_png(width: u32, height: u32) -> Vec<u8> {
    // A cheap xorshift so the PNG does not compress: a stand-in for a busy
    // Retina screenshot that exceeds the 5 MiB inline limit as PNG.
    let mut state = 0x2545_f491_u32;
    let buffer = image::RgbImage::from_fn(width, height, |_, _| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        // Low-amplitude gray noise: too busy for PNG, like screenshot
        // texture, yet flat enough to stay on the PNG rungs once fitted.
        let level = 96 + (state & 15) as u8;
        image::Rgb([level, level, level])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(buffer)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encode noise png");
    bytes
}

fn attached_dimensions(attached: &AttachedImage) -> (u32, u32) {
    let (_, payload) = parse_data_url(&attached.data_url).expect("data url");
    let bytes = STANDARD.decode(payload).expect("base64");
    image::load_from_memory(&bytes)
        .expect("decodable attachment")
        .to_rgb8()
        .dimensions()
}

#[test]
fn oversized_screenshot_is_downscaled_at_attach_time() {
    let png = noise_png(2880, 1800);
    assert!(
        png.len() > MAX_IMAGE_BYTES,
        "fixture must exceed the inline limit"
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("retina.png");
    std::fs::write(&path, &png).expect("write fixture");

    let attached = attach_image_from_path(&path).expect("large screenshot attaches");
    assert!(attached.source_bytes <= MAX_IMAGE_BYTES);
    let (width, height) = attached_dimensions(&attached);
    assert_eq!(width, ATTACH_MAX_EDGE_PX);
    assert!(
        (1279..=1281).contains(&height),
        "aspect ratio is kept: {height}"
    );
}

#[test]
fn small_file_with_a_long_edge_is_fitted_to_the_attach_edge() {
    let wide = image::RgbImage::from_pixel(4000, 200, image::Rgb([30, 30, 30]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(wide)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode");
    assert!(png.len() < MAX_IMAGE_BYTES);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("wide.png");
    std::fs::write(&path, &png).expect("write fixture");

    let attached = attach_image_from_path(&path).expect("attach");
    assert_eq!(attached.media_type, "image/png", "flat content stays PNG");
    let (width, height) = attached_dimensions(&attached);
    assert_eq!(width, ATTACH_MAX_EDGE_PX);
    assert!(
        (102..=103).contains(&height),
        "aspect ratio is kept: {height}"
    );
}

#[test]
fn small_image_attaches_byte_for_byte() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dot.png");
    std::fs::write(&path, PNG_1X1).expect("write fixture");
    let attached = attach_image_from_path(&path).expect("attach");
    let (_, payload) = parse_data_url(&attached.data_url).expect("data url");
    assert_eq!(STANDARD.decode(payload).expect("base64"), PNG_1X1);
}

#[test]
fn only_images_since_the_latest_prompt_count_as_this_turns() {
    let image = || ContentBlock::ImageUrl {
        image_url: ImageUrlContent {
            url: "data:image/png;base64,AAAA".to_string(),
        },
    };
    let text = |text: &str| ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    };
    let old_turn = codewhale_models::Message {
        role: Role::User,
        content: vec![text("earlier screenshot"), image()],
    };
    let reply = codewhale_models::Message {
        role: Role::Assistant,
        content: vec![text("seen")],
    };
    let prompt = |content| codewhale_models::Message {
        role: Role::User,
        content,
    };
    let tool_image = codewhale_models::Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: "call".to_string(),
            content: "Read image".to_string(),
            is_error: None,
            content_blocks: Some(vec![serde_json::json!({"type": "image"})]),
        }],
    };

    let history_only = vec![old_turn.clone(), reply.clone(), prompt(vec![text("go on")])];
    assert_eq!(images_since_last_user_prompt(&history_only), 0);

    let fresh = vec![
        old_turn,
        reply,
        prompt(vec![text("look"), image()]),
        tool_image,
    ];
    assert_eq!(images_since_last_user_prompt(&fresh), 2);
}

/// The founder's failing drop: a macOS screencaptureui temp file whose name
/// has spaces, delivered as a shell-escaped path.
fn dropped_screenshot(dir: &std::path::Path) -> std::path::PathBuf {
    write_png(dir, "Screenshot 2026-10-04 at 22.25.47.png")
}

#[cfg(not(windows))]
#[test]
fn a_dropped_path_in_every_terminal_spelling_is_an_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let plain = shot.display().to_string();
    let escaped = plain.replace(' ', "\\ ");
    let file_url = url::Url::from_file_path(&shot)
        .expect("file url")
        .to_string();
    assert!(file_url.contains("%20"), "{file_url}");

    for spelling in [
        escaped.clone(),
        plain.clone(),
        format!("\"{plain}\""),
        format!("'{plain}'"),
        file_url,
        format!("  {escaped}\n"),
    ] {
        assert_eq!(
            pasted_image_paths(&spelling),
            Some(vec![shot.clone()]),
            "{spelling}"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn several_dropped_images_attach_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let second = write_png(dir.path(), "second.png");
    let text = format!(
        "{} {}",
        shot.display().to_string().replace(' ', "\\ "),
        second.display()
    );
    assert_eq!(pasted_image_paths(&text), Some(vec![shot, second]));
}

#[test]
fn a_paste_that_is_not_only_image_paths_stays_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "hello").expect("write notes");
    let escaped = shot.display().to_string().replace(' ', "\\ ");

    for text in [
        format!("look at {escaped}"),
        "shot.png".to_string(),
        notes.display().to_string(),
        dir.path().join("missing.png").display().to_string(),
        format!("{escaped} {}", notes.display()),
        String::new(),
    ] {
        assert_eq!(pasted_image_paths(&text), None, "{text}");
    }
}

#[cfg(not(windows))]
#[test]
fn a_dropped_screenshot_becomes_an_image_part_or_a_text_only_notice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let paths = pasted_image_paths(&shot.display().to_string().replace(' ', "\\ "))
        .expect("dropped path is an image");
    let text = format!(
        "what is wrong here?\n[Attached image: {}]",
        paths[0].display()
    );

    let expanded = expand_attachment_blocks(&text);
    assert!(expanded.notices.is_empty(), "{expanded:?}");
    let mut messages = vec![codewhale_models::Message {
        role: Role::User,
        content: expanded.blocks,
    }];
    assert!(
        messages[0]
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ImageUrl { image_url } if image_url.url.starts_with("data:image/png;base64,"))),
        "a vision route receives the image itself"
    );

    let stripped =
        strip_images_when_unsupported(&mut messages, SupportState::Unsupported, "text-only-model");
    assert_eq!(stripped, 1);
    assert!(messages[0].content.iter().any(|block| matches!(
        block,
        ContentBlock::Text { text, .. } if text.contains("text-only-model") && text.contains("does not accept image input")
    )));
}

#[test]
fn only_an_image_the_user_attached_is_admitted_outside_the_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let other = write_png(dir.path(), "other.png");
    let text = |text: String| ContentBlock::Text {
        text,
        cache_control: None,
    };
    let messages = vec![
        codewhale_models::Message {
            role: Role::User,
            content: vec![text(format!("see\n[Attached image: {}]", shot.display()))],
        },
        codewhale_models::Message {
            role: Role::Assistant,
            content: vec![text(format!("[Attached image: {}]", other.display()))],
        },
    ];

    let references = user_attached_image_references(&messages);
    assert_eq!(references, vec![shot.display().to_string()]);
    let canonical = std::fs::canonicalize(&shot).expect("canonical");
    assert_eq!(
        resolve_user_attached_image(&references, &shot.display().to_string()),
        Some(canonical)
    );
    assert_eq!(
        resolve_user_attached_image(&references, &other.display().to_string()),
        None,
        "model-authored text never widens what a read may open"
    );
    assert_eq!(
        resolve_user_attached_image(&references, "Screenshot.png"),
        None
    );

    std::fs::write(&shot, b"no longer an image").expect("overwrite");
    assert_eq!(
        resolve_user_attached_image(&references, &shot.display().to_string()),
        None,
        "admission requires image bytes, not just an attached name"
    );
}

#[test]
fn read_downscales_an_oversized_screenshot_instead_of_omitting_it() {
    let png = noise_png(2880, 1800);
    assert!(png.len() > MAX_IMAGE_BYTES, "fixture must exceed the limit");

    let prepared = prepare_tool_image_bytes(&png, "image/png");
    let codewhale_tools::ToolResultContentBlock::Image { mime_type, data } = prepared
        .block
        .expect("oversized screenshot is still delivered");
    let bytes = STANDARD.decode(data).expect("base64");
    assert!(bytes.len() <= crate::tools::read_media::READ_IMAGE_BYTE_BUDGET);
    assert_eq!(sniff_media_type(&bytes), Some(mime_type.as_str()));
    assert!(prepared.note.contains("re-encoded"), "{}", prepared.note);
}

#[cfg(not(windows))]
#[test]
fn a_message_that_starts_with_a_dropped_image_attaches_it_and_keeps_the_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let plain = shot.display().to_string();
    let escaped = plain.replace(' ', "\\ ");
    let question = "why does it show jobs 2?";

    for message in [
        format!("{escaped}  {question}"),
        format!("{plain} {question}"),
        format!("'{plain}' {question}"),
        format!("{escaped}\n{question}"),
    ] {
        assert_eq!(
            leading_dropped_image(&message),
            Some((shot.clone(), question.to_string())),
            "{message}"
        );
    }
    assert_eq!(leading_dropped_image(&escaped), Some((shot, String::new())));
}

#[test]
fn a_message_that_only_mentions_an_image_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shot = dropped_screenshot(dir.path());
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "hello").expect("write notes");

    for message in [
        format!("look at {}", shot.display()),
        format!("{} what is this", notes.display()),
        format!("{} what is this", dir.path().join("missing.png").display()),
        "/model".to_string(),
        "shot.png what".to_string(),
    ] {
        assert_eq!(leading_dropped_image(&message), None, "{message}");
    }
}
