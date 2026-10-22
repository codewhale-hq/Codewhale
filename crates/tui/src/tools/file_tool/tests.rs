use super::*;
use serde_json::json;
use tempfile::tempdir;

fn tool() -> FileTool {
    FileTool::with_patch("File")
}

#[tokio::test]
async fn lowercase_read_returns_a_typed_image_block() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("shot.png");
    std::fs::write(&path, crate::image_attach::tests::PNG_1X1).expect("write png");
    let context = ToolContext::new(tmp.path().to_path_buf());

    let rich = ReadTool
        .execute_rich(json!({ "path": "shot.png" }), &context)
        .await
        .expect("read image");

    assert!(rich.result.success);
    assert_eq!(rich.content_blocks.len(), 1);
    assert!(matches!(
        &rich.content_blocks[0],
        codewhale_tools::ToolResultContentBlock::Image { mime_type, data }
            if mime_type == "image/png" && !data.is_empty()
    ));
}

async fn err(tool: &FileTool, input: Value) -> String {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    tool.execute(input, &ctx)
        .await
        .expect_err("call must be refused")
        .to_string()
}

/// #5209's shape one level up: `File{path, content}` used to answer an
/// intended write with the file's contents under a success receipt.
#[tokio::test]
async fn missing_action_is_refused_instead_of_silently_reading() {
    let message = err(&tool(), json!({"path": "a.rs", "content": "fn main() {}"})).await;
    assert!(
        message.contains("requires an `action`"),
        "must say what is missing: {message}"
    );
    assert!(
        message.contains("nothing was run"),
        "must deny having done work: {message}"
    );
    for action in [
        "read",
        "list",
        "search_name",
        "search_content",
        "write",
        "edit",
        "patch",
    ] {
        assert!(message.contains(action), "must name `{action}`: {message}");
    }
}

#[tokio::test]
async fn non_string_action_is_refused_with_the_valid_values() {
    let message = err(&tool(), json!({"action": 3, "path": "a.rs"})).await;
    assert!(message.contains("to be a string"), "{message}");
    assert!(message.contains("read"), "{message}");
}

#[tokio::test]
async fn unknown_action_names_the_actions_that_dispatch() {
    let message = err(&tool(), json!({"action": "str_replace", "path": "a.rs"})).await;
    assert!(
        message.contains("str_replace"),
        "must quote the bad value: {message}"
    );
    assert!(message.contains("nothing was run"), "{message}");
    assert!(
        message.contains("edit"),
        "must name the real action: {message}"
    );
}

/// A refusal must never advertise an action this instance cannot run.
#[tokio::test]
async fn read_only_instances_do_not_suggest_write_actions() {
    let message = err(&FileTool::read_only("File"), json!({"path": "a.rs"})).await;
    assert!(message.contains("read"), "{message}");
    assert!(
        !message.contains("write"),
        "read-only File must not offer write: {message}"
    );
    assert!(
        !message.contains("edit"),
        "read-only File must not offer edit: {message}"
    );
}

#[tokio::test]
async fn disabled_write_refusal_states_what_is_available() {
    let message = err(
        &FileTool::read_only("File"),
        json!({"action": "write", "path": "a.rs", "content": "x"}),
    )
    .await;
    assert!(message.contains("nothing was written"), "{message}");
    assert!(message.contains("Available actions here"), "{message}");
}

