// === Live /models limits: the ceilings a provider states itself ==========
//
// Every route that is not OrcaRouter, OpenRouter or Concentrate used to build
// its offerings with `limit: None`, so a provider publishing context and
// output ceilings in its own `/models` body lost them and the route fell back
// to the legacy 128K window. These tests pin the shapes that are read, the
// precedence among them, and the rule that one unreadable field costs only its
// own candidate.
//
// Synthetic ids only; nothing here touches the network.

#[test]
fn limits_a_live_listing_states_context_and_output_ceilings() {
    let limits = listed_limits_by_model_id(
        r#"{"object":"list","data":[
            {"id":"row-openai","limit":{"context":1048000,"output":64000}},
            {"id":"row-flat","context_length":200000,"max_output_tokens":32000},
            {"id":"row-window","context_window":131072},
            {"id":"row-len","max_model_len":262144},
            {"id":"row-max","max_context_length":524288,"max_completion_tokens":16000}
        ]}"#,
    );
    let row = |id: &str| {
        limits
            .get(id)
            .unwrap_or_else(|| panic!("no limit for {id}"))
    };
    assert_eq!(row("row-openai").context, Some(1_048_000));
    assert_eq!(row("row-openai").output, Some(64_000));
    assert_eq!(row("row-flat").context, Some(200_000));
    assert_eq!(row("row-flat").output, Some(32_000));
    assert_eq!(row("row-window").context, Some(131_072));
    assert_eq!(row("row-len").context, Some(262_144));
    assert_eq!(row("row-max").context, Some(524_288));
    assert_eq!(row("row-max").output, Some(16_000));
    // `input` stays empty: no route is clamped on an inference.
    assert!(limits.values().all(|limit| limit.input.is_none()));
}

#[test]
fn limits_a_relay_metadata_block_states_capability_ahead_of_budget() {
    let limits = listed_limits_by_model_id(
        r#"{"data":[
            {"id":"row-both","metadata":{"context_limit":1000000,"context_budget":200000,"output_budget":32000}},
            {"id":"row-budget","metadata":{"context_budget":131072}}
        ]}"#,
    );
    // `context_limit` is the model capability; `context_budget` is the share
    // one deployment accepts, so the capability wins when both are stated.
    assert_eq!(limits["row-both"].context, Some(1_000_000));
    assert_eq!(limits["row-both"].output, Some(32_000));
    assert_eq!(limits["row-budget"].context, Some(131_072));
}

#[test]
fn limits_one_unreadable_field_costs_only_its_own_candidate() {
    // Selecting a field by presence and parsing whatever was selected let an
    // unreadable high-priority field hide every readable field below it, so
    // the route fell back to 128K even though the listing stated a window.
    let limits = listed_limits_by_model_id(
        r#"{"data":[
            {"id":"row-mixed","context_length":200000,"metadata":{"context_limit":"1M"}},
            {"id":"row-block","context_length":300000,"limit":{"context":null}}
        ]}"#,
    );
    assert_eq!(limits["row-mixed"].context, Some(200_000));
    assert_eq!(limits["row-block"].context, Some(300_000));
}

#[test]
fn limits_a_row_stating_no_readable_ceiling_is_left_out() {
    let limits = listed_limits_by_model_id(
        r#"{"data":[
            {"id":"row-none"},
            {"id":"row-zero","context_length":0},
            {"id":"row-bad","context_length":"unbounded"}
        ]}"#,
    );
    assert!(limits.is_empty(), "{limits:?}");
}

