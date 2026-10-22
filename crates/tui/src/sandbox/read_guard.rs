//! Read deny-list (S1, #5568 follow-up).
//!
//! # What this is
//!
//! Every sandbox posture Codewhale ships — including `read-only` — grants the
//! sandboxed process read access to the entire filesystem
//! (`policy.rs::has_full_disk_read_access`). #5568 added the *plumbing* for an
//! opt-in deny-list (Seatbelt last-match-wins `deny file-read*` rules;
//! bubblewrap masks), but the list shipped empty, so in practice nothing was
//! denied. This module supplies (a) a curated default set covering the obvious
//! credential stores and (b) the in-process matcher that Codewhale's own
//! file-reading tools consult — those tools call `std::fs` directly inside the
//! harness process and are never wrapped by `sandbox-exec` or `bwrap` at all,
//! so the OS-level rules alone left the largest hole wide open.
//!
//! # What this is NOT
//!
//! **This is defense-in-depth, not a security boundary.** It raises the cost of
//! a confused or prompt-injected agent stumbling into `~/.ssh/id_ed25519`; it
//! does not contain a deliberate attacker. Specifically it does NOT stop:
//!
//! - **Hardlinks.** A hardlink is a second *name* for the same inode with no
//!   trace of the first. `foo` hardlinked to `~/.ssh/id_rsa` canonicalizes to
//!   `foo`, matches nothing, and is read. No path-based deny-list can fix this;
//!   only an inode-level or MAC-label check could.
//! - **Content already elsewhere.** A key copied into the workspace before the
//!   agent ran, or pasted into the conversation, is readable.
//! - **Indirect reads.** `ssh-agent`, `security find-generic-password`,
//!   `aws sts get-session-token`, a helper the user installed — a process that
//!   *hands over* a secret without the agent reading the file. On macOS
//!   `~/Library/Keychains` is denied but the keychain *daemon* is not.
//! - **`danger-full-access`.** That posture bypasses the OS wrapper entirely
//!   (`should_sandbox() == false`). The in-process tool checks still apply, but
//!   a shell command does not.
//! - **Anything on the network side.** A denied read does not stop exfiltration
//!   of what *was* read.
//! - **Reads by MCP servers and other child processes** that Codewhale did not
//!   itself wrap.
//!
//! Treat it as a seatbelt, and keep the real controls (least-privilege
//! credentials, short-lived tokens, approval prompts) doing the real work.
//!
//! # Matching rules
//!
//! - **Deny wins.** A path matching any deny rule is refused; there is no allow
//!   rule that can override one. Exemptions (`sandbox_read_denylist_exempt`)
//!   subtract from the *built-in defaults* only, before matching — a path the
//!   user explicitly listed in `sandbox_denied_read_paths` can never be
//!   exempted back open.
//! - **Symlinks.** Both the literal path and its `canonicalize()`d target are
//!   tested, so a symlink pointing into `~/.ssh` is denied by its target even
//!   though its own name is innocuous. The *rules* are resolved the same way
//!   when they are built: a rule spelled `/etc/ssh` also denies
//!   `/private/etc/ssh` on macOS, where `/etc` is itself a symlink, and a rule
//!   written against a `/var/...` directory fires for the `/private/var/...`
//!   spelling that `canonicalize` and `current_dir` hand back. Without that,
//!   the resolved candidate never matched a literal rule, which is exactly the
//!   shape hosted macOS CI runs in (`$TMPDIR` under `/var/folders`).
//!   Exemptions are matched the same way, so exempting `/private/etc/sudoers`
//!   — the spelling a denial message may name — reopens the `/etc/sudoers`
//!   rule it resolves from. Rules are resolved when the list is built
//!   (startup and config reload); a symlink retargeted afterwards is seen
//!   through the literal candidate only until the list is rebuilt, which is
//!   within the defense-in-depth posture above.
//! - **`..` and relative paths.** Two candidates are tested. The literal one is
//!   lexically normalized (`.`/`..` folded without touching the disk), which
//!   catches traversal into a denied tree even when nothing on the path exists
//!   yet. The resolved one keeps `..` components raw and lets the OS apply
//!   them *after* resolving each symlink component — the secure order — because
//!   with `pub/link -> denied/sub`, the path `pub/link/../secret` really reads
//!   `denied/secret`; folding `..` first would hide that. When the path does
//!   not exist, the deepest existing ancestor is canonicalized (with the same
//!   raw order) and the remainder re-appended.
//! - **Case.** On macOS and Windows — where the default filesystem is
//!   case-insensitive — comparison is case-folded, so `~/.SSH/ID_RSA` is denied.
//!   On Linux comparison is exact, matching the filesystem's own semantics.
//! - **Boundaries are component-wise.** `~/.awsome/notes.md` is not under
//!   `~/.aws`; a plain string `starts_with` would have said it was.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// Why a read was refused, so callers can render one clear message.
///
/// A denial is always an explicit error. It is never rendered as an empty file,
/// a zero-length result, or a "not found" — a silent empty read teaches an agent
/// that the file is empty and invites it to try a dozen sibling paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadDenial {
    /// The path the caller asked for, as written.
    pub requested: PathBuf,
    /// The deny rule that matched.
    pub rule: DenyRule,
    /// True when the match was on the symlink target rather than the literal
    /// path — worth saying out loud, or the refusal looks arbitrary.
    pub via_symlink: bool,
}

impl ReadDenial {
    /// One-line, non-leaky refusal message.
    ///
    /// Names the *rule*, not the resolved secret path: telling the model that
    /// `notes.txt` really points at `/Users/x/.ssh/id_ed25519` hands it the
    /// location it was looking for.
    #[must_use]
    pub fn message(&self, tool: &str) -> String {
        let via = if self.via_symlink {
            " (reached through a symlink)"
        } else {
            ""
        };
        format!(
            "{tool} refused to read {}{via}: the sandbox read deny-list blocks {}. \
             This path is treated as a credential store. If it is genuinely needed, \
             add it to `sandbox_read_denylist_exempt` in your Codewhale config.",
            self.requested.display(),
            self.rule.describe(),
        )
    }
}