/// The wrapper is the only schema the model reads. Every per-action
/// description must come from the tool that implements the action, so the
/// stale-copy drift that produced "default 200" cannot recur.
#[test]
fn wrapper_borrows_every_inner_description() {
    let schema = tool().input_schema();
    let properties = schema["properties"].as_object().expect("properties");
    for (name, property) in properties {
        let text = if name == "replace" {
            property.to_string()
        } else {
            property["description"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        assert!(
            !text.trim().is_empty(),
            "`{name}` lost its description — an inner parameter was probably renamed"
        );
    }
}

#[test]
fn read_parameters_state_the_defaults_the_code_actually_uses() {
    let schema = tool().input_schema();
    let max_lines = schema["properties"]["max_lines"]["description"]
        .as_str()
        .expect("max_lines description");
    let inner = ReadFileTool.input_schema()["properties"]["max_lines"]["description"]
        .as_str()
        .expect("inner max_lines description")
        .to_string();
    assert!(
        max_lines.contains(inner.trim_end_matches('.')),
        "wrapper must quote the implementing tool verbatim: {max_lines}"
    );
    assert!(
        !max_lines.contains("200"),
        "the retired 200-line default must not be advertised: {max_lines}"
    );
    assert!(
        !max_lines.contains("blame"),
        "`File` has no blame action; blame lives on `Git`: {max_lines}"
    );
}

/// `fuzz` belongs to `patch` alone. `edit` read it into a discarded
/// binding while the schema kept advertising it, and a live model read
/// that as "an optional fuzzy-matching flag for the search" — a
/// capability claim no code honored. The advertisement is gone; what
/// remains must describe only the integer `patch` really uses.
#[test]
fn fuzz_is_advertised_only_for_patch() {
    let schema = tool().input_schema();
    assert_eq!(schema["properties"]["fuzz"]["type"], "integer");
    let fuzz = schema["properties"]["fuzz"]["description"]
        .as_str()
        .expect("fuzz description");
    assert!(fuzz.contains("action=patch"), "{fuzz}");
    assert!(
        !fuzz.contains("action=edit"),
        "edit no longer accepts fuzz: {fuzz}"
    );
    assert!(
        EditFileTool.input_schema()["properties"]
            .get("fuzz")
            .is_none(),
        "edit must not advertise a parameter it does not implement"
    );
}

/// `File` is in the active catalog in every mode, so its schema is re-sent
/// on every turn of every session. Borrowing the implementing tools'
/// descriptions buys accuracy with bytes; this bounds what that costs and
/// prints the per-parameter breakdown when it trips, so the next increase
/// is a decision rather than a drift.
///
/// Raised 3000 → 3100 in v0.9.7 for the `expected_hash` content-hash guard
/// (#3979) — a new parameter on three actions, kept to instruction-only
/// text. That is a decision, not drift: the budget exists to price schema
/// bytes, not to forbid new capability.
///
/// Raised 3100 → 3200 in v0.10.2 for the file-search repair's `exclude`
/// parameter (#6934): the same decision, one new parameter, borrowed here
/// the same way the others are. `exclude` carries its glob examples as
/// instruction rather than explanation — without them the parameter's
/// replacement semantics are guessable only by reading the source.
#[test]
fn schema_stays_within_its_catalog_byte_budget() {
    const BUDGET_BYTES: usize = 3_200;

    let schema = FileTool::with_patch("File").input_schema();
    let mut rows: Vec<(usize, String)> = schema["properties"]
        .as_object()
        .expect("properties")
        .iter()
        .map(|(name, property)| (property.to_string().len(), name.clone()))
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    let total: usize = rows.iter().map(|(bytes, _)| bytes).sum();

    assert!(
        total <= BUDGET_BYTES,
        "File schema is {total} bytes against a {BUDGET_BYTES} budget; \
         trim explanation, keep instruction. Breakdown: {rows:?}"
    );
}

// === Per-action parameter validation ===
//
// #5209 taught `edit` to refuse a parameter it does not implement. Only
// `edit` learned it, so every other action still dropped unknown keys and
// answered anyway: a misspelled `start_line` on `read` returned the head
// of the file with nothing in the response admitting the requested window
// was never honored. These cover the refusal on every action, and — the
// half that keeps a refusal honest — that each action still accepts its
// full legitimate parameter set, optional names and aliases included.

/// A workspace with one file, already read, so `edit` and `patch` are
/// past their freshness precondition and reach parameter validation.
async fn workspace() -> (tempfile::TempDir, ToolContext) {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    std::fs::write(tmp.path().join("doc.txt"), "alpha\nbeta\ngamma\n").expect("write");
    tool()
        .execute(json!({"action": "read", "path": "doc.txt"}), &ctx)
        .await
        .expect("seed read");
    (tmp, ctx)
}

/// The minimal call that dispatches for each action, in schema order.
fn minimal_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("read", json!({"action": "read", "path": "doc.txt"})),
        ("list", json!({"action": "list"})),
        (
            "search_name",
            json!({"action": "search_name", "query": "doc"}),
        ),
        (
            "search_content",
            json!({"action": "search_content", "pattern": "alpha"}),
        ),
        (
            "write",
            json!({"action": "write", "path": "new.txt", "content": "x\n"}),
        ),
        (
            "edit",
            json!({"action": "edit", "path": "doc.txt", "search": "alpha", "replace": "delta"}),
        ),
        (
            "patch",
            json!({"action": "patch", "path": "doc.txt", "patch": "@@ -1,1 +1,1 @@\n-alpha\n+delta\n"}),
        ),
    ]
}

