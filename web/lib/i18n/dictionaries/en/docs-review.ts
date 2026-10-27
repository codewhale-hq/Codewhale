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
    "See every file Codewhale changed in a session, roll the workspace back to an earlier turn, get a code review of a diff, and keep a receipt of that review.",
  bodyClassName: "text-ink-soft leading-relaxed",
  title: "Review what changed",
  lede:
    "Codewhale shows you each edit as it happens and keeps a snapshot of your workspace before and after every turn. Use this page to check what changed, put files back, and get a second opinion before you push.",
  sections: [
    {
      "id": "receipts",
      "title": "See what a session did",
      "blocks": [
        {
          "p": "Session receipts list recorded file changes, commands, tools, agents, approvals and failures. Review receipts below cover a separate code review."
        },
        {
          "code": "/receipts\n/receipts json\ncodewhale receipts --last --format md\nGET /v1/threads/{id}/receipt\nGET /v1/threads/{id}/turns/{turn_id}/receipt",
          "lang": "Codewhale / Terminal / HTTP"
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
          p: "Every file edit appears in the transcript as a diff when it is made. To see everything at once, run:",
        },
        { code: "/diff", lang: "Codewhale" },
        {
          p: "`/diff` shows all changes since this session started. From 0.10.2 it compares with the session's first restore point, so it also works in folders that are not git repositories, and it opens in a pager. Your own git history is untouched, so `git diff` and `git status` work as usual.",
        },
      ],
    },
    {
      id: "restore",
      title: "Roll files back to an earlier turn",
      blocks: [
        {
          p: "Before and after each turn, Codewhale snapshots the workspace into a separate git store. It never writes to your repository's own `.git`, and it works in folders that are not git repositories.",
        },
        {
          code: `/restore           # list the 20 most recent snapshots
/restore list 50   # list more (up to 100)
/restore 1         # put files back to the newest snapshot`,
          lang: "Codewhale",
        },
        {
          p: "Restoring changes files, so it needs a trusted workspace (`/trust on`) or Full Access; anyone can list snapshots. You can also ask in plain language: “undo your last edit” rolls back Codewhale's own turn.",
        },
        {
          p: "`/undo` takes back your last request. In 0.10.1 it needs `/trust on` to put files back and steps back one tool call at a time. From 0.10.2 it restores every file the request changed in any access mode and removes the request and its reply. `/restore` rolls the whole workspace back to an earlier snapshot.",
        },
        {
          note: "Snapshots are kept for 7 days. A workspace larger than 2 GB skips snapshots and Codewhale tells you once; raise `[snapshots] max_workspace_gb` in config if you want them anyway. If git is missing or the disk is full, the turn still runs without a snapshot.",
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
          p: "A review receipt records what was reviewed and what the review found, so you can prove the diff you push is the diff that was reviewed.",
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
/export file notes/session.md      # the same, written to a file
codewhale sessions export <id>     # full archive: messages, tool calls, results, artifacts`,
          lang: "Codewhale / Terminal",
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
