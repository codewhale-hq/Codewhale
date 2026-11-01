use codewhale_config::{EntryRef, PluginAuthority};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::composition_scope::SelectionRevision;
use codewhale_skills::{NativeSkillRef, NativeSkillRoots, SkillHostPort};

struct HostPort;
static PORT: HostPort = HostPort;

pub(super) fn install() {
    codewhale_skills::install_skill_host(&PORT);
}

impl SkillHostPort for HostPort {
    fn check_selection(
        &self,
        selection: Option<SelectionRevision>,
        plugins: Option<&PluginRegistry>,
        plugin_id: &str,
        hash: &str,
        scope: Option<&EntryRef>,
    ) -> Result<(), String> {
        super::manager()
            .shared
            .check_selection(selection, plugins, plugin_id, hash, scope)
    }

    fn verify_native_skill(
        &self,
        authority: &PluginAuthority,
        reference: &NativeSkillRef,
    ) -> Result<(), String> {
        super::skills::verify_native_skill(authority, reference)
    }

    fn native_roots(&self, plugins: &PluginRegistry) -> NativeSkillRoots {
        super::skills::roots_for_plugins(plugins)
            .into_iter()
            .map(|(root, authority, reference)| (root.snapshots, authority, reference))
            .collect()
    }
}
