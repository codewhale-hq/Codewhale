use super::tests::{git, repo};
use super::*;
use std::fs;

fn request(body: Value) -> GitPushRequest {
    serde_json::from_value(body).expect("push request")
}

fn status_of(error: GitWriteError) -> (StatusCode, Option<&'static str>, Value) {
    match error {
        GitWriteError::Api(error) => (error.status, None, Value::Null),
        GitWriteError::Coded {
            status,
            code,
            extra,
            ..
        } => (
            status,
            Some(code),
            extra.get("stale").cloned().unwrap_or_default(),
        ),
    }
}

fn output(dir: &FsPath, args: &[&str]) -> String {
    let output = Git::output(args, dir).expect("spawn git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn refs(bare: &FsPath) -> Vec<String> {
    output(bare, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .map(str::to_string)
        .collect()
}

fn commit(dir: &FsPath, name: &str) -> String {
    fs::write(dir.join(name), name).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", name]);
    output(dir, &["rev-parse", "HEAD"])
}

async fn refused(dir: &FsPath, body: Value) -> (StatusCode, Option<&'static str>, Value) {
    match push(dir, request(body)).await {
        Ok(response) => panic!("push was not refused: {}", response.0),
        Err(error) => status_of(error),
    }
}

#[tokio::test]
async fn push_args_name_one_configured_remote_and_one_commit_refspec() {
    let tmp = repo();
    let root = tmp.path();
    let plan = |remote: Option<&'static str>, set_upstream: bool| async move {
        match push_plan(root, remote, set_upstream, None).await {
            Ok(Ok(plan)) => Ok(plan),
            Ok(Err(error)) => Err(status_of(error).0),
            Err(error) => Err(error.status),
        }
    };

    for (remote, set_upstream) in [(None, false), (Some("  "), false), (None, true)] {
        assert_eq!(
            plan(remote, set_upstream).await.unwrap_err(),
            StatusCode::BAD_REQUEST,
            "{remote:?} without a configured origin"
        );
    }

    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", &bare.path().to_string_lossy()],
    );
    let head = output(root, &["rev-parse", "HEAD"]);
    let refspec = format!("{head}:refs/heads/main");
    let explicit = plan(Some(" origin "), false).await.unwrap();
    assert_eq!(
        explicit.args,
        [
            "push",
            "--no-follow-tags",
            "--no-recurse-submodules",
            "--",
            "origin",
            refspec.as_str()
        ]
    );
    assert!(explicit.upstream.is_none());
    for remote in ["--mirror", "fork", "../other"] {
        assert_eq!(
            plan(Some(remote), true).await.unwrap_err(),
            StatusCode::BAD_REQUEST,
            "{remote}"
        );
    }
    let upstream = plan(None, true).await.unwrap();
    assert_eq!(upstream.args, explicit.args);
    assert_eq!(
        upstream.upstream,
        Some(("main".to_string(), "origin".to_string()))
    );

    git(root, &["checkout", "-q", "--detach"]);
    assert_eq!(
        plan(None, false).await.unwrap_err(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn git_push_named_fields_are_validated_before_git_runs() {
    let head = "a".repeat(40);
    let expect = |body: Value| serde_json::from_value::<GitExpect>(body).unwrap();
    assert_eq!(named_push(None, None).unwrap(), None);
    assert_eq!(
        named_push(
            Some(" main "),
            Some(expect(json!({ "head": head.to_uppercase() })))
        )
        .unwrap(),
        Some(NamedPush {
            branch: "main".to_string(),
            head: head.clone(),
        })
    );
    for (branch, body) in [
        (None, Some(json!({ "head": head }))),
        (Some("main"), None),
        (Some("main"), Some(json!({}))),
        (Some("main"), Some(json!({ "head": null }))),
        (Some("main"), Some(json!({ "head": "abc" }))),
        (
            Some("main"),
            Some(json!({ "head": head, "index": "b".repeat(64) })),
        ),
        (
            Some("main"),
            Some(json!({ "head": head, "revision": "b".repeat(64) })),
        ),
        (Some(""), Some(json!({ "head": head }))),
        (Some("--force"), Some(json!({ "head": head }))),
        (Some("main:refs/heads/other"), Some(json!({ "head": head }))),
        (Some("a b"), Some(json!({ "head": head }))),
    ] {
        let error = named_push(branch, body.clone().map(expect))
            .expect_err(&format!("{branch:?} {body:?}"));
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{branch:?} {body:?}");
    }
}

#[tokio::test]
async fn git_push_sends_only_the_named_branch_at_the_expected_head() {
    let tmp = repo();
    let root = tmp.path();
    let bare = tempfile::tempdir().unwrap();
    let remote = bare.path();
    git(remote, &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(root, &["config", "push.default", "matching"]);
    git(root, &["config", "push.followTags", "true"]);
    git(
        root,
        &["config", "remote.origin.push", "refs/heads/*:refs/heads/*"],
    );
    git(
        root,
        &[
            "config",
            "--add",
            "remote.origin.push",
            "refs/tags/*:refs/tags/*",
        ],
    );
    git(root, &["branch", "unrelated"]);
    git(root, &["tag", "-a", "-m", "release", "v1"]);
    let main = output(root, &["rev-parse", "HEAD"]);
    assert!(refs(remote).is_empty());

    let first = push(root, request(json!({ "set_upstream": true })))
        .await
        .unwrap_or_else(|error| panic!("first push: {:?}", status_of(error)));
    assert_eq!(first.0["ok"], true);
    assert_eq!(refs(remote), [format!("refs/heads/main {main}")]);
    assert_eq!(
        output(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/main"
    );

    git(root, &["switch", "-q", "-c", "feature"]);
    let stale_feature = commit(root, "f1.txt");
    let feature = commit(root, "f2.txt");
    git(root, &["branch", "-f", "unrelated", "HEAD"]);
    let (status, code, stale) = refused(
        root,
        json!({ "branch": "feature", "expect": { "head": stale_feature } }),
    )
    .await;
    assert_eq!(
        (status, code),
        (StatusCode::CONFLICT, Some("git_state_changed"))
    );
    assert_eq!(stale, json!(["head"]));
    let (status, code, stale) = refused(
        root,
        json!({ "branch": "main", "expect": { "head": main } }),
    )
    .await;
    assert_eq!(
        (status, code),
        (StatusCode::CONFLICT, Some("git_state_changed"))
    );
    assert_eq!(stale, json!(["branch"]));
    assert_eq!(refs(remote), [format!("refs/heads/main {main}")]);

    git(root, &["config", "remote.origin.mirror", "true"]);
    let (status, code, _) = refused(
        root,
        json!({ "branch": "feature", "expect": { "head": feature } }),
    )
    .await;
    assert_eq!((status, code), (StatusCode::BAD_REQUEST, None));
    git(root, &["config", "--unset", "remote.origin.mirror"]);
    assert_eq!(refs(remote), [format!("refs/heads/main {main}")]);

    let pushed = push(
        root,
        request(
            json!({ "branch": "feature", "expect": { "head": feature }, "set_upstream": true }),
        ),
    )
    .await
    .unwrap_or_else(|error| panic!("named push: {:?}", status_of(error)));
    assert_eq!(pushed.0["ok"], true);
    assert_eq!(
        refs(remote),
        [
            format!("refs/heads/feature {feature}"),
            format!("refs/heads/main {main}"),
        ]
    );
    assert_eq!(
        output(root, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/feature"
    );

    let next = commit(root, "f3.txt");
    git(root, &["checkout", "-q", "--detach"]);
    let (status, code, stale) = refused(
        root,
        json!({ "branch": "feature", "expect": { "head": next } }),
    )
    .await;
    assert_eq!(
        (status, code),
        (StatusCode::CONFLICT, Some("git_state_changed"))
    );
    assert_eq!(stale, json!(["branch"]));
    assert_eq!(refused(root, json!({})).await.0, StatusCode::BAD_REQUEST);

    git(root, &["switch", "-q", "feature"]);
    let pushed = push(
        root,
        request(json!({ "branch": "feature", "expect": { "head": next } })),
    )
    .await
    .unwrap_or_else(|error| panic!("existing upstream push: {:?}", status_of(error)));
    assert_eq!(pushed.0["ok"], true);
    assert_eq!(
        refs(remote),
        [
            format!("refs/heads/feature {next}"),
            format!("refs/heads/main {main}"),
        ]
    );

    git(
        root,
        &["switch", "-q", "-c", "renamed", "--track", "origin/main"],
    );
    let renamed = commit(root, "r.txt");
    assert_eq!(
        refused(
            root,
            json!({ "branch": "renamed", "expect": { "head": renamed } })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        refs(remote),
        [
            format!("refs/heads/feature {next}"),
            format!("refs/heads/main {main}"),
        ]
    );
}
