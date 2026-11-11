const DRY = typeof DRY_RUN !== "undefined" && DRY_RUN;
const rgba = (v) => {
  const n = parseInt(v.hex, 16);
  return { r: ((n >> 16) & 255) / 255, g: ((n >> 8) & 255) / 255, b: (n & 255) / 255, a: v.alpha };
};
const same = (a, b) =>
  typeof a === "number" || typeof b === "number"
    ? a === b
    : !!a && !!b && ["r", "g", "b", "a"].every((k) => Math.abs((a[k] ?? 1) - (b[k] ?? 1)) < 0.002);
const report = { version: PAYLOAD.version, dry: DRY, created: [], updated: [], unchanged: 0, notInTokens: [], styles: { created: [], updated: [], unchanged: 0 } };
const collections = await figma.variables.getLocalVariableCollectionsAsync();
const variables = await figma.variables.getLocalVariablesAsync();
for (const wanted of PAYLOAD.collections) {
  let collection = collections.find((c) => c.name === wanted.name);
  if (!collection) {
    if (DRY) {
      report.created.push(...wanted.variables.map((v) => `${wanted.name}: ${v.name}`));
      continue;
    }
    collection = figma.variables.createVariableCollection(wanted.name);
    collection.renameMode(collection.modes[0].modeId, wanted.modes[0]);
    for (const mode of wanted.modes.slice(1)) collection.addMode(mode);
  }
  const modeId = Object.fromEntries(collection.modes.map((m) => [m.name, m.modeId]));
  const missingMode = wanted.modes.find((m) => !modeId[m]);
  if (missingMode) throw new Error(`${wanted.name} has no mode named ${missingMode}`);
  const existing = variables.filter((v) => v.variableCollectionId === collection.id);
  const names = new Set(wanted.variables.map((v) => v.name));
  report.notInTokens.push(...existing.filter((v) => !names.has(v.name)).map((v) => `${wanted.name}: ${v.name}`));
  for (const token of wanted.variables) {
    let variable = existing.find((v) => v.name === token.name);
    if (!variable) {
      report.created.push(`${wanted.name}: ${token.name}`);
      if (DRY) continue;
      variable = figma.variables.createVariable(token.name, collection, token.type);
      variable.scopes = token.scopes;
      variable.setVariableCodeSyntax("WEB", token.web);
    } else if (!DRY && (variable.codeSyntax.WEB || "") !== token.web) {
      variable.setVariableCodeSyntax("WEB", token.web);
    }
    let changed = false;
    for (const mode of wanted.modes) {
      const next = token.type === "COLOR" ? rgba(token.values[mode]) : token.values[mode];
      if (same(variable.valuesByMode[modeId[mode]], next)) continue;
      changed = true;
      if (!DRY) variable.setValueForMode(modeId[mode], next);
    }
    if (changed && !report.created.includes(`${wanted.name}: ${token.name}`)) report.updated.push(`${wanted.name}: ${token.name}`);
    if (!changed) report.unchanged += 1;
  }
}
const textStyles = await figma.getLocalTextStylesAsync();
for (const wanted of PAYLOAD.textStyles) {
  const font = { family: wanted.family, style: wanted.style };
  let style = textStyles.find((s) => s.name === wanted.name);
  const matches =
    style &&
    style.fontName.family === font.family &&
    style.fontName.style === font.style &&
    style.fontSize === wanted.size &&
    style.lineHeight.unit === "PIXELS" &&
    style.lineHeight.value === wanted.line;
  if (matches) {
    report.styles.unchanged += 1;
    continue;
  }
  report.styles[style ? "updated" : "created"].push(wanted.name);
  if (DRY) continue;
  await figma.loadFontAsync(font);
  if (!style) {
    style = figma.createTextStyle();
    style.name = wanted.name;
  }
  style.fontName = font;
  style.fontSize = wanted.size;
  style.lineHeight = { unit: "PIXELS", value: wanted.line };
}
return report;
