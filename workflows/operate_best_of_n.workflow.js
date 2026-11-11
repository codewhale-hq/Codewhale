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
    const arm = ["single", "independent", "serial", "fixed_policy", "evolution"].includes(args.evolution.arm)
      ? args.evolution.arm : "evolution";
    const evolutionaryPopulation = Math.floor(Math.min(8, Math.max(2, Number(args?.n ?? 4) || 4)));
    const requestedGenerations = args.evolution.generations === 3 ? 3 : 2;
    const executionBudget = Math.floor(Math.min(16, Math.max(arm === "single" ? 1 : 2,
      Number(args.evolution.executionBudget ?? evolutionaryPopulation * requestedGenerations) || 8)));
    const evolutionaryGenerations = arm === "serial" ? executionBudget : requestedGenerations;
    const population = ["single", "serial"].includes(arm) ? 1
      : arm === "independent" ? executionBudget : evolutionaryPopulation;
    const generations = ["serial", "fixed_policy", "evolution"].includes(arm)
      ? evolutionaryGenerations : 1;
    if (population * generations > executionBudget) {
      throw new Error("Evolution population exceeds its frozen execution budget");
    }
    if (args.evolution.policy !== undefined) {
      throw new Error("Freeze the initial shadow policy in the host manifest");
    }
    const frozen = await task.experimentalSearch({ action: "summary" });
    if (frozen.budget_plan?.feasible === false) {
      throw new Error(`Infeasible experimental budget plan: required ${frozen.budget_plan.required_turns}, available ${frozen.budget_plan.available_turns}`);
    }
    const basePolicy = Object.freeze({ ...frozen.shadow_policy });
    const all = [];
    const selections = [];
    let parents = [];
    const mode = args.evolution.mode === "solve_task" ? "solve_task" : "learn_policy";
    const chooseDelta = (parent) => {
      const previous = Object.keys(parent.lineage?.mutation_delta ?? {});
      if (parent.status === "FAIL" && !previous.includes("planning_budget")
        && parent.policy.planning_budget < 3) {
        return { planning_budget: parent.policy.planning_budget + 1 };
      }
      if (!previous.includes("max_repair_rounds") && parent.policy.max_repair_rounds < 2) {
        return { max_repair_rounds: parent.policy.max_repair_rounds + 1 };
      }
      if (!previous.includes("extra_verification")) {
        return { extra_verification: !parent.policy.extra_verification };
      }
      return parent.policy.candidate_strategy === "direct"
        ? { candidate_strategy: "focused" } : { candidate_strategy: "direct" };
    };
    for (let generation = 0; generation < generations; generation++) {
      if (generation > 0 && !parents.length) break;
      if (generation > 0 && mode === "solve_task" && all.some((item) => item.receipt.status === "PASS")) break;
      phase(`Evolution generation ${generation}`);
      const plans = Array.from({ length: population }, (_, index) => {
        const id = `candidate_g${generation}_${index + 1}`;
        if (generation === 0) return { id, lineage: {}, policy: basePolicy, parent: null };
        const parent = parents[index % parents.length];
        const fixed = arm === "serial" || arm === "fixed_policy";
        const operator = parent.status === "FAIL"
          ? (fixed ? "fixed_policy_repair" : "targeted_assertion_repair")
          : (fixed ? "fixed_policy_elite" : "elite_reproduction");
        const mutationDelta = fixed ? {} : chooseDelta(parent);
        const policy = Object.freeze({ ...parent.policy, ...mutationDelta });
        return {
          id,
          parent,
          policy,
          lineage: {
            parent_ids: [parent.id],
            parent_policy: parent.policy,
            parent_policy_id: parent.policy_id,
            mutation_delta: mutationDelta,
            hypothesis: parent.status === "FAIL"
              ? "The verified failing assertion identifies a repairable defect in the parent approach."
              : "The verified passing parent can seed a shadow policy comparison on the same task.",
            predicted_effect: fixed
              ? (parent.status === "FAIL" ? "The frozen policy and verified public feedback will repair this task." : "The frozen policy will preserve task correctness on a fresh baseline.")
              : parent.status === "FAIL"
                ? `A fresh attempt using ${Object.keys(mutationDelta)[0]} will repair the parent's public failure.`
                : `The changed ${Object.keys(mutationDelta)[0]} will preserve task correctness with a measurable execution difference.`,
            mutation_operator: operator,
            evidence_refs: [parent.receipt],
            target_scope: writeRoots,
            policy,
          },
        };
      });
      const replies = await parallel(plans.map((plan) => async () => {
        const applied = await task.experimentalSearch({
          action: "apply_policy", candidate_id: plan.id, generation,
          policy: plan.policy, lineage: plan.lineage,
        });
        const planning = [];
        for (let step = 1; step <= plan.policy.planning_budget; step++) {
          try {
            const proposed = await task({
              description: [
                "Plan one bounded implementation step for this public coding task. Do not edit files.",
                "Reply with one JSON object containing only a plan string. Do not call tools.",
                `BRIEF: ${String(brief)}`,
                `PUBLIC FEEDBACK: ${String(applied.feedback_packet).slice(0, 10400)}`,
                `PLANNING PASS: ${step} of ${plan.policy.planning_budget}`,
              ].join("\n"),
              label: `planner_${plan.id}_${step}`,
              type: "explore", model, allowedTools: [], maxSteps: 1,
              schemaRepairAttempts: 0,
              writeAuthority: "read_only", worktree: false,
              responseSchema: {
                type: "object", additionalProperties: false,
                required: ["plan"], properties: { plan: { type: "string" } },
              },
            });
            planning.push(String(proposed.plan).slice(0, 3000));
          } catch (error) {
            planning.push(`Planner pass ${step} failed: ${String(error).slice(0, 300)}`);
          }
        }
        return task({
        description: [
          "You are one isolated Codewhale coding candidate. Implement the brief in this worktree only.",
          "Do not inspect other candidates, hidden tests, evaluator internals or the parent checkout.",
          "Read the target file before editing it. Use only the permitted file tools. The host runs all tests after you finish.",
          "Do not push or merge. Your self_verdict is informational.",
          `BRIEF: ${String(brief)}`,
          `RUBRIC: ${String(rubric)}`,
          `SHADOW POLICY: ${JSON.stringify(plan.policy)}`,
          `APPLIED POLICY ID: ${String(applied.effective_policy_id)}`,
          `HOST PLANNING NOTES: ${JSON.stringify(planning)}`,
          plan.policy.candidate_strategy === "focused"
            ? "Prefer the smallest change that addresses the verified defect."
            : "Implement the brief directly, checking edge cases against the public contract.",
          "Do not execute candidate code or local checks in the worker; the isolated host oracle does that.",
          ...(plan.parent ? [
            `MUTATION DECLARATION: ${JSON.stringify(plan.lineage)}`,
            `HOST FEEDBACK PACKET: ${String(applied.feedback_packet).slice(0, 10400)}`,
            plan.parent.status === "FAIL"
              ? "Trace the failing assertion to one code path and repair that path."
              : "Preserve the verified core approach while exploring the declared policy change.",
            "You start from the host-sealed parent code tree. Read its target files and repair the verified failure.",
          ] : ["Make an independent first-generation attempt."]),
          `CANDIDATE ID: ${plan.id}`,
          "Summarize the files changed and local checks. The host evaluates your worktree.",
        ].join("\n"),
        label: plan.id,
        type: "implementer",
        allowedTools: ["read", "write_file", "apply_patch"],
        maxSteps: 4,
        schemaRepairAttempts: 0,
        model,
        ...(thinking ? { thinking } : {}),
        worktree: true,
        ...(applied.parent_base_ref ? { worktreeBase: applied.parent_base_ref } : {}),
        writeAuthority: "worktree_write",
        writeRoots,
        ...(exactFiles.length ? { exactFiles } : {}),
        });
      }), { mode: "partial" });
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
        let devReview = null;
        if (["PASS", "FAIL"].includes(receipt.status) && plan.policy.extra_verification) {
          try {
            devReview = await task({
              description: [
                "Review the DEV candidate's complete changed files and public oracle feedback. Do not edit.",
                `BRIEF: ${String(brief)}`,
                `CHANGED FILES: ${String(receipt.review_material ?? "INCOMPLETE")}`,
                `PUBLIC FEEDBACK: ${String(receipt.public_failure_feedback ?? "")}`,
                "Reply as plain JSON text with verdict and reasons. Do not call a tool.",
              ].join("\n"),
              label: `dev_review_${plan.id}`, type: "explore", model,
              allowedTools: [], maxSteps: 1, schemaRepairAttempts: 0,
              writeAuthority: "read_only", worktree: false,
              responseSchema: {
                type: "object", additionalProperties: false,
                required: ["verdict", "reasons"],
                properties: {
                  verdict: { type: "string", enum: ["pass", "fail"] },
                  reasons: { type: "array", items: { type: "string" } },
                },
              },
            });
          } catch (error) {
            devReview = { verdict: "unknown", reasons: [String(error)] };
          }
        }
        const appliedStatus = await task.experimentalSearch({
          action: "policy_status", candidate_id: plan.id,
        }).catch((error) => ({ error: String(error) }));
        all.push({ id: plan.id, generation, lineage: plan.lineage, policy: plan.policy,
          receipt, devReview, appliedStatus });
      }
      const selection = await task.experimentalSearch({
        action: "select_generation", generation,
        candidate_ids: plans.map((plan) => plan.id),
        dev_reviews: Object.fromEntries(all.filter((item) => item.generation === generation)
          .map((item) => [item.id, item.devReview?.verdict ?? "not_requested"])),
      });
      selections.push(selection);
      const selectionCosts = new Map((selection.body.candidates ?? [])
        .map((row) => [row.id, { cost_complete: row.cost_complete,
          full_cost_microusd: row.full_cost_microusd,
          token_usage_complete: row.token_usage_complete,
          full_provider_tokens: row.full_provider_tokens }]));
      for (const item of all.filter((item) => item.generation === generation)) {
        item.selectionCost = selectionCosts.get(item.id) ?? null;
      }
      const byId = new Map(all.filter((item) => item.generation === generation)
        .map((item) => [item.id, item]));
      parents = selection.body.parent_ids.map((id) => byId.get(id)).filter(Boolean).map((item) => ({
        id: item.id,
        receipt: item.receipt.receipt,
        status: item.receipt.status,
        policy: item.policy,
        policy_id: item.receipt.policy_id,
        lineage: item.lineage,
        feedback: item.receipt.public_failure_feedback,
        material: item.receipt.review_material,
      }));
    }
    const passing = all.filter((item) => item.receipt.status === "PASS"
      && (!item.policy.extra_verification || item.devReview?.verdict === "pass"));
    passing.sort((a, b) => {
      const aFootprint = a.receipt.changed_paths?.length ?? Infinity;
      const bFootprint = b.receipt.changed_paths?.length ?? Infinity;
      const rank = (item) => item.selectionCost?.cost_complete
        ? [0, item.selectionCost.full_cost_microusd]
        : item.selectionCost?.token_usage_complete
          ? [1, item.selectionCost.full_provider_tokens] : [2, Infinity];
      const [aBasis, aUsage] = rank(a);
      const [bBasis, bUsage] = rank(b);
      return aBasis - bBasis || aUsage - bUsage || aFootprint - bFootprint
        || a.id.localeCompare(b.id);
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
      const completeIdentity = (receipt) => receipt?.review_complete === true
        && /^[0-9a-f]{64}$/.test(String(receipt.artifact_id ?? ""))
        && /^[0-9a-f]{64}$/.test(String(receipt.candidate_tree_hash ?? ""))
        && /^[0-9a-f]{64}$/.test(String(receipt.review_material_sha256 ?? ""))
        && Number.isSafeInteger(receipt.review_material_bytes)
        && receipt.review_material_bytes > 0
        && typeof receipt.review_material === "string";
      if (replay.status === "PASS" && replay.artifact_id === provisional.receipt.artifact_id
        && completeIdentity(provisional.receipt)
        && completeIdentity(replay)
        && replay.candidate_tree_hash === provisional.receipt.candidate_tree_hash
        && replay.review_material_sha256 === provisional.receipt.review_material_sha256
        && replay.review_material_bytes === provisional.receipt.review_material_bytes) {
        phase("Independent read-only review");
        const description = [
            "Review this verified coding patch adversarially. Do not edit or apply it.",
            `BRIEF: ${String(brief)}`,
            `ARTIFACT ID: ${String(provisional.receipt.artifact_id)}`,
            `TREE HASH: ${String(provisional.receipt.candidate_tree_hash)}`,
            `POLICY ID: ${String(provisional.receipt.policy_id)}`,
            `REVIEW MATERIAL SHA256: ${String(provisional.receipt.review_material_sha256)}`,
            `COMPLETE CHANGED FILES: ${String(provisional.receipt.review_material)}`,
            `HOST REPLAY: ${JSON.stringify({ status: replay.status, test_count: replay.test_count })}`,
            "Reply as plain JSON text with verdict and reasons. Do not call a tool.",
          ].join("\n");
        let attempts = 0;
        for (const label of ["evolution_reviewer", "evolution_reviewer_retry"]) {
          attempts++;
          try {
            review = await task({
              description: label === "evolution_reviewer" ? description
                : `${description}\nThe previous completed answer was a tool-only response. Return the JSON text directly.`,
              label, type: "explore", model,
              allowedTools: [], maxSteps: 1, schemaRepairAttempts: 0,
              writeAuthority: "read_only", worktree: false,
              responseSchema: {
                type: "object", additionalProperties: false,
                required: ["verdict", "reasons"],
                properties: {
                  verdict: { type: "string", enum: ["pass", "fail"] },
                  reasons: { type: "array", items: { type: "string" } },
                },
              },
            });
            break;
          } catch (error) {
            review = { verdict: "unknown", reasons: [String(error)] };
            if (label !== "evolution_reviewer"
              || !String(error).includes("Model answered only with a tool call")) break;
          }
        }
        review = { ...review,
          review_status: review.verdict === "unknown" ? "INCOMPLETE" : "COMPLETE",
          attempts,
          artifact_id: provisional.receipt.artifact_id,
          tree_hash: provisional.receipt.candidate_tree_hash,
          policy_id: provisional.receipt.policy_id,
          review_material_sha256: provisional.receipt.review_material_sha256,
          available_bytes: provisional.receipt.review_material_bytes,
          unreviewed_scope: review.verdict === "unknown" ? ["whole_artifact"] : [],
        };
        if (review.verdict === "pass" && review.review_status === "COMPLETE") winner = provisional.id;
      } else {
        review = { verdict: "unknown", review_status: "INCOMPLETE",
          reasons: ["Host replay or complete review identity was unavailable"],
          artifact_id: provisional.receipt.artifact_id,
          unreviewed_scope: ["whole_artifact"] };
      }
    }
    return {
      scenario: "operate-evolution", strategy, arm, mode, population, generations, executionBudget,
      manifest: frozen.manifest, baseline_sha: frozen.baseline_sha,
      candidates: all.map((item) => ({
        id: item.id, generation: item.generation, status: item.receipt.status,
        reason: item.receipt.reason, patch_sha256: item.receipt.patch_sha256,
        artifact_id: item.receipt.artifact_id, policy_id: item.receipt.policy_id,
        evaluation_id: item.receipt.evaluation_id,
        receipt: item.receipt.receipt, lineage: item.lineage, policy: item.policy,
        dev_review: item.devReview, applied_policy: item.appliedStatus,
        selection_cost: item.selectionCost,
      })),
      winner_id: winner ?? "NONE", replay, review, selections,
      code_selection: { winner_id: winner ?? "NONE", artifact_id:
        winner ? all.find((item) => item.id === winner)?.receipt.artifact_id : null,
        basis: winner ? "public_pass_holdout_replay_read_only_review" : "no_qualified_code" },
      policy_promotion_proposal: { status: "NOT_QUALIFIED", reason:
        "No frozen new-task policy holdout panel has been evaluated in this run.",
        proposed_policy_id: winner ? all.find((item) => item.id === winner)?.receipt.policy_id : null,
        rollback_policy_id: frozen.shadow_policy ?
          all.find((item) => item.generation === 0)?.receipt.policy_id ?? null : null },
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
    const replies = await parallel(plans.map((id) => async () => {
      const applied = await task.experimentalSearch({
        action: "apply_policy", candidate_id: id, generation: 0,
        policy: frozen.shadow_policy, lineage: {},
      });
      const planning = [];
      for (let step = 1; step <= frozen.shadow_policy.planning_budget; step++) {
        try {
          const proposed = await task({
            description: ["Plan one step for the public coding brief. Do not edit files.",
              "Reply with one JSON object containing only a plan string. Do not call tools.",
              `BRIEF: ${String(brief)}`].join("\n"),
            label: `planner_${id}_${step}`, type: "explore", model,
            allowedTools: [], maxSteps: 1, schemaRepairAttempts: 0,
            writeAuthority: "read_only", worktree: false,
            responseSchema: { type: "object", additionalProperties: false,
              required: ["plan"], properties: { plan: { type: "string" } } },
          });
          planning.push(String(proposed.plan).slice(0, 3000));
        } catch (error) {
          planning.push(`Planner pass ${step} failed: ${String(error).slice(0, 300)}`);
        }
      }
      return task({
      description: [
        "Implement the brief independently in this isolated worktree.",
        "Do not inspect other candidates, hidden tests or evaluator internals.",
        "Use only the permitted file-writing tools. The isolated host runs all checks.",
        `BRIEF: ${String(brief)}`,
        `APPLIED POLICY ID: ${String(applied.effective_policy_id)}`,
        `HOST PLANNING NOTES: ${JSON.stringify(planning)}`,
        `RUBRIC: ${String(rubric)}`,
        `CANDIDATE ID: ${id}`,
        "Summarize the files changed and local checks. The host evaluates your worktree.",
      ].join("\n"),
      label: id,
      type: "implementer",
      allowedTools: ["read", "write_file", "apply_patch"],
      maxSteps: 4,
      schemaRepairAttempts: 0,
      model,
      ...(thinking ? { thinking } : {}),
      worktree: true,
      writeAuthority: "worktree_write",
      writeRoots,
      ...(exactFiles.length ? { exactFiles } : {}),
      });
    }), { mode: "partial" });
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
      if (replay.status === "PASS" && replay.artifact_id === provisional.artifact_id
        && provisional.review_complete) {
        try {
          review = await task({
          description: [
            "Review this verified patch without editing or applying it.",
            `BRIEF: ${String(brief)}`,
            `COMPLETE CHANGED FILES: ${String(provisional.review_material)}`,
            `HOST REPLAY: ${JSON.stringify({ status: replay.status, test_count: replay.test_count })}`,
            "Return only the required structured response.",
          ].join("\n"),
          label: "evaluator_reviewer",
          type: "review", model,
          allowedTools: [], maxSteps: 1, schemaRepairAttempts: 0,
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
        } catch (error) {
          review = { verdict: "unknown", reasons: [String(error)] };
        }
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