#[test]
fn limits_a_body_without_a_data_array_yields_no_limits() {
    assert!(listed_limits_by_model_id(r#"{"object":"list"}"#).is_empty());
    assert!(listed_limits_by_model_id("not json").is_empty());
}

#[test]
fn limits_a_generic_route_keeps_the_limits_its_listing_states() {
    let delta = catalog_delta_from_models_body(
        ProviderKind::Openai,
        "openai".to_string(),
        "https://relay.example/v1",
        r#"{"data":[{"id":"gpt-relay","context_length":1048000,"max_output_tokens":32000}]}"#,
        &[],
    )
    .expect("generic route delta");
    let row = delta
        .offerings
        .iter()
        .find(|row| row.wire_model_id == "gpt-relay")
        .expect("listed row");
    let limit = row.limit.as_ref().expect("listed limits are kept");
    assert_eq!(limit.context, Some(1_048_000));
    assert_eq!(limit.output, Some(32_000));
    assert_eq!(limit.input, None);

    let body = r#"{"data":[
        {"id":"deepseek-flash","context_window":1048576,
         "effort":{"supported_levels":["low","high","max"],"default_level":"high"}},
        {"id":"new-model","effort":{"supported_levels":["max","future","low"],"default_level":"future"}},
        {"id":"unknown-model"}
    ]}"#;
    let direct = catalog_delta_from_models_body(
        ProviderKind::Deepseek,
        "deepseek".to_string(),
        "https://api.deepseek.com/v1/",
        body,
        &[],
    )
    .expect("direct DeepSeek catalog");
    let find = |id: &str| {
        direct
            .offerings
            .iter()
            .find(|row| row.wire_model_id == id)
            .unwrap()
    };
    assert_eq!(
        find("deepseek-flash").limit.as_ref().unwrap().context,
        Some(1_048_576)
    );
    assert_eq!(
        find("deepseek-flash").reasoning_options,
        vec![
            json!({"type":"toggle"}),
            json!({"type":"effort", "values":["low","high","max"], "default":"high"})
        ]
    );
    let projected =
        crate::reasoning_preference::ReasoningEffort::catalog_effort_values_for_offering(find(
            "deepseek-flash",
        ))
        .unwrap()
        .into_iter()
        .filter(|effort| {
            !matches!(
                effort,
                crate::reasoning_preference::ReasoningEffort::Off
                    | crate::reasoning_preference::ReasoningEffort::Auto
            )
        })
        .map(|effort| effort.as_setting())
        .collect::<Vec<_>>();
    assert_eq!(projected, ["low", "high", "max"]);
    assert_eq!(
        find("new-model").reasoning_options,
        vec![
            json!({"type":"toggle"}),
            json!({"type":"effort", "values":["max","low"]})
        ]
    );
    assert!(find("unknown-model").reasoning_options_present);
    assert!(find("unknown-model").reasoning_options.is_empty());
    assert!(
        crate::reasoning_preference::ReasoningEffort::catalog_effort_values_for_offering(find(
            "unknown-model"
        ))
        .is_none()
    );
    let mut unknown = find("unknown-model").clone();
    let mut old = unknown.clone();
    old.reasoning_options = vec![json!({"type":"effort", "values":["high"]})];
    old.reasoning_options_present = true;
    unknown.complete_from(&old, Some("https://api.deepseek.com/v1"));
    assert!(unknown.reasoning_options.is_empty());

    for (provider, endpoint) in [
        (ProviderKind::Deepseek, "https://relay.example/v1"),
        (
            ProviderKind::Deepseek,
            "https://api.deepseek.com.example/v1",
        ),
        (ProviderKind::Custom, "https://api.deepseek.com/v1"),
    ] {
        let proxy =
            catalog_delta_from_models_body(provider, "proxy".to_string(), endpoint, body, &[])
                .expect("compatible route catalog");
        assert!(
            proxy
                .offerings
                .iter()
                .all(|row| !row.reasoning_options_present)
        );
        assert!(
            proxy
                .offerings
                .iter()
                .all(|row| row.reasoning_options.is_empty())
        );
    }
    for invalid in [
        r#"{"data":[{"id":"a","effort":{"supported_levels":"high"}}]}"#,
        r#"{"data":[{"id":"a","effort":{"supported_levels":["low"],"default_level":"high"}}]}"#,
        r#"{"data":[{"id":"a","effort":{"supported_levels":["low","low"]}}]}"#,
        r#"{"data":[{"id":"a"},{"id":"a"}]}"#,
        r#"{"data":[{"id":"a","effort":{"supported_levels":["bad\nvalue"]}}]}"#,
    ] {
        assert!(
            catalog_delta_from_models_body(
                ProviderKind::Deepseek,
                "deepseek".to_string(),
                "https://api.deepseek.com",
                invalid,
                &[],
            )
            .is_err()
        );
    }
}

#[test]
fn siwc_refresh_rotates_atomically_and_rejects_a_different_account() {
    codewhale_runtime::oauth::test_support::siwc_refresh_rotates_atomically_and_rejects_a_different_account(|config| CodewhaleClient::new(config).unwrap().chatgpt_reasoning_api);
}
