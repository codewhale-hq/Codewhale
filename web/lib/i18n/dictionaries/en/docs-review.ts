import type { DocsReviewDict } from "../types";

/**
 * English reference dictionary for `app/[locale]/docs/review/page.tsx`
 * ("Review what changed"). Checked against the slash-command registry
 * (`/diff`, `/restore`, `/undo`, `/review`, `/export`), the workspace
 * snapshot module (crates/snapshot/src/lib.rs), `ReviewArgs` in
 * crates/tui/src/lib.rs, and docs/RECEIPTS.md.
 */
export const docsReview: DocsReviewDict = {
  metaTitle: "Review what changed · Codewhale Docs",
  metaDescription:
    "Review recorded changes, restore available workspace snapshots and get a code review before pushing.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Review what changed",
  lede:
    "Review edits, restore available workspace snapshots and get a second opinion before pushing.",
  sections: [
    {
      "id": "receipts",
      "title": "See what a session did",
      "blocks": [
        {
          "p": "Inspect the next primary model request without sending it: [/preview-request](https://github.com/codewhale-hq/Codewhale/blob/v0.10.2/docs/PREVIEW_REQUEST.md)."
        },
        {
          "p": "A session receipt lists the files changed, the commands run and who approved each step, and ends with what it did not record. It shows no cost or token line: the footer and `/cost` show cost as a figure or as \"unknown\". Review receipts below cover a separate code review."
        },
        {
          "code": "/receipts\n/receipts json",
          "lang": "Codewhale"
        },
        { code: "codewhale receipts --last --format md", lang: "Terminal" },
        {
          code: "GET /v1/threads/{id}/receipt\nGET /v1/threads/{id}/turns/{turn_id}/receipt",
          lang: "HTTP"
        },
        {
          "p": "These surfaces only read records. They never call a provider, run tools or change session state."
        },
        {
          "note": "A Runtime receipt does not itemize files changed by shell commands. The terminal can report changes from its turn snapshots; older records may omit who decided an approval."
        }
      ]
    },
    {
      id: "diff",
      title: "See the changes",
      blocks: [
        {
          p: "File-tool edits appear in the transcript as diffs. To inspect available session changes together, run:",
        },
        { code: "/diff", lang: "Codewhale" },
        {
          p: "From v0.10.2, `/diff` uses the session's first restore point when available, so it can also work in folders without a git repository. Paths that snapshots did not capture remain outside that comparison; your own `git diff` and `git status` continue to use your repository.",
        },
      ],
    },
    {
      id: "restore",
      title: "Roll files back to an earlier turn",
      blocks: [
        {
          p: "Workspace snapshots, when available, are stored separately from your repository's `.git`. They also work in folders without a git repository.",
        },
        {
          code: `/restore           # list the 20 most recent snapshots
/restore list 50   # list more (up to 100)
/restore 1         # put files back to the newest snapshot`,
          lang: "Codewhale",
        },
        {
          p: "Restoring changes files, so it needs a trusted workspace (`/trust on`) or Full Access; anyone can list snapshots. You can also ask Codewhale to undo its last change.",
        },
        {
          p: "`/undo` reverts your last request and its recorded file changes. It refuses file reversal if restore points are unavailable or files changed afterward. `/restore` selects a workspace snapshot.",
        },
        {
          note: "Snapshots default to 7-day retention with count pruning that preserves recent turn restore points; workspaces above 2 GB skip them unless you adjust `[snapshots] max_workspace_gb`. Ignored or skipped files and external effects are not a complete backup, and a turn can run without a snapshot when git or storage is unavailable.",
        },
      ],
    },
    {
      id: "code-review",
      title: "Get a code review",
      blocks: [
        {
          p: "Inside a session, `/review` runs a structured review of a file, a diff, or a pull request. From the shell, `codewhale review` reviews a git diff and prints findings:",
        },
        {
          code: `codewhale review                      # unstaged changes in the working tree
codewhale review --staged             # what you are about to commit
codewhale review --base origin/main   # everything on this branch
codewhale review --pr 123             # a GitHub pull request (needs gh)`,
          lang: "Terminal",
        },
        {
          p: "Add `--path <file>` to review one path, `--model` to pick the reviewer, or `--json` for machine-readable output. A diff over 200,000 characters is refused rather than cut short; raise the limit with `--max-chars`.",
        },
        {
          note: "`--post` publishes the review as a comment on the pull request. Without it, nothing leaves your terminal except the model request.",
        },
      ],
    },
    {
      id: "review-receipts",
      title: "Keep a receipt of the review",
      blocks: [
        {
          p: "A review receipt records the reviewed diff fingerprint and reported findings, so you can check whether the diff changed afterward. It does not prove that the review found every defect or that the change is safe.",
        },
        {
          code: `codewhale review --base origin/main --write-receipt
codewhale review --base origin/main --check-receipt`,
          lang: "Terminal",
        },
        {
          list: [
            "`--write-receipt` saves a local JSON file after a successful review: a fingerprint of the diff, the provider and model, finding counts, unresolved risk, and a hash of the review text. It does not store the diff itself.",
            "`--check-receipt` makes no model call. It exits non-zero if the diff has changed since the receipt, if the review left unresolved risk, or if an attached check did not pass — so it works as a pre-push gate.",
            "`--receipt-path <file>` writes or reads a specific receipt instead of the latest one.",
          ],
        },
      ],
    },
    {
      id: "export",
      title: "Save the whole session",
      blocks: [
        {
          p: "To keep or share the full record of a session, export it:",
        },
        {
          code: `/export clipboard                  # a redacted copy of this session
/export file notes/session.md      # the same, written to a file`,
          lang: "Codewhale",
        },
        {
          code: "codewhale sessions export <id>",
          lang: "Terminal",
        },
        {
          p: "`codewhale sessions` lists saved sessions and their ids. The archive holds the complete context, so treat it like source code.",
        },
      ],
    },
  ],
  next: [
    {
      href: "/docs/modes",
      label: "Set modes and approvals",
      note: "Choose whether Codewhale asks before it edits or runs anything.",
    },
    {
      href: "/docs/fleet",
      label: "Run a workflow",
      note: "Repeatable, multi-step work with a record of every run.",
    },
    {
      href: "/docs/troubleshooting",
      label: "Fix a problem",
      note: "What to do when a turn stalls, a key fails, or a session will not resume.",
    },
  ],
  sourceNote:
    "Source documents: docs/RECEIPTS.md, docs/CONFIGURATION.md ([snapshots]), crates/snapshot/src/lib.rs · Update docs-map.ts when changing.",
};
