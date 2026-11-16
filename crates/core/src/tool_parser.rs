//! Legacy parser for text-based tool calls from DeepSeek models.
//!
//! The engine prefers structured tool-call items and uses this fallback when
//! a response has tool-call markers but no structured calls. Unbalanced argument
//! objects are rejected; they must not become calls with invented empty args.
//!
//! Some DeepSeek outputs tool calls as text in various formats:
//! ```text
//! [TOOL_CALL]
//! {tool => "tool_name", args => {...}}
//! [/TOOL_CALL]
//! ```
//!
//! Or XML-style format:
//! ```text
//! <codewhale:tool_call>
//! <invoke name="tool_name">
//! <parameter name="arg">value</parameter>
//! </invoke>
//! </codewhale:tool_call>
//! ```
//!
//! This module parses these text patterns into structured tool calls.

use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// A parsed tool call from text content.
#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    /// Tool name
    pub name: String,
    /// Tool arguments as JSON
    pub args: Value,
    /// Generated ID for the tool call
    pub id: String,
}

/// Result of parsing text for tool calls.
#[derive(Debug)]
pub struct ParseResult {
    /// The text with tool call markers removed (for display)
    pub clean_text: String,
    /// Parsed tool calls found in the text
    pub tool_calls: Vec<ParsedToolCall>,
}

static TOOL_CALL_REGEX: OnceLock<Regex> = OnceLock::new();
static XML_TOOL_CALL_REGEX: OnceLock<Regex> = OnceLock::new();
static INVOKE_REGEX: OnceLock<Regex> = OnceLock::new();
static THINKING_REGEX: OnceLock<Regex> = OnceLock::new();
static FAKE_TOOL_WRAPPER_REGEX: OnceLock<Regex> = OnceLock::new();

const FAKE_TOOL_CALL_MARKERS: &[&str] = &[
    "<function_calls>",
    "<｜DSML｜tool_calls>",
    "<｜DSML｜invoke ",
    "<|DSML|tool_calls>",
    "<|DSML|invoke ",
    "<|dsml|tool_calls>",
    "<|dsml|invoke ",
    "<|tool_calls>",
    // DeepSeek native tool-call tokens (#3880). See
    // `engine::streaming::TOOL_CALL_MARKER_PAIRS` for why the `▁` (U+2581)
    // separator matters: these match no DSML entry, so they used to reach the
    // user as visible text.
    "<｜tool▁calls▁begin｜>",
    "<｜tool▁call▁begin｜>",
    "<|tool▁calls▁begin|>",
    "<|tool▁call▁begin|>",
    "<｜tool_calls_begin｜>",
    "<｜tool_call_begin｜>",
    "<|tool_calls_begin|>",
    "<|tool_call_begin|>",
];

/// Tool-call wrapper pairs whose start and end markers are plain literals, so
/// their regex alternative is built by escaping rather than hand-written. The
/// DSML entries stay hand-written above because they carry attributes
/// (`invoke name="…"`) and need `\b[^>]*>` rather than a literal match.
const LITERAL_FAKE_WRAPPER_PAIRS: &[(&str, &str)] = &[
    ("<｜tool▁calls▁begin｜>", "<｜tool▁calls▁end｜>"),
    ("<｜tool▁call▁begin｜>", "<｜tool▁call▁end｜>"),
    ("<｜tool▁outputs▁begin｜>", "<｜tool▁outputs▁end｜>"),
    ("<｜tool▁output▁begin｜>", "<｜tool▁output▁end｜>"),
    ("<|tool▁calls▁begin|>", "<|tool▁calls▁end|>"),
    ("<|tool▁call▁begin|>", "<|tool▁call▁end|>"),
    ("<|tool▁outputs▁begin|>", "<|tool▁outputs▁end|>"),
    ("<|tool▁output▁begin|>", "<|tool▁output▁end|>"),
    ("<｜tool_calls_begin｜>", "<｜tool_calls_end｜>"),
    ("<｜tool_call_begin｜>", "<｜tool_call_end｜>"),
    ("<｜tool_outputs_begin｜>", "<｜tool_outputs_end｜>"),
    ("<｜tool_output_begin｜>", "<｜tool_output_end｜>"),
    ("<|tool_calls_begin|>", "<|tool_calls_end|>"),
    ("<|tool_call_begin|>", "<|tool_call_end|>"),
    ("<|tool_outputs_begin|>", "<|tool_outputs_end|>"),
    ("<|tool_output_begin|>", "<|tool_output_end|>"),
];

