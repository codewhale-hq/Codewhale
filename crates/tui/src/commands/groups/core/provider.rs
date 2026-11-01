//! Provider switching: flip between DeepSeek, hosted providers, and self-hosted
//! OpenAI-compatible DeepSeek V4 servers at runtime.
//!
//! `/provider` with no args opens the picker modal (#52). `/provider <name>`
//! keeps the v0.6.6 CLI form for muscle-memory + scripted use.

use crate::commands::traits::{CommandInfo, RegisterCommand};
use crate::tui::app::{App, AppAction};
use codewhale_config::ProviderKind;
use codewhale_localization::MessageId;
use codewhale_runtime::config::{canonical_model_id_for_provider, provider_passes_model_through};

use super::CommandResult;

pub(in crate::commands) const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "provider",
    aliases: &[],
    usage: "/provider [setup [name]|name [model]]",
    description_id: MessageId::CmdProviderDescription,
};

pub(in crate::commands) struct ProviderCmd;

impl RegisterCommand for ProviderCmd {
    fn info() -> &'static CommandInfo {
        &COMMAND_INFO
    }

    fn execute(app: &mut App, arg: Option<&str>) -> CommandResult {
        provider(app, arg)
    }
}

/// Switch or view the current LLM backend.
///
/// With no args, opens the picker modal. With `<provider> [model]`, performs
/// the switch directly (e.g. `/provider nim flash` lands on
/// `deepseek-ai/deepseek-v4-flash`). The optional model accepts shorthand
/// (`flash`, `pro`, `v4-flash`, `v4-pro`) or any normal provider model ID.
pub fn provider(app: &mut App, args: Option<&str>) -> CommandResult {
    let trimmed = args.map(str::trim).filter(|s| !s.is_empty());
    let Some(args) = trimmed else {
        return CommandResult::action(AppAction::OpenProviderPicker);
    };

    let mut parts = args.split_whitespace();
    let name = parts.next().unwrap_or("");
    let model_arg = parts.next();

    if name.eq_ignore_ascii_case("fallback") {
        return provider_fallback(app, model_arg);
    }
    if name.eq_ignore_ascii_case("setup") {
        return match model_arg {
            None => CommandResult::action(AppAction::OpenProviderSetup { provider: None }),
            Some(raw) => match provider_setup_action_for_name(raw) {
                Ok(action) => CommandResult::action(action),
                Err(message) => CommandResult::error(message),
            },
        };
    }

    if codewhale_runtime::config::is_legacy_antigravity_identity(name) {
        return CommandResult::error(
            codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE.to_string(),
        );
    }

    let Some(row) = codewhale_config::descriptors::compatibility_for_selector(name) else {
        return CommandResult::error(format!(
            "Unknown provider '{name}'. Expected: {}.",
            ProviderKind::names_hint()
        ));
    };

    let target = row.kind;
    let selected_key: codewhale_config::ProviderId = row.id.into();

    let model = match model_arg {
        None => None,
        Some(raw) => {
            // Expand provider shorthands (flash/pro, Xiaomi MiMo tts/omni, …)
            // uniformly, then either keep the id verbatim for providers that take
            // opaque/custom model tags, or resolve it to the canonical family id.
            // Families are treated equally: each resolves through its own
            // canonical map (DeepSeek, GLM via Z.ai/Zhipu, Kimi, MiniMax, …) and
            // an id matching none passes through unchanged — the upstream API is
            // the authority. Wire-id translation is deferred to the route
            // resolver at request time. DeepSeek's two retiring aliases are
            // also deferred because this command does not own the target base
            // URL: a custom endpoint may still use either id natively.
            let expanded = expand_model_alias_for_provider(target, raw);
            if provider_passes_model_through(target)
                || is_route_ambiguous_deepseek_alias(target, &expanded)
            {
                Some(expanded)
            } else {
                match canonical_model_id_for_provider(target, &expanded) {
                    Some(canonical) => Some(canonical),
                    None => {
                        return CommandResult::error(format!(
                            "Invalid model '{raw}'. Provide a non-empty model id."
                        ));
                    }
                }
            }
        }
    };

    if app
        .provider_identity
        .as_ref()
        .is_some_and(|identity| identity.key == selected_key)
        && model.is_none()
    {
        return CommandResult::message(format!("Already on provider: {}", selected_key));
    }

    CommandResult::action(AppAction::SwitchProvider {
        provider: selected_key,
        model,
    })
}

