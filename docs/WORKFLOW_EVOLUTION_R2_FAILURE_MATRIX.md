# Experimental evolution R2: owners and failure matrix

This experimental path remains opt-in and review-only. The frozen task, evaluator,
execution budget, and permitted paths are host-owned. Candidate source and any
process launched from it are untrusted. `Refs #7250`; draft PR #7262.

## Existing owners

| Boundary | Existing owner | R2 change |
| --- | --- | --- |
| Candidate tasks and route | Workflow/Fleet in `crates/engine/src/tools/workflow/mod.rs` | Admit only bounded candidate requests and record actual task identities. |
| Evaluation | `crates/engine/src/tools/workflow/experimental_search.rs` | Seal code, run process-level cases through the sandbox, compare results in the host. |
| OS execution | `crates/sandbox/src/{lib,bwrap,policy}.rs` | Use Linux bubblewrap with explicit read grants and private writable scratch; reject unavailable isolation. |
| Policy schedule | `workflows/operate_best_of_n.workflow.js` | Inherit a parent genome, apply genes to actual task dispatch, and preserve lineage. |
| Experimental task runner | `scripts/experimental-evolution-smoke.py` | Freeze separate case bundles and preserve receipts for the real provider run. |

## Failure ways fixed before implementation

The independent R1 audit is input evidence. Its Python probes reproduced the
unsandboxed gate and missing untracked artifact; its JS probe used a stub host
only to inspect control flow. Existing v4–v8 hidden cases are known DEV material.

| ID | Failure to provoke | Required observed outcome |
| --- | --- | --- |
| S01 | Candidate reads or writes a synthetic file outside its sealed source/scratch | OS blocks access; marker stays unchanged. |
| S02 | Candidate guesses holdout, receipt, sibling, parent, or credential path | All are absent from its mount namespace. |
| S03 | Candidate build or install hook attempts the same access | The hook executes under identical isolation or the gate is unsupported. |
| S04 | Candidate connects to loopback, external network, or host UNIX socket | Connection fails under the frozen network policy. |
| S05 | Candidate forks, daemonizes, or changes session, then waits past timeout | Killing the bubblewrap PID namespace leaves no delayed writes. |
| S06 | bubblewrap missing, disabled, or unable to unshare | `UNSUPPORTED_ISOLATION` before candidate code executes. |
| S07 | Candidate prints `PASS`, `Ran 999 tests`, or forged JSON | Host counts frozen input/expected pairs and compares actual output. |
| S08 | Symlink, inherited FD, environment, or mount exposes trusted state | Capture rejects link; child gets minimal env/FDs and explicit mounts only. |
| A01 | Candidate adds only an untracked helper file | Sealed blob survives deletion of generator worktree and replays identically. |
| A02 | Candidate changes, deletes, renames, or changes mode/binary contents | Canonical manifest captures the complete tree effect and verifies hashes. |
| A03 | Generator tree changes after seal or CAS blob is corrupted | Former has no effect; latter rejects replay. |
| A04 | Path traversal, case collision, special file, link, oversize artifact | Capture rejects before materialization. |
| A05 | Same code generated under distinct policies | Code identity can match; execution and policy identities remain distinct. |
| E01 | G0→G1 changes gene A; G1→G2 changes gene B | G2 keeps A and applied request graph reflects both changes. |
| E02 | G0 all passes in `learn_policy` | Elite survives and a descendant still runs within budget. |
| E03 | Functional FAIL versus safety REJECTED | Only the former can parent a repair. |
| E04 | Two policies face the same DEV task/seed panel | Selection follows host results, never candidate ID or self-verdict. |
| E05 | Policy carries to a new task | Only genome crosses tasks; no previous patch or holdout answer does. |
| E06 | Mutation changes prompt text but not execution | Applied-policy witness and task events expose the mismatch. |
| E07 | Missing cost or incomplete panel | Selection marks missing and withholds cost advantage. |
| E08 | Population and generation ceilings reached | Admission stops before any extra provider request. |
| R01 | Patch exceeds reviewer excerpt length | Final reviewer receives complete bounded material or explicit incomplete status. |
| R02 | Provider response is unknown after interruption | Journal records unknown; request is not silently replayed. |
| R03 | Planner, reviewer, or repair consumes budget | All are included in the same reservation/settlement ledger. |
| R04 | Four arms use the same candidate count but different model work | Report actual calls/tokens/cost/wall and missing values. |
| R05 | New task holdout used during adaptation | Holdout stays outside selection and prompt feedback. |
| R06 | Final review fails after oracle passes | Promotion remains `NONE` and review reason is preserved. |
| R07 | Any unsafe or incomplete result appears | Report negative outcome without rewriting status. |
| I01 | Experimental flag is absent | Existing Best-of-N path retains its prior behavior. |
| I02 | Experimental flag is present | A real Fleet worker, sealed artifact, sandboxed case, host oracle, and replay appear in one run. |

## Execution evidence rule

Run the process-level cases with a real Linux bubblewrap backend. Preserve the
exact command, exit status, nonzero case count, manifest hash, artifact hash,
backend probe, stdout/stderr, and receipt paths. A fixture control-flow result
is labeled as such and never counted as model-performance evidence.
