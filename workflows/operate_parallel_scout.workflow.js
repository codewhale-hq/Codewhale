/**
 * Operate starter — parallel scout with partial-failure synthesis.
 *
 * Dogfood source: docs/examples/dogfood-automatic/wf_a3_partial_failure_synthesis.workflow.js
 * Run: /workflow run workflows/operate_parallel_scout.workflow.js
 */
export default async function () {
  phase("Parallel scouts");
  const slots = await parallel([
    () =>
      task({
        description: "Return the string READY_A. Read-only.",
        label: "scout-a",
        type: "explore",
      }),
    () => { throw new Error("Demonstration: scout B could not produce a result"); },
    () =>
      task({
        description: "Return the string READY_C. Read-only.",
        label: "scout-c",
        type: "explore",
      }),
  ]);

  phase("Synthesize");
  const surviving = (slots || []).filter((s) => s != null);
  const summary = await task({
    label: "synthesizer",
    // Read-only synthesizer: "general" is write-capable and, without declared
    // write scope, would claim the workspace root (same as operate_read_audit).
    type: "review",
    description: [
      "Build one operator-facing summary from the surviving scout results.",
      "Explicitly note which parallel slot failed or returned null.",
      `slot_count=${(slots || []).length} surviving=${surviving.length}`,
      "dropped_slots:",
      JSON.stringify(slots.errors || []),
      "slots_json:",
      JSON.stringify(slots),
    ].join("\n"),
  });

  return {
    scenario: "WF-A3",
    slots,
    surviving_count: surviving.length,
    summary,
  };
}
