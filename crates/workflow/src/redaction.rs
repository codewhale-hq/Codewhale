//! Redaction for anything that can reach a log, a span, or a durable receipt.
//!
//! Two classes of content are stripped before text is allowed to leave the
//! process — either onto a provider's wire or into a journal:
//!
//! 1. **Absolute paths.** `/Users/hunter/src/app`, `/home/x/...`, `C:\Users\…`
//!    and `~/…` carry the operator's username, home directory, and machine
//!    layout. A journal travels further than the machine that wrote it.
//! 2. **Repo-relative paths.** `crates/tui/src/main.rs`, `./deploy.sh`,
//!    `../../secret/notes.md` — and their escaped spellings, `crates\/tui\/…`
//!    and `crates\\tui\\…`. An absolute path discloses the machine; a relative
//!    one discloses the private tree's shape, which is exactly as much as the
//!    reader of a routing summary at another provider needs to reconstruct it.
//!    The rule is deliberately conservative — see `looks_relative` — because
//!    the failure it must not trade for is mangling ordinary prose or a
//!    `provider/model` label.
//! 3. **Secret-shaped tokens.** Provider keys, bearer tokens, and
//!    `SOMETHING_KEY=value` assignments. These have no business in a routing
//!    summary and must never be persisted next to one.
//!
//! Redaction is *recorded*, not silent: [`Redaction::kinds`] names what was
//! removed so a receipt can disclose the fact without disclosing the content.

use std::collections::BTreeSet;

/// A redaction kind, as it appears on a disclosure. These are stable labels —
/// receipts persist them.
pub const REDACTION_ABSOLUTE_PATH: &str = "absolute_path";
pub const REDACTION_RELATIVE_PATH: &str = "relative_path";
pub const REDACTION_SECRET: &str = "secret";

/// Placeholder substituted for a removed absolute path.
const PATH_PLACEHOLDER: &str = "<path>";
/// Placeholder substituted for a removed secret-shaped token.
const SECRET_PLACEHOLDER: &str = "<redacted>";

const SECRET_NAME_MARKERS: &[&str] = &[
    "api_key",
    "apikey",
    "secret",
    "token",
    "password",
    "passwd",
    "credential",
    "authorization",
    "auth_token",
    "access_key",
    "private_key",
    "session_key",
];

/// HTTP authorization scheme keywords, in their canonical HTTP capitalization.
///
/// These are *never* the secret — the secret is the token that follows them.
/// Redacting the keyword and stopping there is the failure this list exists to
/// prevent: `Authorization: Bearer <token>` would keep the token and still
/// claim on the receipt that a secret had been removed.
///
/// The capitalization is stored, not normalized away, because it carries
/// evidence: `Bearer` is HTTP syntax and `bearer` is an English noun. See
/// [`is_canonical_auth_scheme`].
const AUTH_SCHEMES: &[&str] = &["Bearer", "Basic", "Digest", "Token"];

/// The one scheme keyword whose canonical capitalization is, by itself, enough
/// to treat the following token as a credential.
///
/// The asymmetry is deliberate and is the whole of the false-positive story.
/// `Basic`, `Digest`, and `Token` are ordinary capitalized English words —
/// "Basic auth is enabled", "Token holders vote", "Digest the results" — and
/// arming on them would redact the next word of perfectly ordinary prose. `Bearer`
/// capitalized is, in a technical corpus, the HTTP scheme essentially every
/// time. So `Bearer qqq` loses `qqq` even though a three-letter lowercase token
/// looks like nothing at all, while the other three need header context first.
///
/// The residual cost is stated plainly: `Bearer tokens are rotated weekly`
/// redacts `tokens`. That is a capitalized-`Bearer` sentence, which is header
/// syntax by shape; ordinary prose says "bearer", and lowercase never arms on
/// its own.
const SELF_EVIDENT_AUTH_SCHEME: &str = "Bearer";

/// Minimum length before a bare token is treated as a credential value.
const CREDENTIAL_VALUE_MIN_LEN: usize = 16;

/// The result of redacting one string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction {
    text: String,
    kinds: BTreeSet<String>,
}

impl Redaction {
    /// The redacted text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Consume the redaction, yielding the redacted text.
    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }

    /// Whether anything was removed.
    #[must_use]
    pub fn redacted(&self) -> bool {
        !self.kinds.is_empty()
    }

    /// Which classes of content were removed — never the content itself.
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        self.kinds.iter().cloned().collect()
    }
}

/// How strongly the preceding tokens claim that the *next* token is a
/// credential.
///
/// A credential is routinely written as two or three tokens
/// (`Authorization: Bearer <token>`), so the decision cannot be made per token.
/// But the evidence for "a credential follows" is not uniform, and collapsing it
/// to a boolean is what produces either a leak or a mangled sentence:
///
/// - `Authorization: Bearer qqq` — the credential is `qqq`, three lowercase
///   letters, indistinguishable in shape from a word. Only the *context* says
///   it is a secret, and the context says so unambiguously.
/// - `authorization: bearer shares responsibility` — the same context markers,
///   lowercase, and the next token is an English verb. Redacting `shares` would
///   destroy a sentence and put a false `secret` kind on a receipt.
///
/// So the arming carries its own strength, and the shape test is applied only
/// where the context is weak enough to need it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialArm {
    /// Nothing is expected; the next token is judged on its own.
    None,
    /// The next token is the credential only if it also *looks* like one.
    /// Lowercase scheme words and bare `Authorization:` land here.
    IfShaped,
    /// The next token is the credential whatever it looks like. Reached by
    /// canonical HTTP capitalization — `Authorization: Bearer …`, or a bare
    /// `Bearer` — where the syntax alone settles it.
    Certain,
}

