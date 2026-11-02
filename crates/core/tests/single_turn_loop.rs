//! The workspace must contain exactly one turn loop.
//!
//! `crates/core` carried a placeholder `engine/` tree whose `Engine::run`
//! accepted `Op::SendMessage`, appended to a journal, and emitted
//! `TurnComplete { status: "completed" }` without ever contacting a model. It
//! had no callers, but its doc comments ("the real turn loop is wired here in
//! the next slice") were load-bearing for `docs/ARCHITECTURE.md`'s claim that
//! core owns the agent loop, and a reader could reasonably have built on it.
//!
//! This guard is deliberately a source scan rather than a type check: the thing
//! being prevented is a *second implementation*, which by definition would not
//! be reachable from the first.
//!
//! # What changed, and why (#6242)
//!
//! Until #6242 this scan matched a function *name* — `async fn run_turn`. A
//! name is not the rule. `acp_server.rs` grew a full second turn loop called
//! `run_agentic_prompt_turn`, and the guard stayed green for as long as it
//! existed; nobody picked that name to evade anything, which is exactly the
//! problem. A guard satisfied by spelling is satisfied by accident.
//!
//! So the scan now matches the *shape*. A turn loop is a loop whose body does
//! all three of these in one iteration:
//!
//! 1. **drives a model** — calls something that opens or consumes a provider
//!    message stream / completion: an identifier ending in `stream`,
//!    `create_message`, `completion`, or `complete`; *starting* with
//!    `create_message` (the `LlmClient` method family — `create_message`,
//!    `create_message_stream`, `create_message_boxed`, …); or spelled
//!    `request_…model…` (a wrapper that requests a model response);
//! 2. **dispatches tool calls** — calls something that executes tools
//!    (`execute_*tool*`, `dispatch_*tool*`, `run_*tool*`, …), or runs
//!    model-written code in a REPL/kernel (`repl.run(…)`, `kernel.execute(…)`)
//!    — a code round is a tool round whatever the executor is called;
//! 3. **assembles its own prompt** — pushes onto a message history
//!    (`…messages…`/`…history…`/`…conversation…`/`…prompt…`.`push`/`extend`).
//!
//! Anything doing all three per iteration *is* a turn loop, whatever it is
//! called. Renaming it does not hide it; only [`ALLOWED_TURN_LOOPS`] does, and
//! that list is read back at the end of the test so a stale entry fails too.
//!
//! # #6511: suffix-only matching was a spelling guard too
//!
//! After #6242 the model-call marker was still a *suffix* list, so two loops
//! passed by spelling: the sub-agent loop calls
//! `request_subagent_model_response_with_retries(…)` and the RLM loop calls
//! `client.create_message_boxed(…)` and runs its code rounds through
//! `repl.run(…)`. CI said "exactly one turn loop" while three existed. The
//! markers cover both spellings. Both the child and RLM producer/code loops
//! now enter the canonical Engine; neither has a migration exception.
//!
//! # Known limitations
//!
//! - It is a lexical scan, not a type check. A turn loop that reaches the
//!   provider through an indirection matching none of the markers above, or
//!   that never mutates a message history, would not be seen. The markers are
//!   deliberately broad rather than exact for that reason, and
//!   [`detector_sees_a_renamed_turn_loop`] pins the detector against going
//!   vacuously blind.
//! - `#[cfg(test)]` modules and `tests/`, `benches/`, `examples/` sources are
//!   skipped: a test harness that drives rounds is not a shipped turn loop.
//! - It reports one loop per (file, enclosing function). A function with two
//!   turn loops in it is one finding, not two.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A loop that is permitted to drive model/tool rounds, and the reason.
///
/// Every entry must be *named*, *documented*, and *reachable* — an entry whose
/// loop no longer exists fails the test, so an exception has to be consciously
/// renewed instead of quietly outliving its reason.
struct AllowedTurnLoop {
    /// Workspace-relative path, `/`-separated.
    path: &'static str,
    /// Name of the function that lexically encloses the loop.
    owner: &'static str,
    /// Why this loop is allowed to exist. Exceptions cite their issue.
    why: &'static str,
}

const ALLOWED_TURN_LOOPS: &[AllowedTurnLoop] = &[AllowedTurnLoop {
    path: "crates/engine/src/core/engine/turn_loop.rs",
    owner: "run_turn",
    why: "THE turn loop. `Engine::run_turn` is the single agent loop; \
              docs/ARCHITECTURE.md and AGENTS.md both name it as the owner.",
}];

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// A call to an identifier ending in one of these, i.e. "this iteration talks
/// to a model".
const MODEL_CALL_SUFFIXES: &[&str] = &["stream", "create_message", "completion", "complete"];

/// A call to an identifier *starting* with one of these also talks to a model:
/// the `LlmClient` method family (`create_message_boxed`, …) is spelled by
/// prefix, not suffix (#6511).
const MODEL_CALL_PREFIXES: &[&str] = &["create_message"];

/// Receiver-name fragments for "this iteration runs model-written code":
/// `repl.run(…)` / `kernel.execute(…)` is a tool round by another name.
const CODE_RUNNER_RECEIVER_FRAGMENTS: &[&str] = &["repl", "kernel"];

/// Prefix/infix pairs for "this iteration dispatches tool calls".
const TOOL_DISPATCH_VERBS: &[&str] = &[
    "execute", "dispatch", "run", "invoke", "perform", "handle", "call",
];

