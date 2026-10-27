use std::sync::Arc;

use crate::cost_status::{
    CostScopeToken, EffectiveRouteEnvelope, EffectiveRouteUsage, RuntimeUsageDropRecord,
    RuntimeUsageMissingReason, RuntimeUsageRecord, acknowledge_retired_usage_source,
    record_interactive_runtime_usage, record_interactive_runtime_usage_drop,
    register_runtime_decision_sink, register_runtime_usage_sink_with_drop,
    report_interactive_decision,
};
use codewhale_models::Usage;

/// Install an interactive sink whose stale-scope fallback is an origin-session
/// sidecar. `/new` may close the foreground pool while a detached provider call
/// is still running; the sidecar keeps that exact response with the old saved
/// session instead of either dropping it or contaminating the new one.
pub(crate) fn register_persistent_interactive_runtime_usage_sink(
    owner: &str,
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
) {
    let Ok(manager) = crate::session_manager::SessionManager::default_location() else {
        // With no durable origin gate, leave the owner on the bounded journal
        // fallback. An in-memory-only sink could keep accepting a deleted
        // session's responses after its directory becomes available again.
        return;
    };
    register_persistent_interactive_runtime_usage_sink_at(
        owner,
        scope,
        session_id,
        turn_id,
        manager.sessions_dir().to_path_buf(),
    );
}

pub(crate) fn register_persistent_interactive_runtime_usage_sink_at(
    owner: &str,
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    sessions_dir: std::path::PathBuf,
) {
    let usage_session_id = session_id.to_string();
    let usage_turn_id = turn_id.to_string();
    let drop_session_id = usage_session_id.clone();
    let drop_turn_id = usage_turn_id.clone();
    let usage_sessions_dir = sessions_dir.clone();
    let decision_sessions_dir = sessions_dir.clone();
    let decision_session_id = session_id.to_string();
    let decision_turn_id = turn_id.to_string();
    register_runtime_usage_sink_with_drop(
        owner,
        Arc::new(move |record| {
            crate::session_manager::SessionManager::new(usage_sessions_dir.clone())
                .map(|manager| {
                    report_effective_route_for_interactive_origin_with_manager(
                        scope,
                        &usage_session_id,
                        &usage_turn_id,
                        &record.source_id,
                        &record.usage.route,
                        &record.usage.usage,
                        &manager,
                    )
                })
                .unwrap_or(false)
        }),
        Some(Arc::new(move |record| {
            crate::session_manager::SessionManager::new(sessions_dir.clone())
                .map(|manager| {
                    report_missing_usage_for_interactive_origin_with_manager(
                        scope,
                        &drop_session_id,
                        &drop_turn_id,
                        &record.source_id,
                        &record.route,
                        record.reason,
                        &manager,
                    )
                })
                .unwrap_or(false)
        })),
    );
    register_runtime_decision_sink(
        owner,
        Arc::new(move |receipt| {
            let Ok(manager) =
                crate::session_manager::SessionManager::new(decision_sessions_dir.clone())
            else {
                return false;
            };
            let accepted = manager
                .persist_late_decision_receipt(&decision_session_id, &decision_turn_id, &receipt)
                .unwrap_or(false);
            if accepted {
                // The append may have been handled by a deletion tombstone. Only
                // a live origin may enter the foreground diagnostic projection.
                let _ = manager.with_live_session_origin(&decision_session_id, || {
                    report_interactive_decision(scope, &receipt);
                    true
                });
            }
            accepted
        }),
    );
}

#[cfg(test)]
pub(crate) fn register_persistent_interactive_runtime_usage_sink_for_test(
    owner: &str,
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    manager: &crate::session_manager::SessionManager,
) {
    register_persistent_interactive_runtime_usage_sink_at(
        owner,
        scope,
        session_id,
        turn_id,
        manager.sessions_dir().to_path_buf(),
    );
}

/// Report an interactive auxiliary response against its immutable origin.
/// A stale foreground scope is not an error: it means `/new` or session load
/// already moved on, so the exact receipt is appended to the old session's
/// durable sidecar instead of being redirected to the active session.
pub(crate) fn report_effective_route_for_interactive_origin(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    usage: &Usage,
) {
    let persisted =
        crate::session_manager::SessionManager::default_location().is_ok_and(|manager| {
            report_effective_route_for_interactive_origin_with_manager(
                scope, session_id, turn_id, source_id, route, usage, &manager,
            )
        });
    if !persisted {
        tracing::warn!("late interactive usage could not be persisted for its origin session");
    }
}

pub(crate) fn report_effective_route_for_interactive_origin_with_manager(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    usage: &Usage,
    manager: &crate::session_manager::SessionManager,
) -> bool {
    let record = RuntimeUsageRecord {
        source_id: source_id.to_string(),
        usage: EffectiveRouteUsage {
            route: route.sanitized_for_persistence(),
            usage: usage.clone(),
        },
    };
    match manager.with_live_session_origin(session_id, || {
        record_interactive_runtime_usage(scope, record.clone())
    }) {
        Ok(None) => {
            acknowledge_retired_usage_source(scope, source_id);
            return true;
        }
        Ok(Some(true)) => return true,
        Ok(Some(false)) => {}
        Err(_) => return false,
    }
    manager
        .persist_late_runtime_usage(session_id, turn_id, &record)
        .unwrap_or(false)
}

pub(crate) fn report_unreceipted_for_interactive_origin(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
) {
    report_missing_usage_for_interactive_origin(
        scope,
        session_id,
        turn_id,
        source_id,
        route,
        RuntimeUsageMissingReason::SuccessWithoutUsage,
    );
}

pub(crate) fn report_missing_usage_for_interactive_origin(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    reason: RuntimeUsageMissingReason,
) {
    let persisted =
        crate::session_manager::SessionManager::default_location().is_ok_and(|manager| {
            report_missing_usage_for_interactive_origin_with_manager(
                scope, session_id, turn_id, source_id, route, reason, &manager,
            )
        });
    if !persisted {
        tracing::warn!(
            "late interactive missing-usage receipt could not be persisted for its origin session"
        );
    }
}

#[cfg(test)]
pub(crate) fn report_unreceipted_for_interactive_origin_with_manager(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    manager: &crate::session_manager::SessionManager,
) -> bool {
    report_missing_usage_for_interactive_origin_with_manager(
        scope,
        session_id,
        turn_id,
        source_id,
        route,
        RuntimeUsageMissingReason::SuccessWithoutUsage,
        manager,
    )
}

pub(crate) fn report_missing_usage_for_interactive_origin_with_manager(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    reason: RuntimeUsageMissingReason,
    manager: &crate::session_manager::SessionManager,
) -> bool {
    let record = RuntimeUsageDropRecord {
        reason,
        source_id: source_id.to_string(),
        route: route.sanitized_for_persistence(),
    };
    match manager.with_live_session_origin(session_id, || {
        record_interactive_runtime_usage_drop(scope, record.clone())
    }) {
        Ok(None) => {
            acknowledge_retired_usage_source(scope, source_id);
            return true;
        }
        Ok(Some(true)) => return true,
        Ok(Some(false)) => {}
        Err(_) => return false,
    }
    manager
        .persist_late_runtime_drop(session_id, turn_id, &record)
        .unwrap_or(false)
}
