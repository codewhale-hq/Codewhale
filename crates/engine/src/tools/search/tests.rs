use std::fs;

use serde_json::{Value, json};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use crate::tools::spec::{ApprovalRequirement, ToolContext, ToolSpec};

use super::{GrepFilesTool, matches_glob};

#[tokio::test]
async fn grep_match_scope_can_be_read_without_guessing_ranges_and_refuses_stale_source() {
    let tmp = tempdir().expect("tempdir");
    // A late definition must not be displaced by the cached file-prefix cap;
    // a matching comment must not manufacture a function.
    let mut source = String::from("// fn target_symbol() { marker 源 }\r\n");
    for index in 0..30 {
        source.push_str(&format!("fn prefix_{index}() {{}}\r\n"));
    }
    let declaration = "/// Navigation contract 源\r\n#[cfg(feature = \"trace\")]\r\npub(crate)\r\nasync\r\nunsafe\r\nextern \"C\"\r\nfn target_symbol() -> u32 {\r\n    // marker 源\r\n    7\r\n}\r\n";
    source.push_str(declaration);
    let path = tmp.path().join("scope.rs");
    fs::write(&path, &source).expect("write");
    let ctx = ToolContext::new(tmp.path());
    let result = GrepFilesTool
        .execute(
            json!({"pattern": "target_symbol|marker", "path": "scope.rs"}),
            &ctx,
        )
        .await
        .expect("grep");
    let payload: Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(payload["total_matches"], 3);
    assert_eq!(
        payload["matches"][0]["line"],
        "// fn target_symbol() { marker 源 }"
    );
    let navigation = payload["navigation"].as_array().expect("search scopes");
    assert_eq!(navigation.len(), 1);
    assert_eq!(navigation[0]["status"], "ready");
    let symbols = navigation[0]["symbols"]
        .as_array()
        .expect("function scopes");
    assert_eq!(symbols.len(), 1, "no fake or unrelated prefix functions");
    assert_eq!(symbols[0]["name"], "target_symbol");
    assert_eq!(symbols[0]["source"], declaration);
    let reference = symbols[0]["read"].clone();
    assert_eq!(reference["path"], "scope.rs");
    assert_eq!(reference["offset"], 32);
    assert_eq!(reference["limit"], declaration.lines().count());
    assert_eq!(
        reference["expected_revision"],
        crate::tools::file::content_hash(source.as_bytes())
    );
    assert_eq!(
        crate::tools::echolocation::delivered_navigation_refs(&result.content).len(),
        1
    );

    let read = crate::tools::file_tool::ReadTool
        .execute(reference.clone(), &ctx)
        .await
        .expect("follow exact search reference through the real reader");
    for line in declaration.lines() {
        assert!(
            read.content.contains(line),
            "missing {line:?}: {}",
            read.content
        );
    }
    // The ordinary symbol footer may name prefix functions; their source
    // declarations must not leak into the requested declaration window.
    for index in 0..30 {
        assert!(!read.content.contains(&format!("fn prefix_{index}() {{}}")));
    }

    fs::write(&path, source.replace("    7", "    8")).expect("same-size mutation");
    let error = crate::tools::file_tool::ReadTool
        .execute(reference, &ctx)
        .await
        .expect_err("search references never authorize stale coordinates");
    assert!(error.to_string().contains("stale"), "{error}");

    // A partial scan cannot mint a whole-file revision, and broad searches
    // must not turn into a function-body dump.
    for (body, input) in [
        (
            source.clone(),
            json!({"pattern": "target_symbol|marker", "path": "scope.rs", "max_results": 1}),
        ),
        (
            "fn target_symbol() {}\n".repeat(17),
            json!({"pattern": "target_symbol", "path": "scope.rs"}),
        ),
        (
            format!("{}fn target_symbol() {{}}\n", "// padding\n".repeat(30_000)),
            json!({"pattern": "target_symbol", "path": "scope.rs"}),
        ),
    ] {
        fs::write(&path, body).expect("boundary fixture");
        let result = GrepFilesTool.execute(input, &ctx).await.expect("grep");
        let payload: Value = serde_json::from_str(&result.content).expect("json");
        assert!(payload.get("navigation").is_none(), "{payload}");
        assert!(crate::tools::echolocation::delivered_navigation_refs(&result.content).is_empty());
    }

    fs::write(&path, "fn target_symbol() {\n").expect("invalid syntax");
    let result = GrepFilesTool
        .execute(
            json!({"pattern": "target_symbol", "path": "scope.rs"}),
            &ctx,
        )
        .await
        .expect("grep still works for incomplete Rust");
    let payload: Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(payload["navigation"][0]["reason"], "syntax_error");
    assert!(crate::tools::echolocation::delivered_navigation_refs(&result.content).is_empty());
}