fn with_key(mut input: Value, key: &str, value: Value) -> Value {
    input
        .as_object_mut()
        .expect("object")
        .insert(key.to_string(), value);
    input
}

/// The gap this closes. A parameter with no known meaning is refused by
/// every action, not just `edit`, and the refusal carries the same four
/// facts everywhere: what was wrong, what is allowed, what is required,
/// and that nothing was done.
#[tokio::test]
async fn every_action_refuses_an_unknown_parameter() {
    for (action, call) in minimal_calls() {
        let (_tmp, ctx) = workspace().await;
        let message = tool()
            .execute(with_key(call, "bogus_param", json!(true)), &ctx)
            .await
            .expect_err("an unknown parameter must be refused")
            .to_string();
        assert!(
            message.contains("bogus_param"),
            "{action} must name the offending parameter: {message}"
        );
        assert!(
            message.contains(&format!("unexpected File {action} parameter")),
            "{action} must name the action it refused: {message}"
        );
        assert!(
            message.contains("Allowed parameters are"),
            "{action} must name the allowed set: {message}"
        );
        assert!(
            message.contains("Required:"),
            "{action} must name the required set: {message}"
        );
        assert!(
            message.contains(&format!("The {action} was not performed")),
            "{action} must deny having done the work: {message}"
        );
    }
}

/// The specific silent wrong answer that motivated this: a misspelled
/// read window used to be dropped, and the head of the file came back
/// under a success receipt as if it were the requested range.
#[tokio::test]
async fn a_misspelled_read_window_is_refused_rather_than_answered_with_the_head() {
    let (_tmp, ctx) = workspace().await;
    let message = tool()
        .execute(
            json!({"action": "read", "path": "doc.txt", "start_lien": 2}),
            &ctx,
        )
        .await
        .expect_err("a misspelled window must not silently return the head")
        .to_string();
    assert!(message.contains("start_lien"), "{message}");
    assert!(message.contains("`start_line`"), "{message}");
}

/// A refusal is only worth having if the legitimate call still lands.
/// Every action's full parameter set — every optional name included —
/// must survive validation.
#[tokio::test]
async fn every_action_accepts_its_full_legitimate_parameter_set() {
    let full: Vec<(&str, Value)> = vec![
        (
            "read",
            json!({"action": "read", "path": "doc.txt", "start_line": 1, "max_lines": 2, "pages": "1"}),
        ),
        ("list", json!({"action": "list", "path": "."})),
        (
            "search_name",
            json!({"action": "search_name", "query": "doc", "path": ".", "limit": 5,
                   "extensions": ["txt"], "exclude": ["target/**"]}),
        ),
        (
            "search_content",
            json!({"action": "search_content", "pattern": "alpha", "path": ".",
                   "include": ["*.txt"], "exclude": ["target/**"], "context_lines": 1,
                   "case_insensitive": true, "max_results": 5}),
        ),
        (
            "write",
            json!({"action": "write", "path": "new.txt", "content": "x\n"}),
        ),
        (
            "edit",
            json!({"action": "edit", "path": "doc.txt", "search": "alpha", "replace": "delta"}),
        ),
        (
            "patch",
            json!({"action": "patch", "path": "doc.txt",
                   "patch": "@@ -1,1 +1,1 @@\n-alpha\n+delta\n",
                   "fuzz": 3, "create_if_missing": false}),
        ),
    ];

    for (action, call) in full {
        let (_tmp, ctx) = workspace().await;
        let result = tool()
            .execute(call, &ctx)
            .await
            .unwrap_or_else(|error| panic!("{action} must accept its own parameters: {error}"));
        assert!(result.success, "{action}: {}", result.content);
    }
}

