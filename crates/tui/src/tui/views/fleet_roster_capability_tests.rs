/// #5038: a pinned model surfaces capability badges from the shared Fleet
/// resolver; members inheriting the session route omit the field, and an
/// unknown pinned model degrades to honest absence, never fabricated facts.
#[test]
fn detail_shows_capability_badges_for_pinned_models_only() {
    fn detail_text(member: &AgentProfile) -> String {
        member_detail_lines_with_session(member, None, &[], Locale::En)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.clone().into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Pinned known model (glm-5.2, no provider): registry facts.
    let view = view_with_overrides();
    let reviewer = view.members.iter().find(|m| m.id == "reviewer").unwrap();
    let text = detail_text(reviewer);
    assert!(text.contains("Capabilities"), "{text}");
    assert!(text.contains("1M ctx"), "{text}");
    assert!(text.contains("(registry)"), "{text}");

    // Inheriting member: no pinned model, no capabilities field.
    let scout = FleetRoster::built_ins_only().get("scout").unwrap().clone();
    assert!(!detail_text(&scout).contains("Capabilities"));

    // Unknown pinned model: the field is omitted rather than guessed.
    let mut unknown = reviewer.clone();
    unknown.profile.model = Some("totally-made-up-model-xyz".to_string());
    let text = detail_text(&unknown);
    assert!(!text.contains("Capabilities"), "{text}");
    assert!(text.contains("model totally-made-up-model-xyz"), "{text}");
}

/// #5038: the pinned operator row names its session model's capabilities.
#[test]
fn operator_detail_surfaces_session_model_capabilities() {
    let text = operator_detail_lines(&operator())
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone().into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Capabilities"), "{text}");
    assert!(text.contains("1.05M ctx"), "{text}");
    assert!(text.contains("tools"), "{text}");
    assert!(
        text.contains("catalog"),
        "operator should use provider-scoped catalog facts: {text}"
    );
}
