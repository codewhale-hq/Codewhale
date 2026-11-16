use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use codewhale_plugins::activation::TestPolicyGuard;
use serde_json::json;
use tempfile::tempdir;
use wiremock::MockServer;

use super::pandoc::PandocConvertTool;
use super::spec::{ToolContext, ToolError, ToolSpec};
use super::speech::SpeechTool;
use super::speech::test_support::{config, provider};
use crate::client::CodewhaleClient;
use crate::test_support::{EnvVarGuard, SealedHome, lock_test_env};

fn stub_pandoc_path(dir: &Path) -> OsString {
    let stub = dir.join("pandoc");
    fs::write(
        &stub,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pandoc 3.1.9'; exit 0; fi\nsrc=\"$2\"\nout=\"\"\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"--output\" ]; then out=\"$2\"; fi; shift; done\nif [ -n \"$out\" ]; then cat \"$src\" > \"$out\"; else cat \"$src\"; fi\n",
    )
    .expect("stub");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod");
    std::env::join_paths(
        std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )
    .expect("path")
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .expect("metadata")
        .file_type()
        .is_symlink()
}

#[test]
fn a_dangling_link_leaving_the_workspace_is_refused() {
    let ws = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let ctx = ToolContext::new(ws.path().to_path_buf());

    symlink(
        outside.path().join("missing.txt"),
        ws.path().join("dangling.txt"),
    )
    .unwrap();
    symlink(outside.path().join("hop.txt"), ws.path().join("hop2")).unwrap();
    symlink(ws.path().join("hop2"), ws.path().join("hop1")).unwrap();
    symlink(
        Path::new("../codewhale-missing-target"),
        ws.path().join("climb"),
    )
    .unwrap();
    symlink(outside.path().join("nodir"), ws.path().join("dirlink")).unwrap();

    for raw in ["dangling.txt", "hop1", "climb", "dirlink/new.txt"] {
        let err = ctx.resolve_path(raw).expect_err(raw);
        assert!(
            matches!(err, ToolError::PathEscape { .. }),
            "{raw}: {err:?}"
        );
    }

    symlink("selfloop", ws.path().join("selfloop")).unwrap();
    ctx.resolve_path("selfloop").expect_err("a link loop");

    let outer = tempdir().unwrap();
    let nested_ws = outer.path().join("outer").join("ws");
    fs::create_dir_all(&nested_ws).unwrap();
    let nested_ctx = ToolContext::new(nested_ws.clone());
    symlink("..", nested_ws.join("up")).unwrap();
    symlink("up/../escaped.txt", nested_ws.join("out.txt")).unwrap();
    let err = nested_ctx
        .resolve_path("out.txt")
        .expect_err("parent traversal after symlink must not resolve inside workspace");
    assert!(matches!(err, ToolError::PathEscape { .. }), "{err:?}");
}

