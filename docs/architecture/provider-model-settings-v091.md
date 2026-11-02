# Provider, model, and settings contract for v0.9.1

This note records the live-code answers used for the v0.9.1 cutover. A provider
is a route/account boundary. A model is a provider-qualified choice. Provider
setup and adding a model are deliberately separate operations.

## Live state definitions

1. **Configured, enabled, current, saved, and default are distinct.** A provider
   is configured when `config::provider_is_configured` finds the active route,
   usable auth/external consent, or meaningful explicit provider configuration
   (`provider_is_configured` in `crates/tui/src/config.rs`; grep the symbol
   rather than trusting a line number). A used model is a
   `(provider identity, model id)` route in the recent-use index
   (`crates/engine/src/model_relevance.rs`, #6533); the
   current model is `App::{api_provider,model,auto_model}`; a saved
   provider-specific preference is `Settings::provider_models`; and the startup
   default is `Settings::default_provider` plus the provider-scoped preference
   (with `default_model` retained only as the DeepSeek compatibility fallback).
   Startup resolves these layers in `App::new` (`tui/app.rs:2964-3220`).

2. **Duplicate IDs remain provider-qualified.** `ModelPickerRow` carries both an
   `ApiProvider` and wire model ID. Cross-provider rows render as
   `Provider display name · model-id`, and apply events preserve the provider
   (`tui/model_picker.rs:203-213,1247-1255`). No bare model ID is treated as a
   globally unique owner.

3. **Non-catalog cases are conservative.** The active custom/unknown/local tag
   remains a selectable current row when the route accepts passthrough IDs.
   Retired aliases are normalized for display without losing the pre-apply
   value. `auto` is synthetic and is never recorded as a used route.
   Self-hosted/keyless means only that authentication is unnecessary; it does
   not imply reachability or health. Row selectability and explanations come
   from the route-specific readiness snapshot
   (`tui/model_picker.rs:434-459,934-1018`).

4. **Discovery is intentional; the default view ranks by use.** The ordinary
   `Configured` view shows, in order: the current route, pins and Fleet models,
   up to eight routes ranked by decayed recent use (sessions in the last 30
   days, one-week half-life, each route counted once per session), then one
   default per credentialed route (`assign_default_sections` in
   `tui/model_picker.rs`). `Catalog`, `Recent`, `Coding`, `Cheap`, and
   `Long context` are explicit discovery views; a typed query also searches the
   full lake. Applying a catalog row records that route as used this session,
   so it ranks in later ordinary opens without exposing the rest of the
   catalog; routes left unused age out of the default view.

5. **Cross-provider apply has a bounded effect.** Merely moving focus previews
   destination route facts and changes nothing. Enter validates the destination,
   switches only the current session route, saves that provider's model
   preference, and records the route in the recent-use index. It does not rewrite the global
   startup provider/model unless the separate save-as-default API is used
   (`tui/ui.rs:9495-9880`, `settings.rs:1461-1499`). Escape emits only picker
   browsing memory and does not mutate session or settings
   (`tui/model_picker.rs:1604-1611`).

6. **Existing configuration paths stay available.** The native `ConfigView`,
   `/config`, `/config <key>`, `/config <key> <value>`, `--save`, diagnostics,
   root/legacy config resolution, and CLI overrides remain consumers of the
   same `Config` and `Settings` structures (`commands/groups/config/config.rs`,
   `tui/views/mod.rs:1192-1770`). The modal is an additional typed editor, not a
   replacement storage format.

7. **First-run safety is narrower than education.** Trust/workspace scope,
   permission posture, external-credential consent, and any credential needed
   by the chosen route are runtime gates. Mode/Fleet/Workflow explanations,
   theme selection, and catalog browsing are optional education and must remain
   skippable. Onboarding cannot imply that a keyless route is healthy.

8. **Provider names appear only for provider facts.** Auth environment variables,
   endpoints/protocols, provider telemetry, external credential sources, and
   legacy compatibility name the exact provider. Generic cache, retry,
   permission, model-validation, and recovery copy uses the active provider or
   neutral wording.

9. **Readiness comes from one resolved snapshot.** UI labels use
   `provider_readiness::resolve_for_model`, which combines effective config,
   credential/consent state, live session health, protocol capability, and the
   selected model (`provider_readiness.rs`, `tui/model_picker.rs:971-1018`).
   `configured`, `ready`, `managed`, and `unavailable` are not synonyms.

10. **Old settings still load; use is derived, not stored.** `enabled_models`
    stays optional and serde-defaulted so old `settings.toml` files load
    unchanged, but nothing reads or writes it (#6533). Recent use is rebuilt at
    startup, off the UI thread, from saved session metadata (provider identity,
    model, `updated_at`) and each session's redacted `cost.route_receipts`; a
    committed in-session switch adds to it live. No new store is written.
    Wire spelling is preserved and model IDs are keyed case-insensitively.

## Persistence rule

The ordinary chooser is `auto`, the current route/model, pins and Fleet
models, recently used routes, user-declared `[[models]]` rows, and one default
per credentialed route (the saved provider preference or `[providers.X].model`
only when that route is current or recently used; otherwise its built-in
default). Provider configuration alone never imports that provider's catalog.
Catalog search remains available even when the ordinary set contains only one
model. Cancel never writes. Successful apply writes the smallest
provider-qualified state needed to make the user's choice repeatable.
