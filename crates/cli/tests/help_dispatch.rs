//! Installed help must not load configuration or migrate credentials.

use std::fs;
use std::process::Command;

use codewhale_secrets::{FileKeyringStore, KeyringStore};
use tempfile::TempDir;

const SENTINEL: &str = "cw-help-must-not-read-credentials-91a7";

#[test]
fn passthrough_help_leaves_configuration_and_legacy_credentials_untouched() {
    for malformed in [false, true] {
        let fixture = TempDir::new().expect("isolated help fixture");
        let home = fixture.path().join("home");
        let primary = home.join(".codewhale");
        fs::create_dir_all(&primary).expect("create isolated config directory");
        let config = primary.join("config.toml");
        let config_bytes = if malformed {
            format!("invalid = [{SENTINEL}\n")
        } else {
            "provider = \"deepseek\"\n".to_string()
        };
        fs::write(&config, &config_bytes).expect("write config fixture");
        let legacy = home.join(".deepseek/secrets/secrets.json");
        FileKeyringStore::new(&legacy)
            .set("deepseek", SENTINEL)
            .expect("seed synthetic legacy credential");
        let legacy_bytes = fs::read(&legacy).expect("read legacy fixture");

        for subcommand in ["exec", "init", "setup", "mcp", "review", "rc", "uninstall"] {
            for flag in ["--help", "-h"] {
                let output = Command::new(env!("CARGO_BIN_EXE_codewhale"))
                    .env_clear()
                    .env("HOME", &home)
                    .env("USERPROFILE", &home)
                    .env("CODEWHALE_SECRET_BACKEND", "file")
                    // No CODEWHALE_HOME: this is the legacy-migration path.
                    .current_dir(&home)
                    .args([subcommand, flag])
                    .output()
                    .expect("run installed help");
                assert!(output.status.success(), "{subcommand} {flag}");
                assert!(output.stderr.is_empty(), "{subcommand} {flag}");
                let help = String::from_utf8(output.stdout).expect("UTF-8 help");
                assert!(
                    help.contains(&format!("Usage: codewhale {subcommand}")),
                    "{subcommand} must show its own help"
                );
                assert!(!help.contains(SENTINEL));
                assert_eq!(fs::read_to_string(&config).unwrap(), config_bytes);
                assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
                assert!(
                    !primary.join("secrets").exists(),
                    "help must not migrate credentials"
                );
                assert!(
                    !primary.join("telemetry").exists(),
                    "help must not initialize telemetry"
                );
            }
        }
    }
}