/// Receiver-name fragments for "this iteration assembles its own prompt".
const HISTORY_RECEIVER_FRAGMENTS: &[&str] = &["messages", "history", "conversation", "prompt"];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TurnLoopSite {
    path: String,
    owner: String,
    line: usize,
    keyword: String,
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Blank out comments, string literals, and char literals, preserving byte
/// offsets and newlines. Brace matching is only trustworthy over source that
/// cannot contain a `{` inside a comment or a `"{"` literal.
fn sanitize(src: &str) -> String {
    let bytes: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = bytes.clone();
    let blank = |out: &mut Vec<char>, from: usize, to: usize| {
        for slot in out.iter_mut().take(to.min(bytes.len())).skip(from) {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    };
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        let next = bytes.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            let start = i;
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            blank(&mut out, start, i);
        } else if c == '/' && next == Some('*') {
            let start = i;
            let mut depth = 0usize;
            while i < bytes.len() {
                if bytes[i] == '/' && bytes.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == '*' && bytes.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            blank(&mut out, start, i);
        } else if c == 'r' && matches!(next, Some('"') | Some('#')) {
            // Raw string `r"..."` / `r#"..."#`, but not the identifier `red`.
            let mut hashes = 0usize;
            let mut j = i + 1;
            while bytes.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if bytes.get(j) != Some(&'"') || (i > 0 && is_ident_char(bytes[i - 1])) {
                i += 1;
                continue;
            }
            let start = i;
            j += 1;
            loop {
                if j >= bytes.len() {
                    break;
                }
                if bytes[j] == '"' {
                    let closing = (1..=hashes).all(|k| bytes.get(j + k) == Some(&'#'));
                    if closing {
                        j += hashes + 1;
                        break;
                    }
                }
                j += 1;
            }
            i = j;
            blank(&mut out, start, i);
        } else if c == '"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == '\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            blank(&mut out, start, i);
        } else if c == '\'' {
            // `'a'` / `'\n'` are char literals; `'a` alone is a lifetime.
            let is_char_lit = if bytes.get(i + 1) == Some(&'\\') {
                true
            } else {
                bytes.get(i + 2) == Some(&'\'')
            };
            if is_char_lit {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == '\'' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                blank(&mut out, start, i);
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out.into_iter().collect()
}

/// Strip `#[cfg(test)]`-gated items. A test harness that drives rounds is not
/// a shipped turn loop.
fn strip_cfg_test(src: &str) -> String {
    let chars: Vec<char> = sanitize(src).chars().collect();
    let mut out: Vec<char> = src.chars().collect();
    let mut search_from = 0usize;
    let needle = "cfg(test)";
    while let Some(rel) = src[search_from..].find(needle) {
        let at = search_from + rel;
        search_from = at + needle.len();
        // Require a `#[` immediately before (allowing whitespace).
        let prefix = &src[..at];
        let Some(hash) = prefix.rfind("#[") else {
            continue;
        };
        if !prefix[hash + 2..].trim().is_empty() {
            continue;
        }
        let hash_ci = src[..hash].chars().count();
        let mut i = src[..search_from].chars().count();
        // Skip the cfg attribute's close, then find this item's boundary.
        // Commas end gated struct fields/initializers, while commas inside a
        // function signature or generic type do not. Braces in strings and
        // comments were already blanked by the same existing sanitizer.
        while i < chars.len() && chars[i] != ']' {
            i += 1;
        }
        i += 1;
        let mut nesting = 0usize;
        let mut angles = 0usize;
        while i < chars.len() {
            match chars[i] {
                '(' | '[' => nesting += 1,
                ')' | ']' => nesting = nesting.saturating_sub(1),
                '<' if nesting == 0 => angles += 1,
                '>' if nesting == 0 => angles = angles.saturating_sub(1),
                '{' | ';' | ',' if nesting == 0 && angles == 0 => break,
                _ => {}
            }
            i += 1;
        }
        if i >= chars.len() {
            continue;
        }
        if matches!(chars[i], ';' | ',') {
            for slot in out.iter_mut().take(i + 1).skip(hash_ci) {
                if *slot != '\n' {
                    *slot = ' ';
                }
            }
            continue;
        }
        let mut depth = 0usize;
        let mut j = i;
        while j < chars.len() {
            if chars[j] == '{' {
                depth += 1;
            } else if chars[j] == '}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            j += 1;
        }
        for slot in out.iter_mut().take((j + 1).min(chars.len())).skip(hash_ci) {
            if *slot != '\n' {
                *slot = ' ';
            }
        }
    }
    out.into_iter().collect()
}

/// Every `loop`/`for … in …`/`while …` block, as (keyword, body char range).
fn loop_bodies(chars: &[char]) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !is_ident_char(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_ident_char(chars[i]) {
            i += 1;
        }
        if start > 0 && is_ident_char(chars[start - 1]) {
            continue;
        }
        let word: String = chars[start..i].iter().collect();
        if word != "loop" && word != "for" && word != "while" {
            continue;
        }
        // Walk to the body `{`, tracking paren/bracket depth so `for x in v[0] {`
        // and `while let Some(x) = it.next() {` resolve correctly.
        let mut j = i;
        let mut nesting = 0i32;
        let mut body_open = None;
        let mut saw_in = false;
        while j < chars.len() {
            match chars[j] {
                '(' | '[' => nesting += 1,
                ')' | ']' => nesting -= 1,
                ';' if nesting == 0 => break,
                '{' if nesting == 0 => {
                    body_open = Some(j);
                    break;
                }
                _ => {}
            }
            if word == "for"
                && nesting == 0
                && chars[j] == 'i'
                && chars.get(j + 1) == Some(&'n')
                && !is_ident_char(chars[j - 1])
                && chars.get(j + 2).is_some_and(|c| !is_ident_char(*c))
            {
                saw_in = true;
            }
            j += 1;
        }
        let Some(open) = body_open else { continue };
        // `impl Trait for Type {` and `for<'a> Fn(..)` are the `for` keyword
        // without a loop. A `for` loop always has an `in` before its body.
        if word == "for" && !saw_in {
            continue;
        }
        let mut depth = 0usize;
        let mut k = open;
        while k < chars.len() {
            if chars[k] == '{' {
                depth += 1;
            } else if chars[k] == '}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            k += 1;
        }
        found.push((word, open, k.min(chars.len().saturating_sub(1))));
    }
    found
}

/// Every identifier in `body` that is called as a function or method, i.e.
/// immediately followed (modulo whitespace) by `(`. Macros (`name!(`) and
/// bare parentheses are not identifiers and are skipped.
fn called_identifiers(body: &str) -> impl Iterator<Item = &str> {
    body.match_indices('(').filter_map(|(at, _)| {
        let before = body[..at].trim_end();
        let start = before
            .rfind(|c: char| !is_ident_char(c))
            .map_or(0, |i| i + 1);
        let ident = &before[start..];
        (!ident.is_empty() && !ident.starts_with(|c: char| c.is_ascii_digit())).then_some(ident)
    })
}

/// True when `ident` names something that requests a model response.
fn is_model_call(ident: &str) -> bool {
    MODEL_CALL_SUFFIXES
        .iter()
        .any(|suffix| ident.ends_with(suffix))
        || MODEL_CALL_PREFIXES
            .iter()
            .any(|prefix| ident.starts_with(prefix))
        || (ident.starts_with("request_") && ident.contains("model"))
}

/// True when `body` calls something that drives a model.
fn drives_a_model(body: &str) -> bool {
    called_identifiers(body).any(is_model_call)
}

/// The identifier a `.method(` call at byte `at` is invoked on, when the
/// receiver is a plain identifier (`messages.push(`, `repl.run(`).
fn method_receiver(body: &str, at: usize) -> Option<&str> {
    let before = body[..at].trim_end().strip_suffix('.')?;
    let receiver_end = before.trim_end();
    let ident_start = receiver_end
        .rfind(|c: char| !is_ident_char(c))
        .map_or(0, |i| i + 1);
    Some(&receiver_end[ident_start..])
}

/// True when `body` calls `.method(` on a receiver whose name contains one of
/// `fragments`.
fn calls_method_on(body: &str, methods: &[&str], fragments: &[&str]) -> bool {
    methods.iter().any(|method| {
        body.match_indices(method).any(|(at, _)| {
            let after = &body[at + method.len()..];
            if !after.trim_start().starts_with('(') {
                return false;
            }
            method_receiver(body, at)
                .is_some_and(|receiver| fragments.iter().any(|f| receiver.contains(f)))
        })
    })
}

/// True when `body` calls something like `execute_tool_calls(…)`, or runs
/// model-written code on a REPL/kernel (`repl.run(…)`).
fn dispatches_tool_calls(body: &str) -> bool {
    let named_tool_dispatch = TOOL_DISPATCH_VERBS.iter().any(|verb| {
        body.match_indices(verb).any(|(at, _)| {
            if at > 0 && is_ident_char(body[..at].chars().next_back().unwrap_or(' ')) {
                return false;
            }
            let rest = &body[at + verb.len()..];
            if !rest.starts_with('_') {
                return false;
            }
            let ident_end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
            rest[..ident_end].contains("tool")
        })
    });
    named_tool_dispatch
        || calls_method_on(body, &["run", "execute"], CODE_RUNNER_RECEIVER_FRAGMENTS)
}

/// True when `body` pushes onto something whose name reads like a prompt or
/// message history — the loop assembling its own conversation.
fn assembles_prompt_history(body: &str) -> bool {
    calls_method_on(body, &["push", "extend"], HISTORY_RECEIVER_FRAGMENTS)
}

/// Name of the function that lexically encloses char offset `at`.
fn enclosing_fn(chars: &[char], at: usize) -> String {
    let head: String = chars[..at].iter().collect();
    let mut best = None;
    let mut search = 0usize;
    while let Some(rel) = head[search..].find("fn ") {
        let idx = search + rel;
        search = idx + 3;
        let before_ok = idx == 0 || !is_ident_char(head[..idx].chars().next_back().unwrap_or(' '));
        if !before_ok {
            continue;
        }
        let rest = head[idx + 3..].trim_start();
        let name_end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
        if name_end > 0 {
            best = Some(rest[..name_end].to_string());
        }
    }
    best.unwrap_or_else(|| "<unknown>".to_string())
}

/// A resolved local method, including the syntactic impl receiver. Matching
/// `self.phase()` by its spelling alone would join unrelated impls.
#[derive(Clone)]
struct LocalMethod {
    receiver: String,
    name: String,
    body: String,
    open: usize,
    close: usize,
}

fn matching_brace(chars: &[char], open: usize) -> usize {
    let mut depth = 0usize;
    for (at, ch) in chars.iter().enumerate().skip(open) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return at;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced source block at {open}");
}

/// Reuse the sanitized, balanced source rather than phase names or markers.
/// Only inherent impls with literal receiver names are resolved. Unknown or
/// ambiguous calls are refused when needed, rather than guessed across types.
fn local_methods(prepared: &str) -> Vec<LocalMethod> {
    let chars: Vec<char> = prepared.chars().collect();
    let mut methods = Vec::new();
    for (byte, _) in prepared.match_indices("impl ") {
        if byte > 0 && is_ident_char(prepared[..byte].chars().next_back().unwrap()) {
            continue;
        }
        let start = prepared[..byte].chars().count();
        let Some(open_rel) = chars[start..].iter().position(|c| *c == '{') else {
            continue;
        };
        let open = start + open_rel;
        let header: String = chars[start + 5..open].iter().collect();
        // Trait impls and generic/path receivers require more resolution than
        // this literal local graph provides; direct shape detection remains.
        let receiver = header.trim();
        if receiver.is_empty() || !receiver.chars().all(is_ident_char) {
            continue;
        }
        let close = matching_brace(&chars, open);
        let impl_body: String = chars[open + 1..close].iter().collect();
        for (fn_byte, _) in impl_body.match_indices("fn ") {
            if fn_byte > 0 && is_ident_char(impl_body[..fn_byte].chars().next_back().unwrap()) {
                continue;
            }
            let rest = &impl_body[fn_byte + 3..];
            let name_end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
            if name_end == 0 {
                continue;
            }
            let fn_start = open + 1 + impl_body[..fn_byte].chars().count();
            let Some(body_rel) = chars[fn_start..close].iter().position(|c| *c == '{') else {
                continue;
            };
            let body_open = fn_start + body_rel;
            let signature: String = chars[fn_start..body_open].iter().collect();
            if signature.contains(';')
                || !signature
                    .split(|c: char| !is_ident_char(c))
                    .any(|v| v == "self")
            {
                continue;
            }
            let body_close = matching_brace(&chars, body_open);
            methods.push(LocalMethod {
                receiver: receiver.to_string(),
                name: rest[..name_end].to_string(),
                body: chars[body_open..=body_close].iter().collect(),
                open: body_open,
                close: body_close,
            });
        }
    }
    methods
}

type MethodGraph = BTreeMap<(String, String), Vec<String>>;

fn method_graph(sources: &[String]) -> MethodGraph {
    let mut graph = MethodGraph::new();
    for (index, source) in sources.iter().enumerate() {
        let prepared = sanitize(&strip_cfg_test(source));
        let compact: String = prepared.chars().filter(|c| !c.is_whitespace()).collect();
        let tokens: Vec<&str> = prepared
            .split(|c: char| !is_ident_char(c))
            .filter(|s| !s.is_empty())
            .collect();
        for method in local_methods(&prepared) {
            if index + 1 != sources.len() {
                // A child contributes to its parent's inherent impl only
                // through a literal parent binding, with no local same-named
                // type. Unrelated imports/local types are scanned at their
                // own file but cannot impersonate the parent's phase owner.
                let parent_binding = compact.contains("usesuper::*;")
                    || compact.contains(&format!("usesuper::{};", method.receiver));
                let local_type = tokens.windows(2).any(|pair| {
                    matches!(pair[0], "struct" | "enum" | "type" | "union")
                        && pair[1] == method.receiver
                });
                if !parent_binding || local_type {
                    continue;
                }
            }
            graph
                .entry((method.receiver, method.name))
                .or_default()
                .push(method.body);
        }
    }
    graph
}

/// A helper's own loops are independently scanned at their lexical owner.
/// Do not transfer a delegated loop's effects into its caller and thereby
/// conceal a second owner. Ordinary blocks/async expressions stay intact.
fn without_owned_loops(body: &str) -> String {
    let mut chars: Vec<char> = body.chars().collect();
    for (_, open, close) in loop_bodies(&chars) {
        for ch in &mut chars[open..=close] {
            if *ch != '\n' {
                *ch = ' ';
            }
        }
    }
    chars.into_iter().collect()
}

/// Once a `self` call resolves locally, its method name is not evidence of a
/// provider call: inspect its real body. For example an actual snapshot
/// `*_before_complete` helper must not turn a human-operation dispatcher into
/// a model loop merely because it has the old broad suffix. Unresolved calls
/// keep the original conservative marker rule.
fn without_resolved_model_names(body: &str, receiver: &str, graph: &MethodGraph) -> String {
    let mut edits = Vec::new();
    for (at, _) in body.match_indices('(') {
        let before = body[..at].trim_end();
        let start = before
            .rfind(|c: char| !is_ident_char(c))
            .map_or(0, |i| i + 1);
        let name = &before[start..];
        if is_model_call(name)
            && method_receiver(body, start) == Some("self")
            && graph.contains_key(&(receiver.to_string(), name.to_string()))
        {
            edits.push((start, start + name.len()));
        }
    }
    let mut result = body.as_bytes().to_vec();
    for (start, end) in edits {
        result[start..end].fill(b' ');
    }
    String::from_utf8(result).unwrap()
}

fn resolved_loop_body(
    body: &str,
    receiver: &str,
    graph: &MethodGraph,
    delegates: &BTreeSet<(String, String)>,
) -> (String, Vec<String>) {
    let mut resolved = without_resolved_model_names(body, receiver, graph);
    let mut pending = vec![body.to_string()];
    let mut visited = BTreeSet::new();
    let mut ambiguous = Vec::new();
    while let Some(current) = pending.pop() {
        for (at, _) in current.match_indices('(') {
            let before = current[..at].trim_end();
            let name_start = before
                .rfind(|c: char| !is_ident_char(c))
                .map_or(0, |i| i + 1);
            let name = &before[name_start..];
            if method_receiver(&current, name_start) != Some("self")
                || !visited.insert(name.to_string())
            {
                continue;
            }
            let key = (receiver.to_string(), name.to_string());
            if delegates.contains(&key) {
                continue;
            }
            let Some(bodies) = graph.get(&key) else {
                continue; // external/parent methods cannot be invented here
            };
            if bodies.len() != 1 {
                ambiguous.push(format!("{receiver}::{name}"));
            }
            // Conditional platform impls may have several definitions. Union
            // their possible effects conservatively, then refuse ambiguity
            // only if those effects could constitute a turn loop.
            for body in bodies {
                let contribution = without_owned_loops(body);
                resolved.push_str(&without_resolved_model_names(
                    &contribution,
                    receiver,
                    graph,
                ));
                pending.push(contribution);
            }
        }
    }
    (resolved, ambiguous)
}

/// Entry dispatchers that call an independently owned turn loop are
/// delegation, not phases. Find those owners by the same shape rule, then
/// propagate only this boundary through actual same-receiver call edges.
fn delegated_loop_boundaries(graph: &MethodGraph) -> BTreeSet<(String, String)> {
    let mut boundaries = BTreeSet::new();
    for (key, bodies) in graph {
        for body in bodies {
            let chars: Vec<char> = body.chars().collect();
            for (_, open, close) in loop_bodies(&chars) {
                let direct: String = chars[open..=close].iter().collect();
                let (resolved, _) = resolved_loop_body(&direct, &key.0, graph, &BTreeSet::new());
                if drives_a_model(&resolved)
                    && dispatches_tool_calls(&resolved)
                    && assembles_prompt_history(&resolved)
                {
                    boundaries.insert(key.clone());
                }
            }
        }
    }
    loop {
        let before = boundaries.len();
        for (key, bodies) in graph {
            if bodies.iter().any(|body| {
                called_identifiers(body).any(|name| {
                    body.match_indices(name)
                        .any(|(at, _)| method_receiver(body, at) == Some("self"))
                        && boundaries.contains(&(key.0.clone(), name.to_string()))
                })
            }) {
                boundaries.insert(key.clone());
            }
        }
        if boundaries.len() == before {
            return boundaries;
        }
    }
}

/// Find every turn loop by shape, following only resolved same-receiver local
/// calls in its declared source closure. No method name is an exemption.
fn detect_turn_loops_with_graph(
    src: &str,
    rel_path: &str,
    graph: &MethodGraph,
) -> Vec<TurnLoopSite> {
    let prepared = sanitize(&strip_cfg_test(src));
    let methods = local_methods(&prepared);
    let delegates = delegated_loop_boundaries(graph);
    let chars: Vec<char> = prepared.chars().collect();
    let mut sites: Vec<TurnLoopSite> = Vec::new();
    for (keyword, open, close) in loop_bodies(&chars) {
        let direct: String = chars[open..=close.max(open)].iter().collect();
        let (body, ambiguous) = methods
            .iter()
            .find(|m| m.open <= open && close <= m.close)
            .map_or_else(
                || (direct.clone(), Vec::new()),
                |m| resolved_loop_body(&direct, &m.receiver, graph, &delegates),
            );
        if !(drives_a_model(&body)
            && dispatches_tool_calls(&body)
            && assembles_prompt_history(&body))
        {
            continue;
        }
        assert!(
            ambiguous.is_empty(),
            "ambiguous local turn-loop phase resolution: {ambiguous:?}"
        );
        let owner = enclosing_fn(&chars, open);
        let line = chars[..open].iter().filter(|c| **c == '\n').count() + 1;
        if sites.iter().any(|s| s.owner == owner) {
            continue;
        }
        sites.push(TurnLoopSite {
            path: rel_path.to_string(),
            owner,
            line,
            keyword,
        });
    }
    sites
}

fn detect_turn_loops(src: &str, rel_path: &str) -> Vec<TurnLoopSite> {
    detect_turn_loops_with_graph(src, rel_path, &method_graph(&[src.to_string()]))
}

/// Follow ordinary literal out-of-line modules, not every file in a directory.
/// This is a finite local closure. Test-only declarations are blanked first;
/// missing declared files fail instead of silently dropping a phase.
fn local_module_sources(file: &Path, seen: &mut BTreeSet<PathBuf>, out: &mut Vec<String>) {
    let file = file.canonicalize().expect("declared module source exists");
    if !seen.insert(file.clone()) {
        return;
    }
    let source = std::fs::read_to_string(&file).expect("read declared module source");
    let prepared = sanitize(&strip_cfg_test(&source));
    let base = if matches!(
        file.file_name().and_then(|n| n.to_str()),
        Some("lib.rs" | "main.rs" | "mod.rs")
    ) {
        file.parent().unwrap().to_path_buf()
    } else {
        file.with_extension("")
    };
    for (at, _) in prepared.match_indices("mod ") {
        if at > 0 && is_ident_char(prepared[..at].chars().next_back().unwrap()) {
            continue;
        }
        // Inline-module declarations need their own lexical directory and
        // are not folded into this file's local method namespace.
        if prepared[..at].chars().fold(0isize, |depth, ch| match ch {
            '{' => depth + 1,
            '}' => depth - 1,
            _ => depth,
        }) != 0
        {
            continue;
        }
        let rest = prepared[at + 4..].trim_start();
        let end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
        if end == 0 || !rest[end..].trim_start().starts_with(';') {
            continue;
        }
        // An explicit path is not guessed as a conventional module. Leave it
        // unresolved so the required owner's stale-entry assertion can fail.
        let line_prefix = prepared[..at]
            .rsplit_once([';', '{', '}'])
            .map_or(&prepared[..at], |(_, p)| p);
        if line_prefix.contains("path") {
            continue;
        }
        let named = base.join(format!("{}.rs", &rest[..end]));
        let directory = base.join(&rest[..end]).join("mod.rs");
        let child = match (named.is_file(), directory.is_file()) {
            (true, false) => named,
            (false, true) => directory,
            _ => panic!(
                "missing or ambiguous declared module {} in {}",
                &rest[..end],
                file.display()
            ),
        };
        local_module_sources(&child, seen, out);
    }
    out.push(source);
}

// ---------------------------------------------------------------------------
// Source discovery
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    // crates/core/tests -> crates/core -> crates -> <root>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root above crates/core")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name == ".git" || name == "node_modules" {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Shipped source only: test, bench, and example trees drive rounds on
/// purpose.
fn is_shipped_source(rel: &Path) -> bool {
    let components: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((file, dirs)) = components.split_last() else {
        return false;
    };
    if dirs
        .iter()
        .any(|d| d == "tests" || d == "benches" || d == "examples")
    {
        return false;
    }
    !(file == "tests.rs" || file.ends_with("_tests.rs"))
}

fn rel_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn workspace_declares_exactly_one_turn_loop() {
    let root = workspace_root();
    let crates = root.join("crates");
    assert!(crates.is_dir(), "expected {} to exist", crates.display());

    let mut files = Vec::new();
    rust_sources(&crates, &mut files);
    assert!(
        files.len() > 100,
        "source scan found too few files to trust"
    );

    let mut scanned = 0usize;
    let mut sites: Vec<TurnLoopSite> = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(&root).unwrap_or(file).to_path_buf();
        if !is_shipped_source(&rel) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        scanned += 1;
        let prepared = sanitize(&strip_cfg_test(&text));
        let chars: Vec<char> = prepared.chars().collect();
        let needs_local_closure = loop_bodies(&chars).iter().any(|(_, open, close)| {
            let body: String = chars[*open..=*close].iter().collect();
            body.contains("self")
        });
        if needs_local_closure {
            let mut sources = Vec::new();
            local_module_sources(file, &mut BTreeSet::new(), &mut sources);
            let resolved = std::panic::catch_unwind(|| {
                detect_turn_loops_with_graph(
                    &text,
                    &rel_slash(&root, file),
                    &method_graph(&sources),
                )
            })
            .unwrap_or_else(|_| panic!("local turn-loop resolution failed in {}", file.display()));
            sites.extend(resolved);
        } else {
            sites.extend(detect_turn_loops(&text, &rel_slash(&root, file)));
        }
    }
    assert!(
        scanned > 100,
        "only {scanned} shipped sources scanned — the exclusion rules are \
         swallowing the workspace, so a pass here proves nothing"
    );
    sites.sort();

    let allowed: BTreeSet<(&str, &str)> = ALLOWED_TURN_LOOPS
        .iter()
        .map(|entry| (entry.path, entry.owner))
        .collect();

    let unlisted: Vec<&TurnLoopSite> = sites
        .iter()
        .filter(|site| !allowed.contains(&(site.path.as_str(), site.owner.as_str())))
        .collect();
    assert!(
        unlisted.is_empty(),
        "found {} turn loop(s) that are not on ALLOWED_TURN_LOOPS:\n{}\n\n\
         A turn loop is a loop that, in one iteration, drives a model stream, \
         dispatches tool calls, and appends to its own prompt history. There \
         is supposed to be exactly one of those — `Engine::run_turn`. If you \
         are migrating the runtime, move the loop that exists instead of \
         adding another beside it. If this genuinely must exist for now, add \
         a named, documented ALLOWED_TURN_LOOPS entry citing the issue that \
         deletes it, so the exception is visible and has to be renewed.",
        unlisted.len(),
        unlisted
            .iter()
            .map(|s| format!(
                "  - {}:{} (`{}`, `{}` loop)",
                s.path, s.line, s.owner, s.keyword
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // The allowlist is read back: an entry whose loop is gone (or renamed, or
    // moved) fails, so no exception outlives its reason unnoticed.
    let detected: BTreeSet<(&str, &str)> = sites
        .iter()
        .map(|site| (site.path.as_str(), site.owner.as_str()))
        .collect();
    let stale: Vec<&AllowedTurnLoop> = ALLOWED_TURN_LOOPS
        .iter()
        .filter(|entry| !detected.contains(&(entry.path, entry.owner)))
        .collect();
    assert!(
        stale.is_empty(),
        "ALLOWED_TURN_LOOPS has {} entr(y/ies) with no matching turn loop in \
         the tree:\n{}\n\n\
         Either the loop was deleted (good — delete the entry too), or it \
         moved/was renamed (update the entry), or the detector stopped seeing \
         it, which is the #6242 failure all over again and must be fixed \
         rather than papered over.",
        stale.len(),
        stale
            .iter()
            .map(|e| format!("  - {} :: {} — {}", e.path, e.owner, e.why))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// The detector must find a turn loop it has never been told the name of.
///
/// This is the regression for #6242: the old guard matched `run_turn` and was
/// green for as long as `run_agentic_prompt_turn` existed. If the shape match
/// ever degrades back into a name match, this fails.
#[test]
fn detector_sees_a_renamed_turn_loop() {
    let src = r#"
        async fn absolutely_not_called_run_turn(&mut self) -> Result<()> {
            let mut messages = self.history.clone();
            loop {
                let stream = self.client.create_message_stream(request).await?;
                let (text, calls) = self.consume(stream).await?;
                messages.push(Message::assistant(text));
                if calls.is_empty() {
                    return Ok(());
                }
                let results = self.execute_tool_calls(calls).await?;
                messages.extend(results);
            }
        }
    "#;
    let sites = detect_turn_loops(src, "crates/whatever/src/sneaky.rs");
    assert_eq!(
        sites.len(),
        1,
        "shape detector missed a turn loop that avoids the name `run_turn`: {sites:#?}"
    );
    assert_eq!(sites[0].owner, "absolutely_not_called_run_turn");

    // …and must not fire on a loop that only does part of the job.
    let not_a_turn_loop = r#"
        async fn render(&mut self) -> Result<()> {
            for event in events {
                messages.push(event.text);
                self.paint(event);
            }
            Ok(())
        }
    "#;
    assert!(
        detect_turn_loops(not_a_turn_loop, "crates/whatever/src/ui.rs").is_empty(),
        "shape detector fired on a loop that never drives a model or tools"
    );
}

/// #6511: the two spellings that hid the sub-agent and RLM loops from the
/// suffix-only marker must both be seen.
#[test]
fn detector_sees_prefix_spelled_model_calls_and_repl_rounds() {
    // Sub-agent shape: a `request_…model…` wrapper plus a tool runner.
    let subagent = r#"
        async fn child_loop(&mut self) {
            loop {
                let api = request_subagent_model_response_with_retries(&client, request).await;
                messages.push(api.message);
                let output = run_tool_with_person_aware_timeout(call).await;
                messages.push(output);
            }
        }
    "#;
    let sites = detect_turn_loops(subagent, "crates/whatever/src/child.rs");
    assert_eq!(sites.len(), 1, "missed the sub-agent spelling: {sites:#?}");
    assert_eq!(sites[0].owner, "child_loop");

    // RLM shape: `create_message_boxed` plus a REPL code round.
    let rlm = r#"
        async fn recursive_loop(client: Arc<dyn Client>) {
            for iteration in 0..LIMIT {
                let response = client.create_message_boxed(request).await;
                let round = repl.run(&code, Some(&bridge)).await;
                messages.push(metadata(round));
            }
        }
    "#;
    let sites = detect_turn_loops(rlm, "crates/whatever/src/rlm.rs");
    assert_eq!(sites.len(), 1, "missed the RLM spelling: {sites:#?}");
    assert_eq!(sites[0].owner, "recursive_loop");

    // A REPL round with no model call is not a turn loop.
    let replay = r#"
        async fn replay(&mut self) {
            for block in blocks {
                let round = repl.run(&block.code, None).await;
                history.push(round.stdout);
            }
        }
    "#;
    assert!(
        detect_turn_loops(replay, "crates/whatever/src/replay.rs").is_empty(),
        "a REPL replay that never calls a model is not a turn loop"
    );
}

#[test]
fn core_does_not_reintroduce_a_placeholder_engine_module() {
    let core_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert!(
        !core_src.join("engine").exists(),
        "crates/core/src/engine/ is back. It was removed in v0.9.11 because it \
         emitted TurnComplete without calling a model and had no consumers; a \
         boundary type that does real work belongs in a named module, not a \
         second `engine`."
    );
}

#[test]
fn detector_follows_resolved_phases_across_declared_modules() {
    let root = r#"impl DifferentName { async fn outer(&mut self) {
        loop { self.request_phase().await; self.apply_phase().await; }
    } }"#;
    let request = r#"use super::*; impl DifferentName { async fn request_phase(&mut self) {
        messages.push(input); client.create_message_stream(request).await;
    } }"#;
    let apply = r#"use super::*; impl DifferentName { async fn apply_phase(&mut self) {
        self.execute_tool_calls(calls).await;
    } }"#;
    let graph = method_graph(&[request.into(), apply.into(), root.into()]);
    let sites = detect_turn_loops_with_graph(root, "arbitrary.rs", &graph);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].owner, "outer");
    // A missing phase cannot produce a green approximation of the owner.
    let missing = method_graph(&[request.into(), root.into()]);
    assert!(detect_turn_loops_with_graph(root, "arbitrary.rs", &missing).is_empty());
}

