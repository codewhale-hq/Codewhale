//! Deterministic JSON argument repair for malformed tool-call inputs.
//!
//! DeepSeek streams `tool_calls.function.arguments` as deltas. Two failure
//! shapes are common: (a) SSE chunk boundary cuts inside a JSON string and
//! reassembly leaves a trailing comma or unclosed brace; (b) some local
//! backends emit literal control characters inside JSON string values.
//!
//! The repair ladder runs five stages before reporting unrecoverable input:
//!
//!  1. Strict parse — done if it parses.
//!  2. Strip literal control chars inside string values.
//!  3. Strip trailing commas before `}` or `]`.
//!  4. Balance braces/brackets (append closers).
//!  5. Strip excess closers if delta is negative.
//!
//! Stages 1-3 never change structure: they parse as-is, or normalize text
//! that was already structurally complete. Stages 4-5 do — they synthesize
//! or discard closers to force a parse. A value that only parsed because of
//! stage 4 or 5 came from argument text that was *incomplete*, and the usual
//! cause is a provider cutting the stream at its output limit mid-argument.
//! `Repaired::structure_synthesized` reports that, because such a value must
//! never be dispatched as if the model had finished writing it.

use serde_json::Value;

/// Maximum raw argument length we'll attempt to repair (1 MiB).
const MAX_ARG_LEN: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ArgRepairError {
    #[error("argument exceeded {0} chars; refusing to repair")]
    TooLarge(usize),
    #[error("argument could not be repaired into valid JSON")]
    Unrepairable,
}

/// Repair a raw JSON argument string into a valid `serde_json::Value`.
///
/// Runs the deterministic ladder; on success returns the parsed value.
/// A repaired value plus whether the repair had to invent structure.
#[derive(Debug, Clone)]
pub struct Repaired {
    pub value: Value,
    /// True when the text only parsed after closers were appended (stage 4)
    /// or discarded (stage 5) — i.e. the argument text was structurally
    /// incomplete. Callers making the *final* dispatch decision must treat
    /// this as malformed input rather than executing it.
    pub structure_synthesized: bool,
}

impl Repaired {
    fn intact(value: Value) -> Self {
        Self {
            value,
            structure_synthesized: false,
        }
    }
    fn synthesized(value: Value) -> Self {
        Self {
            value,
            structure_synthesized: true,
        }
    }
}

pub fn repair(raw: &str) -> Result<Repaired, ArgRepairError> {
    // Stage 1: strict parse. Valid JSON is returned intact at any size: the
    // size bound exists to cap the cost of the repair stages below, and a
    // strict serde parse is what callers already fall back to on oversize
    // input. Checking size first made every valid argument over the bound
    // (a large `write`, a generated fixture) fail as malformed.
    if let Ok(v) = serde_json::from_str(raw) {
        return Ok(Repaired::intact(v));
    }
    if raw.len() > MAX_ARG_LEN {
        return Err(ArgRepairError::TooLarge(raw.len()));
    }
    // Stage 2: strip control chars inside strings
    let mut s = strip_control_chars_in_strings(raw);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(Repaired::intact(v));
    }
    // Stage 3: strip trailing commas
    s = strip_trailing_commas(&s);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(Repaired::intact(v));
    }
    // Stage 4: balance braces
    // Stages 4 and 5 change structure. Anything they rescue is reported as
    // synthesized: `balance_braces` counts braces without tracking string
    // literals, so a stream cut at the end of a complete string value yields
    // JSON that parses cleanly and is still missing whatever the model had
    // not written yet.
    s = balance_braces(&s, 50);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(Repaired::synthesized(v));
    }
    // Stage 5: strip excess closers
    s = strip_excess_closers(&s);
    if let Ok(v) = serde_json::from_str(&s) {
        return Ok(Repaired::synthesized(v));
    }
    Err(ArgRepairError::Unrepairable)
}

/// Strip ASCII control characters (0x00–0x1F except \t, \n, \r) that appear
/// inside JSON string values. We walk character-by-character tracking whether
/// we're inside a string (between unescaped double-quotes).
fn strip_control_chars_in_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            out.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            out.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if in_string && (ch as u32) < 0x20 && ch != '\t' && ch != '\n' && ch != '\r' {
            // Drop control characters inside strings
            continue;
        }
        out.push(ch);
    }
    out
}

