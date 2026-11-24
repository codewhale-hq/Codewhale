/** Public model presentation from the same reviewed Engine catalog owner.
 * Live/configured/account inventories never enter this projection. */
export function parseModelCatalog(input) {
  let data;
  try { data = typeof input === "string" ? JSON.parse(input) : input; } catch { return null; }
  const reviewed = data?.reviewed;
  if (!reviewed || typeof reviewed.revision !== "string" || !reviewed.revision ||
      !reviewed.intrinsic || typeof reviewed.intrinsic !== "object" || Array.isArray(reviewed.intrinsic) ||
      !Array.isArray(reviewed.public_models) || !reviewed.public_models.length) return null;
  const safeText = value => typeof value === "string" && value.length > 0 && value.length <= 512 &&
    value.trim() === value && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
  const nullableNumber = value => value === undefined || value === null ||
    (Number.isSafeInteger(value) && value > 0 && value <= 0xffffffff);
  const validDate = value => typeof value === "string" && /^\d{4}-\d{2}-\d{2}$/.test(value) &&
    Number.isFinite(Date.parse(`${value}T00:00:00Z`)) && new Date(`${value}T00:00:00Z`).toISOString().slice(0, 10) === value;
  const rows = [];
  const ids = new Set();
  for (const row of reviewed.public_models) {
    if (!row || !safeText(row.id) || ids.has(row.id) ||
        !(row.label === null || safeText(row.label)) ||
        !(row.added_at === null || validDate(row.added_at)) ||
        !Array.isArray(row.aliases) || !row.aliases.every(safeText)) return null;
    const fact = reviewed.intrinsic[row.id.toLowerCase()];
    if (!fact || !safeText(fact.source) || !nullableNumber(fact.context_window) || !nullableNumber(fact.max_output) ||
        !(fact.reasoning === undefined || fact.reasoning === null || typeof fact.reasoning === "boolean")) return null;
    ids.add(row.id);
    rows.push({ id: row.id, provider: row.label, contextWindow: fact.context_window ?? null,
      maxOutput: fact.max_output ?? null, reasoning: fact.reasoning === true, addedAt: row.added_at });
  }
  rows.sort((a, b) => a.addedAt && b.addedAt && a.addedAt !== b.addedAt ? b.addedAt.localeCompare(a.addedAt) :
    a.addedAt !== b.addedAt ? a.addedAt ? -1 : 1 : a.id.localeCompare(b.id));
  return rows;
}
