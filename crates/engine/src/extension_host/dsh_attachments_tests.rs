use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use codewhale_config::OwnerRef;
use codewhale_models::{ContentBlock, ImageUrlContent, Message, Role};
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::host_services::{
    HostService, PersonVia, PluginApprover, ReviewApproval, TestOffer,
};
use codewhale_plugins::registry::granted_host_services;
use codewhale_runtime::hashing::sha256_hex;
use codewhale_tools::ToolResultContentBlock;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::super::core_call::InvocationGuard;
use super::super::dsh_broker::{BrokerActTool, BrokerCall, BrokerContext, TABLE, TestGrants};
use super::super::protocol::{
    ContentBlockWire, DshBrokerParams, DshService, RpcErrorWire, ToolResultWire, error_code,
};
use super::super::supervisor::HostRequestContext;
use super::super::test_support::{FixturePlugins, fake_authority, host_tool, plugin_id};
use super::super::tool::{resolve_held_image, wire_to_result};
use super::super::{
    ExtensionHostManager, ExtensionHostOptions, HostCohort, OwnerState, TestManagerGuard,
};
use crate::image_attach::test_support::PNG_1X1;
use crate::image_attach::{
    MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION, bound_rich_tool_result, encode_image_bytes,
};
use crate::rlm::session::SessionObjectSnapshot;
use crate::tools::codemode::{
    ExtensionCaller, NestedCallGate, NestedCallRequest, NestedCallVerdict, NestedDecision,
};
use crate::tools::spec::{RichToolResult, ToolContext, ToolError};

const HOST_GENERATION: u64 = 7;

fn serve_acts(mut requests: mpsc::Receiver<NestedCallRequest>) {
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            if let NestedCallRequest::Tool(tool_request) = request {
                let _ = tool_request.reply.send(NestedCallVerdict::Run {
                    name: tool_request.name,
                    input: tool_request.input,
                    supports_parallel: true,
                    decision: NestedDecision::Auto,
                    hook_context: None,
                });
            }
        }
    });
}

struct Seam {
    manager: Arc<ExtensionHostManager>,
    owner: OwnerRef,
    gate: NestedCallGate,
    _grants: TestGrants,
}

fn seam(plugin: &str) -> Seam {
    let manager = ExtensionHostManager::new(ExtensionHostOptions::default());
    let owner = {
        let mut registry = manager.shared.registry.lock().unwrap();
        let owner = registry
            .begin_owner(
                HostCohort::reviewed(),
                plugin,
                "Attachments Fixture",
                Some(fake_authority(plugin)),
                "hash",
            )
            .unwrap();
        registry.mark_active(&owner);
        owner
    };
    let (tx_event, mut rx_event) = mpsc::channel(8);
    tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
    let (gate, requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
    serve_acts(requests);
    Seam {
        manager: Arc::new(manager),
        owner,
        gate: gate.for_extension(
            ExtensionCaller {
                origin: format!("extension:{plugin}"),
                tool: "attach_tool".to_string(),
                scope: format!("ext:{plugin}@hash"),
            },
            BrokerActTool::all().to_vec(),
        ),
        _grants: TestGrants::only(&[DshService::Attachments]),
    }
}

impl Seam {
    fn context_with_messages(&self, session: &str, messages: Vec<Message>) -> ToolContext {
        ToolContext::new(std::env::temp_dir())
            .with_session_objects(SessionObjectSnapshot::new(
                session.into(),
                "fixture-model".into(),
                std::env::temp_dir(),
                None,
                Arc::new(messages),
            ))
            .with_origin_turn_id("attach-turn")
    }

    fn context(&self, session: &str) -> ToolContext {
        self.context_with_messages(session, Vec::new())
    }

    fn begin(&self, context: &ToolContext, call_id: &str) -> InvocationGuard {
        self.manager
            .shared
            .core_calls
            .begin(
                HostCohort::reviewed(),
                HOST_GENERATION,
                &self.owner,
                call_id,
                self.gate.extension().unwrap().0,
                context,
                &self.gate,
            )
            .expect("an invocation")
    }

    async fn call_in(
        &self,
        cx: HostRequestContext,
        guard: &InvocationGuard,
        method: &str,
        input: Value,
    ) -> Result<Value, RpcErrorWire> {
        let ticket = guard
            .broker_tickets()
            .unwrap()
            .into_iter()
            .find(|wire| wire.service == "attachments")
            .unwrap()
            .ticket;
        self.manager
            .shared
            .core_calls
            .serve_broker_in(
                TABLE,
                &self.manager.shared,
                HostCohort::reviewed(),
                HOST_GENERATION,
                DshBrokerParams {
                    owner: self.owner.clone(),
                    ticket,
                    service: "attachments".to_string(),
                    method: method.to_string(),
                    input,
                },
                cx,
            )
            .await
    }

    async fn call(
        &self,
        guard: &InvocationGuard,
        method: &str,
        input: Value,
    ) -> Result<Value, RpcErrorWire> {
        let (cx, _violations, _cancel) = HostRequestContext::for_test(40);
        self.call_in(cx, guard, method, input).await
    }
}

fn wire_refusal(result: &Result<Value, RpcErrorWire>) -> (i64, String, String) {
    let error = result.as_ref().expect_err("a refusal");
    let code = error
        .data
        .as_ref()
        .and_then(|data| data["dsh_refusal"].as_str())
        .unwrap_or_else(|| panic!("untyped refusal: {error:?}"))
        .to_string();
    (error.code, code, error.message.clone())
}

fn sample_rgba_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
    let image =
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba([r, g, b, 255])));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn png_with_ihdr_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = Vec::with_capacity(17);
    ihdr.extend_from_slice(b"IHDR");
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let crc = crc32(&ihdr);

    let mut out = Vec::new();
    out.extend_from_slice(&PNG_1X1[..8]);
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(&ihdr);
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(&PNG_1X1[33..]);
    out
}