/// Validation runs *after* alias translation, so every cross-harness
/// spelling the alias lane folds must still reach the action it names.
/// A refusal that fired first would undo #5209's fix.
#[tokio::test]
async fn every_alias_survives_validation() {
    let aliased: Vec<(&str, Value)> = vec![
        // Path spellings, on every action that takes a path.
        ("read", json!({"action": "read", "file_path": "doc.txt"})),
        ("read", json!({"action": "read", "filePath": "doc.txt"})),
        ("list", json!({"action": "list", "file_path": "."})),
        (
            "search_name",
            json!({"action": "search_name", "query": "doc", "file_path": "."}),
        ),
        (
            "search_content",
            json!({"action": "search_content", "pattern": "alpha", "file_path": "."}),
        ),
        (
            "write",
            json!({"action": "write", "file_path": "new.txt", "content": "x\n"}),
        ),
        // Read-window spellings.
        (
            "read",
            json!({"action": "read", "path": "doc.txt", "offset": 2, "limit": 1}),
        ),
        (
            "read",
            json!({"action": "read", "path": "doc.txt", "line_offset": 2, "n_lines": 1}),
        ),
        (
            "read",
            json!({"action": "read", "path": "doc.txt", "num_lines": 1}),
        ),
        // Search spellings the wrapper advertises across both actions.
        (
            "search_name",
            json!({"action": "search_name", "query": "doc", "max_results": 5}),
        ),
        (
            "search_content",
            json!({"action": "search_content", "query": "alpha", "limit": 5}),
        ),
    ];

    for (action, call) in aliased {
        let (_tmp, ctx) = workspace().await;
        let result = tool()
            .execute(call.clone(), &ctx)
            .await
            .unwrap_or_else(|error| panic!("{action} must accept {call}: {error}"));
        assert!(result.success, "{action} / {call}: {}", result.content);
    }

    // Edit spellings need their own loop: each one mutates the file.
    for (search, replace) in [
        ("old_string", "new_string"),
        ("old_str", "new_str"),
        ("oldText", "newText"),
        ("old_text", "new_text"),
    ] {
        let (_tmp, ctx) = workspace().await;
        let result = tool()
            .execute(
                json!({"action": "edit", "path": "doc.txt",
                       search: "alpha", replace: "delta"}),
                &ctx,
            )
            .await
            .unwrap_or_else(|error| panic!("edit must accept {search}/{replace}: {error}"));
        assert!(result.success, "{search}/{replace}: {}", result.content);
    }
    let (_tmp, ctx) = workspace().await;
    let result = tool()
        .execute(
            json!({"action": "edit", "path": "doc.txt", "search": "alpha", "replacement": "delta"}),
            &ctx,
        )
        .await
        .expect("edit must accept `replacement`");
    assert!(result.success, "{}", result.content);
}

/// A parameter that belongs to a *different* action is still unknown to
/// this one. Silently dropping it is how a model learns a call worked
/// when the argument it cared about was discarded.
#[tokio::test]
async fn parameters_do_not_leak_between_actions() {
    for (action, call) in [
        (
            "read",
            json!({"action": "read", "path": "doc.txt", "case_insensitive": true}),
        ),
        (
            "write",
            json!({"action": "write", "path": "new.txt", "content": "x\n", "start_line": 2}),
        ),
        (
            "list",
            json!({"action": "list", "path": ".", "context_lines": 3}),
        ),
        (
            "search_name",
            json!({"action": "search_name", "query": "doc", "context_lines": 3}),
        ),
        (
            "search_content",
            json!({"action": "search_content", "pattern": "alpha", "extensions": ["txt"]}),
        ),
    ] {
        let (_tmp, ctx) = workspace().await;
        let message = tool()
            .execute(call, &ctx)
            .await
            .expect_err("another action's parameter must be refused")
            .to_string();
        assert!(
            message.contains(&format!("The {action} was not performed")),
            "{action}: {message}"
        );
    }
}

