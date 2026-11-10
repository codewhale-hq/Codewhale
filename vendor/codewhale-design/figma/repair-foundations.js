const page = await figma.getNodeByIdAsync('229:3');
await figma.setCurrentPageAsync(page);
const title = await figma.getNodeByIdAsync('271:9');
for (const segment of title.getStyledTextSegments(['fontName'])) await figma.loadFontAsync(segment.fontName);
title.characters = 'CW 5 · Metrics, tints and motion · tokens 5.3.0';
title.name = title.characters;
const focus = await figma.getNodeByIdAsync('271:143');
focus.clipsContent = false;
focus.strokeAlign = 'OUTSIDE';
focus.x = 5;
focus.y = 66;
const section = await figma.getNodeByIdAsync('271:140');
section.clipsContent = false;
const scrim = await figma.getNodeByIdAsync('271:133');
const parent = await figma.getNodeByIdAsync('271:132');
const createdNodeIds=[];
let ground = parent.children.find(n=>n.name==='Scrim contrast ground');
if (!ground) {
 ground=figma.createFrame();createdNodeIds.push(ground.id);ground.name='Scrim contrast ground';ground.resize(112,44);ground.fills=[];ground.clipsContent=true;ground.cornerRadius=10;parent.insertChild(0,ground);ground.layoutPositioning='AUTO';ground.x=0;ground.y=0;
 const vars=await figma.variables.getLocalVariablesAsync();
 const cs=await figma.variables.getLocalVariableCollectionsAsync();
 const c=cs.find(c=>c.name==='CW 5 · Color');
 for(const [i,name] of ['text','surface'].entries()) {
  const v=vars.find(v=>v.variableCollectionId===c.id&&v.name===name);const value=v.resolveForConsumer(ground).value;
  const rect=figma.createRectangle();createdNodeIds.push(rect.id);ground.appendChild(rect);rect.name=name+' contrast ground';rect.resize(56,44);rect.x=i*56;rect.fills=[figma.variables.setBoundVariableForPaint({type:'SOLID',color:{r:value.r,g:value.g,b:value.b},opacity:value.a??1},'color',v)];
 }
}
ground.layoutPositioning='AUTO';ground.appendChild(scrim);scrim.layoutPositioning='AUTO';scrim.x=0;scrim.y=0;
return {createdNodeIds,mutatedNodeIds:[title.id,focus.id,section.id,scrim.id,parent.id],boardId:'271:8'};