fn user_message_with_png(bytes: &[u8]) -> (Message, String) {
    let attached = encode_image_bytes(bytes, "user.png").unwrap();
    let b64 = STANDARD.encode(bytes);
    let id = format!("sha256:{}", sha256_hex(bytes));
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::ImageUrl {
            image_url: ImageUrlContent {
                url: attached.data_url,
            },
        }],
    };
    let _ = b64;
    (message, id)
}

#[tokio::test]
async fn an_image_saved_by_a_plugin_lands_on_its_own_tool_result_and_reads_back_by_id() {
    let s = seam("att-save-read");
    let png = sample_rgba_png(8, 6, 12, 180, 90);
    let (user_msg, user_id) = user_message_with_png(&png);
    let ctx = s.context_with_messages("att-s1", vec![user_msg]);
    let guard = s.begin(&ctx, "call-save");

    let valid = s
        .call(
            &guard,
            "validate",
            json!({
                "data_base64": STANDARD.encode(PNG_1X1),
                "media_type": "image/png",
                "name": "studio-image",
            }),
        )
        .await
        .unwrap();
    assert_eq!(valid["ok"], true);
    assert_eq!(valid["media_type"], "image/png");

    let saved = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(PNG_1X1),
                "media_type": "image/png",
                "name": "studio-image",
            }),
        )
        .await
        .unwrap();
    let attachment = saved["attachment"].clone();
    let id = attachment["attachment_id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 71);
    assert!(id.starts_with("sha256:"));

    let read_saved = s
        .call(&guard, "read", json!({ "attachment": attachment.clone() }))
        .await
        .unwrap();
    let stored_bytes = STANDARD
        .decode(read_saved["data_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(format!("sha256:{}", sha256_hex(&stored_bytes)), id);
    assert_eq!(read_saved["attachment"]["bytes"], stored_bytes.len());

    let read_user = s
        .call(
            &guard,
            "read",
            json!({
                "attachment": {
                    "attachment_id": user_id,
                    "media_type": "image/png",
                    "bytes": png.len(),
                    "width": 8,
                    "height": 6,
                }
            }),
        )
        .await
        .unwrap();
    let user_roundtrip = STANDARD
        .decode(read_user["data_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(user_roundtrip, png);

    let thumb = s
        .call(
            &guard,
            "read_request",
            json!({
                "attachment": { "attachment_id": user_id },
                "width": 4,
                "height": 4,
                "max_bytes": 98_304,
            }),
        )
        .await
        .unwrap();
    let thumb_bytes = STANDARD
        .decode(thumb["data_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        thumb["variant_id"],
        format!("sha256:{}", sha256_hex(&thumb_bytes))
    );
    assert!(thumb_bytes.len() <= 98_304);

    let held = guard.seal_held_image();
    let mut settled = Ok(ToolResultWire {
        content: vec![
            ContentBlockWire::Text {
                text: "saved".to_string(),
            },
            ContentBlockWire::ImageRef {
                attachment_id: id.clone(),
            },
        ],
        is_error: false,
        structured: None,
    });
    let blocks = resolve_held_image(held, true, &mut settled, Some(&guard));
    let wire = settled.unwrap();
    let rich = bound_rich_tool_result(RichToolResult::with_content_blocks(
        wire_to_result(wire, "extension:att-save-read"),
        blocks,
    ));
    assert_eq!(rich.content_blocks.len(), 1);
    let ToolResultContentBlock::Image { mime_type, data } = &rich.content_blocks[0];
    assert_eq!(
        mime_type,
        read_saved["attachment"]["media_type"].as_str().unwrap()
    );
    assert_eq!(STANDARD.decode(data).unwrap(), stored_bytes);

    let receipts = guard.seal_receipts().unwrap();
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(acts.len(), 11);
    assert!(acts.iter().all(|act| act["service"] == "attachments"));
    assert_eq!(
        acts.iter()
            .filter(|act| act.get("effect").is_some())
            .count(),
        5
    );
    let last = acts.last().unwrap();
    assert_eq!(last["method"], "result");
    assert_eq!(last["outcome"], "delivered");
    let serialized = receipts.to_string();
    assert!(!serialized.contains("data:image/"));
    assert!(!serialized.contains(STANDARD.encode(PNG_1X1).as_str()));
}

#[tokio::test]
async fn an_image_block_naming_an_id_the_call_did_not_save_fails_the_tool() {
    let s = seam("att-unheld-ref");
    let ctx = s.context("att-s2");

    let unheld_guard = s.begin(&ctx, "call-none");
    let mut settled_none = Ok(ToolResultWire {
        content: vec![ContentBlockWire::ImageRef {
            attachment_id: format!("sha256:{}", "a".repeat(64)),
        }],
        is_error: false,
        structured: None,
    });
    let blocks_none = resolve_held_image(
        unheld_guard.seal_held_image(),
        true,
        &mut settled_none,
        Some(&unheld_guard),
    );
    assert!(blocks_none.is_empty());
    let err = settled_none.unwrap_err();
    assert!(err.to_string().contains("not held by this call"), "{err}");

    let wrong_guard = s.begin(&ctx, "call-wrong");
    let _saved = wrong_guard_save(&s, &wrong_guard).await;
    let mut settled_wrong = Ok(ToolResultWire {
        content: vec![ContentBlockWire::ImageRef {
            attachment_id: format!("sha256:{}", "b".repeat(64)),
        }],
        is_error: false,
        structured: None,
    });
    let blocks_wrong = resolve_held_image(
        wrong_guard.seal_held_image(),
        true,
        &mut settled_wrong,
        Some(&wrong_guard),
    );
    assert!(blocks_wrong.is_empty());
    assert!(settled_wrong.is_err());
    let receipts = wrong_guard.seal_receipts().unwrap();
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(acts.last().unwrap()["outcome"], "dropped");
    assert_eq!(acts.last().unwrap()["reason"], "unheld_reference");
}

async fn wrong_guard_save(s: &Seam, guard: &InvocationGuard) -> Value {
    s.call(
        guard,
        "save",
        json!({
            "data_base64": STANDARD.encode(PNG_1X1),
            "media_type": "image/png",
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_held_image_the_result_does_not_name_is_dropped_and_recorded() {
    let s = seam("att-drop-unreferenced");
    let ctx = s.context("att-s3");
    let guard = s.begin(&ctx, "call-unref");
    let _ = wrong_guard_save(&s, &guard).await;

    let mut settled = Ok(ToolResultWire {
        content: vec![ContentBlockWire::Text {
            text: "forgot image".to_string(),
        }],
        is_error: false,
        structured: None,
    });
    let blocks = resolve_held_image(guard.seal_held_image(), true, &mut settled, Some(&guard));
    assert!(blocks.is_empty());
    assert!(settled.is_ok());
    let receipts = guard.seal_receipts().unwrap();
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(acts.len(), 3);
    assert_eq!(acts[2]["outcome"], "dropped");
    assert_eq!(acts[2]["reason"], "not_referenced");

    let failed_guard = s.begin(&ctx, "call-fail");
    let _ = wrong_guard_save(&s, &failed_guard).await;
    let mut failed: Result<ToolResultWire, ToolError> = Err(ToolError::execution_failed("boom"));
    let failed_blocks = resolve_held_image(
        failed_guard.seal_held_image(),
        true,
        &mut failed,
        Some(&failed_guard),
    );
    assert!(failed_blocks.is_empty());
    let fail_receipts = failed_guard.seal_receipts().unwrap();
    let fail_acts = fail_receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(fail_acts.len(), 3);
    assert_eq!(fail_acts[2]["outcome"], "dropped");
    assert_eq!(fail_acts[2]["reason"], "tool_failed");
}

#[tokio::test]
async fn a_second_save_in_one_tool_call_is_refused_with_a_reason() {
    let s = seam("att-second-save");
    let ctx = s.context("att-s4");
    let guard = s.begin(&ctx, "call-two");
    let _first = wrong_guard_save(&s, &guard).await;

    let second = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(PNG_1X1),
                "media_type": "image/png",
            }),
        )
        .await;
    let (rpc, refusal, msg) = wire_refusal(&second);
    assert_eq!(rpc, error_code::REFUSED);
    assert_eq!(refusal, "refused");
    assert!(msg.contains("TOO_MANY_IMAGES"), "{msg}");
    assert!(
        msg.contains("this Codewhale keeps one image per tool result"),
        "{msg}"
    );
    let _ = resolve_held_image(
        guard.seal_held_image(),
        true,
        &mut Ok(ToolResultWire {
            content: Vec::new(),
            is_error: false,
            structured: None,
        }),
        Some(&guard),
    );
    let receipts = guard.seal_receipts().unwrap();
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    let refused_record = acts
        .iter()
        .find(|act| act["method"] == "save" && act["outcome"] == "refused")
        .expect("refused save detail record");
    assert_eq!(refused_record["reason"], "TOO_MANY_IMAGES");
}

#[tokio::test]
async fn an_id_from_another_conversation_is_refused_like_an_unknown_id() {
    let s = seam("att-foreign-conv");
    let png = sample_rgba_png(4, 4, 200, 10, 10);
    let (user_msg, conv_a_id) = user_message_with_png(&png);
    let _ctx_a = s.context_with_messages("conv-a", vec![user_msg]);
    let ctx_b = s.context_with_messages("conv-b", Vec::new());
    let guard_b = s.begin(&ctx_b, "call-b");

    let foreign = s
        .call(
            &guard_b,
            "read",
            json!({ "attachment": { "attachment_id": conv_a_id } }),
        )
        .await;
    let unknown = s
        .call(
            &guard_b,
            "read",
            json!({ "attachment": { "attachment_id": format!("sha256:{}", "0".repeat(64)) } }),
        )
        .await;
    assert_eq!(wire_refusal(&foreign), wire_refusal(&unknown));
    assert_eq!(wire_refusal(&foreign).0, error_code::REFUSED);
    assert!(wire_refusal(&foreign).2.contains("ATTACHMENT_NOT_FOUND"));
}

#[tokio::test]
async fn a_declared_type_that_differs_from_the_bytes_is_refused() {
    let s = seam("att-bad-type");
    let ctx = s.context("att-s6");
    let guard = s.begin(&ctx, "call-type");

    let mismatch = s
        .call(
            &guard,
            "validate",
            json!({
                "data_base64": STANDARD.encode(PNG_1X1),
                "media_type": "image/jpeg",
            }),
        )
        .await;
    assert!(wire_refusal(&mismatch).2.contains("IMAGE_TYPE_MISMATCH"));

    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0u8; 32]);
    let bmp_res = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(&bmp),
                "media_type": "image/png",
            }),
        )
        .await;
    assert!(wire_refusal(&bmp_res).2.contains("UNSUPPORTED_IMAGE_TYPE"));

    let truncated = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(&PNG_1X1[..20]),
                "media_type": "image/png",
            }),
        )
        .await;
    assert!(wire_refusal(&truncated).2.contains("INVALID_IMAGE"));

    let bad_b64 = s
        .call(
            &guard,
            "validate",
            json!({
                "data_base64": "!!!not-base64!!!",
                "media_type": "image/png",
            }),
        )
        .await;
    assert!(wire_refusal(&bad_b64).2.contains("INVALID_IMAGE_BASE64"));
}

