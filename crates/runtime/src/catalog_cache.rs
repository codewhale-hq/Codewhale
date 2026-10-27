use std::path::PathBuf;

/// Filename under the CodeWhale `catalog` state dir.
pub const CACHE_FILE: &str = "models-dev-catalog.json";

/// Resolve the on-disk cache path under the CodeWhale `catalog` state dir.
///
/// Under `cfg(test)` this is confined the same way settings and config paths
/// are: `resolve_state_dir` lives in `codewhale-config`, which is compiled as a
/// plain dependency here and so has no view of this crate's isolated test root.
/// Without this shield a test that never asked for the developer's catalog read
/// their real `~/.codewhale/catalog` and rendered against whatever models they
/// last fetched (#5359).
#[must_use]
pub fn cache_path() -> Option<PathBuf> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        if !crate::test_support::guarded_environment_provides_state_paths() {
            return Some(
                crate::test_support::unsealed_test_state_root()
                    .join("catalog")
                    .join(CACHE_FILE),
            );
        }
    }
    codewhale_config::resolve_state_dir("catalog")
        .ok()
        .map(|dir| dir.join(CACHE_FILE))
}

pub fn valid_catalog_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().any(|byte| byte.is_ascii_alphanumeric())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-' | b'~')
        })
}
