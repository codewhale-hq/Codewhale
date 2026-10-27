pub const TOOL_PROJECTION_WARNING_MAX_NAMES: usize = 8;
pub const TOOL_PROJECTION_WARNING_MAX_NAME_CHARS: usize = 64;

/// Return a privacy-safe, display-bounded sample of omitted wire tool names.
///
/// MCP catalogs may contain thousands of tools and their names are supplied by
/// external servers. Keep the exact count separately, but never let one route
/// diagnostic flood a transcript, toast, or runtime record.
#[must_use]
pub fn bounded_tool_projection_warning_names(omitted_tool_names: &[String]) -> Vec<String> {
    omitted_tool_names
        .iter()
        .take(TOOL_PROJECTION_WARNING_MAX_NAMES)
        .map(|name| {
            let cleaned: String = name
                .chars()
                .filter(|c| !c.is_control() && !codewhale_sanitize::sanitize::is_bidi_control(*c))
                .collect();
            let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
            let collapsed = if collapsed.is_empty() {
                "<unnamed>".to_string()
            } else {
                collapsed
            };
            if collapsed.chars().count() <= TOOL_PROJECTION_WARNING_MAX_NAME_CHARS {
                return collapsed;
            }
            let mut out: String = collapsed
                .chars()
                .take(TOOL_PROJECTION_WARNING_MAX_NAME_CHARS - 1)
                .collect();
            out.push('…');
            out
        })
        .collect()
}