#[tokio::test]
async fn an_image_over_the_byte_pixel_or_dimension_limit_is_refused_before_decoding_finishes() {
    let s = seam("att-limits");
    let ctx = s.context("att-s7");
    let guard = s.begin(&ctx, "call-limits");

    let mut too_many_bytes = PNG_1X1.to_vec();
    too_many_bytes.resize(MAX_IMAGE_BYTES + 1, 0);
    let over_bytes = s
        .call(
            &guard,
            "validate",
            json!({
                "data_base64": STANDARD.encode(&too_many_bytes),
                "media_type": "image/png",
            }),
        )
        .await;
    assert!(wire_refusal(&over_bytes).2.contains("IMAGE_TOO_LARGE"));

    let wide_header = png_with_ihdr_dimensions(MAX_IMAGE_DIMENSION + 1, 1);
    let over_dim = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(&wide_header),
                "media_type": "image/png",
            }),
        )
        .await;
    assert!(
        wire_refusal(&over_dim)
            .2
            .contains("IMAGE_DIMENSION_TOO_LARGE")
    );

    let pixel_bomb = png_with_ihdr_dimensions(6000, 6000);
    let over_pixels = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(&pixel_bomb),
                "media_type": "image/png",
            }),
        )
        .await;
    assert!(
        wire_refusal(&over_pixels)
            .2
            .contains("IMAGE_TOO_MANY_PIXELS")
    );
}