/// Strip trailing commas before `}` or `]` (optionally across whitespace)
/// and at end of input. String-aware: a `,}` or `,]` inside a string value
/// is content, e.g. source code in a `write` call, and is left untouched.
///
/// Single pass: a run of commas and whitespace outside a string is held
/// back until the next significant character decides it, so a long run of
/// commas (a model repetition loop, re-repaired on every streamed delta)
/// costs linear time rather than a rescan per comma.
fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // Held-back commas and whitespace, and whether it holds any comma.
    let mut held = String::new();
    let mut held_comma = false;
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        if ch == ',' {
            held.push(ch);
            held_comma = true;
            continue;
        }
        if held_comma && ch.is_whitespace() {
            held.push(ch);
            continue;
        }
        if held_comma {
            flush_held_commas(&mut out, &held, matches!(ch, '}' | ']'));
            held.clear();
            held_comma = false;
        }
        if ch == '"' {
            in_string = true;
        }
        out.push(ch);
    }
    if held_comma {
        flush_held_commas(&mut out, &held, true);
    }
    out
}

/// Emit a held comma/whitespace run: dropping its commas when it trails
/// before a closer or the end of input, keeping it verbatim otherwise.
fn flush_held_commas(out: &mut String, held: &str, drop_commas: bool) {
    if drop_commas {
        out.extend(held.chars().filter(|c| *c != ','));
    } else {
        out.push_str(held);
    }
}

/// Balance braces and brackets: count `{`/`}` and `[`/`]`, append closers if
/// positive delta (more opens than closes). Caps iterations so a
/// catastrophically broken input doesn't loop forever.
fn balance_braces(s: &str, max_iter: usize) -> String {
    let mut out = s.to_string();
    for _ in 0..max_iter {
        let brace_delta: i32 = out
            .chars()
            .map(|ch| match ch {
                '{' => 1,
                '}' => -1,
                _ => 0,
            })
            .sum();
        let bracket_delta: i32 = out
            .chars()
            .map(|ch| match ch {
                '[' => 1,
                ']' => -1,
                _ => 0,
            })
            .sum();
        if brace_delta <= 0 && bracket_delta <= 0 {
            break;
        }
        // Append needed closers in reverse order (brackets before braces
        // for correct nesting when both are unbalanced).
        for _ in 0..bracket_delta.max(0) {
            out.push(']');
        }
        for _ in 0..brace_delta.max(0) {
            out.push('}');
        }
    }
    out
}

