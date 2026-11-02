//! Pure cache inspection, history and status formatting for `/cache`.
//! Route resolution, pricing classes and elapsed observation stay host-owned.

use crate::diagnostics_reports::format_cost_amount_precise;
use codewhale_command_contract::facets::{
    CommandPresentationContext, DebugCacheTelemetry, DebugCacheTurn, DebugCostProjection,
    DebugPromptInspection, DebugWarmupKey,
};
use std::collections::{BTreeMap, BTreeSet};

pub fn format_warmup_status(
    last_warmup: Option<&DebugWarmupKey>,
    current: &DebugWarmupKey,
    last_hash_short: Option<&str>,
    current_hash_short: &str,
) -> String {
    match last_warmup {
        None => format!(
            "Warmup status: no previous warmup (current key: {})\n",
            current_hash_short
        ),
        Some(previous) if previous == current => {
            format!(
                "Warmup status: valid (key {} matches)\n",
                current_hash_short
            )
        }
        Some(previous) => {
            let mut reasons = Vec::new();
            if previous.provider != current.provider {
                reasons.push("provider changed");
            }
            if previous.model != current.model {
                reasons.push("model changed");
            }
            if previous.base_url != current.base_url {
                reasons.push("base URL changed");
            }
            if previous.static_prefix_hash != current.static_prefix_hash {
                reasons.push("static prefix changed");
            }
            if previous.tool_catalog_hash != current.tool_catalog_hash {
                reasons.push("tool catalog changed");
            }
            if previous.project_pack_hash != current.project_pack_hash {
                reasons.push("project pack changed");
            }
            if previous.skills_hash != current.skills_hash {
                reasons.push("skills changed");
            }
            let reason_text = if reasons.is_empty() {
                "unknown prefix input changed".to_string()
            } else {
                reasons.join(", ")
            };
            format!(
                "Warmup status: invalid ({} -> {}; {})\n",
                last_hash_short.unwrap_or(""),
                current_hash_short,
                reason_text
            )
        }
    }
}

pub(super) fn format_verbose_diff(
    previous: &DebugPromptInspection,
    current: &DebugPromptInspection,
) -> String {
    let mut out = String::new();
    let max_len = previous.layers.len().max(current.layers.len());
    for index in 0..max_len {
        match (previous.layers.get(index), current.layers.get(index)) {
            (Some(prev), Some(curr)) if prev == curr => {
                out.push_str(&format!("  [{index}] {} unchanged\n", curr.name));
            }
            (Some(prev), Some(curr)) => {
                out.push_str(&format!("  [{index}] {} changed\n", curr.name));
                if prev.name != curr.name {
                    out.push_str(&format!("    name: {} -> {}\n", prev.name, curr.name));
                }
                if prev.stability != curr.stability {
                    out.push_str(&format!(
                        "    stability: {} -> {}\n",
                        prev.stability.label(),
                        curr.stability.label()
                    ));
                }
                if prev.char_len != curr.char_len {
                    out.push_str(&format!(
                        "    chars: {} -> {} ({:+})\n",
                        prev.char_len,
                        curr.char_len,
                        curr.char_len as i64 - prev.char_len as i64
                    ));
                }
                if prev.sha256 != curr.sha256 {
                    out.push_str(&format!(
                        "    hash: {} -> {}\n",
                        short_hash(&prev.sha256),
                        short_hash(&curr.sha256)
                    ));
                }
            }
            (None, Some(curr)) => {
                out.push_str(&format!("  [{index}] {} added\n", curr.name));
            }
            (Some(prev), None) => {
                out.push_str(&format!("  [{index}] {} removed\n", prev.name));
            }
            (None, None) => unreachable!("index is within max_len"),
        }
    }
    out
}

fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