#[tokio::test]
async fn a_revoked_or_disabled_owner_cannot_read_or_save() {
    let s = seam("att-revoked");
    let ctx = s.context("att-s8");
    let guard = s.begin(&ctx, "call-revoke");
    let _saved = wrong_guard_save(&s, &guard).await;

    s.manager.shared.core_calls.revoke_owner("att-revoked");
    let after_revoke = s
        .call(
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(PNG_1X1),
                "media_type": "image/png",
            }),
        )
        .await;
    assert_eq!(wire_refusal(&after_revoke).0, error_code::REFUSED);
    assert_eq!(wire_refusal(&after_revoke).1, "ticket_refused");

    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Attachments]);
    let temp = codewhale_plugins::test_fixture::PluginTempDir::new().unwrap();
    let root = temp.path();
    let config = DiscoveryConfig {
        workspace: root.join("project"),
        user_plugins_dir: root.join("user"),
        workspace_plugins_dir: root.join("workspace"),
        builtin_plugin_dirs: Vec::new(),
        state_path: root.join("state/plugin-state.json"),
    };
    std::fs::create_dir_all(&config.workspace).unwrap();
    let write_plugin = |name: &str, with_service: bool| {
        let dir = config.user_plugins_dir.join(name);
        std::fs::create_dir_all(dir.join("native")).unwrap();
        std::fs::write(dir.join("native/index.mjs"), "export function apply() {}\n").unwrap();
        let cap = if with_service {
            "[capabilities]\nhost_services = [\"attachments\"]\n"
        } else {
            ""
        };
        std::fs::write(
            dir.join("plugin.toml"),
            format!(
                "schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n[native]\npath = \"native/index.mjs\"\n{cap}"
            ),
        )
        .unwrap();
    };
    write_plugin("alpha", true);
    let mut reg = discover_with_config(&config);
    let token = reg.host_service_review("alpha").unwrap().token;
    reg.trust_reviewed(
        "alpha",
        ReviewApproval {
            approved_by: PluginApprover::person(PersonVia::Terminal),
            host_services: Some(token),
        },
    )
    .unwrap();
    reg.enable("alpha").unwrap();
    let reg = discover_with_config(&config);
    let alpha_auth = reg.authority_for("alpha").unwrap();
    assert!(
        granted_host_services(&alpha_auth)
            .unwrap()
            .contains(&HostService::Attachments)
    );

    write_plugin("beta", false);
    let mut reg = discover_with_config(&config);
    reg.trust_reviewed(
        "beta",
        ReviewApproval {
            approved_by: PluginApprover::person(PersonVia::Terminal),
            host_services: None,
        },
    )
    .unwrap();
    reg.enable("beta").unwrap();
    let reg = discover_with_config(&config);
    let beta_auth = reg.authority_for("beta").unwrap();
    assert!(
        !granted_host_services(&beta_auth)
            .unwrap()
            .contains(&HostService::Attachments)
    );
}

