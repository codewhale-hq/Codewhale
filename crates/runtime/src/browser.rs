use std::process::Command;

use anyhow::Result;

/// Open a URL in the system's default browser.
///
/// Dispatches to the platform-appropriate opener:
/// - macOS: `open`
/// - Linux / BSD: `xdg-open`
/// - Windows: `rundll32 url.dll,FileProtocolHandler`
/// - Other: returns an error.
///
/// This is the single entry point for URL opening — every call site in
/// the codebase should use this instead of hardcoding `Command::new("open")`,
/// `Command::new("xdg-open")`, or `Command::new("cmd")`.
pub fn open_url(url: &str) -> Result<()> {
    let mut command = browser_open_command(url)?;
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("failed to launch browser command: {e}"))
}

fn browser_open_command(url: &str) -> Result<Command> {
    if url.trim().is_empty() {
        return Err(anyhow::anyhow!("browser URL cannot be empty"));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        Ok(command)
    }

    #[cfg(any(
        all(target_os = "linux", not(target_env = "ohos")),
        target_os = "netbsd",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        Ok(command)
    }

    // Not `cmd /C start`: cmd.exe would parse `&`, `|`, `^` and `%` inside
    // the URL. The protocol handler receives it as data.
    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("rundll32");
        cmd.args(["url.dll,FileProtocolHandler", url]);
        Ok(cmd)
    }

    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "linux", not(target_env = "ohos")),
        target_os = "windows",
        target_os = "netbsd",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )))]
    Err(anyhow::anyhow!(
        "browser opening is unsupported on this platform"
    ))
}

#[cfg(test)]
mod tests {
    // ===================================================================
    // open_url tests
    // ===================================================================

    #[test]
    fn open_url_builds_platform_command_without_spawning() {
        let command = super::browser_open_command("https://example.com").expect("command");

        #[cfg(target_os = "macos")]
        {
            assert_eq!(command.get_program(), "open");
            assert_eq!(
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                vec!["https://example.com"]
            );
        }

        #[cfg(any(
            target_os = "netbsd",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            assert_eq!(command.get_program(), "xdg-open");
        }

        #[cfg(all(target_os = "linux", not(target_env = "ohos")))]
        {
            assert_eq!(command.get_program(), "xdg-open");
            assert_eq!(
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                vec!["https://example.com"]
            );
        }

        #[cfg(target_os = "windows")]
        {
            assert_eq!(command.get_program(), "rundll32");
            assert_eq!(
                command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                vec!["url.dll,FileProtocolHandler", "https://example.com"]
            );
            // Shell metacharacters stay inside the single URL argument.
            let url = "https://example.com/?a=1&b=2|x^y%PATH%";
            let command = super::browser_open_command(url).expect("command");
            assert_eq!(command.get_program(), "rundll32");
            assert_eq!(
                command
                    .get_args()
                    .last()
                    .map(|arg| arg.to_string_lossy().into_owned()),
                Some(url.to_string())
            );
        }
    }

    #[test]
    fn open_url_rejects_empty_url_gracefully() {
        // An empty URL should fail with a clear error, not panic.
        let result = super::browser_open_command("");
        match result {
            Ok(_) => panic!("empty URL should not build an opener command"),
            Err(e) => {
                let msg = e.to_string();
                assert!(!msg.is_empty(), "error message must not be empty");
                assert!(msg.contains("empty"), "unexpected error message: {msg}");
            }
        }
    }
}
