//! Publish the rich tool image already admitted to model history through the
//! existing immutable session evidence store for authenticated UI retrieval.

use std::io;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use codewhale_tools::ToolResultContentBlock;
use serde_json::{Value, json};

use crate::tools::large_output_router::{
    EVIDENCE_RETENTION_SECS, EvidenceArtifact, EvidenceRetentionState, evidence_is_expired,
    publish_evidence_metadata, read_evidence_metadata, unix_millis_now,
};
use crate::tools::spec::RichToolResult;

/// No plugin-supplied descriptor may cross this authority boundary. Publication
/// failure only omits the UI image; the validated model content remains intact.
pub(super) async fn project(
    mut rich: RichToolResult,
    session_id: &str,
    call_id: &str,
    tool_name: &str,
) -> RichToolResult {
    if let Some(metadata) = rich.result.metadata.as_mut().and_then(Value::as_object_mut) {
        metadata.remove("tool_media");
    }
    if rich.content_blocks.is_empty() {
        return rich;
    }
    let fallback = rich.result.clone();
    let session_id = session_id.to_owned();
    let call_id = call_id.to_owned();
    let tool_name = tool_name.to_owned();
    // Image decoding and confined disk publication must not block the reactor.
    tokio::task::spawn_blocking(move || {
        let mut rich = crate::image_attach::bound_rich_tool_result(rich);
        if rich.content_blocks.is_empty() {
            return rich;
        }
        match publish_image(&rich.content_blocks[0], &session_id, &call_id, &tool_name) {
            Ok(descriptor) => {
                let metadata = rich.result.metadata.get_or_insert_with(|| json!({}));
                if !metadata.is_object() {
                    *metadata = json!({});
                }
                metadata["tool_media"] = json!([descriptor]);
            }
            Err(_) => rich.result.content.push_str(
                "\n[Tool image preview unavailable: session evidence could not be published.]",
            ),
        }
        rich
    })
    .await
    .unwrap_or_else(|_| {
        let mut rich = RichToolResult::plain(fallback);
        rich.result
            .content
            .push_str("\n[Tool image omitted: image preparation failed.]");
        rich
    })
}

