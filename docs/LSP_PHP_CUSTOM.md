# LSP: PHP Support & Custom Language Server Extension

> 阅读简体中文版：[zh_hans/LSP_PHP_CUSTOM.md](zh_hans/LSP_PHP_CUSTOM.md)
>
> v0.8.65+ | `codex/lsp-php-custom-servers`

## Overview

This feature adds **PHP** to the built-in LSP language registry and introduces a
`[lsp.custom]` config section so users can register arbitrary LSP servers by
file extension — covering languages absent from the built-in `Language` enum
(Ruby, C#, Swift, Lua, etc.).

## Changes

### 1. PHP built-in support

- `Language::Php` variant added to the enum in `crates/lsp/src/registry.rs`
- `.php` files are detected and routed to `intelephense --stdio` by default
- User can override via `[lsp.servers].php`

### 2. Custom LSP server extension

New struct `CustomLspDef` (defined in `crates/lsp/src/lib.rs` and mirrored
in `crates/config/src/lib.rs`):

```rust
pub struct CustomLspDef {
    pub language_id: String,  // LSP languageId for textDocument/didOpen
    pub command: String,      // executable to spawn
    pub args: Vec<String>,    // arguments (default empty)
}
```

New config field `LspConfig.custom: HashMap<String, CustomLspDef>` — keyed by
file extension (without dot), e.g. `"rb"`, `"cs"`, `"swift"`.

In `LspManager::diagnostics_for`, when the built-in registry returns
`Language::Other`, the manager checks the user's custom table before giving up.
Custom servers get their own lazy-spawn transport map and once-per-extension
missing-binary warnings (no log spam).

### 3. Transport generalization

`StdioLspTransport::spawn` now accepts `&str language_id` instead of
`Language`, so both built-in and custom servers share the same transport
implementation. The old `Language` import is removed from `client.rs`.

### 4. Polling pipeline extracted

`poll_diagnostics` is a new private method that both built-in and custom
diagnostics paths share — eliminating duplicated call/wait/filter/sort/truncate
logic.

## Configuration

### Built-in PHP (enabled by default if `intelephense` is on PATH)

```toml
# No config needed — PHP .php files are detected automatically.
# Override the server if desired:
[lsp.servers]
php = ["phpactor", "language-server"]
```

### Custom language servers

```toml
[lsp.custom.rb]
command = "ruby-lsp"
args = ["--stdio"]
language_id = "ruby"

[lsp.custom.cs]
command = "csharp-ls"
language_id = "csharp"

[lsp.custom.swift]
command = "sourcekit-lsp"
language_id = "swift"
```

Keys are file extensions (no leading dot). The `args` field defaults to empty.
`language_id` must match what the LSP server expects in `textDocument/didOpen`.

## Architecture

```
edit_file / write_file / apply_patch success
        │
        ▼
  LspManager.diagnostics_for(file)
        │
        ├── custom_for_extension(file) ── found? ──► transport_for_custom(ext, def)
        │                                                  │
        ├── detect_language(file) ── Other? ──► return None (skip)
        │
        └── transport_for(lang)
                │
                ▼
          poll_diagnostics(file, text, transport)
                │
                ▼
          DiagnosticBlock → injected into session message stream
```

## Verification

```
cargo test -p codewhale-tui --lib lsp::
# 32 tests passed (3 new: detects_php_extension, language_ids_for_php,
# server_for_php_is_intelephense)
cargo clippy -p codewhale-tui --lib
# lsp module: zero new warnings
```

## Files touched

| File | Change |
|------|--------|
| `crates/lsp/src/registry.rs` | +Php variant, detection, server mapping, tests |
| `crates/lsp/src/lib.rs` | +CustomLspDef, LspConfig.custom, LspManager custom fallback |
| `crates/lsp/src/client.rs` | spawn accepts &str language_id |
| `crates/tui/src/config.rs` | LspConfigToml.custom + into_runtime |
| `crates/config/src/lib.rs` | LspConfigToml.custom + CustomLspDef |
| `config.example.toml` | PHP docs + custom extension examples |