#[tokio::test]
async fn no_attachments_method_takes_a_path() {
    let s = seam("att-no-paths");
    let ctx = s.context("att-s9");
    let guard = s.begin(&ctx, "call-path");
    let b64 = STANDARD.encode(PNG_1X1);
    let valid_id = format!("sha256:{}", sha256_hex(PNG_1X1));

    for field in ["path", "url", "hostPath", "host_path"] {
        let mut validate_input = json!({ "data_base64": b64, "media_type": "image/png" });
        validate_input[field] = json!("/etc/passwd");
        let res = s.call(&guard, "validate", validate_input).await;
        assert_eq!(wire_refusal(&res).1, "invalid_input", "{field}");

        let mut save_input = json!({ "data_base64": b64, "media_type": "image/png" });
        save_input[field] = json!("/etc/passwd");
        let res = s.call(&guard, "save", save_input).await;
        assert_eq!(wire_refusal(&res).1, "invalid_input", "{field}");

        let mut read_input = json!({ "attachment": { "attachment_id": valid_id } });
        read_input[field] = json!("/etc/passwd");
        let res = s.call(&guard, "read", read_input).await;
        assert_eq!(wire_refusal(&res).1, "invalid_input", "{field}");

        let mut req_input = json!({ "attachment": { "attachment_id": valid_id }, "width": 32, "height": 32, "max_bytes": 4096 });
        req_input[field] = json!("/etc/passwd");
        let res = s.call(&guard, "read_request", req_input).await;
        assert_eq!(wire_refusal(&res).1, "invalid_input", "{field}");
    }

    for bad_name in ["../escape.png", "a/b.png", "a\\b.png", "C:file", ".hidden"] {
        let res = s
            .call(
                &guard,
                "save",
                json!({
                    "data_base64": b64,
                    "media_type": "image/png",
                    "name": bad_name,
                }),
            )
            .await;
        assert_eq!(wire_refusal(&res).1, "invalid_input", "{bad_name}");
    }

    for bad_id in [
        "/etc/passwd",
        "https://evil.example/x.png",
        "C:\\tmp\\x.png",
        "sha256:ZZZZ",
        "not-sha",
    ] {
        let res = s
            .call(
                &guard,
                "read",
                json!({ "attachment": { "attachment_id": bad_id } }),
            )
            .await;
        assert!(wire_refusal(&res).2.contains("INVALID_ATTACHMENT_REF"));
    }
}

