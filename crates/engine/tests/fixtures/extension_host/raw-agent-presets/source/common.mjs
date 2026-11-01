export const inject=['tools','commands','prompt','skills','agentPresets'];
export function apply(ctx,config){
 const id=ctx.agentPresets.composedPreset();
 const value=config.text+':'+id;
 ctx.tools.register({name:'preset_echo',description:'Selected preset echo',parameters:{type:'object',properties:{}},execute:()=>value});
 ctx.commands.register({name:'preset-echo',description:'Selected preset echo',handler:()=>value});
 ctx.prompt.registerSection({id:'selected',text:value});
 ctx.skills.registerRoot({path:'source/skills/'+id});
 ctx.on('tools/pre-execute',()=>({kind:'annotate',text:value}));
}
