//! Shared command-discovery shadowing contract.
//!
//! Both the command palette (`tui::command_palette`) and slash completion
//! (`tui::widgets`) must interpret user-command ownership over built-in
//! command tokens identically. This module owns the three decisions they
//! share:
//!
//! - whether a user command (by canonical name or accepted alias) shadows a
//!   built-in canonical token,
//! - whether a user command shadows a specific built-in alias token, and
//! - which built-in aliases remain unshadowed and may be presented.
//!
//! The functions are pure: they consume immutable built-in metadata and
//! accepted user-command metadata and never persist or cache registry state.

use super::traits::CommandInfo;
use super::user_registry::UserCommandMetadata;

/// Returns true when any user command claims the built-in canonical token,
/// either through its canonical name or through an accepted alias.
///
/// Hidden user commands retain token ownership even though they are excluded
/// from discovery output rows.
pub fn user_command_shadows_builtin_canonical(
    builtin: &CommandInfo,
    user_commands: &[&UserCommandMetadata],
) -> bool {
    user_commands.iter().any(|user| {
        user.name == builtin.name || user.aliases.iter().any(|alias| alias == builtin.name)
    })
}

/// Returns true when any user command claims the given built-in alias token,
/// either through its canonical name or through an accepted alias.
pub fn user_command_shadows_builtin_alias(
    builtin_alias: &str,
    user_commands: &[&UserCommandMetadata],
) -> bool {
    user_commands.iter().any(|user| {
        user.name == builtin_alias || user.aliases.iter().any(|alias| alias == builtin_alias)
    })
}

