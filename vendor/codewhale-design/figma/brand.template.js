const apply = typeof APPLY_BRAND !== 'undefined' && APPLY_BRAND === true;
const page = figma.root.children.find(node => node.id === '267:8');
if (!page) throw Error('Whale page identity changed');
await figma.setCurrentPageAsync(page);
const character = await figma.getNodeByIdAsync('269:8');
if (!character || character.type !== 'COMPONENT' || character.parent?.id !== page.id) throw Error('Inspect existing character master');
const characterName = 'Codewhale/Character legacy mark';
if (!['Codewhale/Mark', characterName].includes(character.name)) throw Error('Character master was edited; reconcile before renaming');
const matches = page.children.filter(node => node.name === 'Codewhale/Logo monochrome');
if (matches.length > 1 || (matches.length && matches[0].type !== 'COMPONENT_SET')) throw Error('Ambiguous existing logo family');
const prior = matches[0];
const unfinished = page.children.filter(node => node.name.startsWith('CW7930 pending monochrome'));
if (unfinished.length) return { blocked: true, unfinishedNodeIds: unfinished.map(node => node.id), safeToRetryWithoutCanvasRead: false, mutatedNodeIds: [], createdNodeIds: [] };
if (!apply) return { dryRun: true, pageId: page.id, existingCharacterId: character.id, rename: { before: character.name, after: characterName }, officialLogoId: prior?.id ?? null, sourceHash: CW_BRAND.sourceHash, proposedVariants: ['Surface=Light', 'Surface=Dark'], preservedNodeIds: ['269:9','269:18','269:45','269:52','269:59','269:63'], createdNodeIds: [], mutatedNodeIds: [] };
const createdNodeIds = [];
const mutatedNodeIds = [];
let logo = prior;
try {
if (!logo) {
  const variants = [];
  for (const [surface, svg] of [['Light', CW_BRAND.light], ['Dark', CW_BRAND.dark]]) {
    const component = figma.createComponent();
    createdNodeIds.push(component.id);
    page.appendChild(component);
    component.name = 'CW7930 pending monochrome ' + surface;
    component.fills = [];
    const art = figma.createNodeFromSvg(svg);
    component.appendChild(art);
    component.resize(art.width, art.height);
    art.x = 0; art.y = 0;
    component.description = 'Official monochrome C-curl from whale/source/mark.svg. Original source paths; not the blue character. Source ' + CW_BRAND.sourceHash;
    variants.push(component);
    createdNodeIds.push(art.id, ...art.findAll().map(node => node.id));
  }
  logo = figma.combineAsVariants(variants, page);
  createdNodeIds.push(logo.id);
  logo.name = 'CW7930 pending monochrome family';
  variants[0].name = 'Surface=Light';
  variants[1].name = 'Surface=Dark';
  logo.fills = [];
  logo.description = 'Official logo; black on light, near-white on dark. The existing blue character remains separate and retains its instances.';
  variants[0].x = 24; variants[0].y = 24;
  variants[1].x = variants[0].width + 48; variants[1].y = 24;
  logo.resize(variants[0].width + variants[1].width + 72, Math.max(...variants.map(node => node.height)) + 48);
  logo.x = Math.max(0, ...page.children.filter(node => node.id !== logo.id).map(node => node.x + node.width)) + 160;
  logo.y = 80;
  logo.name = 'Codewhale/Logo monochrome';
}
if (character.name !== characterName) { character.name = characterName; mutatedNodeIds.push(character.id); }
} catch (error) {
  return { failed: true, error: String(error), pageId: page.id, createdNodeIds, mutatedNodeIds, safeToRetryWithoutCanvasRead: false };
}
return { dryRun: false, pageId: page.id, logoId: logo.id, characterId: character.id, createdNodeIds, mutatedNodeIds, preservedBlueArtwork: true, existingGeometryChanged: false, instancesDetached: false, needsVisualReview: true };
