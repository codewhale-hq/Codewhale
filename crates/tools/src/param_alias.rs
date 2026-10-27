use std::borrow::Cow;

use serde_json::Value;

use crate::ToolError;

/// Rewrite well-known parameter spellings from other coding harnesses onto the
/// names this tool actually implements.
///
/// Every mainstream harness names the same three file-edit arguments
/// differently — `old_string`/`new_string`, `old_str`/`new_str`,
/// `oldText`/`newText` — and models carry whichever spelling their training
/// saw most. CodeWhale's canonical `search`/`replace` is the odd one out, so a
/// model reaching for its prior used to burn a full turn on a rejection
/// (#5209) and then guess again. Translating an unambiguous synonym is
/// strictly better than refusing it: the edit the model asked for is the edit
/// that happens, and the schema still advertises exactly one canonical name so
/// there is no new ambiguity to learn.
///
/// This is deliberately *not* a silent-acceptance path. Only exact synonyms
/// are mapped, a synonym that disagrees with an explicitly supplied canonical
/// value is an error rather than a coin flip, and any parameter that is not a
/// known synonym still fails validation. The #5209 guarantee — no fabricated
/// "Replaced 1 occurrence" for an edit that never landed — is unchanged.
pub struct ParamAlias {
    /// Spelling a model might emit.
    alias: &'static str,
    /// Parameter this tool implements.
    canonical: &'static str,
}

pub const fn alias(alias: &'static str, canonical: &'static str) -> ParamAlias {
    ParamAlias { alias, canonical }
}

/// Path spellings shared by every file action. `path` is CodeWhale's
/// canonical name and the most common one in the field, but `file_path` is
/// widespread enough in training data to be worth accepting everywhere.
pub const PATH_ALIASES: &[ParamAlias] = &[alias("file_path", "path"), alias("filePath", "path")];

/// `input` with [`PATH_ALIASES`] folded onto `path`, exactly as the file
/// tools' `execute` does before touching disk.
///
/// Policy gates (typed file rules, the workspace-write carve-out, repo law,
/// Auto-Review) must judge the path the tool will act on. Reading only the
/// raw `path` key misses accepted `file_path`/`filePath` spellings. A
/// conflicting pair, which `execute` refuses, is returned unchanged.
pub fn with_canonical_path_argument(input: &Value) -> Cow<'_, Value> {
    if !PATH_ALIASES
        .iter()
        .any(|ParamAlias { alias, .. }| input.get(*alias).is_some())
    {
        return Cow::Borrowed(input);
    }
    let mut folded = input.clone();
    match apply_param_aliases(&mut folded, PATH_ALIASES, "path") {
        Ok(()) => Cow::Owned(folded),
        Err(_) => Cow::Borrowed(input),
    }
}

/// `path` and every spelling [`PATH_ALIASES`] folds onto it, for gates that
/// deliberately over-collect candidate targets.
pub fn path_argument_keys() -> impl Iterator<Item = &'static str> {
    std::iter::once("path").chain(PATH_ALIASES.iter().map(|ParamAlias { alias, .. }| *alias))
}

/// Apply `aliases` to `input`, in place.
///
/// An alias is consumed only when the canonical key is absent. When both are
/// present and *equal* the alias is dropped as a harmless duplicate; when both
/// are present and disagree the call fails, because guessing which one the
/// model meant is exactly the fabrication this path exists to prevent.
pub fn apply_param_aliases(
    input: &mut Value,
    aliases: &[ParamAlias],
    tool_label: &str,
) -> Result<(), ToolError> {
    let Some(obj) = input.as_object_mut() else {
        return Ok(());
    };

    for ParamAlias { alias, canonical } in aliases {
        let Some(alias_value) = obj.remove(*alias) else {
            continue;
        };
        match obj.get(*canonical) {
            None => {
                obj.insert((*canonical).to_string(), alias_value);
            }
            Some(existing) if existing == &alias_value => {}
            Some(_) => {
                return Err(ToolError::invalid_input(format!(
                    "{tool_label} received both `{canonical}` and its alias `{alias}` with different values, so the intended argument is ambiguous; nothing was changed. Pass only `{canonical}`."
                )));
            }
        }
    }

    Ok(())
}