#[tokio::test]
async fn grep_declarations_offer_complete_guarded_reads_before_matching_uses() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("types.rs");
    let ctx = ToolContext::new(tmp.path());
    fs::write(&path, "const _: u32 = 9;\r\n").expect("write unnamed constant");
    let anonymous = GrepFilesTool
        .execute(json!({"pattern": "_", "path": "types.rs"}), &ctx)
        .await
        .expect("grep unnamed constant");
    let anonymous: Value = serde_json::from_str(&anonymous.content).expect("json");
    assert_eq!(anonymous["total_matches"], 1);
    assert!(
        anonymous["navigation"][0]["symbols"]
            .as_array()
            .expect("syntax scopes")
            .is_empty(),
        "an unnamed constant must not manufacture a named navigation target"
    );
    let prefix: String = (0..30)
        .map(|index| format!("fn prefix_{index}() {{}}\r\n"))
        .collect();
    for (name, declaration) in [
        (
            "Payload",
            "pub type Payload<T> =\r\n    Result<Vec<T>, ()>\r\n;\r\n",
        ),
        (
            "RETRIES",
            "pub const RETRIES: usize = {\r\n    3\r\n}\r\n;\r\n",
        ),
        (
            "DEFAULT_PORTS",
            "pub static DEFAULT_PORTS: [u16; 2] =\r\n    [80, 443]\r\n;\r\n",
        ),
        (
            "ACTIVE_PORTS",
            "pub static mut ACTIVE_PORTS: [u16; 2] =\r\n    [0, 0]\r\n;\r\n",
        ),
        (
            "RawPacket",
            "pub union RawPacket {\r\n    pub bytes: [u8; 4],\r\n    pub word: u32,\r\n}\r\n",
        ),
        (
            "Packet",
            "pub struct Packet {\r\n    pub body: u32,\r\n}\r\n",
        ),
        (
            "Pair",
            "pub struct Pair(\r\n    pub u8,\r\n    pub u16,\r\n);\r\n",
        ),
        ("Marker", "pub struct Marker;\r\n"),
        (
            "Mode",
            "pub enum Mode {\r\n    Idle,\r\n    Active { id: u32 },\r\n}\r\n",
        ),
        (
            "Channel",
            "pub trait Channel {\r\n    fn send(&self);\r\n}\r\n",
        ),
        ("UnsafeChannel", "unsafe\r\ntrait UnsafeChannel {}\r\n"),
        ("AutoChannel", "auto\r\ntrait AutoChannel {}\r\n"),
        ("const_fn", "const\r\nfn const_fn() {}\r\n"),
        ("async_fn", "async\r\nfn async_fn() {}\r\n"),
        ("unsafe_fn", "unsafe\r\nfn unsafe_fn() {}\r\n"),
        ("foreign_fn", "extern \"C\"\r\nfn foreign_fn() {}\r\n"),
        ("visible_fn", "pub(crate)\r\nfn visible_fn() {}\r\n"),
    ] {
        for header in [
            "",
            "/// Navigation contract 源\r\n#[cfg(feature = \"trace\")]\r\n",
        ] {
            let declaration = format!("{header}{declaration}");
            // Exercise late declarations, inline-module qualification, an earlier
            // matching use, and a declaration lookalike that must stay a comment.
            // Put the use on its own line: declaration matching is line-based.
            let source = format!(
                "mod transport {{\r\n{prefix}// struct {name} {{ fake }}\r\nfn consumer() {{\r\n    let _ = stringify!({name});\r\n}}\r\n{declaration}}}\r\n"
            );
            fs::write(&path, &source).expect("write declaration fixture");
            let result = GrepFilesTool
                .execute(json!({"pattern": name, "path": "types.rs"}), &ctx)
                .await
                .expect("grep declaration");
            let payload: Value = serde_json::from_str(&result.content).expect("json");
            assert_eq!(payload["total_matches"], 3);
            let symbols = payload["navigation"][0]["symbols"]
                .as_array()
                .expect("syntax scopes");
            assert_eq!(
                symbols[0]["name"],
                format!("transport::{name}"),
                "the complete declaration must precede the matching use"
            );
            assert_eq!(symbols.len(), 2, "no comment or unrelated prefix scopes");
            assert_eq!(symbols[0]["source"], declaration);
            let reference = symbols[0]["read"].clone();
            assert_eq!(reference["offset"], 36);
            assert_eq!(reference["limit"], declaration.lines().count());
            assert_eq!(
                reference["expected_revision"],
                crate::tools::file::content_hash(source.as_bytes())
            );
            if !header.is_empty() {
                let attributes = GrepFilesTool
                    .execute(json!({"pattern": "trace", "path": "types.rs"}), &ctx)
                    .await
                    .expect("find the owning declaration from an attribute-only hit");
                let attributes: Value = serde_json::from_str(&attributes.content).expect("json");
                assert_eq!(attributes["total_matches"], 1);
                assert_eq!(
                    attributes["navigation"][0]["symbols"][0]["read"], reference,
                    "attribute-only search must offer the owning declaration: {attributes}"
                );
                assert_eq!(
                    attributes["navigation"][0]["symbols"][0]["source"],
                    declaration
                );
            }
            let read = crate::tools::file_tool::ReadTool
                .execute(reference.clone(), &ctx)
                .await
                .expect("follow declaration reference through the real reader");
            for line in declaration.lines() {
                assert!(
                    read.content.contains(line),
                    "missing {line:?}: {}",
                    read.content
                );
            }
            assert!(!read.content.contains("fn consumer()"));
            assert!(!read.content.contains("fn prefix_0()"));

            fs::write(&path, source.replacen("fake", "edit", 1)).expect("same-size edit");
            let error = crate::tools::file_tool::ReadTool
                .execute(reference, &ctx)
                .await
                .expect_err("declaration references must reject stale snapshots too");
            assert!(error.to_string().contains("stale"), "{error}");
        }
    }
}

