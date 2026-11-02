---
name: v4-best-practices
description: Use when working with deepseek-v4-pro or deepseek-v4-flash in thinking mode on multi-step or plan-driven tasks. Provides rules to prevent stale references, unverified plan assumptions, and vague plan output.
---

# V4 Best Practices

Rules for multi-step V4 thinking-mode workflows. Each rule prevents a
specific, observable failure class.

## 1. Verify references before writing

Before referencing a file path, function, or type in code or plan output,
call `grep_files` or `read_file` to confirm it exists in the workspace.

```
# Bad:  edit_file path="src/config/loader.rs" (assumed from memory)
# Good: grep_files pattern="pub fn load_config" → confirms src/config/mod.rs:42
#        then reference src/config/mod.rs:42
```

Failure avoided: `edit_file` errors on non-existent paths; LSP diagnostics
on hallucinated symbols.

## 2. Spawn a verifier sub-agent before multi-file execution

Before executing a plan that touches 3+ files, spawn a `deepseek-v4-flash`
sub-agent (thinking off) to read the target files and confirm path/symbol
assumptions still hold.

```
agent type="verifier" model="deepseek-v4-flash"
  prompt: "Read these files and confirm: [list assumptions]. Report mismatches."
```

Failure avoided: multi-step edits fail partway because file structure
changed since the plan was drafted.

## 3. Plan output must use confirmed path:line references

In plan-mode output, replace vague location pointers with `path:line`
references drawn from a prior `grep_files` result.

```
# Bad:  "Update the retry logic in the client module"
# Good: "Update retry loop at crates/engine/src/client.rs:187"
```

Failure avoided: agent-mode execution cannot locate the intended edit
target when plan directions are imprecise.