fn get_tool_call_regex() -> &'static Regex {
    TOOL_CALL_REGEX.get_or_init(|| {
        // Match [TOOL_CALL] ... [/TOOL_CALL] blocks
        Regex::new(r"(?s)\[TOOL_CALL\]\s*(.*?)\s*\[/TOOL_CALL\]")
            .expect("TOOL_CALL regex pattern is valid")
    })
}

fn get_xml_tool_call_regex() -> &'static Regex {
    XML_TOOL_CALL_REGEX.get_or_init(|| {
        // Match <codewhale:tool_call>...</codewhale:tool_call> or similar XML patterns
        Regex::new(r"(?s)<(?:codewhale:)?tool_call[^>]*>\s*(.*?)\s*</(?:codewhale:)?tool_call>")
            .expect("XML tool_call regex pattern is valid")
    })
}

fn get_invoke_regex() -> &'static Regex {
    INVOKE_REGEX.get_or_init(|| {
        // Match <invoke name="tool_name">...</invoke> patterns
        Regex::new(r#"(?s)<invoke\s+name\s*=\s*"([^"]+)"[^>]*>(.*?)</invoke>"#)
            .expect("invoke regex pattern is valid")
    })
}

fn get_thinking_regex() -> &'static Regex {
    THINKING_REGEX.get_or_init(|| {
        // Match thinking blocks including partial closing tags
        Regex::new(r"(?s)</?(?:think|thinking)[^>]*>").expect("thinking regex pattern is valid")
    })
}

fn get_fake_tool_wrapper_regex() -> &'static Regex {
    FAKE_TOOL_WRAPPER_REGEX.get_or_init(|| {
        let mut alternatives = vec![
            r"<function_calls>.*?</function_calls>".to_string(),
            r"<｜DSML｜tool_calls>.*?</｜DSML｜tool_calls>".to_string(),
            r"<｜DSML｜invoke\b[^>]*>.*?</｜DSML｜invoke>".to_string(),
            r"<\|DSML\|tool_calls>.*?</\|DSML\|tool_calls>".to_string(),
            r"<\|DSML\|invoke\b[^>]*>.*?</\|DSML\|invoke>".to_string(),
            r"<\|dsml\|tool_calls>.*?</\|dsml\|tool_calls>".to_string(),
            r"<\|dsml\|invoke\b[^>]*>.*?</\|dsml\|invoke>".to_string(),
            r"<\|tool_calls>.*?</\|tool_calls>".to_string(),
        ];
        alternatives.extend(
            LITERAL_FAKE_WRAPPER_PAIRS
                .iter()
                .map(|(start, end)| format!("{}.*?{}", regex::escape(start), regex::escape(end))),
        );
        Regex::new(&format!("(?s){}", alternatives.join("|")))
            .expect("fake tool wrapper regex pattern is valid")
    })
}