impl CredentialArm {
    const fn is_armed(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// What one token resolved to, and whether it armed the *next* token.
struct TokenOutcome {
    /// `None` keeps the token verbatim.
    text: Option<String>,
    /// Whether — and how strongly — the following token carries the credential
    /// this one introduced. `Authorization:` and a bare `Bearer` reveal nothing
    /// themselves; the value after them is the whole secret.
    arm: CredentialArm,
}

impl TokenOutcome {
    const fn keep() -> Self {
        Self {
            text: None,
            arm: CredentialArm::None,
        }
    }

    const fn keep_and_arm(arm: CredentialArm) -> Self {
        Self { text: None, arm }
    }

    fn replace(text: String) -> Self {
        Self {
            text: Some(text),
            arm: CredentialArm::None,
        }
    }

    fn replace_and_arm(text: String, arm: CredentialArm) -> Self {
        Self {
            text: Some(text),
            arm,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerMode {
    Array,
    Object,
    TupleOrMap,
}

impl ContainerMode {
    fn from_open(open: char) -> Option<Self> {
        match open {
            '[' => Some(Self::Array),
            '{' => Some(Self::Object),
            '(' => Some(Self::TupleOrMap),
            _ => None,
        }
    }

    const fn close(self) -> char {
        match self {
            Self::Array => ']',
            Self::Object => '}',
            Self::TupleOrMap => ')',
        }
    }

    const fn accepts_object_keys(self) -> bool {
        matches!(self, Self::Object | Self::TupleOrMap)
    }

    const fn expects_values(self) -> bool {
        matches!(self, Self::Array | Self::TupleOrMap)
    }
}

#[derive(Clone, Copy)]
struct ArmedContainer {
    mode: ContainerMode,
    arm: CredentialArm,
}

const MAX_CONTAINER_DEPTH: usize = 64;
const MAX_NESTED_ASSIGNMENT_DEPTH: u8 = 3;

fn push_container_if_opening(
    containers: &mut Vec<ArmedContainer>,
    open: char,
    arm: CredentialArm,
) -> CredentialArm {
    let Some(mode) = ContainerMode::from_open(open) else {
        return arm;
    };
    if containers.len() < MAX_CONTAINER_DEPTH {
        containers.push(ArmedContainer { mode, arm });
        if mode.expects_values() {
            arm
        } else {
            CredentialArm::None
        }
    } else {
        arm
    }
}

fn pop_containers_for(containers: &mut Vec<ArmedContainer>, text: &str) {
    for ch in text.chars() {
        if matches!(ch, ']' | '}' | ')')
            && let Some(pos) = containers.iter().rposition(|c| c.mode.close() == ch)
        {
            containers.truncate(pos);
        }
    }
}

fn container_restore_arm(containers: &[ArmedContainer]) -> CredentialArm {
    containers
        .last()
        .filter(|c| c.mode.expects_values())
        .map_or(CredentialArm::None, |c| c.arm)
}

fn is_container_object_key(token: &str) -> bool {
    let Some(key) = token.strip_suffix(':') else {
        return false;
    };
    !key.is_empty()
        && !key.ends_with("://")
        && !key.contains(['=', ':'])
        && !token.contains(STRUCTURAL_DELIMITERS)
        && !codewhale_sanitize::redact::contains_bare_secret_token(key)
}

fn trailing_secret_suffix(token: &str) -> (&str, &str) {
    let head = token.trim_end_matches([',', ']', '}', ')']);
    (head, &token[head.len()..])
}

/// Redact absolute paths and secret-shaped tokens from `input`.
///
/// Operates token-by-token over whitespace-free runs, which is enough for the
/// bounded, already whitespace-collapsed text this crate transmits and keeps
/// the rule set auditable without a regex dependency.
///
/// One piece of state crosses token boundaries, and it has to: a credential is
/// routinely written as *two* tokens (`Authorization: Bearer <token>`), so a
/// purely per-token rule either leaks the value or redacts the English word
/// `bearer`. Carrying "the next token is the credential" forward — and *how
/// certainly*, see `CredentialArm` — is what lets this do neither.
#[must_use]
pub fn redact_for_disclosure(input: &str) -> Redaction {
    let mut kinds = BTreeSet::new();
    let masked_keys = codewhale_sanitize::redact::mask_private_keys(input, SECRET_PLACEHOLDER);
    if masked_keys.is_some() {
        kinds.insert(REDACTION_SECRET.to_string());
    }
    let input = masked_keys.as_deref().unwrap_or(input);
    let (tokens, seps) = split_whitespace_preserving(input);
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut arm = CredentialArm::None;
    let mut containers: Vec<ArmedContainer> = Vec::new();
    let mut pending_name: Option<(&str, CredentialArm)> = None;
    let mut index = 0;

    while index < tokens.len() {
        let mut token = tokens[index];
        if seps[index].contains(['\n', '\r']) {
            pending_name = None;
        }
        let name = pending_name.take();
        if !arm.is_armed()
            && let Some(container) = containers.last().copied()
        {
            if token
                .chars()
                .all(|c| matches!(c, ']' | '}' | ')' | ',' | ';' | '"' | '\''))
            {
                pop_containers_for(&mut containers, token);
                arm = container_restore_arm(&containers);
                out.push(token.to_string());
                index += 1;
                continue;
            }
            if container.mode.accepts_object_keys() && is_container_object_key(token) {
                arm = container.arm;
                out.push(token.to_string());
                index += 1;
                continue;
            }
        }
        if arm.is_armed() {
            let lead_end = token
                .find(|c: char| !matches!(c, '[' | '{' | '('))
                .unwrap_or(token.len());
            let (lead_brackets, rest_token) = token.split_at(lead_end);
            let mut pushed_prefix = "";
            if !lead_brackets.is_empty()
                && rest_token
                    .chars()
                    .all(|c| matches!(c, '[' | '{' | '(' | '"' | '\''))
            {
                for ch in lead_brackets.chars() {
                    if ContainerMode::from_open(ch).is_some() {
                        arm = push_container_if_opening(&mut containers, ch, arm);
                    }
                }
                out.push(token.to_string());
                index += 1;
                continue;
            }
            if let Some(last_open) = lead_brackets
                .chars()
                .rev()
                .find_map(ContainerMode::from_open)
            {
                let close = last_open.close();
                if !rest_token.contains(close) {
                    for ch in lead_brackets.chars() {
                        if ContainerMode::from_open(ch).is_some() {
                            arm = push_container_if_opening(&mut containers, ch, arm);
                        }
                    }
                    pushed_prefix = lead_brackets;
                    token = rest_token;
                    if let Some(container) = containers.last().copied()
                        && container.mode.accepts_object_keys()
                        && is_container_object_key(token)
                    {
                        arm = container.arm;
                        out.push(format!("{pushed_prefix}{token}"));
                        index += 1;
                        continue;
                    }
                }
            }
            if token
                .chars()
                .all(|c| matches!(c, ']' | '}' | ')' | ',' | ';' | '"' | '\''))
            {
                pop_containers_for(&mut containers, token);
                arm = container_restore_arm(&containers);
                out.push(format!("{pushed_prefix}{token}"));
                index += 1;
                continue;
            }
            if let Some(container) = containers.last().copied()
                && container.mode.accepts_object_keys()
                && is_container_object_key(token)
            {
                arm = container.arm;
                out.push(format!("{pushed_prefix}{token}"));
                index += 1;
                continue;
            }
            // `Authorization: Bearer <token>` — the scheme keyword is not the
            // secret, so it survives and the arming carries past it. A
            // canonically capitalized keyword also *upgrades* the arming: the
            // header name alone left the shape in doubt, and `Bearer` settles
            // it.
            if is_auth_scheme(token) {
                if is_canonical_auth_scheme(token) {
                    arm = CredentialArm::Certain;
                }
                out.push(format!("{pushed_prefix}{token}"));
                index += 1;
                continue;
            }
            // Weak arming still defers to shape, so an `authorization:` that
            // introduces a sentence rather than a secret leaves the sentence
            // intact — and leaves the receipt honest about having removed
            // nothing.
            if arm == CredentialArm::Certain || looks_like_credential_value(token) {
                let (value_part, suffix) = trailing_secret_suffix(token);
                if !value_part.is_empty() {
                    kinds.insert(REDACTION_SECRET.to_string());
                    pop_containers_for(&mut containers, suffix);
                    arm = container_restore_arm(&containers);
                    out.push(format!("{pushed_prefix}{SECRET_PLACEHOLDER}{suffix}"));
                    index += 1;
                    continue;
                }
            }
            // Fall through: the token is judged on its own merits, and the
            // arming state is replaced wholesale by `redact_token` below.
        }

        if let Some((name, sensitivity)) = name
            && name.starts_with('-')
            && !token.starts_with(['-', '=', ':'])
            && (sensitivity == CredentialArm::Certain || looks_like_credential_value(token))
        {
            kinds.insert(REDACTION_SECRET.to_string());
            out.push(SECRET_PLACEHOLDER.to_string());
            index += 1;
            continue;
        }

        if let Some((name, sensitivity)) = name
            && let Some(rest) = token
                .strip_prefix([':', '='])
                .map(|rest| rest.trim_start_matches(['=', '>']))
        {
            let separator = &token[..token.len() - rest.len()];
            let weak = sensitivity == CredentialArm::IfShaped
                || name.eq_ignore_ascii_case("authorization");
            if rest.is_empty() {
                arm = if weak {
                    CredentialArm::IfShaped
                } else {
                    CredentialArm::Certain
                };
                out.push(token.to_string());
            } else if is_auth_scheme(rest) {
                arm = if is_canonical_auth_scheme(rest) {
                    CredentialArm::Certain
                } else {
                    CredentialArm::IfShaped
                };
                out.push(token.to_string());
            } else if !weak || looks_like_credential_value(rest) {
                kinds.insert(REDACTION_SECRET.to_string());
                arm = CredentialArm::None;
                out.push(format!("{separator}{SECRET_PLACEHOLDER}"));
            } else {
                arm = CredentialArm::None;
                out.push(token.to_string());
            }
            index += 1;
            continue;
        }

        let next = tokens.get(index + 1).copied();
        let outcome = redact_token(token, next, &mut kinds, &mut containers);
        arm = outcome.arm;
        if outcome.text.is_none() && !arm.is_armed() {
            let chunk = codewhale_sanitize::redact::last_escaped_chunk(token);
            let candidate = unwrap_token(chunk);
            if !candidate.contains(['=', ':']) {
                let sensitivity = name_sensitivity(candidate);
                if sensitivity.is_armed() {
                    pending_name = Some((candidate, sensitivity));
                }
            }
        }
        out.push(outcome.text.unwrap_or_else(|| token.to_string()));
        index += 1;
    }

    let mut text = String::with_capacity(input.len());
    text.push_str(seps[0]);
    for (idx, piece) in out.iter().enumerate() {
        text.push_str(piece);
        text.push_str(seps[idx + 1]);
    }
    Redaction { text, kinds }
}

fn split_whitespace_preserving(input: &str) -> (Vec<&str>, Vec<&str>) {
    let mut tokens = Vec::new();
    let mut seps = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ws_start = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        seps.push(&input[ws_start..i]);
        if i == bytes.len() {
            return (tokens, seps);
        }
        let tok_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        tokens.push(&input[tok_start..i]);
    }
    seps.push("");
    (tokens, seps)
}

fn name_sensitivity(name: &str) -> CredentialArm {
    match codewhale_sanitize::redact::key_sensitivity(name) {
        codewhale_sanitize::redact::KeySensitivity::Certain => return CredentialArm::Certain,
        codewhale_sanitize::redact::KeySensitivity::IfShaped => return CredentialArm::IfShaped,
        codewhale_sanitize::redact::KeySensitivity::None => {}
    }
    let norm = codewhale_sanitize::redact::normalize_sensitive_key(name);
    if SECRET_NAME_MARKERS
        .iter()
        .any(|marker| *marker != "token" && norm.contains(marker))
    {
        return CredentialArm::Certain;
    }
    CredentialArm::None
}

const STRUCTURAL_DELIMITERS: [char; 9] = [',', '(', ')', '[', ']', '{', '}', '&', '?'];

fn redact_backslash_chunk_workflow(
    chunk: &str,
    chunk_next: Option<&str>,
    pending_arm: &mut CredentialArm,
    kinds: &mut BTreeSet<String>,
    depth: u8,
) -> Option<String> {
    if pending_arm.is_armed() {
        let (core, lead_quote, trail_quote) = codewhale_sanitize::redact::split_value_quotes(chunk);
        if !core.is_empty()
            && (*pending_arm == CredentialArm::Certain || looks_like_credential_value(core))
        {
            *pending_arm = CredentialArm::None;
            kinds.insert(REDACTION_SECRET.to_string());
            return Some(format!("{lead_quote}{SECRET_PLACEHOLDER}{trail_quote}"));
        }
        *pending_arm = CredentialArm::None;
    }
    let outcome =
        redact_assignment_segment_at(chunk, chunk_next, kinds, CredentialArm::None, depth);
    *pending_arm = outcome.arm;
    outcome.text
}

fn next_escaped_chunk_slice(rest: &str) -> &str {
    let mut cursor = 0;
    while let Some(rel) = rest[cursor..].find('\\') {
        let idx = cursor + rel;
        if codewhale_sanitize::redact::escape_delimiter_end(rest, idx).is_some() {
            return &rest[..idx];
        }
        if rest[idx + 1..].starts_with('\\') {
            cursor = idx + 2;
        } else {
            cursor = idx + 1;
        }
    }
    rest
}

fn redact_workflow_backslash_assignments(
    text: &str,
    next: Option<&str>,
    kinds: &mut BTreeSet<String>,
    depth: u8,
) -> TokenOutcome {
    let mut out = String::new();
    let mut changed = false;
    let mut pending_arm = CredentialArm::None;
    let mut chunk_start = 0;
    let mut cursor = 0;
    while let Some(rel) = text[cursor..].find('\\') {
        let idx = cursor + rel;
        if let Some(delim_end) = codewhale_sanitize::redact::escape_delimiter_end(text, idx) {
            let chunk = &text[chunk_start..idx];
            let chunk_next = Some(next_escaped_chunk_slice(&text[delim_end..]));
            if !changed {
                if let Some(redacted) = redact_backslash_chunk_workflow(
                    chunk,
                    chunk_next,
                    &mut pending_arm,
                    kinds,
                    depth,
                ) {
                    out.reserve(text.len());
                    out.push_str(&text[..chunk_start]);
                    out.push_str(&redacted);
                    out.push_str(&text[idx..delim_end]);
                    changed = true;
                }
            } else {
                if let Some(redacted) = redact_backslash_chunk_workflow(
                    chunk,
                    chunk_next,
                    &mut pending_arm,
                    kinds,
                    depth,
                ) {
                    out.push_str(&redacted);
                } else {
                    out.push_str(chunk);
                }
                out.push_str(&text[idx..delim_end]);
            }
            chunk_start = delim_end;
            cursor = delim_end;
        } else if text[idx + 1..].starts_with('\\') {
            cursor = idx + 2;
        } else {
            cursor = idx + 1;
        }
    }
    if chunk_start == 0 {
        return TokenOutcome::keep();
    }
    let tail = &text[chunk_start..];
    if let Some(redacted) =
        redact_backslash_chunk_workflow(tail, next, &mut pending_arm, kinds, depth)
    {
        if !changed {
            out.reserve(text.len());
            out.push_str(&text[..chunk_start]);
        }
        out.push_str(&redacted);
        changed = true;
    } else if changed {
        out.push_str(tail);
    }
    if changed {
        TokenOutcome::replace_and_arm(out, pending_arm)
    } else if pending_arm.is_armed() {
        TokenOutcome::keep_and_arm(pending_arm)
    } else {
        TokenOutcome::keep()
    }
}

fn redact_assignment_segment(
    segment: &str,
    next: Option<&str>,
    kinds: &mut BTreeSet<String>,
    container_arm: CredentialArm,
) -> TokenOutcome {
    redact_assignment_segment_at(segment, next, kinds, container_arm, 0)
}

fn redact_assignment_segment_at(
    segment: &str,
    next: Option<&str>,
    kinds: &mut BTreeSet<String>,
    container_arm: CredentialArm,
    depth: u8,
) -> TokenOutcome {
    for separator in ['=', ':'] {
        let Some((name, value)) = segment.split_once(separator) else {
            continue;
        };
        let sensitivity = match name_sensitivity(name) {
            CredentialArm::None => container_arm,
            s => s,
        };
        if sensitivity.is_armed() {
            if is_auth_scheme(value) {
                return TokenOutcome::keep_and_arm(if is_canonical_auth_scheme(value) {
                    CredentialArm::Certain
                } else {
                    CredentialArm::IfShaped
                });
            }
            if value.is_empty() {
                let weak = sensitivity == CredentialArm::IfShaped
                    || name.eq_ignore_ascii_case("authorization");
                return if weak {
                    if next.is_some_and(|next| {
                        is_auth_scheme(next)
                            || looks_like_credential_value(next)
                            || next.starts_with(['[', '{', '('])
                    }) {
                        TokenOutcome::keep_and_arm(CredentialArm::IfShaped)
                    } else {
                        TokenOutcome::keep()
                    }
                } else if next.is_some() {
                    TokenOutcome::keep_and_arm(CredentialArm::Certain)
                } else {
                    TokenOutcome::keep()
                };
            }
            if sensitivity == CredentialArm::Certain || looks_like_credential_value(value) {
                kinds.insert(REDACTION_SECRET.to_string());
                return TokenOutcome::replace(format!("{name}{separator}{SECRET_PLACEHOLDER}"));
            }
        }
        if let Some(kind) = classify_path(value) {
            kinds.insert(kind.to_string());
            return TokenOutcome::replace(format!("{name}{separator}{PATH_PLACEHOLDER}"));
        }
        if depth < MAX_NESTED_ASSIGNMENT_DEPTH && !value.starts_with("//") {
            let (core, lead_quote, trail_quote) =
                codewhale_sanitize::redact::split_value_quotes(value);
            if core.contains('\\') {
                let nested = redact_workflow_backslash_assignments(core, next, kinds, depth + 1);
                if let Some(replaced) = nested.text {
                    return TokenOutcome::replace_and_arm(
                        format!("{name}{separator}{lead_quote}{replaced}{trail_quote}"),
                        nested.arm,
                    );
                }
                if nested.arm.is_armed() {
                    return TokenOutcome::keep_and_arm(nested.arm);
                }
            }
            if !core.contains("://") && core.contains(['=', ':']) {
                let nested =
                    redact_assignment_segment_at(core, next, kinds, CredentialArm::None, depth + 1);
                if let Some(replaced) = nested.text {
                    return TokenOutcome::replace_and_arm(
                        format!("{name}{separator}{lead_quote}{replaced}{trail_quote}"),
                        nested.arm,
                    );
                }
                if nested.arm.is_armed() {
                    return TokenOutcome::keep_and_arm(nested.arm);
                }
            }
        }
    }
    TokenOutcome::keep()
}

fn redact_structured_token(
    token: &str,
    next: Option<&str>,
    kinds: &mut BTreeSet<String>,
    containers: &mut Vec<ArmedContainer>,
) -> Option<TokenOutcome> {
    if !token.contains(STRUCTURAL_DELIMITERS) || !token.contains(['=', ':']) {
        return None;
    }
    let parts: Vec<(&str, &str)> = token
        .split_inclusive(STRUCTURAL_DELIMITERS)
        .map(|piece| match piece.chars().next_back() {
            Some(last) if STRUCTURAL_DELIMITERS.contains(&last) => {
                piece.split_at(piece.len() - last.len_utf8())
            }
            _ => (piece, ""),
        })
        .collect();
    let mut out = String::with_capacity(token.len());
    let mut changed = false;
    let mut seg_arm = CredentialArm::None;
    for (idx, &(segment, delim)) in parts.iter().enumerate() {
        let mut delim_handled = false;
        if !segment.is_empty() {
            if seg_arm.is_armed() {
                if is_auth_scheme(segment) {
                    if is_canonical_auth_scheme(segment) {
                        seg_arm = CredentialArm::Certain;
                    }
                    out.push_str(segment);
                } else if seg_arm == CredentialArm::Certain || looks_like_credential_value(segment)
                {
                    kinds.insert(REDACTION_SECRET.to_string());
                    out.push_str(SECRET_PLACEHOLDER);
                    changed = true;
                    seg_arm = CredentialArm::None;
                } else {
                    out.push_str(segment);
                    seg_arm = CredentialArm::None;
                }
            } else {
                let seg_next = parts[idx + 1..]
                    .iter()
                    .map(|&(seg, _)| seg)
                    .find(|seg| !seg.is_empty())
                    .or(next);
                let container_arm = containers
                    .last()
                    .filter(|c| c.mode.accepts_object_keys())
                    .map_or(CredentialArm::None, |c| c.arm);
                let outcome = redact_assignment_segment(segment, seg_next, kinds, container_arm);
                if let Some(replaced) = outcome.text {
                    out.push_str(&replaced);
                    changed = true;
                } else {
                    out.push_str(segment);
                }
                if outcome.arm.is_armed() {
                    seg_arm = outcome.arm;
                }
            }
        }
        if let Some(delim_char) = delim.chars().next() {
            if ContainerMode::from_open(delim_char).is_some()
                && (seg_arm.is_armed() || !containers.is_empty())
            {
                let push_arm = if seg_arm.is_armed() {
                    seg_arm
                } else {
                    containers.last().map_or(CredentialArm::None, |c| c.arm)
                };
                if push_arm.is_armed() {
                    seg_arm = push_container_if_opening(containers, delim_char, push_arm);
                    delim_handled = true;
                }
            } else if matches!(delim_char, ']' | '}' | ')') {
                pop_containers_for(containers, delim);
                seg_arm = container_restore_arm(containers);
                delim_handled = true;
            } else if delim_char == ',' && !seg_arm.is_armed() {
                seg_arm = container_restore_arm(containers);
                delim_handled = true;
            }
        }
        let _ = delim_handled;
        out.push_str(delim);
    }
    if changed {
        if let Some(masked) =
            codewhale_sanitize::redact::mask_bare_secret_tokens(&out, SECRET_PLACEHOLDER)
        {
            kinds.insert(REDACTION_SECRET.to_string());
            return Some(TokenOutcome {
                text: Some(masked),
                arm: seg_arm,
            });
        }
        return Some(TokenOutcome {
            text: Some(out),
            arm: seg_arm,
        });
    }
    if seg_arm.is_armed() {
        return Some(TokenOutcome::keep_and_arm(seg_arm));
    }
    None
}

/// Redact one whitespace-free token, recording what was removed.
///
/// `next` is the following non-empty token, used only to decide whether a bare
/// authorization scheme keyword is introducing a credential or is just a word.
fn redact_token(
    token: &str,
    next: Option<&str>,
    kinds: &mut BTreeSet<String>,
    containers: &mut Vec<ArmedContainer>,
) -> TokenOutcome {
    if let Some(outcome) = redact_structured_token(token, next, kinds, containers) {
        return outcome;
    }
    // `NAME=value` / `NAME:value` — a secret-bearing name redacts its value and
    // keeps the name, which is the useful half.
    let container_arm = containers
        .last()
        .filter(|c| c.mode.accepts_object_keys())
        .map_or(CredentialArm::None, |c| c.arm);
    let outcome = redact_assignment_segment(token, next, kinds, container_arm);
    if outcome.text.is_some() || outcome.arm.is_armed() {
        return outcome;
    }

    // A bare `Bearer` in canonical HTTP capitalization introduces a credential
    // on its own — that is what makes `Bearer qqq` lose `qqq`, which no shape
    // test could ever do. Any other scheme keyword, and any other
    // capitalization, needs something credential-shaped to actually follow:
    // `bearer of bad news` and `Token holders vote` are prose.
    if unwrap_token(token) == SELF_EVIDENT_AUTH_SCHEME {
        return TokenOutcome::keep_and_arm(CredentialArm::Certain);
    }
    if is_auth_scheme(token) && next.is_some_and(looks_like_credential_value) {
        return TokenOutcome::keep_and_arm(CredentialArm::IfShaped);
    }

    let secret = codewhale_sanitize::redact::mask_bare_secret_tokens(token, SECRET_PLACEHOLDER);

    if let Some(kind) = classify_path(token) {
        kinds.insert(kind.to_string());
        if secret.is_some() {
            kinds.insert(REDACTION_SECRET.to_string());
        }
        return TokenOutcome::replace(PATH_PLACEHOLDER.to_string());
    }

    if let Some(masked) = secret {
        kinds.insert(REDACTION_SECRET.to_string());
        return TokenOutcome::replace(masked);
    }

    TokenOutcome::keep()
}

/// Strip the punctuation a token picks up from surrounding prose.
fn unwrap_token(token: &str) -> &str {
    token
        .trim_start_matches(['(', '[', '{', '"', '\'', '<'])
        .trim_end_matches([',', '.', ';', ':', ')', ']', '}', '"', '\'', '>', '!', '?'])
}

/// Whether a token is an HTTP authorization scheme keyword (and nothing else),
/// in any capitalization.
fn is_auth_scheme(token: &str) -> bool {
    let word = unwrap_token(token);
    !word.is_empty()
        && word.chars().all(|ch| ch.is_ascii_alphabetic())
        && AUTH_SCHEMES
            .iter()
            .any(|scheme| scheme.eq_ignore_ascii_case(word))
}

/// Whether a token is a scheme keyword spelled the way HTTP spells it —
/// `Bearer`, not `bearer` or `BEARER`.
///
/// This is the evidence that separates syntax from prose. It is a weak signal
/// read honestly: capitalization is *suggestive*, so it upgrades an arming that
/// header context already established, and stands alone only for
/// [`SELF_EVIDENT_AUTH_SCHEME`].
fn is_canonical_auth_scheme(token: &str) -> bool {
    AUTH_SCHEMES.contains(&unwrap_token(token))
}

/// Whether a token has the shape of an opaque credential value.
///
/// Deliberately conservative: long, punctuation-free-ish, and mixing letters
/// with digits. Ordinary words — however long — never qualify, which is what
/// keeps `bearer of responsibility` out of the redactor.
fn looks_like_credential_value(token: &str) -> bool {
    let (value, _, _) = codewhale_sanitize::redact::split_value_quotes(unwrap_token(token));
    if codewhale_sanitize::redact::contains_bare_secret_token(value) {
        return true;
    }
    if value.chars().count() < CREDENTIAL_VALUE_MIN_LEN {
        return false;
    }
    let mut has_digit = false;
    let mut has_alpha = false;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            has_digit = true;
        } else if ch.is_ascii_alphabetic() {
            has_alpha = true;
        } else if !matches!(ch, '-' | '_' | '.' | '=' | '+' | '/' | '~') {
            return false;
        }
    }
    has_digit && has_alpha
}

/// Whether a token is an absolute or home-relative filesystem path.
///
/// A lone `/` or `~` is punctuation, not a path; a Windows drive letter needs
/// its `:\` to count.
fn looks_absolute(token: &str) -> bool {
    let trimmed = token.trim_start_matches(['(', '[', '"', '\'']);
    if trimmed.len() < 2 {
        return false;
    }
    if let Some(rest) = trimmed.strip_prefix('/') {
        return rest.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '.' || ch == '_');
    }
    if trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
        return true;
    }
    if trimmed.starts_with("\\\\") {
        return true;
    }
    let mut chars = trimmed.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some('\\' | '/')) if drive.is_ascii_alphabetic()
    )
}

