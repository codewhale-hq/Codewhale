//! Native-client projection of the existing event, payload and notification
//! policy owners. This endpoint prepares an attempt; it never submits a banner.

use super::*;
use codewhale_engine::runtime_threads::{RuntimeEventRecord, RuntimeTurnStatus};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_notify::payload::NotificationPayload;
use codewhale_notify::{
    DeliveryOutcome, Method, NotificationGate, audio as notification_audio, sound_policy,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareRequest {
    seq: u64,
    focused: bool,
    unfocused_for_ms: u64,
    locale: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PreparedNotification {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    headline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    /// A selection, never an audio receipt. No file paths cross this boundary.
    sound: &'static str,
}

impl PreparedNotification {
    fn suppressed(status: &'static str) -> Self {
        Self {
            status,
            headline: None,
            body: None,
            sound: "off",
        }
    }
}

pub(super) async fn prepare(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(request): Json<PrepareRequest>,
) -> Result<Json<PreparedNotification>, ApiError> {
    let Some(previous) = request.seq.checked_sub(1) else {
        return Err(ApiError::bad_request(
            "notification sequence must be positive",
        ));
    };
    // Read the selected thread's durable event, never a renderer-supplied copy
    // or title. Dropping the replay receiver stops the bounded reader.
    let mut replay = state
        .runtime_threads
        .replay_events(&id, Some(previous), None)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut selected = None;
    while let Some(batch) = replay.batches.recv().await {
        let batch = batch.map_err(ApiError::internal)?;
        if let Some(event) = batch.into_iter().next() {
            if event.seq == request.seq {
                selected = Some(event);
            }
            break;
        }
    }
    let event = selected.ok_or_else(|| {
        ApiError::bad_request("notification event does not belong to this thread")
    })?;
    let locale = Locale::shipped()
        .iter()
        .copied()
        .find(|locale| locale.tag().eq_ignore_ascii_case(&request.locale))
        .or_else(|| matches!(request.locale.as_str(), "zh" | "zh-CN").then_some(Locale::ZhHans))
        .ok_or_else(|| ApiError::bad_request("unsupported notification locale"))?;
    // Replay can wait on disk while the same turn's request settles. Validate
    // the selected record against a fresh snapshot, with no later await.
    let detail = state
        .runtime_threads
        .get_thread_detail(&id)
        .await
        .map_err(map_thread_err)?;
    let config = state.config.read().clone();
    Ok(Json(prepare_record(
        &config,
        &detail,
        &event,
        request.focused,
        Duration::from_millis(request.unfocused_for_ms),
        locale,
        Utc::now(),
    )))
}

#[allow(clippy::too_many_arguments)] // Canonical snapshot plus observed host facts; no second policy object.
fn prepare_record(
    config: &Config,
    detail: &ThreadDetail,
    event: &RuntimeEventRecord,
    focused: bool,
    unfocused_for: Duration,
    locale: Locale,
    now: chrono::DateTime<Utc>,
) -> PreparedNotification {
    // Old/recovered records remain visible in the work history but cannot
    // become a fresh OS interruption. The host also anchors its replay cursor.
    let age = now.signed_duration_since(event.timestamp).num_seconds();
    if !(0..=60).contains(&age) || event.payload.get("recovered") == Some(&json!(true)) {
        return PreparedNotification::suppressed("expired");
    }
    let Some(turn) = detail
        .turns
        .iter()
        .find(|turn| Some(&turn.id) == event.turn_id.as_ref())
    else {
        return PreparedNotification::suppressed("settled");
    };
    if detail.thread.latest_turn_id.as_deref() != Some(turn.id.as_str()) {
        return PreparedNotification::suppressed("settled");
    }
    if matches!(
        event.event.as_str(),
        "approval.required" | "user_input.required"
    ) && !matches!(
        turn.status,
        RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
    ) {
        return PreparedNotification::suppressed("settled");
    }
    let payload = match event.event.as_str() {
        "turn.completed" if turn.status == RuntimeTurnStatus::Completed => {
            NotificationPayload::turn_complete(&tr(locale, MessageId::NotificationTurnComplete))
        }
        "approval.required" => {
            let Some(pending) = detail.pending_approvals.iter().find(|pending| {
                event.payload.get("id").and_then(Value::as_str) == Some(pending.id.as_str())
                    && pending.turn_id == turn.id
            }) else {
                return PreparedNotification::suppressed("settled");
            };
            NotificationPayload::approval_needed(
                &tr(locale, MessageId::ConfigLabelNotificationApprovalNeeded),
                &pending.tool_name,
            )
        }
        "user_input.required"
            if detail.pending_user_inputs.iter().any(|pending| {
                event.payload.get("id").and_then(Value::as_str) == Some(pending.id.as_str())
                    && pending.turn_id == turn.id
            }) =>
        {
            NotificationPayload::input_needed(&tr(
                locale,
                MessageId::ConfigLabelNotificationInputNeeded,
            ))
        }
        _ => return PreparedNotification::suppressed("unsupported_event"),
    };
    prepare_payload(
        config,
        &payload,
        Duration::from_millis(turn.duration_ms.unwrap_or(0)),
        focused,
        unfocused_for,
    )
}

fn prepare_payload(
    config: &Config,
    payload: &NotificationPayload,
    elapsed: Duration,
    focused: bool,
    unfocused_for: Duration,
) -> PreparedNotification {
    let notification_config = config.notifications_config();
    let Some((method, threshold, _)) = codewhale_notify::settings_projection(&notification_config)
    else {
        return PreparedNotification::suppressed("suppressed");
    };
    let method = match method {
        Method::Auto => Method::MacOS,
        Method::Off => Method::Off,
        _ => return PreparedNotification::suppressed("unsupported_method"),
    };
    let attention =
        codewhale_notify::native_attention_allowed(&notification_config, focused, unfocused_for);
    let threshold = if payload.kind() == codewhale_notify::payload::NotificationKind::TurnComplete {
        threshold
    } else {
        Duration::ZERO
    };
    let mut sound = "off";
    // Capture the shared policy's intended sinks. Neither closure performs IO;
    // the response says prepared, never dispatched/delivered. The native host
    // owns the subsequent permission check and submission receipt.
    let outcome = codewhale_notify::delivery::notify_with_sinks(
        method,
        false,
        payload,
        threshold,
        elapsed,
        NotificationGate::from_config(&notification_config),
        attention,
        &mut std::io::sink(),
        &mut |kind, bell| {
            sound_policy::decide_configured(
                &notification_config,
                kind,
                sound_policy::epoch_millis_now(),
                bell,
            )
        },
        &mut |cue, _| {
            sound = match cue {
                sound_policy::SoundCue::Beep => "beep",
                sound_policy::SoundCue::Whale => "whale",
                sound_policy::SoundCue::File(_) => "file",
                sound_policy::SoundCue::Bell => "bell",
                sound_policy::SoundCue::DoubleBell => "double-bell",
            };
            notification_audio::AudioOutcome::Dispatched
        },
        &mut |_| DeliveryOutcome::Dispatched(Method::MacOS),
    );
    if !matches!(outcome, DeliveryOutcome::Dispatched(_)) {
        return PreparedNotification::suppressed("suppressed");
    }
    PreparedNotification {
        status: "prepared",
        headline: Some(payload.headline().to_string()),
        body: Some(payload.body()),
        sound,
    }
}