pub(in crate::commands) fn provider_setup_action_for_name(raw: &str) -> Result<AppAction, String> {
    if codewhale_runtime::config::is_legacy_antigravity_identity(raw) {
        return Err(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE.to_string());
    }
    if raw.eq_ignore_ascii_case("ds4") || raw.eq_ignore_ascii_case("dwarfstar") {
        return Ok(AppAction::OpenDs4Setup);
    }
    // First-class aliases (zen, opencode-zen, …) resolve through the provider
    // registry. There are no setup templates anymore: named custom hosts are
    // configured with `/provider setup` and the blank custom form (#6289).
    match codewhale_config::descriptors::compatibility_for_selector(raw) {
        Some(provider) => Ok(AppAction::OpenProviderSetup {
            provider: Some(provider.id.into()),
        }),
        None => Err(format!(
            "Unknown provider '{raw}'. Expected: {}.",
            ProviderKind::names_hint()
        )),
    }
}

fn is_route_ambiguous_deepseek_alias(provider: ProviderKind, model: &str) -> bool {
    matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) && (model.eq_ignore_ascii_case("deepseek-chat")
        || model.eq_ignore_ascii_case("deepseek-reasoner"))
}

fn provider_fallback(app: &mut App, subcommand: Option<&str>) -> CommandResult {
    match subcommand {
        Some("reset") => {
            let Some((_, primary, _)) = app.fallback_chain_entries().first().copied() else {
                return CommandResult::message(
                    "No fallback providers configured. Add `fallback_providers` to your config.",
                );
            };
            CommandResult::with_message_and_action(
                format!(
                    "Fallback chain reset to primary provider: {}.",
                    primary.as_str()
                ),
                AppAction::SwitchProvider {
                    provider: primary.as_str().into(),
                    model: None,
                },
            )
        }
        Some(other) => CommandResult::error(format!(
            "Unknown fallback command '{other}'. Usage: /provider fallback [reset]"
        )),
        None => {
            let entries = app.fallback_chain_entries();
            if entries.is_empty() {
                return CommandResult::message(
                    "No fallback providers configured. Add `fallback_providers` to your config.",
                );
            }

            let mut lines = vec![
                format!(
                    "Current provider: {}",
                    app.provider_identity_for_persistence()
                ),
                "Fallback chain:".to_string(),
            ];
            for (index, provider, is_current) in entries {
                let role = if index == 0 { "primary" } else { "fallback" };
                let marker = if is_current { " <- current" } else { "" };
                lines.push(format!(
                    "  [{index}] {} ({role}){marker}",
                    provider.as_str()
                ));
            }
            if let Some(reason) = app.last_fallback_reason.as_deref() {
                lines.push(format!("Last fallback: {reason}"));
            }
            lines.push("Use `/provider fallback reset` to return to the primary provider.".into());
            CommandResult::message(lines.join("\n"))
        }
    }
}

