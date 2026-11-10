const updateControls = typeof UPDATE_CONTROLS !== 'undefined' && UPDATE_CONTROLS === true;
const ownedSetIds = ['276:130','276:155','276:181','277:125','277:148','277:177','277:196','277:213','277:218','277:241','277:263','277:273','277:286','277:303','278:133','278:150','278:158','278:237','278:259','278:284','278:290','278:299','278:307','278:314','278:325','278:340'];
const applyControls = typeof APPLY_CONTROLS !== 'undefined' && APPLY_CONTROLS === true;
const page = figma.root.children.find(p => p.name === 'Components 5 · Controls');
if (!page) throw Error('Components 5 · Controls is missing; inspect the file before creating a page.');
await figma.setCurrentPageAsync(page);
const localVariables = await figma.variables.getLocalVariablesAsync();
const collections = await figma.variables.getLocalVariableCollectionsAsync();
const variableByCss = new Map();
for (const token of CW_CONTROLS.variables) {
  const collection = collections.find(c => c.name === token.collection);
  const variable = localVariables.find(v => v.name === token.name && v.variableCollectionId === collection?.id);
  if (variable) variableByCss.set(token.web.slice(4, -1), variable);
}
const existing = new Map(page.findAllWithCriteria({ types: ['COMPONENT_SET'] }).map(n => [n.name, n]));
const missingVariables = CW_CONTROLS.variables.filter(t => !variableByCss.has(t.web.slice(4, -1))).map(t => t.name);
const plan = CW_CONTROLS.controls.map(c => ({ name: c.name, variants: c.variants.map(v => v.state), existing: existing.get(c.name)?.id || null }));
if (!applyControls) return { dryRun: true, sourceVersion: CW_CONTROLS.version, pageId: page.id, plan, missingVariables, createdNodeIds: [], mutatedNodeIds: [] };
if (missingVariables.length) throw Error('Run figma/sync.js first; missing variables: ' + missingVariables.join(', '));
const styles = await figma.getLocalTextStylesAsync();
for (const role of CW_CONTROLS.typeRoles) {
  const style = styles.find(s => s.name === role.name);
  if (!style) throw Error('Run figma/sync.js first; missing text style ' + role.name);
  await figma.loadFontAsync(style.fontName);
}
const createdNodeIds = [];
const mutatedNodeIds = [];
const created = n => { createdNodeIds.push(n.id); return n; };
const warnings = new Set();
function cssVar(value) { return value?.match(/^var\((--[\w-]+)(?:,[^)]+)?\)$/)?.[1]; }
function resolve(value, seen = []) {
  if (!value) return null;
  const key = cssVar(value);
  if (!key) return value;
  if (seen.includes(key)) return null;
  if (key === '--glass-fill') return resolve('var(--glass-frosted-fill)', [...seen, key]);
  return resolve(CW_CONTROLS.cssVariables[key], [...seen, key]);
}
function scalar(value) {
  const v = resolve(value);
  return v && /^-?[\d.]+(?:px)?$/.test(v) ? parseFloat(v) : null;
}
function number(node, field, value) {
  const raw = scalar(value);
  if (raw === null) return;
  if ((field === 'minWidth' || field === 'minHeight') && raw === 0) { node[field] = null; return; }
  node[field] = Math.max(0, raw);
  const variable = variableByCss.get(cssVar(value));
  if (variable && variable.resolvedType === 'FLOAT') node.setBoundVariable(field, variable);
}
function boundPaint(variable, opacity = 1) {
  const resolved = variable.resolveForConsumer(page).value;
  const base = { type: 'SOLID', color: { r: resolved.r, g: resolved.g, b: resolved.b }, opacity: (resolved.a ?? 1) * opacity };
  const result = figma.variables.setBoundVariableForPaint(base, 'color', variable);
  return { ...result, opacity: (resolved.a ?? 1) * opacity };
}
function paint(value) {
  if (!value || value === 'transparent' || value === 'none') return [];
  if (value.startsWith('var(--avatar-fill')) value = 'var(--hover)';
  if (value.startsWith('var(--avatar-ground')) value = 'var(--panel)';
  let key = cssVar(value);
  if (key === '--glass-fill') key = '--glass-frosted-fill';
  let variable = variableByCss.get(key);
  if (variable?.resolvedType === 'COLOR') return [boundPaint(variable)];
  const resolved = resolve(value);
  const mix = resolved?.match(/color-mix\(in srgb, var\((--[\w-]+)\) ([\d.]+)%, transparent\)/);
  if (mix) { variable = variableByCss.get(mix[1]); if (variable) return [boundPaint(variable, Number(mix[2]) / 100)]; }
  if (value === 'currentColor') return paint('var(--text)');
  warnings.add('Unsupported paint: ' + value);
  return [];
}
function radius(node, value) {
  if (!value || !('cornerRadius' in node)) return;
  if (value === '50%') { node.cornerRadius = 999; return; }
  const variable = variableByCss.get(cssVar(value));
  const raw = scalar(value);
  if (raw !== null) node.cornerRadius = raw;
  if (variable) for (const field of ['topLeftRadius', 'topRightRadius', 'bottomLeftRadius', 'bottomRightRadius']) node.setBoundVariable(field, variable);
}
function padding(node, value) {
  if (!value) return;
  const values = value.match(/var\([^)]+\)|-?[\d.]+px|\b0\b/g);
  if (!values?.length) return;
  const [a, b = a, c = a, d = b] = values;
  for (const [field, v] of [['paddingTop', a], ['paddingRight', b], ['paddingBottom', c], ['paddingLeft', d]]) number(node, field, v);
}
function border(node, styles) {
  const match = styles.border?.match(/^([\d.]+)px solid (.+)$/);
  const color = styles['border-color'] || match?.[2];
  if (color) { node.strokes = paint(color); node.strokeWeight = match ? Number(match[1]) : 1; node.strokeAlign = 'INSIDE'; }
  for (const [key, field] of [['border-top','strokeTopWeight'],['border-bottom','strokeBottomWeight']]) {
    const edge = styles[key]?.match(/^([\d.]+)px solid (.+)$/);
    if (edge) { if (!node.strokes.length) { node.strokes = paint(edge[2]); node.strokeWeight = 0; } node[field] = Number(edge[1]); node.strokeAlign = 'INSIDE'; }
  }
  if (styles.outline && styles.outline !== 'none') { node.strokes = paint('var(--accent)'); number(node, 'strokeWeight', 'var(--focus-width)'); }
}
function textRole(styles) {
  const match = styles['font-size']?.match(/var\(--type-([\w]+)-size\)/);
  const name = match ? match[1] : styles['font-family']?.includes('mono') ? 'code' : 'control';
  return name;
}
async function render(spec, parent, inherited = {}) {
  const s = { ...inherited, ...spec.styles };
  if (spec.inheritColor && inherited.color) s.color = inherited.color;
  if (spec.componentId) {
    const component = await figma.getNodeByIdAsync(spec.componentId);
    const instance = created(component.createInstance());
    parent.appendChild(instance);
    instance.name = spec.name;
    const labels = instance.findAllWithCriteria({ types: ['TEXT'] });
    for (const label of labels) { await figma.loadFontAsync(label.fontName); label.characters = spec.name; }
    return instance;
  }
  if (spec.svg) {
    const node = created(figma.createNodeFromSvg(spec.svg));
    node.name = spec.name;
    parent.appendChild(node);
    node.resize(16, 16);
    for (const child of node.findAllWithCriteria({ types: ['VECTOR'] })) { createdNodeIds.push(child.id); if (child.strokes.length) child.strokes = paint(s.color || 'var(--text)'); if (child.fills.length) child.fills = paint(s.color || 'var(--text)'); }
    return node;
  }
  if (spec.text !== undefined) {
    const node = created(figma.createText());
    node.name = spec.name;
    parent.appendChild(node);
    const role = textRole(s);
    const canonical = styles.find(t => t.name === 'CW 5/' + role[0].toUpperCase() + role.slice(1));
    const fontSize = scalar(s['font-size']) || canonical.fontSize;
    const sourceLine = s['line-height'];
    const lineHeight = sourceLine && /^[\d.]+$/.test(sourceLine) ? Number(sourceLine) * fontSize : scalar(sourceLine) || canonical.lineHeight.value;
    const weight = scalar(s['font-weight']) || 400;
    const family = s['font-family']?.includes('mono') ? 'Geist Mono' : canonical.fontName.family;
    const weightOf = style => style.fontName.variationSettings?.wght || ({ Regular: 400, Medium: 500, SemiBold: 600 }[style.fontName.style] || 400);
    const matching = style => style.fontName.family === family && style.fontSize === fontSize && style.lineHeight.unit === 'PIXELS' && Math.abs(style.lineHeight.value - lineHeight) < .01 && weightOf(style) === weight;
    let style = matching(canonical) ? canonical : styles.find(matching);
    if (!style) {
      const font = { family, style: 'Regular', variationSettings: { wght: weight } };
      await figma.loadFontAsync(font);
      style = figma.createTextStyle();
      style.name = 'CW 5/Source/' + (family === 'Geist Mono' ? 'Code' : 'Text') + ' ' + fontSize + '/' + lineHeight + ' ' + weight;
      style.fontName = font;
      style.fontSize = fontSize;
      style.lineHeight = { unit: 'PIXELS', value: lineHeight };
      styles.push(style);
    }
    await figma.loadFontAsync(style.fontName);
    await node.setTextStyleIdAsync(style.id);
    node.characters = spec.text;
    node.fills = paint(s.color || 'var(--text)');
    if (spec.width || spec.wrap) { node.textAutoResize = 'HEIGHT'; node.resize(spec.width || Math.max(1, parent.width - parent.paddingLeft - parent.paddingRight), node.height); }
    if (s.flex === '1') node.layoutGrow = 1;
    if (s['text-align'] === 'center') node.textAlignHorizontal = 'CENTER';
    return node;
  }
  const node = created(figma.createAutoLayout());
  node.name = spec.name;
  parent.appendChild(node);
  node.layoutMode = spec.layout || (s['flex-direction'] === 'column' ? 'VERTICAL' : 'HORIZONTAL');
  node.primaryAxisSizingMode = 'AUTO';
  node.counterAxisSizingMode = 'AUTO';
  node.fills = paint(spec.styles.background || spec.styles['background-color']);
  const mixAlpha = (spec.styles.background || '').match(/color-mix\(in srgb, var\(--[\w-]+\) ([\d.]+)%, transparent\)/);
  if (mixAlpha) node.opacity = Number(mixAlpha[1]) / 100;
  node.clipsContent = s.overflow === 'hidden';
  number(node, 'itemSpacing', s.gap);
  padding(node, s.padding);
  for (const [css, field] of [['padding-top', 'paddingTop'], ['padding-right', 'paddingRight'], ['padding-bottom', 'paddingBottom'], ['padding-left', 'paddingLeft'], ['min-width', 'minWidth'], ['min-height', 'minHeight']]) number(node, field, s[css]);
  radius(node, s['border-radius']);
  border(node, s);
  if (spec.alignCross === 'CENTER') node.counterAxisAlignItems = 'CENTER';
  if (s['align-items'] === 'center' || spec.align === 'CENTER') node.counterAxisAlignItems = 'CENTER';
  if (s['justify-content'] === 'flex-end') node.primaryAxisAlignItems = 'MAX';
  if (s['justify-content'] === 'center' || spec.align === 'CENTER') node.primaryAxisAlignItems = 'CENTER';
  const ownWidth = spec.width ?? (s.width === '100%' ? Math.max(1, parent.width - parent.paddingLeft - parent.paddingRight) : scalar(s.width));
  const ownHeight = spec.height ?? scalar(s.height);
  if (ownWidth !== null && ownWidth !== undefined) { node.resize(ownWidth, node.height); node.counterAxisSizingMode = node.layoutMode === 'VERTICAL' ? 'FIXED' : node.counterAxisSizingMode; node.primaryAxisSizingMode = node.layoutMode === 'HORIZONTAL' ? 'FIXED' : node.primaryAxisSizingMode; }
  if (ownHeight !== null && ownHeight !== undefined) { node.resize(node.width, ownHeight); node.primaryAxisSizingMode = node.layoutMode === 'VERTICAL' ? 'FIXED' : node.primaryAxisSizingMode; node.counterAxisSizingMode = node.layoutMode === 'HORIZONTAL' ? 'FIXED' : node.counterAxisSizingMode; }
  if (spec.shape === 'CIRCLE') node.cornerRadius = 999;
  const inheritable = Object.fromEntries(['color', 'font-size', 'line-height', 'font-family', 'font-weight'].filter(k => s[k]).map(k => [k, s[k]]));
  for (const child of spec.children) await render(child, node, inheritable);
  if (node.children.length === 0) node.resize(ownWidth || scalar(s['min-width']) || 16, ownHeight || (spec.shape === 'ELLIPSE' ? ownWidth : scalar(s['min-height'])) || 16);
  return node;
}
const right = Math.max(0, ...page.children.map(n => n.x + n.width)) + 160;
const createdFamilies = [];
let y = 80;
for (const family of CW_CONTROLS.controls) {
  const prior = existing.get(family.name);
  if (prior && (!updateControls || !ownedSetIds.includes(prior.id))) continue;
  const components = [];
  for (const variant of family.variants) {
    const previous = prior?.children.find(c => c.name === 'State=' + variant.state);
    const component = previous || created(figma.createComponent());
    if (previous) mutatedNodeIds.push(previous.id);
    if (previous) for (const child of [...component.children]) child.remove();
    component.name = 'State=' + variant.state;
    component.layoutMode = 'VERTICAL';
    component.primaryAxisSizingMode = 'AUTO';
    component.counterAxisSizingMode = 'AUTO';
    component.fills = [];
    component.description = 'From codewhale-app/packages/ui/src/components; token version ' + CW_CONTROLS.version + '. State: ' + variant.state + '. See figma/controls.manifest.json for exact source digests. Keyboard and behavior remain owned by the React control.';
    if (!previous) page.appendChild(component);
    await render(variant.tree, component);
    components.push(component);
  }
  const set = prior || created(figma.combineAsVariants(components, page));
  if (prior) mutatedNodeIds.push(prior.id);
  set.name = family.name;
  set.fills = paint('var(--panel)');
  set.description = 'Source-aligned ' + family.name + '. Variables bind to CW 5. Imported specimen content is editable; layout widths are examples, native control metrics come from source CSS.';
  const maxWidth = Math.max(...components.map(c => c.width));
  const maxHeight = Math.max(...components.map(c => c.height));
  for (let i = 0; i < components.length; i++) { components[i].x = 24 + (i % 3) * (maxWidth + 32); components[i].y = 24 + Math.floor(i / 3) * (maxHeight + 32); }
  set.resize(48 + Math.min(3, components.length) * (maxWidth + 32) - 32, 48 + Math.ceil(components.length / 3) * (maxHeight + 32) - 32);
  if (!prior) { set.x = right; set.y = y; }
  y += set.height + 96;
  createdFamilies.push({ name: family.name, id: set.id, variants: components.map(c => ({ id: c.id, name: c.name, width: c.width, height: c.height })) });
}
return { dryRun: false, sourceVersion: CW_CONTROLS.version, createdFamilies, createdNodeIds, mutatedNodeIds, warnings: [...warnings], skippedExistingFamilies: plan.filter(p => p.existing), needsVisualReview: true };
