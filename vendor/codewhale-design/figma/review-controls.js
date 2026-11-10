const page = await figma.getNodeByIdAsync('229:4');
await figma.setCurrentPageAsync(page);
const ids = ['276:130','276:155','276:181','277:125','277:148','277:177','277:196','277:213','277:218','277:241','277:263','277:273','277:286','277:303','278:133','278:150','278:158','278:237','278:259','278:284','278:290','278:299','278:307','278:314','278:325','278:340'];
const sets = [];
for (const id of ids) sets.push(await figma.getNodeByIdAsync(id));
const styles = await figma.getLocalTextStylesAsync();
for (const style of styles.filter(s=>s.name.startsWith('CW 5/'))) await figma.loadFontAsync(style.fontName);
const collection = (await figma.variables.getLocalVariableCollectionsAsync()).find(c=>c.name==='CW 5 · Color');
const vs = await figma.variables.getLocalVariablesAsync();
const variable = name => vs.find(v=>v.variableCollectionId===collection.id&&v.name===name);
const paint = name => {const v=variable(name),r=v.resolveForConsumer(page).value;return [figma.variables.setBoundVariableForPaint({type:'SOLID',color:{r:r.r,g:r.g,b:r.b},opacity:r.a??1},'color',v)];};
const createdNodeIds = [];
const add = n => {createdNodeIds.push(n.id);return n;};
function frame(name, direction='VERTICAL') {const f=add(figma.createAutoLayout());f.name=name;f.layoutMode=direction;f.primaryAxisSizingMode='AUTO';f.counterAxisSizingMode='AUTO';f.itemSpacing=16;f.fills=[];return f;}
async function text(parent,label,role='Caption') {const n=add(figma.createText());parent.appendChild(n);n.name=label;n.fontName=styles.find(s=>s.name==='CW 5/'+role).fontName;await n.setTextStyleIdAsync(styles.find(s=>s.name==='CW 5/'+role).id);n.characters=label;n.fills=paint(role==='Caption'?'muted':'text');return n;}
const boards=[];
for (const theme of ['Dark','Light']) {
 const name='Controls · source review · '+theme;
 const old=page.children.find(n=>n.name===name);
 if(old){boards.push({id:old.id,name:old.name,existing:true});continue;}
 const board=frame(name);page.appendChild(board);board.x=theme==='Dark'?0:2360;board.y=1200;board.paddingTop=32;board.paddingBottom=32;board.paddingLeft=32;board.paddingRight=32;board.fills=paint('panel');board.setExplicitVariableModeForCollection(collection,collection.modes.find(m=>m.name===theme).modeId);
 await text(board,'CW 5 · Source-aligned controls · '+theme,'Title');
 await text(board,'26 families · 61 states · token version 5.3.0 · real CSS metrics, editable Figma instances');
 const columns=frame('Families','HORIZONTAL');board.appendChild(columns);columns.itemSpacing=24;
 const cols=[frame('Inputs'),frame('Structure and feedback'),frame('Data and product')];
 for(const c of cols){columns.appendChild(c);c.itemSpacing=24;}
 const cards=[];
 for(let i=0;i<sets.length;i++){
  const set=sets[i];const card=frame(set.name);cols[i%3].appendChild(card);card.paddingTop=20;card.paddingBottom=20;card.paddingLeft=20;card.paddingRight=20;card.fills=paint('surface');card.cornerRadius=14;card.resize(704,card.height);card.counterAxisSizingMode='FIXED';await text(card,set.name,'Heading');
  const perRow=Math.max(...set.children.map(c=>c.width))>320?1:2;
  let row;
  for(let j=0;j<set.children.length;j++){
   if(j%perRow===0){row=frame('States','HORIZONTAL');card.appendChild(row);row.itemSpacing=24;}
   const cell=frame(set.children[j].name);row.appendChild(cell);cell.itemSpacing=8;await text(cell,set.children[j].name.replace('State=',''));
   const instance=add(set.children[j].createInstance());cell.appendChild(instance);instance.name=set.name+' / '+set.children[j].name;
  }
  cards.push({name:set.name,id:card.id});
 }
 boards.push({id:board.id,name:board.name,width:board.width,height:board.height,cards});
}
return {boards,createdNodeIds,mutatedNodeIds:[]};