/// A single deny rule. Kept as an enum rather than a bare path so the refusal
/// message can name the rule ("SSH keys") instead of echoing a secret path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyRule {
    /// Everything at or below a directory (or a single file at that path).
    Subtree {
        /// Normalized absolute path, as configured.
        path: PathBuf,
        /// `path` with symlinks resolved, kept only when it differs. A read is
        /// matched against both spellings: the literal one catches
        /// `/etc/sudoers` as written, this one catches `/private/etc/sudoers`
        /// — the same file, reached by the name the OS actually uses.
        resolved: Option<PathBuf>,
        /// Human label, e.g. "SSH keys (~/.ssh)".
        label: &'static str,
    },
    /// Any file whose *name* matches, anywhere on disk. Used for `.env`, which
    /// has no fixed location.
    FileName {
        /// Human label.
        label: &'static str,
    },
}

impl DenyRule {
    /// A subtree rule that also remembers where its path really leads.
    fn subtree(path: PathBuf, label: &'static str) -> Self {
        let resolved = canonicalize_best_effort(&path);
        DenyRule::Subtree {
            resolved: (resolved != path).then_some(resolved),
            path,
            label,
        }
    }

    /// True when this rule, under either spelling, lies at or below one of
    /// `roots`. Exemptions subtract whole rules, so both spellings count.
    fn is_within_any(&self, roots: &[PathBuf]) -> bool {
        let DenyRule::Subtree { path, resolved, .. } = self else {
            return false;
        };
        roots.iter().any(|root| {
            path_is_within(path, root)
                || resolved
                    .as_deref()
                    .is_some_and(|real| path_is_within(real, root))
        })
    }

    #[must_use]
    fn describe(&self) -> String {
        match self {
            DenyRule::Subtree { label, .. } => (*label).to_string(),
            DenyRule::FileName { label } => (*label).to_string(),
        }
    }
}

/// The compiled deny-list.
#[derive(Debug, Clone, Default)]
pub struct ReadDenylist {
    subtrees: Vec<DenyRule>,
    deny_env_files: bool,
}

impl ReadDenylist {
    /// An empty deny-list: denies nothing. Used when the user turns defaults
    /// off and configures no paths of their own.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build the effective deny-list.
    ///
    /// * `include_defaults` — apply the built-in credential-store set.
    /// * `extra` — user-configured `sandbox_denied_read_paths`; these are
    ///   absolute (or `~`-prefixed) paths and are never exemptable.
    /// * `exempt` — user-configured `sandbox_read_denylist_exempt`; subtracts
    ///   from the built-in defaults only.
    #[must_use]
    pub fn build(include_defaults: bool, extra: &[PathBuf], exempt: &[PathBuf]) -> Self {
        // Each exemption is kept in both spellings too, so exempting the path a
        // denial named (`/private/etc/sudoers` on macOS) reopens the rule it
        // resolved from (`/etc/sudoers`), and vice versa.
        let exempt_normalized: Vec<PathBuf> = exempt
            .iter()
            .cloned()
            .map(expand_home_prefix)
            .map(|p| normalize_lexically(&p))
            .flat_map(|p| {
                let resolved = canonicalize_best_effort(&p);
                let real = (resolved != p).then_some(resolved);
                std::iter::once(p).chain(real)
            })
            .collect();

        let mut subtrees = Vec::new();
        let mut deny_env_files = false;

        if include_defaults {
            // The `.env` rule has no fixed location, so its exemption is
            // name-shaped: any exempt entry whose FILE NAME is `.env` — bare
            // `.env`, `~/.env`, `some/project/.env` — disables the whole
            // filename rule. Comparing the raw string could never match: the
            // entries above were normalized to absolute paths.
            deny_env_files = !exempt_normalized.iter().any(|p| {
                p.file_name()
                    .is_some_and(|name| name == std::ffi::OsStr::new(".env"))
            });
            for (raw, label) in default_denied_subtrees() {
                let rule = DenyRule::subtree(normalize_lexically(&raw), label);
                if rule.is_within_any(&exempt_normalized) {
                    continue;
                }
                subtrees.push(rule);
            }
        }

        // User-listed denies are appended last and are NOT filtered by the
        // exempt list: deny wins over allow, without exception.
        for raw in extra {
            let path = normalize_lexically(&expand_home_prefix(raw.clone()));
            if path.as_os_str().is_empty() {
                continue;
            }
            subtrees.push(DenyRule::subtree(
                path,
                "a path in `sandbox_denied_read_paths`",
            ));
        }

        Self {
            subtrees,
            deny_env_files,
        }
    }