/// Every action's required names must be names it also allows, or a
/// refusal would tell the model to pass something the action rejects.
#[test]
fn every_required_parameter_is_also_an_allowed_one() {
    use crate::tools::file::{
        EDIT_PARAMS, LIST_PARAMS, PATCH_PARAMS, READ_PARAMS, SEARCH_CONTENT_PARAMS,
        SEARCH_NAME_PARAMS, WRITE_PARAMS,
    };

    for params in [
        READ_PARAMS,
        WRITE_PARAMS,
        EDIT_PARAMS,
        LIST_PARAMS,
        SEARCH_NAME_PARAMS,
        SEARCH_CONTENT_PARAMS,
        PATCH_PARAMS,
    ] {
        params.assert_required_is_allowed();
    }
}

#[test]
fn advertised_actions_match_the_actions_that_dispatch() {
    for (tool, expected) in [
        (FileTool::with_patch("File"), 7),
        (FileTool::new("File"), 6),
        (FileTool::read_only("File"), 4),
    ] {
        let schema = tool.input_schema();
        let advertised = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .len();
        assert_eq!(advertised, expected);
        assert_eq!(advertised, tool.available_actions().len());
    }
}

#[test]
fn primitive_schemas_are_separate_and_small_contract_shaped() {
    assert_eq!(ReadTool.name(), "read");
    assert_eq!(WriteTool.name(), "write");
    assert_eq!(EditTool.name(), "edit");
    assert_eq!(
        ReadTool.input_schema()["properties"]
            .as_object()
            .expect("read properties")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        // `max_bytes` is the model-requested per-call output budget (C05).
        ["expected_revision", "limit", "max_bytes", "offset", "path"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    assert_eq!(
        WriteTool.input_schema()["properties"]
            .as_object()
            .expect("write properties")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        ["content", "path"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    assert_eq!(
        EditTool.input_schema()["required"],
        json!(["path", "edits"])
    );
    assert!(!tool().model_visible(), "legacy File must stay hidden");
}

#[tokio::test]
async fn primitive_edit_applies_disjoint_matches_against_original() {
    let (tmp, ctx) = workspace().await;
    let result = EditTool
        .execute(
            json!({
                "path": "doc.txt",
                "edits": [
                    {"oldText": "alpha", "newText": "ALPHA-LONG"},
                    {"oldText": "gamma", "newText": "g"}
                ]
            }),
            &ctx,
        )
        .await
        .expect("multi-edit");

    assert!(result.success, "{}", result.content);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("doc.txt")).expect("read"),
        "ALPHA-LONG\nbeta\ng\n"
    );
    assert_eq!(
        result.content,
        "Successfully replaced 2 block(s) in doc.txt."
    );
}

#[tokio::test]
async fn primitive_edit_rejects_overlap_without_writing() {
    let (tmp, ctx) = workspace().await;
    let before = std::fs::read_to_string(tmp.path().join("doc.txt")).expect("before");
    let error = EditTool
        .execute(
            json!({
                "path": "doc.txt",
                "edits": [
                    {"oldText": "alpha\nbeta", "newText": "one"},
                    {"oldText": "beta\ngamma", "newText": "two"}
                ]
            }),
            &ctx,
        )
        .await
        .expect_err("overlap must fail");

    assert!(error.to_string().contains("overlap"));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("doc.txt")).expect("after"),
        before
    );
}

#[tokio::test]
async fn primitive_edit_does_not_require_a_prior_read() {
    let tmp = tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("doc.txt"), "alpha\n").expect("write");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    let result = EditTool
        .execute(
            json!({
                "path": "doc.txt",
                "edits": [{"oldText": "alpha", "newText": "beta"}]
            }),
            &ctx,
        )
        .await
        .expect("lowercase edit must be Pi-simple");
    assert!(result.success, "{}", result.content);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("doc.txt")).expect("updated"),
        "beta\n"
    );
}