#[tokio::test]
async fn grep_prioritizes_matched_identifiers_not_other_text_on_their_line() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("ranking.rs");
    let ctx = ToolContext::new(tmp.path());
    // Several declarations can share one matched line. The symbol cap must
    // retain the matching identifier even when unrelated scopes fill it first.
    let crowded: String = (0..40)
        .map(|index| format!("fn unrelated_{index}() {{}} "))
        .collect();
    let crowded_functions =
        format!("fn consumer() {{ needle(); }}\r\n{crowded}fn needle() {{}}\r\n");
    let crowded_trait = format!(
        "fn consumer() {{ let _ = stringify!(needle); }}\r\ntrait Contract {{ {crowded}fn needle(); }}\r\n"
    );
    let crowded_impl = format!(
        "struct Owner; fn consumer() {{ Owner::needle(); }}\r\nimpl Owner {{ {crowded}fn needle() {{}} }}\r\n"
    );
    for (source, pattern, case_insensitive, expected, line) in [
        (
            "fn consumer() { target(); }\r\nfn target() {}\r\n",
            "target",
            false,
            "target",
            2,
        ),
        (
            "fn consumer() { let _ = Payload; }\r\npub struct Payload;\r\n",
            "Payload",
            false,
            "Payload",
            2,
        ),
        (
            "fn consumer() { let _ = Payload; }\r\npub struct Payload;\r\n",
            "pub|Payload",
            false,
            "Payload",
            2,
        ),
        (
            "// 源\r\nfn consumer() { let _ = Étape; }\r\npub struct Étape;\r\n",
            "Étape",
            false,
            "Étape",
            3,
        ),
        (
            "fn consumer() { let _ = r#type; }\r\npub struct r#type;\r\n",
            "r#type",
            false,
            "r#type",
            2,
        ),
        (
            "\u{feff}fn consumer() { let _ = A; }\r\nstruct A;\r\n",
            "A",
            false,
            "A",
            2,
        ),
        (
            "#!/usr/bin/env codew\r\nfn consumer() { let _ = A; }\r\nstruct A;\r\n",
            "A",
            false,
            "A",
            3,
        ),
        (
            "\u{feff}#!/usr/bin/env codew\r\nfn consumer() { let _ = A; }\r\nstruct A;\r\n",
            "A",
            false,
            "A",
            3,
        ),
        (
            "\u{feff}fn consumer() { let _ = A; }\r\nstruct A;\r\n",
            "^fn.*|A",
            false,
            "A",
            2,
        ),
        (
            "fn consumer() { let _ = a; }\r\nstruct A;\r\n",
            "a",
            true,
            "A",
            2,
        ),
        (
            "struct Payload;\r\nfn consumer() {}\r\n",
            r"consumer|\B",
            false,
            "consumer",
            2,
        ),
        (&crowded_functions, "needle", false, "needle", 2),
        (&crowded_trait, "needle", false, "Contract::needle", 2),
        (&crowded_impl, "needle", false, "impl@2::needle", 2),
    ] {
        fs::write(&path, source).expect("write ranking fixture");
        let result = GrepFilesTool
            .execute(
                json!({"pattern": pattern, "path": "ranking.rs", "context_lines": 0,
                    "case_insensitive": case_insensitive}),
                &ctx,
            )
            .await
            .expect("grep identifier");
        let payload: Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(payload["total_matches"], 2, "{payload}");
        assert_eq!(payload["matches"][0]["line_number"], line - 1);
        assert_eq!(payload["matches"][1]["line_number"], line);
        let symbol = &payload["navigation"][0]["symbols"][0];
        assert_eq!(
            symbol["name"], expected,
            "only a nonempty match overlapping the identifier earns definition priority: {payload}"
        );
        let reference = symbol["read"].clone();
        assert_eq!(reference["offset"], line);
        assert_eq!(reference["limit"], 1);
        assert_eq!(
            reference["expected_revision"],
            crate::tools::file::content_hash(source.as_bytes())
        );
        let read = crate::tools::file_tool::ReadTool
            .execute(reference, &ctx)
            .await
            .expect("follow ranked identifier reference");
        assert!(read.content.contains(source.lines().nth(line - 1).unwrap()));
    }
}