#[test]
fn a_dangling_link_inside_the_workspace_still_resolves() {
    let ws = tempdir().unwrap();
    let ctx = ToolContext::new(ws.path().to_path_buf());
    let canonical = ws.path().canonicalize().unwrap();

    symlink(ws.path().join("later.txt"), ws.path().join("absolute")).unwrap();
    symlink("sub/later.txt", ws.path().join("relative")).unwrap();

    for name in ["absolute", "relative"] {
        assert_eq!(
            ctx.resolve_path(name).expect(name),
            canonical.join(name),
            "{name}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pandoc_output_is_published_without_writing_through_a_link() {
    let _env = lock_test_env();
    let bin = tempdir().unwrap();
    let _path = EnvVarGuard::set("PATH", stub_pandoc_path(bin.path()));
    let ws = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(ws.path().join("in.md"), "PAYLOAD\n").unwrap();
    let ctx = ToolContext::new(ws.path().to_path_buf());
    let convert = |output: &str| json!({"source_path": "in.md", "target_format": "plain", "output_path": output});

    symlink(
        outside.path().join("created.txt"),
        ws.path().join("dangling.txt"),
    )
    .unwrap();
    let err = PandocConvertTool
        .execute(convert("dangling.txt"), &ctx)
        .await
        .expect_err("a dangling link out of the workspace");
    assert!(matches!(err, ToolError::PathEscape { .. }), "{err:?}");
    assert!(!outside.path().join("created.txt").exists());

    symlink(ws.path().join("later.txt"), ws.path().join("alias.txt")).unwrap();
    PandocConvertTool
        .execute(convert("alias.txt"), &ctx)
        .await
        .expect("a dangling link inside the workspace");
    assert!(!is_link(&ws.path().join("alias.txt")));
    assert_eq!(
        fs::read_to_string(ws.path().join("alias.txt")).unwrap(),
        "PAYLOAD\n"
    );
    assert!(!ws.path().join("later.txt").exists());

    fs::write(outside.path().join("victim.txt"), "keep").unwrap();
    fs::hard_link(
        outside.path().join("victim.txt"),
        ws.path().join("hard.txt"),
    )
    .unwrap();
    PandocConvertTool
        .execute(convert("hard.txt"), &ctx)
        .await
        .expect_err("a hard-linked output");
    assert_eq!(
        fs::read_to_string(outside.path().join("victim.txt")).unwrap(),
        "keep"
    );

    let err = PandocConvertTool
        .execute(
            json!({
                "source_path": "in.md",
                "target_format": "plain",
                "output_path": "spare.txt",
                "path": "other.txt"
            }),
            &ctx,
        )
        .await
        .expect_err("an unexpected field");
    assert!(matches!(err, ToolError::InvalidInput { .. }), "{err:?}");
    assert!(!ws.path().join("spare.txt").exists());

    PandocConvertTool
        .execute(convert("plain.txt"), &ctx)
        .await
        .expect("an ordinary output");
    assert_eq!(
        fs::read_to_string(ws.path().join("plain.txt")).unwrap(),
        "PAYLOAD\n"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn speech_output_is_published_without_writing_through_a_link() {
    let _home = SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(false);
    let ws = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let server = MockServer::start().await;
    let client = CodewhaleClient::new(&config(&server.uri(), false)).unwrap();
    let tool = SpeechTool::new("speech", Some(client), None);
    let ctx = ToolContext::new(ws.path().to_path_buf());

    symlink(
        outside.path().join("created.wav"),
        ws.path().join("dangling.wav"),
    )
    .unwrap();
    provider(&server, 0).await;
    let err = tool
        .execute(json!({"text": "hello", "output": "dangling.wav"}), &ctx)
        .await
        .expect_err("a dangling link out of the workspace");
    assert!(matches!(err, ToolError::PathEscape { .. }), "{err:?}");
    assert!(!outside.path().join("created.wav").exists());

    fs::write(outside.path().join("victim.wav"), "keep").unwrap();
    symlink(
        outside.path().join("victim.wav"),
        ws.path().join("speech.wav"),
    )
    .unwrap();
    server.reset().await;
    provider(&server, 1).await;
    tool.execute(json!({"text": "hello"}), &ctx)
        .await
        .expect("default name");
    assert_eq!(
        fs::read_to_string(outside.path().join("victim.wav")).unwrap(),
        "keep"
    );
    assert!(!is_link(&ws.path().join("speech.wav")));
    assert_eq!(fs::read(ws.path().join("speech.wav")).unwrap(), b"hi");

    fs::create_dir(ws.path().join("audio")).unwrap();
    fs::write(outside.path().join("victim2.wav"), "keep").unwrap();
    symlink(
        outside.path().join("victim2.wav"),
        ws.path().join("audio").join("speech.wav"),
    )
    .unwrap();
    server.reset().await;
    provider(&server, 1).await;
    tool.execute(json!({"text": "hello", "output_dir": "audio"}), &ctx)
        .await
        .expect("output_dir default name");
    assert_eq!(
        fs::read_to_string(outside.path().join("victim2.wav")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read(ws.path().join("audio").join("speech.wav")).unwrap(),
        b"hi"
    );

    fs::write(outside.path().join("victim3.wav"), "keep").unwrap();
    fs::hard_link(
        outside.path().join("victim3.wav"),
        ws.path().join("hard.wav"),
    )
    .unwrap();
    server.reset().await;
    provider(&server, 1).await;
    tool.execute(json!({"text": "hello", "output": "hard.wav"}), &ctx)
        .await
        .expect_err("a hard-linked output");
    assert_eq!(
        fs::read_to_string(outside.path().join("victim3.wav")).unwrap(),
        "keep"
    );
}