#[test]
fn detector_local_call_cycles_are_finite_and_do_not_join_unrelated_impls() {
    let src = r#"
        impl A {
            async fn renamed(&mut self) { loop { self.first().await; } }
            async fn first(&mut self) { self.second().await; }
            async fn second(&mut self) { self.first().await;
                client.create_message_stream(req).await;
                self.execute_tool_calls(calls).await; messages.push(msg);
            }
        }
        impl B { fn first(&mut self) { history.push(msg); } }
    "#;
    let sites = detect_turn_loops(src, "cycles.rs");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].owner, "renamed");
    let unrelated = r#"
        impl A { fn outer(&mut self) { loop { self.first(); } }
                 fn first(&mut self) { messages.push(msg); } }
        impl B { fn first(&mut self) {
            client.create_message_stream(req); execute_tool_calls(calls);
        } }
    "#;
    assert!(detect_turn_loops(unrelated, "unrelated.rs").is_empty());
}

#[test]
fn detector_does_not_hide_two_phased_owners_or_a_foreign_inline_loop() {
    let src = r#"impl Owner {
        fn one(&mut self) { loop { self.model(); self.results(); } }
        fn two(&mut self) { loop { self.model(); self.results(); } }
        fn model(&mut self) { client.create_message_stream(req); }
        fn results(&mut self) { execute_tool_calls(calls); history.push(msg); }
    }
    fn foreign() { loop { client.create_message_stream(req);
        execute_tool_calls(calls); history.push(msg); } }
    "#;
    let sites = detect_turn_loops(src, "more_than_one.rs");
    let owners: BTreeSet<&str> = sites.iter().map(|s| s.owner.as_str()).collect();
    assert_eq!(owners, BTreeSet::from(["one", "two", "foreign"]));
}