#[tokio::test]
async fn a_read_with_no_conversation_snapshot_is_refused() {
    let s = seam("att-no-snap");
    let ctx = ToolContext::new(std::env::temp_dir()).with_origin_turn_id("no-snap");
    let id = format!("sha256:{}", sha256_hex(PNG_1X1));
    let guard = s.manager.shared.core_calls.begin_scoped(
        HostCohort::reviewed(),
        HOST_GENERATION,
        &s.owner,
        "call-no-snap",
        s.gate.extension().unwrap().0,
        &ctx,
        &s.gate,
        None,
        String::new(),
        &[DshService::Attachments],
    );
    assert!(
        guard
            .as_ref()
            .and_then(|g| g.broker_tickets())
            .is_none_or(|tickets| tickets.is_empty()),
        "no session_objects yields no broker ticket"
    );

    let mut call_ctx = BrokerContext::detached_for_test(&s.owner, DshService::Attachments);
    let _ = &mut call_ctx;
    let res = super::serve(BrokerCall {
        owner: s.owner.clone(),
        session_id: "missing-snap".into(),
        service: DshService::Attachments,
        method: "read".into(),
        input: json!({ "attachment": { "attachment_id": id } }),
        context: call_ctx,
    })
    .await;
    let err = res.unwrap_err();
    assert_eq!(err.code(), "refused");
}

#[tokio::test]
async fn an_arm_whose_cancel_has_fired_has_no_effect() {
    let s = seam("att-cancel");
    let ctx = s.context("att-s11");
    let guard = s.begin(&ctx, "call-cancel");

    let (cx, _violations, host_cancel) = HostRequestContext::for_test(1);
    host_cancel.cancel();
    let cancelled = s
        .call_in(
            cx,
            &guard,
            "save",
            json!({
                "data_base64": STANDARD.encode(PNG_1X1),
                "media_type": "image/png",
            }),
        )
        .await;
    assert_eq!(
        (wire_refusal(&cancelled).0, wire_refusal(&cancelled).1),
        (error_code::CANCELLED, "cancelled".to_string())
    );
    assert!(guard.seal_held_image().is_none());

    let detached = BrokerContext::detached_for_test(&s.owner, DshService::Attachments);
    detached.cancel().cancel();
    let direct = super::serve(BrokerCall {
        owner: s.owner.clone(),
        session_id: "att-s11".into(),
        service: DshService::Attachments,
        method: "save".into(),
        input: json!({
            "data_base64": STANDARD.encode(PNG_1X1),
            "media_type": "image/png",
        }),
        context: detached,
    })
    .await;
    assert_eq!(direct.unwrap_err().code(), "refused");
}

