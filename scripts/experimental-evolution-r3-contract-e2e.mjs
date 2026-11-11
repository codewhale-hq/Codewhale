import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = fs.readFileSync(path.join(root, "workflows/operate_best_of_n.workflow.js"), "utf8");
const bridge = fs.readFileSync(path.join(root, "crates/engine/src/tools/workflow/mod.rs"), "utf8");
const bridgeSection = bridge.slice(bridge.indexOf("async fn experimental_search"), bridge.indexOf("fn cancel_all", bridge.indexOf("async fn experimental_search")));
const bridgeFields = [...bridgeSection.matchAll(/"([a-z_0-9]+)": value\.get\(/g)].map((match) => match[1]);
const { default: workflow } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const digest = (value) => crypto.createHash("sha256").update(String(value)).digest("hex");
const basePolicy = Object.freeze({ planning_budget: 1, max_repair_rounds: 2, extra_verification: false, candidate_strategy: "direct", trace_preference: "evidence_refs" });

async function execute(options = {}) {
  const state = { policies: {}, raw: {}, taskLabels: [], reviewerPrompts: [], candidateCalls: 0, budgetPreflight: null };
  const task = async (request) => {
    state.taskLabels.push(request.label);
    if (request.label.startsWith("planner_")) return { plan: "bounded plan" };
    if (request.label.startsWith("candidate_")) {
      state.candidateCalls += 1;
      return { delivered: true };
    }
    if (request.label.startsWith("dev_review_")) return { verdict: "pass", reasons: ["DEV observation"] };
    state.reviewerPrompts.push(request.description);
    if (options.toolOnlyFirst && state.reviewerPrompts.length === 1) {
      throw new Error("Model answered only with a tool call, and this turn offers no tools.");
    }
    if (options.reviewerFails) throw new Error("bounded reviewer unavailable");
    return { verdict: "pass", reasons: ["independent review complete"] };
  };
  task.experimentalSearch = async (request) => {
    if (request.action === "summary") {
      return { manifest: "frozen-manifest", baseline_sha: "frozen-baseline", shadow_policy: basePolicy,
        budget_plan: options.infeasible ? { feasible: false, required_turns: 44, available_turns: 37, final_review_turns: 2 }
          : { feasible: true, required_turns: 42, available_turns: 50, final_review_turns: 2 } };
    }
    if (request.action === "apply_policy") {
      state.policies[request.candidate_id] = request.policy;
      return { effective_policy_id: digest(JSON.stringify(request.policy)), feedback_packet: "public-only feedback", parent_base_ref: request.generation ? "sealed-parent" : null };
    }
    if (request.action === "policy_status") return { events: state.taskLabels.filter((label) => label.includes(request.candidate_id)) };
    if (request.action === "evaluate") {
      const value = { candidate_id: request.candidate_id, status: "PASS", reason: null,
        artifact_id: digest(request.candidate_id), candidate_tree_hash: digest(`tree:${request.candidate_id}`),
        policy_id: digest(JSON.stringify(state.policies[request.candidate_id])), evaluation_id: digest(`evaluation:${request.candidate_id}`),
        review_material: "x".repeat(10499), review_complete: true, review_material_sha256: digest("x".repeat(10499)),
        review_material_bytes: 10499, public_failure_feedback: "", changed_paths: ["src/task.py"], test_count: 41,
        receipt_path: `host-receipt-${request.candidate_id}`,
        usage: { cost_microusd: options.writerCosts?.[request.candidate_id] ?? 7 }, lineage: request.lineage };
      state.raw[request.candidate_id] = value;
      return Object.fromEntries([...bridgeFields.map((field) => [field, value[field]]), ["receipt", value.receipt_path]]);
    }
    if (request.action === "select_generation") return { body: {
      parent_ids: request.candidate_ids.slice(0, 1),
      candidates: request.candidate_ids.map((id) => ({ id,
        full_cost_microusd: options.fullCosts?.[id] ?? 7,
        cost_complete: true })),
    } };
    if (request.action === "replay") {
      const value = { ...state.raw[request.candidate_id], test_count: 54 };
      return Object.fromEntries([...bridgeFields.map((field) => [field, value[field]]), ["receipt", value.receipt_path]]);
    }
    throw new Error(`unexpected action ${request.action}`);
  };
  globalThis.task = task;
  globalThis.phase = () => {};
  globalThis.parallel = async (items) => Promise.all(items.map(async (item) => {
    try { return await item(); } catch (error) { return { __taskError: true, message: String(error) }; }
  }));
  let result = null;
  let error = null;
  try {
    result = await workflow({ strategy: "search", n: 2, model: "contract-model", writeRoots: ["src"], brief: "controlled task",
      evolution: { manifestPath: "frozen-manifest", executionBudget: 2, generations: 2, arm: "independent", mode: "learn_policy" } });
  } catch (caught) { error = caught; }
  return { state, result, error };
}

const checks = [
  ["bridge_binds_full_review_identity", async () => {
    const { state, result, error } = await execute();
    assert.equal(error, null);
    assert.equal(result.winner_id, "candidate_g0_1", JSON.stringify(result.review));
    assert.equal(result.review.tree_hash, digest("tree:candidate_g0_1"));
    assert.equal(result.review.review_material_sha256, digest("x".repeat(10499)));
    assert.equal(result.review.available_bytes, 10499);
    assert(!state.reviewerPrompts[0].includes("undefined"));
  }],
  ["failed_review_has_incomplete_coverage", async () => {
    const { result, error } = await execute({ reviewerFails: true });
    assert.equal(error, null);
    assert.equal(result.winner_id, "NONE");
    assert.equal(result.review.review_status, "INCOMPLETE");
    assert(result.review.unreviewed_scope?.includes("whole_artifact"));
  }],
  ["known_tool_only_review_has_one_bounded_retry", async () => {
    const { state, result, error } = await execute({ toolOnlyFirst: true });
    assert.equal(error, null);
    assert.equal(state.reviewerPrompts.length, 2, JSON.stringify(result.review));
    assert.equal(result.review.review_status, "COMPLETE");
    assert.equal(result.winner_id, "candidate_g0_1");
  }],
  ["infeasible_plan_fails_before_paid_tasks", async () => {
    const { state, result, error } = await execute({ infeasible: true });
    assert(error);
    assert.equal(result, null);
    assert.equal(state.candidateCalls, 0);
    assert.equal(state.taskLabels.length, 0);
    assert(String(error).includes("budget"));
  }],
  ["final_candidate_uses_full_host_cost", async () => {
    const { result, error } = await execute({
      writerCosts: { candidate_g0_1: 1, candidate_g0_2: 100 },
      fullCosts: { candidate_g0_1: 200, candidate_g0_2: 150 },
    });
    assert.equal(error, null);
    assert.equal(result.winner_id, "candidate_g0_2", JSON.stringify(result.candidates));
  }],
];

let failures = 0;
for (const [name, check] of checks) {
  try { await check(); process.stdout.write(`PASS ${name}\n`); }
  catch (error) { failures += 1; process.stdout.write(`FAIL ${name}: ${error.message}\n`); }
}
process.stdout.write(`contract checks: ${checks.length - failures} passed; ${failures} failed\n`);
process.exitCode = failures ? 1 : 0;
