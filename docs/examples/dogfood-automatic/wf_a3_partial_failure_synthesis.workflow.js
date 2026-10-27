/**
 * #4131 WF-A3 — partial failure + synthesis.
 *
 * parallel() uses all-settled semantics: a failed slot becomes null; the run
 * continues so a synthesizer can still produce an operator summary. The slot
 * is null but not anonymous — `slots.errors` carries `{ index, kind, message }`
 * for every drop, so the summary can name what was lost instead of guessing.
 *
 * Run: /workflow run docs/examples/dogfood-automatic/wf_a3_partial_failure_synthesis.workflow.js
 *
 * For pure VM proof without model spend, use workflow-js unit tests:
 *   cargo test -p codewhale-workflow-js --locked parallel_fan_out_maps_one_failure_to_null_slot
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
  // The typed failure ledger: which slot died, and of what.
  const dropped = (slots.errors || []).map(
    (e) => `slot ${e.index} (${e.kind}): ${e.message}`,
  );
  for (const line of dropped) {
    log(`dropped: ${line}`);
  }
  const summary = await task({
    label: "synthesizer",
    type: "review",
    description: [
      "Build one operator-facing summary from the surviving scout results.",
      "Explicitly note which parallel slot failed, and why.",
      `slot_count=${(slots || []).length} surviving=${surviving.length}`,
      "dropped_slots:",
      dropped.length ? dropped.join("\n") : "(none)",
      "slots_json:",
      JSON.stringify(slots),
    ].join("\n"),
  });

  return {
    scenario: "WF-A3",
    slots,
    surviving_count: surviving.length,
    dropped_slots: dropped,
    summary,
  };
}