/// Render a prefix-cache stability and health summary for `/cache stats`.
///
/// Surfaces the current prefix fingerprint, stability ratio, change history,
/// and an aggregated cache-hit summary from per-turn telemetry.  When the
/// prefix has changed, a prominent warning is included so users can
/// correlate cache misses with prefix drift.
pub(super) fn format_cache_stats(telemetry: &DebugCacheTelemetry) -> String {
    let mut out = String::new();
    out.push_str("Cache Stats\n");

    // ── Prefix stability ──────────────────────────────────────────────
    out.push_str("\n── Prefix Stability\n");
    match telemetry.prefix_stability_pct {
        Some(pct) => {
            let checks = telemetry.prefix_checks_total;
            let changes = telemetry.prefix_change_count;
            let stable_checks = checks.saturating_sub(changes);

            let drift = telemetry.prefix_drift_count;
            if changes == 0 {
                out.push_str(&format!(
                    "  Stability: {pct}% ({stable_checks}/{checks} checks)\n"
                ));
                out.push_str("  Status:    stable (no prefix changes this session)\n");
                if telemetry.prefix_context_updates > 0 {
                    out.push_str(&format!(
                        "  Context updates: {} (workspace drift delivered as history, header unchanged)\n",
                        telemetry.prefix_context_updates
                    ));
                }
            } else {
                out.push_str(&format!(
                    "  Stability: {pct}% ({stable_checks}/{checks} checks, {changes} change{})\n",
                    if changes == 1 { "" } else { "s" }
                ));
                if drift == 0 {
                    out.push_str(
                        "  Status:    stable (all changes were declared header changes)\n",
                    );
                } else {
                    out.push_str(&format!(
                        "  Status:    WARNING — {drift} undeclared drift{}\n",
                        if drift == 1 { "" } else { "s" }
                    ));
                }
                if let Some(ref reason) = telemetry.prefix_pin_reason {
                    out.push_str(&format!("  Pin reason: {reason}\n"));
                }
                if telemetry.prefix_context_updates > 0 {
                    out.push_str(&format!(
                        "  Context updates: {} (workspace drift delivered as history, header unchanged)\n",
                        telemetry.prefix_context_updates
                    ));
                }
                if let Some(ref reason) = telemetry.prefix_last_miss_reason {
                    out.push_str(&format!("  Last miss:  {reason}\n"));
                }
                if let Some(ref desc) = telemetry.last_prefix_change_desc {
                    out.push_str(&format!("  Last change: {desc}\n"));
                }
            }
        }
        None => {
            out.push_str("  Stability: unknown (no checks recorded yet)\n");
            out.push_str("  Run a turn first to collect prefix stability data.\n");
        }
    }

    // ── Prefix fingerprint ────────────────────────────────────────────
    out.push_str("\n── Prefix Fingerprint\n");
    match &telemetry.last_pinned_prefix_hash {
        Some(hash) => {
            out.push_str(&format!("  Pinned hash: {hash}\n"));
            let short = if hash.len() >= 12 { &hash[..12] } else { hash };
            out.push_str(&format!("  Short id:    {short}\n"));
            if telemetry.prefix_drift_count > 0 {
                out.push_str("  Drift:       WARNING — undeclared hash change this session\n");
                out.push_str(&format!(
                    "               ({change} change{plural} detected, {drift} undeclared)\n",
                    change = telemetry.prefix_change_count,
                    plural = if telemetry.prefix_change_count == 1 {
                        ""
                    } else {
                        "s"
                    },
                    drift = telemetry.prefix_drift_count,
                ));
            } else if telemetry.prefix_change_count > 0 {
                out.push_str("  Drift:       none (all changes were declared)\n");
                out.push_str(&format!(
                    "               ({change} change{plural} detected)\n",
                    change = telemetry.prefix_change_count,
                    plural = if telemetry.prefix_change_count == 1 {
                        ""
                    } else {
                        "s"
                    },
                ));
            } else {
                out.push_str("  Drift:       none (hash stable)\n");
            }
        }
        None => {
            out.push_str("  Pinned hash: unavailable\n");
            out.push_str("  Run a turn first, or use /cache inspect.\n");
        }
    }

    // ── Cache hit-rate summary ────────────────────────────────────────
    out.push_str("\n── Cache Hit Rate\n");
    let history = &telemetry.history;
    if history.is_empty() {
        out.push_str("  No turn telemetry recorded yet.\n");
    } else {
        // Aggregate only cache-aware turns; skip turns where the provider
        // did not report cache telemetry (cache_hit_tokens is None).
        // When cache_miss_tokens is None, infer it as
        //   input_tokens − cache_hit_tokens  (matches /cache table logic).
        let mut turns = 0u64;
        let (hit, miss, input) =
            telemetry
                .history
                .iter()
                .fold((0u64, 0u64, 0u64), |(hit, miss, input), rec| {
                    let Some(hit_tokens) = rec.cache_hit_tokens else {
                        return (hit, miss, input);
                    };
                    let h = u64::from(hit_tokens);
                    let m = u64::from(
                        rec.cache_miss_tokens
                            .unwrap_or(rec.input_tokens.saturating_sub(hit_tokens)),
                    );
                    turns += 1;
                    (hit + h, miss + m, input + u64::from(rec.input_tokens))
                });
        let total_cache = hit + miss;
        let avg_pct = if total_cache > 0 {
            (hit as f64 / total_cache as f64 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        out.push_str(&format!("  Turns recorded: {turns}\n"));
        out.push_str(&format!(
            "  Cache hit tokens:  {hit} ({avg_pct:.1}% of {total_cache} cache-aware tokens)\n",
            hit = format_tokens(hit),
            total_cache = format_tokens(total_cache),
        ));
        out.push_str(&format!(
            "  Cache miss tokens: {miss}\n",
            miss = format_tokens(miss),
        ));
        out.push_str(&format!(
            "  Total input tokens: {input}\n",
            input = format_tokens(input),
        ));
        if avg_pct < 80.0 {
            out.push_str("  NOTE: cache hit rate is low (< 80%). Check prefix stability above or consider /compact.\n");
        }
    }

    out
}

/// Render three-zone prefix contract status for `/cache zones` (#2264).
///
/// Displays the PinnedPrefix fingerprint, AppendLog size, and TurnScratch
/// state. PinnedPrefix is frozen and checked for drift each turn, and
/// AppendLog is the backing store for the engine's session history
/// (`core::session::Session::messages`). TurnScratch is still type
/// scaffolding: nothing on the request path populates it.
pub(super) fn format_cache_zones(telemetry: &DebugCacheTelemetry) -> String {
    let mut out = String::new();
    out.push_str("Cache Zones (#2264 three-zone contract)\n");

    // ── PinnedPrefix ─────────────────────────────────────────────────
    out.push_str("\n── PinnedPrefix (system + tools, frozen baseline)\n");
    match &telemetry.last_pinned_prefix_hash {
        Some(hash) => {
            let short = if hash.len() >= 12 { &hash[..12] } else { hash };
            out.push_str(&format!("  Short id: {short}\n"));
            if telemetry.prefix_change_count > 0 {
                out.push_str(&format!(
                    "  Status:    WARNING — {change} drift{plural} detected\n",
                    change = telemetry.prefix_change_count,
                    plural = if telemetry.prefix_change_count == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
            } else {
                out.push_str("  Status:    stable (no drift this session)\n");
            }
            if let Some(pct) = telemetry.prefix_stability_pct {
                out.push_str(&format!("  Stability: {pct}%\n"));
            }
        }
        None => {
            out.push_str("  Status:    unavailable (not yet frozen)\n");
            out.push_str("  Run a turn first to freeze the baseline.\n");
        }
    }

    // ── AppendLog ────────────────────────────────────────────────────
    out.push_str("\n── AppendLog (conversation history, append-only)\n");
    out.push_str("  Status:      wired — backs the engine session history\n");
    let msg_count = telemetry.api_message_count;
    out.push_str(&format!("  Messages:    {msg_count}\n"));
    let history_count = telemetry.non_system_message_count;
    out.push_str(&format!("  History msgs: {history_count}\n"));

    // ── TurnScratch ──────────────────────────────────────────────────
    out.push_str("\n── TurnScratch (per-turn ephemeral data)\n");
    out.push_str("  Status:      not wired — type scaffolding, unused by requests\n");

    // ── Zone contract summary ────────────────────────────────────────
    out.push_str("\n── Contract Status\n");
    let has_drift = telemetry.prefix_change_count > 0;
    out.push_str(&format!(
        "  PinnedPrefix: {}\n",
        if telemetry.last_pinned_prefix_hash.is_some() {
            if has_drift {
                "WARNING — drifted"
            } else {
                "OK"
            }
        } else {
            "not frozen"
        }
    ));
    out.push_str("  AppendLog:    wired (session history)\n");
    out.push_str("  TurnScratch:  not wired\n");

    out
}

/// Formats a u64 token count with a compact suffix: K for thousands,
/// M for millions. Never returns scientific notation.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub(super) fn format_static_prefix_status(
    previous: Option<&DebugPromptInspection>,
    current: &DebugPromptInspection,
) -> String {
    let Some(previous) = previous else {
        return "Static base prefix stability: no previous request\n".to_string();
    };
    if previous.base_static_prefix_hash == current.base_static_prefix_hash {
        return "Static base prefix stability: OK\n".to_string();
    }

    let changed = changed_static_layers(previous, current);
    if changed.is_empty() {
        "Static base prefix stability: WARNING (base hash changed)\n".to_string()
    } else {
        format!(
            "Static base prefix stability: WARNING changed layers: {}\n",
            changed.join(", ")
        )
    }
}

pub(super) fn format_first_divergence(
    previous: Option<&DebugPromptInspection>,
    current: &DebugPromptInspection,
) -> String {
    let Some(previous) = previous else {
        return "First divergence from previous request: unavailable\n".to_string();
    };
    let max_len = previous.layers.len().max(current.layers.len());
    for index in 0..max_len {
        match (previous.layers.get(index), current.layers.get(index)) {
            (Some(prev), Some(curr)) if prev.name == curr.name && prev.sha256 == curr.sha256 => {}
            (Some(prev), Some(curr)) if prev.name == curr.name => {
                return format!("First divergence from previous request: {}\n", curr.name);
            }
            (Some(_), Some(curr)) => {
                return format!("First divergence from previous request: {}\n", curr.name);
            }
            (None, Some(curr)) => {
                return format!("First divergence from previous request: {}\n", curr.name);
            }
            (Some(prev), None) => {
                return format!(
                    "First divergence from previous request: {} removed\n",
                    prev.name
                );
            }
            (None, None) => break,
        }
    }
    "First divergence from previous request: none\n".to_string()
}

fn changed_static_layers(
    previous: &DebugPromptInspection,
    current: &DebugPromptInspection,
) -> Vec<String> {
    current
        .layers
        .iter()
        .filter(|layer| layer.stability.label() == "static")
        .filter(|layer| {
            previous
                .layers
                .iter()
                .find(|previous_layer| previous_layer.name == layer.name)
                .is_none_or(|previous_layer| previous_layer.sha256 != layer.sha256)
        })
        .map(|layer| layer.name.clone())
        .collect()
}

/// Column header for the per-turn cache/cost table. The widths here must match
/// the row format strings below.
const TURN_CACHE_ROW_HEADER: &str = "turn  route                        in    out    hit   miss  write  replay    ratio        cost   age";

/// Rule width for the table. Sized to the header above.
const TURN_CACHE_TABLE_WIDTH: usize = 106;

/// Render one turn's cost cell, collecting the reason when it has none.
///
/// A turn with no route provenance (legacy or synthetic record) and a turn on a
/// route that is not money-metered both render as `—` — neither is a real
/// zero-dollar charge.
fn turn_cost_cell(
    rec: &DebugCacheTurn,
    cost: &DebugCostProjection,
    unpriced_reasons: &mut BTreeMap<u8, String>,
    unpriced_classes: &mut BTreeSet<String>,
) -> String {
    if let Some(amount) = rec.priced_amount {
        return format_cost_amount_precise(amount, cost.currency);
    }
    if let (Some(reason), Some(rank)) = (&rec.unpriced_reason_key, rec.unpriced_reason_sort_rank) {
        unpriced_reasons.insert(rank, reason.clone());
    }
    unpriced_classes.extend(rec.unpriced_classes.iter().cloned());
    "—".to_string()
}

pub(crate) fn format_cache_history(
    telemetry: &DebugCacheTelemetry,
    count: usize,
    cost: &DebugCostProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let total = telemetry.history.len();
    let start = total.saturating_sub(count);
    let rows: Vec<&DebugCacheTurn> = telemetry.history.iter().skip(start).collect();

    let mut totals_input: u64 = 0;
    let mut totals_hit: u64 = 0;
    let mut totals_miss: u64 = 0;
    let mut cache_reported = false;
    let mut totals_write: u64 = 0;
    let mut totals_reasoning: u64 = 0;
    // Preserve the host enum's reason order rather than sorting its labels.
    let mut unpriced_reasons: BTreeMap<u8, String> = BTreeMap::new();
    let mut unpriced_classes: BTreeSet<String> = BTreeSet::new();
    let mut header = presentation.translate(
        "cmd_cache_header",
        &[
            ("count", &rows.len().to_string()),
            ("total", &total.to_string()),
            ("model", &telemetry.model),
        ],
    )?;
    header.push_str(&"─".repeat(TURN_CACHE_TABLE_WIDTH));
    header.push('\n');
    header.push_str(TURN_CACHE_ROW_HEADER);
    header.push('\n');
    header.push_str(&"─".repeat(TURN_CACHE_TABLE_WIDTH));
    header.push('\n');

    let mut body = String::new();
    let absolute_start = total.saturating_sub(rows.len());
    for (i, rec) in rows.iter().enumerate() {
        let turn_index = absolute_start + i + 1;
        totals_input += u64::from(rec.input_tokens);

        let replay_cell = rec
            .reasoning_replay_tokens
            .map_or_else(|| "—".to_string(), |t| t.to_string());
        let write = u32::try_from(rec.priced_cache_write).unwrap_or(u32::MAX);
        let write_cell = rec
            .cache_write_tokens
            .map_or_else(|| "—".to_string(), |_| write.to_string());
        totals_write += rec.priced_cache_write;
        totals_reasoning += u64::from(rec.reasoning_tokens.unwrap_or(0));
        let cost_cell = turn_cost_cell(rec, cost, &mut unpriced_reasons, &mut unpriced_classes);
        let route_cell = format_turn_cache_route(rec);
        let age = codewhale_command_contract::elapsed::format_elapsed_secs(rec.age_seconds);

        // No cache telemetry → render `—` everywhere and don't pollute totals
        // with inferred zeros. Some providers (and some routes inside DeepSeek)
        // skip the cache fields; including a synthesized 0/N for those turns
        // would make every aggregate ratio look broken.
        if rec.cache_hit_tokens.is_none()
            && rec.cache_miss_tokens.is_none()
            && rec.cache_write_tokens.is_none()
        {
            body.push_str(&format!(
                "{turn:>4}  {route:<24}  {input:>5}  {output:>5}  {hit:>5}  {miss:>5}  {write:>5}  {replay:>6}   {ratio:>6}   {cost:>9}   {age}\n",
                turn = turn_index,
                route = route_cell,
                input = rec.input_tokens,
                output = rec.output_tokens,
                hit = "—",
                miss = "—",
                write = write_cell,
                replay = replay_cell,
                ratio = "—",
                cost = cost_cell,
                age = age,
            ));
            continue;
        }
        cache_reported = true;

        let miss_reported = rec.cache_miss_tokens;
        let hit = u32::try_from(rec.priced_cache_read).unwrap_or(u32::MAX);
        let miss = u32::try_from(rec.priced_cache_miss).unwrap_or(u32::MAX);
        // Use the same mutually-exclusive hit/miss/write partition as pricing.
        // Inferring `input - hit` here and then adding write counted creation
        // tokens twice in exactly the turns with a write premium.
        let accounted = u64::from(hit) + u64::from(miss) + u64::from(write);
        let ratio = if accounted == 0 {
            "    —".to_string()
        } else {
            format!("{:>5.1}%", 100.0 * f64::from(hit) / accounted as f64)
        };
        totals_hit += u64::from(hit);
        totals_miss += u64::from(miss);

        let miss_cell = match miss_reported {
            Some(_) => format!("{miss}"),
            None => format!("{miss}*"),
        };

        body.push_str(&format!(
            "{turn:>4}  {route:<24}  {input:>5}  {output:>5}  {hit:>5}  {miss:>5}  {write:>5}  {replay:>6}   {ratio}   {cost:>9}   {age}\n",
            turn = turn_index,
            route = route_cell,
            input = rec.input_tokens,
            output = rec.output_tokens,
            hit = hit,
            miss = miss_cell,
            write = write_cell,
            replay = replay_cell,
            ratio = ratio,
            cost = cost_cell,
            age = age,
        ));
    }

    // Anthropic-normalized aggregate: hit / (hit + miss + write).
    let totals_accounted = totals_hit + totals_miss + totals_write;
    let avg_ratio = if totals_accounted == 0 {
        "—".to_string()
    } else {
        format!(
            "{:.1}%",
            100.0 * totals_hit as f64 / totals_accounted as f64
        )
    };

    let total_cell = |total: u64| {
        if cache_reported {
            total.to_string()
        } else {
            "—".to_string()
        }
    };

    let mut footer = String::new();
    footer.push_str(&"─".repeat(TURN_CACHE_TABLE_WIDTH));
    footer.push('\n');
    // Reasoning is reported separately from `sum_out` on purpose: providers
    // count it *inside* the completion tokens they bill, so adding the two
    // would double-count it.
    footer.push_str(&format!(
        "sum_write: {}  sum_reasoning: {totals_reasoning} (already inside out)\n",
        total_cell(totals_write)
    ));
    footer.push_str(&presentation.translate(
        "cmd_cache_totals",
        &[
            ("sum_in", &totals_input.to_string()),
            ("sum_hit", &total_cell(totals_hit)),
            ("sum_miss", &total_cell(totals_miss)),
            ("avg", &avg_ratio),
        ],
    )?);
    footer.push_str(&presentation.translate("cmd_cache_footnote", &[])?);
    if !unpriced_reasons.is_empty() || !unpriced_classes.is_empty() {
        // Reasons are localized prose; token-class labels are key names and
        // stay raw, the same split `/cost` uses.
        let mut notes = Vec::new();
        for reason in unpriced_reasons.values() {
            notes.push(presentation.translate(&format!("cost_reason_{reason}"), &[])?);
        }
        notes.extend(unpriced_classes);
        footer.push_str(
            &presentation.translate("cmd_cache_unpriced_note", &[("notes", &notes.join(", "))])?,
        );
    }
    footer.push_str(&presentation.translate("cmd_cache_advice", &[])?);
    if let Some(line) = session_cache_rates_line(telemetry, presentation)? {
        footer.push_str("\n\n");
        footer.push_str(&line);
    }

    Ok(format!("{header}{body}{footer}"))
}

pub fn format_turn_cache_route(rec: &DebugCacheTurn) -> String {
    let Some(model) = rec.model.as_deref().filter(|model| !model.is_empty()) else {
        return "—".to_string();
    };
    let provider = rec
        .provider_identity
        .as_deref()
        .filter(|provider| !provider.trim().is_empty())
        .or(rec.provider.as_deref())
        .unwrap_or("?");
    let route = if rec.auto_model {
        format!("auto:{provider}/{model}")
    } else {
        format!("{provider}/{model}")
    };
    truncate_route_cell(&route, 24)
}

fn truncate_route_cell(route: &str, max_chars: usize) -> String {
    if route.chars().count() <= max_chars {
        return route.to_string();
    }
    if max_chars <= 3 {
        return route.chars().take(max_chars).collect();
    }
    let mut out: String = route.chars().take(max_chars - 3).collect();
    out.push_str("...");
    out
}

/// Preserve the upstream session-rate labels without exposing host state.
pub(super) fn session_cache_rates_line(
    telemetry: &DebugCacheTelemetry,
    presentation: &dyn CommandPresentationContext,
) -> Result<Option<String>, String> {
    let rates = telemetry.session_cache_rates;
    if rates.agents.is_none() {
        return Ok(None);
    }
    let labelled = rates.labelled(
        &presentation.translate("cmd_cache_rate_parent", &[])?,
        &presentation.translate("cmd_cache_rate_agents", &[])?,
        &presentation.translate("cmd_cache_rate_combined", &[])?,
    );
    labelled
        .map(|rates| presentation.translate("cmd_cache_session_rates", &[("rates", &rates)]))
        .transpose()
}
