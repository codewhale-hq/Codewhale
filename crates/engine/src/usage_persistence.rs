use crate::cost_status::{
    CostScopeToken, EffectiveRouteEnvelope, EffectiveRouteUsage, RuntimeUsageDropRecord,
    RuntimeUsageMissingReason, RuntimeUsageRecord, mark_retired_usage_source,
    record_interactive_runtime_usage, record_interactive_runtime_usage_drop,
};
use codewhale_models::Usage;

pub(crate) fn report_runtime_usage_record_for_origin_with_manager(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    record: &RuntimeUsageRecord,
    manager: &crate::session_manager::SessionManager,
) -> std::io::Result<()> {
    let mut publication = Ok(());
    let admitted = manager.with_live_session_origin(session_id, || {
        publication = manager
            .persist_runtime_usage_record_admitted(session_id, turn_id, record)
            .map(|_| ());
        if publication.is_err() {
            return false;
        }
        let _ = record_interactive_runtime_usage(scope, record.clone());
        true
    })?;
    publication?;
    if admitted.is_none() {
        mark_retired_usage_source(scope, &record.source_id);
    }
    Ok(())
}

pub fn report_effective_route_for_interactive_origin_with_manager(
    scope: CostScopeToken,
    session_id: &str,
    turn_id: &str,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    usage: &Usage,
    manager: &crate::session_manager::SessionManager,
) -> bool {
    let record = RuntimeUsageRecord::legacy(
        source_id.to_string(),
        EffectiveRouteUsage {
            route: route.sanitized_for_persistence(),
            usage: usage.clone(),
        },
    );
    let mut durability = Ok(true);
    match manager.with_live_session_origin(session_id, || {
        durability = manager.persist_legacy_usage_admitted(session_id, turn_id, &record);
        durability.as_ref().is_ok_and(|handled| *handled)
            && record_interactive_runtime_usage(scope, record.clone())
    }) {
        Ok(None) => {
            mark_retired_usage_source(scope, source_id);
            return true;
        }
        Ok(Some(true)) => return true,
        Ok(Some(false)) => {
            if durability.is_err() {
                return false;
            }
        }
        Err(_) => return false,
    }
    manager
        .persist_late_runtime_usage(session_id, turn_id, &record)
        .unwrap_or(false)
}

pub fn report_unreceipted_for_interactive_origin(
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

pub fn report_missing_usage_for_interactive_origin(
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

#[cfg(any(test, feature = "test-support"))]
pub fn report_unreceipted_for_interactive_origin_with_manager(
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

pub fn report_missing_usage_for_interactive_origin_with_manager(
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
            mark_retired_usage_source(scope, source_id);
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