/// Which path kind a token is, if any — the single decision both the bare-token
/// and the `NAME=value` rules ask, so an assignment can never be classified
/// differently from the same value standing alone.
///
/// Escaped spellings are resolved first: a path that arrives inside a JSON
/// string is written `crates\/tui\/src\/main.rs` or `crates\\tui\\src\\main.rs`,
/// and reading only the literal characters would let either spelling through
/// while the receipt claimed nothing was removed.
///
/// A URL is not a filesystem path and is left to the URL-bearing-input guard
/// that already refuses such a task, so a token carrying a scheme is declined
/// here rather than silently reclassified.
fn classify_path(token: &str) -> Option<&'static str> {
    let raw = trim_path_punctuation(token);
    let unescaped = unescape_path(raw);
    let candidate = trim_path_punctuation(&unescaped);
    if candidate.contains("://") {
        return None;
    }
    // Both spellings are asked, because unescaping is lossy in one direction
    // that matters: a UNC share is *literally* `\\host\share`, and collapsing
    // its leading pair would demote a machine-identifying path to a relative
    // one.
    if looks_absolute(raw) || looks_absolute(candidate) {
        return Some(REDACTION_ABSOLUTE_PATH);
    }
    if looks_relative(candidate) {
        return Some(REDACTION_RELATIVE_PATH);
    }
    None
}