#[tokio::test]
async fn grep_associated_items_offer_focused_guarded_reads() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("members.rs");
    let ctx = ToolContext::new(tmp.path());
    let trait_items = [
        (
            "Payload",
            "    /// Payload contract\r\n    #[cfg(feature = \"trace\")]\r\n    type Payload:\r\n        Copy;\r\n",
        ),
        (
            "DefaultPayload",
            "    type DefaultPayload =\r\n        u16;\r\n",
        ),
        (
            "LIMIT",
            "    #[cfg(feature = \"trace\")]\r\n    const LIMIT: usize\r\n    ;\r\n",
        ),
        (
            "DEFAULT_LIMIT",
            "    const DEFAULT_LIMIT: usize = {\r\n        8\r\n    }\r\n    ;\r\n",
        ),
        (
            "send",
            "    #[must_use]\r\n    async\r\n    fn send(&self)\r\n    ;\r\n",
        ),
        (
            "flush",
            "    unsafe\r\n    fn flush(&mut self) -> bool {\r\n        true\r\n    }\r\n",
        ),
    ];
    let impl_items = [
        (
            "Payload",
            "    #[cfg(feature = \"trace\")]\r\n    default\r\n    type Payload =\r\n        u32;\r\n",
        ),
        (
            "LIMIT",
            "    default\r\n    const LIMIT: usize = {\r\n        4\r\n    }\r\n    ;\r\n",
        ),
        (
            "send",
            "    default\r\n    fn send(&self) {\r\n        let _ = ();\r\n    }\r\n",
        ),
    ];
    let prefix: String = (0..30)
        .map(|index| format!("fn prefix_{index}() {{}}\r\n"))
        .collect();
    let trait_body: String = trait_items.iter().map(|(_, body)| *body).collect();
    let impl_body: String = impl_items.iter().map(|(_, body)| *body).collect();
    let source = format!(
        "mod transport {{\r\n// stamp:a\r\n{prefix}fn consumer() {{\r\n    let _ = stringify!(Payload DefaultPayload LIMIT DEFAULT_LIMIT send flush);\r\n}}\r\npub trait Channel {{\r\n{trait_body}}}\r\nimpl Channel for () {{\r\n{impl_body}}}\r\n}}\r\n"
    );
    let impl_line = source[..source.find("impl Channel for () {").unwrap()]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    for (scope, items) in [
        ("transport::Channel::".to_string(), trait_items.as_slice()),
        (
            format!("transport::impl@{impl_line}::"),
            impl_items.as_slice(),
        ),
    ] {
        for (name, declaration) in items {
            fs::write(&path, &source).expect("write associated items");
            let result = GrepFilesTool
                .execute(
                    json!({"pattern": format!(r"\b{name}\b"), "path": "members.rs"}),
                    &ctx,
                )
                .await
                .expect("grep associated item");
            let payload: Value = serde_json::from_str(&result.content).expect("json");
            let symbols = payload["navigation"][0]["symbols"]
                .as_array()
                .expect("syntax scopes");
            assert!(
                symbols[0]["name"]
                    .as_str()
                    .is_some_and(|qualified| qualified.ends_with(&format!("::{name}"))),
                "associated definitions must precede uses and trait containers: {payload}"
            );
            let qualified = format!("{scope}{name}");
            let symbol = symbols
                .iter()
                .find(|symbol| symbol["name"].as_str() == Some(qualified.as_str()))
                .unwrap_or_else(|| panic!("missing {qualified}: {payload}"));
            assert_eq!(symbol["source"], *declaration);
            let offset = source[..source.find(declaration).unwrap()]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            let reference = symbol["read"].clone();
            assert_eq!(reference["path"], "members.rs");
            assert_eq!(reference["offset"], offset);
            assert_eq!(reference["limit"], declaration.lines().count());
            assert_eq!(
                reference["expected_revision"],
                crate::tools::file::content_hash(source.as_bytes())
            );
            let read = crate::tools::file_tool::ReadTool
                .execute(reference.clone(), &ctx)
                .await
                .expect("follow associated-item reference");
            for line in declaration.lines() {
                assert!(
                    read.content.contains(line),
                    "missing {line:?}: {}",
                    read.content
                );
            }
            assert!(!read.content.contains("fn consumer()"));
            assert!(!read.content.contains("fn prefix_0()"));

            let partial = crate::tools::file_tool::ReadTool
                .execute(
                    json!({"path": "members.rs", "offset": offset, "limit": 1,
                        "expected_revision": reference["expected_revision"]}),
                    &ctx,
                )
                .await
                .expect("partially read associated item beyond the cached prefix");
            let offered = crate::tools::echolocation::delivered_navigation_refs(&partial.content);
            assert_eq!(
                serde_json::to_value(offered.first()).unwrap(),
                reference,
                "complete the member before its enclosing trait: {}",
                partial.content
            );

            fs::write(&path, source.replace("stamp:a", "stamp:b")).expect("same-size edit");
            let error = crate::tools::file_tool::ReadTool
                .execute(reference, &ctx)
                .await
                .expect_err("associated-item references must reject stale snapshots");
            assert!(error.to_string().contains("stale"), "{error}");
        }
    }
}

#[tokio::test]
async fn grep_max_results_is_clamped_not_trusted() {
    let tmp = tempdir().expect("tempdir");
    let body = "needle here\n".repeat(1_200);
    fs::write(tmp.path().join("hay.txt"), &body).expect("write");
    let ctx = ToolContext::new(tmp.path());
    let result = GrepFilesTool
        .execute(json!({"pattern": "needle", "max_results": 1_000_000}), &ctx)
        .await
        .expect("grep runs");
    let payload: Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(payload["matches"].as_array().expect("matches").len(), 1000);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["total_matches"], 1000);
}

