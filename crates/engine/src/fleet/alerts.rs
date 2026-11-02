//! Opt-in fleet alert routing and adapter payloads.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use codewhale_protocol::fleet::{FleetAlertEventClass, FleetRunId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_ALERT_TIMEOUT_SECONDS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetAlertConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub routes: Vec<FleetAlertRoute>,
    #[serde(default)]
    pub adapters: BTreeMap<String, FleetAlertAdapterConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetAlertRoute {
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<FleetAlertEventClass>,
    pub adapter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FleetAlertAdapterConfig {
    Slack {
        webhook_env: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    Webhook {
        url_env: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        secret_env: Option<String>,
    },
    PagerDuty {
        routing_key_env: String,
        #[serde(default = "default_pagerduty_severity")]
        severity: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetAlertEvent {
    pub class: FleetAlertEventClass,
    pub run_id: FleetRunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FleetAlertDelivery {
    pub adapter: String,
    pub event_class: FleetAlertEventClass,
    pub dry_run: bool,
    pub sent: bool,
    pub redacted_payload: Value,
}

pub trait FleetAlertSecretResolver {
    fn resolve(&self, name: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FleetEnvSecretResolver;

impl FleetAlertSecretResolver for FleetEnvSecretResolver {
    fn resolve(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
}

pub struct FleetAlertDispatcher<R = FleetEnvSecretResolver> {
    config: FleetAlertConfig,
    resolver: R,
}

impl FleetAlertConfig {
    pub fn dry_run_for_adapter(adapter: FleetAlertAdapterConfig) -> Self {
        let mut adapters = BTreeMap::new();
        adapters.insert("dry-run".to_string(), adapter);
        Self {
            enabled: true,
            dry_run: true,
            routes: vec![FleetAlertRoute {
                events: Vec::new(),
                adapter: "dry-run".to_string(),
            }],
            adapters,
        }
    }
}

impl<R> FleetAlertDispatcher<R>
where
    R: FleetAlertSecretResolver,
{
    pub fn new(config: FleetAlertConfig, resolver: R) -> Self {
        Self { config, resolver }
    }

    pub fn dispatch(&self, event: &FleetAlertEvent) -> Result<Vec<FleetAlertDelivery>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        let mut deliveries = Vec::new();
        for route in self
            .config
            .routes
            .iter()
            .filter(|route| route_matches(route, event.class))
        {
            let adapter = self.config.adapters.get(&route.adapter).ok_or_else(|| {
                anyhow!("Fleet alert adapter {} is not configured", route.adapter)
            })?;
            let prepared = prepare_alert(&route.adapter, adapter, event, self.config.dry_run)?;
            let sent = if self.config.dry_run {
                false
            } else {
                send_alert(adapter, &prepared.body, &self.resolver)?
            };
            deliveries.push(FleetAlertDelivery {
                adapter: route.adapter.clone(),
                event_class: event.class,
                dry_run: self.config.dry_run,
                sent,
                redacted_payload: prepared.redacted_payload,
            });
        }
        Ok(deliveries)
    }
}

impl FleetAlertEvent {
    pub fn inspection_commands(&self) -> Vec<String> {
        let mut commands = vec!["codewhale fleet status".to_string()];
        if let Some(worker_id) = &self.worker_id {
            commands.push(format!("codewhale fleet inspect {worker_id}"));
        }
        commands
    }
}

struct PreparedAlert {
    body: Value,
    redacted_payload: Value,
}

fn prepare_alert(
    adapter_name: &str,
    adapter: &FleetAlertAdapterConfig,
    event: &FleetAlertEvent,
    dry_run: bool,
) -> Result<PreparedAlert> {
    let safe_event = safe_event_payload(event);
    let prepared = match adapter {
        FleetAlertAdapterConfig::Slack {
            webhook_env,
            channel,
        } => {
            let body = slack_body(event, channel.as_deref());
            let redacted_payload = json!({
                "adapter": adapter_name,
                "kind": "slack",
                "dry_run": dry_run,
                "target": redacted_env(webhook_env),
                "event": safe_event,
                "body": body,
            });
            PreparedAlert {
                body,
                redacted_payload,
            }
        }
        FleetAlertAdapterConfig::Webhook {
            url_env,
            secret_env,
        } => {
            let body = json!({
                "source": "codewhale",
                "event": safe_event,
            });
            let redacted_payload = json!({
                "adapter": adapter_name,
                "kind": "webhook",
                "dry_run": dry_run,
                "target": redacted_env(url_env),
                "headers": redacted_secret_header(secret_env.as_deref()),
                "body": body,
            });
            PreparedAlert {
                body,
                redacted_payload,
            }
        }
        FleetAlertAdapterConfig::PagerDuty {
            routing_key_env,
            severity,
        } => {
            let body = pagerduty_body(event, severity, redacted_env(routing_key_env));
            let redacted_payload = json!({
                "adapter": adapter_name,
                "kind": "pagerduty",
                "dry_run": dry_run,
                "target": "https://events.pagerduty.com/v2/enqueue",
                "body": body,
            });
            PreparedAlert {
                body,
                redacted_payload,
            }
        }
    };
    Ok(prepared)
}

fn send_alert<R>(
    adapter: &FleetAlertAdapterConfig,
    redacted_body: &Value,
    resolver: &R,
) -> Result<bool>
where
    R: FleetAlertSecretResolver,
{
    let client = codewhale_release::tls::reqwest_blocking_client_builder()
        .timeout(Duration::from_secs(DEFAULT_ALERT_TIMEOUT_SECONDS))
        .build()
        .context("building Fleet alert HTTP client")?;
    match adapter {
        FleetAlertAdapterConfig::Slack { webhook_env, .. } => {
            let url = required_https_url(resolver, webhook_env)?;
            client
                .post(url)
                .json(redacted_body)
                .send()
                .context("sending Fleet Slack alert")?
                .error_for_status()
                .context("Slack alert rejected")?;
        }
        FleetAlertAdapterConfig::Webhook {
            url_env,
            secret_env,
        } => {
            let url = required_https_url(resolver, url_env)?;
            let mut request = client.post(url).json(redacted_body);
            if let Some(secret_env) = secret_env {
                request = request.header(
                    "X-CodeWhale-Webhook-Secret",
                    required_secret(resolver, secret_env)?,
                );
            }
            request
                .send()
                .context("sending Fleet webhook alert")?
                .error_for_status()
                .context("webhook alert rejected")?;
        }
        FleetAlertAdapterConfig::PagerDuty {
            routing_key_env,
            severity,
        } => {
            let routing_key = required_secret(resolver, routing_key_env)?;
            let mut body = redacted_body.clone();
            if let Some(map) = body.as_object_mut() {
                map.insert("routing_key".to_string(), Value::String(routing_key));
            }
            if let Some(payload) = body.get_mut("payload").and_then(Value::as_object_mut) {
                payload.insert("severity".to_string(), Value::String(severity.clone()));
            }
            client
                .post("https://events.pagerduty.com/v2/enqueue")
                .json(&body)
                .send()
                .context("sending Fleet PagerDuty alert")?
                .error_for_status()
                .context("PagerDuty alert rejected")?;
        }
    }
    Ok(true)
}

fn route_matches(route: &FleetAlertRoute, class: FleetAlertEventClass) -> bool {
    route.events.is_empty() || route.events.contains(&class)
}

fn safe_event_payload(event: &FleetAlertEvent) -> Value {
    json!({
        "class": event.class,
        "run_id": event.run_id.0.clone(),
        "worker_id": event.worker_id.clone(),
        "task_id": event.task_id.clone(),
        "status": event.status.clone(),
        "reason": short_reason(&event.reason),
        "commands": event.inspection_commands(),
    })
}

fn slack_body(event: &FleetAlertEvent, channel: Option<&str>) -> Value {
    let text = format!(
        "Codewhale Fleet {}: run={} task={} reason={}",
        alert_class_label(event.class),
        event.run_id.0,
        event.task_id.as_deref().unwrap_or("-"),
        short_reason(&event.reason)
    );
    let mut body = json!({
        "text": text,
        "blocks": [
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": text
                }
            },
            {
                "type": "context",
                "elements": [
                    {
                        "type": "mrkdwn",
                        "text": event.inspection_commands().join(" | ")
                    }
                ]
            }
        ]
    });
    if let Some(channel) = channel
        && let Some(map) = body.as_object_mut()
    {
        map.insert("channel".to_string(), Value::String(channel.to_string()));
    }
    body
}

fn pagerduty_body(event: &FleetAlertEvent, severity: &str, routing_key: String) -> Value {
    json!({
        "routing_key": routing_key,
        "event_action": "trigger",
        "payload": {
            "summary": format!("Codewhale Fleet {}: {}", alert_class_label(event.class), short_reason(&event.reason)),
            "severity": severity,
            "source": "codewhale",
            "custom_details": safe_event_payload(event),
        }
    })
}

fn redacted_env(name: &str) -> String {
    format!("<redacted:env:{name}>")
}

fn alert_class_label(class: FleetAlertEventClass) -> &'static str {
    match class {
        FleetAlertEventClass::Stale => "stale",
        FleetAlertEventClass::RestartExhausted => "restart_exhausted",
        FleetAlertEventClass::NeedsHuman => "needs_human",
        FleetAlertEventClass::BudgetExceeded => "budget_exceeded",
        FleetAlertEventClass::VerifierFailed => "verifier_failed",
        FleetAlertEventClass::RunCompleted => "run_completed",
    }
}

fn redacted_secret_header(secret_env: Option<&str>) -> Value {
    match secret_env {
        Some(name) => json!({ "X-CodeWhale-Webhook-Secret": redacted_env(name) }),
        None => json!({}),
    }
}

fn required_secret<R>(resolver: &R, name: &str) -> Result<String>
where
    R: FleetAlertSecretResolver,
{
    resolver
        .resolve(name)
        .ok_or_else(|| anyhow!("Fleet alert secret {name} is not configured"))
}

fn required_https_url<R>(resolver: &R, name: &str) -> Result<String>
where
    R: FleetAlertSecretResolver,
{
    let url = resolver
        .resolve(name)
        .ok_or_else(|| anyhow!("Fleet alert URL {name} is not configured"))?;
    validate_https_alert_url(name, &url)?;
    Ok(url)
}

fn validate_https_alert_url(name: &str, url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .with_context(|| format!("Fleet alert URL from {name} is not a valid URL"))?;
    if parsed.scheme() != "https" {
        return Err(anyhow!("Fleet alert URL from {name} must use https"));
    }
    Ok(())
}

fn short_reason(reason: &str) -> String {
    let trimmed = reason.trim();
    if trimmed.len() <= 240 {
        return trimmed.to_string();
    }
    let prefix: String = trimmed.chars().take(237).collect();
    format!("{prefix}...")
}

fn default_pagerduty_severity() -> String {
    "error".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_protocol::fleet::{
        FleetReceipt, FleetScore, FleetTaskFailureKind, FleetTaskResult, FleetWorkerEvent,
        FleetWorkerEventPayload,
    };

    impl FleetAlertEvent {
        fn stale_from_worker_event(event: &FleetWorkerEvent) -> Option<Self> {
            let FleetWorkerEventPayload::Stale { last_heartbeat_at } = &event.payload else {
                return None;
            };
            Some(Self {
                class: FleetAlertEventClass::Stale,
                run_id: event.run_id.clone(),
                worker_id: Some(event.worker_id.clone()),
                task_id: Some(event.task_id.clone()),
                status: "stale".to_string(),
                reason: last_heartbeat_at
                    .as_ref()
                    .map(|ts| format!("worker heartbeat stale since {ts}"))
                    .unwrap_or_else(|| "worker heartbeat is stale".to_string()),
            })
        }

        fn verifier_failed(receipt: &FleetReceipt) -> Option<Self> {
            if receipt.failure_kind != Some(FleetTaskFailureKind::Verifier) {
                return None;
            }
            Some(Self {
                class: FleetAlertEventClass::VerifierFailed,
                run_id: receipt.run_id.clone(),
                worker_id: Some(receipt.worker_id.clone()),
                task_id: Some(receipt.task_id.clone()),
                status: "verifier_failed".to_string(),
                reason: receipt
                    .score
                    .as_ref()
                    .and_then(|score| score.notes.clone())
                    .unwrap_or_else(|| "verifier failed".to_string()),
            })
        }
    }

    #[derive(Default)]
    struct MapResolver {
        values: BTreeMap<String, String>,
    }

    impl FleetAlertSecretResolver for MapResolver {
        fn resolve(&self, name: &str) -> Option<String> {
            self.values.get(name).cloned()
        }
    }

    fn event(class: FleetAlertEventClass) -> FleetAlertEvent {
        FleetAlertEvent {
            class,
            run_id: FleetRunId::from("run-1"),
            worker_id: Some("worker-1".to_string()),
            task_id: Some("task-a".to_string()),
            status: "stale".to_string(),
            reason: "worker heartbeat stale".to_string(),
        }
    }

    #[test]
    fn fleet_alert_disabled_by_default() {
        let dispatcher =
            FleetAlertDispatcher::new(FleetAlertConfig::default(), MapResolver::default());

        let deliveries = dispatcher
            .dispatch(&event(FleetAlertEventClass::Stale))
            .unwrap();

        assert!(deliveries.is_empty());
    }

    #[test]
    fn fleet_alert_policy_routes_event_classes_to_adapters() {
        let mut adapters = BTreeMap::new();
        adapters.insert(
            "ops-slack".to_string(),
            FleetAlertAdapterConfig::Slack {
                webhook_env: "FLEET_SLACK_WEBHOOK".to_string(),
                channel: Some("#fleet".to_string()),
            },
        );
        adapters.insert(
            "release-webhook".to_string(),
            FleetAlertAdapterConfig::Webhook {
                url_env: "FLEET_WEBHOOK_URL".to_string(),
                secret_env: None,
            },
        );
        let dispatcher = FleetAlertDispatcher::new(
            FleetAlertConfig {
                enabled: true,
                dry_run: true,
                routes: vec![
                    FleetAlertRoute {
                        events: vec![FleetAlertEventClass::Stale],
                        adapter: "ops-slack".to_string(),
                    },
                    FleetAlertRoute {
                        events: vec![FleetAlertEventClass::RunCompleted],
                        adapter: "release-webhook".to_string(),
                    },
                ],
                adapters,
            },
            MapResolver::default(),
        );

        let deliveries = dispatcher
            .dispatch(&event(FleetAlertEventClass::Stale))
            .unwrap();

        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].adapter, "ops-slack");
        assert_eq!(deliveries[0].event_class, FleetAlertEventClass::Stale);
        assert!(!deliveries[0].sent);
        assert_eq!(deliveries[0].redacted_payload["kind"], "slack");
    }

    #[test]
    fn fleet_alert_dry_run_redacts_secrets() {
        let mut adapters = BTreeMap::new();
        adapters.insert(
            "pager".to_string(),
            FleetAlertAdapterConfig::PagerDuty {
                routing_key_env: "FLEET_PD_ROUTING_KEY".to_string(),
                severity: "critical".to_string(),
            },
        );
        let mut resolver = MapResolver::default();
        resolver.values.insert(
            "FLEET_PD_ROUTING_KEY".to_string(),
            "real-routing-key-secret".to_string(),
        );
        let dispatcher = FleetAlertDispatcher::new(
            FleetAlertConfig {
                enabled: true,
                dry_run: true,
                routes: vec![FleetAlertRoute {
                    events: vec![FleetAlertEventClass::RestartExhausted],
                    adapter: "pager".to_string(),
                }],
                adapters,
            },
            resolver,
        );

        let deliveries = dispatcher
            .dispatch(&event(FleetAlertEventClass::RestartExhausted))
            .unwrap();
        let payload = serde_json::to_string(&deliveries[0].redacted_payload).unwrap();

        assert!(payload.contains("<redacted:env:FLEET_PD_ROUTING_KEY>"));
        assert!(!payload.contains("real-routing-key-secret"));
        assert!(payload.contains("codewhale fleet inspect worker-1"));
    }

    #[test]
    fn fleet_alert_url_validation_requires_https() {
        validate_https_alert_url("FLEET_WEBHOOK_URL", "https://hooks.example.invalid/fleet")
            .expect("https alert URL should be accepted");

        let err =
            validate_https_alert_url("FLEET_WEBHOOK_URL", "http://hooks.example.invalid/fleet")
                .expect_err("cleartext alert URL should fail");
        assert!(format!("{err:#}").contains("must use https"));
    }

    #[test]
    fn required_https_url_uses_secret_resolver() {
        let mut resolver = MapResolver::default();
        resolver.values.insert(
            "FLEET_WEBHOOK_URL".to_string(),
            "https://hooks.example.invalid/fleet".to_string(),
        );

        let url = required_https_url(&resolver, "FLEET_WEBHOOK_URL").expect("resolve URL");
        assert_eq!(url, "https://hooks.example.invalid/fleet");
    }

    #[test]
    fn fleet_alert_event_is_derived_from_ledgered_stale_worker_event() {
        let worker_event = FleetWorkerEvent {
            seq: 4,
            run_id: FleetRunId::from("run-1"),
            worker_id: "worker-1".to_string(),
            task_id: "task-a".to_string(),
            timestamp: "2026-06-13T02:00:00Z".to_string(),
            payload: FleetWorkerEventPayload::Stale {
                last_heartbeat_at: Some("2026-06-13T01:57:00Z".to_string()),
            },
            extra: BTreeMap::new(),
        };

        let alert = FleetAlertEvent::stale_from_worker_event(&worker_event).unwrap();

        assert_eq!(alert.class, FleetAlertEventClass::Stale);
        assert_eq!(alert.worker_id.as_deref(), Some("worker-1"));
        assert!(alert.reason.contains("2026-06-13T01:57:00Z"));
        assert_eq!(
            alert.inspection_commands(),
            vec![
                "codewhale fleet status".to_string(),
                "codewhale fleet inspect worker-1".to_string()
            ]
        );
    }

    #[test]
    fn fleet_alert_verifier_failed_event_is_derived_from_receipt() {
        let receipt = FleetReceipt {
            run_id: FleetRunId::from("run-1"),
            task_id: "task-a".to_string(),
            worker_id: "worker-1".to_string(),
            attempt: None,
            terminal_seq: None,
            completed_at: "2026-06-13T02:00:00Z".to_string(),
            result: FleetTaskResult::Fail,
            failure_kind: Some(FleetTaskFailureKind::Verifier),
            artifacts: vec![],
            score: Some(FleetScore {
                value: 0.0,
                max: Some(1.0),
                notes: Some("regex scorer could not be compiled".to_string()),
            }),
            resolved_route: None,
            saved_session_id: None,
            effective_permissions: None,
        };

        let alert = FleetAlertEvent::verifier_failed(&receipt).unwrap();

        assert_eq!(alert.class, FleetAlertEventClass::VerifierFailed);
        assert_eq!(alert.status, "verifier_failed");
        assert!(alert.reason.contains("regex scorer"));
    }
}
