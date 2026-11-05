use serde_json::Value;

use crate::schema_sanitize::{
    KimiParameterSchemaError, sanitize_for_kimi_parameters, sanitize_for_responses,
    sanitize_for_strict, strict_schema_supported,
};

pub const TOOL_PROJECTION_WARNING_MAX_NAMES: usize = 8;
pub const TOOL_PROJECTION_WARNING_MAX_NAME_CHARS: usize = 64;

/// Return a privacy-safe, display-bounded sample of omitted wire tool names.
///
/// MCP catalogs may contain thousands of tools and their names are supplied by
/// external servers. Keep the exact count separately, but never let one route
/// diagnostic flood a transcript, toast, or runtime record.
#[must_use]
pub fn bounded_tool_projection_warning_names(omitted_tool_names: &[String]) -> Vec<String> {
    omitted_tool_names
        .iter()
        .take(TOOL_PROJECTION_WARNING_MAX_NAMES)
        .map(|name| {
            let cleaned: String = name
                .chars()
                .filter(|c| !c.is_control() && !codewhale_sanitize::sanitize::is_bidi_control(*c))
                .collect();
            let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
            let collapsed = if collapsed.is_empty() {
                "<unnamed>".to_string()
            } else {
                collapsed
            };
            if collapsed.chars().count() <= TOOL_PROJECTION_WARNING_MAX_NAME_CHARS {
                return collapsed;
            }
            let mut out: String = collapsed
                .chars()
                .take(TOOL_PROJECTION_WARNING_MAX_NAME_CHARS - 1)
                .collect();
            out.push('…');
            out
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteToolProjection {
    Exact { strict_honored: bool },
    Relaxed { strict_honored: bool },
    RelaxedNonStrict,
    Mfjs { strict_honored: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolSchemaProjection {
    Exact {
        parameters: Value,
        strict: Option<bool>,
        constraint_note: Option<String>,
    },
    Relaxed {
        parameters: Value,
        strict: Option<bool>,
        constraint_note: Option<String>,
    },
    Omitted {
        reason: KimiParameterSchemaError,
    },
}

fn non_strict_flag(strict_intent: Option<bool>, strict_honored: bool) -> Option<bool> {
    match (strict_intent, strict_honored) {
        (_, false) => None,
        (Some(true), true) => None,
        (other, true) => other,
    }
}

fn projected(
    input_schema: &Value,
    strict_intent: Option<bool>,
    parameters: Value,
    strict: Option<bool>,
    constraint_note: Option<String>,
) -> ToolSchemaProjection {
    if parameters == *input_schema && strict == strict_intent && constraint_note.is_none() {
        ToolSchemaProjection::Exact {
            parameters,
            strict,
            constraint_note,
        }
    } else {
        ToolSchemaProjection::Relaxed {
            parameters,
            strict,
            constraint_note,
        }
    }
}

#[must_use]
pub fn project_tool_schema(
    route: RouteToolProjection,
    input_schema: &Value,
    strict_intent: Option<bool>,
) -> ToolSchemaProjection {
    match route {
        RouteToolProjection::Exact { strict_honored } => {
            let mut parameters = input_schema.clone();
            let strict = if strict_intent == Some(true)
                && strict_honored
                && strict_schema_supported(input_schema)
            {
                sanitize_for_strict(&mut parameters);
                Some(true)
            } else {
                non_strict_flag(strict_intent, strict_honored)
            };
            projected(input_schema, strict_intent, parameters, strict, None)
        }
        RouteToolProjection::Relaxed { strict_honored } => {
            let mut parameters = input_schema.clone();
            let constraint_note = sanitize_for_responses(&mut parameters);
            let strict = if strict_intent == Some(true)
                && strict_honored
                && strict_schema_supported(input_schema)
                && strict_schema_supported(&parameters)
            {
                sanitize_for_strict(&mut parameters);
                Some(true)
            } else {
                non_strict_flag(strict_intent, strict_honored)
            };
            projected(
                input_schema,
                strict_intent,
                parameters,
                strict,
                constraint_note,
            )
        }
        RouteToolProjection::RelaxedNonStrict => {
            let mut parameters = input_schema.clone();
            let constraint_note = sanitize_for_responses(&mut parameters);
            projected(
                input_schema,
                strict_intent,
                parameters,
                Some(false),
                constraint_note,
            )
        }
        RouteToolProjection::Mfjs { strict_honored } => {
            let strict_shape = strict_intent == Some(true)
                && strict_honored
                && strict_schema_supported(input_schema);
            let mut parameters = input_schema.clone();
            if strict_shape {
                sanitize_for_strict(&mut parameters);
            }
            match sanitize_for_kimi_parameters(&mut parameters) {
                Ok(constraint_note) => projected(
                    input_schema,
                    strict_intent,
                    parameters,
                    if strict_shape {
                        Some(true)
                    } else {
                        non_strict_flag(strict_intent, strict_honored)
                    },
                    constraint_note,
                ),
                Err(reason) => ToolSchemaProjection::Omitted { reason },
            }
        }
    }
}

#[must_use]
pub fn description_with_constraint_note(
    description: &str,
    constraint_note: Option<String>,
) -> String {
    match constraint_note {
        None => description.to_string(),
        Some(note) if description.trim().is_empty() => note,
        Some(note) => format!("{}\n\n{}", description.trim(), note),
    }
}

#[must_use]
pub fn forced_tool_name(tool_choice: &Value) -> Option<&str> {
    if let Some(name) = tool_choice.as_str() {
        return (!matches!(name, "auto" | "none" | "any" | "required")).then_some(name);
    }
    match tool_choice.get("type").and_then(Value::as_str) {
        Some("tool") => tool_choice.get("name").and_then(Value::as_str),
        Some("function") => tool_choice.get("name").and_then(Value::as_str).or_else(|| {
            tool_choice
                .pointer("/function/name")
                .and_then(Value::as_str)
        }),
        _ => None,
    }
}
