use std::path::Path;

pub fn open_workspace_directory(
    workspace: &Path,
) -> anyhow::Result<(std::path::PathBuf, std::fs::File)> {
    let canonical = workspace.canonicalize()?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0200_0000 | 0x0020_0000); // BACKUP_SEMANTICS, OPEN_REPARSE_POINT
    }
    let directory = options.open(&canonical)?;
    let metadata = directory.metadata()?;
    anyhow::ensure!(
        metadata.is_dir() && !crate::path_identity::metadata_is_link_or_reparse(&metadata),
        "workspace is not an ordinary directory"
    );
    anyhow::ensure!(
        workspace.canonicalize()? == canonical,
        "selected workspace changed"
    );
    Ok((canonical, directory))
}