#[tokio::test]
async fn grep_truncated_fires_only_on_a_proven_cut() {
    let tmp = tempdir().expect("tempdir");
    // Exactly max: the walk completes, nothing cut, flag stays false.
    fs::write(tmp.path().join("exact.txt"), "needle\n".repeat(3)).expect("write");
    // Over max across files: the second file proves the cut.
    fs::write(tmp.path().join("a.txt"), "needle\n".repeat(2)).expect("write");
    fs::write(tmp.path().join("b.txt"), "needle\n".repeat(5)).expect("write");
    let ctx = ToolContext::new(tmp.path());

    let result = GrepFilesTool
        .execute(
            json!({"pattern": "needle", "path": "exact.txt", "max_results": 3}),
            &ctx,
        )
        .await
        .expect("grep runs");
    let payload: Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(payload["matches"].as_array().expect("matches").len(), 3);
    assert_eq!(payload["truncated"], false);

    let result = GrepFilesTool
        .execute(
            json!({"pattern": "needle", "path": "a.txt", "max_results": 1}),
            &ctx,
        )
        .await
        .expect("grep runs");
    let payload: Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(payload["matches"].as_array().expect("matches").len(), 1);
    assert_eq!(payload["truncated"], true);
}

#[test]
fn grep_description_matches_default_exclusion_behavior() {
    let description = GrepFilesTool.description();

    assert!(description.contains("skips common non-code directories"));
    assert!(!description.contains("respects `.gitignore`"));
}

/// Representative of the ~150 shared `optional_*` call sites outside the
/// three that motivated the change: a wrong type is refused by name, an
/// absent or null field still takes its default.
#[tokio::test]
async fn grep_refuses_mistyped_optional_parameters_by_name() {
    let tmp = tempdir().expect("tempdir");
    fs::write(tmp.path().join("a.txt"), "needle\n").expect("write");
    let ctx = ToolContext::new(tmp.path());

    for (field, input) in [
        (
            "max_results",
            json!({"pattern": "needle", "max_results": "10"}),
        ),
        (
            "case_insensitive",
            json!({"pattern": "needle", "case_insensitive": "true"}),
        ),
        (
            "context_lines",
            json!({"pattern": "needle", "context_lines": 2.5}),
        ),
        ("path", json!({"pattern": "needle", "path": ["."]})),
    ] {
        let err = GrepFilesTool
            .execute(input, &ctx)
            .await
            .expect_err("a mistyped optional parameter must be refused");
        let err = err.to_string();
        assert!(err.contains(field), "error must name '{field}': {err}");
    }

    GrepFilesTool
        .execute(
            json!({"pattern": "needle", "max_results": Value::Null, "path": Value::Null}),
            &ctx,
        )
        .await
        .expect("explicit nulls read as absent");
}

/// The model supplies the pattern, so it compiles through the bounded
/// regex cache: an over-long pattern is refused, not compiled.
#[tokio::test]
async fn grep_refuses_a_pattern_past_the_user_regex_length_cap() {
    let tmp = tempdir().expect("tempdir");
    fs::write(tmp.path().join("a.txt"), "needle\n").expect("write");
    let ctx = ToolContext::new(tmp.path());
    let long = "a".repeat(codewhale_runtime::regex_cache::MAX_USER_REGEX_PATTERN_BYTES + 1);
    let err = GrepFilesTool
        .execute(json!({ "pattern": long }), &ctx)
        .await
        .expect_err("an over-long pattern must be refused");
    assert!(err.to_string().contains("limit"), "{err}");
}

#[test]
fn test_matches_glob_star() {
    assert!(matches_glob("test.rs", "*.rs"));
    assert!(matches_glob("foo.rs", "*.rs"));
    assert!(!matches_glob("test.ts", "*.rs"));
    assert!(!matches_glob("test.rs.bak", "*.rs"));
}

#[test]
fn test_matches_glob_question() {
    assert!(matches_glob("test.rs", "test.??"));
    assert!(!matches_glob("test.rs", "test.?"));
}

#[test]
fn test_matches_glob_double_star() {
    assert!(matches_glob("src/main.rs", "src/**"));
    assert!(matches_glob("src/lib/mod.rs", "src/**"));
    assert!(matches_glob("node_modules/pkg/index.js", "node_modules/*"));
}

#[test]
fn test_matches_glob_path() {
    assert!(matches_glob("src/main.rs", "src/*.rs"));
    assert!(!matches_glob("lib/main.rs", "src/*.rs"));
}

/// Regression for #249: byte-index slicing panics on multi-byte
/// characters inside filenames like `dialogue_line__冰糖.mp3`.
#[test]
fn test_matches_glob_unicode_filename() {
    let filename = "dialogue_line__冰糖.mp3";
    // The filename should match *.mp3 without panicking.
    assert!(matches_glob(filename, "*.mp3"));
    // Asterisk matching against multi-byte characters must succeed.
    assert!(matches_glob(filename, "dialogue_line__*"));
    // Literal multi-byte characters inside the pattern must match.
    assert!(matches_glob(filename, "*冰糖*"));
    // Non-matching pattern must not panic either.
    assert!(!matches_glob(filename, "nonexistent*"));
}

#[tokio::test]
async fn grep_pods_chart_matched_dirs_with_marks() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    fs::create_dir(tmp.path().join("net")).expect("subdir");
    fs::write(tmp.path().join("net/server.rs"), "fn serve() {}\n").expect("write");
    fs::write(tmp.path().join("net/client.rs"), "fn connect() {}\n").expect("write");

    let result = GrepFilesTool
        .execute(json!({"pattern": "serve"}), &ctx)
        .await
        .expect("execute");
    assert!(result.success);
    let body: Value = serde_json::from_str(&result.content).expect("json result");
    assert_eq!(body["total_matches"], 1);
    let pods = body["pods"].as_array().expect("pods array");
    assert_eq!(pods.len(), 1);
    assert_eq!(pods[0]["dir"], "net");
    let mates: Vec<&str> = pods[0]["mates"]
        .as_array()
        .expect("mates")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(mates.contains(&"client.rs"), "{mates:?}");
    assert!(mates.contains(&"server.rs (match)"), "{mates:?}");
    assert_eq!(body["pods_omitted"], 0);
}