/// Strip the punctuation a path picks up from surrounding prose, including the
/// escaped quotes it picks up from a JSON string.
fn trim_path_punctuation(token: &str) -> &str {
    token
        .trim_start_matches(['(', '[', '{', '"', '\'', '<', '`'])
        .trim_end_matches([
            ',', ';', ':', '.', ')', ']', '}', '"', '\'', '>', '`', '!', '?',
        ])
}

/// Resolve `\/` and `\\` to the separator they escape, and drop escaped quotes.
///
/// Returns an owned string only because most tokens need no work; the borrowed
/// fast path is not worth a second code path in a function this small.
fn unescape_path(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('/') => {
                out.push('/');
                chars.next();
            }
            Some('\\') => {
                out.push('\\');
                chars.next();
            }
            Some('"') | Some('\'') => {
                chars.next();
            }
            // A lone backslash is a separator in its own right (`C:\Users`).
            _ => out.push('\\'),
        }
    }
    out
}

/// Whether a token is a repo-relative filesystem path, judged conservatively.
///
/// Two signals, and nothing else, because the cost of a false positive here is
/// paid in the operator's own summary: a redacted word plus a `relative_path`
/// kind on a receipt that removed prose.
///
/// 1. **Explicit relative syntax** — `./x`, `../x`, and their backslash forms.
///    Nothing but a path is spelled that way.
/// 2. **A file extension on the last segment** of a multi-segment token: stem
///    plus 1–8 ASCII *alphabetic* characters. The alphabetic requirement is
///    what keeps `zai/glm-5.2` a model label rather than a file, and the
///    multi-segment requirement is what keeps every bare `provider/model` pair
///    — `deepseek/deepseek-v4-flash`, `anthropic/claude-opus-5` — intact.
///
/// The deliberate gap is an extension-less directory (`crates/tui/src`), which
/// stays. Catching it would need a rule that cannot tell a directory from
/// `read/write/execute`, and shredding prose to hide a directory name is the
/// worse trade.
fn looks_relative(candidate: &str) -> bool {
    if !candidate.contains(['/', '\\']) {
        return false;
    }
    let explicit_prefix = ["./", "../", ".\\", "..\\"]
        .iter()
        .any(|prefix| candidate.starts_with(prefix));
    if explicit_prefix {
        return true;
    }
    let trimmed = candidate.trim_end_matches(['/', '\\']);
    let segments: Vec<&str> = trimmed.split(['/', '\\']).collect();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return false;
    }
    let last = segments[segments.len() - 1];
    let Some((stem, extension)) = last.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (1..=8).contains(&extension.chars().count())
        && extension.chars().all(|ch| ch.is_ascii_alphabetic())
}