/// Returns the built-in aliases that are not claimed by any user command,
/// preserving the built-in declaration order.
///
/// The built-in canonical token itself is not part of this projection; callers
/// decide canonical visibility through [`user_command_shadows_builtin_canonical`].
pub fn unshadowed_builtin_aliases<'a>(
    builtin: &'a CommandInfo,
    user_commands: &[&UserCommandMetadata],
) -> Vec<&'a str> {
    builtin
        .aliases
        .iter()
        .copied()
        .filter(|alias| !user_command_shadows_builtin_alias(alias, user_commands))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::get_command_info;

    fn help_builtin() -> &'static CommandInfo {
        get_command_info("help").expect("built-in help must be registered")
    }

    fn attach_builtin() -> &'static CommandInfo {
        get_command_info("attach").expect("built-in attach must be registered")
    }

    fn metadata(name: &str, aliases: &[&str], hidden: bool) -> UserCommandMetadata {
        UserCommandMetadata {
            name: name.to_string(),
            body: String::new(),
            description: Some(format!("description of {name}")),
            usage: None,
            arguments: None,
            argument_hint: None,
            allowed_tools: None,
            pausable: false,
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            hidden,
            plugin_authority: None,
            workspace_sourced: false,
            extension: None,
        }
    }

    fn slice(commands: &[UserCommandMetadata]) -> Vec<&UserCommandMetadata> {
        commands.iter().collect()
    }

    #[test]
    fn canonical_name_claims_builtin_canonical_token() {
        let user = metadata("help", &[], false);
        let owned = [user];
        let users = slice(&owned);
        assert!(user_command_shadows_builtin_canonical(
            help_builtin(),
            &users
        ));
    }

    #[test]
    fn accepted_alias_claims_builtin_canonical_token() {
        let user = metadata("assistant", &["help"], false);
        let owned = [user];
        let users = slice(&owned);
        assert!(user_command_shadows_builtin_canonical(
            help_builtin(),
            &users
        ));
    }

    #[test]
    fn hidden_user_command_retains_canonical_shadow_ownership() {
        let user = metadata("help", &[], true);
        let owned = [user];
        let users = slice(&owned);
        assert!(user_command_shadows_builtin_canonical(
            help_builtin(),
            &users
        ));
    }

    #[test]
    fn unrelated_user_commands_do_not_shadow() {
        let user = metadata("assistant", &["a"], false);
        let owned = [user];
        let users = slice(&owned);
        assert!(!user_command_shadows_builtin_canonical(
            help_builtin(),
            &users
        ));
        assert!(!user_command_shadows_builtin_alias("?", &users));
    }

    #[test]
    fn canonical_name_claims_builtin_alias_token() {
        let user = metadata("?", &[], false);
        let owned = [user];
        let users = slice(&owned);
        assert!(user_command_shadows_builtin_alias("?", &users));
    }

    #[test]
    fn accepted_alias_claims_builtin_alias_token() {
        let user = metadata("assistant", &["image"], false);
        let owned = [user];
        let users = slice(&owned);
        assert!(user_command_shadows_builtin_alias("image", &users));
    }

    #[test]
    fn unshadowed_aliases_preserve_declaration_order() {
        let user = metadata("assistant", &["image"], false);
        let owned = [user];
        let users = slice(&owned);
        let aliases = unshadowed_builtin_aliases(attach_builtin(), &users);
        assert_eq!(aliases, vec!["media", "fujian"]);
    }

    #[test]
    fn claimed_canonical_token_does_not_change_alias_projection() {
        // A user command claiming the built-in canonical token makes the whole
        // built-in invisible; the alias projection stays stable so consumers
        // can rely on it for the description fallback path.
        let user = metadata("attach", &[], false);
        let owned = [user];
        let users = slice(&owned);
        let aliases = unshadowed_builtin_aliases(attach_builtin(), &users);
        assert_eq!(aliases, vec!["image", "media", "fujian"]);
    }

    #[test]
    fn hidden_commands_shadow_aliases_too() {
        let user = metadata("secret", &["image"], true);
        let owned = [user];
        let users = slice(&owned);
        assert!(user_command_shadows_builtin_alias("image", &users));
        let aliases = unshadowed_builtin_aliases(attach_builtin(), &users);
        assert_eq!(aliases, vec!["media", "fujian"]);
    }

    #[test]
    fn all_aliases_shadowed_yields_empty_projection() {
        let user = metadata("assistant", &["image", "media", "fujian"], false);
        let owned = [user];
        let users = slice(&owned);
        let aliases = unshadowed_builtin_aliases(attach_builtin(), &users);
        assert!(aliases.is_empty());
    }

    #[test]
    fn rejected_aliases_do_not_shadow() {
        // Rejected aliases are absent from accepted metadata and therefore
        // cannot claim any token.
        let user = metadata("assistant", &[], false);
        let owned = [user];
        let users = slice(&owned);
        assert!(!user_command_shadows_builtin_alias("collision", &users));
    }

    #[test]
    fn empty_metadata_shadows_nothing() {
        let users: Vec<&UserCommandMetadata> = Vec::new();
        assert!(!user_command_shadows_builtin_canonical(
            help_builtin(),
            &users
        ));
        assert!(!user_command_shadows_builtin_alias("?", &users));
        let aliases = unshadowed_builtin_aliases(help_builtin(), &users);
        assert_eq!(aliases, vec!["?", "bangzhu", "帮助"]);
    }

    #[test]
    fn discovery_predicates_agree_with_registry_lookup() {
        // Contract guard: the shared predicates must agree with the registry's
        // own alias-aware `get` lookup used by the palette, so the Phase 3/4
        // rewiring cannot introduce a behavioral divergence.
        let user = metadata("assistant", &["help"], false);
        let owned = [user];
        let users = slice(&owned);
        let registry = crate::commands::user_registry::UserCommandRegistry::from_loaded(vec![(
            "assistant".to_string(),
            "---\ndescription: d\naliases: help\n---\nbody".to_string(),
        )]);
        assert_eq!(
            registry.get("help").is_some(),
            user_command_shadows_builtin_canonical(help_builtin(), &users),
            "registry lookup and shared predicate must agree"
        );
    }
}
