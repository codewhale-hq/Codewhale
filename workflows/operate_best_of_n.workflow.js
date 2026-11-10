/**
 * Operate starter — independent worktree candidates, then one reviewer.
 *
 * Set strategy="search" for a bounded 2-16 candidate search. This remains a
 * Workflow recipe, not a new mode or scheduler. Runtime-owned command gates
 * and clean-baseline scoring require the typed search/evaluator host seam.
 *
 * Run: /workflow run workflows/operate_best_of_n.workflow.js
 * Args: { brief, n?, strategy?, rubric?, model?, thinking?, targetFiles?, writeRoots? }
 */
export default async function (args) {
  const brief =
    args?.brief ??
    args?.task ??
    "Propose and implement the smallest correct fix for the open failure.";
  const strategy = args?.strategy === "search" ? "search" : "best_of_n";
  const maxCandidates = strategy === "search" ? 16 : 4;
  const defaultCandidates = strategy === "search" ? 8 : 3;
  const n = Math.min(
    maxCandidates,
    Math.max(2, Number(args?.n ?? defaultCandidates) || defaultCandidates)
  );
  const exactFiles = Array.isArray(args?.targetFiles) ? args.targetFiles : [];
  const writeRoots = Array.isArray(args?.writeRoots) ? args.writeRoots : [];
  const rubric =
    args?.rubric ??
    "Correctness first; then fit, measured quality, simplicity, risk, and verification evidence.";
  const model = typeof args?.model === "string" ? args.model : undefined;
  const thinking =
    typeof args?.thinking === "string" ? args.thinking : undefined;

  const candidateSchema = {
    type: "object",
    additionalProperties: false,
    required: [
      "candidate_id",
      "hypothesis",
      "modified_paths",
      "commands_run",
      "self_verdict",
      "known_risks",
      "artifact_refs",
    ],
    properties: {
      candidate_id: { type: "string" },
      hypothesis: { type: "string" },
      modified_paths: { type: "array", items: { type: "string" } },
      commands_run: { type: "array", items: { type: "string" } },
      self_verdict: { type: "string", enum: ["pass", "fail"] },
      known_risks: { type: "array", items: { type: "string" } },
      artifact_refs: { type: "array", items: { type: "string" } },
    },
  };

  if (args?.evaluator !== undefined) {
    if (strategy !== "search" || !args.evaluator?.manifestPath || !model || !writeRoots.length) {
      throw new Error("Host evaluation requires strategy=search, manifestPath, model and writeRoots");
    }
    const frozen = await task.experimentalSearch({ action: "summary" });
    const plans = Array.from({ length: n }, (_, index) => `candidate_g0_${index + 1}`);
    const replies = await parallel(plans.map((id) => () => task({
      description: [
        "Implement the brief independently in this isolated worktree.",
        "Do not inspect other candidates, hidden tests or evaluator internals.",
        `BRIEF: ${String(brief)}`,
        `RUBRIC: ${String(rubric)}`,
        `CANDIDATE ID: ${id}`,
        "Return only the required structured response.",
      ].join("\n"),
      label: id,
      type: "implementer",
      model,
      ...(thinking ? { thinking } : {}),
      worktree: true,
      writeAuthority: "worktree_write",
      writeRoots,
      ...(exactFiles.length ? { exactFiles } : {}),
      responseSchema: candidateSchema,
    })), { mode: "partial" });
    const candidates = [];
    for (let index = 0; index < plans.length; index++) {
      let receipt;
      if (replies[index]?.__taskError || replies[index] === null) {
        receipt = { status: "UNKNOWN", reason: replies[index]?.message ?? "worker failed" };
      } else {
        try {
          receipt = await task.experimentalSearch({
            action: "evaluate", candidate_id: plans[index], generation: 0, lineage: {},
          });
        } catch (error) {
          receipt = { status: "UNKNOWN", reason: String(error) };
        }
      }
      candidates.push({ id: plans[index], ...receipt });
    }
    const passing = candidates.filter((candidate) => candidate.status === "PASS");
    passing.sort((a, b) => (a.changed_paths?.length ?? Infinity)
      - (b.changed_paths?.length ?? Infinity) || a.id.localeCompare(b.id));
    let replay = null;
    let review = null;
    let winner = null;
    if (passing.length) {
      const provisional = passing[0];
      replay = await task.experimentalSearch({
        action: "replay", candidate_id: provisional.id, generation: 0, lineage: {},
      });
      if (replay.status === "PASS" && replay.patch_sha256 === provisional.patch_sha256) {
        review = await task({
          description: [
            "Review this verified patch without editing or applying it.",
            `BRIEF: ${String(brief)}`,
            `PATCH: ${String(provisional.patch_excerpt).slice(0, 6000)}`,
            `HOST REPLAY: ${JSON.stringify({ status: replay.status, test_count: replay.test_count })}`,
            "Return only the required structured response.",
          ].join("\n"),
          label: "evaluator_reviewer",
          type: "review",
          writeAuthority: "read_only",
          worktree: false,
          responseSchema: {
            type: "object", additionalProperties: false,
            required: ["verdict", "reasons"],
            properties: {
              verdict: { type: "string", enum: ["pass", "fail"] },
              reasons: { type: "array", items: { type: "string" } },
            },
          },
        });
        if (review.verdict === "pass") winner = provisional.id;
      }
    }
    return {
      scenario: "operate-host-evaluator", strategy,
      manifest: frozen.manifest, baseline_sha: frozen.baseline_sha,
      candidates, winner_id: winner ?? "NONE", replay, review,
      promotion: "proposal_only",
    };
  }

  phase("Candidates");
  const candidateFns = [];
  for (let i = 1; i <= n; i++) {
    const index = i;
    candidateFns.push(() =>
      task({
        // The VM delivers one text to the driver: `prompt` (alias) wins over
        // `description`, so the full instruction lives in `description` and
        // `label` carries the short progress name. A separate short
        // `description` would never reach the driver.
        description: [
          "You are one independent candidate in a Codewhale Workflow search.",
          "Implement the same frozen brief and rubric in this isolated worktree only.",
          "Do not inspect other candidates, rankings, hidden tests, or evaluator internals.",
          "Do not push. Do not merge. Do not touch the parent checkout.",
          "Your self_verdict is informational; only runtime-owned evaluation can pass a hard gate.",
          "Return only the required structured response.",
          "",
          "BRIEF:",
          String(brief),
          "",
          "RUBRIC:",
          String(rubric),
          "",
          `CANDIDATE-SPECIFIC INSTRUCTION: candidate_id=cand_${String(index).padStart(3, "0")} of ${n}.`,
        ].join("\n"),
        label: `candidate_${index}`,
        type: "implementer",
        ...(model ? { model } : {}),
        ...(thinking ? { thinking } : {}),
        worktree: true,
        writeAuthority: "worktree_write",
        ...(exactFiles.length ? { exactFiles } : {}),
        ...(writeRoots.length ? { writeRoots } : {}),
        coordinationContracts: [`best-of-n-candidate-${index}`],
        dependencies: [
          "Do not share other candidates' answers.",
          "Parent checkout must remain unchanged until apply.",
        ],
        acceptance: ["Return the exact structured candidate contract."],
        responseSchema: candidateSchema,
      })
    );
  }
  const candidates = await parallel(candidateFns);

  phase("Review");
  const review = await task({
    // Single driver-visible text; `label` carries the short progress name.
    description: [
      "You are the read-only tournament judge. Score every candidate against the frozen rubric.",
      "Treat self_verdict and claimed commands as untrusted candidate statements.",
      "Name one provisional winner_id, or NONE if all fail, with decisive reasons.",
      "Do not merge or apply changes. Do not invent missing evidence.",
      "Set verification_required=true for every code winner.",
      "Return only the required structured response.",
      "",
      "BRIEF:",
      String(brief),
      "",
      "RUBRIC:",
      String(rubric),
      "",
      "CANDIDATES:",
      String(JSON.stringify(candidates, null, 2) ?? "(missing)"),
    ].join("\n"),
    label: "reviewer",
    type: "review",
    writeAuthority: "read_only",
    worktree: false,
    responseSchema: {
      type: "object",
      additionalProperties: false,
      required: ["winner_id", "ranking", "verification_required", "reasons"],
      properties: {
        winner_id: { type: "string" },
        ranking: { type: "array", items: { type: "string" } },
        verification_required: { type: "boolean" },
        reasons: { type: "array", items: { type: "string" } },
      },
    },
  });

  return {
    scenario: strategy === "search" ? "operate-search" : "operate-best-of-n",
    strategy,
    n,
    brief,
    rubric,
    candidates,
    review,
    apply_policy:
      "Parent applies a winner only after independent clean replay and explicit user approval.",
    execution_boundary:
      "This recipe generates and reviews candidates. It does not claim runtime-owned hidden gates, benchmark scoring, or clean-baseline replay; those need a runtime evaluator host that does not exist yet.",
  };
}