/// Parse tool calls from text content.
/// Returns the clean text (with markers removed) and any parsed tool calls.
pub fn parse_tool_calls(text: &str) -> ParseResult {
    let mut tool_calls = Vec::new();
    let mut clean_text = text.to_string();
    let mut id_counter = 0;

    // First, remove thinking tags
    let thinking_regex = get_thinking_regex();
    clean_text = thinking_regex.replace_all(&clean_text, "").to_string();

    // Parse [TOOL_CALL] format
    let regex = get_tool_call_regex();
    for cap in regex.captures_iter(text) {
        let (Some(full_match), Some(inner)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        let full_match = full_match.as_str();
        let inner = inner.as_str().trim();

        if let Some(parsed) = parse_tool_call_inner(inner, &mut id_counter) {
            tool_calls.push(parsed);
        }

        clean_text = clean_text.replace(full_match, "");
    }

    // Parse XML-style <codewhale:tool_call> or <tool_call> format
    let xml_regex = get_xml_tool_call_regex();
    for cap in xml_regex.captures_iter(text) {
        let (Some(full_match), Some(inner)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        let full_match = full_match.as_str();
        let inner = inner.as_str().trim();

        // Parse invoke blocks inside
        if let Some(parsed) = parse_invoke_block(inner, &mut id_counter) {
            tool_calls.push(parsed);
        } else if let Some(parsed) = parse_tool_call_inner(inner, &mut id_counter) {
            tool_calls.push(parsed);
        }

        clean_text = clean_text.replace(full_match, "");
    }

    // Also parse standalone <invoke> blocks that might not be wrapped
    let invoke_regex = get_invoke_regex();
    for cap in invoke_regex.captures_iter(&clean_text.clone()) {
        let (Some(full_match), Some(tool_name), Some(inner)) = (cap.get(0), cap.get(1), cap.get(2))
        else {
            continue;
        };
        let full_match = full_match.as_str();
        let tool_name = tool_name.as_str();
        let inner = inner.as_str();

        let args = parse_xml_parameters(inner);
        id_counter += 1;
        tool_calls.push(ParsedToolCall {
            name: tool_name.to_string(),
            args,
            id: format!("xml_tool_{id_counter}"),
        });

        clean_text = clean_text.replace(full_match, "");
    }

    clean_text = get_fake_tool_wrapper_regex()
        .replace_all(&clean_text, "")
        .to_string();

    // Clean up extra whitespace and empty lines
    clean_text = clean_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    ParseResult {
        clean_text,
        tool_calls,
    }
}

/// Parse an `<invoke>` block into a tool call.
fn parse_invoke_block(content: &str, id_counter: &mut u32) -> Option<ParsedToolCall> {
    let invoke_regex = get_invoke_regex();
    let cap = invoke_regex.captures(content)?;

    let tool_name = cap.get(1)?.as_str();
    let inner = cap.get(2)?.as_str();

    let args = parse_xml_parameters(inner);

    *id_counter += 1;
    Some(ParsedToolCall {
        name: tool_name.to_string(),
        args,
        id: format!("xml_tool_{id_counter}"),
    })
}

/// Parse XML-style parameters like <parameter name="foo">value</parameter>
fn parse_xml_parameters(content: &str) -> Value {
    let param_regex = Regex::new(
        "(?s)<(?:parameter|param)\\s+name\\s*=\\s*\"([^\"]+)\"[^>]*>(.*?)</(?:parameter|param)>",
    )
    .ok();
    let simple_tag_regex =
        Regex::new("(?s)<([a-zA-Z_][a-zA-Z0-9_]*)>(.*?)</([a-zA-Z_][a-zA-Z0-9_]*)>").ok();

    let mut map = serde_json::Map::new();

    // Try parsing <parameter name="...">value</parameter>
    if let Some(regex) = param_regex {
        for cap in regex.captures_iter(content) {
            if let (Some(name), Some(value)) = (cap.get(1), cap.get(2)) {
                let name_str = name.as_str();
                let value_str = value.as_str().trim();

                // Try to parse as JSON, otherwise use as string
                let json_value = serde_json::from_str(value_str)
                    .unwrap_or_else(|_| Value::String(value_str.to_string()));
                map.insert(name_str.to_string(), json_value);
            }
        }
    }

    // Also try parsing <tagname>value</tagname> format
    if let Some(regex) = simple_tag_regex {
        for cap in regex.captures_iter(content) {
            if let (Some(name), Some(value), Some(close)) = (cap.get(1), cap.get(2), cap.get(3)) {
                if name.as_str() != close.as_str() {
                    continue;
                }
                let name_str = name.as_str();
                // Skip known wrapper tags
                if ["invoke", "tool_call", "parameter", "param"].contains(&name_str) {
                    continue;
                }
                let value_str = value.as_str().trim();
                if !map.contains_key(name_str) {
                    let json_value = serde_json::from_str(value_str)
                        .unwrap_or_else(|_| Value::String(value_str.to_string()));
                    map.insert(name_str.to_string(), json_value);
                }
            }
        }
    }

    Value::Object(map)
}

/// Parse the inner content of a `TOOL_CALL` block.
fn parse_tool_call_inner(inner: &str, id_counter: &mut u32) -> Option<ParsedToolCall> {
    // Try to parse as JSON first
    if let Ok(json) = serde_json::from_str::<Value>(inner) {
        return parse_from_json(&json, id_counter);
    }

    // Once the arrow format is recognized, invalid arguments must not fall
    // through to the looser name search inside the argument text.
    let tool_regex = Regex::new(r#"tool\s*=>\s*"([^"]+)""#).ok()?;
    if let Some(cap) = tool_regex.captures(inner) {
        let name = cap.get(1)?.as_str().to_string();
        return parse_arrow_syntax(inner, name, id_counter);
    }

    if inner.starts_with('{') && extract_braced_object(inner, ArgumentSyntax::Json)? != inner {
        return None;
    }

    // Try to extract tool name and args from any format
    parse_flexible_format(inner, id_counter)
}

/// Parse from JSON object.
fn parse_from_json(json: &Value, id_counter: &mut u32) -> Option<ParsedToolCall> {
    let obj = json.as_object()?;

    // Try different field names for the tool name
    let name = obj
        .get("tool")
        .or_else(|| obj.get("name"))
        .or_else(|| obj.get("function"))
        .and_then(|v| v.as_str())?
        .to_string();

    // Try different field names for the arguments
    let args = obj
        .get("args")
        .or_else(|| obj.get("arguments"))
        .or_else(|| obj.get("input"))
        .or_else(|| obj.get("parameters"))
        .cloned()
        .unwrap_or(json!({}));

    *id_counter += 1;
    Some(ParsedToolCall {
        name,
        args,
        id: format!("text_tool_{id_counter}"),
    })
}

/// Parse the arrow syntax: {tool => "name", args => {...}}
fn parse_arrow_syntax(inner: &str, name: String, id_counter: &mut u32) -> Option<ParsedToolCall> {
    // Match the same whitespace-tolerant arrow form as the tool name.
    let args_regex = Regex::new(r"\bargs\s*=>").ok()?;
    let args = if let Some(args_start) = args_regex.find(inner) {
        let args_str = inner[args_start.end()..].trim();
        let syntax = if args_str.strip_prefix('{').is_some_and(|content| {
            let content = content.trim_start();
            !content.starts_with('"') && !content.starts_with('}')
        }) {
            ArgumentSyntax::Cli
        } else {
            ArgumentSyntax::Json
        };
        // The enclosing arrow object must close too. Its arguments decide
        // the quote rules: CLI backslashes are literal, unlike JSON escapes.
        if inner.starts_with('{') && extract_braced_object(inner, syntax)? != inner {
            return None;
        }
        // Try to parse as JSON first
        if let Ok(args_json) = serde_json::from_str::<Value>(args_str) {
            args_json
        } else {
            let object = extract_braced_object(args_str, syntax)?;
            if let Ok(json) = serde_json::from_str::<Value>(object) {
                json
            } else {
                let content = &object[1..object.len() - 1];
                // A broken JSON object is not a CLI argument list, even if a
                // string inside it happens to contain `key=value` text.
                if matches!(syntax, ArgumentSyntax::Json) {
                    return None;
                }
                // Try CLI-style args: --arg_name "value" or --arg_name value
                let args = parse_cli_style_args(content);
                if args.as_object()?.is_empty() {
                    return None;
                }
                args
            }
        }
    } else {
        if inner.starts_with('{') && extract_braced_object(inner, ArgumentSyntax::Json)? != inner {
            return None;
        }
        json!({})
    };

    *id_counter += 1;
    Some(ParsedToolCall {
        name,
        args,
        id: format!("text_tool_{id_counter}"),
    })
}

/// Parse CLI-style arguments: --`arg_name` "value" or --`arg_name` value
fn parse_cli_style_args(content: &str) -> Value {
    let mut map = serde_json::Map::new();
    let mut key_value_content = std::borrow::Cow::Borrowed(content);

    // Pattern: --arg_name "value" or --arg_name 'value' or --arg_name value
    let arg_regex =
        Regex::new(r#"--([a-zA-Z_][a-zA-Z0-9_]*)\s+(?:"([^"]*)"|'([^']*)'|(\S+))"#).ok();

    if let Some(regex) = arg_regex {
        for cap in regex.captures_iter(content) {
            if let Some(arg_name) = cap.get(1) {
                let arg_name = arg_name.as_str();
                // Get the value from whichever capture group matched
                let value = cap
                    .get(2)
                    .or_else(|| cap.get(3))
                    .or_else(|| cap.get(4))
                    .map_or("", |m| m.as_str());

                // Try to parse as JSON value, otherwise use as string
                let json_value = serde_json::from_str(value)
                    .unwrap_or_else(|_| Value::String(value.to_string()));
                map.insert(arg_name.to_string(), json_value);
            }
        }
        key_value_content = regex.replace_all(content, " ");
    }

    // Also try simple key=value format
    let kv_regex =
        Regex::new(r#"([a-zA-Z_][a-zA-Z0-9_]*)\s*[:=]\s*(?:"([^"]*)"|'([^']*)'|(\S+))"#).ok();
    if let Some(regex) = kv_regex {
        for cap in regex.captures_iter(&key_value_content) {
            if let Some(key) = cap.get(1) {
                let key = key.as_str();
                if !map.contains_key(key) {
                    let value = cap
                        .get(2)
                        .or_else(|| cap.get(3))
                        .or_else(|| cap.get(4))
                        .map_or("", |m| m.as_str());
                    let json_value = serde_json::from_str(value)
                        .unwrap_or_else(|_| Value::String(value.to_string()));
                    map.insert(key.to_string(), json_value);
                }
            }
        }
    }

    Value::Object(map)
}

/// Try to parse a flexible format.
fn parse_flexible_format(inner: &str, id_counter: &mut u32) -> Option<ParsedToolCall> {
    // Look for common patterns like:
    // tool: list_dir
    // name: "list_dir"
    // function: list_dir

    let patterns = [(
        r#"(?:tool|name|function)\s*[:=]\s*"?([a-zA-Z_][a-zA-Z0-9_]*)"?"#,
        1,
    )];

    for (pattern, group) in patterns {
        if let Ok(regex) = Regex::new(pattern)
            && let Some(cap) = regex.captures(inner)
            && let Some(name_match) = cap.get(group)
        {
            let name = name_match.as_str().to_string();

            // Missing arguments may be empty; a present but malformed object
            // must not silently turn into an empty-argument tool call.
            let args = if inner.contains('{') {
                extract_json_object(inner)?
            } else {
                json!({})
            };

            *id_counter += 1;
            return Some(ParsedToolCall {
                name,
                args,
                id: format!("text_tool_{id_counter}"),
            });
        }
    }

    None
}

/// Extract the first JSON object from a string.
fn extract_json_object(text: &str) -> Option<Value> {
    serde_json::from_str(extract_braced_object(text, ArgumentSyntax::Json)?).ok()
}

#[derive(Clone, Copy)]
enum ArgumentSyntax {
    Json,
    Cli,
}

/// Extract one balanced object without treating quoted braces as delimiters.
/// Both JSON and the legacy CLI argument form use this byte-safe boundary scan.
fn extract_braced_object(text: &str, syntax: ArgumentSyntax) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut at_value_start = true;

    for (offset, ch) in text[start..].char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if matches!(syntax, ArgumentSyntax::Json) && ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
                at_value_start = false;
            }
            continue;
        }
        match ch {
            '"' if matches!(syntax, ArgumentSyntax::Json) || at_value_start => quote = Some(ch),
            '\'' if matches!(syntax, ArgumentSyntax::Cli) && at_value_start => quote = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + ch.len_utf8()]);
                }
            }
            _ => {}
        }
        // The existing CLI regex recognizes quotes only at the start of a
        // value; an apostrophe inside O'Brien.txt is ordinary filename data.
        at_value_start = ch.is_whitespace() || matches!(ch, '{' | ':' | '=' | '>');
    }
    None
}