#[tokio::test]
async fn test_grep_files_basic() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    // Create test files
    fs::write(
        tmp.path().join("test.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .expect("write");
    fs::write(
        tmp.path().join("lib.rs"),
        "pub fn hello() {}\npub fn world() {}\n",
    )
    .expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "fn"}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    assert!(result.content.contains("main"));
    assert!(result.content.contains("hello"));
}

#[tokio::test]
async fn test_grep_files_with_context() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    fs::write(
        tmp.path().join("test.txt"),
        "line1\nline2\nMATCH\nline4\nline5\n",
    )
    .expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "MATCH", "context_lines": 1}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    assert!(result.content.contains("line2")); // context before
    assert!(result.content.contains("line4")); // context after

    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["context_before"], "line2");
    assert_eq!(matches[0]["context_after"], "line4");
    assert!(matches[0]["context_before"].is_string());
    assert!(matches[0]["context_after"].is_string());
}

#[tokio::test]
async fn test_grep_files_multi_line_context_remains_arrays() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    fs::write(tmp.path().join("test.txt"), "a\nb\nMATCH\nd\ne\n").expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "MATCH", "context_lines": 2}), &ctx)
        .await
        .expect("execute");

    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["context_before"], json!(["a", "b"]));
    assert_eq!(matches[0]["context_after"], json!(["d", "e"]));
}

#[tokio::test]
async fn test_grep_files_case_insensitive() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    fs::write(
        tmp.path().join("test.txt"),
        "Hello World\nHELLO WORLD\nhello world\n",
    )
    .expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "hello", "case_insensitive": true}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    // Should find all 3 lines
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["total_matches"].as_u64().unwrap(), 3);
}

#[tokio::test]
async fn test_grep_files_include_filter() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    fs::write(tmp.path().join("test.rs"), "fn test() {}\n").expect("write");
    fs::write(tmp.path().join("test.js"), "function test() {}\n").expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "test", "include": ["*.rs"]}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    // Should only match .rs file
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    let file = matches[0]["file"].as_str().unwrap();
    assert!(
        file.rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
    );
}

#[tokio::test]
#[cfg(unix)]
async fn test_grep_files_does_not_follow_symlinked_files() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().join("workspace");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&root).expect("mkdir workspace");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    let outside_file = outside.join("secret.txt");
    fs::write(&outside_file, "NEEDLE\n").expect("write outside");
    std::os::unix::fs::symlink(&outside_file, root.join("secret.txt")).expect("symlink");

    let ctx = ToolContext::new(root);
    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "NEEDLE"}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["total_matches"].as_u64().unwrap(), 0);
    assert_eq!(parsed["files_searched"].as_u64().unwrap(), 0);
}

#[tokio::test]
#[cfg(unix)]
async fn test_grep_files_default_mode_skips_symlinked_directories_but_keeps_real_files() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let real_dir = workspace.join("real");
    std::fs::create_dir_all(&real_dir).expect("mkdir workspace");
    fs::write(real_dir.join("needle.txt"), "NEEDLE\n").expect("write real file");
    std::os::unix::fs::symlink(&workspace, real_dir.join("loop")).expect("symlink loop");

    let ctx = ToolContext::new(workspace);
    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "NEEDLE"}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["total_matches"].as_u64().unwrap(), 1);
    assert_eq!(parsed["files_searched"].as_u64().unwrap(), 1);
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert!(
        matches[0]["file"]
            .as_str()
            .unwrap()
            .ends_with("real/needle.txt")
    );
}

#[tokio::test]
#[cfg(unix)]
async fn test_grep_files_follow_symlinks_avoids_directory_cycles() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    let real_dir = workspace.join("real");
    fs::create_dir_all(&real_dir).expect("mkdir");
    fs::write(real_dir.join("needle.txt"), "NEEDLE\n").expect("write");
    std::os::unix::fs::symlink(&workspace, real_dir.join("loop")).expect("symlink loop");

    let ctx = ToolContext::new(workspace).with_follow_symlinks(true);
    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "NEEDLE"}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["total_matches"].as_u64().unwrap(), 1);
    assert_eq!(parsed["files_searched"].as_u64().unwrap(), 1);
    let matches = parsed["matches"].as_array().unwrap();
    assert!(matches[0]["file"].as_str().unwrap().ends_with("needle.txt"));
}