#[test]
fn detector_does_not_transfer_a_delegated_loop_to_its_caller() {
    let src = r#"impl Owner {
        fn transport(&mut self) { loop { self.other_owner(); } }
        fn other_owner(&mut self) { loop { self.request(); self.results(); } }
        fn request(&mut self) { client.create_message_stream(req); }
        fn results(&mut self) { execute_tool_calls(calls); history.push(msg); }
    }"#;
    let sites = detect_turn_loops(src, "delegated.rs");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].owner, "other_owner");
}

#[test]
fn detector_ignores_test_only_effects_and_refuses_ambiguous_methods() {
    let root = r#"impl Owner { fn outer(&mut self) { loop { self.phase(); } } }
        #[cfg(test)] impl Owner { fn phase(&mut self) {
            client.create_message_stream(req); execute_tool_calls(calls); messages.push(msg);
        } }"#;
    assert!(detect_turn_loops(root, "tests.rs").is_empty());
    let one = "use super::*; impl Owner { fn phase(&mut self) { client.create_message_stream(req); execute_tool_calls(calls); messages.push(msg); } }";
    let graph = method_graph(&[one.into(), one.into(), root.into()]);
    assert!(
        std::panic::catch_unwind(|| detect_turn_loops_with_graph(root, "ambiguous.rs", &graph))
            .is_err()
    );
}