/// Strip excess closers when the delta is negative (more closes than opens).
fn strip_excess_closers(s: &str) -> String {
    let mut brace_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '}' => {
                if brace_depth > 0 {
                    brace_depth -= 1;
                    out.push(ch);
                }
                // else drop excess closer
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                    out.push(ch);
                }
            }
            '{' => {
                brace_depth += 1;
                out.push(ch);
            }
            '[' => {
                bracket_depth += 1;
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_parse_passes_through() {
        let r = repair(r#"{"path": "hello.txt"}"#).unwrap();
        assert_eq!(r.value, json!({"path": "hello.txt"}));
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn repairs_trailing_comma() {
        let r = repair(r#"{"path": "hello.txt",}"#).unwrap();
        assert_eq!(r.value, json!({"path": "hello.txt"}));
        // Structurally complete, just sloppy — must stay dispatchable.
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn repairs_trailing_comma_in_array() {
        let r = repair(r#"["a", "b",]"#).unwrap();
        assert_eq!(r.value, json!(["a", "b"]));
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn repairs_missing_close_brace() {
        let r = repair(r#"{"path": "hello.txt""#).unwrap();
        assert_eq!(r.value, json!({"path": "hello.txt"}));
        assert!(r.structure_synthesized);
    }

    #[test]
    fn repairs_missing_close_bracket() {
        let r = repair(r#"["a", "b""#).unwrap();
        assert_eq!(r.value, json!(["a", "b"]));
        assert!(r.structure_synthesized);
    }

    #[test]
    fn strips_embedded_control_chars() {
        // Raw \x0B (vertical tab) inside a string value
        let raw = "{\"key\": \"val\x0Bue\"}";
        let v = repair(raw).unwrap();
        assert_eq!(v.value, json!({"key": "value"}));
        assert!(!v.structure_synthesized);
    }

    #[test]
    fn rejects_empty_string() {
        assert!(matches!(repair(""), Err(ArgRepairError::Unrepairable)));
    }

    #[test]
    fn rejects_gibberish() {
        assert!(matches!(
            repair("not json at all"),
            Err(ArgRepairError::Unrepairable)
        ));
    }

    #[test]
    fn balances_nested_braces() {
        let r = repair(r#"{"outer": {"inner": "val""#).unwrap();
        assert_eq!(r.value, json!({"outer": {"inner": "val"}}));
        // Closers were appended, so this is a truncated argument, not a
        // complete one that merely needed tidying.
        assert!(r.structure_synthesized);
    }

    #[test]
    fn strips_excess_closers() {
        let r = repair(r#"{"key": "val"}}"#).unwrap();
        assert_eq!(r.value, json!({"key": "val"}));
        assert!(r.structure_synthesized);
    }

    #[test]
    fn handles_double_encoded_json() {
        // This is a valid JSON string containing a JSON object literal.
        // repair parses it as a string; the engine's existing fallback
        // (parse_tool_input) will unwrap the string and re-parse.
        let r = repair(r#""{\"path\": \"hello.txt\"}""#).unwrap();
        assert_eq!(
            r.value,
            Value::String(r#"{"path": "hello.txt"}"#.to_string())
        );
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn oversize_input_rejected() {
        let big = "x".repeat(MAX_ARG_LEN + 1);
        assert!(repair(&big).is_err());
    }

    #[test]
    fn oversize_valid_json_parses_intact() {
        // A valid argument over the repair bound whose string content holds
        // an unbalanced brace must come back exactly as written, not be
        // refused as too large or "repaired".
        let content = format!("fn main() {{{}", "x".repeat(MAX_ARG_LEN));
        let raw = serde_json::json!({"path": "big.rs", "content": content}).to_string();
        assert!(raw.len() > MAX_ARG_LEN);
        let r = repair(&raw).expect("valid oversize JSON parses");
        assert!(!r.structure_synthesized);
        assert_eq!(r.value["content"].as_str(), Some(content.as_str()));

        // Oversize text that is not valid JSON is still refused before any
        // repair stage runs.
        let truncated = &raw[..raw.len() - 2];
        assert!(matches!(
            repair(truncated),
            Err(ArgRepairError::TooLarge(_))
        ));
    }

    #[test]
    fn a_write_cut_at_a_string_boundary_is_reported_as_synthesized() {
        // The defect this flag exists for: `balance_braces` counts braces
        // without tracking string literals, so a provider that cuts the
        // stream at its output limit right after a complete string value
        // yields text that parses cleanly once one `}` is appended. Nothing
        // downstream could previously tell this from a finished argument, so
        // the truncated `content` was written to the user's file.
        let cut = r#"{"path": "notes.md", "content": "first line""#;
        let r = repair(cut).unwrap();
        assert_eq!(
            r.value,
            json!({"path": "notes.md", "content": "first line"}),
            "the ladder still parses it — that is exactly why the flag is needed"
        );
        assert!(
            r.structure_synthesized,
            "a truncated write must be reported as synthesized so dispatch refuses it"
        );
    }

    #[test]
    fn a_complete_argument_needing_only_control_char_stripping_stays_intact() {
        // Stage 2 normalizes text that was already structurally complete, so
        // it must NOT be flagged — otherwise every DeepSeek chunk-boundary
        // repair would start failing tool calls that are perfectly fine.
        let r = repair("{\"a\": \"line\u{0008}break\"}").unwrap();
        assert_eq!(r.value, json!({"a": "linebreak"}));
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn repairs_brace_balance_with_trailing_comma() {
        let r = repair(r#"{"a": 1,"#).unwrap();
        assert_eq!(r.value, json!({"a": 1}));
        assert!(r.structure_synthesized);
    }

    #[test]
    fn trailing_comma_repair_leaves_string_content_alone() {
        // Object-level trailing comma forces the repair path; the `,}` and
        // `,]` inside `content` are the model's code and must survive.
        let raw = r#"{"path":"a.js","content":"const o = {a:1,};\nlet v = [1,2,];\n",}"#;
        let r = repair(raw).unwrap();
        assert_eq!(
            r.value,
            json!({"path": "a.js", "content": "const o = {a:1,};\nlet v = [1,2,];\n"})
        );
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn a_long_comma_run_is_repaired_in_linear_time() {
        // A repetition loop of commas outside any string must not cost a
        // rescan per comma: repair runs on every streamed partial buffer.
        let mut raw = String::from(r#"{"a":[1"#);
        for _ in 0..200_000 {
            raw.push_str(", ");
        }
        raw.push_str("]}");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(repair(&raw).map(|r| r.value));
        });
        let repaired = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("trailing-comma repair must finish in linear time");
        assert_eq!(repaired.unwrap(), json!({"a": [1]}));
    }

    #[test]
    fn interior_comma_runs_are_left_for_the_parser_to_reject() {
        // Only commas that trail before a closer are removed; a doubled
        // separator between values is not invented away.
        assert!(repair(r#"{"a":[1,,2]}"#).is_err());
        let r = repair("{\"a\":[1 , ,\n]}").unwrap();
        assert_eq!(r.value, json!({"a": [1]}));
        assert!(!r.structure_synthesized);
    }

    #[test]
    fn trailing_comma_before_whitespace_and_closer_is_stripped() {
        let r = repair("{\"a\": [1, 2 ,\n ],\n}").unwrap();
        assert_eq!(r.value, json!({"a": [1, 2]}));
        assert!(!r.structure_synthesized);
    }
}