#[tokio::test]
async fn test_grep_files_invalid_regex() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    let tool = GrepFilesTool;
    let result = tool.execute(json!({"pattern": "[invalid"}), &ctx).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_grep_files_respects_cancel_token() {
    let tmp = tempdir().expect("tempdir");
    fs::write(tmp.path().join("test.txt"), "needle\n").expect("write");
    let cancel_token = CancellationToken::new();
    cancel_token.cancel();
    let ctx = ToolContext::new(tmp.path().to_path_buf()).with_cancel_token(cancel_token);

    let tool = GrepFilesTool;
    let err = tool
        .execute(json!({"pattern": "needle"}), &ctx)
        .await
        .expect_err("cancelled grep should return an error");

    assert!(
        format!("{err:?}").contains("cancelled"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn test_grep_files_streaming_stops_at_max_results() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    // Two files with many matches each; the walk must stop once the
    // budget is exhausted without dropping context for the last match.
    for name in ["a.txt", "b.txt"] {
        let body: String = (1..=20).map(|n| format!("needle {n}\n")).collect();
        fs::write(tmp.path().join(name), body).expect("write");
    }

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "needle", "max_results": 5}), &ctx)
        .await
        .expect("execute");

    assert!(result.success);
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 5);
    assert_eq!(parsed["total_matches"].as_u64().unwrap(), 5);
    // All five matches must come from the first file walked, in file
    // order (streaming preserves walk order).
    let first_file = matches[0]["file"].as_str().unwrap().to_string();
    for m in matches {
        assert_eq!(m["file"].as_str().unwrap(), first_file);
    }
    // The final in-budget match still gets its full after-context even
    // though the match budget was exhausted on it.
    assert_eq!(
        matches[4]["context_after"],
        json!(["needle 6", "needle 7"]),
        "last match must keep after-context lines"
    );
}

#[tokio::test]
async fn test_grep_files_ring_buffer_context_matches_full_read() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    // Matches at the start, middle, and end of the file exercise the
    // partial before-context (ring not yet full) and truncated
    // after-context (EOF) paths.
    fs::write(
        tmp.path().join("ctx.txt"),
        "MATCH first\nb1\nb2\nb3\nMATCH mid\na1\na2\na3\nMATCH last\n",
    )
    .expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "MATCH", "context_lines": 2}), &ctx)
        .await
        .expect("execute");

    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 3);
    // Token diet: empty context arrays are omitted on the wire.
    assert!(matches[0].get("context_before").is_none());
    assert_eq!(matches[0]["context_after"], json!(["b1", "b2"]));
    assert_eq!(matches[1]["context_before"], json!(["b2", "b3"]));
    assert_eq!(matches[1]["context_after"], json!(["a1", "a2"]));
    assert_eq!(matches[2]["context_before"], json!(["a2", "a3"]));
    assert!(matches[2].get("context_after").is_none());
    assert_eq!(matches[2]["line_number"].as_u64().unwrap(), 9);
}

#[tokio::test]
async fn test_grep_files_streaming_skips_invalid_utf8_files() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());

    // Invalid UTF-8 after a matching line: the whole file must be
    // skipped, matching the historical read_to_string behavior.
    fs::write(
        tmp.path().join("binary.txt"),
        [b"needle\n".as_slice(), &[0xFF, 0xFE, 0x00]].concat(),
    )
    .expect("write");
    fs::write(tmp.path().join("clean.txt"), "needle\n").expect("write");

    let tool = GrepFilesTool;
    let result = tool
        .execute(json!({"pattern": "needle"}), &ctx)
        .await
        .expect("execute");

    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["total_matches"].as_u64().unwrap(), 1);
    assert_eq!(parsed["files_searched"].as_u64().unwrap(), 1);
    let matches = parsed["matches"].as_array().unwrap();
    assert!(matches[0]["file"].as_str().unwrap().ends_with("clean.txt"));
}

#[test]
fn test_grep_files_tool_properties() {
    let tool = GrepFilesTool;
    assert_eq!(tool.name(), "grep_files");
    assert!(tool.is_read_only());
    assert!(tool.is_sandboxable());
    assert_eq!(tool.approval_requirement(), ApprovalRequirement::Auto);
}

#[test]
fn test_parallel_support_flags() {
    let tool = GrepFilesTool;
    assert!(tool.supports_parallel());
}

#[test]
fn double_star_exclude_prefix_stops_at_a_path_component() {
    // Directory excludes still cover the directory and everything under it.
    assert!(matches_glob("build", "build/**"));
    assert!(matches_glob("build/out.o", "build/**"));
    assert!(matches_glob(".git/config", ".git/**"));
    // A same-prefix sibling file or directory is a different path.
    assert!(!matches_glob("build.rs", "build/**"));
    assert!(!matches_glob("build.gradle", "build/**"));
    assert!(!matches_glob("builders/x.rs", "build/**"));
    assert!(!matches_glob(".github/workflows/ci.yml", ".git/**"));
    assert!(!matches_glob(".gitignore", ".git/**"));
    assert!(!matches_glob("targets/a.rs", "target/**"));
}

#[tokio::test]
async fn grep_files_reports_truncation_when_matches_exceed_the_cap() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    // Three files, two matches each: six matches against a cap of 3.
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(tmp.path().join(name), "needle\nneedle\n").expect("write");
    }

    let capped = GrepFilesTool
        .execute(json!({"pattern": "needle", "max_results": 3}), &ctx)
        .await
        .expect("execute");
    let capped: Value = serde_json::from_str(&capped.content).unwrap();
    assert_eq!(capped["matches"].as_array().unwrap().len(), 3);
    assert_eq!(capped["truncated"], json!(true), "{capped}");

    // Exactly at the cap with nothing more is complete, not truncated.
    let exact = GrepFilesTool
        .execute(json!({"pattern": "needle", "max_results": 6}), &ctx)
        .await
        .expect("execute");
    let exact: Value = serde_json::from_str(&exact.content).unwrap();
    assert_eq!(exact["matches"].as_array().unwrap().len(), 6);
    assert_eq!(exact["truncated"], json!(false), "{exact}");
}

