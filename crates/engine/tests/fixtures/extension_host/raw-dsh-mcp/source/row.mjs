export const inject=['tools','commands','prompt']
export function apply(ctx,config){
 ctx.tools.register({name:'mixed_echo',description:'mixed graph',parameters:{type:'object'},execute:()=>config.tag})
 ctx.commands.register({name:'mixed-echo',description:'mixed graph',handler:()=>config.tag})
 ctx.prompt.registerSection({id:'mixed',text:'Selected '+config.tag})
 ctx.on('tools/pre-execute',()=>({kind:'annotate',text:config.tag}))
}