/// Whether a string contains anything this module would redact.
///
/// Used by assertions and by durable-write guards that must fail loudly rather
/// than persist a path or a key.
#[must_use]
#[cfg(test)]
pub fn contains_redactable(input: &str) -> bool {
    redact_for_disclosure(input).redacted()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_paths_are_replaced_and_recorded() {
        let redaction = redact_for_disclosure("fix /Users/hunter/src/app/main.rs and ~/notes.md");

        assert!(!redaction.text().contains("/Users/"));
        assert!(!redaction.text().contains("~/"));
        assert!(redaction.text().contains(PATH_PLACEHOLDER));
        assert!(redaction.redacted());
        assert_eq!(redaction.kinds(), vec![REDACTION_ABSOLUTE_PATH.to_string()]);
    }

    #[test]
    fn windows_paths_and_unc_shares_count_as_absolute() {
        for token in ["C:\\Users\\hunter\\app", "\\\\share\\team\\notes"] {
            let redaction = redact_for_disclosure(token);
            assert!(redaction.redacted(), "{token} must be redacted");
            assert_eq!(redaction.text(), PATH_PLACEHOLDER);
        }
    }

    #[test]
    fn secret_shaped_tokens_and_assignments_are_replaced() {
        let redaction = redact_for_disclosure("use sk-live-abc123 and ZAI_API_KEY=zzz");

        assert!(!redaction.text().contains("sk-live-abc123"));
        assert!(!redaction.text().contains("zzz"));
        assert!(
            redaction.text().contains("ZAI_API_KEY=<redacted>"),
            "the name stays, the value goes: {}",
            redaction.text()
        );
        assert_eq!(redaction.kinds(), vec![REDACTION_SECRET.to_string()]);
    }

    /// The credential in an `Authorization` header is a *separate token* from
    /// the header name and from the scheme keyword. Redacting only the keyword
    /// leaves the secret in the clear while the receipt claims a secret was
    /// removed — the exact failure this covers.
    #[test]
    fn a_multi_token_authorization_header_loses_its_credential() {
        let credentials = [
            ["sk", "-live-abc123def456"].concat(),
            ["eyJhbGciOiJIUzI1NiIsInR5cCI6IkpX", "VCJ9"].concat(),
            ["abcdef0123456789", "abcdef"].concat(),
        ];
        let headers = [
            format!("Authorization: Bearer {}", credentials[0]),
            format!("authorization: bearer {}", credentials[1]),
            format!("-H Authorization:Bearer {}", credentials[2]),
        ];
        for header in headers {
            let redaction = redact_for_disclosure(&header);
            let text = redaction.text();
            assert!(redaction.redacted(), "{header} must be redacted");
            assert!(
                text.contains(SECRET_PLACEHOLDER),
                "{header} must carry a placeholder: {text}"
            );
            for leaked in &credentials {
                assert!(!text.contains(leaked), "{leaked} leaked through: {text}");
            }
            assert_eq!(redaction.kinds(), vec![REDACTION_SECRET.to_string()]);
        }
    }

    /// A bare scheme keyword introduces a credential only when a
    /// credential-shaped token actually follows it.
    #[test]
    fn a_bare_bearer_token_is_removed_but_the_scheme_word_survives() {
        let redaction = redact_for_disclosure("send Bearer 9f8e7d6c5b4a3f2e1d0c9b8a and retry");
        let text = redaction.text();

        assert!(
            text.contains("Bearer"),
            "the scheme keyword is not a secret"
        );
        assert!(!text.contains("9f8e7d6c5b4a3f2e1d0c9b8a"), "{text}");
        assert!(text.ends_with("and retry"), "{text}");
    }

    /// Ordinary words that merely *look* like credential prefixes must survive,
    /// and must not report a `secret` redaction kind. `Asia`, `bearer`, and any
    /// identifier containing them are prose, not keys.
    #[test]
    fn ordinary_words_and_identifiers_are_not_mistaken_for_secrets() {
        for text in [
            "ship the Asia region rollout",
            "ASIA is a continent, not a key",
            "the bearer of this note may enter",
            "authorization: needed before merge",
            "rename bearer_token_header to auth_header_name",
            "aws_region defaults to us-east-1",
            "pk_display is a public identifier",
        ] {
            let redaction = redact_for_disclosure(text);
            assert!(!redaction.redacted(), "{text} must survive: {redaction:?}");
            assert_eq!(redaction.text(), text);
        }
    }

    /// The adversarial prose set. Every line here is ordinary English that the
    /// scheme/prefix rules could plausibly mistake for credential syntax, and
    /// every one of them must come back byte-identical with an empty `kinds`.
    ///
    /// A false positive is not a harmless over-redaction: it mangles the routing
    /// summary a human reads *and* writes `secret` onto a durable receipt that
    /// removed nothing, which makes the disclosure a lie in the safe direction.
    #[test]
    fn adversarial_prose_survives_the_credential_state_machine() {
        for text in [
            // The lowercase scheme word, in every position that could arm it.
            "bearer shares responsibility for the rollout",
            "the bearer of bad news is rarely thanked",
            "bearer",
            "each bearer token header is rewritten downstream",
            // Capitalized scheme words that are ordinary English. These are why
            // canonical capitalization arms only for `Bearer`.
            "Token holders vote on the proposal",
            "Basic auth is enabled for the staging endpoint",
            "Digest the results before the review",
            // Weak header context introducing a sentence, not a secret.
            "authorization: needed before merge",
            "authorization: bearer shares responsibility",
            // Identifiers and prefixes that resemble key material.
            "variables like aws_region and pk_display stay readable",
            "aws_ prefixed variables are documented in the runbook",
            // `pk_` is a *public* key prefix and carries nothing; it is
            // deliberately absent from SECRET_VALUE_PREFIXES. (`sk_` is not
            // listed here because it genuinely is a secret prefix and redacting
            // it is correct.)
            "pk_ and pub_ are conventions, not values",
            "asia and akia are four letter strings",
            "the variables were renamed in the same commit",
            // Long lowercase words are still words: shape alone must not fire.
            "internationalization is spelled with eighteen letters",
        ] {
            let redaction = redact_for_disclosure(text);
            assert!(
                !redaction.redacted(),
                "{text:?} is prose and must survive untouched: {redaction:?}"
            );
            assert_eq!(redaction.text(), text);
            assert!(
                redaction.kinds().is_empty(),
                "{text:?} must not claim a redaction it did not make"
            );
        }
    }

    /// The adversarial credential set. A real credential is often short,
    /// lowercase, punctuated, or otherwise shapeless — `Bearer qqq` is the
    /// canonical example, and no shape test could ever catch it. Context has to.
    #[test]
    fn adversarial_credentials_lose_the_whole_value() {
        for (text, leaked) in [
            // The short, shapeless credential. This is the leak the evidence
            // model exists to close.
            ("Bearer qqq", "qqq"),
            ("Authorization: Bearer qqq", "qqq"),
            ("authorization: Bearer qqq", "qqq"),
            // Punctuated and quoted header forms.
            ("Authorization: Bearer qqq.", "qqq"),
            ("-H \"Authorization: Bearer qqq\"", "qqq"),
            ("Authorization:Bearer qqq", "qqq"),
            // The scheme keyword may not absorb the redaction and leave the
            // value behind.
            ("send Bearer hunter2 now", "hunter2"),
            (
                "curl -H Authorization: Bearer sk-live-0000 -X POST",
                "sk-live-0000",
            ),
        ] {
            let redaction = redact_for_disclosure(text);
            let redacted_text = redaction.text();
            assert!(
                redaction.redacted(),
                "{text:?} carries a credential and must be redacted"
            );
            assert!(
                !redacted_text.split(' ').any(|token| token == leaked
                    || token.trim_end_matches(['.', ',', '"', '\'']) == leaked),
                "{leaked:?} leaked through {text:?}: {redacted_text}"
            );
            assert!(
                redacted_text.contains(SECRET_PLACEHOLDER),
                "{text:?} must carry a placeholder: {redacted_text}"
            );
            assert!(
                redaction.kinds().contains(&REDACTION_SECRET.to_string()),
                "{text:?} must disclose the secret kind"
            );
            // The scheme keyword is not the secret and must still be readable,
            // so a reader can tell *what* was removed.
            assert!(
                redacted_text.to_ascii_lowercase().contains("bearer"),
                "the scheme keyword must survive: {redacted_text}"
            );
        }
    }

    /// The one documented false positive of the capitalization rule, pinned so
    /// it stays deliberate rather than becoming a surprise. Capitalized `Bearer`
    /// followed by a word is treated as header syntax; ordinary prose spells it
    /// lowercase, which the test above covers.
    #[test]
    fn capitalized_bearer_arms_even_in_prose_and_that_is_the_known_cost() {
        let redaction = redact_for_disclosure("Bearer tokens are rotated weekly");
        assert_eq!(redaction.text(), "Bearer <redacted> are rotated weekly");

        // The lowercase spelling — what prose actually uses — is untouched.
        let prose = redact_for_disclosure("bearer tokens are rotated weekly");
        assert!(!prose.redacted());
    }

    /// The real AWS shape still goes, so dropping the bare `asia`/`akia`
    /// prefixes did not trade a false positive for a false negative.
    #[test]
    fn full_aws_access_key_ids_are_still_removed() {
        for key in [
            ["AKIA", "IOSFODNN7EXAMPLE"].concat(),
            ["ASIA", "IOSFODNN7EXAMPLE"].concat(),
        ] {
            let redaction = redact_for_disclosure(&format!("creds {key} rotated"));
            assert!(!redaction.text().contains(&key), "{}", redaction.text());
            assert_eq!(redaction.kinds(), vec![REDACTION_SECRET.to_string()]);
        }
    }

    #[test]
    fn ordinary_prose_is_left_alone() {
        let redaction = redact_for_disclosure("refactor the parser and add a regression test");
        assert!(!redaction.redacted());
        assert_eq!(
            redaction.text(),
            "refactor the parser and add a regression test"
        );
        assert!(redaction.kinds().is_empty());
    }

    /// A repo-relative path discloses the private tree's shape to whatever
    /// provider the routing summary reaches, and is persisted next to it. Every
    /// spelling one arrives in — bare, quoted, JSON-escaped with `\/` or `\\`,
    /// explicitly relative, assigned to a name, trailing prose punctuation —
    /// must lose the path *and* say so on the receipt.
    #[test]
    fn repo_relative_paths_are_redacted_in_every_spelling_and_disclosed() {
        for token in [
            "crates/tui/src/main.rs",
            "src/lib.rs",
            "web/lib/deploy-preflight.test.ts",
            ".github/workflows/web.yml",
            "crates\\tui\\src\\main.rs",
            "crates\\/tui\\/src\\/main.rs",
            "\\\"crates/tui/src/main.rs\\\"",
            "\"crates/tui/src/main.rs\"",
            "(crates/tui/src/main.rs)",
            "./deploy.sh",
            "../../secret/notes.md",
            "..\\secret\\notes.md",
        ] {
            let redaction = redact_for_disclosure(token);
            assert!(redaction.redacted(), "{token} must be redacted");
            assert!(
                !redaction.text().contains("main.rs")
                    && !redaction.text().contains("notes.md")
                    && !redaction.text().contains("deploy"),
                "{token} leaked: {}",
                redaction.text()
            );
            assert!(
                redaction
                    .kinds()
                    .contains(&REDACTION_RELATIVE_PATH.to_string()),
                "{token} must disclose the relative_path kind: {:?}",
                redaction.kinds()
            );
        }

        // In a sentence, and as a value: the name survives, the path does not.
        let sentence = redact_for_disclosure("patch crates/tui/src/main.rs, then path=src/lib.rs");
        assert_eq!(
            sentence.text(),
            "patch <path> then path=<path>",
            "prose keeps its shape around the placeholder"
        );
        assert_eq!(
            sentence.kinds(),
            vec![REDACTION_RELATIVE_PATH.to_string()],
            "one kind, honestly reported"
        );
    }

    /// The other half of the same rule: it must not shred ordinary prose,
    /// `provider/model` labels, or bare punctuation, because a false positive
    /// here costs the operator their own summary *and* puts a redaction kind on
    /// a receipt that removed nothing.
    #[test]
    fn prose_labels_and_bare_punctuation_are_not_paths() {
        for token in [
            // Provider/model labels — the exact shape a Fleet receipt carries.
            "deepseek/deepseek-v4-flash",
            "zai/glm-5.2",
            "anthropic/claude-opus-5",
            "workspace/glm-pair",
            // Prose that happens to carry separators.
            "a/b",
            "and/or",
            "read/write/execute",
            "TODO/FIXME",
            "provider/model/reasoning",
            // Bare punctuation and non-paths.
            "/",
            "~",
            "5:30",
            "v0.9.2",
            // A URL is not a filesystem path; the URL-bearing-input guard owns
            // it, and reclassifying it here would be a silent behavior change.
            "https://example.test/a/b.rs",
        ] {
            let redaction = redact_for_disclosure(token);
            assert!(!redaction.redacted(), "{token} must not be redacted");
            assert_eq!(redaction.text(), token, "{token} must survive verbatim");
        }
    }

    /// The documented gap, pinned so it stays a decision rather than a
    /// surprise: an extension-less directory survives, because no rule can
    /// separate it from `read/write/execute` without shredding prose.
    #[test]
    fn an_extension_less_directory_is_the_known_residual() {
        let redaction = redact_for_disclosure("look in crates/tui/src");
        assert!(!redaction.redacted());
    }

    const BODY: &str = "Z7qX4mNb2Vc9Lk3PwR8t";
    const HEX: &str = "3f9a8c1d2e4b5a6f7c8d9e0a1b2c3d4e5f6a7b8c";

    fn reported_credential_rows() -> Vec<(String, String)> {
        let jwt = ["eyJhbGciOiJIUzI1NiJ9", ".eyJzdWIiOiIxIn0", ".c2lnbmF0dXJl"].concat();
        let pem = "-----BEGIN PRIVATE KEY----- MIIEvQIBADANBgkqhkiG9w0BAQEFAASC -----END PRIVATE KEY-----";
        vec![
            (format!("OPENAI_KEY=sk-proj-{BODY}"), BODY.to_string()),
            (format!("GITHUB_PAT=ghp_{BODY}"), BODY.to_string()),
            (format!("key=sk-live-{BODY}"), BODY.to_string()),
            (format!("(ghp_{BODY})"), BODY.to_string()),
            (format!("'sk-live-{BODY}'"), BODY.to_string()),
            (format!("--api-key={BODY}A"), BODY.to_string()),
            (format!("X-Api-Key: {BODY}AB"), BODY.to_string()),
            (format!("use {jwt} now"), jwt.clone()),
            (format!("glpat-{BODY}"), BODY.to_string()),
            (format!("hf_{BODY}"), BODY.to_string()),
            (format!("AIzaSy{BODY}"), BODY.to_string()),
            (
                "ZAI_API_KEY = zk8Q2mX7vB4nL9pR3tYc".to_string(),
                "zk8Q2mX7vB4nL9pR3tYc".to_string(),
            ),
            ("password: hunter2".to_string(), "hunter2".to_string()),
            (format!("xai-{BODY}"), BODY.to_string()),
            (pem.to_string(), "MIIEvQIBADANBgkq".to_string()),
            (
                format!("{{\"token_value\":\"ghp_{BODY}\"}}"),
                BODY.to_string(),
            ),
            (format!("密钥：sk-live-{BODY}"), BODY.to_string()),
            (format!("use ghs_{BODY}@github.com"), BODY.to_string()),
            (
                format!("https://x-access-token:ghs_{BODY}@github.com/o/r.git"),
                BODY.to_string(),
            ),
            (
                format!("https://ghp_{BODY}@github.com/o/r.git"),
                BODY.to_string(),
            ),
            (
                format!("https://oauth2:glpat-{BODY}@gitlab.com/o/r.git"),
                BODY.to_string(),
            ),
            (
                format!("x=-----BEGIN PRIVATE KEY-----y{BODY}-----END x OPENAI_API_KEY=sk-proj-{BODY}"),
                BODY.to_string(),
            ),
            (
                "SSH_KEY=-----BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEAAAAAB -----END OPENSSH PRIVATE KEY-----".to_string(),
                "b3BlbnNzaC1rZXktdjEAAAAAB".to_string(),
            ),
            (
                "key=\"-----BEGIN PRIVATE KEY----- MIIEvQIBADANBgkqB -----END PRIVATE KEY-----\"".to_string(),
                "MIIEvQIBADANBgkqB".to_string(),
            ),
            (
                "-----BEGIN PGP PRIVATE KEY BLOCK----- lQOYBFBxyzB -----END PGP PRIVATE KEY BLOCK-----".to_string(),
                "lQOYBFBxyzB".to_string(),
            ),
            (
                "(\"k\":\"-----BEGIN PRIVATE KEY-----\\nMIIEvQIBADANBgkqB\\n-----END PRIVATE KEY-----\\n\")".to_string(),
                "MIIEvQIBADANBgkqB".to_string(),
            ),
            (format!("SLACKTOKEN={HEX}"), HEX.to_string()),
            (format!("GHTOKEN={HEX}"), HEX.to_string()),
            (format!("apitoken={HEX}"), HEX.to_string()),
            (format!("authtoken={HEX}"), HEX.to_string()),
            (format!("mytoken: {HEX}"), HEX.to_string()),
            (format!("OPENAIAPIKEY={HEX}"), HEX.to_string()),
            (format!("SECRETKEY={HEX}"), HEX.to_string()),
            (format!("apiKey := {HEX}"), HEX.to_string()),
            (format!("'api_key' => {HEX}"), HEX.to_string()),
            (format!("password == {HEX}"), HEX.to_string()),
            ("password := Hunter2Xyzzy99".to_string(), "Hunter2Xyzzy99".to_string()),
            (format!("run codewhale login --api-key {HEX} first"), HEX.to_string()),
            ("mysql -u root --password Hunter2Xyzzy99".to_string(), "Hunter2Xyzzy99".to_string()),
            (format!("deploy --token {HEX}"), HEX.to_string()),
            (format!("access_tokens={HEX}"), HEX.to_string()),
            (format!("GITHUB_TOKENS={HEX}"), HEX.to_string()),
            (format!("SESSION_TOKENS={HEX}"), HEX.to_string()),
            (format!("api_tokens: {HEX}"), HEX.to_string()),
            (format!("\"refresh_tokens\": [\"{HEX}\"]"), HEX.to_string()),
            (format!("API_TOKENS={HEX}"), HEX.to_string()),
            (format!("SLACK_BOT_TOKENS={HEX}"), HEX.to_string()),
            (format!("{{\"model\":\"gpt\",\"api_key\":\"{HEX}\"}}"), HEX.to_string()),
            ("{\"user\":\"app\",\"password\":\"Hunter2Xyzzy99\"}".to_string(), "Hunter2Xyzzy99".to_string()),
            (format!("{{\"id\":1,\"access_token\":\"{HEX}\"}}"), HEX.to_string()),
            (format!("curl https://api.example.test/v1?x=1&api_key={HEX}"), HEX.to_string()),
            (format!("{{\"stdout\":\"ok\\nsk-proj-{BODY}\\n\"}}"), BODY.to_string()),
            (format!("{{\"line\":\"name\\tghp_{BODY}\"}}"), BODY.to_string()),
            (format!("{{\"html\":\"\\u003cghs_{BODY}\\u003e\"}}"), BODY.to_string()),
            (format!("{{\"log\":\"key\\nsk-ant-api03-{BODY}\"}}"), BODY.to_string()),
            (format!("printf 'a\\nsk-live-{BODY}'"), BODY.to_string()),
            ("API_TOKENS=hunter2".to_string(), "hunter2".to_string()),
            (
                "ACCESS_TOKENS=my_short_secret".to_string(),
                "my_short_secret".to_string(),
            ),
            (
                "ACCESS_TOKENS=secret_token_without_digits".to_string(),
                "secret_token_without_digits".to_string(),
            ),
            (
                format!("{{\"api_tokens\": \"{HEX}\"}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"api_tokens\": [ \"{HEX}\" ]}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"api_tokens\": {{ \"prod\": \"{HEX}\" }}}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"api_tokens\": [ \"{HEX}\", \"9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b\" ]}}"),
                "9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b".to_string(),
            ),
            (
                format!("{{\"api_tokens\":[\"{HEX}\",\"9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b\"]}}"),
                "9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b".to_string(),
            ),
            (
                format!("{{\"stdout\":\"ok\\n\\\"api_key\\\": \\\"{HEX}\\\"\"}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"stdout\":\"ok\\n\\\"api_key\\\":\\\"{HEX}\\\"\"}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"stdout\":\"ok\\napi_key: {HEX}\"}}"),
                HEX.to_string(),
            ),
            (
                "{\"stdout\":\"ok\\npassword: Hunter2Xyzzy99\"}".to_string(),
                "Hunter2Xyzzy99".to_string(),
            ),
            (
                "{\"stdout\":\"ok\\n\\\"api_key\\\":\\\"short_secret\\\"\"}".to_string(),
                "short_secret".to_string(),
            ),
            (
                format!("{{\"stdout\":\"ok\\n\\\"api_key\\\":\\n\\\"{HEX}\\\"\"}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"stdout\":\"API_KEY={HEX}\"}}"),
                HEX.to_string(),
            ),
            (
                format!("{{\"stdout\":\"ok\\u003cAPI_KEY={HEX}\\u003e\"}}"),
                HEX.to_string(),
            ),
            (
                format!("\\\\\\\"api_key\\\\\\\":\\\\\\\"{HEX}\\\\\\\""),
                HEX.to_string(),
            ),
            (
                "{\"authorization\":\"basic dXNlcjpIdW50ZXIyWHl6enk5OQ==\"}".to_string(),
                "dXNlcjpIdW50ZXIyWHl6enk5OQ==".to_string(),
            ),
            (
                "{\"b\":\"Authorization: Basic dXNlcjpIdW50ZXIyWHl6enk5OQ==\"}".to_string(),
                "dXNlcjpIdW50ZXIyWHl6enk5OQ==".to_string(),
            ),
            (
                format!("FOO=\"bar\nbaz\"\nAPI_KEY={HEX}"),
                HEX.to_string(),
            ),
        ]
    }

    #[test]
    fn credentials_in_every_reported_spelling_are_removed_and_disclosed() {
        for (input, leaked) in reported_credential_rows() {
            let redaction = redact_for_disclosure(&input);
            assert!(
                !redaction.text().contains(&leaked),
                "{input} leaked: {}",
                redaction.text()
            );
            assert_eq!(
                redaction.kinds(),
                vec![REDACTION_SECRET.to_string()],
                "{input}"
            );
        }
    }

    #[test]
    fn operators_and_flags_are_kept_and_only_the_value_is_removed() {
        for (input, expected) in [
            (
                format!("set apiKey := \"{HEX}\" in main.go"),
                "set apiKey := <redacted> in main.go",
            ),
            (
                format!("set 'api_key' => '{HEX}' in config"),
                "set 'api_key' => <redacted> in config",
            ),
            (
                format!("check password == {HEX}"),
                "check password == <redacted>",
            ),
            (
                "set password := Hunter2Xyzzy99 now".to_string(),
                "set password := <redacted> now",
            ),
            (
                format!("run codewhale login --api-key {HEX} first"),
                "run codewhale login --api-key <redacted> first",
            ),
            (format!("deploy --token {HEX}"), "deploy --token <redacted>"),
            (format!("SLACKTOKEN={HEX}"), "SLACKTOKEN=<redacted>"),
            (format!("mytoken: {HEX}"), "mytoken: <redacted>"),
        ] {
            let redaction = redact_for_disclosure(&input);
            assert_eq!(redaction.text(), expected, "{input}");
            assert_eq!(redaction.kinds(), vec![REDACTION_SECRET.to_string()]);
        }
    }

    #[test]
    fn token_counts_are_not_secrets() {
        for text in [
            "max_tokens=8192",
            "tokens=5",
            "completion_tokens: 50",
            "tokenBudget: 9",
            "--token-limit 5",
            "TOKEN_LIMIT=4096",
            "tokenizer=bpe",
            "tokenizer=sentencepiece-v2",
            "output_tokens=512",
            "set --max-tokens 4096 --token-limit 5",
            "run --max-tokens=8192 claude-3-5-sonnet-20241022",
            "eval --tokens=5 deadbeef12345678",
            "{\"tokens\": { \"input\": 100, \"output\": 200 }}",
        ] {
            let redaction = redact_for_disclosure(text);
            assert_eq!(redaction.text(), text);
            assert!(redaction.kinds().is_empty(), "{text}");
        }

        let nested_obj =
            redact_for_disclosure(&format!("{{\"api_tokens\": {{ \"prod\": \"{HEX}\" }}}}"));
        assert!(
            nested_obj.text().contains("\"prod\":"),
            "{}",
            nested_obj.text()
        );
        assert!(!nested_obj.text().contains(HEX), "{}", nested_obj.text());

        let rel = redact_for_disclosure("set --tokenizer=bpe crates/engine/src/v2.rs");
        assert_eq!(rel.text(), "set --tokenizer=bpe <path>");
        assert_eq!(rel.kinds(), vec![REDACTION_RELATIVE_PATH.to_string()]);

        let abs = redact_for_disclosure("run --token-limit=4096 /Users/alice/src/v2");
        assert_eq!(abs.text(), "run --token-limit=4096 <path>");
        assert_eq!(abs.kinds(), vec![REDACTION_ABSOLUTE_PATH.to_string()]);
    }

    #[test]
    fn the_routing_payload_carries_none_of_the_reported_credentials() {
        for (input, leaked) in reported_credential_rows() {
            let payload = crate::fleet_reasoning::bounded_routing_payload(&format!(
                "fix the deploy script\n{input}\nthen rerun"
            ));
            assert!(
                !payload.text().contains(&leaked),
                "{input} reached the router: {}",
                payload.text()
            );
            assert!(payload.disclosure().redacted, "{input}");
            assert!(payload.text().starts_with("fix the deploy script"));
            assert!(payload.text().ends_with("then rerun"), "{}", payload.text());
        }
    }

    #[test]
    fn contains_redactable_matches_the_redactor() {
        assert!(contains_redactable("/Users/hunter"));
        assert!(contains_redactable("token=abc"));
        assert!(!contains_redactable("land a fix in the workflow crate"));
    }
}
