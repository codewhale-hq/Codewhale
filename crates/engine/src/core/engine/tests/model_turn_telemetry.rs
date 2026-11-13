use super::*;

#[test]
fn model_turn_telemetry_names_the_model_the_last_request_used() {
    let mut turn = crate::core::turn::TurnContext::new(4);
    turn.add_parent_usage(&Usage {
        input_tokens: 10,
        output_tokens: 5,
        ..Usage::default()
    });
    turn.note_parent_model("deepseek-v4pro");

    record_model_turn_telemetry(
        codewhale_config::ProviderKind::Deepseek,
        "auto",
        &turn,
        TurnOutcomeStatus::Completed,
    );

    let reported = codewhale_telemetry::session_counters()
        .usage_events()
        .into_iter()
        .any(|event| {
            matches!(
                event,
                codewhale_telemetry::Event::ModelUsage { provider, model, .. }
                    if provider == "deepseek" && model == "deepseek-v4-pro"
            )
        });
    assert!(reported, "the resolved wire model, not `auto`, is reported");
}
