#[cfg(test)]
use crate::bridge::invalidate_runtime_bridge;
use crate::bridge::{acquire_runtime_bridge, runtime_app_models};
use crate::http::ADVERTISED_ROUTES;
use crate::turns::handle_thread_request;
use crate::{AppState, AppTransport};
use anyhow::Context as _;
#[cfg(not(test))]
use anyhow::bail;
use anyhow::{Result, anyhow};
use codewhale_config::ConfigStore;
use codewhale_protocol::{AppRequest, AppResponse, ThreadListParams, ThreadRequest};
use serde_json::json;
use std::time::Duration;

pub(crate) async fn process_app_request(
    state: &AppState,
    req: AppRequest,
    _transport: AppTransport,
) -> AppResponse {
    match req {
        AppRequest::Capabilities => AppResponse {
            ok: true,
            data: json!({
                "routes": ADVERTISED_ROUTES,
                "interactive_execution": false,
                "goal_execution": false,
                "tool_free_turns": true,
                "full_api_entrypoint": "codewhale app-server --http",
                "config": ["get", "set", "unset", "list", "reload"],
                "events": ["response_start", "response_delta", "response_end"],
                "transport": "stdio+http",
                "config_path": state.config_path.as_ref().map(|p| p.display().to_string()),
            }),
            events: Vec::new(),
        },
        AppRequest::ConfigGet { key } => {
            let cfg = state.config.read().await;
            let value = cfg.get_display_value(&key);
            AppResponse {
                ok: true,
                data: json!({ "key": key, "value": value }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigSet { key, value } => {
            // Only propagate a mutation that actually happened. `set_value`
            // leaves the config untouched on an unknown key or invalid value,
            // so this is a no-op from the caller's point of view — but
            // `propagate_config` invalidates the cached stdio bridge
            // regardless, and dropping the last reference kills the running
            // child runtime along with its thread map. A single typo'd key
            // would orphan every in-flight thread on that bridge.
            let result = {
                let (key, value) = (key.clone(), value.clone());
                persist_config_mutation(state, move |cfg| cfg.set_value(&key, &value)).await
            };
            let ok = result.is_ok();
            let message = result.err().map(|e| e.to_string());
            let value = if ok {
                value
            } else {
                codewhale_config::persistence::redact_secrets(&value)
            };
            AppResponse {
                ok,
                data: json!({ "key": key, "value": value, "error": message }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigUnset { key } => {
            // See ConfigSet: a failed unset changed nothing and must not tear
            // down the runtime bridge.
            let result = {
                let key = key.clone();
                persist_config_mutation(state, move |cfg| cfg.unset_value(&key)).await
            };
            let ok = result.is_ok();
            let message = result.err().map(|e| e.to_string());
            AppResponse {
                ok,
                data: json!({ "key": key, "error": message }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigList => {
            let cfg = state.config.read().await;
            AppResponse {
                ok: true,
                data: json!({ "values": cfg.list_values() }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigReload => {
            // Re-read both `config.toml` and the sibling `permissions.toml`
            // from disk (the headless equivalent of the TUI
            // `reload_runtime_config` codepath) and push the fresh
            // snapshots into `state.config` and the live `Runtime`.
            //
            // `ConfigStore::load` resolves the same default config path
            // that `build_state` used at startup when `config_path` is
            // `None`, so a `None` here reloads from the same on-disk file
            // the server booted from.
            // Disk is already the source of truth here, so nothing to
            // persist. External `permissions.toml` edits reach the Engine
            // because the update invalidates the runtime bridge; the next
            // turn's child loads both files fresh.
            if let Err(error) = update_config_store(state, |_| Ok(())).await {
                return AppResponse {
                    ok: false,
                    data: json!({ "error": error.to_string() }),
                    events: Vec::new(),
                };
            }

            AppResponse {
                ok: true,
                data: json!({ "reloaded": true }),
                events: Vec::new(),
            }
        }
        AppRequest::Models => {
            let result = tokio::time::timeout(Duration::from_secs(30), runtime_app_models(state))
                .await
                .context("Runtime model catalog request timed out")
                .and_then(|result| result);
            match result {
                Ok(models) => AppResponse {
                    ok: true,
                    data: json!({ "models": models }),
                    events: Vec::new(),
                },
                Err(error) => AppResponse {
                    ok: false,
                    data: json!({ "error": error.to_string() }),
                    events: Vec::new(),
                },
            }
        }
        AppRequest::ThreadLoadedList => {
            let response = handle_thread_request(
                state,
                ThreadRequest::List(ThreadListParams {
                    include_archived: false,
                    limit: Some(50),
                }),
            )
            .await;
            match response {
                Ok(thread_resp) => AppResponse {
                    ok: true,
                    data: json!({ "threads": thread_resp.threads }),
                    events: thread_resp.events,
                },
                Err(err) => AppResponse {
                    ok: false,
                    data: json!({ "error": err.message }),
                    events: Vec::new(),
                },
            }
        }
        AppRequest::SubmitUserInput { request_id, .. } => {
            // This transport cannot deliver a clarification answer, and
            // saying otherwise was the bug: the previous implementation
            // reported `resolved: true` and filed the answers in a map with
            // no reader anywhere in this crate.
            //
            // It cannot be made to work here. `handle_line_during_turn`
            // executes exactly one method while a turn is streaming —
            // `thread/interrupt`. Everything else, `app/request` included,
            // queues until the turn ends, so an answer sent over this
            // transport would wait on the very turn that is waiting for it.
            // The runtime API owns the pending request and can resume the
            // turn, so that is where the reply belongs.
            AppResponse {
                ok: false,
                data: json!({
                    "error": "user_input_reply_unsupported",
                    "request_id": request_id,
                    "message": concat!(
                        "the app-server control transport cannot deliver clarification answers: ",
                        "only `thread/interrupt` runs while a turn is streaming, so an answer sent ",
                        "here would queue behind the turn waiting for it. Reply on the runtime API ",
                        "instead: POST /v1/user-input/{thread_id}/{request_id}."
                    ),
                }),
                events: Vec::new(),
            }
        }
    }
}

/// Install the saved config in the bookkeeping Runtime, then apply it through
/// the already captured canonical owner. Unbound production frontends refuse
/// application while retaining the saved config for explicit recovery.
/// A saved config whose owner reload fails is reported as retained but unapplied.
pub(crate) async fn propagate_config(state: &AppState) -> Result<()> {
    {
        let mut runtime = state.runtime.write().await;
        let snapshot = state.config.read().await.clone();
        runtime.update_config(snapshot);
    }
    if state.captured_owner.is_some() {
        let shared = acquire_runtime_bridge(state)
            .await
            .map_err(|_| anyhow!("config saved but captured Runtime owner is unavailable"))?;
        let request = {
            let bridge = shared.lock().await;
            bridge
                .authed(
                    bridge
                        .client
                        .post(format!("{}/v1/config/reload", bridge.base_url)),
                )
                .timeout(Duration::from_secs(10))
        };
        let response = request
            .send()
            .await
            .context("config saved; captured owner reload outcome is uncertain, not replayed")?;
        anyhow::ensure!(
            response.status().is_success(),
            "config saved but captured owner rejected reload (status {})",
            response.status()
        );
    } else {
        #[cfg(test)]
        invalidate_runtime_bridge(state).await;
        #[cfg(not(test))]
        bail!("config saved but no captured Runtime owner can apply it");
    }
    Ok(())
}

/// Prefix of a config error that is the server's fault (the file could not
/// be read, parsed, or written), reported over HTTP `/app` as a 500 rather
/// than the 400 a rejected key or value gets.
pub(crate) const CONFIG_LOAD_ERROR: &str = "failed to load config";

/// See [`CONFIG_LOAD_ERROR`].
pub(crate) const CONFIG_SAVE_ERROR: &str = "failed to save config";

/// Apply `mutate` to the config on disk, then propagate the saved result.
///
/// The mutation runs against a freshly loaded store, not the in-memory
/// snapshot, so edits another process (TUI, `codewhale login`) saved since
/// startup survive. With no explicit `--config` the store resolves the same
/// default path the runtime child reads, so the change reaches turns and
/// survives a restart. Any load, mutation, or save failure is returned and
/// nothing is propagated, so the caller never reports `ok` for a change
/// that was not kept.
///
/// The work runs on its own task: once the file is written, a caller that
/// goes away (an HTTP client disconnect) must not leave disk ahead of
/// `state.config`, the live runtime, and the cached bridge.
pub(crate) async fn persist_config_mutation(
    state: &AppState,
    mutate: impl FnOnce(&mut codewhale_config::ConfigToml) -> Result<()> + Send + 'static,
) -> Result<()> {
    update_config_store(state, move |store| {
        mutate(&mut store.config)?;
        store
            .save()
            .map_err(|err| anyhow!("{CONFIG_SAVE_ERROR}: {err}"))
    })
    .await
}

/// Serialize reloads and writes before reading disk, and finish propagation
/// even if the requesting connection disappears. A reload mutates nothing.
pub(crate) async fn update_config_store(
    state: &AppState,
    update: impl FnOnce(&mut ConfigStore) -> Result<()> + Send + 'static,
) -> Result<()> {
    let state = state.clone();
    tokio::spawn(async move {
        // Own the write guard across load→mutate→save→install so two
        // concurrent mutations cannot overwrite one another. All disk work
        // runs off-runtime; the owned operation still finishes propagation
        // when the requesting connection goes away.
        let mut config = state.config.clone().write_owned().await;
        let config_path = state.config_path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut store = ConfigStore::load(config_path)
                .map_err(|err| anyhow!("{CONFIG_LOAD_ERROR}: {err}"))?;
            update(&mut store)?;
            *config = store.config;
            Ok(())
        })
        .await
        .map_err(|err| anyhow!("config store task failed: {err}"))??;
        propagate_config(&state).await?;
        Ok(())
    })
    .await
    .map_err(|err| anyhow!("config update task failed: {err}"))?
}