fn publish_image(
    block: &ToolResultContentBlock,
    session_id: &str,
    call_id: &str,
    tool_name: &str,
) -> io::Result<Value> {
    let ToolResultContentBlock::Image { mime_type, data } = block;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid tool image evidence");
    let bytes = STANDARD.decode(data).map_err(|_| invalid())?;
    // The caller passed the shared rich-image validator; retain the shared decode
    // guard when deriving dimensions rather than trusting metadata or headers.
    let (_, width, height) =
        crate::image_attach::decode_and_guard_image(&bytes).map_err(|_| invalid())?;
    let identity = serde_json::to_vec(&(session_id, call_id, 0u8)).map_err(|_| invalid())?;
    let handle = format!(
        "art_image_{}",
        codewhale_runtime::hashing::sha256_hex(&identity)
    );
    let storage_path = PathBuf::from(codewhale_runtime::artifacts::ARTIFACTS_DIR_NAME)
        .join(format!("{handle}.image"));
    let digest = codewhale_runtime::hashing::sha256_hex(&bytes);
    let now = unix_millis_now();
    let proposed = EvidenceArtifact {
        handle: handle.clone(),
        digest: digest.clone(),
        size_bytes: bytes.len() as u64,
        content_type: mime_type.clone(),
        tool_name: tool_name.to_owned(),
        call_id: call_id.to_owned(),
        origin_session: session_id.to_owned(),
        generation: 1,
        redacted: false,
        encoding: "binary".to_owned(),
        retention_state: EvidenceRetentionState::Live,
        created_at_unix_ms: now,
        retain_until_unix_ms: now.saturating_add(EVIDENCE_RETENTION_SECS * 1_000),
        storage_path: storage_path.clone(),
    };
    let matches_image = |existing: &EvidenceArtifact| {
        existing.digest == digest
            && existing.size_bytes == bytes.len() as u64
            && existing.content_type == *mime_type
            && existing.call_id == call_id
            && existing.tool_name == tool_name
            && existing.origin_session == session_id
            && existing.handle == handle
            && existing.storage_path == storage_path
            && existing.generation == 1
            && existing.encoding == "binary"
            && !existing.redacted
            && !evidence_is_expired(existing, unix_millis_now())
    };
    let artifact = match read_evidence_metadata(session_id, &handle) {
        Ok(existing) if matches_image(&existing) => existing,
        Ok(_) => return Err(invalid()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => proposed,
        Err(error) => return Err(error),
    };
    codewhale_runtime::artifacts::write_session_relative_immutable(
        session_id,
        &storage_path,
        &bytes,
    )?;
    if let Err(error) = publish_evidence_metadata(session_id, &artifact) {
        // Concurrent identical replays can propose different timestamps. Keep
        // the winner's immutable lifetime, never replace or extend it.
        if error.kind() != io::ErrorKind::AlreadyExists
            || !matches_image(&read_evidence_metadata(session_id, &handle)?)
        {
            return Err(error);
        }
    }
    Ok(json!({
        "version": 1, "session_id": session_id, "artifact_id": handle,
        "tool_call_id": call_id, "media_type": mime_type, "byte_size": bytes.len(),
        "sha256": digest, "width": width, "height": height,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolResult;

    struct ArtifactRoot(Option<PathBuf>);
    impl Drop for ArtifactRoot {
        fn drop(&mut self) {
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(self.0.take());
        }
    }

    fn image_result(pixel: u8, mime: &str) -> RichToolResult {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([pixel, 0, 0, 255]),
        ))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
        let mut result = ToolResult::success("Captured observation".to_owned());
        result.metadata = Some(json!({"tool_media":[{"artifact_id":"forged"}],"keep":true}));
        RichToolResult::with_content_blocks(
            result,
            vec![ToolResultContentBlock::Image {
                mime_type: mime.to_owned(),
                data: STANDARD.encode(bytes.into_inner()),
            }],
        )
    }

    #[test]
    fn tool_media_publication_is_immutable_and_replay_preserves_manifest() {
        // Serialize the shared disk fixture before entering the async runtime.
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_tool_media_publication_is_immutable_and_replay_preserves_manifest());
    }

    async fn check_tool_media_publication_is_immutable_and_replay_preserves_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let _root = ArtifactRoot(
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
                temp.path().to_owned(),
            )),
        );
        let first = project(
            image_result(3, "image/png"),
            "media_owner",
            "call/a",
            "screenshot",
        )
        .await;
        let metadata = first.result.metadata.as_ref().unwrap();
        let descriptor = &metadata["tool_media"][0];
        assert_eq!(descriptor["version"], 1);
        assert_eq!(descriptor["width"], 2);
        assert_eq!(descriptor["height"], 1);
        assert_eq!(metadata["keep"], true);
        assert_eq!(first.content_blocks.len(), 1);
        let handle = descriptor["artifact_id"].as_str().unwrap();
        let manifest_before = read_evidence_metadata("media_owner", handle).unwrap();
        let replay = project(
            image_result(3, "image/png"),
            "media_owner",
            "call/a",
            "screenshot",
        )
        .await;
        assert_eq!(replay.result.metadata, first.result.metadata);
        assert_eq!(
            serde_json::to_value(read_evidence_metadata("media_owner", handle).unwrap()).unwrap(),
            serde_json::to_value(&manifest_before).unwrap()
        );
        let (parallel_a, parallel_b) = tokio::join!(
            project(
                image_result(9, "image/png"),
                "media_owner",
                "concurrent",
                "screenshot"
            ),
            project(
                image_result(9, "image/png"),
                "media_owner",
                "concurrent",
                "screenshot"
            ),
        );
        assert!(
            parallel_a
                .result
                .metadata
                .as_ref()
                .unwrap()
                .get("tool_media")
                .is_some()
        );
        assert_eq!(parallel_a.result.metadata, parallel_b.result.metadata);
        let conflict = project(
            image_result(4, "image/png"),
            "media_owner",
            "call/a",
            "screenshot",
        )
        .await;
        assert!(
            conflict
                .result
                .metadata
                .as_ref()
                .unwrap()
                .get("tool_media")
                .is_none()
        );
        assert_eq!(
            conflict.content_blocks.len(),
            1,
            "UI publication conflict must retain validated model image"
        );
        assert!(conflict.result.content.contains("preview unavailable"));
        let other_call = project(
            image_result(4, "image/png"),
            "media_owner",
            "call:a",
            "screenshot",
        )
        .await;
        assert_ne!(
            other_call.result.metadata.unwrap()["tool_media"][0]["artifact_id"],
            descriptor["artifact_id"],
            "raw call IDs must not collide after sanitization"
        );
        let other_session = project(
            image_result(3, "image/png"),
            "other_owner",
            "call/a",
            "screenshot",
        )
        .await;
        assert_ne!(
            other_session.result.metadata.unwrap()["tool_media"][0]["artifact_id"],
            descriptor["artifact_id"]
        );
        assert!(
            !temp.path().join("media_owner.json").exists(),
            "publication must not create a second session state authority"
        );
    }

    #[test]
    fn tool_media_rejects_spoofed_mime_bytes_and_plugin_descriptors() {
        // Serialize the shared disk fixture before entering the async runtime.
        let _guard = codewhale_runtime::artifacts::TEST_ARTIFACT_SESSIONS_GUARD
            .lock()
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_tool_media_rejects_spoofed_mime_bytes_and_plugin_descriptors());
    }

    async fn check_tool_media_rejects_spoofed_mime_bytes_and_plugin_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        let _root = ArtifactRoot(
            codewhale_runtime::artifacts::set_test_artifact_sessions_root(Some(
                temp.path().to_owned(),
            )),
        );
        let mime = project(
            image_result(0, "image/jpeg"),
            "media_owner",
            "bad-mime",
            "screenshot",
        )
        .await;
        assert!(mime.content_blocks.is_empty());
        assert!(mime.result.metadata.unwrap().get("tool_media").is_none());
        let mut bad = image_result(0, "image/png");
        bad.content_blocks = vec![ToolResultContentBlock::Image {
            mime_type: "image/png".to_owned(),
            data: STANDARD.encode(b"not an image"),
        }];
        let bytes = project(bad, "media_owner", "bad-bytes", "screenshot").await;
        assert!(bytes.content_blocks.is_empty());
        assert!(bytes.result.metadata.unwrap().get("tool_media").is_none());
        let mut plain = image_result(0, "image/png");
        plain.content_blocks.clear();
        let plain = project(plain, "media_owner", "no-image", "screenshot").await;
        assert!(plain.result.metadata.unwrap().get("tool_media").is_none());
        assert!(!temp.path().join("media_owner").exists());
    }
}