#[test]
fn local_module_closure_reads_only_declared_production_sources() {
    let dir = std::env::temp_dir().join(format!(
        "cw-turn-phases-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("outer")).unwrap();
    let root = dir.join("outer.rs");
    std::fs::write(&root, "mod phase; #[cfg(test)] mod absent; impl Owner { fn outer(&mut self) { loop { self.phase(); } } }").unwrap();
    std::fs::write(dir.join("outer/phase.rs"), "use super::*; impl Owner { fn phase(&mut self) { client.create_message_stream(req); execute_tool_calls(calls); messages.push(msg); } }").unwrap();
    std::fs::write(
        dir.join("outer/undeclared.rs"),
        "impl Owner { fn phase(&mut self) { } }",
    )
    .unwrap();
    let mut sources = Vec::new();
    local_module_sources(&root, &mut BTreeSet::new(), &mut sources);
    assert_eq!(sources.len(), 2);
    assert_eq!(
        detect_turn_loops_with_graph(&sources[1], "outer.rs", &method_graph(&sources)).len(),
        1
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn gated_fields_and_initializers_do_not_unbalance_the_local_graph() {
    let src = r#"struct Owner {
        #[cfg(test)] hidden: std::collections::BTreeMap<String, String>,
        actual: bool,
    }
    impl Owner {
        fn new() -> Self { Self { #[cfg(test)] hidden: value, actual: true } }
        #[cfg(test)] fn fixture(&self, first: bool, second: bool) {
            let text = "{not a brace}";
        }
        fn outer(&mut self) { loop { self.phase(); } }
        fn phase(&mut self) { client.create_message_stream(req); execute_tool_calls(calls); history.push(msg); }
    }"#;
    let sites = detect_turn_loops(src, "field.rs");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].owner, "outer");
}

#[test]
fn resolved_completion_named_snapshot_helper_is_not_a_model_call() {
    let src = r#"impl Owner {
        fn operations(&mut self) { loop { self.snapshot_before_complete(); self.results(); } }
        fn snapshot_before_complete(&mut self) { persist_snapshot(); }
        fn results(&mut self) { execute_tool_calls(calls); history.push(msg); }
    }"#;
    assert!(detect_turn_loops(src, "completion.rs").is_empty());
    // An unresolved provider helper remains conservatively visible, and a
    // locally resolved provider helper is followed to its real client call.
    let unknown = src.replace(
        "fn snapshot_before_complete(&mut self) { persist_snapshot(); }",
        "",
    );
    assert_eq!(detect_turn_loops(&unknown, "unknown.rs").len(), 1);
    let actual = src.replace("persist_snapshot();", "client.create_message_stream(req);");
    assert_eq!(detect_turn_loops(&actual, "actual.rs").len(), 1);
}

#[test]
fn same_named_child_type_or_unrelated_import_cannot_impersonate_a_phase() {
    let root = "impl Owner { fn outer(&mut self) { loop { self.phase(); } } }";
    let effects = "impl Owner { fn phase(&mut self) { client.create_message_stream(req); execute_tool_calls(calls); history.push(msg); } }";
    let shadow = format!("use super::*; struct Owner; {effects}");
    let unrelated = format!("use unrelated::Owner; {effects}");
    for child in [shadow, unrelated, effects.to_string()] {
        let graph = method_graph(&[child, root.to_string()]);
        assert!(detect_turn_loops_with_graph(root, "owner.rs", &graph).is_empty());
    }
    // False-green counterpart: an unrelated child's non-provider method
    // cannot erase a real unresolved model marker on the root receiver.
    let actual_root = "impl Owner { fn outer(&mut self) { loop { self.create_message_stream(req); execute_tool_calls(calls); history.push(msg); } } }";
    for child in [
        "use super::*; struct Owner; impl Owner { fn create_message_stream(&mut self) { } }",
        "use unrelated::Owner; impl Owner { fn create_message_stream(&mut self) { } }",
    ] {
        let graph = method_graph(&[child.to_string(), actual_root.to_string()]);
        assert_eq!(
            detect_turn_loops_with_graph(actual_root, "actual.rs", &graph).len(),
            1
        );
    }
    for import in ["use super::*;", "use super::Owner;"] {
        let child = format!("{import} {effects}");
        let graph = method_graph(&[child, root.to_string()]);
        assert_eq!(
            detect_turn_loops_with_graph(root, "owner.rs", &graph).len(),
            1
        );
    }
}

#[test]
fn rlm_cannot_reintroduce_client_only_authority_or_a_direct_provider_producer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("engine/src");
    for path in [
        "rlm/bridge.rs",
        "rlm/turn.rs",
        "tools/rlm.rs",
        "core/engine/rlm_host.rs",
    ] {
        let source = std::fs::read_to_string(root.join(path)).unwrap();
        let production = sanitize(&strip_cfg_test(&source));
        for retired in [
            "ModelClientRlmAdapter",
            "RlmLlmClient",
            "run_rlm_turn_impl",
            "create_message_boxed",
            "create_message_stream",
        ] {
            assert!(
                !production.contains(retired),
                "{path} reintroduced retired model authority {retired}"
            );
        }
    }
}
