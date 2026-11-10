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

  if (args?.evolution !== undefined) {
    if (strategy !== "search" || !args.evolution?.manifestPath || !model || !writeRoots.length) {
      throw new Error("Evolution requires strategy=search, manifestPath, model and writeRoots");
    }
    const arm = ["single", "independent", "evolution"].includes(args.evolution.arm)
      ? args.evolution.arm : "evolution";
    const evolutionaryPopulation = Math.floor(Math.min(8, Math.max(2, Number(args?.n ?? 4) || 4)));
    const evolutionaryGenerations = args.evolution.generations === 3 ? 3 : 2;
    const executionBudget = Math.floor(Math.min(16, Math.max(2,
      Number(args.evolution.executionBudget ?? evolutionaryPopulation * evolutionaryGenerations) || 8)));
    const population = arm === "single" ? 1
      : arm === "independent" ? executionBudget : evolutionaryPopulation;
    const generations = arm === "evolution" ? evolutionaryGenerations : 1;
    if (arm === "evolution" && population * generations > executionBudget) {
      throw new Error("Evolution population exceeds its frozen execution budget");
    }
    if (args.evolution.policy !== undefined) {
      throw new Error("Freeze the initial shadow policy in the host manifest");
    }
    const frozen = await task.experimentalSearch({ action: "summary" });
    const basePolicy = Object.freeze({ ...frozen.shadow_policy });
    const all = [];
    let parents = [];
    const seenFailureSignatures = new Set();
    for (let generation = 0; generation < generations; generation++) {
      if (generation > 0 && !parents.length) break;
      phase(`Evolution generation ${generation}`);
      const plans = Array.from({ length: population }, (_, index) => {
        const id = `candidate_g${generation}_${index + 1}`;
        if (generation === 0) return { id, lineage: {}, policy: basePolicy, parent: null };
        const parent = parents[index % parents.length];
        const operator = index % 2 === 0
          ? "targeted_assertion_repair" : "alternate_implementation_strategy";
        const policy = Object.freeze({
          ...basePolicy,
          planning_budget: Math.min(3, basePolicy.planning_budget + (index % 2)),
          candidate_strategy: index % 2 === 0 ? "focused" : "direct",
        });
        return {
          id,
          parent,
          policy,
          lineage: {
            parent_ids: [parent.id],
            hypothesis: operator === "targeted_assertion_repair"
              ? "The verified failing assertion identifies a repairable defect in the parent approach."
              : "The verified failure suggests replacing the parent's implementation strategy.",
            predicted_effect: "The public hard gate that failed for the parent will pass on fresh replay.",
            mutation_operator: operator,
            evidence_refs: [parent.receipt],
            target_scope: writeRoots,
            policy,
          },
        };
      });
      const replies = await parallel(plans.map((plan) => () => task({
        description: [
          "You are one isolated Codewhale coding candidate. Implement the brief in this worktree only.",
          "Do not inspect other candidates, hidden tests, evaluator internals or the parent checkout.",
          "Do not push or merge. Your self_verdict is informational.",
          `BRIEF: ${String(brief)}`,
          `RUBRIC: ${String(rubric)}`,
          `SHADOW POLICY: ${JSON.stringify(plan.policy)}`,
          `Spend at most ${plan.policy.planning_budget} short planning steps before editing.`,
          `After a failed local check, make at most ${plan.policy.max_repair_rounds} repair rounds.`,
          plan.policy.candidate_strategy === "focused"
            ? "Prefer the smallest change that addresses the verified defect."
            : "Implement the brief directly, checking edge cases against the public contract.",
          plan.policy.extra_verification
            ? "Run an additional independent local verification before returning."
            : "Run the relevant local verification before returning.",
          ...(plan.parent ? [
            `MUTATION DECLARATION: ${JSON.stringify(plan.lineage)}`,
            `VERIFIED PARENT FAILURE: ${String(plan.parent.feedback).slice(-2400)}`,
            ...(plan.policy.trace_preference === "evidence_refs"
              ? [`PARENT PATCH EXCERPT: ${String(plan.parent.patch).slice(0, 6000)}`]
              : []),
            plan.lineage.mutation_operator === "targeted_assertion_repair"
              ? "Trace the failing assertion to one code path and repair that path."
              : "Replace the parent approach where the observed failure exposes a design flaw.",
            "You start from a fresh baseline. Use the parent patch only where it supports the declared repair.",
          ] : ["Make an independent first-generation attempt."]),
          `CANDIDATE ID: ${plan.id}`,
          "Summarize the files changed and local checks. The host evaluates your worktree.",
        ].join("\n"),
        label: plan.id,
        type: "implementer",
        model,
        ...(thinking ? { thinking } : {}),
        worktree: true,
        writeAuthority: "worktree_write",
        writeRoots,
        ...(exactFiles.length ? { exactFiles } : {}),
      })), { mode: "partial" });
      for (let index = 0; index < plans.length; index++) {
        const plan = plans[index];
        let receipt;
        if (replies[index]?.__taskError || replies[index] === null) {
          receipt = { status: "UNKNOWN", reason: replies[index]?.message ?? "worker failed" };
        } else {
          try {
            receipt = await task.experimentalSearch({
              action: "evaluate", candidate_id: plan.id, generation, lineage: plan.lineage,
              policy: plan.policy,
            });
          } catch (error) {
            receipt = { status: "UNKNOWN", reason: String(error) };
          }
        }
        all.push({ id: plan.id, generation, lineage: plan.lineage, policy: plan.policy, receipt });
      }
      const failures = all.filter((item) => item.generation === generation
        && item.receipt.status === "FAIL"
        && item.receipt.public_failure_feedback);
      parents = failures.map((item) => ({
        id: item.id,
        receipt: item.receipt.receipt,
        feedback: item.receipt.public_failure_feedback,
        patch: item.receipt.patch_excerpt,
      }));
      const signatures = failures.map((item) =>
        `${item.receipt.reason}:${String(item.receipt.public_failure_feedback).slice(-400)}`);
      if (generation > 0 && signatures.length
        && signatures.every((signature) => seenFailureSignatures.has(signature))) break;
      for (const signature of signatures) seenFailureSignatures.add(signature);
    }
    const passing = all.filter((item) => item.receipt.status === "PASS");
    passing.sort((a, b) => {
      const aFootprint = a.receipt.changed_paths?.length ?? Infinity;
      const bFootprint = b.receipt.changed_paths?.length ?? Infinity;
      const aCost = a.receipt.usage?.cost_microusd ?? Infinity;
      const bCost = b.receipt.usage?.cost_microusd ?? Infinity;
      return aFootprint - bFootprint || aCost - bCost || a.id.localeCompare(b.id);
    });
    let winner = null;
    let replay = null;
    let review = null;
    if (passing.length) {
      const provisional = passing[0];
      replay = await task.experimentalSearch({
        action: "replay", candidate_id: provisional.id,
        generation: provisional.generation, lineage: provisional.lineage,
        policy: provisional.policy,
      });
      if (replay.status === "PASS" && replay.patch_sha256 === provisional.receipt.patch_sha256) {
        phase("Independent read-only review");
        review = await task({
          description: [
            "Review this verified coding patch adversarially. Do not edit or apply it.",
            `BRIEF: ${String(brief)}`,
            `PATCH: ${String(provisional.receipt.patch_excerpt).slice(0, 6000)}`,
            `HOST REPLAY: ${JSON.stringify({ status: replay.status, test_count: replay.test_count })}`,
            "Return only the required structured response.",
          ].join("\n"),
          label: "evolution_reviewer",
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
      scenario: "operate-evolution", strategy, arm, population, generations, executionBudget,
      manifest: frozen.manifest, baseline_sha: frozen.baseline_sha,
      candidates: all.map((item) => ({
        id: item.id, generation: item.generation, status: item.receipt.status,
        reason: item.receipt.reason, patch_sha256: item.receipt.patch_sha256,
        receipt: item.receipt.receipt, lineage: item.lineage, policy: item.policy,
      })),
      winner_id: winner ?? "NONE", replay, review,
      promotion: "proposal_only",
      apply_policy: "Explicit human review is required before any apply, push or merge.",
    };
  }

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
        "Summarize the files changed and local checks. The host evaluates your worktree.",
      ].join("\n"),
      label: id,
      type: "implementer",
      model,
      ...(thinking ? { thinking } : {}),
      worktree: true,
      writeAuthority: "worktree_write",
      writeRoots,
      ...(exactFiles.length ? { exactFiles } : {}),
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
