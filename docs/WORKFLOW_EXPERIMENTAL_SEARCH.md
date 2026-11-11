# Workflow experimental search

Workflow remains the scheduler; Fleet and Runtime still own child routing and
worktrees. The checked-in `workflows/operate_best_of_n.workflow.js` has an
optional host evaluator for completed search candidates. With no `evaluator`
argument, its existing Best-of-N generation and reviewer path runs as before.

## Verified candidate slice

The version 1 JSON spec freezes a clean Git HEAD, objective, write scope,
protected paths, trusted case bundles, Linux bubblewrap policy, and wall-time
bound. Keep the spec and case bundles outside the candidate repository. A case
bundle is an array of `{"input": ..., "expected": ...}` objects. At least one
public and one held-out gate must each require a nonzero number of cases.

```json
{
  "version": 1,
  "objective": "Increment the input integer",
  "baseline_sha": "<full output of git rev-parse HEAD>",
  "allowed_write_roots": ["candidate.py"],
  "protected_paths": ["tests"],
  "isolation_backend": "linux-bwrap",
  "max_wall_secs": 60,
  "hard_gates": [
    {
      "candidate_argv": ["/usr/bin/python3", "candidate.py"],
      "cases_path": "/trusted/public.json",
      "timeout_secs": 10,
      "min_tests": 1,
      "holdout": false
    },
    {
      "candidate_argv": ["/usr/bin/python3", "candidate.py"],
      "cases_path": "/trusted/heldout.json",
      "timeout_secs": 10,
      "min_tests": 1,
      "holdout": true
    }
  ]
}
```

Invoke the existing `workflow` tool with a checked-in source path, or use the
CLI `workflow-tool --input-json` equivalent:

```json
{
  "action": "run",
  "source_path": "workflows/operate_best_of_n.workflow.js",
  "args": {
    "strategy": "search",
    "brief": "Increment the input integer",
    "n": 2,
    "writeRoots": ["candidate.py"],
    "evaluator": {"manifestPath": "/trusted/evaluator.json"}
  }
}
```

The ordinary model and provider configuration still controls candidate writers.
The host freezes the evaluator before starting the Workflow. Once a candidate
task has terminated successfully, the host verifies its Git common directory
and starting HEAD, seals its complete tree difference, and runs the public
cases. A public PASS is independently materialized and replayed against public
and held-out cases. The Workflow returns each receipt with
`promotion: "proposal_only"`; it does not choose, apply, push, or merge code.

The case bundles are the trusted oracle. The candidate command receives one
JSON input on stdin and emits one JSON value on stdout. The host compares that
value with the frozen expected value and counts the cases it actually ran.
Candidate self-verdicts, shell output, and claimed test counts are never the
oracle. The oracle is read by the host, not imported into the candidate
process.

## Isolation and retained evidence

Candidate commands run only on Linux with the strict bubblewrap backend.
The capability probe must establish a private network namespace, inaccessible
host marker, writable private scratch, and read-only sealed source mount.
The process owner applies CPU, address-space, file-size, descriptor, and process
limits. The evaluator clears the host environment and closes inherited file
descriptors before the sandbox starts. Missing bubblewrap or an unsuccessful
probe rejects the run.

The private run-owned directory (mode 0700 on Unix) is reported as `run_root` and named
`codewhale-evaluator-workflow_*` in the system temporary directory. It retains
the frozen baseline blobs, sealed artifact manifests and blobs, manifest,
receipts, and bounded stdout/stderr logs so evidence remains reviewable after
the writer worktree disappears. Each evaluation's materialized candidate and
scratch directories use temporary directory ownership and are removed when
the evaluation ends, including on failure or cancellation. An unsuccessful
freeze removes the new run directory. After archiving evidence, the operator
may delete that exact run directory; restart cleanup must identify the
`manifest.json` run id before removing anything. There is no automatic
cross-process replay API in this slice.

The full sealed artifact records added, modified, deleted, binary, and
executable-mode changes. Its tree hash is separate from the artifact manifest
hash. Receipts bind spec, baseline Git SHA and tree identity, case bundle
hashes, artifact and candidate tree hashes, sandbox probe, and native binary
SHA256, plus bubblewrap and gate executable hashes. Replays compare these
against the frozen spec and prior receipt.
Missing or conflicting evidence yields `UNKNOWN`, `REJECTED`, or an explicit
error, never a PASS.

Limits are 4 MiB per file, 64 MiB for the baseline or candidate tree, 8 MiB
of changed content, 4,096 files, 256 cases per gate, and at most 12 gates.
The sandbox has per-process and per-case bounds; there is no aggregate scratch
or host-memory quota. Windows, macOS, and Linux hosts without working
bubblewrap are unsupported for this evaluator. A candidate can run only an
absolute executable with one repo-relative program path argument in this
version. These restrictions deliberately keep the trusted execution surface
small.

Policy genomes, mutation, selection, strategy transfer, model-turn planning,
and promotion research remain separate from this evaluator.