#[test]
fn a_state_file_without_attachments_or_with_unrecognized_grant_reads_as_before() {
    assert_eq!(
        HostService::parse("attachments"),
        Some(HostService::Attachments)
    );
    assert!(codewhale_plugins::host_services::offered(
        HostService::Attachments
    ));

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let state_path = root.join("plugin-state.json");
    std::fs::write(
        &state_path,
        json!({
            "version": 1,
            "plugins": {
                "legacy-plugin": {
                    "trusted": true,
                    "enabled": true,
                    "content_hash": "abc",
                    "host_services": ["network"]
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let cfg = DiscoveryConfig {
        workspace: root.join("workspace"),
        user_plugins_dir: root.join("user"),
        workspace_plugins_dir: root.join("workspace/.codewhale/plugins"),
        builtin_plugin_dirs: Vec::new(),
        state_path,
    };
    let registry = discover_with_config(&cfg);
    assert!(registry.list().is_empty());
}

#[tokio::test]
async fn a_real_node_host_enforces_attachments_registration_and_projects_image_ref_blocks() {
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        codewhale_config::extension_host::ExtensionHostRuntime::Node,
        None,
        None,
        super::super::bundle_sha256(),
    );
    let failure = resolution.failure();
    let node = resolution
        .selected
        .unwrap_or_else(|| panic!("Node runtime is required: {failure}"))
        .path;

    let _policy = TestPolicyGuard::extension_host(true);
    let _offer = TestOffer::only(&[HostService::Attachments]);
    let _grants = TestGrants::only(&[DshService::Attachments]);

    let unreg_fixture = FixturePlugins::new(&["dsh-attachments"]).await;
    let unreg_id = plugin_id(&unreg_fixture, "dsh-attachments");
    let unreg_manager = unreg_fixture.manager(node.clone());
    let unreg_engine = unreg_manager.attach(unreg_fixture.registry());
    unreg_engine.sync().await.unwrap();
    assert!(matches!(
        unreg_manager.owner_state(&unreg_id),
        Some(OwnerState::Ready)
    ));
    unreg_manager.shutdown().await;

    let fixture = FixturePlugins::new(&["dsh-attachments-render"]).await;
    let manager = fixture.manager(node);
    let engine = manager.attach(fixture.registry());
    engine.sync().await.unwrap();
    let _manager_guard = TestManagerGuard::install(Arc::clone(&manager));

    let tool = host_tool(&engine, fixture.workspace(), "render_attachment_result");
    let (tx_event, mut rx_event) = mpsc::channel(16);
    tokio::spawn(async move { while rx_event.recv().await.is_some() {} });
    let (gate, requests) = NestedCallGate::new(None, tx_event, Duration::from_secs(60));
    serve_acts(requests);

    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(fixture.workspace()).with_plugin_registry(engine.plugin_view()));
    engine.install_tools(&mut registry);

    let mut context = ToolContext::new(fixture.workspace())
        .with_plugin_registry(engine.plugin_view())
        .with_session_objects(SessionObjectSnapshot::new(
            "real-host-attach".into(),
            "fixture-model".into(),
            fixture.workspace().to_path_buf(),
            None,
            Arc::new(Vec::new()),
        ))
        .with_origin_turn_id("real-host-turn");
    context.execution.nested_call_gate = Some(gate.for_extension(
        tool.extension_caller().expect("extension caller"),
        registry.all(),
    ));

    let wanted_id = format!("sha256:{}", sha256_hex(PNG_1X1));
    let unheld_err = tool
        .execute_rich(json!({ "attachmentId": wanted_id }), &context)
        .await
        .expect_err("real Node renderResult image block must fail closed when unheld");
    assert!(
        unheld_err.to_string().contains("not held by this call"),
        "{unheld_err}"
    );

    let dropped_text = tool
        .execute_rich(json!({ "textOnly": "plain reply" }), &context)
        .await
        .expect("text-only renderResult succeeds");
    assert!(dropped_text.content_blocks.is_empty());
    assert!(dropped_text.result.content.contains("plain reply"));

    {
        let _revoked = TestGrants::only(&[]);
        let revoked_err = tool
            .execute_rich(json!({ "attachmentId": wanted_id }), &context)
            .await
            .expect_err(
                "real Node renderResult image block must fail closed when grant is revoked",
            );
        assert!(
            revoked_err.to_string().contains("grant was revoked"),
            "{revoked_err}"
        );
        let text_when_revoked = tool
            .execute_rich(json!({ "textOnly": "plain still works" }), &context)
            .await
            .expect("text-only renderResult succeeds when attachments grant is absent");
        assert!(text_when_revoked.content_blocks.is_empty());
    }

    manager.shutdown().await;
}

static LATE_REVOKE_MANAGERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<ExtensionHostManager>>>,
> = std::sync::OnceLock::new();

fn revoke_owner_after_arm(call: BrokerCall) -> super::BrokerFuture {
    Box::pin(async move {
        let plugin_id = call.owner.plugin_id.clone();
        let result = super::serve(call).await;
        let manager = LATE_REVOKE_MANAGERS
            .get()
            .and_then(|map| map.lock().unwrap().get(&plugin_id).cloned());
        if let Some(manager) = manager {
            manager
                .shared
                .registry
                .lock()
                .unwrap()
                .revoke_owner(&plugin_id);
        }
        result
    })
}

#[tokio::test]
async fn late_refusal_after_save_rolls_back_held_image_without_touching_prior_valid_save() {
    let s = seam("att-late-revoke");
    let ctx = s.context("att-late-1");
    let guard = s.begin(&ctx, "call-late-1");
    let managers = LATE_REVOKE_MANAGERS.get_or_init(|| std::sync::Mutex::new(Default::default()));
    managers
        .lock()
        .unwrap()
        .insert(s.owner.plugin_id.clone(), Arc::clone(&s.manager));

    let ticket = guard
        .broker_tickets()
        .unwrap()
        .into_iter()
        .find(|wire| wire.service == "attachments")
        .unwrap()
        .ticket;
    let (cx, _violations, _cancel) = HostRequestContext::for_test(40);
    let err = s
        .manager
        .shared
        .core_calls
        .serve_broker_in(
            &[(DshService::Attachments, revoke_owner_after_arm)],
            &s.manager.shared,
            HostCohort::reviewed(),
            HOST_GENERATION,
            DshBrokerParams {
                owner: s.owner.clone(),
                ticket,
                service: "attachments".to_string(),
                method: "save".to_string(),
                input: json!({
                    "data_base64": STANDARD.encode(PNG_1X1),
                    "media_type": "image/png",
                }),
            },
            cx,
        )
        .await
        .expect_err("owner revocation after save arm completes must fail closed");
    managers.lock().unwrap().remove(&s.owner.plugin_id);
    assert!(
        err.message.contains("owner is no longer current"),
        "{err:?}"
    );
    assert!(
        guard.seal_held_image().is_none(),
        "late refusal must roll back newly staged held_image"
    );
    let receipts = guard.seal_receipts().unwrap();
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    let rolled_back = acts
        .iter()
        .find(|act| act["method"] == "save" && act["outcome"] == "refused_after_hold")
        .expect("refused_after_hold receipt record");
    assert_eq!(rolled_back["reason"], "owner_stale");
    assert_eq!(
        rolled_back["attachment_id"],
        format!("sha256:{}", sha256_hex(PNG_1X1))
    );

    let s2 = seam("att-late-preserve");
    let ctx2 = s2.context("att-late-2");
    let guard2 = s2.begin(&ctx2, "call-late-2");
    let _saved = wrong_guard_save(&s2, &guard2).await;
    managers
        .lock()
        .unwrap()
        .insert(s2.owner.plugin_id.clone(), Arc::clone(&s2.manager));
    let ticket2 = guard2
        .broker_tickets()
        .unwrap()
        .into_iter()
        .find(|wire| wire.service == "attachments")
        .unwrap()
        .ticket;
    let (cx2, _v2, _c2) = HostRequestContext::for_test(40);
    let err2 = s2
        .manager
        .shared
        .core_calls
        .serve_broker_in(
            &[(DshService::Attachments, revoke_owner_after_arm)],
            &s2.manager.shared,
            HostCohort::reviewed(),
            HOST_GENERATION,
            DshBrokerParams {
                owner: s2.owner.clone(),
                ticket: ticket2,
                service: "attachments".to_string(),
                method: "validate".to_string(),
                input: json!({
                    "data_base64": STANDARD.encode(PNG_1X1),
                    "media_type": "image/png",
                }),
            },
            cx2,
        )
        .await
        .expect_err("subsequent validate call must fail closed when owner is revoked");
    managers.lock().unwrap().remove(&s2.owner.plugin_id);
    assert!(err2.message.contains("owner is no longer current"));
    let still_held = guard2
        .seal_held_image()
        .expect("later failing broker call must not roll back earlier held_image");
    assert_eq!(
        still_held.attachment_id,
        format!("sha256:{}", sha256_hex(PNG_1X1))
    );
}

#[tokio::test]
async fn revoked_attachments_grant_prevents_image_projection_and_records_drop() {
    let s = seam("att-revoke-proj");
    let ctx = s.context("att-rev");

    let guard = s.begin(&ctx, "call-rev-ref");
    let saved = wrong_guard_save(&s, &guard).await;
    let id = saved["attachment"]["attachment_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut settled = Ok(ToolResultWire {
        content: vec![
            ContentBlockWire::Text {
                text: "image".to_string(),
            },
            ContentBlockWire::ImageRef { attachment_id: id },
        ],
        is_error: false,
        structured: None,
    });
    let blocks = resolve_held_image(guard.seal_held_image(), false, &mut settled, Some(&guard));
    assert!(blocks.is_empty());
    let err = settled.unwrap_err();
    assert!(err.to_string().contains("grant was revoked"), "{err}");
    let receipts = guard.seal_receipts().unwrap();
    let acts = receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(acts.last().unwrap()["outcome"], "dropped");
    assert_eq!(acts.last().unwrap()["reason"], "grant_revoked");

    let guard_text = s.begin(&ctx, "call-rev-text");
    let _ = wrong_guard_save(&s, &guard_text).await;
    let mut settled_text = Ok(ToolResultWire {
        content: vec![ContentBlockWire::Text {
            text: "text".to_string(),
        }],
        is_error: false,
        structured: None,
    });
    let text_blocks = resolve_held_image(
        guard_text.seal_held_image(),
        false,
        &mut settled_text,
        Some(&guard_text),
    );
    assert!(text_blocks.is_empty());
    assert!(settled_text.is_ok());
    let text_receipts = guard_text.seal_receipts().unwrap();
    let text_acts = text_receipts["broker_acts"]["acts"].as_array().unwrap();
    assert_eq!(text_acts.last().unwrap()["outcome"], "dropped");
    assert_eq!(text_acts.last().unwrap()["reason"], "grant_revoked");
}