fn expand_model_alias_for_provider(provider: ProviderKind, name: &str) -> String {
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    if matches!(provider, ProviderKind::XiaomiMimo) {
        return match lower.as_str() {
            "pro" | "mimo" => "mimo-v2.6-pro".to_string(),
            "ultraspeed" | "pro-ultraspeed" => "mimo-v2.5-pro-ultraspeed".to_string(),
            "text" | "omni" | "v2.5-omni" => "mimo-v2.6-flash".to_string(),
            "tts" | "speech" | "mimo-tts" => "mimo-v2.5-tts".to_string(),
            "voicedesign" | "voice-design" | "mimo-voice-design" => {
                "mimo-v2.5-tts-voicedesign".to_string()
            }
            "voiceclone" | "voice-clone" | "mimo-voice-clone" => {
                "mimo-v2.5-tts-voiceclone".to_string()
            }
            // Not a shorthand: keep the id as typed (case preserved for custom
            // token-plan model ids).
            _ => trimmed.to_string(),
        };
    }

    match lower.as_str() {
        "pro" | "v4-pro" => "deepseek-v4-pro".to_string(),
        "flash" | "v4-flash" => "deepseek-flash".to_string(),
        // Not a shorthand: keep the id as typed (case preserved for opaque
        // model tags on passthrough providers like Ollama/HuggingFace).
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::lock_test_env;
    use crate::tui::app::TuiOptions;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn create_test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = codewhale_localization::Locale::En;
        app.api_provider = codewhale_config::ProviderKind::Deepseek;
        app
    }

    #[test]
    fn no_args_opens_picker_modal() {
        let mut app = create_test_app();
        let result = provider(&mut app, None);
        assert!(result.message.is_none());
        assert_eq!(result.action, Some(AppAction::OpenProviderPicker));
    }

    #[test]
    fn retired_antigravity_selectors_return_the_tombstone_without_an_action() {
        let _guard = lock_test_env();
        for identity in ["antigravity", "agy", "AGY"] {
            let mut app = create_test_app();
            let result = provider(&mut app, Some(identity));
            assert!(result.is_error, "{identity}");
            assert_eq!(result.action, None, "{identity}");
            let message = result.message.expect("tombstone message");
            assert!(message.contains("non-runnable"), "{identity}: {message}");
            assert!(message.contains("GEMINI_API_KEY"), "{identity}: {message}");
            assert_eq!(app.api_provider, codewhale_config::ProviderKind::Deepseek);

            let setup = provider_setup_action_for_name(identity)
                .expect_err("setup must not open for the tombstone");
            assert!(setup.contains("provider `google`"), "{identity}: {setup}");
        }
    }

    #[test]
    fn setup_subcommand_opens_provider_setup_catalog() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("setup"));
        assert!(result.message.is_none());
        assert_eq!(
            result.action,
            Some(AppAction::OpenProviderSetup { provider: None })
        );
    }

    #[test]
    fn setup_subcommand_can_focus_provider() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("setup anthropic"));
        assert_eq!(
            result.action,
            Some(AppAction::OpenProviderSetup {
                provider: Some(ProviderKind::Anthropic.as_str().into()),
            })
        );
    }

    #[test]
    fn setup_subcommand_opens_ds4_preset() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("setup ds4"));
        assert_eq!(result.action, Some(AppAction::OpenDs4Setup));
        assert!(result.message.is_none());
    }

    #[test]
    fn setup_subcommand_rejects_retired_template_name() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("setup agnes"));
        assert!(result.action.is_none());
        let msg = result.message.expect("expected error message");
        assert!(msg.contains("Unknown provider 'agnes'"));
        assert!(result.is_error);
    }

    #[test]
    fn setup_subcommand_opens_first_class_zen_provider() {
        let mut app = create_test_app();
        for name in ["setup opencode-zen", "setup zen"] {
            let result = provider(&mut app, Some(name));
            assert_eq!(
                result.action,
                Some(AppAction::OpenProviderSetup {
                    provider: Some(ProviderKind::OpencodeZen.as_str().into()),
                }),
                "{name} must resolve through the provider registry"
            );
        }
    }

    #[test]
    fn setup_subcommand_rejects_unknown_provider() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("setup not-a-provider"));
        let msg = result.message.expect("expected error message");
        assert!(msg.contains("Unknown provider"));
        assert!(msg.contains("openrouter"));
        assert!(result.is_error);
    }

    #[test]
    fn unknown_provider_returns_error() {
        let mut app = create_test_app();
        // "anthropic" became a real provider in #3014; probe with an id that
        // stays unknown.
        let result = provider(&mut app, Some("not-a-provider"));
        let msg = result.message.expect("expected error message");
        assert!(msg.contains("Unknown provider"));
        assert!(msg.contains("openrouter"));
        assert!(msg.contains("xiaomi-mimo"));
        assert!(msg.contains("novita"));
        assert!(result.action.is_none());
    }

    #[test]
    fn switch_to_openrouter_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("openrouter"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Openrouter.as_str());
                assert_eq!(model, None);
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_xiaomi_mimo_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("xiaomi-mimo"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::XiaomiMimo.as_str());
                assert_eq!(model, None);
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_xiaomi_mimo_accepts_tts_shorthands() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("xiaomi-mimo tts"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::XiaomiMimo.as_str());
                assert_eq!(model.as_deref(), Some("mimo-v2.5-tts"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }

        let result = provider(&mut app, Some("xiaomi-mimo voiceclone"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::XiaomiMimo.as_str());
                assert_eq!(model.as_deref(), Some("mimo-v2.5-tts-voiceclone"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_xiaomi_mimo_accepts_chat_shorthands() {
        let mut app = create_test_app();
        for (input, expected) in [
            ("xiaomi-mimo pro-ultraspeed", "mimo-v2.5-pro-ultraspeed"),
            ("xiaomi-mimo ultraspeed", "mimo-v2.5-pro-ultraspeed"),
            ("xiaomi-mimo omni", "mimo-v2.6-flash"),
            ("xiaomi-mimo v2.5-omni", "mimo-v2.6-flash"),
        ] {
            let result = provider(&mut app, Some(input));
            match result.action {
                Some(AppAction::SwitchProvider { provider, model }) => {
                    assert_eq!(provider.as_str(), ProviderKind::XiaomiMimo.as_str());
                    assert_eq!(model.as_deref(), Some(expected));
                }
                other => panic!("expected SwitchProvider for {input}, got {other:?}"),
            }
        }
    }

    #[test]
    fn switch_to_atlascloud_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("atlascloud"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Atlascloud.as_str());
                assert_eq!(model, None);
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_wanjie_ark_preserves_model_id() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("ark-wanjie account-model-id"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::WanjieArk.as_str());
                assert_eq!(model.as_deref(), Some("account-model-id"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_openai_preserves_dashscope_model_id() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("openai qwen-plus"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Openai.as_str());
                assert_eq!(model.as_deref(), Some("qwen-plus"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_qianfan_preserves_model_id() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("qianfan custom-qianfan-service-id"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Qianfan.as_str());
                assert_eq!(model.as_deref(), Some("custom-qianfan-service-id"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn zhipu_aliases_fold_into_zai_and_canonicalize_glm() {
        // Zhipu AI and Z.ai are the same vendor: `zhipu`/`zhipuai` select the
        // single Zai provider and store the canonical GLM family id in Z.ai's own
        // casing (`glm-5.2` → `GLM-5.2`).
        let mut app = create_test_app();
        let result = provider(&mut app, Some("zhipu glm-5.2"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Zai.as_str());
                assert_eq!(model.as_deref(), Some("GLM-5.2"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }

        let result = provider(&mut app, Some("zhipuai glm-5-1"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Zai.as_str());
                assert_eq!(model.as_deref(), Some("GLM-5.1"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_novita_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("novita"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Novita.as_str());
                assert_eq!(model, None);
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_fireworks_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("fireworks pro"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Fireworks.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-v4-pro"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_siliconflow_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("siliconflow flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Siliconflow.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_siliconflow_cn_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("siliconflow-CN flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::SiliconflowCN.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_together_canonicalizes_deepseek_aliases() {
        // Together is symmetric with the other DeepSeek-hosting routes: the
        // canonical family id is stored and the route resolver performs the
        // wire-id translation (deepseek-v4-pro → Together's catalog slug) at
        // request time, rather than the command storing a wire slug.
        let mut app = create_test_app();
        let result = provider(&mut app, Some("together deepseek-v4-pro"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Together.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-v4-pro"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }

        let result = provider(&mut app, Some("together flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Together.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_sglang_flash_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("sglang flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Sglang.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_vllm_flash_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("vllm flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Vllm.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_ollama_preserves_model_tag() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("ollama qwen2.5-coder:7b"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Ollama.as_str());
                assert_eq!(model.as_deref(), Some("qwen2.5-coder:7b"));
            }
            other => panic!("expected SwitchProvider, got {other:?}"),
        }
    }

    #[test]
    fn switching_to_active_provider_without_model_is_a_noop() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("deepseek"));
        let msg = result.message.expect("expected message");
        assert!(msg.contains("Already on provider"));
        assert!(result.action.is_none());
    }

    #[test]
    fn switch_to_nim_emits_action_without_model_override() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("nvidia-nim"));
        assert!(result.message.is_none());
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::NvidiaNim.as_str());
                assert_eq!(model, None);
            }
            other => panic!("expected SwitchProvider action, got {other:?}"),
        }
    }

    #[test]
    fn nim_flash_shorthand_emits_action_with_model_override() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("nim flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::NvidiaNim.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider action, got {other:?}"),
        }
    }

    #[test]
    fn nim_pro_shorthand_emits_action_with_model_override() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("nim pro"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::NvidiaNim.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-v4-pro"));
            }
            other => panic!("expected SwitchProvider action, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_active_provider_with_new_model_still_emits_action() {
        let mut app = create_test_app();
        let result = provider(&mut app, Some("deepseek flash"));
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Deepseek.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-flash"));
            }
            other => panic!("expected SwitchProvider action, got {other:?}"),
        }
    }

    #[test]
    fn switch_to_deepseek_canonicalizes_provider_prefixed_model_override() {
        let mut app = create_test_app();
        app.api_provider = ProviderKind::Openrouter;

        let result = provider(&mut app, Some("deepseek deepseek/deepseek-v4-pro"));

        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::Deepseek.as_str());
                assert_eq!(model.as_deref(), Some("deepseek-v4-pro"));
            }
            other => panic!("expected SwitchProvider action, got {other:?}"),
        }
    }

    #[test]
    fn direct_deepseek_provider_commands_retire_aliases_at_official_wire_boundary() {
        let mut app = create_test_app();
        app.api_provider = ProviderKind::Openrouter;

        for provider_name in ["deepseek", "deepseek-cn", "deepseek-anthropic"] {
            for alias in ["deepseek-chat", "deepseek-reasoner"] {
                let result = provider(&mut app, Some(&format!("{provider_name} {alias}")));
                match result.action {
                    Some(AppAction::SwitchProvider { provider, model }) => {
                        assert_eq!(provider.as_str(), provider_name);
                        assert_eq!(model.as_deref(), Some(alias));
                        let row =
                            codewhale_config::descriptors::compatibility_for_id(provider.as_str())
                                .expect("command descriptor");
                        let official_base_url = row.base_url;
                        let intrinsic = row.kind;
                        assert_eq!(
                            codewhale_runtime::config::wire_model_for_provider_route(
                                intrinsic,
                                official_base_url,
                                model.as_deref().expect("command model"),
                            ),
                            codewhale_config::catalog::reviewed::constants::DEEPSEEK_ALIAS_REPLACEMENT
                        );
                        app.reasoning_effort =
                            codewhale_engine::reasoning_preference::ReasoningEffort::Max;
                        app.reasoning_effort_preference = None;
                        app.apply_provider_switch_reasoning_effort(
                            intrinsic,
                            official_base_url,
                            model.as_deref(),
                        );
                        assert_eq!(
                            app.reasoning_effort,
                            if alias == "deepseek-chat" {
                                codewhale_engine::reasoning_preference::ReasoningEffort::Off
                            } else {
                                codewhale_engine::reasoning_preference::ReasoningEffort::High
                            },
                            "{provider:?} {alias}"
                        );
                    }
                    other => panic!("expected SwitchProvider action, got {other:?}"),
                }
            }
        }

        let wanjie = provider(&mut app, Some("wanjie-ark deepseek-reasoner"));
        assert!(matches!(
            wanjie.action,
            Some(AppAction::SwitchProvider {
                provider: ref selected,
                model: Some(ref model),
            }) if selected.as_str() == "wanjie-ark" && model == "deepseek-reasoner"
        ));
    }

    #[test]
    fn provider_command_preserves_alias_owned_by_custom_deepseek_endpoint() {
        let mut app = create_test_app();
        app.model_ids_passthrough = true;

        let result = provider(&mut app, Some("deepseek deepseek-reasoner"));
        let Some(AppAction::SwitchProvider { provider, model }) = result.action else {
            panic!("expected SwitchProvider action");
        };
        let model = model.expect("command model");

        assert_eq!(provider.as_str(), ProviderKind::Deepseek.as_str());
        assert_eq!(model, "deepseek-reasoner");
        assert_eq!(
            codewhale_runtime::config::wire_model_for_provider_route(
                ProviderKind::Deepseek,
                "https://models.example/v1",
                &model,
            ),
            "deepseek-reasoner"
        );
        app.reasoning_effort = codewhale_engine::reasoning_preference::ReasoningEffort::Max;
        app.reasoning_effort_preference = None;
        app.apply_provider_switch_reasoning_effort(
            ProviderKind::Deepseek,
            "https://models.example/v1",
            Some(&model),
        );
        assert_eq!(
            app.reasoning_effort,
            codewhale_engine::reasoning_preference::ReasoningEffort::Max,
            "custom endpoint owns alias semantics"
        );

        app.reasoning_effort_preference =
            Some(codewhale_engine::reasoning_preference::ReasoningEffort::Max);
        app.apply_provider_switch_reasoning_effort(
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
            Some(&model),
        );
        assert_eq!(
            app.reasoning_effort,
            codewhale_engine::reasoning_preference::ReasoningEffort::Max,
            "explicit effort must beat compatibility inference"
        );
    }

    #[test]
    fn provider_fallback_status_and_reset_use_configured_chain() {
        let mut app = create_test_app();
        app.provider_chain = Some(codewhale_config::ProviderChain::new(
            codewhale_config::ProviderKind::Deepseek,
            &[codewhale_config::ProviderKind::Openrouter],
        ));

        let status = provider(&mut app, Some("fallback"));
        let message = status.message.expect("fallback status");
        assert!(message.contains("Current provider: deepseek"));
        assert!(message.contains("[0] deepseek (primary) <- current"));
        assert!(message.contains("[1] openrouter (fallback)"));

        let reset = provider(&mut app, Some("fallback reset"));
        assert!(reset.message.as_deref().unwrap_or("").contains("deepseek"));
        assert!(matches!(
            reset.action,
            Some(AppAction::SwitchProvider {
                provider: ref selected,
                model: None
            }) if selected.as_str() == "deepseek"
        ));
    }

    /// #2574: `/provider fallback reset` returns to the *primary* (chain entry
    /// 0), not to whatever fallback is currently active. The resolved
    /// `SwitchProvider` action is the canonical restore path — it re-seats
    /// `api_provider` and rebuilds the chain at position 0 (see
    /// `switch_provider`), so a bare `ProviderChain::reset()` is not needed here.
    #[test]
    fn provider_fallback_reset_targets_primary_even_when_on_fallback() {
        let _lock = lock_test_env();
        let mut providers = codewhale_runtime::config::ProvidersConfig::default();
        providers.openrouter.api_key = Some("fixture-fallback-key".to_string());
        let config = {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let __cw_config_init_0_1 = vec![ProviderKind::Openrouter];
            let __cw_config_init_0_2 = Some(providers);
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.fallback_providers = __cw_config_init_0_1;
            __cw_config_init_0_value.providers = __cw_config_init_0_2;
            __cw_config_init_0_value
        };
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(std::path::PathBuf::from(".")),
            &config,
        );
        app.api_provider = ProviderKind::Deepseek;
        app.provider_chain = Some(codewhale_config::ProviderChain::new(
            codewhale_config::ProviderKind::Deepseek,
            &[codewhale_config::ProviderKind::Openrouter],
        ));
        // Simulate having already fallen back to the secondary provider.
        // The secondary route is admitted from the captured fixture config.
        let advanced = app.advance_fallback("recoverable error");
        assert_eq!(advanced, Some(ProviderKind::Openrouter));
        assert_eq!(app.api_provider, ProviderKind::Openrouter);

        let reset = provider(&mut app, Some("fallback reset"));
        assert!(
            reset
                .message
                .as_deref()
                .unwrap_or("")
                .contains("primary provider: deepseek")
        );
        assert!(matches!(
            reset.action,
            Some(AppAction::SwitchProvider {
                provider: ref selected,
                model: None
            }) if selected.as_str() == "deepseek"
        ));
    }

    #[test]
    fn aggregator_passes_unrecognized_model_through() {
        // Equal treatment: a non-DeepSeek id on a DeepSeek-hosting aggregator is
        // not rejected — it passes through so the upstream API stays the
        // authority on what it can serve.
        let mut app = create_test_app();
        let result = provider(&mut app, Some("nim gpt-4"));
        assert!(result.message.is_none());
        match result.action {
            Some(AppAction::SwitchProvider { provider, model }) => {
                assert_eq!(provider.as_str(), ProviderKind::NvidiaNim.as_str());
                assert_eq!(model.as_deref(), Some("gpt-4"));
            }
            other => panic!("expected SwitchProvider action, got {other:?}"),
        }
    }
}
