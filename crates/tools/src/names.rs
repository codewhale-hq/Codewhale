pub const MULTI_TOOL_PARALLEL_NAME: &str = "multi_tool_use.parallel";

pub const REQUEST_USER_INPUT_NAME: &str = "request_user_input";

pub const TOOL_SEARCH_NAME: &str = "tool_search";

pub const LEGACY_TOOL_SEARCH_REGEX_NAME: &str = "tool_search_tool_regex";

pub const LEGACY_TOOL_SEARCH_BM25_NAME: &str = "tool_search_tool_bm25";

pub fn is_tool_search_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_SEARCH_NAME | LEGACY_TOOL_SEARCH_REGEX_NAME | LEGACY_TOOL_SEARCH_BM25_NAME
    )
}

/// Sentinel phrase the TUI matches on to recognise a current-format truncated
/// preview. It must stay a literal substring of every footer variant.
pub const SPILLOVER_RECOVERY_HINT: &str = "omitted range recovery:";
