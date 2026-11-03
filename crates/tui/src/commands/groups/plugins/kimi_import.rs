//! Explicit local import bridge for Kimi-managed plugins.
//!
//! Listing is read-only and only considers immediate, canonical child
//! directories of `~/.kimi-code/plugins/managed`. Import requires the exact
//! content hash shown by listing, then routes the local directory through the
//! ordinary reviewed installer. The resulting Codewhale plugin still starts
//! disabled and untrusted; this module never launches or probes an external
//! Kimi application, daemon, MCP binary, or permission grant.
//!
//! FEAT-020: the managed-directory scan runs host-side in the TUI adapter;
//! the handler consumes the portable `PluginManagedScan` and renders it.

use super::render::ReviewTextContext;
use std::fmt::Write as _;
use std::path::Path;

use codewhale_command_contract::facets::{CommandPluginContext, CommandPresentationContext};

use crate::commands::CommandResult;

const LIST_COMMAND: &str = "/plugin import kimi [list]";
const APPROVE_COMMAND: &str = "/plugin import kimi approve <name> <content-hash>";

pub(super) fn usage(presentation: &mut dyn CommandPresentationContext) -> String {
    presentation
        .translate(
            "plugin_kimi_usage",
            &[
                ("list_command", LIST_COMMAND),
                ("approve_command", APPROVE_COMMAND),
            ],
        )
        .unwrap_or_default()
}

pub(super) fn dispatch(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    words: &[&str],
    home_override: Option<&Path>,
) -> CommandResult {
    match words {
        [] | ["list"] => list(presentation, plugin, home_override),
        ["approve", name, content_hash] => {
            approve(presentation, plugin, name, content_hash, home_override)
        }
        _ => CommandResult::error(usage(presentation)),
    }
}

fn list(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    home_override: Option<&Path>,
) -> CommandResult {
    let scan = match plugin.managed_scan(home_override) {
        Ok(scan) => scan,
        Err(error) => return CommandResult::error(error),
    };
    let root = escape_review_path(&scan.root, ReviewTextContext::Prose);
    let mut output = presentation
        .translate("plugin_kimi_managed_root_heading", &[("root", &root)])
        .unwrap_or_default();
    output.push('\n');
    if scan.candidates.is_empty() {
        output.push_str("  ");
        output.push_str(
            &presentation
                .translate("plugin_kimi_none_found", &[])
                .unwrap_or_default(),
        );
        output.push('\n');
    }
    for candidate in &scan.candidates {
        let name = escape_review_text(&candidate.name, ReviewTextContext::Prose);
        let version = escape_review_text(&candidate.version, ReviewTextContext::Prose);
        let license = candidate
            .license
            .as_deref()
            .map(|value| escape_review_text(value, ReviewTextContext::Prose))
            .unwrap_or_else(|| {
                presentation
                    .translate("plugin_kimi_license_unspecified", &[])
                    .unwrap_or_default()
            });
        let applicability = presentation
            .translate(
                if candidate.applicable {
                    "plugin_kimi_applicable"
                } else {
                    "plugin_kimi_not_applicable"
                },
                &[],
            )
            .unwrap_or_default();
        let inventory = escape_review_text(&candidate.inventory, ReviewTextContext::Prose);
        let summary = presentation
            .translate(
                "plugin_kimi_candidate_summary",
                &[
                    ("name", &name),
                    ("version", &version),
                    ("license", &license),
                    ("applicability", &applicability),
                    ("inventory", &inventory),
                ],
            )
            .unwrap_or_default();
        let _ = writeln!(output, "\n{summary}");

        let path = escape_review_path(&candidate.canonical_path, ReviewTextContext::Prose);
        let approve_command = format!(
            "/plugin import kimi approve {} {}",
            candidate.name, candidate.content_hash
        );
        let details = presentation
            .translate(
                "plugin_kimi_candidate_details",
                &[
                    ("path", &path),
                    ("content_hash", &candidate.content_hash),
                    ("capability_hash", &candidate.capability_hash),
                    ("approve_command", &approve_command),
                ],
            )
            .unwrap_or_default();
        let _ = writeln!(output, "{details}");
    }
    if !scan.rejected.is_empty() {
        output.push('\n');
        output.push_str(
            &presentation
                .translate("plugin_kimi_rejected_heading", &[])
                .unwrap_or_default(),
        );
        output.push('\n');
        for rejection in &scan.rejected {
            let _ = writeln!(output, "  - {rejection}");
        }
    }
    output.push('\n');
    output.push_str(
        &presentation
            .translate("plugin_kimi_inspection_footer", &[])
            .unwrap_or_default(),
    );
    CommandResult::message(output)
}

fn approve(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    name: &str,
    expected_hash: &str,
    home_override: Option<&Path>,
) -> CommandResult {
    let scan = match plugin.managed_scan(home_override) {
        Ok(scan) => scan,
        Err(error) => return CommandResult::error(error),
    };
    let Some(candidate) = scan
        .candidates
        .into_iter()
        .find(|candidate| candidate.name == name)
    else {
        let name = escape_review_text(name, ReviewTextContext::InlineCode);
        return CommandResult::error(
            presentation
                .translate(
                    "plugin_kimi_candidate_missing",
                    &[("name", &name), ("list_command", "/plugin import kimi")],
                )
                .unwrap_or_default(),
        );
    };
    if candidate.content_hash != expected_hash {
        let name = escape_review_text(name, ReviewTextContext::InlineCode);
        let expected = escape_review_text(expected_hash, ReviewTextContext::Prose);
        return CommandResult::error(
            presentation
                .translate(
                    "plugin_kimi_candidate_changed",
                    &[
                        ("name", &name),
                        ("expected", &expected),
                        ("actual", &candidate.content_hash),
                        ("list_command", "/plugin import kimi"),
                    ],
                )
                .unwrap_or_default(),
        );
    }

    // The facet revalidates and copies the source through the ordinary local
    // installer; its result is always rediscovered disabled/untrusted and
    // presents the post-copy authority review before any activation.
    super::install_bundle_with_expected_hash(
        presentation,
        plugin,
        &candidate.canonical_path,
        expected_hash,
    )
}

pub(super) fn escape_review_path(path: &Path, context: ReviewTextContext) -> String {
    super::escape_review_path(path, context)
}

pub(super) fn escape_review_text(value: &str, context: ReviewTextContext) -> String {
    super::escape_review_text(value, context)
}