    /// True when nothing is denied — i.e. the posture really does grant read of
    /// every file on disk.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.subtrees.is_empty() && !self.deny_env_files
    }

    /// Every literal subtree path, for handing to the OS wrappers
    /// (`SandboxManager::set_denied_read_subpaths`). The filename rule (`.env`)
    /// has no fixed path and therefore cannot be expressed to Seatbelt or
    /// bubblewrap as a subpath — it is enforced in-process only, which is a
    /// real gap for shell commands and is documented as such.
    #[must_use]
    pub fn subtree_paths(&self) -> Vec<PathBuf> {
        self.subtrees
            .iter()
            .filter_map(|rule| match rule {
                DenyRule::Subtree { path, .. } => Some(path.clone()),
                DenyRule::FileName { .. } => None,
            })
            .collect()
    }

    /// Check a path a tool is about to read.
    ///
    /// `requested` may be relative, may contain `..`, may be a symlink, and may
    /// not exist. Both the lexically normalized path and the canonicalized
    /// target are tested; either matching is a denial.
    pub fn check(&self, requested: &Path) -> Result<(), ReadDenial> {
        if self.is_empty() {
            return Ok(());
        }

        let literal = absolutize(requested);
        let resolved = canonicalize_best_effort(requested);
        let via_symlink = resolved != literal;

        for candidate in std::iter::once(&literal).chain(via_symlink.then_some(&resolved)) {
            if self.deny_env_files && is_env_file(candidate) {
                return Err(ReadDenial {
                    requested: requested.to_path_buf(),
                    rule: DenyRule::FileName {
                        label: "environment files (`.env`, `.env.<name>`)",
                    },
                    via_symlink: via_symlink && candidate == &resolved,
                });
            }
            for rule in &self.subtrees {
                let DenyRule::Subtree {
                    path,
                    resolved: rule_resolved,
                    ..
                } = rule
                else {
                    continue;
                };
                let hit = path_is_within(candidate, path)
                    || rule_resolved
                        .as_deref()
                        .is_some_and(|real| path_is_within(candidate, real));
                if hit {
                    return Err(ReadDenial {
                        requested: requested.to_path_buf(),
                        rule: rule.clone(),
                        via_symlink: via_symlink && candidate == &resolved,
                    });
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Process-wide active deny-list
//
// Codewhale's file-reading tools (`read_file`, `read`, `read_media`, …) run
// in-process and are never wrapped by `sandbox-exec` or `bwrap`, so they have
// to consult the deny-list themselves. Threading config through every tool
// signature would touch dozens of call sites for one read; a process-global
// set once at startup keeps the blast radius to the tools that actually read
// files.
// ---------------------------------------------------------------------------

static ACTIVE: RwLock<Option<Arc<ReadDenylist>>> = RwLock::new(None);
static FALLBACK: OnceLock<Arc<ReadDenylist>> = OnceLock::new();

/// Install the deny-list resolved from user config. Called once during startup.
pub fn set_active(list: ReadDenylist) {
    if let Ok(mut slot) = ACTIVE.write() {
        *slot = Some(Arc::new(list));
    }
}

/// The deny-list in force for this process.
///
/// Falls back to the built-in defaults when startup has not installed one, so
/// a code path that runs before config load is protected rather than open.
#[must_use]
pub fn active() -> Arc<ReadDenylist> {
    if let Ok(slot) = ACTIVE.read()
        && let Some(list) = slot.as_ref()
    {
        return Arc::clone(list);
    }
    Arc::clone(FALLBACK.get_or_init(|| Arc::new(ReadDenylist::build(true, &[], &[]))))
}

/// `.env`, `.env.local`, `.env.production` — but deliberately NOT
/// `.env.example`, `.env.sample`, `.env.template`, `.env.defaults`, or
/// `.env.dist`. Those are committed placeholders that a coding agent has a
/// legitimate, routine reason to read (they document which variables a project
/// needs), and denying them would break ordinary development for no security
/// gain — they contain no secrets by construction.
fn is_env_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = fold_case_str(name);
    if name == ".env" {
        return true;
    }
    let Some(suffix) = name.strip_prefix(".env.") else {
        return false;
    };
    const PLACEHOLDER_SUFFIXES: &[&str] = &[
        "example", "sample", "template", "defaults", "dist", "schema",
    ];
    !PLACEHOLDER_SUFFIXES.contains(&suffix)
}

/// The built-in default deny set.
///
/// Chosen against one test: **would denying this break ordinary development?**
/// Everything here is a credential store that build tools, language servers,
/// test runners, and source reading never need. Deliberately excluded, despite
/// containing or neighbouring secrets:
///
/// - `~/.gitconfig` — read constantly by tooling; holds config, not secrets.
///   (`~/.git-credentials`, which holds the secrets, IS denied.)
/// - `~/.cargo`, `~/.npm`, `~/.m2` as a whole — the seatbelt profile already
///   grants these read+write because `cargo build` and `npx` fail without
///   them. Only the credential *files* inside them are denied.
/// - `~/.docker` as a whole — `docker build` reads it. Only
///   `~/.docker/config.json` (registry auth) is denied.
/// - `~/.config` as a whole — far too broad; individual credential dirs inside
///   it are listed instead.
/// - The user's source tree, `~/Documents`, `~/Downloads` — a coding agent must
///   still be able to read the user's code, which is the entire point.
fn default_denied_subtrees() -> Vec<(PathBuf, &'static str)> {
    let mut out = machine_wide_denied_subtrees();
    if let Ok(Some(active)) = codewhale_paths::codewhale_home() {
        out.push((
            active.join("secrets"),
            "Codewhale secret store (active home)",
        ));
        out.push((
            active.join("credentials"),
            "Codewhale OAuth credentials (active home)",
        ));
    }
    if let Some(legacy) = codewhale_paths::legacy_deepseek_home() {
        out.push((
            legacy.join("secrets"),
            "Codewhale secret store (legacy home)",
        ));
        out.push((
            legacy.join("credentials"),
            "Codewhale OAuth credentials (legacy home)",
        ));
    }
    out.push((
        crate::oauth::auth_file_path(),
        "Codex CLI login (resolved auth file)",
    ));
    out.extend(runtime_daemon_socket_dirs());
    let Some(home) = dirs::home_dir() else {
        return out;
    };
    let h = |rel: &str| home.join(rel);

    let mut ambient = vec![
        // --- SSH / GPG ---
        (h(".ssh"), "SSH keys and known-hosts (~/.ssh)"),
        (h(".gnupg"), "GnuPG keyring (~/.gnupg)"),
        // --- Cloud provider credentials ---
        (h(".aws"), "AWS credentials (~/.aws)"),
        (
            h(".config/gcloud"),
            "Google Cloud credentials (~/.config/gcloud)",
        ),
        (h(".azure"), "Azure credentials (~/.azure)"),
        (h(".kube"), "Kubernetes credentials (~/.kube)"),
        (h(".oci"), "Oracle Cloud credentials (~/.oci)"),
        (
            h(".config/doctl"),
            "DigitalOcean credentials (~/.config/doctl)",
        ),
        (h(".config/fly"), "Fly.io credentials (~/.config/fly)"),
        (h(".vercel"), "Vercel credentials (~/.vercel)"),
        (
            h(".wrangler/config"),
            "Cloudflare credentials (~/.wrangler/config)",
        ),
        (
            h(".config/gh/hosts.yml"),
            "GitHub CLI tokens (~/.config/gh/hosts.yml)",
        ),
        (
            h(".config/glab-cli"),
            "GitLab CLI tokens (~/.config/glab-cli)",
        ),
        // --- Package-registry and network credential files ---
        (h(".netrc"), "netrc credentials (~/.netrc)"),
        (h("_netrc"), "netrc credentials (~/_netrc)"),
        (h(".pgpass"), "PostgreSQL password file (~/.pgpass)"),
        (h(".my.cnf"), "MySQL credentials (~/.my.cnf)"),
        (h(".npmrc"), "npm auth tokens (~/.npmrc)"),
        (h(".pypirc"), "PyPI auth tokens (~/.pypirc)"),
        (
            h(".git-credentials"),
            "stored git credentials (~/.git-credentials)",
        ),
        (
            h(".cargo/credentials"),
            "crates.io token (~/.cargo/credentials)",
        ),
        (
            h(".cargo/credentials.toml"),
            "crates.io token (~/.cargo/credentials.toml)",
        ),
        (
            h(".docker/config.json"),
            "Docker registry auth (~/.docker/config.json)",
        ),
        (
            h(".m2/settings-security.xml"),
            "Maven master password (~/.m2)",
        ),
        (
            h(".gradle/gradle.properties"),
            "Gradle credentials (~/.gradle/gradle.properties)",
        ),
        // --- Codewhale's own credential stores ---
        // Duplicated with `tools::file::is_codewhale_credential_path` on
        // purpose: that guard is scoped to the *active* config, this one is
        // unconditional, and neither should depend on the other still existing.
        (
            h(".codewhale/secrets"),
            "Codewhale secret store (~/.codewhale/secrets)",
        ),
        (
            h(".deepseek/secrets"),
            "Codewhale secret store (~/.deepseek/secrets)",
        ),
        (
            h(".codewhale/credentials"),
            "Codewhale OAuth credentials (ambient home)",
        ),
        (
            h(".deepseek/credentials"),
            "Codewhale OAuth credentials (ambient legacy home)",
        ),
        (h(".kimi/credentials"), "Kimi CLI credentials"),
        (h(".claude/.credentials.json"), "Claude Code credentials"),
        (
            h(".codewhale-cu/recordings"),
            "Computer Use recordings and trajectories",
        ),
        // --- Browser profiles (cookies, saved passwords, session tokens) ---
        (h(".mozilla"), "Firefox profile (~/.mozilla)"),
        (
            h(".config/google-chrome"),
            "Chrome profile (~/.config/google-chrome)",
        ),
        (
            h(".config/chromium"),
            "Chromium profile (~/.config/chromium)",
        ),
        (
            h(".config/BraveSoftware"),
            "Brave profile (~/.config/BraveSoftware)",
        ),
    ];

    if cfg!(target_os = "macos") {
        ambient.extend([
            (
                h("Library/Keychains"),
                "macOS keychain (~/Library/Keychains)",
            ),
            (
                h("Library/Application Support/Google/Chrome"),
                "Chrome profile (~/Library/Application Support/Google/Chrome)",
            ),
            (
                h("Library/Application Support/Firefox"),
                "Firefox profile (~/Library/Application Support/Firefox)",
            ),
            (
                h("Library/Application Support/BraveSoftware"),
                "Brave profile (~/Library/Application Support/BraveSoftware)",
            ),
            (h("Library/Safari"), "Safari profile (~/Library/Safari)"),
            (
                h("Library/Cookies"),
                "macOS cookie store (~/Library/Cookies)",
            ),
        ]);
    }

    out.extend(ambient);
    out
}

fn runtime_daemon_socket_dirs() -> Vec<(PathBuf, &'static str)> {
    use codewhale_app_server::daemon_socket::{SocketPathInputs, resolve_socket_path};
    let live = SocketPathInputs::from_environment(None).unwrap_or_else(|_| SocketPathInputs {
        xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        user_home: codewhale_paths::user_home(),
        macos: cfg!(target_os = "macos"),
        ..SocketPathInputs::default()
    });
    let layouts = [
        live.clone(),
        SocketPathInputs {
            codewhale_home_override: None,
            ..live.clone()
        },
        SocketPathInputs {
            codewhale_home_override: None,
            xdg_runtime_dir: None,
            ..live.clone()
        },
        SocketPathInputs {
            codewhale_home_override: None,
            xdg_runtime_dir: None,
            macos: false,
            ..live
        },
    ];
    let mut dirs: Vec<PathBuf> = Vec::new();
    for layout in &layouts {
        if let Ok(socket) = resolve_socket_path(layout)
            && let Some(dir) = socket.parent()
            && !dirs.iter().any(|seen| seen == dir)
        {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs.into_iter()
        .map(|dir| (dir, "Codewhale runtime daemon socket and owner receipt"))
        .collect()
}

fn machine_wide_denied_subtrees() -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = vec![
        (
            PathBuf::from("/etc/shadow"),
            "system password hashes (/etc/shadow)",
        ),
        (
            PathBuf::from("/etc/sudoers"),
            "sudoers policy (/etc/sudoers)",
        ),
        (PathBuf::from("/etc/ssh"), "system SSH host keys (/etc/ssh)"),
    ];
    if cfg!(target_os = "macos") {
        out.push((
            PathBuf::from("/Library/Keychains"),
            "system keychain (/Library/Keychains)",
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Path handling
//
// The evasion cases this has to survive are the whole reason the module exists;
// a deny-list a symlink walks around is theater.
// ---------------------------------------------------------------------------

/// Expand a leading `~` to the user's home directory.
fn expand_home_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if text == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

/// Fold `.` and `..` without touching the disk, and make the path absolute
/// against the current directory when it is relative.
///
/// Purely lexical on purpose: this is the check that catches
/// `workspace/../../../.ssh/id_rsa` even when nothing on that path exists yet.
/// It is paired with — never a substitute for — `canonicalize_best_effort`,
/// which is what catches symlinks.
fn normalize_lexically(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };

    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past the root: `/..` is `/`.
                if out
                    .components()
                    .next_back()
                    .is_some_and(|c| !matches!(c, Component::RootDir | Component::Prefix(_)))
                {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn absolutize(path: &Path) -> PathBuf {
    normalize_lexically(path)
}

/// Make a path absolute against the current directory WITHOUT folding `.` or
/// `..` components.
///
/// Folding first is unsound: with `pub/link -> denied/sub`, the path
/// `pub/link/../secret` lexically becomes `pub/secret`, but the OS resolves the
/// symlink *before* applying `..` and really reads `denied/secret`. Keeping the
/// raw `..` components lets `fs::canonicalize` apply the secure order —
/// resolve each component, then let `..` pop the resolved result.
fn absolutize_raw(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

/// Resolve symlinks as far as the filesystem allows.
///
/// `canonicalize` fails on a path that does not exist, which is exactly the
/// case for a read of a file that is about to be created — and also the case an
/// evader would reach for. So on failure we walk up the RAW ancestor chain
/// (each surviving `..` included) to the deepest ancestor that *does* exist,
/// canonicalize that — resolving any symlinks, with the OS applying any `..`
/// components above it in the secure order — and re-append the remaining
/// components verbatim. A dropped `..` can only leave the candidate *deeper*
/// inside an already-resolved ancestor, which subtree matching still denies;
/// the old lexical-first fold popped symlinks out of existence instead.
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    let absolute = absolutize_raw(path);
    if let Ok(resolved) = std::fs::canonicalize(&absolute) {
        return resolved;
    }

    let mut suffix: Vec<OsString> = Vec::new();
    let mut cursor = absolute.as_path();
    loop {
        let Some(parent) = cursor.parent() else {
            return absolute;
        };
        if let Some(name) = cursor.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(resolved) = std::fs::canonicalize(parent) {
            let mut out = resolved;
            for name in suffix.iter().rev() {
                out.push(name);
            }
            return out;
        }
        cursor = parent;
    }
}

/// Case-fold when — and only when — the platform's default filesystem is
/// case-insensitive. Folding on Linux would deny `~/.SSH` on a system where
/// that is a genuinely different directory.
fn fold_case_str(text: &str) -> String {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

fn fold_component(component: &std::ffi::OsStr) -> OsString {
    match component.to_str() {
        Some(text) => OsString::from(fold_case_str(text)),
        None => component.to_os_string(),
    }
}

/// True when `candidate` is `root` itself or lives beneath it.
///
/// Compared component by component, not by string prefix: `~/.awsome` must not
/// match the `~/.aws` rule, and `starts_with` on the raw strings says it does.
/// (`Path::starts_with` is already component-wise; the case folding is what
/// forces the manual walk.)
fn path_is_within(candidate: &Path, root: &Path) -> bool {
    if !cfg!(any(target_os = "macos", target_os = "windows")) {
        return candidate.starts_with(root);
    }

    let mut root_components = root.components().map(|c| fold_component(c.as_os_str()));
    let mut candidate_components = candidate
        .components()
        .map(|c| fold_component(c.as_os_str()));

    loop {
        match (root_components.next(), candidate_components.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(r), Some(c)) if r == c => {}
            (Some(_), Some(_)) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denylist_for(paths: &[PathBuf]) -> ReadDenylist {
        ReadDenylist::build(false, paths, &[])
    }

    // Two tests below move the process-wide cwd. libtest runs tests as
    // parallel threads of one process, so they take this lock; nextest runs
    // each test in its own process and never contends for it.
    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_cwd() -> std::sync::MutexGuard<'static, ()> {
        CWD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // Unix only: the fixture redirects the home directory through `HOME`,
    // and Windows resolves the profile through the known-folder API instead.
    #[cfg(unix)]
    #[test]
    fn sandbox_read_guard_denies_active_and_known_agent_credentials_in_real_file_tool() {
        use crate::tools::spec::{ToolContext, ToolSpec};
        let _env = crate::test_support::lock_test_env();
        let temp = tempfile::tempdir().expect("private fixture");
        let home = temp.path().join("home");
        let active_home = temp.path().join("active");
        let codex = temp.path().join("codex");
        let _home = crate::test_support::EnvVarGuard::set("HOME", home.to_str().unwrap());
        let _active =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", active_home.to_str().unwrap());
        let _codex = crate::test_support::EnvVarGuard::set("CODEX_HOME", codex.to_str().unwrap());
        let _auth = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_AUTH_FILE");
        assert_eq!(dirs::home_dir().as_deref(), Some(home.as_path()));
        let denied = [
            active_home.join("credentials/fixture.json"),
            codex.join("auth.json"),
            home.join(".kimi/credentials/fixture.json"),
            home.join(".claude/.credentials.json"),
            home.join(".codewhale-cu/recordings/trajectories/fixture.jsonl"),
        ];
        for path in &denied {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "read-s10-synthetic-value").unwrap();
        }
        struct RestoreActive(Option<Arc<ReadDenylist>>);
        impl Drop for RestoreActive {
            fn drop(&mut self) {
                *ACTIVE.write().unwrap() = self.0.take();
            }
        }
        let previous = ACTIVE.write().unwrap().take();
        let _restore = RestoreActive(previous);
        set_active(ReadDenylist::build(true, &[], &[]));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let context = ToolContext::new(temp.path());
        for path in denied {
            let result = runtime.block_on(
                crate::tools::file::ReadFileTool
                    .execute(serde_json::json!({"path": path}), &context),
            );
            assert!(result.is_err(), "file tool read a credential source");
        }
        let safe = home.join(".claude/settings.json");
        std::fs::write(&safe, "ordinary-readable-setting").unwrap();
        let allowed = runtime
            .block_on(
                crate::tools::file::ReadFileTool
                    .execute(serde_json::json!({"path": safe}), &context),
            )
            .expect("ordinary settings remain readable");
        assert!(allowed.content.contains("ordinary-readable-setting"));
    }

    #[test]
    fn empty_denylist_denies_nothing_and_reports_full_disk_read() {
        let list = ReadDenylist::empty();
        assert!(list.is_empty());
        assert!(list.check(Path::new("/etc/hosts")).is_ok());
    }

    #[test]
    fn direct_path_under_a_denied_root_is_refused() {
        let secret = tempfile::tempdir().expect("tempdir");
        let file = secret.path().join("id_ed25519");
        std::fs::write(&file, "KEY").expect("write");

        let list = denylist_for(&[secret.path().to_path_buf()]);
        let denial = list.check(&file).expect_err("must deny");
        assert_eq!(denial.requested, file);
        assert!(!denial.via_symlink);
    }

    #[test]
    fn sibling_with_a_shared_string_prefix_is_not_denied() {
        // `~/.awsome` must not be caught by the `~/.aws` rule. This is the bug
        // a naive string `starts_with` ships with.
        let tmp = tempfile::tempdir().expect("tempdir");
        let denied = tmp.path().join("aws");
        let innocent = tmp.path().join("awsome");
        std::fs::create_dir_all(&denied).expect("mkdir");
        std::fs::create_dir_all(&innocent).expect("mkdir");
        let note = innocent.join("notes.md");
        std::fs::write(&note, "notes").expect("write");

        let list = denylist_for(std::slice::from_ref(&denied));
        assert!(
            list.check(&note).is_ok(),
            "sibling prefix must stay readable"
        );
    }

    #[test]
    fn dot_dot_traversal_out_of_the_workspace_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("secrets");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        std::fs::create_dir_all(&workspace).expect("mkdir");
        let secret = secret_dir.join("token");
        std::fs::write(&secret, "TOKEN").expect("write");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        let sneaky = workspace.join("..").join("secrets").join("token");
        list.check(&sneaky)
            .expect_err("`..` must not walk around the deny-list");
    }

    #[test]
    #[cfg(unix)]
    fn symlink_pointing_into_a_denied_tree_is_refused_by_its_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("secrets");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        std::fs::create_dir_all(&workspace).expect("mkdir");
        let secret = secret_dir.join("id_rsa");
        std::fs::write(&secret, "KEY").expect("write");

        let link = workspace.join("harmless.txt");
        std::os::unix::fs::symlink(&secret, &link).expect("symlink");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        let denial = list.check(&link).expect_err("symlink must not walk around");
        assert!(denial.via_symlink, "denial should report the symlink hop");
        assert!(denial.message("read_file").contains("symlink"));
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_parent_directory_is_refused() {
        // The link is on a *directory* in the middle of the path, not the leaf.
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("secrets");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        std::fs::create_dir_all(&workspace).expect("mkdir");
        std::fs::write(secret_dir.join("token"), "TOKEN").expect("write");

        let link_dir = workspace.join("data");
        std::os::unix::fs::symlink(&secret_dir, &link_dir).expect("symlink");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        list.check(&link_dir.join("token"))
            .expect_err("symlinked parent must not walk around");
    }

    /// F5 regression: `..` must be applied by the OS *after* resolving each
    /// symlink component, never folded lexically first. With
    /// `pub/link -> denied/sub`, the path `pub/link/../secret` really reads
    /// `denied/secret`; the old lexical-first fold produced `pub/secret` and
    /// the check returned Ok while the read sailed through.
    #[test]
    #[cfg(unix)]
    fn dot_dot_through_a_symlink_is_applied_after_symlink_resolution() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let denied = tmp.path().join("denied");
        std::fs::create_dir_all(denied.join("sub")).expect("mkdir");
        std::fs::write(denied.join("sub").join("secret"), "TOKEN").expect("write");
        let pub_dir = tmp.path().join("pub");
        std::fs::create_dir_all(&pub_dir).expect("mkdir");
        std::os::unix::fs::symlink(denied.join("sub"), pub_dir.join("link")).expect("symlink");

        let list = denylist_for(std::slice::from_ref(&denied));
        list.check(&pub_dir.join("link").join("..").join("secret"))
            .expect_err("`..` through a symlink must not walk around the deny-list");

        // Same evasion with a not-yet-existing leaf: the direct canonicalize
        // fails, so the raw-ancestor walk has to carry the check.
        list.check(&pub_dir.join("link").join("..").join("not-yet"))
            .expect_err("the raw-ancestor walk must resolve the symlink before `..`");
    }

    /// A chain of symlinks (link → link → secret) must be followed to the
    /// final target, not just one hop. Integrator defeat attempt: indirection
    /// depth is not a bypass.
    #[test]
    #[cfg(unix)]
    fn symlink_chains_resolve_to_the_denied_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let denied = tmp.path().join("denied");
        std::fs::create_dir_all(&denied).expect("mkdir");
        std::fs::write(denied.join("id_ed25519"), "KEY").expect("write");
        std::os::unix::fs::symlink(denied.join("id_ed25519"), tmp.path().join("hop2"))
            .expect("symlink");
        std::os::unix::fs::symlink(tmp.path().join("hop2"), tmp.path().join("hop1"))
            .expect("symlink");
        let list = denylist_for(std::slice::from_ref(&denied));
        list.check(&tmp.path().join("hop1"))
            .expect_err("a symlink chain must resolve to the denied target");
    }

    /// A relative read issued with the process cwd INSIDE a denied subtree
    /// must be refused: the absolutization against cwd lands under the rule.
    /// (Serialized with the other cwd-moving test; restores cwd regardless.)
    #[test]
    fn relative_read_from_inside_a_denied_tree_is_refused() {
        let _cwd = lock_cwd();
        let tmp = tempfile::tempdir().expect("tempdir");
        let denied = tmp.path().join("denied");
        std::fs::create_dir_all(&denied).expect("mkdir");
        std::fs::write(denied.join("id_ed25519"), "KEY").expect("write");
        let prior = std::env::current_dir().expect("cwd");
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let _restore = Restore(prior);
        std::env::set_current_dir(&denied).expect("chdir into the denied tree");

        let list = denylist_for(std::slice::from_ref(&denied));
        list.check(Path::new("id_ed25519"))
            .expect_err("a relative read from inside the denied tree must be refused");
        list.check(Path::new("./id_ed25519"))
            .expect_err("the dotted spelling must not differ from the bare one");
    }

    /// Separator noise must not dodge matching: repeated slashes collapse and a
    /// trailing slash never changes which subtree a path belongs to.
    #[test]
    fn double_slash_and_trailing_slash_variants_are_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("secrets");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        std::fs::write(secret_dir.join("token"), "TOKEN").expect("write");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        let root = secret_dir.parent().expect("parent");
        let double = PathBuf::from(format!("{}/secrets//token", root.display()));
        list.check(&double)
            .expect_err("double slashes must not walk around the deny-list");
        let trailing = PathBuf::from(format!("{}/secrets/", root.display()));
        list.check(&trailing)
            .expect_err("a trailing slash must not walk around the deny-list");
        // And the `.env` filename rule is name-based, so a trailing slash on it
        // still leaves the file name intact.
        let list = ReadDenylist::build(true, &[], &[]);
        let env_dir = tmp.path().join("nested");
        std::fs::create_dir_all(&env_dir).expect("mkdir");
        list.check(&env_dir.join(".env"))
            .unwrap_err_or_panic("`.env` under any directory is denied by name");
    }

    /// The real attack spelling on macOS: `~/.SSH/ID_RSA` on a case-insensitive
    /// filesystem is `~/.ssh/id_rsa`. (The tempdir sibling above covers the
    /// mechanism; this one pins the default rule itself.)
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_case_variation_of_the_default_ssh_rule_is_refused() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        if !home.join(".ssh").is_dir() {
            // Nothing to match against; skip rather than fake a pass.
            return;
        }
        let list = ReadDenylist::build(true, &[], &[]);
        list.check(&home.join(".SSH").join("ID_RSA"))
            .expect_err("~/.SSH/ID_RSA is ~/.ssh/id_rsa on a case-insensitive filesystem");
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn case_variation_is_refused_on_case_insensitive_filesystems() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("Secrets");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        std::fs::write(secret_dir.join("id_rsa"), "KEY").expect("write");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        let shouted = tmp.path().join("SECRETS").join("ID_RSA");
        list.check(&shouted)
            .expect_err("case variation must not walk around on a case-insensitive FS");
    }

    #[test]
    fn nonexistent_path_under_a_denied_root_is_still_refused() {
        // canonicalize() fails here; the deepest-existing-ancestor walk is what
        // has to carry the check.
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("secrets");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        list.check(&secret_dir.join("not-created-yet").join("key"))
            .expect_err("a not-yet-existing path under a denied root must still be denied");
    }

    #[test]
    fn env_files_are_denied_but_committed_placeholders_are_not() {
        let list = ReadDenylist::build(true, &[], &[]);
        let tmp = tempfile::tempdir().expect("tempdir");

        for denied in [".env", ".env.local", ".env.production"] {
            let path = tmp.path().join(denied);
            list.check(&path)
                .unwrap_err_or_panic(&format!("{denied} should be denied"));
        }
        for allowed in [".env.example", ".env.sample", ".env.template", ".env.dist"] {
            let path = tmp.path().join(allowed);
            assert!(
                list.check(&path).is_ok(),
                "{allowed} is a committed placeholder and must stay readable"
            );
        }
    }

    /// F3 regression: exempting `.env` used to compare a *normalized absolute*
    /// path against the bare string `.env`, which could never match — the
    /// exemption was dead code. The rule is name-shaped, so any exempt entry
    /// whose file name is `.env` must disable it.
    #[test]
    fn exempting_env_by_name_disables_the_env_file_rule() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(&env_file, "SECRET=1\n").expect("write");

        // Bare `.env` — the spelling the docs advertise.
        let list = ReadDenylist::build(true, &[], &[PathBuf::from(".env")]);
        assert!(
            list.check(&env_file).is_ok(),
            "exempting `.env` must disable the env-file rule everywhere"
        );

        // Any path ending in `/.env` — e.g. `~/.env` or `project/.env`.
        let home_spelled = dirs::home_dir().map(|h| h.join(".env"));
        let exempt_path = home_spelled.as_deref().unwrap_or(env_file.as_path());
        let list = ReadDenylist::build(true, &[], &[exempt_path.to_path_buf()]);
        assert!(
            list.check(&env_file).is_ok(),
            "an exempt entry named `.env` must disable the whole env-file rule"
        );

        // An exemption for anything else must leave the rule armed.
        let unrelated = tmp.path().join("notes");
        let list = ReadDenylist::build(true, &[], std::slice::from_ref(&unrelated));
        list.check(&env_file)
            .unwrap_err_or_panic("an unrelated exemption must not reopen `.env` files");
    }

    #[test]
    fn ordinary_source_files_stay_readable_under_the_defaults() {
        let list = ReadDenylist::build(true, &[], &[]);
        let tmp = tempfile::tempdir().expect("tempdir");
        for ordinary in [
            "main.rs",
            "Cargo.toml",
            "README.md",
            ".gitignore",
            ".env.example",
        ] {
            let path = tmp.path().join(ordinary);
            assert!(
                list.check(&path).is_ok(),
                "{ordinary} must stay readable — a coding agent has to read the source tree"
            );
        }
    }

    #[test]
    fn defaults_cover_ssh_and_cloud_credential_stores() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let list = ReadDenylist::build(true, &[], &[]);
        for rel in [
            ".ssh/id_ed25519",
            ".aws/credentials",
            ".config/gcloud/x",
            ".netrc",
        ] {
            list.check(&home.join(rel))
                .unwrap_err_or_panic(&format!("~/{rel} should be denied by default"));
        }
    }

    #[test]
    fn exempt_narrows_the_defaults_but_never_an_explicit_deny() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let ssh = home.join(".ssh");

        // Exempting the default rule reopens it.
        let exempted = ReadDenylist::build(true, &[], std::slice::from_ref(&ssh));
        assert!(
            exempted.check(&ssh.join("id_rsa")).is_ok(),
            "an exempted default must be readable again"
        );

        // The same exemption must NOT reopen a path the user explicitly denied.
        let both =
            ReadDenylist::build(true, std::slice::from_ref(&ssh), std::slice::from_ref(&ssh));
        both.check(&ssh.join("id_rsa"))
            .unwrap_err_or_panic("deny must win over allow");
    }

    #[test]
    fn defaults_can_be_turned_off_entirely() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let list = ReadDenylist::build(false, &[], &[]);
        assert!(list.is_empty());
        assert!(list.check(&home.join(".ssh/id_rsa")).is_ok());
    }

    #[test]
    fn subtree_paths_feed_the_os_wrappers_and_omit_the_filename_rule() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let list = ReadDenylist::build(false, &[tmp.path().to_path_buf()], &[]);
        let paths = list.subtree_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], normalize_lexically(tmp.path()));
    }

    #[test]
    #[cfg(unix)]
    fn denial_message_names_the_rule_without_echoing_the_resolved_secret_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret_dir = tmp.path().join("secrets");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&secret_dir).expect("mkdir");
        std::fs::create_dir_all(&workspace).expect("mkdir");
        std::fs::write(secret_dir.join("id_rsa"), "KEY").expect("write");

        let link = workspace.join("notes.txt");
        std::os::unix::fs::symlink(secret_dir.join("id_rsa"), &link).expect("symlink");

        let list = denylist_for(std::slice::from_ref(&secret_dir));
        let message = list.check(&link).expect_err("deny").message("read_file");
        assert!(message.contains("notes.txt"), "{message}");
        assert!(
            !message.contains("id_rsa"),
            "the refusal must not hand back the secret's real location: {message}"
        );
        assert!(
            message.contains("sandbox_read_denylist_exempt"),
            "{message}"
        );
    }

    #[test]
    fn root_parent_traversal_does_not_escape_above_root() {
        // Spell the traversal from the current drive/root so the assertion
        // holds on Windows too: there `/../../etc` resolves against the cwd's
        // drive and normalizes to `D:\etc`, not a bare `/etc`.
        let cwd = std::env::current_dir().expect("cwd");
        let root: PathBuf = cwd
            .components()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .collect();
        let traversal = root.join("..").join("..").join("etc");
        assert_eq!(normalize_lexically(&traversal), root.join("etc"));
        if cfg!(unix) {
            assert_eq!(
                normalize_lexically(Path::new("/../../etc")),
                PathBuf::from("/etc")
            );
        }
    }

    /// Hosted macOS CI hands `tempfile` a `/var/folders/...` directory that is
    /// really `/private/var/folders/...`; `canonicalize` and `current_dir`
    /// return the resolved spelling while the rule was written against the
    /// literal one, and no symlink test above fired. Build that shape
    /// explicitly so the regression is caught on every host, not only where
    /// `$TMPDIR` happens to be a symlink.
    #[test]
    #[cfg(unix)]
    fn rule_spelled_through_a_symlinked_root_matches_the_resolved_spelling() {
        let _cwd = lock_cwd();
        let tmp = tempfile::tempdir().expect("tempdir");
        let real_root = tmp.path().join("real");
        let denied = real_root.join("secrets");
        std::fs::create_dir_all(&denied).expect("mkdir");
        std::fs::write(denied.join("id_rsa"), "KEY").expect("write");
        let alias_root = tmp.path().join("alias");
        std::os::unix::fs::symlink(&real_root, &alias_root).expect("symlink");

        // The rule names the alias, the way a `/var/...` tempdir rule does.
        let list = denylist_for(&[alias_root.join("secrets")]);

        // A read spelled through the real directory is the same file.
        list.check(&denied.join("id_rsa"))
            .expect_err("the resolved spelling of a denied tree must be refused");
        // An innocuous symlink resolves to the real spelling, never the alias.
        let link = tmp.path().join("notes.txt");
        std::os::unix::fs::symlink(denied.join("id_rsa"), &link).expect("symlink");
        list.check(&link)
            .expect_err("a symlink into the denied tree must be refused by its target");
        // A relative read from inside it absolutizes against the real cwd.
        let prior = std::env::current_dir().expect("cwd");
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let _restore = Restore(prior);
        std::env::set_current_dir(&denied).expect("chdir into the denied tree");
        list.check(Path::new("id_rsa"))
            .expect_err("a relative read from inside the denied tree must be refused");
        // And the innocent sibling of the real directory stays readable.
        let sibling = real_root.join("notes.md");
        std::fs::write(&sibling, "notes").expect("write");
        assert!(list.check(&sibling).is_ok(), "sibling must stay readable");
    }

    /// A denial names the path as requested, so on macOS it may say
    /// `/private/etc/sudoers`; exempting that spelling must reopen the
    /// `/etc/sudoers` rule it resolves from, and the literal spelling must
    /// keep working too. Unrelated defaults stay armed either way.
    #[cfg(target_os = "macos")]
    #[test]
    fn exempting_either_spelling_of_a_symlinked_default_rule_reopens_it() {
        for exempt in ["/private/etc/sudoers", "/etc/sudoers"] {
            let list = ReadDenylist::build(true, &[], &[PathBuf::from(exempt)]);
            assert!(
                list.check(Path::new("/etc/sudoers")).is_ok(),
                "exempting {exempt} must reopen the literal spelling"
            );
            assert!(
                list.check(Path::new("/private/etc/sudoers")).is_ok(),
                "exempting {exempt} must reopen the resolved spelling"
            );
            list.check(Path::new("/private/etc/ssh/ssh_host_ed25519_key"))
                .unwrap_err_or_panic("an unrelated default rule stays armed");
        }
    }

    /// On macOS `/etc` is a symlink to `/private/etc`, so the machine-wide
    /// default rules were readable under their real names.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_private_spelling_of_a_machine_wide_rule_is_refused() {
        let list = ReadDenylist::build(true, &[], &[]);
        list.check(Path::new("/private/etc/sudoers"))
            .unwrap_err_or_panic("/private/etc/sudoers is /etc/sudoers");
        list.check(Path::new("/private/etc/ssh/ssh_host_ed25519_key"))
            .unwrap_err_or_panic("/private/etc/ssh is /etc/ssh");
        assert!(
            list.check(Path::new("/private/etc/hosts")).is_ok(),
            "/etc/hosts is not a credential store"
        );
    }

    // Small helper so the intent of a "must be denied" assertion reads clearly.
    trait ExpectDenied {
        fn unwrap_err_or_panic(self, message: &str);
    }
    impl ExpectDenied for Result<(), ReadDenial> {
        fn unwrap_err_or_panic(self, message: &str) {
            assert!(self.is_err(), "{message}");
        }
    }
}