#[cfg(unix)]
#[tokio::test]
async fn grep_files_skips_an_unreadable_subdirectory() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    fs::write(tmp.path().join("found.txt"), "needle\n").expect("write");
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).expect("mkdir");
    fs::write(locked.join("hidden.txt"), "needle\n").expect("write");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    if fs::read_dir(&locked).is_ok() {
        // Running with privileges that ignore directory modes; nothing to prove.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
        return;
    }

    let result = GrepFilesTool
        .execute(json!({"pattern": "needle"}), &ctx)
        .await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");

    let result = result.expect("an unreadable subdirectory must not fail the search");
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let matches = parsed["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "{parsed}");
    assert!(
        matches[0]["file"]
            .as_str()
            .is_some_and(|file| file.ends_with("found.txt")),
        "{parsed}"
    );
    assert_eq!(parsed["unreadable_paths_skipped"], json!(1), "{parsed}");
}

#[test]
fn double_star_without_a_directory_boundary_is_a_string_prefix() {
    assert!(matches_glob("src/test_foo.rs", "src/test_**"));
    assert!(matches_glob("src/test_dir/a.rs", "src/test_**"));
    assert!(!matches_glob("src/other.rs", "src/test_**"));
}

#[tokio::test]
async fn grep_files_include_directory_glob_matches_nested_files() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    fs::create_dir_all(tmp.path().join("src").join("inner")).expect("mkdir");
    fs::create_dir_all(tmp.path().join("srcs")).expect("mkdir");
    fs::write(
        tmp.path().join("src").join("inner").join("a.rs"),
        "needle\n",
    )
    .expect("write");
    fs::write(tmp.path().join("srcs").join("b.rs"), "needle\n").expect("write");

    let result = GrepFilesTool
        .execute(json!({"pattern": "needle", "include": ["src/**"]}), &ctx)
        .await
        .expect("execute");
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    let files: Vec<&str> = parsed["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["file"].as_str())
        .collect();
    assert_eq!(files.len(), 1, "{parsed}");
    assert!(
        files[0].replace('\\', "/").ends_with("src/inner/a.rs"),
        "{parsed}"
    );
}

#[tokio::test]
async fn grep_files_stops_at_the_cap_without_scanning_further_files() {
    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
        fs::write(tmp.path().join(name), "needle\n").expect("write");
    }

    let result = GrepFilesTool
        .execute(json!({"pattern": "needle", "max_results": 2}), &ctx)
        .await
        .expect("execute");
    let parsed: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(parsed["matches"].as_array().unwrap().len(), 2, "{parsed}");
    // The budget fills exactly at the end of the second file; the walk
    // must stop there rather than scan on looking for one more match.
    assert_eq!(parsed["files_searched"], json!(2), "{parsed}");
    assert_eq!(parsed["truncated"], json!(true), "{parsed}");
}

#[cfg(unix)]
#[tokio::test]
async fn grep_files_counts_an_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().expect("tempdir");
    let ctx = ToolContext::new(tmp.path().to_path_buf());
    fs::write(tmp.path().join("found.txt"), "needle\n").expect("write");
    let locked = tmp.path().join("locked.txt");
    fs::write(&locked, "needle\n").expect("write");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    if fs::File::open(&locked).is_ok() {
        // Running with privileges that ignore file modes; nothing to prove.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).expect("chmod");
        return;
    }

    let result = GrepFilesTool
        .execute(json!({"pattern": "needle"}), &ctx)
        .await;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).expect("chmod");

    let parsed: Value = serde_json::from_str(&result.expect("execute").content).unwrap();
    assert_eq!(parsed["matches"].as_array().unwrap().len(), 1, "{parsed}");
    assert_eq!(parsed["unreadable_paths_skipped"], json!(1), "{parsed}");
}

#[test]
fn pathological_globs_stay_bounded() {
    let started = std::time::Instant::now();

    let nested = "{{a},c0,c1,c2,c3,c4,c5,c6,c7,c8,c9}".repeat(10);
    assert!(!matches_glob("file.txt", &nested));
    assert!(matches_glob("c3c7.txt", "{{a},c3}{{b},c7}.txt"));

    let deep = format!("{}x/b", "d/".repeat(30));
    let repeated = format!("{}x/b", "**/".repeat(10));
    assert!(matches_glob(&deep, &repeated));
    assert!(!matches_glob(&format!("{}x/c", "d/".repeat(30)), &repeated));

    let deep_nesting = format!("{}x{}", "{".repeat(8000), ",y}".repeat(8000));
    assert!(!matches_glob("x", &deep_nesting));

    assert!(matches_glob("src/a/src", "src/**/**/src"));
    assert!(!matches_glob("src", "src/**/**/src"));
    assert!(!matches_glob("src/foo.rs", "src/**/**/src"));
    assert!(matches_glob("src1/a/b/file.rs", "src*/**/**/file.rs"));

    let many_stars = format!("{}b", "*a".repeat(10));
    let a40 = "a".repeat(40);
    assert!(!matches_glob(&format!("{a40}.rs"), &many_stars));
    assert!(matches_glob(&format!("{a40}b"), &many_stars));

    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
}