/// Check if text contains tool call markers (either format).
pub fn has_tool_call_markers(text: &str) -> bool {
    text.contains("[TOOL_CALL]")
        || text.contains("<codewhale:tool_call")
        || text.contains("<tool_call")
        || text.contains("<invoke ")
        || FAKE_TOOL_CALL_MARKERS
            .iter()
            .any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_argument_values_do_not_create_extra_key_value_arguments() {
        let text = r#"[TOOL_CALL]{tool => "exec", args => {--command "MODE=dev node app.js"}}[/TOOL_CALL]"#;
        let result = parse_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.tool_calls[0].args,
            json!({"command": "MODE=dev node app.js"})
        );

        let mixed = r#"[TOOL_CALL]{tool => "write_file", args => {--content "mode=dev" path=out.txt}}[/TOOL_CALL]"#;
        let result = parse_tool_calls(mixed);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.tool_calls[0].args,
            json!({"content": "mode=dev", "path": "out.txt"})
        );
    }

    #[test]
    fn xml_parameters_preserve_multiline_content() {
        for parameter in [
            "<parameter name=\"content\">first line\nsecond line</parameter>",
            "<content>first line\nsecond line</content>",
        ] {
            let text = format!("<invoke name=\"write_file\">{parameter}</invoke>");
            let result = parse_tool_calls(&text);
            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0].name, "write_file");
            assert_eq!(
                result.tool_calls[0].args,
                json!({"content": "first line\nsecond line"})
            );
            assert!(result.clean_text.is_empty());
        }
    }

    #[test]
    fn test_parse_arrow_syntax() {
        let text = r#"I'll list the directory.
[TOOL_CALL]
{tool => "list_dir", args => {}}
[/TOOL_CALL]"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "list_dir");
        assert_eq!(result.clean_text, "I'll list the directory.");
    }

    #[test]
    fn test_parse_json_syntax() {
        let text = r#"Let me check.
[TOOL_CALL]
{"tool": "read_file", "args": {"path": "test.txt"}}
[/TOOL_CALL]"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "read_file");
        assert_eq!(result.tool_calls[0].args["path"], "test.txt");
    }

    #[test]
    fn unicode_and_quoted_braces_preserve_json_arguments() {
        for args in [
            json!({"content": "日本語"}),
            json!({"content": "你好世界，这是一个测试"}),
            json!({"content": "café 🐋"}),
            json!({
                "content": "literal } then {, escaped \"quote }\", and \\ slash",
                "nested": {"items": [{"text": "{深い}"}]},
            }),
        ] {
            for body in [
                format!(r#"{{tool => "write_file", args => {args}}}"#),
                format!(r#"{{tool=>"write_file", args=>{args}}}"#),
                format!("{{tool\t=>\"write_file\", args\t\n=>{args}}}"),
                format!("tool: write_file {args}"),
            ] {
                let result = parse_tool_calls(&format!("[TOOL_CALL]{body}[/TOOL_CALL]"));
                assert_eq!(result.tool_calls.len(), 1, "{body}");
                assert_eq!(result.tool_calls[0].name, "write_file");
                assert_eq!(result.tool_calls[0].args, args, "{body}");
            }
        }
    }

    #[test]
    fn unicode_and_quoted_braces_preserve_cli_arguments() {
        let result = parse_tool_calls(
            r#"[TOOL_CALL]
{tool => "write_file", args => {
--path "鲸鱼.md"
--content '你好 } { 世界'
}}
[/TOOL_CALL]"#,
        );
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.tool_calls[0].args,
            json!({"path": "鲸鱼.md", "content": "你好 } { 世界"})
        );

        // The legacy CLI grammar treats quoted backslashes literally,
        // including one immediately before the closing quote.
        let result = parse_tool_calls(
            r#"[TOOL_CALL]{tool => "write_file", args => {--path '目录\' --content 'literal } {\'}}[/TOOL_CALL]"#,
        );
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(
            result.tool_calls[0].args,
            json!({"path": "目录\\", "content": "literal } {\\"})
        );

        for (arguments, path) in [
            (r#"--path "C:\""#, "C:\\"),
            (r#"--path 'C:\'"#, "C:\\"),
            ("--path O'Brien.txt", "O'Brien.txt"),
            (r#"path="C:\""#, "C:\\"),
            ("path=O'Brien.txt", "O'Brien.txt"),
            (r#"--path "O'Brien {界}.txt""#, "O'Brien {界}.txt"),
        ] {
            let text =
                format!(r#"[TOOL_CALL]{{tool=>"read_file", args=>{{{arguments}}}}}[/TOOL_CALL]"#);
            let result = parse_tool_calls(&text);
            assert_eq!(result.tool_calls.len(), 1, "{text}");
            assert_eq!(result.tool_calls[0].name, "read_file");
            assert_eq!(result.tool_calls[0].args["path"], path, "{text}");
        }
    }

    #[test]
    fn malformed_objects_do_not_become_empty_or_nested_tool_calls() {
        for body in [
            r#"{tool => "write_file", args => {"#,
            // The argument object closes, but the enclosing arrow object does not.
            r#"{tool => "write_file", args => {"content":"日本語"}"#,
            r#"{tool => "write_file", args => {"content":"日本語",}}"#,
            r#"{tool => "write_file", args=> {"content":"x",}}"#,
            r#"{tool => "write_file", args  => {"content":"x",}}"#,
            "{tool => \"write_file\", args\t\n=> {\"content\":\"x\",}}",
            // Neither the tool name nor CLI-looking text in a broken JSON
            // string may be reinterpreted as a different argument format.
            r#"{tool => "write_file", args => {"content":"tool: exec_shell key=value",}}"#,
            r#"{tool => "write_file", args => {--content "unterminated}}"#,
            r#"{tool => "read_file", args => {--path "C:\"}"#,
            r#"{tool => "read_file", args => {--path O'Brien.txt}"#,
            r#"tool: write_file {"#,
            r#"tool: write_file {"content":"日本語""#,
            r#"tool: write_file {"content":"日本語",}"#,
            r#"tool: write_file {"content":"escaped quote \"}"#,
        ] {
            let result = parse_tool_calls(&format!("[TOOL_CALL]{body}[/TOOL_CALL]"));
            assert!(result.tool_calls.is_empty(), "{body}: {result:?}");
        }
    }

    #[test]
    fn flexible_call_without_an_argument_object_still_parses() {
        for body in ["tool: list_dir", r#"{tool => "list_dir"}"#] {
            let result = parse_tool_calls(&format!("[TOOL_CALL]{body}[/TOOL_CALL]"));
            assert_eq!(result.tool_calls.len(), 1);
            assert_eq!(result.tool_calls[0].name, "list_dir");
            assert_eq!(result.tool_calls[0].args, json!({}));
        }
    }

    #[test]
    fn test_parse_multiple_tool_calls() {
        let text = r#"First I'll list, then read.
[TOOL_CALL]
{tool => "list_dir", args => {}}
[/TOOL_CALL]
[TOOL_CALL]
{tool => "read_file", args => {"path": "file.txt"}}
[/TOOL_CALL]"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].name, "list_dir");
        assert_eq!(result.tool_calls[1].name, "read_file");
    }

    #[test]
    fn test_no_tool_calls() {
        let text = "Just some regular text without any tool calls.";
        let result = parse_tool_calls(text);
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.clean_text, text);
    }

    #[test]
    fn test_dsml_wrappers_are_stripped_without_execution() {
        let text = "before\n<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"read_file\">\n<｜DSML｜parameter name=\"path\" string=\"true\">secret.txt</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>\nafter";

        assert!(has_tool_call_markers(text));
        let result = parse_tool_calls(text);

        assert!(result.tool_calls.is_empty());
        assert!(result.clean_text.contains("before"));
        assert!(result.clean_text.contains("after"));
        assert!(!result.clean_text.contains("DSML"));
        assert!(!result.clean_text.contains("read_file"));
        assert!(!result.clean_text.contains("secret.txt"));
    }

    #[test]
    fn test_ascii_dsml_wrappers_are_stripped_without_execution() {
        let text = "before <|DSML|invoke name=\"grep_files\"><|DSML|parameter name=\"pattern\">SECRET</|DSML|parameter></|DSML|invoke> after";

        assert!(has_tool_call_markers(text));
        let result = parse_tool_calls(text);

        assert!(result.tool_calls.is_empty());
        assert!(result.clean_text.contains("before"));
        assert!(result.clean_text.contains("after"));
        assert!(!result.clean_text.contains("DSML"));
        assert!(!result.clean_text.contains("grep_files"));
        assert!(!result.clean_text.contains("SECRET"));
    }

    #[test]
    fn test_deepseek_native_tool_tokens_are_stripped_without_execution() {
        // #3880: DeepSeek's own tool-call tokens use `▁` (U+2581) as the word
        // separator, so they matched none of the DSML shapes and survived into
        // the text shown to the user.
        for (start, end) in [
            ("<｜tool▁calls▁begin｜>", "<｜tool▁calls▁end｜>"),
            ("<｜tool▁call▁begin｜>", "<｜tool▁call▁end｜>"),
            ("<|tool▁calls▁begin|>", "<|tool▁calls▁end|>"),
            ("<｜tool_calls_begin｜>", "<｜tool_calls_end｜>"),
            ("<|tool_call_begin|>", "<|tool_call_end|>"),
        ] {
            let text = format!(
                "before {start}function<｜tool▁sep｜>grep_files\n```json\n{{\"pattern\":\"SECRET\"}}\n```{end} after"
            );

            assert!(has_tool_call_markers(&text), "not detected: {start}");
            let result = parse_tool_calls(&text);

            // The wrapper is scrubbed, never executed: a model forging a tool
            // call in plain text must not become a real invocation.
            assert!(result.tool_calls.is_empty(), "{start} was executed");
            assert!(
                result.clean_text.contains("before"),
                "{:?}",
                result.clean_text
            );
            assert!(
                result.clean_text.contains("after"),
                "{:?}",
                result.clean_text
            );
            assert!(
                !result.clean_text.contains("grep_files")
                    && !result.clean_text.contains("SECRET")
                    && !result.clean_text.contains("tool▁")
                    && !result.clean_text.contains("tool_calls"),
                "leaked {start}: {:?}",
                result.clean_text
            );
        }
    }

    #[test]
    fn test_has_markers() {
        assert!(has_tool_call_markers("[TOOL_CALL]test[/TOOL_CALL]"));
        assert!(has_tool_call_markers(
            "<｜DSML｜tool_calls>...</｜DSML｜tool_calls>"
        ));
        assert!(!has_tool_call_markers("no markers here"));
    }
}
