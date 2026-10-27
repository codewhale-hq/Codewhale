/// Whether one path component names the repository metadata directory as the
/// filesystem resolves it, not just as spelled: `.git` in any letter case
/// (macOS and Windows default to case-insensitive names) and, on Windows,
/// with the trailing dots/spaces or `:stream` suffix it drops and the `GIT~N`
/// short-name alias. The one `.git` rule for workspace file routes, file
/// restore, displayed workspace paths, the write carve-out and sub-agent
/// deliverables.
pub fn is_git_metadata_name(name: &std::ffi::OsStr) -> bool {
    git_metadata_name(&name.to_string_lossy(), cfg!(windows))
}

pub fn git_metadata_name(name: &str, windows: bool) -> bool {
    let name = if windows {
        name.split(':')
            .next()
            .unwrap_or_default()
            .trim_end_matches(['.', ' '])
    } else {
        name
    };
    name.eq_ignore_ascii_case(".git")
        || (windows
            && name.len() > 4
            && name
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("git~"))
            && name[4..].bytes().all(|byte| byte.is_ascii_digit()))
}
