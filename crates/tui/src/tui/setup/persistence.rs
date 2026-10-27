use std::path::Path;

use crate::tui::app::App;
use codewhale_runtime::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SetupPersistenceFacts {
    pub(super) home_result: String,
    pub(super) config_result: String,
    pub(super) state_result: String,
    pub(super) constitution_result: String,
    pub(super) memory_result: String,
    pub(super) notes_result: String,
    pub(super) result: String,
}

impl Default for SetupPersistenceFacts {
    fn default() -> Self {
        Self {
            home_result: "CODEWHALE_HOME not loaded".to_string(),
            config_result: "config path not loaded".to_string(),
            state_result: "setup state path not loaded".to_string(),
            constitution_result: "constitution path not loaded".to_string(),
            memory_result: "memory path not loaded".to_string(),
            notes_result: "notes path not loaded".to_string(),
            result: "persistence paths not loaded".to_string(),
        }
    }
}

impl SetupPersistenceFacts {
    pub(super) fn from_app_config(app: &App, config: &Config, codewhale_home: &Path) -> Self {
        let home_source = if codewhale_config::codewhale_home_is_explicit() {
            "explicit"
        } else {
            "default"
        };
        let home_presence = dir_presence(codewhale_home);
        let config_path = codewhale_config::resolve_config_path(app.config_path.clone())
            .unwrap_or_else(|_| codewhale_home.join("config.toml"));
        let state_path = codewhale_config::SetupState::path().unwrap_or_else(|_| {
            codewhale_home.join(codewhale_config::setup_state::SETUP_STATE_FILE_NAME)
        });
        let constitution_path = codewhale_config::UserConstitution::path()
            .unwrap_or_else(|_| codewhale_home.join("constitution.json"));
        let memory_path = config.memory_path();
        let notes_path = config.notes_path();

        let config_presence = file_presence(&config_path);
        let state_presence = file_presence(&state_path);
        let constitution_presence = file_presence(&constitution_path);
        let memory_presence = file_presence(&memory_path);
        let notes_presence = file_presence(&notes_path);

        Self {
            home_result: format!(
                "{home_source} CODEWHALE_HOME at {} ({home_presence})",
                codewhale_home.display()
            ),
            config_result: path_result(&config_path, config_presence),
            state_result: path_result(&state_path, state_presence),
            constitution_result: path_result(&constitution_path, constitution_presence),
            memory_result: path_result(&memory_path, memory_presence),
            notes_result: path_result(&notes_path, notes_presence),
            result: format!(
                "home_source={home_source}, home={home_presence}, config={config_presence}, setup_state={state_presence}, constitution={constitution_presence}, memory={memory_presence}, notes={notes_presence}, mode=read_only_review"
            ),
        }
    }
}

fn path_result(path: &Path, presence: &'static str) -> String {
    format!("{} ({presence})", path.display())
}

fn dir_presence(path: &Path) -> &'static str {
    if path.is_dir() {
        "present"
    } else if path.exists() {
        "exists-not-dir"
    } else {
        "missing"
    }
}

fn file_presence(path: &Path) -> &'static str {
    if path.is_file() {
        "present"
    } else if path.exists() {
        "exists-not-file"
    } else {
        "missing"
    }
}
